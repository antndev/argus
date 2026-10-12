use std::io::Read;
use std::sync::Arc;

use argus_model::{
    Alert, AlertState, Event, HostSummary, IngestReply, Message, Mute, MuteRequest, NodeStatus, Overview, Report, RuleInfo, SeriesData, Severity,
};
use axum::Json;
use axum::extract::{Path, Query, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Config;

use crate::rules::{self, STALE};
use crate::sync::bearer;
use crate::util::{DAY, HOUR, MINUTE, now_ms, random_id, sha256_hex};
use crate::{App, db, notify};

const MAX_REPORT: u64 = 16 * 1024 * 1024;
const MAX_UPLOAD: usize = 4 * 1024 * 1024;

pub async fn ingest(State(app): State<Arc<App>>, request: Request) -> Response {
    let headers = request.headers();
    let Some(token) = bearer(headers) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let hash = sha256_hex(token.as_bytes());
    let config = app.config();
    let Some(host) = config.shared.agents.iter().find(|(_, expected)| **expected == hash).map(|(id, _)| id.clone()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let gzip = headers.get(header::CONTENT_ENCODING).and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("gzip"));
    let Ok(body) = axum::body::to_bytes(request.into_body(), MAX_UPLOAD).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let mut raw = Vec::new();
    if gzip {
        if GzDecoder::new(&body[..]).take(MAX_REPORT).read_to_end(&mut raw).is_err() {
            return StatusCode::BAD_REQUEST.into_response();
        }
    } else {
        raw.extend_from_slice(&body);
    }
    let Ok(report) = serde_json::from_slice::<Report>(&raw) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    if report.host.id != host {
        return StatusCode::FORBIDDEN.into_response();
    }
    app.store_report(report, now_ms());
    Json(IngestReply { checks: config.shared.checks.get(&host).cloned().unwrap_or_default() }).into_response()
}

fn host_state(alerts: &[Alert], mutes: &[Mute], host: &str, now: i64) -> Severity {
    alerts
        .iter()
        .filter(|a| a.host == host && a.state == AlertState::Firing && !mutes.iter().any(|m| m.covers(a, now)))
        .map(|a| a.severity)
        .max()
        .unwrap_or(Severity::Ok)
}

pub async fn overview(State(app): State<Arc<App>>) -> Json<Overview> {
    let now = now_ms();
    let snapshots = app.snapshots();
    let (alerts, mutes, backups) = {
        let db = app.db();
        let alerts: Vec<Alert> = db::alerts(&db).into_iter().filter(|a| a.state == AlertState::Firing).collect();
        let mutes: Vec<Mute> = db::mutes(&db).into_iter().filter(|m| m.until > now).collect();
        (alerts, mutes, db::backups(&db))
    };
    let config = app.config();
    let me = config.name.clone();
    let hosts = snapshots
        .into_iter()
        .map(|snapshot| {
            let report = snapshot.report;
            let owner = if app.owns(&report.host.site) { me.clone() } else { report.host.site.clone() };
            let mut state = host_state(&alerts, &mutes, &report.host.id, now);
            if now - snapshot.last_seen >= STALE && state < Severity::Critical {
                state = Severity::Critical;
            }
            HostSummary {
                limits: rules::limits(&app, &report.host.id, &report.metrics),
                state,
                owner,
                last_seen: snapshot.last_seen,
                latest: report.metrics,
                inventory: report.inventory,
                checks: report.checks,
                info: report.host,
            }
        })
        .collect();
    let mut nodes = vec![NodeStatus { name: me.clone(), site: me, this: true, reachable: true, last_sync: app.peer_state().last_sync }];
    if let Some(peer) = &config.peer {
        nodes.push(NodeStatus {
            name: peer.name.clone(),
            site: peer.name.clone(),
            this: false,
            reachable: !app.peer_down(now),
            last_sync: app.peer_state().last_sync,
        });
    }
    Json(Overview { now, node: config.name.clone(), nodes, hosts, alerts, mutes, backups })
}

#[derive(Deserialize)]
pub struct SeriesQuery {
    host: String,
    keys: String,
    range: Option<i64>,
}

pub async fn series(State(app): State<Arc<App>>, Query(query): Query<SeriesQuery>) -> Json<SeriesData> {
    let now = now_ms();
    let range = query.range.map_or(6 * HOUR, |seconds| seconds * 1_000).clamp(10 * MINUTE, 731 * DAY);
    let keys: Vec<String> = query.keys.split(',').map(str::to_string).filter(|k| !k.is_empty()).take(16).collect();
    let retention = app.config().shared.retention.clone();
    Json(db::query_series(&app.db(), &query.host, &keys, now - range, now, &retention))
}

#[derive(Deserialize)]
pub struct EventsQuery {
    limit: Option<usize>,
}

pub async fn events(State(app): State<Arc<App>>, Query(query): Query<EventsQuery>) -> Json<Vec<Event>> {
    Json(db::events(&app.db(), query.limit.unwrap_or(100).min(1_000)))
}

pub async fn rules(State(app): State<Arc<App>>) -> Json<Vec<RuleInfo>> {
    Json(rules::describe(&app))
}

fn record(app: &App, mute: &Mute, kind: &str, note: String) {
    let db = app.db();
    let _ = db::save_mute(&db, mute);
    let (target, text) = mute.label.split_once(", ").map_or((mute.label.clone(), String::new()), |(a, b)| (a.to_string(), b.to_string()));
    let event = Event { id: random_id(12), time: now_ms(), severity: Severity::Muted, kind: kind.to_string(), target, text, note };
    let _ = db::save_event(&db, &event);
}

pub async fn create_mute(State(app): State<Arc<App>>, Json(request): Json<MuteRequest>) -> Response {
    let now = now_ms();
    let until = request.until.unwrap_or_else(|| now + request.minutes.unwrap_or(240) * MINUTE);
    if until <= now || until > now + 365 * DAY {
        return (StatusCode::BAD_REQUEST, "until must be in the future and within a year").into_response();
    }
    let label = match (&request.alert, &request.host) {
        (Some(key), _) => match db::alerts(&app.db()).into_iter().find(|a| &a.key == key) {
            Some(alert) => format!("{}, {}", alert.host, alert.title),
            None => return (StatusCode::NOT_FOUND, "unknown alert").into_response(),
        },
        (None, Some(host)) => format!("{host}, All alerts"),
        (None, None) => return (StatusCode::BAD_REQUEST, "alert or host required").into_response(),
    };
    let reason = request.reason.trim().chars().take(200).collect::<String>();
    let mute = Mute { id: random_id(12), alert: request.alert, host: request.host, label, until, reason, updated: now };
    record(&app, &mute, "muted", String::new());
    app.wake_sync();
    Json(mute).into_response()
}

pub async fn delete_mute(State(app): State<Arc<App>>, Path(id): Path<String>) -> StatusCode {
    let now = now_ms();
    let Some(mut mute) = db::mutes(&app.db()).into_iter().find(|m| m.id == id) else {
        return StatusCode::NOT_FOUND;
    };
    mute.until = now;
    mute.updated = now;
    record(&app, &mute, "unmuted", "notifications resume".to_string());
    app.wake_sync();
    StatusCode::NO_CONTENT
}

fn version(config: &Config) -> String {
    sha256_hex(serde_json::to_string(config).unwrap_or_default().as_bytes())
}

fn config_view(app: &App) -> Value {
    let config = app.config();
    json!({
        "config": config.redacted(),
        "version": version(&config),
        "path": app.config_path.as_ref().map(|p| p.display().to_string()),
        "restart": config.listen != app.boot.listen || config.database != app.boot.database,
    })
}

pub async fn get_config(State(app): State<Arc<App>>) -> Json<Value> {
    Json(config_view(&app))
}

#[derive(Deserialize)]
pub struct ConfigUpdate {
    version: String,
    config: Config,
}

pub async fn put_config(State(app): State<Arc<App>>, Json(update): Json<ConfigUpdate>) -> Response {
    let current = app.config();
    if update.version != version(&current) {
        return (StatusCode::CONFLICT, "the configuration changed in the meantime, reload and try again").into_response();
    }
    let mut next = update.config;
    if let Err(error) = next.restore(&current) {
        return (StatusCode::BAD_REQUEST, error).into_response();
    }
    let worker = app.clone();
    match tokio::task::spawn_blocking(move || worker.apply_config(next, true)).await {
        Ok(Ok(_)) => {
            app.wake_sync();
            Json(config_view(&app)).into_response()
        }
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn token() -> Json<Value> {
    Json(json!({ "token": random_id(40) }))
}

pub async fn notify_test(State(app): State<Arc<App>>) -> Json<Value> {
    let message = Message {
        id: random_id(12),
        time: now_ms(),
        title: "Test notification".to_string(),
        message: format!("Sent from the {} node dashboard", app.config().name),
        priority: 3,
        delivered: None,
    };
    Json(json!({ "result": notify::deliver(&app, vec![message]) }))
}
