use std::collections::{HashMap, HashSet};

use argus_model::{Alert, AlertState, BackupSchedule, BackupState, CheckStatus, Event, HostSnapshot, Limit, Metric, RuleInfo, SERVICES_HOST, Severity};

use crate::config::{Config, Rule};
use crate::util::{MINUTE, SECOND, duration_text, glob, matches_hosts, random_id};
use crate::{App, db, notify};

pub const STALE: i64 = 90 * SECOND;
pub const CHECK_FAILURES: u32 = 3;
pub const BACKUP_HOST: &str = "zerobyte";

#[derive(Clone, Debug)]
pub struct Condition {
    pub key: String,
    pub host: String,
    pub rule: String,
    pub item: String,
    pub severity: Severity,
    pub title: String,
    pub detail: String,
    pub value: Option<f64>,
    pub hold: i64,
}

impl Condition {
    fn new(host: &str, rule: &str, item: &str, severity: Severity, title: String, detail: String) -> Self {
        Self {
            key: format!("{host}|{rule}|{item}"),
            host: host.to_string(),
            rule: rule.to_string(),
            item: item.to_string(),
            severity,
            title,
            detail,
            value: None,
            hold: 0,
        }
    }
}

fn format_value(metric: &Metric) -> String {
    match (metric.group.as_str(), metric.field.as_str()) {
        ("cpu" | "memory" | "fs", _) | ("smart", "used") => format!("{:.0}%", metric.value),
        ("temp", _) | ("smart", "temp") => format!("{:.0} °C", metric.value),
        ("load", _) => format!("{:.1}", metric.value),
        ("net", _) => format!("{:.1} Mbit/s", metric.value),
        _ => format!("{:.1}", metric.value),
    }
}

fn metric_label(rule: &Rule, metric: &Metric) -> String {
    match (metric.group.as_str(), metric.field.as_str()) {
        ("cpu", _) => "CPU".to_string(),
        ("memory", _) => "Memory".to_string(),
        ("fs", _) => format!("Filesystem {}", metric.item),
        ("load", _) => format!("Load {} min", metric.field),
        ("temp", _) | ("smart", "temp") => format!("Temperature {}", metric.item),
        _ if metric.item.is_empty() => rule.name.clone(),
        _ => format!("{} {}", rule.name, metric.item),
    }
}

fn metric_conditions(app: &App, snapshot: &HostSnapshot) -> Vec<Condition> {
    let host = &snapshot.report.host.id;
    let mut conditions = Vec::new();
    let config = app.config();
    for rule in config.shared.rules.iter().filter(|r| matches_hosts(&r.hosts, host)) {
        for metric in snapshot.report.metrics.iter().filter(|m| glob(&rule.metric, &m.key())) {
            let limit = match (rule.above, rule.below) {
                (Some(above), _) if metric.value > above => above,
                (_, Some(below)) if metric.value < below => below,
                _ => continue,
            };
            let mut shown = metric.clone();
            shown.value = limit;
            let hold = if rule.hold.0 > 0 { format!(" for {}", duration_text(rule.hold.0)) } else { String::new() };
            let mut condition = Condition::new(
                host,
                &rule.name,
                &metric.item,
                rule.severity,
                format!("{} at {}", metric_label(rule, metric), format_value(metric)),
                format!("Limit {}{hold}", format_value(&shown)),
            );
            condition.value = Some(metric.value);
            condition.hold = rule.hold.0;
            conditions.push(condition);
        }
    }
    conditions
}

