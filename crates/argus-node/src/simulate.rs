use std::sync::Arc;

use argus_model::{CheckResult, CheckStatus, Event, Metric, Raid, Report, Severity};
use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;

use crate::util::{now_ms, random_id};
use crate::{App, db, log};

pub const NODE: &str = "";

const HOST_FAULTS: [(&str, &str); 8] = [
    ("cpu", "CPU at 97%"),
    ("memory", "Memory at 96%"),
    ("disk", "Disk at 97%"),
    ("temperature", "Temperature at 92 °C"),
    ("check", "A check fails"),
    ("raid", "RAID degraded"),
    ("reboot", "Reboot required"),
    ("offline", "Host stops reporting"),
];

const NODE_FAULTS: [(&str, &str); 2] = [("backup", "A backup fails"), ("peer", "Sync with the second node fails")];

fn set_metric(report: &mut Report, group: &str, item: &str, field: &str, value: f64) {
    match report.metrics.iter_mut().find(|m| m.group == group && m.item == item && m.field == field) {
        Some(metric) => metric.value = value,
        None => report.metrics.push(Metric::new(group, item, field, value)),
    }
}

pub fn apply(app: &App, report: &mut Report) -> bool {
    let faults = app.faults_of(&report.host.id);
    for fault in &faults {
        match fault.as_str() {
            "offline" => return false,
            "cpu" => set_metric(report, "cpu", "", "total", 97.0),
            "memory" => set_metric(report, "memory", "", "used", 96.0),
            "disk" => {
                let mount = report.metrics.iter().find(|m| m.group == "fs" && m.field == "used").map_or_else(|| "/".to_string(), |m| m.item.clone());
                set_metric(report, "fs", &mount, "used", 97.0);
            }
            "temperature" => {
                let sensors: Vec<String> = report.metrics.iter().filter(|m| m.group == "temp" && m.field == "value").map(|m| m.item.clone()).collect();
                if sensors.is_empty() {
                    set_metric(report, "temp", "CPU", "value", 92.0);
                }
                for sensor in sensors {
                    set_metric(report, "temp", &sensor, "value", 92.0);
                }
            }
            "check" => {
                if report.checks.is_empty() {
                    report.checks.push(CheckResult { name: "Simulated check".into(), kind: "http".into(), target: "simulation".into(), ..Default::default() });
                }
                let check = &mut report.checks[0];
                check.status = CheckStatus::Failed;
                check.message = "Simulated failure".to_string();
                check.latency_ms = None;
                let name = check.name.clone();
                set_metric(report, "check", &name, "ok", 0.0);
            }
            "raid" => match report.inventory.raid.first_mut() {
                Some(raid) => {
                    raid.degraded = true;
                    raid.state = "degraded".to_string();
                }
                None => report.inventory.raid.push(Raid {
                    name: "md0".into(),
                    level: "raid1".into(),
                    state: "degraded".into(),
                    devices: "[U_]".into(),
                    degraded: true,
                }),
            },
            "reboot" => report.inventory.reboot_required = Some(true),
            _ => {}
        }
    }
    true
}

fn describe(fault: &str) -> Option<&'static str> {
    HOST_FAULTS.iter().chain(NODE_FAULTS.iter()).find(|(id, _)| *id == fault).map(|(_, text)| *text)
}

fn state(app: &App) -> Response {
    Json(json!({
        "faults": app.faults(),
        "host_faults": HOST_FAULTS.iter().map(|(id, text)| json!({ "id": id, "text": text })).collect::<Vec<_>>(),
        "node_faults": NODE_FAULTS.iter().map(|(id, text)| json!({ "id": id, "text": text })).collect::<Vec<_>>(),
    }))
    .into_response()
}

pub async fn get(State(app): State<Arc<App>>) -> Response {
    if !app.config().simulate {
        return StatusCode::NOT_FOUND.into_response();
    }
    state(&app)
}

#[derive(Deserialize)]
pub struct Toggle {
    #[serde(default)]
    host: String,
    fault: String,
}

pub async fn toggle(State(app): State<Arc<App>>, Json(request): Json<Toggle>) -> Response {
    if !app.config().simulate {
        return StatusCode::NOT_FOUND.into_response();
    }
    if request.fault == "clear" {
        app.clear_faults();
        log("simulation cleared");
        return state(&app);
    }
    let node_fault = NODE_FAULTS.iter().any(|(id, _)| *id == request.fault);
    let host = if node_fault { NODE.to_string() } else { request.host };
    let Some(text) = describe(&request.fault) else { return (StatusCode::BAD_REQUEST, "unknown fault").into_response() };
    if !node_fault && !app.snapshots().iter().any(|s| s.report.host.id == host) {
        return (StatusCode::BAD_REQUEST, "unknown host").into_response();
    }
    let active = app.toggle_fault(&host, &request.fault);
    let target = if node_fault { app.config().name.clone() } else { host };
    let event = Event {
        id: random_id(12),
        time: now_ms(),
        severity: Severity::Ok,
        kind: if active { "simulated" } else { "simulation ended" }.to_string(),
        target: target.clone(),
        text: text.to_string(),
        note: "from the dashboard".to_string(),
    };
    let _ = db::save_event(&app.db(), &event);
    log(&format!("simulation {} on {target}: {text}", if active { "started" } else { "ended" }));
    state(&app)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use argus_model::HostInfo;

    #[test]
    fn faults_change_reports_only_while_enabled() {
        let app = App::new(Config { simulate: true, ..Default::default() }, db::open(":memory:").expect("db"));
        let mut report = Report { host: HostInfo { id: "h".into(), ..Default::default() }, ..Default::default() };
        report.metrics.push(Metric::new("cpu", "", "total", 5.0));
        app.toggle_fault("h", "cpu");
        app.toggle_fault("h", "check");
        assert!(apply(&app, &mut report));
        assert_eq!(report.metrics[0].value, 97.0);
        assert_eq!(report.checks[0].status, CheckStatus::Failed);
        app.toggle_fault("h", "offline");
        assert!(!apply(&app, &mut report.clone()));
        app.clear_faults();
        let mut clean = Report { host: HostInfo { id: "h".into(), ..Default::default() }, ..Default::default() };
        assert!(apply(&app, &mut clean) && clean.metrics.is_empty());
    }
}
