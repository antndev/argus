use std::collections::{BTreeMap, HashSet};
use std::path::Path;
use std::time::Duration;

use argon2::password_hash::{PasswordHasher, SaltString};
use argus_model::{Check, Severity, duration_text};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::util::{DAY, HOUR, MINUTE, SECOND, sha256_hex};

pub const HIDDEN: &str = "********";

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Span(pub i64);

impl<'de> Deserialize<'de> for Span {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Seconds(i64),
            Text(String),
        }
        match Raw::deserialize(deserializer)? {
            Raw::Seconds(seconds) => Ok(Span(seconds * 1_000)),
            Raw::Text(text) => argus_model::parse_duration(&text)
                .map(|d| Span(d.as_millis() as i64))
                .ok_or_else(|| serde::de::Error::custom(format!("invalid duration \"{text}\""))),
        }
    }
}

impl Serialize for Span {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&duration_text(Duration::from_millis(self.0.max(0) as u64)))
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub name: String,
    #[serde(default = "default_listen")]
    pub listen: String,
    #[serde(default = "default_database")]
    pub database: String,
    #[serde(default = "enabled")]
    pub ui: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub simulate: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dashboard: Option<Dashboard>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<Peer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zerobyte: Option<ZeroByte>,
    #[serde(default)]
    pub shared: Shared,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Shared {
    #[serde(default)]
    pub revision: i64,
    #[serde(default)]
    pub agents: BTreeMap<String, String>,
    #[serde(default)]
    pub checks: BTreeMap<String, Vec<Check>>,
    #[serde(default = "default_rules")]
    pub rules: Vec<Rule>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub push: Option<Push>,
    #[serde(default = "backup_age")]
    pub backup_max_age: Span,
    #[serde(default)]
    pub retention: Retention,
}

impl Default for Shared {
    fn default() -> Self {
        Self {
            revision: 0,
            agents: BTreeMap::new(),
            checks: BTreeMap::new(),
            rules: default_rules(),
            push: None,
            backup_max_age: backup_age(),
            retention: Retention::default(),
        }
    }
}