fn service_conditions(app: &App, now: i64, active: &HashMap<String, Alert>) -> Vec<Condition> {
    let config = app.config();
    let known = app.services.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let failures = app.failures.lock().unwrap_or_else(|p| p.into_inner()).clone();
    let mut conditions = Vec::new();
    for service in &config.shared.services {
        let name = &service.check.name;
        let Some(status) = known.get(name).filter(|s| now - s.last_seen < 3 * MINUTE) else {
            if now - app.started >= 3 * MINUTE {
                let detail = known
                    .get(name)
                    .map_or("No agent has run it yet".to_string(), |s| format!("Last run by {} {} ago", s.runner, duration_text(now - s.last_seen)));
                conditions.push(Condition::new(SERVICES_HOST, "check", name, Severity::Critical, format!("{name} not checked"), detail));
            }
            continue;
        };
        let failed = failures.get(&format!("{SERVICES_HOST}|{name}")).copied().unwrap_or(0);
        let ongoing = active.get(&format!("{SERVICES_HOST}|check|{name}"));
        let (severity, title) = match (status.check.status, ongoing) {
            (CheckStatus::Failed, _) if failed >= CHECK_FAILURES => (Severity::Critical, format!("{name} failing")),
            (CheckStatus::Failed, Some(alert)) => (alert.severity, alert.title.clone()),
            (CheckStatus::Warning, _) => (Severity::Warning, format!("{name} degraded")),
            _ => continue,
        };
        conditions.push(Condition::new(SERVICES_HOST, "check", name, severity, title, status.check.message.clone()));
    }
    conditions
}

fn inventory_conditions(app: &App, snapshot: &HostSnapshot, active: &HashMap<String, Alert>) -> Vec<Condition> {
    let report = &snapshot.report;
    let host = report.host.id.as_str();
    let mut conditions = Vec::new();
    let failures = app.failures.lock().unwrap_or_else(|p| p.into_inner());
    for check in &report.checks {
        let failed = failures.get(&format!("{host}|{}", check.name)).copied().unwrap_or(0);
        let ongoing = active.get(&format!("{host}|check|{}", check.name));
        let (severity, title) = match (check.status, ongoing) {
            (CheckStatus::Failed, _) if failed >= CHECK_FAILURES => (Severity::Critical, format!("{} failing", check.name)),
            (CheckStatus::Failed, Some(alert)) => (alert.severity, alert.title.clone()),
            (CheckStatus::Warning, _) => (Severity::Warning, format!("{} degraded", check.name)),
            _ => continue,
        };
        conditions.push(Condition::new(host, "check", &check.name, severity, title, check.message.clone()));
    }
    drop(failures);
    let inventory = &report.inventory;
    for raid in inventory.raid.iter().filter(|r| r.degraded) {
        conditions.push(Condition::new(
            host,
            "raid",
            &raid.name,
            Severity::Critical,
            format!("RAID {} degraded", raid.name),
            format!("{} {}", raid.state, raid.devices),
        ));
    }
    for disk in inventory.smart.iter().filter(|s| !s.passed) {
        conditions.push(Condition::new(
            host,
            "smart",
            &disk.device,
            Severity::Critical,
            format!("SMART {} failing", disk.device),
            "The drive reports a critical warning".to_string(),
        ));
    }
    if let Some(units) = inventory.failed_units.as_ref().filter(|u| !u.is_empty()) {
        let title = if units.len() == 1 { "1 failed systemd unit".to_string() } else { format!("{} failed systemd units", units.len()) };
        conditions.push(Condition::new(host, "units", "", Severity::Warning, title, units.join(", ")));
    }
    for container in &inventory.containers {
        let kept = matches!(container.restart_policy.as_str(), "always" | "unless-stopped");
        if kept && !matches!(container.state.as_str(), "running" | "stopped") {
            let title = format!("Container {} stopped", container.name);
            conditions.push(Condition {
                hold: 2 * MINUTE,
                ..Condition::new(host, "container", &container.name, Severity::Critical, title, container.status.clone())
            });
        }
    }
    if inventory.reboot_required == Some(true) {
        conditions.push(Condition::new(host, "reboot", "", Severity::Warning, "Reboot required".to_string(), "Installed updates need a restart".to_string()));
    }
    conditions
}

