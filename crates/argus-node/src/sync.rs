use std::collections::HashMap;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use argus_model::{Alert, BackupSchedule, Message, SyncPayload};
use axum::Json;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};

use crate::config::Shared;
use crate::db::Marks;
use crate::notify::HTTP;
use crate::util::{DAY, now_ms, sha256_hex};
use crate::{App, db, log};

const SAMPLE_LIMIT: usize = 50_000;
const MAX_PAYLOAD: usize = 64 * 1024 * 1024;
const INTERVAL: Duration = Duration::from_secs(15);

pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers.get(header::AUTHORIZATION)?.to_str().ok()?.strip_prefix("Bearer ").map(str::trim)
}

pub fn same_secret(provided: &str, expected: &str) -> bool {
    sha256_hex(provided.as_bytes()) == sha256_hex(expected.as_bytes())
}

fn build(app: &App, now: i64, cursor: i64, with_samples: bool) -> SyncPayload {
    let config = app.config();
    let me = config.name.clone();
    let hosts: Vec<_> = app.snapshots().into_iter().filter(|s| with_samples && s.report.host.site == me).collect();
    let ids: Vec<String> = hosts.iter().map(|s| s.report.host.id.clone()).collect();
    let db = app.db();
    let samples = if with_samples { db::samples_since(&db, &ids, cursor, SAMPLE_LIMIT) } else { Vec::new() };
    SyncPayload {
        alerts: db::alerts(&db).into_iter().filter(|a| a.updated > now - DAY).collect(),
        events: db::events(&db, 200),
        messages: db::messages(&db).into_iter().filter(|m| m.time > now - DAY).map(|m| Message { delivered: None, ..m }).collect(),
        sent: db::marks(&db, Marks::Sent, now - DAY),
        receipts: db::marks(&db, Marks::Receipts, now - DAY),
        cursor: samples.last().map_or(cursor, |s| s.time),
        samples,
        hosts,
        backups: db::backups(&db),
        config: serde_json::to_value(&config.shared).ok(),
        history_cleared: db::history_cleared(&db),
    }
}

fn adopt(app: &App, incoming: Option<serde_json::Value>, initiator: bool) {
    let Some(shared) = incoming.and_then(|value| serde_json::from_value::<Shared>(value).ok()) else {
        return;
    };
    let current = app.config();
    let newer = shared.revision > current.shared.revision || (!initiator && shared.revision == current.shared.revision);
    if !newer || shared == current.shared {
        return;
    }
    let mut next = (*current).clone();
    next.shared = shared;
    match app.apply_config(next, false) {
        Ok(_) => log(&format!("took over the shared settings of the {} node", current.peer_name().unwrap_or("peer"))),
        Err(error) => log(&format!("ignoring the shared settings of the peer: {error}")),
    }
}

fn newer<T>(local: Option<&T>, incoming: &T, stamp: impl Fn(&T) -> i64) -> bool {
    local.is_none_or(|l| stamp(incoming) > stamp(l))
}

pub fn merge(app: &App, payload: SyncPayload, initiator: bool) {
    adopt(app, payload.config.clone(), initiator);
    {
        let mut db = app.db();
        let alerts: HashMap<String, Alert> = db::alerts(&db).into_iter().map(|a| (a.key.clone(), a)).collect();
        for alert in &payload.alerts {
            if newer(alerts.get(&alert.key), alert, |a| a.updated) {
                let _ = db::save_alert(&db, alert);
            }
        }
        for message in &payload.messages {
            let _ = db::save_message(&db, message);
        }
        for (id, at) in &payload.sent {
            let _ = db::mark_sent(&db, id, *at);
        }
        for (id, at) in &payload.receipts {
            let _ = db::mark_delivered(&db, id, *at);
        }
        let _ = db::clear_history(&db, payload.history_cleared);
        for event in &payload.events {
            let _ = db::save_event(&db, event);
        }
        let backups: HashMap<String, BackupSchedule> = db::backups(&db).into_iter().map(|b| (b.name.clone(), b)).collect();
        for backup in &payload.backups {
            if newer(backups.get(&backup.name), backup, |b| b.updated) {
                let _ = db::save_backup(&db, backup);
            }
        }
        for snapshot in &payload.hosts {
            let _ = db::save_host(&db, snapshot);
        }
        if let Err(error) = db::insert_samples(&mut db, &payload.samples) {
            log(&format!("storing synced samples failed: {error}"));
        }
        if let Some(oldest) = payload.samples.iter().map(|s| s.time).min() {
            let _ = db::reopen_rollups(&db, oldest);
        }
    }
    for snapshot in payload.hosts {
        app.put_snapshot(snapshot);
    }
}

pub async fn serve(State(app): State<Arc<App>>, request: Request) -> Response {
    let Some(peer) = app.config().peer.clone() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if !bearer(request.headers()).is_some_and(|token| same_secret(token, &peer.token)) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(body) = axum::body::to_bytes(request.into_body(), MAX_PAYLOAD).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let Ok(payload) = serde_json::from_slice::<SyncPayload>(&body) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let now = now_ms();
    let cursor = payload.cursor;
    merge(&app, payload, false);
    app.peer_state().last_sync = Some(now);
    Json(build(&app, now, cursor, true)).into_response()
}

