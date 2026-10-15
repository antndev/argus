use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use argus_model::Container;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Stored {
    #[serde(rename = "ID")]
    id: String,
    name: String,
    #[serde(default)]
    restart_count: u32,
    state: State,
    config: Settings,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Settings {
    #[serde(default)]
    image: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct State {
    running: bool,
    paused: bool,
    restarting: bool,
    dead: bool,
    exit_code: i64,
    started_at: String,
    finished_at: String,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Host {
    restart_policy: Policy,
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
struct Policy {
    name: String,
}

#[derive(Default)]
pub struct Docker {
    previous: HashMap<String, (u64, Instant)>,
}

pub struct Sample {
    pub containers: Vec<Container>,
    pub measured: HashSet<String>,
}

impl Docker {
    pub fn collect(&mut self, root: &str, data: &str) -> Result<Sample, String> {
        let directory = format!("{root}{}/containers", data.trim_end_matches('/'));
        let entries = fs::read_dir(&directory).map_err(|e| format!("cannot read {directory}: {e}"))?;
        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
        let mut current = HashMap::new();
        let mut measured = HashSet::new();
        let mut containers = Vec::new();
        for entry in entries.flatten() {
            let Some(stored) = fs::read_to_string(entry.path().join("config.v2.json")).ok().and_then(|text| serde_json::from_str::<Stored>(&text).ok()) else {
                continue;
            };
            let mut container = container(&stored, now);
            container.restart_policy = fs::read_to_string(entry.path().join("hostconfig.json"))
                .ok()
                .and_then(|text| serde_json::from_str::<Host>(&text).ok())
                .map(|host| host.restart_policy.name)
                .unwrap_or_default();
            if stored.state.running {
                let cgroup = cgroup(root, &stored.id);
                container.memory_bytes = cgroup.as_deref().and_then(memory_bytes).unwrap_or(0);
                if let Some(usage) = cgroup.as_deref().and_then(cpu_usage) {
                    let sample = (usage, Instant::now());
                    if let Some(&(previous, at)) = self.previous.get(&stored.id) {
                        let wall = sample.1.duration_since(at).as_micros() as f64;
                        container.cpu = if wall > 0.0 { usage.saturating_sub(previous) as f64 / wall * 100.0 } else { 0.0 };
                        measured.insert(container.name.clone());
                    }
                    current.insert(stored.id.clone(), sample);
                }
            }
            containers.push(container);
        }
        self.previous = current;
        containers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Sample { containers, measured })
    }
}

fn container(stored: &Stored, now: i64) -> Container {
    let state = &stored.state;
    let started = rfc3339(&state.started_at);
    let finished = rfc3339(&state.finished_at);
    let since = |at: Option<i64>| at.map(|t| ago(now - t)).unwrap_or_default();
    let (name, status) = match () {
        _ if state.running && state.paused => ("paused", format!("Up {} (paused)", since(started))),
        _ if state.restarting => ("restarting", format!("Restarting ({}) {} ago", state.exit_code, since(finished))),
        _ if state.running => ("running", format!("Up {}", since(started))),
        _ if state.dead => ("dead", "Dead".to_string()),
        _ if started.is_none() => ("created", "Created".to_string()),
        _ => ("exited", format!("Exited ({}) {} ago", state.exit_code, since(finished))),
    };
    Container {
        name: stored.name.trim_start_matches('/').to_string(),
        image: stored.config.image.clone(),
        state: name.to_string(),
        status,
        restart_policy: String::new(),
        restarts: stored.restart_count,
        cpu: 0.0,
        memory_bytes: 0,
    }
}

fn cgroup(root: &str, id: &str) -> Option<String> {
    [format!("{root}/sys/fs/cgroup/system.slice/docker-{id}.scope"), format!("{root}/sys/fs/cgroup/docker/{id}")]
        .into_iter()
        .find(|path| Path::new(path).is_dir())
}

fn memory_bytes(cgroup: &str) -> Option<u64> {
    let current: u64 = fs::read_to_string(format!("{cgroup}/memory.current")).ok()?.trim().parse().ok()?;
    let stat = fs::read_to_string(format!("{cgroup}/memory.stat")).unwrap_or_default();
    Some(current.saturating_sub(field(&stat, "inactive_file").unwrap_or(0)))
}

fn cpu_usage(cgroup: &str) -> Option<u64> {
    field(&fs::read_to_string(format!("{cgroup}/cpu.stat")).ok()?, "usage_usec")
}

fn field(text: &str, name: &str) -> Option<u64> {
    text.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix(' ')?.trim().parse().ok())
}

fn rfc3339(text: &str) -> Option<i64> {
    let (date, time) = text.split_once('T')?;
    let mut date = date.split('-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (date.next()??, date.next()??, date.next()??);
    let mut clock = time.trim_end_matches('Z').split(':');
    let (hour, minute) = (clock.next()?.parse::<i64>().ok()?, clock.next()?.parse::<i64>().ok()?);
    let second = clock.next()?.split('.').next()?.parse::<i64>().ok()?;
    if year < 1970 {
        return None;
    }
    let shifted = if month <= 2 { year - 1 } else { year };
    let era = shifted.div_euclid(400);
    let year_of_era = shifted - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days = era * 146_097 + day_of_era - 719_468;
    Some(days * 86_400 + hour * 3_600 + minute * 60 + second)
}

fn ago(seconds: i64) -> String {
    let seconds = seconds.max(0);
    let (count, unit) = match seconds {
        0..60 => (seconds, "second"),
        60..3_600 => (seconds / 60, "minute"),
        3_600..172_800 => (seconds / 3_600, "hour"),
        172_800..1_209_600 => (seconds / 86_400, "day"),
        1_209_600..5_184_000 => (seconds / 604_800, "week"),
        _ => (seconds / 2_592_000, "month"),
    };
    format!("{count} {unit}{}", if count == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_state_becomes_a_container() {
        let json = r#"{"ID":"abc","Name":"/vaultwarden","RestartCount":2,"Config":{"Image":"vaultwarden/server:1.34"},
            "State":{"Running":true,"Paused":false,"Restarting":false,"Dead":false,"ExitCode":0,
            "StartedAt":"2026-10-14T08:00:00.123456789Z","FinishedAt":"0001-01-01T00:00:00Z"}}"#;
        let stored: Stored = serde_json::from_str(json).expect("parses");
        let now = rfc3339("2026-10-14T13:30:00Z").expect("time");
        let running = container(&stored, now);
        assert_eq!((running.name.as_str(), running.state.as_str(), running.status.as_str()), ("vaultwarden", "running", "Up 5 hours"));
        assert_eq!(running.restarts, 2);
        let exited = r#"{"ID":"b","Name":"/dump","Config":{"Image":"alpine"},"State":{"ExitCode":1,"StartedAt":"2026-10-14T08:00:00Z","FinishedAt":"2026-10-14T13:00:00Z"}}"#;
        let exited = container(&serde_json::from_str(exited).expect("parses"), now);
        assert_eq!((exited.state.as_str(), exited.status.as_str()), ("exited", "Exited (1) 30 minutes ago"));
        let created = r#"{"ID":"c","Name":"/new","Config":{},"State":{"StartedAt":"0001-01-01T00:00:00Z"}}"#;
        assert_eq!(container(&serde_json::from_str(created).expect("parses"), now).state, "created");
    }

    #[test]
    fn cgroup_files_and_times() {
        assert_eq!(field("usage_usec 1234\nuser_usec 1000\n", "usage_usec"), Some(1234));
        assert_eq!(field("anon 5\ninactive_file 300\n", "inactive_file"), Some(300));
        let host: Host = serde_json::from_str(r#"{"RestartPolicy":{"Name":"unless-stopped","MaximumRetryCount":0},"Binds":[]}"#).expect("parses");
        assert_eq!(host.restart_policy.name, "unless-stopped");
        assert_eq!(rfc3339("1970-01-02T00:00:01Z"), Some(86_401));
        assert_eq!(rfc3339("2024-02-29T12:00:00.5Z"), Some(1_709_208_000));
        assert_eq!(rfc3339("0001-01-01T00:00:00Z"), None);
        assert_eq!(ago(59), "59 seconds");
        assert_eq!(ago(3_600), "1 hour");
        assert_eq!(ago(3 * 86_400), "3 days");
    }
}