fn backup_conditions(app: &App, now: i64, schedules: Vec<BackupSchedule>) -> Vec<Condition> {
    let max_age = app.config().shared.backup_max_age.0;
    let mut conditions = Vec::new();
    for backup in schedules.into_iter().filter(|b| !b.removed) {
        let name = &backup.name;
        match backup.state {
            BackupState::Failed => conditions.push(Condition::new(
                BACKUP_HOST,
                "backup",
                name,
                Severity::Critical,
                format!("Backup {name} failed"),
                crate::zerobyte::readable(&backup.message),
            )),
            BackupState::Warning => conditions.push(Condition::new(
                BACKUP_HOST,
                "backup",
                name,
                Severity::Warning,
                format!("Backup {name} completed with warnings"),
                crate::zerobyte::readable(&backup.message),
            )),
            _ => {
                if let Some(last) = backup.last_success.filter(|t| now - t > max_age) {
                    conditions.push(Condition::new(
                        BACKUP_HOST,
                        "backup",
                        name,
                        Severity::Critical,
                        format!("Backup {name} overdue"),
                        format!("Last success {} ago", duration_text(now - last)),
                    ));
                }
            }
        }
        match backup.mirror_state {
            BackupState::Failed => conditions.push(Condition::new(
                BACKUP_HOST,
                "mirror",
                name,
                Severity::Critical,
                format!("S3 mirror {name} failed"),
                backup.mirror_message.clone(),
            )),
            BackupState::Unknown => {}
            _ => {
                if let Some(last) = backup.mirror_finished.filter(|t| now - t > max_age) {
                    conditions.push(Condition::new(
                        BACKUP_HOST,
                        "mirror",
                        name,
                        Severity::Critical,
                        format!("S3 mirror {name} overdue"),
                        format!("Last sync {} ago", duration_text(now - last)),
                    ));
                }
            }
        }
    }
    conditions
}

pub fn evaluate(app: &App, now: i64) {
    notify::flush(app, now);
    let peer_down = app.peer_down(now);
    if app.config().peer.is_some() && app.peer_state().last_sync.is_none() && !peer_down {
        return;
    }
    let snapshots = app.snapshots();
    let (active, schedules) = {
        let db = app.db();
        let active: HashMap<String, Alert> = db::alerts(&db).into_iter().filter(|a| a.state != AlertState::Resolved).map(|a| (a.key.clone(), a)).collect();
        (active, db::backups(&db))
    };
    let mut conditions = Vec::new();
    let mut hosts_in_scope = HashSet::new();
    let mut keys_in_scope = HashSet::new();
    for snapshot in &snapshots {
        let host = &snapshot.report.host;
        if snapshot.last_seen < app.started && now - app.started < STALE {
            continue;
        }
        let fresh = now - snapshot.last_seen.max(app.started) < STALE;
        let taken_over = peer_down && fresh;
        if !app.owns(&host.site) && !taken_over {
            continue;
        }
        if fresh {
            hosts_in_scope.insert(host.id.clone());
            conditions.extend(metric_conditions(app, snapshot));
            conditions.extend(inventory_conditions(app, snapshot, &active));
        } else {
            let condition = Condition::new(
                &host.id,
                "unreachable",
                "",
                Severity::Critical,
                "Host unreachable".to_string(),
                format!("No report for {}", duration_text(now - snapshot.last_seen)),
            );
            keys_in_scope.insert(condition.key.clone());
            keys_in_scope.extend(active.values().filter(|a| a.host == host.id && a.state == AlertState::Pending).map(|a| a.key.clone()));
            conditions.push(condition);
        }
    }
    let config = app.config();
    keys_in_scope.extend(active.values().filter(|a| a.rule == "node" && a.owner == config.name()).map(|a| a.key.clone()));
    if let Some(peer) = config.peer_name() {
        keys_in_scope.insert(format!("{peer}|node|"));
        if peer_down {
            let silent = snapshots.iter().filter(|s| s.report.host.site == peer && now - s.last_seen >= STALE).count();
            let since = match app.peer_state().last_sync {
                Some(last) => format!("No sync for {}", duration_text(now - last)),
                None => "No sync since this node started".to_string(),
            };
            let detail = match silent {
                0 => since,
                1 => format!("{since}, no reports from 1 host"),
                n => format!("{since}, no reports from {n} hosts"),
            };
            let title = format!("{} unreachable", peer_role(&config));
            conditions.push(Condition::new(peer, "node", "", Severity::Critical, title, detail));
        }
    }
    let has_backups = config.zerobyte.is_some() || !schedules.is_empty();
    let owns_backups = config.peer.is_none() || config.initiates_sync() || peer_down;
    if owns_backups && !config.shared.services.is_empty() {
        hosts_in_scope.insert(SERVICES_HOST.to_string());
        conditions.extend(service_conditions(app, now, &active));
    }
    if has_backups && owns_backups {
        hosts_in_scope.insert(BACKUP_HOST.to_string());
        conditions.extend(backup_conditions(app, now, schedules));
    }
    reconcile(app, now, conditions, &hosts_in_scope, &keys_in_scope);
}

