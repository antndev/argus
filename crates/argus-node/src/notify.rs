use std::sync::mpsc::Receiver;
use std::sync::{Arc, LazyLock};
use std::thread;
use std::time::Duration;

use argus_model::{Alert, AlertState, Event, Message, Severity};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::config::{Device, Push};
use crate::push::{self, Outcome};
use crate::util::{DAY, MINUTE, duration_text, now_ms, random_id};
use crate::{App, db, log};

pub static HTTP: LazyLock<ureq::Agent> = LazyLock::new(|| ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).build().into());

const RETRY_DELAYS: [u64; 4] = [0, 2, 10, 60];
const MAX_PAYLOAD: usize = 3_500;
const RESEND_AFTER: i64 = 2 * MINUTE;
const RESEND_WITHIN: i64 = 7 * DAY;

fn line(alert: &Alert, fired: bool) -> String {
    let subject =
        if alert.rule == "node" || alert.host == crate::rules::BACKUP_HOST { alert.title.clone() } else { format!("{}: {}", alert.host, alert.title) };
    if fired { subject } else { format!("Resolved, {subject}") }
}

fn priority(alert: &Alert, fired: bool) -> u8 {
    match (fired, alert.severity) {
        (true, Severity::Critical) => 5,
        (true, _) => 4,
        (false, _) => 3,
    }
}

pub fn compose(changes: &[(Alert, bool)], now: i64) -> Vec<Message> {
    let message = |title: String, text: String, priority: u8| Message { id: random_id(12), time: now, title, message: text, priority, delivered: None };
    match changes {
        [] => Vec::new(),
        [(alert, fired)] => {
            let text = if *fired { alert.detail.clone() } else { format!("Resolved after {}", duration_text(now - alert.fired_at.unwrap_or(alert.since))) };
            vec![message(line(alert, *fired), text, priority(alert, *fired))]
        }
        many => {
            let top = many.iter().map(|(a, f)| priority(a, *f)).max().unwrap_or(3);
            let text = many.iter().map(|(a, f)| line(a, *f)).collect::<Vec<_>>().join("\n");
            vec![message(format!("{} alert changes", many.len()), text, top)]
        }
    }
}

pub fn deliver(app: &App, messages: Vec<Message>) -> String {
    let config = app.config();
    let devices = config.shared.push.as_ref().map_or(0, |p| p.devices.len());
    if devices == 0 {
        return "not sent, no phone set up".to_string();
    }
    for message in messages {
        if db::save_message(&app.db(), &message).unwrap_or(false) {
            app.enqueue(message);
        }
    }
    match devices {
        1 => format!("sent by {} node to 1 device", config.name),
        n => format!("sent by {} node to {n} devices", config.name),
    }
}

