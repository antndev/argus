use std::io::Read;
use std::sync::Arc;

use argus_model::{
    Alert, AlertState, BackupSchedule, Event, HostInfo, HostSummary, IngestReply, Message, NodeStatus, Overview, Report, RuleInfo, SeriesData, Severity,
};
use axum::Json;
use axum::extract::{Query, Request, State};
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::Config;

use crate::rules::{self, STALE};
use crate::sync::bearer;
use crate::util::{DAY, HOUR, MINUTE, now_ms, random_id, sha256_hex};
use crate::{App, db, notify, zerobyte};

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
    if config.peer.as_ref().is_some_and(|peer| report.host.site != config.name && report.host.site != peer.name) {
        return (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("unknown site {:?}, use {:?} or {:?}", report.host.site, config.name, config.peer_name().unwrap_or_default()),
        )
            .into_response();
    }
    app.store_report(report, now_ms());
    Json(IngestReply { checks: config.shared.checks.get(&host).cloned().unwrap_or_default() }).into_response()
}

fn host_state(alerts: &[Alert], host: &str) -> Severity {
    alerts.iter().filter(|a| a.host == host && a.state == AlertState::Firing).map(|a| a.severity).max().unwrap_or(Severity::Ok)
}

pub async fn overview(State(app): State<Arc<App>>) -> Json<Overview> {
    let now = now_ms();
    let snapshots = app.snapshots();
    let (alerts, backups) = {
        let db = app.db();
        let alerts: Vec<Alert> = db::alerts(&db).into_iter().filter(|a| a.state == AlertState::Firing).collect();
        let backups = db::backups(&db)
            .into_iter()
            .filter(|b| !b.removed)
            .map(|b| BackupSchedule { message: zerobyte::readable(&b.message), mirror_message: zerobyte::readable(&b.mirror_message), ..b })
            .collect();
        (alerts, backups)
    };
    let config = app.config();
    let me = config.name.clone();
    let mut hosts: Vec<HostSummary> = snapshots
        .into_iter()
        .map(|snapshot| {
            let report = snapshot.report;
            let owner = if app.owns(&report.host.site) { me.clone() } else { report.host.site.clone() };
            let mut state = host_state(&alerts, &report.host.id);
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
    let waiting = config.shared.agents.keys().filter(|id| !hosts.iter().any(|h| h.info.id == **id)).cloned().collect::<Vec<_>>();
    hosts.extend(waiting.into_iter().map(|id| HostSummary {
        info: HostInfo { id, site: me.clone(), ..Default::default() },
        state: Severity::Muted,
        owner: me.clone(),
        ..Default::default()
    }));
    hosts.sort_by_key(|h| config.shared.order.iter().position(|id| *id == h.info.id).unwrap_or(usize::MAX));
    let internal = config.peer.is_none() || config.initiates_sync();
    let mut nodes = vec![NodeStatus { name: me, this: true, internal, reachable: true, last_sync: app.peer_state().last_sync }];
    if let Some(peer) = &config.peer {
        nodes.push(NodeStatus {
            name: peer.name.clone(),
            this: false,
            internal: !internal,
            reachable: !app.peer_down(now),
            last_sync: app.peer_state().last_sync,
        });
    }
    Json(Overview { now, node: config.name.clone(), nodes, hosts, alerts, backups })
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

pub async fn clear_history(State(app): State<Arc<App>>) -> StatusCode {
    match db::clear_history(&app.db(), now_ms()) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

pub async fn events(State(app): State<Arc<App>>, Query(query): Query<EventsQuery>) -> Json<Vec<Event>> {
    Json(db::events(&app.db(), query.limit.unwrap_or(100).min(1_000)))
}

pub async fn rules(State(app): State<Arc<App>>) -> Json<Vec<RuleInfo>> {
    Json(rules::describe(&app))
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

const CHANGED: &str = "the configuration changed in the meantime, reload and try again";

pub async fn put_config(State(app): State<Arc<App>>, Json(update): Json<ConfigUpdate>) -> Response {
    let worker = app.clone();
    let saved = tokio::task::spawn_blocking(move || {
        worker.edit_config(true, |current| {
            if update.version != version(current) {
                return Err(CHANGED.to_string());
            }
            let mut next = update.config;
            next.restore(current)?;
            Ok(next)
        })
    })
    .await;
    match saved {
        Ok(Ok(_)) => {
            app.wake_sync();
            Json(config_view(&app)).into_response()
        }
        Ok(Err(error)) if error == CHANGED => (StatusCode::CONFLICT, error).into_response(),
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

#[derive(Deserialize)]
pub struct Order {
    hosts: Vec<String>,
}

pub async fn order(State(app): State<Arc<App>>, Json(order): Json<Order>) -> Response {
    let worker = app.clone();
    let edit = move || {
        worker.edit_config(true, |current| {
            let mut next = current.clone();
            next.shared.order = order.hosts.into_iter().filter(|id| next.shared.agents.contains_key(id)).collect();
            Ok(next)
        })
    };
    match tokio::task::spawn_blocking(edit).await {
        Ok(Ok(_)) => {
            app.wake_sync();
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