fn rank(alert: &Alert) -> u8 {
    match alert.rule.as_str() {
        "node" => 0,
        "unreachable" => 1,
        _ => 2,
    }
}

fn reconcile(app: &App, now: i64, conditions: Vec<Condition>, hosts: &HashSet<String>, keys: &HashSet<String>) {
    let me = app.config().name().to_string();
    let mut changes: Vec<(Alert, bool)> = Vec::new();
    let mut transitions: Vec<(Alert, bool)> = Vec::new();
    let mut saved = false;
    {
        let db = app.db();
        let existing: HashMap<String, Alert> = db::alerts(&db).into_iter().map(|a| (a.key.clone(), a)).collect();
        let mut seen = HashSet::new();
        for condition in conditions {
            if !seen.insert(condition.key.clone()) {
                continue;
            }
            let previous = existing.get(&condition.key).filter(|a| a.state != AlertState::Resolved);
            let mut alert = previous.cloned().unwrap_or_else(|| Alert {
                key: condition.key.clone(),
                host: condition.host.clone(),
                rule: condition.rule.clone(),
                item: condition.item.clone(),
                since: now,
                ..Default::default()
            });
            if alert.state == AlertState::Firing && condition.severity > alert.severity {
                alert.notified_fire = false;
                transitions.push((Alert { severity: condition.severity, title: condition.title.clone(), ..alert.clone() }, true));
            }
            alert.severity = condition.severity;
            alert.title = condition.title;
            alert.detail = condition.detail;
            alert.value = condition.value;
            if alert.state == AlertState::Pending && now - alert.since >= condition.hold {
                alert.state = AlertState::Firing;
                alert.fired_at = Some(now);
                transitions.push((alert.clone(), true));
            }
            if alert.state == AlertState::Firing && !alert.notified_fire {
                alert.notified_fire = true;
                changes.push((alert.clone(), true));
            }
            if previous != Some(&alert) {
                alert.owner = me.clone();
                alert.updated = now;
                saved |= db::save_alert(&db, &alert).is_ok();
            }
        }
        for alert in existing.values() {
            let in_scope = hosts.contains(&alert.host) || keys.contains(&alert.key);
            if !in_scope || seen.contains(&alert.key) || alert.state == AlertState::Resolved {
                continue;
            }
            if alert.state == AlertState::Pending {
                let mut cleared = alert.clone();
                cleared.state = AlertState::Resolved;
                cleared.resolved_at = Some(now);
                cleared.owner = me.clone();
                cleared.updated = now;
                saved |= db::save_alert(&db, &cleared).is_ok();
                continue;
            }
            let mut resolved = alert.clone();
            resolved.state = AlertState::Resolved;
            resolved.resolved_at = Some(now);
            resolved.owner = me.clone();
            resolved.updated = now;
            if resolved.notified_fire && !resolved.notified_resolve {
                resolved.notified_resolve = true;
                changes.push((resolved.clone(), false));
            }
            transitions.push((resolved.clone(), false));
            saved |= db::save_alert(&db, &resolved).is_ok();
        }
    }
    if saved {
        app.changed();
    }
    changes.sort_by_key(|(alert, _)| rank(alert));
    let note = notify::send_changes(app, &changes, now);
    let db = app.db();
    for (alert, fired) in transitions {
        let note = if changes.iter().any(|(a, f)| a.key == alert.key && *f == fired) { note.clone() } else { "nothing sent".to_string() };
        let event = Event {
            id: random_id(12),
            time: now,
            severity: if fired { alert.severity } else { Severity::Ok },
            kind: if fired { "fired".to_string() } else { "resolved".to_string() },
            target: alert.host.clone(),
            text: alert.title.clone(),
            note,
        };
        let _ = db::save_event(&db, &event);
    }
}

