use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use argus_model::{Check, CheckKind};
use serde::Deserialize;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub id: Option<String>,
    pub site: String,
    #[serde(default)]
    pub role: String,
    #[serde(default)]
    pub os: Option<String>,
    #[serde(default = "default_interval", deserialize_with = "argus_model::duration::deserialize")]
    pub interval: Duration,
    #[serde(default)]
    pub root: String,
    #[serde(default = "default_agent")]
    pub agent: String,
    #[serde(default)]
    pub nodes: Vec<Node>,
    #[serde(default)]
    pub collectors: Collectors,
    #[serde(default)]
    pub checks: Vec<Check>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Node {
    pub url: String,
    pub token: Option<String>,
    pub token_file: Option<PathBuf>,
    #[serde(default = "accept")]
    pub checks: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Collectors {
    pub docker: Option<String>,
    pub systemd: bool,
    pub smart: Vec<String>,
    pub filesystems: Vec<String>,
    pub ignore_interfaces: Vec<String>,
    pub interfaces: HashMap<String, String>,
}

impl Default for Collectors {
    fn default() -> Self {
        Self {
            docker: None,
            systemd: false,
            smart: Vec::new(),
            filesystems: Vec::new(),
            ignore_interfaces: ["lo", "veth", "docker", "br-"].map(String::from).to_vec(),
            interfaces: HashMap::new(),
        }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        Self::parse(&text).map_err(|e| format!("invalid config {}: {e}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = if text.trim_start().starts_with('{') {
            serde_json::from_str(text).map_err(|e| e.to_string())?
        } else {
            toml::from_str(text).map_err(|e| e.to_string())?
        };
        if config.interval.is_zero() {
            return Err("interval must be at least 1 s".into());
        }
        if let Some(check) = config.checks.iter().find(|c| matches!(&c.kind, CheckKind::Exec { command, .. } if command.is_empty())) {
            return Err(format!("check {:?} has an empty command", check.name));
        }
        if let Some(check) = config.checks.iter().find(|c| c.name.contains(',')) {
            return Err(format!("check {:?} has a comma in its name", check.name));
        }
        for check in &config.checks {
            let Some(timeout) = check.kind.timeout() else {
                continue;
            };
            if timeout < Duration::from_secs(1) || timeout >= config.interval {
                return Err(format!("check {:?} needs a timeout of at least 1 s and below the interval", check.name));
            }
        }
        Ok(config)
    }
}

impl Node {
    pub fn resolve_token(&self) -> Result<String, String> {
        let token = match (&self.token, &self.token_file) {
            (Some(token), None) => token.clone(),
            (None, Some(file)) => {
                std::fs::read_to_string(file).map(|t| t.trim().to_string()).map_err(|e| format!("cannot read token file {}: {e}", file.display()))?
            }
            _ => {
                return Err(format!("node {} needs exactly one of token or token_file", self.url));
            }
        };
        if token.is_empty() || !token.chars().all(|c| c.is_ascii_graphic()) {
            return Err(format!("token for node {} must be non-empty printable ASCII without spaces", self.url));
        }
        Ok(token)
    }
}

fn default_interval() -> Duration {
    Duration::from_secs(30)
}

fn default_agent() -> String {
    "native".into()
}

fn accept() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_timeouts_must_fit_the_interval() {
        let base = "site = \"home\"\ninterval = 30\n[[checks]]\nname = \"T\"\ntype = \"tcp\"\nhost = \"h\"\nport = 1\n";
        assert!(Config::parse(base).is_ok());
        assert!(Config::parse(&format!("{base}timeout = 0\n")).is_err());
        assert!(Config::parse(&format!("{base}timeout = \"5m\"\n")).is_err());
    }

    #[test]
    fn home_assistant_options_json_parses() {
        let options = r#"{"id":"homeassistant","site":"home","agent":"addon","interval":30,"root":"","nodes":[{"url":"https://argus.internal.example.com","token":"abc"}]}"#;
        let config = Config::parse(options).expect("options parse");
        assert_eq!(config.id.as_deref(), Some("homeassistant"));
        assert_eq!(config.nodes.len(), 1);
    }

    #[test]
    fn example_config_parses() {
        let config = Config::parse(include_str!("../agent.example.toml")).expect("example parses");
        assert_eq!(config.id.as_deref(), Some("server-1"));
        assert_eq!(config.interval, Duration::from_secs(30));
        assert_eq!(config.nodes.len(), 1);
        assert_eq!(config.collectors.interfaces.get("pppoe0").map(String::as_str), Some("WAN"));
        assert_eq!(config.checks.len(), 5);
        match &config.checks[0].kind {
            CheckKind::Http { status, json, timeout, .. } => {
                assert_eq!(*status, Some(200));
                assert_eq!(json[0].equals, serde_json::json!("ok"));
                assert_eq!(*timeout, Duration::from_secs(10));
            }
            other => panic!("unexpected {other:?}"),
        }
        assert!(matches!(&config.checks[1].kind, CheckKind::Tcp { port: 25565, .. }));
        assert!(matches!(&config.checks[3].kind, CheckKind::FileAge { max_age, .. } if max_age.as_secs() == 129_600));
    }

    #[test]
    fn minimal_config_uses_defaults() {
        let config = Config::parse("site = \"home\"\ninterval = \"1m\"").expect("parses");
        assert_eq!(config.interval, Duration::from_secs(60));
        assert_eq!(config.agent, "native");
        assert!(config.collectors.ignore_interfaces.contains(&"veth".to_string()));
        assert!(config.checks.is_empty());
    }

    #[test]
    fn rejects_mistakes() {
        assert!(Config::parse("site = \"home\"\nintervall = 30").is_err());
        assert!(Config::parse("interval = 30").is_err());
        assert!(Config::parse("site = \"home\"\n[[checks]]\nname = \"x\"\ntype = \"smtp\"").is_err());
        assert!(Config::parse("site = \"home\"\n[[checks]]\nname = \"x\"\ntype = \"exec\"\ncommand = []").is_err());
        assert!(Config::parse("site = \"home\"\n[[checks]]\nname = \"x\"\ntype = \"file_age\"\npath = \"/x\"\nmax_age = \"soon\"").is_err());
    }

    #[test]
    fn token_needs_exactly_one_source() {
        let node = Node { url: "https://a".into(), token: Some("t".into()), token_file: None, checks: true };
        assert_eq!(node.resolve_token(), Ok("t".into()));
        let none = Node { url: "https://a".into(), token: None, token_file: None, checks: true };
        assert!(none.resolve_token().is_err());
        let spaced = Node { url: "https://a".into(), token: Some("a b".into()), token_file: None, checks: true };
        assert!(spaced.resolve_token().is_err());
    }
}
