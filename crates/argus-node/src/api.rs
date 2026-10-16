use std::io::Read;
use std::sync::Arc;

use argus_model::{
    Alert, AlertState, BackupSchedule, Check, CheckResult, Event, HostInfo, HostSummary, IngestReply, NodeStatus, Overview, Report, RuleInfo, SERVICE_PREFIX,
    SeriesData, ServiceStatus, Severity,
};
use axum::Json;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, close_code};
use axum::extract::{Query, Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use flate2::read::GzDecoder;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{Config, Connection};

use crate::rules::{self, STALE};
use crate::sync::bearer;
use crate::util::{DAY, HOUR, MINUTE, now_ms, random_id, sha256_hex};
use crate::{App, db, zerobyte};

const MAX_REPORT: u64 = 16 * 1024 * 1024;
const MAX_UPLOAD: usize = 4 * 1024 * 1024;

fn agent(app: &App, token: &str) -> Option<String> {
    let hash = sha256_hex(token.as_bytes());
    app.config().shared.agents.iter().find(|(_, expected)| **expected == hash).map(|(id, _)| id.clone())
}

fn unzip(body: &[u8]) -> Option<Vec<u8>> {
    let mut raw = Vec::new();
    GzDecoder::new(body).take(MAX_REPORT).read_to_end(&mut raw).ok().map(|_| raw)
}

fn accept(app: &App, token: &str, raw: &[u8]) -> Result<IngestReply, (StatusCode, String)> {
    let host = agent(app, token).ok_or((StatusCode::UNAUTHORIZED, String::new()))?;
    let config = app.config();
    let report = serde_json::from_slice::<Report>(raw).map_err(|_| (StatusCode::BAD_REQUEST, String::new()))?;
    if report.host.id != host {
        return Err((StatusCode::FORBIDDEN, String::new()));
    }
    if config.peer.as_ref().is_some_and(|peer| report.host.site != config.name && report.host.site != peer.name) {
        let text = format!("unknown site {:?}, use {:?} or {:?}", report.host.site, config.name, config.peer_name().unwrap_or_default());
        return Err((StatusCode::UNPROCESSABLE_ENTITY, text));
    }
    app.store_report(report, now_ms());
    let now = now_ms();
    let mut checks = config.shared.checks.get(&host).cloned().unwrap_or_default();
    checks.extend(
        config
            .shared
            .services
            .iter()
            .filter(|service| crate::service_runner(app, service, now).as_deref() == Some(host.as_str()))
            .map(|service| Check { name: format!("{SERVICE_PREFIX}{}", service.check.name), ..service.check.clone() }),
    );
    Ok(IngestReply { checks, persistent: config.shared.connection == Connection::Persistent })
}

pub async fn ingest(State(app): State<Arc<App>>, request: Request) -> Response {
    let headers = request.headers();
    let Some(token) = bearer(headers).map(str::to_string).filter(|token| agent(&app, token).is_some()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let gzip = headers.get(header::CONTENT_ENCODING).and_then(|v| v.to_str().ok()).is_some_and(|v| v.eq_ignore_ascii_case("gzip"));
    let Ok(body) = axum::body::to_bytes(request.into_body(), MAX_UPLOAD).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let raw = if gzip {
        let Some(raw) = unzip(&body) else { return StatusCode::BAD_REQUEST.into_response() };
        raw
    } else {
        body.to_vec()
    };
    match accept(&app, &token, &raw) {
        Ok(reply) => Json(reply).into_response(),
        Err(failure) => failure.into_response(),
    }
}

pub async fn ingest_stream(State(app): State<Arc<App>>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let Some(token) = bearer(&headers).map(str::to_string).filter(|token| agent(&app, token).is_some()) else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    upgrade.max_message_size(MAX_UPLOAD).on_upgrade(move |socket| stream_reports(app, token, socket))
}

async fn stream_reports(app: Arc<App>, token: String, mut socket: WebSocket) {
    while let Some(Ok(message)) = socket.recv().await {
        let Message::Binary(body) = message else { continue };
        let reply = unzip(&body).ok_or((StatusCode::BAD_REQUEST, String::new())).and_then(|raw| accept(&app, &token, &raw));
        let sent = match reply {
            Ok(reply) => socket.send(Message::Binary(serde_json::to_vec(&reply).unwrap_or_default().into())).await,
            Err((status, text)) => {
                let mut reason = if text.is_empty() { format!("status {}", status.as_u16()) } else { format!("status {}, {text}", status.as_u16()) };
                let cut = reason.char_indices().map(|(at, c)| at + c.len_utf8()).take_while(|end| *end <= 120).last().unwrap_or(0);
                reason.truncate(cut);
                let _ = socket.send(Message::Close(Some(CloseFrame { code: close_code::POLICY, reason: reason.into() }))).await;
                break;
            }
        };
        if sent.is_err() {
            break;
        }
    }
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
    let known = app.services.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let services = config
        .shared
        .services
        .iter()
        .map(|service| {
            known.get(&service.check.name).cloned().unwrap_or_else(|| ServiceStatus {
                check: CheckResult { name: service.check.name.clone(), kind: service.check.kind.name().to_string(), ..Default::default() },
                ..Default::default()
            })
        })
        .collect();
    Json(Overview { now, node: config.name.clone(), nodes, hosts, services, alerts, backups })
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
    let keys: Vec<String> = query.keys.split(',').map(str::to_string).filter(|k| !k.is_empty()).take(argus_model::MAX_SERIES).collect();
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