fn peer_role(config: &Config) -> &'static str {
    if config.initiates_sync() { "External node" } else { "Internal node" }
}

pub fn limits(app: &App, host: &str, metrics: &[Metric]) -> Vec<Limit> {
    let config = app.config();
    metrics
        .iter()
        .filter_map(|metric| {
            let key = metric.key();
            let above =
                config.shared.rules.iter().filter(|r| matches_hosts(&r.hosts, host) && glob(&r.metric, &key)).filter_map(|r| r.above).reduce(f64::min)?;
            Some(Limit { group: metric.group.clone(), item: metric.item.clone(), field: metric.field.clone(), above })
        })
        .collect()
}

pub fn describe(app: &App) -> Vec<RuleInfo> {
    let config = app.config();
    let mut rules: Vec<RuleInfo> = config
        .shared
        .rules
        .iter()
        .map(|rule| {
            let mut condition = match (rule.above, rule.below) {
                (Some(above), _) => format!("{} above {above}", rule.metric),
                (_, Some(below)) => format!("{} below {below}", rule.metric),
                _ => rule.metric.clone(),
            };
            if rule.hold.0 > 0 {
                condition.push_str(&format!(" for {}", duration_text(rule.hold.0)));
            }
            RuleInfo {
                name: rule.name.clone(),
                scope: if rule.hosts.iter().any(|h| h == "*") { "All hosts".to_string() } else { rule.hosts.join(", ") },
                condition,
                severity: rule.severity,
            }
        })
        .collect();
    let fixed = [
        ("Host unreachable", "All hosts", "No report for 90 s", Severity::Critical),
        ("Services", "Every service", "3 failed runs in a row", Severity::Critical),
        ("Containers", "Docker hosts", "Stopped for 2 min although it should restart, unless stopped by hand", Severity::Critical),
        ("RAID", "mdraid arrays", "Degraded or inactive", Severity::Critical),
        ("SMART", "NVMe drives", "Critical warning set", Severity::Critical),
        ("Systemd", "Linux hosts", "Failed units", Severity::Warning),
        ("Reboot", "Linux hosts", "Reboot required", Severity::Warning),
    ];
    rules.extend(fixed.into_iter().map(|(name, scope, condition, severity)| RuleInfo {
        name: name.to_string(),
        scope: scope.to_string(),
        condition: condition.to_string(),
        severity,
    }));
    if config.peer.is_some() {
        rules.push(RuleInfo {
            name: "Node".to_string(),
            scope: peer_role(&config).to_string(),
            condition: "No sync for 60 s".to_string(),
            severity: Severity::Critical,
        });
    }
    if config.zerobyte.is_some() {
        rules.push(RuleInfo {
            name: "Backups".to_string(),
            scope: "ZeroByte schedules and mirrors".to_string(),
            condition: format!("Failed, or no success for {}", duration_text(config.shared.backup_max_age.0)),
            severity: Severity::Critical,
        });
    }
    rules
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{Config, Peer, Span, default_rules};
    use crate::util::MINUTE;
    use argus_model::{CheckResult, Container, HostInfo, Inventory, Raid, Report};

    fn config(peer: bool) -> Config {
        let mut config = Config::default();
        config.shared.rules = default_rules();
        if peer {
            config.peer = Some(Peer { legacy_name: None, url: Some("https://external.example.com".into()), token: "t".into() });
        }
        config
    }

    fn report(id: &str, site: &str, metrics: Vec<Metric>) -> Report {
        Report { host: HostInfo { id: id.into(), site: site.into(), ..Default::default() }, metrics, ..Default::default() }
    }

    fn app(peer: bool) -> App {
        let mut app = App::new(config(peer), db::open(":memory:").expect("db"));
        app.started = 0;
        app
    }

    fn alerts(app: &App) -> HashMap<String, Alert> {
        db::alerts(&app.db()).into_iter().map(|a| (a.key.clone(), a)).collect()
    }

    #[test]
    fn metric_rule_waits_for_hold_then_fires_once_and_resolves() {
        let app = app(false);
        let start = 1_000_000_000;
        app.store_report(report("h", "internal", vec![Metric::new("cpu", "", "total", 95.0)]), start);
        evaluate(&app, start);
        assert_eq!(alerts(&app)["h|CPU|"].state, AlertState::Pending);
        app.store_report(report("h", "internal", vec![Metric::new("cpu", "", "total", 95.0)]), start + 5 * MINUTE);
        evaluate(&app, start + 5 * MINUTE);
        let fired = &alerts(&app)["h|CPU|"];
        assert_eq!(fired.state, AlertState::Firing);
        assert!(fired.notified_fire);
        assert_eq!(fired.title, "CPU at 95%");
        app.store_report(report("h", "internal", vec![Metric::new("cpu", "", "total", 20.0)]), start + 6 * MINUTE);
        evaluate(&app, start + 6 * MINUTE);
        let resolved = &alerts(&app)["h|CPU|"];
        assert_eq!(resolved.state, AlertState::Resolved);
        assert!(resolved.notified_resolve);
        let events = db::events(&app.db(), 10);
        assert_eq!(events.len(), 2);
    }

    #[test]
    fn short_spike_never_fires_and_leaves_a_tombstone_for_the_peer() {
        let app = app(false);
        app.store_report(report("h", "internal", vec![Metric::new("cpu", "", "total", 95.0)]), 0);
        evaluate(&app, 0);
        let pending = alerts(&app)["h|CPU|"].clone();
        app.store_report(report("h", "internal", vec![Metric::new("cpu", "", "total", 10.0)]), MINUTE);
        evaluate(&app, MINUTE);
        let cleared = &alerts(&app)["h|CPU|"];
        assert_eq!(cleared.state, AlertState::Resolved);
        assert!(!cleared.notified_fire && !cleared.notified_resolve);
        assert!(cleared.updated > pending.updated);
        app.store_report(report("h", "internal", vec![Metric::new("cpu", "", "total", 95.0)]), 30 * MINUTE);
        evaluate(&app, 30 * MINUTE);
        assert_eq!(alerts(&app)["h|CPU|"].state, AlertState::Pending);
    }

    #[test]
    fn warning_check_stays_open_while_failures_build_up() {
        let app = app(false);
        let mut degraded = report("h", "internal", Vec::new());
        degraded.checks = vec![CheckResult { name: "Web".into(), status: CheckStatus::Warning, ..Default::default() }];
        app.store_report(degraded.clone(), 0);
        evaluate(&app, 0);
        assert_eq!(alerts(&app)["h|check|Web"].severity, Severity::Warning);
        degraded.checks[0].status = CheckStatus::Failed;
        app.store_report(degraded.clone(), MINUTE);
        evaluate(&app, MINUTE);
        let open = &alerts(&app)["h|check|Web"];
        assert_eq!(open.state, AlertState::Firing);
        assert_eq!(open.severity, Severity::Warning);
        app.store_report(degraded.clone(), 2 * MINUTE);
        app.store_report(degraded, 3 * MINUTE);
        evaluate(&app, 3 * MINUTE);
        assert_eq!(alerts(&app)["h|check|Web"].severity, Severity::Critical);
        let fired: Vec<String> = db::events(&app.db(), 10).into_iter().filter(|e| e.kind == "fired").map(|e| e.text).collect();
        assert_eq!(fired, ["Web failing", "Web degraded"]);
    }

    #[test]
    fn unreachable_host_keeps_other_alerts_untouched() {
        let app = app(false);
        app.store_report(report("h", "internal", vec![Metric::new("fs", "/", "used", 90.0)]), 0);
        evaluate(&app, 0);
        assert_eq!(alerts(&app)["h|Disk|/"].state, AlertState::Firing);
        evaluate(&app, 2 * MINUTE);
        let all = alerts(&app);
        assert_eq!(all["h|unreachable|"].state, AlertState::Firing);
        assert_eq!(all["h|Disk|/"].state, AlertState::Firing);
    }

    #[test]
    fn restarted_node_gives_agents_time_before_calling_hosts_unreachable() {
        let mut app = app(false);
        app.store_report(report("h", "internal", Vec::new()), 0);
        app.started = 10 * MINUTE;
        evaluate(&app, 10 * MINUTE + 30 * SECOND);
        assert!(alerts(&app).is_empty());
        evaluate(&app, 10 * MINUTE + 2 * MINUTE);
        assert_eq!(alerts(&app)["h|unreachable|"].state, AlertState::Firing);
    }

    #[test]
    fn peer_hosts_are_left_alone_until_the_peer_is_down() {
        let app = app(true);
        app.peer_state().last_sync = Some(0);
        app.store_report(report("v", "external", vec![Metric::new("fs", "/", "used", 99.0)]), 0);
        evaluate(&app, 30 * SECOND);
        assert!(alerts(&app).is_empty());
        app.store_report(report("v", "external", vec![Metric::new("fs", "/", "used", 99.0)]), 2 * MINUTE);
        evaluate(&app, 2 * MINUTE);
        let all = alerts(&app);
        assert_eq!(all["v|Disk|/"].state, AlertState::Firing);
        assert_eq!(all["external|node|"].state, AlertState::Firing);
    }

    #[test]
    fn standalone_services_run_on_a_home_agent_but_alert_on_their_own() {
        let mut config = config(false);
        config.shared.agents = [("a".to_string(), "x".to_string()), ("b".to_string(), "y".to_string())].into();
        config.shared.order = vec!["b".into(), "a".into()];
        config.shared.services =
            vec![crate::config::Service { check: serde_json::from_str(r#"{"name":"Allsky","type":"ping","host":"10.0.0.72"}"#).expect("check"), runner: None }];
        let app = App::new(config, db::open(":memory:").expect("db"));
        app.store_report(report("a", "internal", Vec::new()), 0);
        app.store_report(report("b", "internal", Vec::new()), 0);
        let service = app.config().shared.services[0].clone();
        assert_eq!(crate::service_runner(&app, &service, 0).as_deref(), Some("b"));
        let mut run = report("b", "internal", vec![Metric::new("check", "service:Allsky", "ok", 0.0)]);
        run.checks = vec![
            CheckResult { name: "service:Allsky".into(), status: CheckStatus::Failed, message: "Timed out".into(), ..Default::default() },
            CheckResult { name: "Web".into(), ..Default::default() },
        ];
        for minute in 0..3 {
            app.store_report(run.clone(), minute * MINUTE);
        }
        let host = app.snapshots().into_iter().find(|s| s.report.host.id == "b").expect("host");
        assert_eq!(host.report.checks.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Web"]);
        assert!(host.report.metrics.is_empty());
        assert_eq!(db::samples_since(&app.db(), &[SERVICES_HOST.into()], -1, 10).len(), 3);
        evaluate(&app, 2 * MINUTE);
        let alert = &alerts(&app)["services|check|Allsky"];
        assert_eq!((alert.severity, alert.title.as_str()), (Severity::Critical, "Allsky failing"));
        assert!(!alerts(&app).keys().any(|k| k.starts_with("b|")));
    }

    #[test]
    fn stopped_containers_alert_only_when_they_should_run() {
        let app = app(false);
        let container =
            |name: &str, state: &str, policy: &str| Container { name: name.into(), state: state.into(), restart_policy: policy.into(), ..Default::default() };
        let mut report = report("h", "internal", Vec::new());
        report.inventory.containers = vec![
            container("minecraft", "exited", "unless-stopped"),
            container("dump", "exited", "no"),
            container("db", "running", "always"),
            container("mc", "stopped", "always"),
        ];
        app.store_report(report.clone(), 0);
        evaluate(&app, 0);
        assert!(alerts(&app).values().all(|a| a.state == AlertState::Pending));
        app.store_report(report, 3 * MINUTE);
        evaluate(&app, 3 * MINUTE);
        let all = alerts(&app);
        assert_eq!(all["h|container|minecraft"].severity, Severity::Critical);
        assert!(["dump", "db", "mc"].iter().all(|name| !all.contains_key(&format!("h|container|{name}"))));
    }

    #[test]
    fn checks_need_three_failures_and_inventory_problems_alert() {
        let app = app(false);
        let mut failing = report("h", "internal", Vec::new());
        failing.checks = vec![CheckResult { name: "Web".into(), status: CheckStatus::Failed, ..Default::default() }];
        failing.inventory =
            Inventory { raid: vec![Raid { name: "md0".into(), degraded: true, ..Default::default() }], reboot_required: Some(true), ..Default::default() };
        for minute in 0..2 {
            app.store_report(failing.clone(), minute * MINUTE);
        }
        evaluate(&app, MINUTE);
        assert!(!alerts(&app).contains_key("h|check|Web"));
        app.store_report(failing, 2 * MINUTE);
        evaluate(&app, 2 * MINUTE);
        let all = alerts(&app);
        assert_eq!(all["h|check|Web"].severity, Severity::Critical);
        assert_eq!(all["h|raid|md0"].state, AlertState::Firing);
        assert_eq!(all["h|reboot|"].severity, Severity::Warning);
    }

    #[test]
    fn firing_check_survives_a_node_restart() {
        let first = app(false);
        let mut failing = report("h", "internal", Vec::new());
        failing.checks = vec![CheckResult { name: "Web".into(), status: CheckStatus::Failed, ..Default::default() }];
        for minute in 0..3 {
            first.store_report(failing.clone(), minute * MINUTE);
        }
        evaluate(&first, 2 * MINUTE);
        let saved = db::alerts(&first.db());
        let db = db::open(":memory:").expect("db");
        for alert in &saved {
            db::save_alert(&db, alert).expect("save");
        }
        let mut second = App::new(config(false), db);
        second.started = 0;
        second.store_report(failing, 3 * MINUTE);
        evaluate(&second, 3 * MINUTE);
        let alert = &alerts(&second)["h|check|Web"];
        assert_eq!(alert.state, AlertState::Firing);
        assert!(alert.resolved_at.is_none());
    }

    #[test]
    fn limits_take_the_lowest_matching_threshold() {
        let app = app(false);
        let mut config = (*app.config()).clone();
        config.shared.rules.push(Rule {
            name: "NVMe".into(),
            metric: "temp|NVMe*|value".into(),
            hosts: vec!["h".into()],
            above: Some(70.0),
            below: None,
            hold: Span(0),
            severity: Severity::Critical,
        });
        app.apply_config(config, false).expect("valid");
        let metrics = vec![Metric::new("temp", "NVMe", "value", 40.0), Metric::new("temp", "CPU package", "value", 40.0)];
        let found = limits(&app, "h", &metrics);
        assert_eq!(found[0].above, 70.0);
        assert_eq!(found[1].above, 80.0);
    }
}
