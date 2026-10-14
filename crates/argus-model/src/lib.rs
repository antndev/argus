use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

pub const KEY_SEPARATOR: char = '|';

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Report {
    pub host: HostInfo,
    pub time: i64,
    pub metrics: Vec<Metric>,
    pub inventory: Inventory,
    pub checks: Vec<CheckResult>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostInfo {
    pub id: String,
    pub site: String,
    pub role: String,
    pub os: String,
    pub kernel: String,
    pub arch: String,
    pub cores: u32,
    pub memory_bytes: u64,
    pub uptime_s: u64,
    pub agent: String,
    pub version: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Metric {
    pub group: String,
    pub item: String,
    pub field: String,
    pub value: f64,
}

impl Metric {
    pub fn new(group: &str, item: &str, field: &str, value: f64) -> Self {
        Self { group: group.to_string(), item: item.to_string(), field: field.to_string(), value }
    }

    pub fn key(&self) -> String {
        metric_key(&self.group, &self.item, &self.field)
    }
}

pub fn metric_key(group: &str, item: &str, field: &str) -> String {
    format!("{group}{KEY_SEPARATOR}{item}{KEY_SEPARATOR}{field}")
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Inventory {
    pub filesystems: Vec<Filesystem>,
    pub interfaces: Vec<Interface>,
    pub sensors: Vec<Sensor>,
    pub containers: Vec<Container>,
    pub raid: Vec<Raid>,
    pub smart: Vec<Smart>,
    pub failed_units: Option<Vec<String>>,
    pub reboot_required: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Filesystem {
    pub mount: String,
    pub fstype: String,
    pub size_bytes: u64,
    pub used_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Interface {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sensor {
    pub name: String,
    pub group: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Container {
    pub name: String,
    pub image: String,
    pub state: String,
    pub status: String,
    pub health: String,
    pub restart_policy: String,
    pub restarts: u32,
    pub cpu: f64,
    pub memory_bytes: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Raid {
    pub name: String,
    pub level: String,
    pub state: String,
    pub devices: String,
    pub degraded: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Smart {
    pub device: String,
    pub passed: bool,
    pub percent_used: Option<f64>,
    pub temperature: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CheckStatus {
    #[default]
    Ok,
    Warning,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct CheckResult {
    pub name: String,
    pub kind: String,
    pub target: String,
    pub status: CheckStatus,
    pub message: String,
    pub latency_ms: Option<f64>,
    pub value: Option<f64>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    #[default]
    Ok,
    Muted,
    Warning,
    Critical,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AlertState {
    #[default]
    Pending,
    Firing,
    Resolved,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Alert {
    pub key: String,
    pub host: String,
    pub rule: String,
    pub item: String,
    pub severity: Severity,
    pub state: AlertState,
    pub title: String,
    pub detail: String,
    pub value: Option<f64>,
    pub since: i64,
    pub fired_at: Option<i64>,
    pub resolved_at: Option<i64>,
    pub notified_fire: bool,
    pub notified_resolve: bool,
    pub owner: String,
    pub updated: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Event {
    pub id: String,
    pub time: i64,
    pub severity: Severity,
    pub kind: String,
    pub target: String,
    pub text: String,
    pub note: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Message {
    pub id: String,
    pub time: i64,
    pub title: String,
    pub message: String,
    pub priority: u8,
    pub delivered: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostSnapshot {
    pub report: Report,
    pub last_seen: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Sample {
    pub host: String,
    pub key: String,
    pub time: i64,
    pub value: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncPayload {
    pub alerts: Vec<Alert>,
    pub events: Vec<Event>,
    pub messages: Vec<Message>,
    pub sent: Vec<(String, i64)>,
    pub receipts: Vec<(String, i64)>,
    pub hosts: Vec<HostSnapshot>,
    pub samples: Vec<Sample>,
    pub backups: Vec<BackupSchedule>,
    pub cursor: i64,
    pub config: Option<serde_json::Value>,
    pub history_cleared: i64,
    pub login: Option<LoginSync>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct LoginSync {
    pub password: Option<String>,
    pub key: String,
    pub changed: i64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum BackupState {
    #[default]
    Unknown,
    Running,
    Ok,
    Warning,
    Failed,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupSchedule {
    pub name: String,
    pub state: BackupState,
    pub last_start: Option<i64>,
    pub last_success: Option<i64>,
    pub last_failure: Option<i64>,
    pub message: String,
    pub mirror_state: BackupState,
    pub mirror_finished: Option<i64>,
    pub mirror_message: String,
    pub size: String,
    pub added: String,
    pub repository: String,
    pub repository_kind: String,
    pub mirrors: Vec<BackupMirror>,
    pub removed: bool,
    pub updated: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BackupMirror {
    pub repository: String,
    pub kind: String,
    pub state: BackupState,
    pub finished: Option<i64>,
    pub message: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NodeStatus {
    pub name: String,
    pub this: bool,
    pub internal: bool,
    pub reachable: bool,
    pub last_sync: Option<i64>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct HostSummary {
    pub info: HostInfo,
    pub last_seen: i64,
    pub state: Severity,
    pub owner: String,
    pub latest: Vec<Metric>,
    pub inventory: Inventory,
    pub checks: Vec<CheckResult>,
    pub limits: Vec<Limit>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Limit {
    pub group: String,
    pub item: String,
    pub field: String,
    pub above: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Overview {
    pub now: i64,
    pub node: String,
    pub nodes: Vec<NodeStatus>,
    pub hosts: Vec<HostSummary>,
    pub services: Vec<ServiceStatus>,
    pub alerts: Vec<Alert>,
    pub backups: Vec<BackupSchedule>,
}

pub const SERVICE_PREFIX: &str = "service:";
pub const SERVICES_HOST: &str = "services";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServiceStatus {
    pub check: CheckResult,
    pub runner: String,
    pub last_seen: i64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleInfo {
    pub name: String,
    pub scope: String,
    pub condition: String,
    pub severity: Severity,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SeriesData {
    pub times: Vec<i64>,
    pub values: Vec<Vec<Option<f64>>>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    #[serde(flatten)]
    pub kind: CheckKind,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum CheckKind {
    Http {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<u16>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        contains: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        json: Vec<JsonAssertion>,
        #[serde(default = "default_timeout", with = "duration")]
        timeout: Duration,
    },
    Tcp {
        host: String,
        port: u16,
        #[serde(default = "default_timeout", with = "duration")]
        timeout: Duration,
    },
    Ping {
        host: String,
    },
    FileAge {
        path: PathBuf,
        #[serde(with = "duration")]
        max_age: Duration,
    },
    Exec {
        command: Vec<String>,
        #[serde(default = "default_timeout", with = "duration")]
        timeout: Duration,
    },
}

impl CheckKind {
    pub fn name(&self) -> &'static str {
        match self {
            CheckKind::Http { .. } => "http",
            CheckKind::Tcp { .. } => "tcp",
            CheckKind::Ping { .. } => "ping",
            CheckKind::FileAge { .. } => "file_age",
            CheckKind::Exec { .. } => "exec",
        }
    }

    pub fn remote(&self) -> bool {
        matches!(self, CheckKind::Http { .. } | CheckKind::Tcp { .. } | CheckKind::Ping { .. })
    }

    pub fn timeout(&self) -> Option<Duration> {
        match self {
            CheckKind::Http { timeout, .. } | CheckKind::Tcp { timeout, .. } | CheckKind::Exec { timeout, .. } => Some(*timeout),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonAssertion {
    pub path: String,
    pub equals: serde_json::Value,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct IngestReply {
    pub checks: Vec<Check>,
}

fn default_timeout() -> Duration {
    Duration::from_secs(10)
}

pub fn parse_duration(text: &str) -> Option<Duration> {
    let text = text.trim();
    let split = text.find(|c: char| !c.is_ascii_digit()).unwrap_or(text.len());
    let (number, unit) = text.split_at(split);
    let number: u64 = number.parse().ok()?;
    let factor = match unit.trim() {
        "" | "s" => 1,
        "m" => 60,
        "h" => 3600,
        "d" => 86400,
        _ => return None,
    };
    number.checked_mul(factor).map(Duration::from_secs)
}

pub fn duration_text(duration: Duration) -> String {
    let seconds = duration.as_secs();
    match seconds {
        0 => "0s".to_string(),
        _ if seconds.is_multiple_of(86400) => format!("{}d", seconds / 86400),
        _ if seconds.is_multiple_of(3600) => format!("{}h", seconds / 3600),
        _ if seconds.is_multiple_of(60) => format!("{}m", seconds / 60),
        _ => format!("{seconds}s"),
    }
}

pub mod duration {
    use super::*;

    pub fn serialize<S: Serializer>(value: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&duration_text(*value))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Seconds(u64),
            Text(String),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Seconds(seconds) => Ok(Duration::from_secs(seconds)),
            Raw::Text(text) => parse_duration(&text).ok_or_else(|| serde::de::Error::custom(format!("invalid duration {text:?}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_reject_unknown_fields() {
        assert!(serde_json::from_str::<Check>(r#"{"name":"Web","type":"http","url":"https://a","contains":"ok"}"#).is_ok());
        assert!(serde_json::from_str::<Check>(r#"{"name":"Web","type":"http","url":"https://a","contain":"ok"}"#).is_err());
    }

    #[test]
    fn durations_parse_and_print_compact() {
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("45"), Some(Duration::from_secs(45)));
        assert_eq!(parse_duration("1w"), None);
        assert_eq!(parse_duration("99999999999999999d"), None);
        assert_eq!(duration_text(Duration::from_secs(129_600)), "36h");
        assert_eq!(duration_text(Duration::from_secs(90)), "90s");
    }

    #[test]
    fn checks_round_trip_with_text_durations() {
        let check: Check = serde_json::from_str(r#"{"name":"Web","type":"http","url":"https://a","timeout":"5s"}"#).expect("parses");
        let text = serde_json::to_string(&check).expect("serializes");
        assert!(text.contains(r#""timeout":"5s""#) && !text.contains("status"));
        assert_eq!(serde_json::from_str::<Check>(&text).expect("round trip"), check);
        assert!(check.kind.remote());
    }
}
