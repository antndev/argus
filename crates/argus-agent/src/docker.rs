use std::collections::HashMap;
use std::time::{Duration, Instant};

use argus_model::Container;
use serde::Deserialize;
use serde::de::DeserializeOwned;

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Summary {
    id: String,
    #[serde(default)]
    names: Vec<String>,
    #[serde(default)]
    image: String,
    #[serde(default)]
    state: String,
    #[serde(default)]
    status: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Details {
    restart_count: u32,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct Stats {
    cpu_stats: CpuStats,
    memory_stats: MemoryStats,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CpuStats {
    cpu_usage: CpuUsage,
    system_cpu_usage: u64,
    online_cpus: u32,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct CpuUsage {
    total_usage: u64,
}

#[derive(Default, Deserialize)]
#[serde(default)]
struct MemoryStats {
    usage: u64,
    stats: HashMap<String, u64>,
}

const BUDGET: Duration = Duration::from_secs(8);

#[derive(Default)]
pub struct Docker {
    previous: HashMap<String, (u64, u64)>,
}

impl Docker {
    pub fn collect(&mut self, agent: &ureq::Agent, url: &str) -> Result<Vec<Container>, String> {
        let base = url.trim_end_matches('/');
        let summaries: Vec<Summary> = get(agent, &format!("{base}/containers/json?all=true"))?;
        let mut current = HashMap::new();
        let mut containers = Vec::with_capacity(summaries.len());
        let deadline = Instant::now() + BUDGET;
        for summary in summaries {
            let in_time = Instant::now() < deadline;
            let restarts = if !in_time { 0 } else { get::<Details>(agent, &format!("{base}/containers/{}/json", summary.id)).map_or(0, |d| d.restart_count) };
            let mut container = Container {
                name: summary.names.first().map_or(summary.id.as_str(), |n| n.trim_start_matches('/')).to_string(),
                image: summary.image,
                state: summary.state,
                status: summary.status,
                restarts,
                cpu: 0.0,
                memory_bytes: 0,
            };
            if in_time
                && container.state == "running"
                && let Ok(stats) = get::<Stats>(agent, &format!("{base}/containers/{}/stats?stream=false&one-shot=true", summary.id))
            {
                let sample = (stats.cpu_stats.cpu_usage.total_usage, stats.cpu_stats.system_cpu_usage);
                if let Some(&previous) = self.previous.get(&summary.id) {
                    container.cpu = cpu_percent(previous, sample, stats.cpu_stats.online_cpus);
                }
                container.memory_bytes = memory_bytes(&stats.memory_stats);
                current.insert(summary.id, sample);
            }
            containers.push(container);
        }
        self.previous = current;
        containers.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(containers)
    }
}

fn get<T: DeserializeOwned>(agent: &ureq::Agent, url: &str) -> Result<T, String> {
    let mut response = agent.get(url).call().map_err(|e| e.to_string())?;
    let body = response.body_mut().read_to_string().map_err(|e| e.to_string())?;
    serde_json::from_str(&body).map_err(|e| e.to_string())
}

fn cpu_percent(previous: (u64, u64), current: (u64, u64), cpus: u32) -> f64 {
    let system = current.1.saturating_sub(previous.1);
    if system == 0 {
        return 0.0;
    }
    current.0.saturating_sub(previous.0) as f64 / system as f64 * f64::from(cpus.max(1)) * 100.0
}

fn memory_bytes(memory: &MemoryStats) -> u64 {
    let inactive = memory.stats.get("inactive_file").or_else(|| memory.stats.get("cache")).copied().unwrap_or(0);
    memory.usage.saturating_sub(inactive)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_cpu_and_memory() {
        let json = r#"{
            "read": "2026-10-12T12:00:00Z",
            "cpu_stats": {"cpu_usage": {"total_usage": 2000000000}, "system_cpu_usage": 120000000000, "online_cpus": 4},
            "precpu_stats": {"cpu_usage": {"total_usage": 0}},
            "memory_stats": {"usage": 314572800, "limit": 16000000000, "stats": {"inactive_file": 104857600, "active_file": 1}}
        }"#;
        let stats: Stats = serde_json::from_str(json).expect("parses");
        assert_eq!(memory_bytes(&stats.memory_stats), 209_715_200);
        let current = (stats.cpu_stats.cpu_usage.total_usage, stats.cpu_stats.system_cpu_usage);
        let percent = cpu_percent((1_000_000_000, 100_000_000_000), current, stats.cpu_stats.online_cpus);
        assert!((percent - 20.0).abs() < 1e-9);
        assert_eq!(cpu_percent(current, current, 4), 0.0);
        let cgroup_v1: MemoryStats = serde_json::from_str(r#"{"usage": 1000, "stats": {"cache": 300}}"#).expect("parses");
        assert_eq!(memory_bytes(&cgroup_v1), 700);
        let stopped: Stats = serde_json::from_str(r#"{"cpu_stats": {"cpu_usage": {}}, "memory_stats": {}}"#).expect("parses");
        assert_eq!(memory_bytes(&stopped.memory_stats), 0);
    }

    #[test]
    fn container_list() {
        let json = r#"[{"Id": "abc", "Names": ["/web"], "Image": "nginx:1", "State": "running", "Status": "Up 3 days", "Ports": []}]"#;
        let list: Vec<Summary> = serde_json::from_str(json).expect("parses");
        assert_eq!(list[0].names[0], "/web");
        assert_eq!(list[0].state, "running");
    }
}
