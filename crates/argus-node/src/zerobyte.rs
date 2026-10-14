use std::sync::Arc;
use std::thread;
use std::time::Duration;

use argus_model::{BackupSchedule, BackupState};
use axum::body::Bytes;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde_json::Value;

use crate::notify::HTTP;
use crate::sync::same_secret;
use crate::util::now_ms;
use crate::{App, db, log};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Start,
    Success,
    Warning,
    Failure,
    MirrorFailure,
}

const SUFFIXES: [(&str, Kind); 5] = [
    (" started", Kind::Start),
    (" completed successfully", Kind::Success),
    (" completed with warnings", Kind::Warning),
    (" mirror sync failed", Kind::MirrorFailure),
    (" failed", Kind::Failure),
];

fn text<'a>(body: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter().find_map(|key| body.get(*key).and_then(Value::as_str)).unwrap_or_default()
}

pub fn parse(body: &Value) -> Option<(String, Kind, String)> {
    let title = text(body, &["title", "subject"]).trim();
    let message = text(body, &["message", "body", "text"]);
    let rest = title.strip_prefix("Zerobyte ")?;
    let (name, kind) = SUFFIXES.iter().find_map(|(suffix, kind)| rest.strip_suffix(suffix).map(|name| (name.trim(), *kind)))?;
    let detail =
        message.lines().find_map(|line| line.strip_prefix("Error: ").or_else(|| line.strip_prefix("Warning: "))).unwrap_or_default().trim().to_string();
    let schedule = message.lines().find_map(|line| line.strip_prefix("Schedule: ")).map(str::trim).filter(|s| !s.is_empty()).unwrap_or(name);
    Some((schedule.to_string(), kind, detail))
}