fn clip(text: &str, bytes: usize) -> &str {
    let mut end = text.len().min(bytes);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

fn payload(message: &Message, push: &Push) -> Vec<u8> {
    let build = |body: &str| {
        json!({ "id": message.id, "title": clip(&message.title, 200), "body": body, "receipts": push.receipt_urls, "url": "/#/alerts" }).to_string()
    };
    let mut limit = message.message.len();
    loop {
        let text = build(clip(&message.message, limit));
        if text.len() <= MAX_PAYLOAD || limit == 0 {
            return text.into_bytes();
        }
        limit = limit.saturating_sub(text.len() - MAX_PAYLOAD);
    }
}

fn forget(app: &App, device: &Device) {
    let removed = app.edit_config(true, |current| {
        let mut next = current.clone();
        if let Some(push) = next.shared.push.as_mut() {
            push.devices.retain(|d| d.endpoint != device.endpoint);
        }
        Ok(next)
    });
    if let Err(error) = removed {
        log(&format!("removing the expired device {} failed: {error}", device.name));
        return;
    }
    let event = Event {
        id: random_id(12),
        time: now_ms(),
        severity: Severity::Warning,
        kind: "device removed".to_string(),
        target: device.name.clone(),
        text: "The phone ended its notification subscription".to_string(),
        note: "enable notifications on the phone again".to_string(),
    };
    let _ = db::save_event(&app.db(), &event);
    log(&format!("{} ended its push subscription and was removed", device.name));
}

fn send(app: &App, push: &Push, device: &Device, body: &[u8], message: &Message) -> bool {
    let mut last = String::new();
    for delay in RETRY_DELAYS {
        thread::sleep(Duration::from_secs(delay));
        match push::send(push, device, body, &message.id, message.priority >= 4, now_ms() / 1_000) {
            Outcome::Sent => return true,
            Outcome::Gone => {
                forget(app, device);
                return true;
            }
            Outcome::Failed(error) => last = error,
        }
    }
    log(&format!("push to {} failed, trying again later: {last}", device.name));
    false
}

pub fn worker(app: &App, queue: &Receiver<Message>) {
    for message in queue {
        if let Some(push) = app.config().shared.push.clone() {
            let body = payload(&message, &push);
            let results: Vec<bool> = push
                .devices
                .iter()
                .map(|device| {
                    let mark = format!("{}|{}", message.id, crate::util::sha256_hex(device.endpoint.as_bytes()));
                    if db::delivered(&app.db(), &mark) {
                        return true;
                    }
                    let ok = send(app, &push, device, &body, &message);
                    if ok {
                        let _ = db::mark_delivery(&app.db(), &mark, now_ms());
                    }
                    ok
                })
                .collect();
            let done = results.into_iter().all(|ok| ok);
            if done {
                let _ = db::mark_sent(&app.db(), &message.id, now_ms());
            }
        }
        app.settle(&message.id);
    }
}

pub fn resend(app: &App, now: i64) {
    if app.config().shared.push.as_ref().is_none_or(|p| p.devices.is_empty()) {
        return;
    }
    let pending = db::unsent(&app.db(), now - RESEND_WITHIN, now - RESEND_AFTER);
    for message in pending {
        app.enqueue(message);
    }
}

pub fn status(app: &App, now: i64) {
    let config = app.config();
    let Some(every) = config.shared.push.as_ref().and_then(|p| p.status_every) else { return };
    let responsible = config.peer.is_none() || config.initiates_sync() || app.peer_down(now);
    if !responsible || now - db::meta(&app.db(), "status_sent") < every.0 {
        return;
    }
    let active = db::alerts(&app.db()).into_iter().filter(|a| a.state == AlertState::Firing).count();
    let text = match active {
        0 => "All clear".to_string(),
        1 => "1 active alert".to_string(),
        n => format!("{n} active alerts"),
    };
    let _ = db::set_meta(&app.db(), "status_sent", now);
    deliver(app, vec![Message { id: random_id(12), time: now, title: "Argus status".to_string(), message: text, priority: 2, delivered: None }]);
}

pub async fn receipt(State(app): State<Arc<App>>, Path(id): Path<String>) -> StatusCode {
    if id.len() <= 32 && id.chars().all(|c| c.is_ascii_alphanumeric()) {
        let _ = db::mark_delivered(&app.db(), &id, now_ms());
    }
    StatusCode::NO_CONTENT
}

#[derive(Deserialize)]
pub struct Subscription {
    name: String,
    endpoint: String,
    p256dh: String,
    auth: String,
}

const NO_PUSH: &str = "turn on phone notifications first";

pub async fn subscribe(State(app): State<Arc<App>>, Json(subscription): Json<Subscription>) -> Response {
    let device = Device {
        name: subscription.name.trim().chars().take(40).collect(),
        endpoint: subscription.endpoint,
        p256dh: subscription.p256dh,
        auth: subscription.auth,
        added: now_ms(),
    };
    let worker = app.clone();
    let edit = move || {
        worker.edit_config(true, |current| {
            let mut next = current.clone();
            let push = next.shared.push.as_mut().ok_or_else(|| NO_PUSH.to_string())?;
            push.devices.retain(|d| d.endpoint != device.endpoint && d.name != device.name);
            push.devices.push(device);
            Ok(next)
        })
    };
    match tokio::task::spawn_blocking(edit).await {
        Ok(Ok(_)) => {
            app.wake_sync();
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(Err(error)) if error == NO_PUSH => (StatusCode::CONFLICT, error).into_response(),
        Ok(Err(error)) => (StatusCode::BAD_REQUEST, error).into_response(),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn messages(State(app): State<Arc<App>>) -> Json<Vec<Message>> {
    let mut list = db::messages(&app.db());
    list.reverse();
    list.truncate(20);
    Json(list)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bursts_become_one_message_with_root_cause_first() {
        let alert = |host: &str, severity| Alert { host: host.into(), title: "x".into(), severity, ..Default::default() };
        let one = compose(&[(alert("h", Severity::Warning), true)], 0);
        assert_eq!(one.len(), 1);
        assert_eq!(one[0].priority, 4);
        let many = compose(&[(alert("a", Severity::Critical), true), (alert("b", Severity::Warning), false)], 0);
        assert_eq!(many.len(), 1);
        assert_eq!(many[0].priority, 5);
        assert!(many[0].message.starts_with("a: x\nResolved, b: x"));
        let node = Alert { host: "vps".into(), rule: "node".into(), title: "vps node unreachable".into(), ..Default::default() };
        assert_eq!(compose(&[(node, true)], 0)[0].title, "vps node unreachable");
    }

    #[test]
    fn payload_stays_under_the_push_size_limit_even_with_wide_characters() {
        let push = Push { receipt_urls: vec!["https://a.example".into()], ..Default::default() };
        let message = Message { id: "abc".into(), title: "T".into(), message: "ä€".repeat(3_000), ..Default::default() };
        let bytes = payload(&message, &push);
        assert!(bytes.len() <= MAX_PAYLOAD);
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(value["id"], "abc");
        assert_eq!(value["receipts"][0], "https://a.example");
        let short = Message { message: "fine".into(), ..message };
        assert_eq!(serde_json::from_slice::<serde_json::Value>(&payload(&short, &push)).expect("json")["body"], "fine");
    }

    #[test]
    fn unsent_messages_are_retried_until_marked_sent() {
        let db = db::open(":memory:").expect("db");
        let message = Message { id: "m1".into(), time: 1_000, ..Default::default() };
        db::save_message(&db, &message).expect("save");
        assert_eq!(db::unsent(&db, 0, 2_000).len(), 1);
        db::mark_sent(&db, "m1", 1_500).expect("mark");
        assert!(db::unsent(&db, 0, 2_000).is_empty());
    }
}