fn exchange(url: &str, token: &str, payload: &SyncPayload) -> Result<SyncPayload, String> {
    HTTP.post(&format!("{}/api/v1/sync", url.trim_end_matches('/')))
        .header("Authorization", &format!("Bearer {token}"))
        .send_json(payload)
        .map_err(|e| e.to_string())?
        .body_mut()
        .with_config()
        .limit(64 * 1024 * 1024)
        .read_json::<SyncPayload>()
        .map_err(|e| e.to_string())
}

pub fn run(app: &App, wake: &Receiver<()>) {
    let mut failing = false;
    loop {
        let Some((peer, url)) = app.config().peer.clone().and_then(|p| p.url.clone().map(|url| (p, url))) else {
            let _ = wake.recv_timeout(INTERVAL);
            continue;
        };
        let now = now_ms();
        let cursor = app.peer_state().cursor;
        let mut payload = build(app, now, cursor, false);
        payload.cursor = cursor;
        match exchange(&url, &peer.token, &payload) {
            Ok(response) => {
                if failing {
                    log(&format!("sync with {} node restored", peer.name));
                    failing = false;
                }
                let full = response.samples.len() >= SAMPLE_LIMIT / 2;
                let next = response.cursor.max(cursor);
                merge(app, response, true);
                {
                    let mut state = app.peer_state();
                    state.last_sync = Some(now);
                    state.cursor = next;
                }
                let _ = db::set_meta(&app.db(), "peer_cursor", next);
                if full {
                    continue;
                }
            }
            Err(error) => {
                if !failing {
                    log(&format!("sync with {} node failed: {error}", peer.name));
                    failing = true;
                }
            }
        }
        let _ = wake.recv_timeout(INTERVAL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Peer};
    use argus_model::{AlertState, HostInfo, HostSnapshot, Report, Sample};

    fn app(name: &str) -> App {
        let mut config = Config { name: name.into(), ..Default::default() };
        config.peer = Some(Peer { name: "other".into(), url: None, token: "secret-secret-secret".into() });
        App::new(config, db::open(":memory:").expect("db"))
    }

    #[test]
    fn newer_alerts_win_and_peer_samples_arrive() {
        let home = app("home");
        let vps = app("vps");
        db::save_alert(&home.db(), &Alert { key: "k".into(), state: AlertState::Firing, updated: 10, ..Default::default() }).expect("save");
        db::save_alert(&vps.db(), &Alert { key: "k".into(), state: AlertState::Resolved, updated: 20, notified_resolve: true, ..Default::default() })
            .expect("save");
        let snapshot =
            HostSnapshot { report: Report { host: HostInfo { id: "v".into(), site: "vps".into(), ..Default::default() }, ..Default::default() }, last_seen: 5 };
        vps.put_snapshot(snapshot);
        db::insert_samples(&mut vps.db(), &[Sample { host: "v".into(), key: "cpu||total".into(), time: 5, value: 1.0 }]).expect("samples");
        let response = build(&vps, 30, 0, true);
        assert_eq!(response.cursor, 5);
        merge(&home, response, true);
        let alert = db::alerts(&home.db()).pop().expect("alert");
        assert_eq!(alert.state, AlertState::Resolved);
        assert!(home.snapshots().iter().any(|s| s.report.host.id == "v"));
        assert_eq!(db::samples_since(&home.db(), &["v".into()], 0, 10).len(), 1);
        merge(&vps, build(&home, 30, 0, false), false);
        assert_eq!(db::alerts(&vps.db()).pop().expect("alert").updated, 20);
    }

    #[test]
    fn early_receipts_reach_the_sending_node() {
        let home = app("home");
        let vps = app("vps");
        db::mark_delivered(&vps.db(), "m1", 2_000).expect("receipt");
        db::save_message(&home.db(), &Message { id: "m1".into(), time: 1_000, ..Default::default() }).expect("message");
        merge(&vps, build(&home, 1_500, 0, false), false);
        assert_eq!(db::messages(&vps.db())[0].delivered, Some(2_000));
        merge(&home, build(&vps, 1_500, 0, true), true);
        assert_eq!(db::messages(&home.db())[0].delivered, Some(2_000));
    }

    fn shared(app: &App, rules: usize, revision: i64) {
        let mut config = (*app.config()).clone();
        config.shared.rules.truncate(rules);
        config.shared.revision = revision;
        app.apply_config(config, false).expect("valid");
    }

    #[test]
    fn newest_shared_settings_win_and_the_initiator_wins_ties() {
        let home = app("home");
        let vps = app("vps");
        shared(&home, 3, 0);
        merge(&vps, build(&home, 0, 0, false), false);
        assert_eq!(vps.config().shared.rules.len(), 3);
        shared(&vps, 2, 50);
        merge(&home, build(&vps, 0, 0, false), true);
        assert_eq!(home.config().shared.rules.len(), 2);
        assert_eq!(home.config().shared.revision, 50);
        shared(&home, 1, 50);
        merge(&home, build(&vps, 0, 0, false), true);
        assert_eq!(home.config().shared.rules.len(), 1);
        merge(&vps, build(&home, 0, 0, false), false);
        assert_eq!(vps.config().shared.rules.len(), 1);
    }

    #[test]
    fn secrets_compare_by_hash() {
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abc", "abd"));
    }
}