pub fn stats(message: &str) -> (Option<String>, Option<String>) {
    let mut size = None;
    let mut added = None;
    for line in message.lines().map(|l| l.trim().trim_start_matches("- ").trim()) {
        if let Some(value) = line.strip_prefix("Total bytes processed: ").or_else(|| line.strip_prefix("Size: ")) {
            size = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("Data added: ") {
            added = Some(value.trim().to_string());
        }
    }
    (size, added)
}

fn short(error: &str) -> String {
    match error.find("/_data/") {
        Some(at) => {
            let start = error[..at].rfind(' ').map_or(0, |space| space + 1);
            format!("{}{}", &error[..start], &error[at + "/_data".len()..])
        }
        None => error.to_string(),
    }
}

pub fn readable(detail: &str) -> String {
    let Some(start) = detail.find('{') else { return detail.trim().to_string() };
    let values: Vec<Value> = serde_json::Deserializer::from_str(&detail[start..]).into_iter::<Value>().map_while(Result::ok).collect();
    if values.is_empty() {
        return detail.trim().to_string();
    }
    let mut errors = Vec::new();
    let mut summary = None;
    for value in &values {
        let message = value.pointer("/error/message").or_else(|| value.get("message")).and_then(Value::as_str).unwrap_or_default();
        match value.get("message_type").and_then(Value::as_str) {
            Some("error") => errors.push(short(message)),
            _ if !message.is_empty() => summary = Some(message.trim_start_matches("Warning: ").trim_start_matches("Fatal: ").trim()),
            _ => {}
        }
    }
    let mut summary = summary.filter(|s| !s.is_empty()).unwrap_or("The backup reported errors").to_string();
    if let Some(first) = summary.get(..1) {
        summary = first.to_uppercase() + &summary[1..];
    }
    match errors.as_slice() {
        [] => summary,
        [only] => format!("{summary}: {only}"),
        [first, rest @ ..] => format!("{summary}: {first}, and {} more", rest.len()),
    }
}

pub fn apply(schedule: &mut BackupSchedule, kind: Kind, at: i64, detail: &str) {
    let detail = readable(detail);
    let latest = schedule.last_start.into_iter().chain(schedule.last_success).chain(schedule.last_failure).max().unwrap_or(i64::MIN);
    let current = at >= latest;
    match kind {
        Kind::Start => {
            schedule.last_start = schedule.last_start.max(Some(at));
            if current {
                schedule.state = BackupState::Running;
            }
        }
        Kind::Success | Kind::Warning => {
            schedule.last_success = schedule.last_success.max(Some(at));
            if current {
                schedule.state = if kind == Kind::Success { BackupState::Ok } else { BackupState::Warning };
                schedule.message = detail;
            }
        }
        Kind::Failure => {
            schedule.last_failure = schedule.last_failure.max(Some(at));
            if current {
                schedule.state = BackupState::Failed;
                schedule.message = detail;
            }
        }
        Kind::MirrorFailure => {
            if schedule.mirror_finished.is_none_or(|t| at >= t) {
                schedule.mirror_state = BackupState::Failed;
                schedule.mirror_finished = Some(at);
                schedule.mirror_message = detail;
            }
        }
    }
}

fn update(app: &App, name: &str, change: impl FnOnce(&mut BackupSchedule)) {
    {
        let db = app.db();
        let mut schedule =
            db::backups(&db).into_iter().find(|b| b.name == name).unwrap_or_else(|| BackupSchedule { name: name.to_string(), ..Default::default() });
        let before = schedule.clone();
        change(&mut schedule);
        if schedule == before {
            return;
        }
        schedule.updated = now_ms();
        let _ = db::save_backup(&db, &schedule);
    }
    app.wake_sync();
}

pub async fn webhook(State(app): State<Arc<App>>, Path(token): Path<String>, body: Bytes) -> StatusCode {
    let Some(expected) = app.config().zerobyte.as_ref().and_then(|z| z.webhook_token.clone()) else {
        return StatusCode::NOT_FOUND;
    };
    if !same_secret(&token, &expected) {
        return StatusCode::UNAUTHORIZED;
    }
    let Some(value) = serde_json::from_slice::<Value>(&body).ok() else {
        return StatusCode::BAD_REQUEST;
    };
    let Some((name, kind, detail)) = parse(&value) else {
        log("zerobyte webhook ignored, enable JSON with the title and message keys");
        return StatusCode::BAD_REQUEST;
    };
    let (size, added) = stats(text(&value, &["message", "body", "text"]));
    let now = now_ms();
    update(&app, &name, |schedule| {
        apply(schedule, kind, now, &detail);
        if matches!(kind, Kind::Success | Kind::Warning) {
            schedule.size = size.unwrap_or_default();
            schedule.added = added.unwrap_or_default();
        }
    });
    StatusCode::NO_CONTENT
}

fn millis(value: &Value) -> Option<i64> {
    let number = value.as_f64()? as i64;
    Some(if number < 100_000_000_000 { number * 1_000 } else { number })
}

fn list(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        Value::Object(map) => map.into_iter().find_map(|(_, v)| v.as_array().cloned()).unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn fetch(base: &str, key: &str, path: &str) -> Result<Value, String> {
    HTTP.get(&format!("{}{path}", base.trim_end_matches('/')))
        .header("x-api-key", key)
        .call()
        .map_err(|e| e.to_string())?
        .body_mut()
        .read_json::<Value>()
        .map_err(|e| e.to_string())
}

pub fn apply_api(schedule: &mut BackupSchedule, item: &Value, mirrors: &[Value]) {
    if let Some(at) = item.get("lastBackupAt").and_then(millis) {
        let kind = match item.get("lastBackupStatus").and_then(Value::as_str) {
            Some("success") => Some(Kind::Success),
            Some("warning") => Some(Kind::Warning),
            Some("error") => Some(Kind::Failure),
            Some("in_progress") => Some(Kind::Start),
            _ => None,
        };
        let detail = item.get("lastBackupError").and_then(Value::as_str).unwrap_or_default();
        if let Some(kind) = kind {
            apply(schedule, kind, at, detail);
        }
    }
    let tasks: Vec<&Value> = mirrors
        .iter()
        .filter(|m| m.get("enabled").and_then(Value::as_bool).unwrap_or(true))
        .filter_map(|m| m.get("lastSyncTask").filter(|t| !t.is_null()))
        .collect();
    if tasks.is_empty() {
        return;
    }
    let failed = tasks.iter().find(|t| t.get("status").and_then(Value::as_str) != Some("succeeded"));
    let oldest = tasks.iter().filter_map(|t| t.get("finishedAt").and_then(millis)).min();
    match failed {
        Some(task) => {
            schedule.mirror_state = BackupState::Failed;
            schedule.mirror_finished = task.get("finishedAt").and_then(millis).or(oldest);
            schedule.mirror_message = task
                .get("error")
                .and_then(Value::as_str)
                .map(readable)
                .unwrap_or_else(|| format!("Last sync {}", task.get("status").and_then(Value::as_str).unwrap_or("unknown")));
        }
        None => {
            schedule.mirror_state = BackupState::Ok;
            schedule.mirror_finished = oldest;
            schedule.mirror_message = String::new();
        }
    }
}

pub fn poll(app: &App) {
    let mut failing = false;
    loop {
        let settings = app.config().zerobyte.clone();
        let Some((base, key, interval)) = settings.and_then(|z| Some((z.url?, z.api_key?, z.interval.0))) else {
            thread::sleep(Duration::from_secs(30));
            continue;
        };
        match fetch(&base, &key, "/api/v1/backups") {
            Ok(value) => {
                failing = false;
                for item in list(value) {
                    let (Some(name), Some(id)) = (item.get("name").and_then(Value::as_str), item.get("shortId").and_then(Value::as_str)) else {
                        continue;
                    };
                    let mirrors = fetch(&base, &key, &format!("/api/v1/backups/{id}/mirrors")).map(list).unwrap_or_default();
                    update(app, name, |schedule| apply_api(schedule, &item, &mirrors));
                }
            }
            Err(error) => {
                if !failing {
                    log(&format!("zerobyte api request failed: {error}"));
                    failing = true;
                }
            }
        }
        thread::sleep(Duration::from_millis(interval.max(30_000) as u64));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn restic_json_becomes_a_readable_sentence() {
        let raw = r#"{"message_type":"error","error":{"message":"open /var/lib/zerobyte/volumes/F2x45tsX/_data/stacks/app/tmp/x.tmp: no such file or directory"},"during":"archival","item":"/x"} {"message_type":"exit_error","code":3,"message":"Warning: at least one source file could not be read"}"#;
        assert_eq!(readable(raw), "At least one source file could not be read: open /stacks/app/tmp/x.tmp: no such file or directory");
        assert_eq!(readable("disk full"), "disk full");
        let two = format!("{raw} {}", r#"{"message_type":"error","error":{"message":"second"}}"#);
        assert!(readable(&two).ends_with("and 1 more"));
    }

    #[test]
    fn parses_all_zerobyte_titles() {
        let cases = [
            ("Zerobyte Daily Media started", Kind::Start),
            ("Zerobyte Daily Media completed successfully", Kind::Success),
            ("Zerobyte Daily Media completed with warnings", Kind::Warning),
            ("Zerobyte Daily Media failed", Kind::Failure),
            ("Zerobyte Daily Media mirror sync failed", Kind::MirrorFailure),
        ];
        for (title, kind) in cases {
            let body = json!({ "title": title, "message": "Volume: v\nRepository: r\nSchedule: Daily Media\nError: disk full" });
            assert_eq!(parse(&body), Some(("Daily Media".to_string(), kind, "disk full".to_string())));
        }
        assert_eq!(parse(&json!({ "subject": "Zerobyte nas started", "body": "" })).map(|p| p.1), Some(Kind::Start));
        assert_eq!(parse(&json!({ "title": "Something else" })), None);
    }

    #[test]
    fn reads_size_from_both_summary_styles() {
        let detailed =
            "Volume: v\nOverview:\n- Data added: 1.2 GB\n- Data stored: 900 MB\n- Total bytes processed: 412 GB\nBackup Statistics:\n- Snapshot: ab12";
        assert_eq!(stats(detailed), (Some("412 GB".to_string()), Some("1.2 GB".to_string())));
        assert_eq!(stats("Duration: 40s\nFiles: 1,234\nSize: 18 GB\nSnapshot: cd34"), (Some("18 GB".to_string()), None));
        assert_eq!(stats("Volume: v"), (None, None));
    }

    #[test]
    fn older_events_do_not_override_newer_state() {
        let mut schedule = BackupSchedule::default();
        apply(&mut schedule, Kind::Start, 10, "");
        assert_eq!(schedule.state, BackupState::Running);
        apply(&mut schedule, Kind::Failure, 20, "disk full");
        assert_eq!(schedule.state, BackupState::Failed);
        apply(&mut schedule, Kind::Success, 15, "");
        assert_eq!(schedule.state, BackupState::Failed);
        assert_eq!(schedule.last_success, Some(15));
        apply(&mut schedule, Kind::Success, 30, "");
        assert_eq!(schedule.state, BackupState::Ok);
    }

    #[test]
    fn api_items_set_backup_and_mirror_state() {
        let mut schedule = BackupSchedule::default();
        let item = json!({ "name": "nas", "shortId": "a1", "lastBackupAt": 1_700_000_000_000i64, "lastBackupStatus": "success" });
        let mirrors = vec![
            json!({ "enabled": true, "lastSyncTask": { "status": "succeeded", "error": null, "finishedAt": 1_700_000_100_000i64 } }),
            json!({ "enabled": false, "lastSyncTask": { "status": "failed", "error": "x", "finishedAt": 1 } }),
        ];
        apply_api(&mut schedule, &item, &mirrors);
        assert_eq!(schedule.state, BackupState::Ok);
        assert_eq!(schedule.mirror_state, BackupState::Ok);
        assert_eq!(schedule.mirror_finished, Some(1_700_000_100_000));
        let failing = vec![json!({ "lastSyncTask": { "status": "failed", "error": "403 from S3", "finishedAt": 1_700_000_200 } })];
        apply_api(&mut schedule, &item, &failing);
        assert_eq!(schedule.mirror_state, BackupState::Failed);
        assert_eq!(schedule.mirror_message, "403 from S3");
        assert_eq!(schedule.mirror_finished, Some(1_700_000_200_000));
    }
}