impl Shared {
    pub fn same(&self, other: &Shared) -> bool {
        Shared { revision: 0, ..self.clone() } == Shared { revision: 0, ..other.clone() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Dashboard {
    pub password: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retention {
    #[serde(default = "raw_retention")]
    pub raw: Span,
    #[serde(default = "medium_retention")]
    pub medium: Span,
    #[serde(default = "long_retention")]
    pub long: Span,
}

impl Default for Retention {
    fn default() -> Self {
        Self { raw: raw_retention(), medium: medium_retention(), long: long_retention() }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Peer {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub token: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Push {
    #[serde(default)]
    pub subject: String,
    #[serde(default)]
    pub public_key: String,
    #[serde(default)]
    pub private_key: String,
    #[serde(default)]
    pub receipt_urls: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_every: Option<Span>,
    #[serde(default)]
    pub devices: Vec<Device>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Device {
    pub name: String,
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    #[serde(default)]
    pub added: i64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ZeroByte {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    #[serde(default = "poll_interval")]
    pub interval: Span,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    pub name: String,
    pub metric: String,
    #[serde(default = "all_hosts")]
    pub hosts: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub above: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub below: Option<f64>,
    #[serde(default, rename = "for")]
    pub hold: Span,
    #[serde(default = "warning")]
    pub severity: Severity,
}

impl Default for Config {
    fn default() -> Self {
        serde_json::from_str(r#"{"name":"node"}"#).expect("defaults")
    }
}

fn hash_password(password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    getrandom::fill(&mut salt).map_err(|e| e.to_string())?;
    let salt = SaltString::encode_b64(&salt).map_err(|e| e.to_string())?;
    argon2::Argon2::default().hash_password(password.as_bytes(), &salt).map(|h| h.to_string()).map_err(|e| e.to_string())
}

fn is_sha256(text: &str) -> bool {
    text.len() == 64 && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn plain_name(text: &str) -> bool {
    !text.is_empty() && text.len() <= 64 && text.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
}

fn web_url(text: &str) -> bool {
    text.starts_with("http://") || text.starts_with("https://")
}

fn keep(incoming: &mut String, current: Option<&String>, what: &str) -> Result<(), String> {
    if incoming == HIDDEN {
        *incoming = current.cloned().ok_or_else(|| format!("{what} has no stored value"))?;
    }
    Ok(())
}

fn keep_option(incoming: &mut Option<String>, current: Option<&String>, what: &str) -> Result<(), String> {
    match incoming {
        Some(value) => keep(value, current, what),
        None => Ok(()),
    }
}

impl Config {
    pub fn parse(text: &str) -> Result<Self, String> {
        serde_json::from_str(text.trim_start_matches('\u{feff}')).map_err(|e| e.to_string())
    }

    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("invalid config {}: {e}", path.display()))
    }

    pub fn save(&self, path: &Path) -> Result<(), String> {
        let mut text = serde_json::to_string_pretty(self).map_err(|e| e.to_string())?;
        text.push('\n');
        let temporary = path.with_extension("json.tmp");
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let write = |options: &std::fs::OpenOptions| -> std::io::Result<()> {
            use std::io::Write;
            let mut file = options.open(&temporary)?;
            file.write_all(text.as_bytes())?;
            file.sync_all()?;
            std::fs::rename(&temporary, path)
        };
        write(&options).map_err(|e| format!("cannot write {}: {e}", path.display()))
    }

    pub fn normalize(&mut self) -> Result<(), String> {
        if let Some(dashboard) = self.dashboard.as_mut().filter(|d| !d.password.starts_with("$argon2")) {
            dashboard.password = hash_password(&dashboard.password)?;
        }
        if let Some(push) = self.shared.push.as_mut().filter(|p| p.public_key.is_empty() || p.private_key.is_empty()) {
            (push.public_key, push.private_key) = crate::push::keypair();
        }
        for token in self.shared.agents.values_mut() {
            let trimmed = token.trim();
            *token = if is_sha256(trimmed) { trimmed.to_ascii_lowercase() } else { sha256_hex(trimmed.as_bytes()) };
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), String> {
        if !plain_name(&self.name) {
            return Err("name needs letters, digits, dash, dot or underscore".into());
        }
        if let Some(peer) = &self.peer {
            if !plain_name(&peer.name) || peer.name == self.name {
                return Err("the peer needs its own name, different from this node".into());
            }
            if peer.token.trim().len() < 16 {
                return Err("the peer token needs at least 16 characters".into());
            }
            if peer.url.as_deref().is_some_and(|url| !web_url(url)) {
                return Err("the peer url must start with http:// or https://".into());
            }
        }
        if let Some(dashboard) = &self.dashboard
            && dashboard.password.is_empty()
        {
            return Err("the dashboard password is empty".into());
        }
        if let Some(zerobyte) = &self.zerobyte {
            if zerobyte.webhook_token.as_deref().is_some_and(|t| t.len() < 16 || !plain_name(t)) {
                return Err("the ZeroByte webhook token needs at least 16 letters or digits".into());
            }
            if zerobyte.url.as_deref().is_some_and(|u| !web_url(u)) {
                return Err("the ZeroByte url must start with http:// or https://".into());
            }
        }
        let shared = &self.shared;
        if let Some(host) = shared.agents.keys().find(|host| !plain_name(host)) {
            return Err(format!("host id {host:?} needs letters, digits, dash, dot or underscore"));
        }
        for (host, checks) in &shared.checks {
            if !shared.agents.contains_key(host) {
                return Err(format!("checks for {host:?} need an agent with that id"));
            }
            let mut names = HashSet::new();
            for check in checks {
                if check.name.trim().is_empty() || !names.insert(check.name.as_str()) {
                    return Err(format!("checks on {host} need unique, non-empty names"));
                }
                if !check.kind.remote() {
                    return Err(format!("check {:?} on {host}: only http, tcp and ping checks can come from the node", check.name));
                }
                if check.kind.timeout().is_some_and(|t| t < Duration::from_secs(1)) {
                    return Err(format!("check {:?} on {host} needs a timeout of at least 1 s", check.name));
                }
            }
        }
        for rule in &shared.rules {
            if rule.name.trim().is_empty() || rule.metric.trim().is_empty() {
                return Err("every rule needs a name and a metric".into());
            }
            if rule.above.is_none() && rule.below.is_none() {
                return Err(format!("rule {:?} needs above or below", rule.name));
            }
            if rule.hold.0 < 0 {
                return Err(format!("rule {:?} has a negative duration", rule.name));
            }
        }
        if let Some(push) = &shared.push {
            if !push.subject.starts_with("mailto:") && !push.subject.starts_with("https://") {
                return Err("the push contact must be a mailto: or https:// address".into());
            }
            if !crate::push::valid_public_key(&push.public_key) || !crate::push::valid_private_key(&push.private_key) {
                return Err("the push key pair is invalid, remove both keys to create a new pair".into());
            }
            if push.receipt_urls.iter().any(|url| !web_url(url)) {
                return Err("confirmation addresses must start with http:// or https://".into());
            }
            if push.status_every.is_some_and(|every| every.0 < HOUR) {
                return Err("the status message needs an interval of at least 1 h".into());
            }
            let valid_device = |d: &Device| {
                !d.name.trim().is_empty()
                    && d.endpoint.starts_with("https://")
                    && crate::push::valid_public_key(&d.p256dh)
                    && crate::push::decode(&d.auth).is_some_and(|auth| auth.len() == 16)
            };
            if let Some(device) = push.devices.iter().find(|d| !valid_device(d)) {
                return Err(format!("the device {:?} has an invalid subscription, remove it and enable it again", device.name));
            }
        }
        let retention = &shared.retention;
        if retention.raw.0 < HOUR || retention.medium.0 < retention.raw.0 || retention.long.0 < retention.medium.0 {
            return Err("retention needs raw of at least 1 h and raw ≤ medium ≤ long".into());
        }
        if shared.backup_max_age.0 < HOUR {
            return Err("the backup age limit needs at least 1 h".into());
        }
        if self.zerobyte.as_ref().is_some_and(|z| z.interval.0 < 30 * SECOND) {
            return Err("the ZeroByte poll interval needs at least 30 s".into());
        }
        Ok(())
    }

    pub fn redacted(&self) -> Config {
        let mut config = self.clone();
        let hide = |value: &mut String| *value = HIDDEN.to_string();
        if let Some(dashboard) = config.dashboard.as_mut() {
            hide(&mut dashboard.password);
        }
        if let Some(peer) = config.peer.as_mut() {
            hide(&mut peer.token);
        }
        if let Some(zerobyte) = config.zerobyte.as_mut() {
            zerobyte.webhook_token.iter_mut().chain(zerobyte.api_key.iter_mut()).for_each(hide);
        }
        config.shared.agents.values_mut().for_each(hide);
        if let Some(push) = config.shared.push.as_mut().filter(|p| !p.private_key.is_empty()) {
            hide(&mut push.private_key);
        }
        config
    }

    pub fn restore(&mut self, current: &Config) -> Result<(), String> {
        if let Some(dashboard) = self.dashboard.as_mut() {
            keep(&mut dashboard.password, current.dashboard.as_ref().map(|d| &d.password), "the dashboard password")?;
        }
        if let Some(peer) = self.peer.as_mut() {
            keep(&mut peer.token, current.peer.as_ref().map(|p| &p.token), "the peer token")?;
        }
        if let Some(zerobyte) = self.zerobyte.as_mut() {
            let stored = current.zerobyte.as_ref();
            keep_option(&mut zerobyte.webhook_token, stored.and_then(|z| z.webhook_token.as_ref()), "the ZeroByte webhook token")?;
            keep_option(&mut zerobyte.api_key, stored.and_then(|z| z.api_key.as_ref()), "the ZeroByte API key")?;
        }
        for (host, token) in self.shared.agents.iter_mut() {
            keep(token, current.shared.agents.get(host), &format!("the token of {host}"))?;
        }
        if let Some(push) = self.shared.push.as_mut() {
            keep(&mut push.private_key, current.shared.push.as_ref().map(|p| &p.private_key), "the push private key")?;
        }
        Ok(())
    }

    pub fn peer_name(&self) -> Option<&str> {
        self.peer.as_ref().map(|p| p.name.as_str())
    }

    pub fn initiates_sync(&self) -> bool {
        self.peer.as_ref().is_some_and(|p| p.url.is_some())
    }
}

fn rule(name: &str, metric: &str, above: f64, hold: i64) -> Rule {
    Rule {
        name: name.to_string(),
        metric: metric.to_string(),
        hosts: all_hosts(),
        above: Some(above),
        below: None,
        hold: Span(hold),
        severity: Severity::Warning,
    }
}

pub fn default_rules() -> Vec<Rule> {
    vec![
        rule("CPU", "cpu||total", 85.0, 5 * MINUTE),
        rule("Memory", "memory||used", 85.0, 5 * MINUTE),
        rule("Disk", "fs|*|used", 85.0, 0),
        rule("Load", "load||5", 10.0, 20 * MINUTE),
        rule("Temperature", "temp|*|value", 80.0, 5 * MINUTE),
    ]
}

fn default_listen() -> String {
    "0.0.0.0:8090".to_string()
}

fn default_database() -> String {
    "argus.db".to_string()
}

fn enabled() -> bool {
    true
}

fn raw_retention() -> Span {
    Span(7 * DAY)
}

fn medium_retention() -> Span {
    Span(90 * DAY)
}

fn long_retention() -> Span {
    Span(730 * DAY)
}

fn backup_age() -> Span {
    Span(25 * HOUR)
}

fn poll_interval() -> Span {
    Span(5 * MINUTE)
}

fn all_hosts() -> Vec<String> {
    vec!["*".to_string()]
}

fn warning() -> Severity {
    Severity::Warning
}

#[cfg(test)]
mod tests {
    use super::*;

    fn example() -> Config {
        Config::parse(include_str!("../node.example.json")).expect("valid example")
    }

    #[test]
    fn parses_and_validates_the_example() {
        let config = example();
        assert_eq!(config.shared.retention.raw, Span(7 * DAY));
        assert!(config.initiates_sync());
        assert!(config.shared.rules.iter().any(|r| r.hold == Span(20 * MINUTE)));
        assert_eq!(config.shared.checks.values().flatten().count(), 2);
        let mut normalized = config.clone();
        normalized.normalize().expect("normalizes");
        normalized.validate().expect("valid");
    }

    #[test]
    fn defaults_fill_a_minimal_file_and_survive_a_round_trip() {
        let config = Config::default();
        assert_eq!(config.shared.rules.len(), 5);
        let text = serde_json::to_string(&config).expect("serializes");
        assert!(text.contains(r#""raw":"7d""#));
        assert_eq!(Config::parse(&text).expect("parses"), config);
        assert!(Config::parse(r#"{"name":"home","shared":{"rules":[]}}"#).expect("parses").shared.rules.is_empty());
    }

    #[test]
    fn rejects_unknown_fields_and_bad_values() {
        assert!(Config::parse(r#"{"name":"home","listen_port":1}"#).is_err());
        let mut config = Config::default();
        config.shared.checks.insert("ghost".into(), Vec::new());
        assert!(config.validate().is_err());
        let mut config = Config::default();
        config.shared.agents.insert("web".into(), "x".into());
        config.shared.checks.insert("web".into(), vec![serde_json::from_str(r#"{"name":"Run","type":"exec","command":["id"]}"#).expect("check")]);
        assert!(config.validate().expect_err("rejected").contains("only http"));
    }

    #[test]
    fn plain_secrets_are_hashed_and_hidden_ones_kept() {
        let mut current = Config { dashboard: Some(Dashboard { password: "secret".into() }), ..Default::default() };
        current.shared.agents.insert("web".into(), "token-in-clear".into());
        current.shared.push = Some(Push { subject: "mailto:admin@example.net".into(), ..Default::default() });
        current.normalize().expect("normalizes");
        assert!(current.dashboard.as_ref().is_some_and(|d| d.password.starts_with("$argon2")));
        assert_eq!(current.shared.agents["web"], sha256_hex(b"token-in-clear"));
        current.validate().expect("valid with generated push keys");
        let mut incoming = current.redacted();
        assert_eq!(incoming.shared.agents["web"], HIDDEN);
        assert_eq!(incoming.shared.push.as_ref().map(|p| p.private_key.as_str()), Some(HIDDEN));
        incoming.shared.agents.insert("new".into(), HIDDEN.into());
        assert!(incoming.clone().restore(&current).is_err());
        incoming.shared.agents.remove("new");
        incoming.restore(&current).expect("restores");
        assert_eq!(incoming, current);
    }
}
