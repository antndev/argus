mod checks;
mod collect;
mod config;
mod docker;
#[cfg(any(target_os = "freebsd", test))]
mod freebsd_parse;
#[cfg(any(target_os = "linux", test))]
mod linux_parse;
#[cfg_attr(target_os = "linux", path = "linux.rs")]
#[cfg_attr(target_os = "freebsd", path = "freebsd.rs")]
#[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), path = "other.rs")]
mod platform;

use std::path::Path;
use std::process::exit;
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use argus_model::{Check, CheckKind, CheckStatus, IngestReply, Metric, Report};
use flate2::Compression;
use flate2::write::GzEncoder;

use crate::collect::Collector;
use crate::config::Config;

const DEFAULT_CONFIG: &str = "/etc/argus/agent.toml";
const USAGE: &str = "usage: argus-agent [run | once | install] [--config <path>]";

fn main() {
    let mut command = None;
    let mut config_path = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--config" => config_path = Some(arguments.next().unwrap_or_else(|| fail(USAGE))),
            "run" | "once" | "install" if command.is_none() => command = Some(argument),
            _ => fail(USAGE),
        }
    }
    let path = config_path.or_else(|| std::env::var("ARGUS_AGENT_CONFIG").ok()).unwrap_or_else(|| DEFAULT_CONFIG.into());
    match command.as_deref() {
        Some("install") => install(&path),
        Some("once") => once(&load(&path)),
        _ => run(&load(&path)),
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    exit(2)
}

fn load(path: &str) -> Config {
    Config::load(Path::new(path)).unwrap_or_else(|error| fail(&error))
}

#[cfg(target_os = "freebsd")]
fn install(config: &str) {
    platform::install(config);
}

#[cfg(not(target_os = "freebsd"))]
fn install(_config: &str) {
    fail("install only sets up the rc.d service on FreeBSD, use the container image or the Home Assistant add-on elsewhere");
}

fn once(config: &Config) {
    let mut collector = Collector::new();
    collector.collect(config);
    thread::sleep(Duration::from_secs(1));
    match serde_json::to_string_pretty(&report(config, &config.checks, &mut collector)) {
        Ok(json) => println!("{json}"),
        Err(error) => fail(&error.to_string()),
    }
}

fn run(config: &Config) -> ! {
    if config.nodes.is_empty() {
        fail("no nodes configured");
    }
    let nodes: Vec<Target> = config
        .nodes
        .iter()
        .map(|node| node.resolve_token().map(|token| Target { url: format!("{}/api/v1/ingest", node.url.trim_end_matches('/')), token, checks: node.checks }))
        .collect::<Result<_, _>>()
        .unwrap_or_else(|error| fail(&error));
    for Target { url, .. } in nodes.iter().filter(|node| clear_text(&node.url)) {
        log(&format!("warning: {url} is not HTTPS, the token is sent in clear text"));
    }
    let http: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).http_status_as_error(false).build().into();
    let mut collector = Collector::new();
    let mut checks = config.checks.clone();
    log(&format!("argus-agent {} reporting to {} node(s) every {} s", env!("CARGO_PKG_VERSION"), nodes.len(), config.interval.as_secs()));
    loop {
        match encode(&report(config, &checks, &mut collector)) {
            Ok(body) => {
                if let Some(remote) = send(&http, &nodes, &body) {
                    let merged = merge_checks(config, remote);
                    if merged != checks {
                        log(&format!("running {} checks, {} from the node", merged.len(), merged.len() - config.checks.len()));
                        checks = merged;
                    }
                }
            }
            Err(error) => log(&format!("cannot encode report: {error}")),
        }
        thread::sleep(until_tick(SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default(), config.interval));
    }
}

fn until_tick(now: Duration, interval: Duration) -> Duration {
    let step = interval.as_millis().max(1);
    Duration::from_millis((step - now.as_millis() % step) as u64)
}

struct Target {
    url: String,
    token: String,
    checks: bool,
}

const MAX_REMOTE_CHECKS: usize = 64;

fn merge_checks(config: &Config, remote: Vec<Check>) -> Vec<Check> {
    let ceiling = config.interval.saturating_sub(Duration::from_secs(1)).max(Duration::from_secs(1));
    let mut checks = config.checks.clone();
    for mut check in remote.into_iter().take(MAX_REMOTE_CHECKS) {
        if !check.kind.remote() || checks.iter().any(|c| c.name == check.name) {
            continue;
        }
        if let CheckKind::Http { timeout, .. } | CheckKind::Tcp { timeout, .. } = &mut check.kind {
            *timeout = (*timeout).clamp(Duration::from_secs(1), ceiling);
        }
        checks.push(check);
    }
    checks
}

fn report(config: &Config, checks: &[Check], collector: &mut Collector) -> Report {
    let (checks, (host, mut metrics, inventory)) = thread::scope(|scope| {
        let checks = scope.spawn(|| checks::run_all(checks));
        let collected = collector.collect(config);
        (checks.join().unwrap_or_default(), collected)
    });
    for check in &checks {
        metrics.push(Metric::new("check", &check.name, "ok", if check.status == CheckStatus::Failed { 0.0 } else { 1.0 }));
        if let Some(latency) = check.latency_ms {
            metrics.push(Metric::new("check", &check.name, "latency", latency));
        }
    }
    let time = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64);
    Report { host, time, metrics, inventory, checks }
}

fn encode(report: &Report) -> std::io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    serde_json::to_writer(&mut encoder, report)?;
    encoder.finish()
}

fn send(http: &ureq::Agent, nodes: &[Target], body: &[u8]) -> Option<Vec<Check>> {
    thread::scope(|scope| {
        let replies: Vec<_> = nodes
            .iter()
            .map(|node| {
                scope.spawn(move || {
                    let sent = http
                        .post(&node.url)
                        .header("Content-Type", "application/json")
                        .header("Content-Encoding", "gzip")
                        .header("Authorization", format!("Bearer {}", node.token))
                        .send(body);
                    let reply = match sent {
                        Ok(mut response) => {
                            let status = response.status().as_u16();
                            let body = response.body_mut().with_config().limit(1024 * 1024).read_to_vec().unwrap_or_default();
                            node_checks(node.checks, status, &body)
                        }
                        Err(error) => Err(error.to_string()),
                    };
                    reply.unwrap_or_else(|error| {
                        log(&format!("{}: {error}", node.url));
                        None
                    })
                })
            })
            .collect();
        replies.into_iter().find_map(|reply| reply.join().ok().flatten())
    })
}

fn node_checks(accept: bool, status: u16, body: &[u8]) -> Result<Option<Vec<Check>>, String> {
    if status != 200 {
        let text = String::from_utf8_lossy(&body[..body.len().min(200)]).trim().to_string();
        return Err(if text.is_empty() { format!("status {status}") } else { format!("status {status}, {text}") });
    }
    if !accept {
        return Ok(None);
    }
    serde_json::from_slice::<IngestReply>(body).map(|reply| Some(reply.checks)).map_err(|error| format!("unreadable reply, {error}"))
}

pub fn log(message: &str) {
    let seconds = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    eprintln!("{} {message}", timestamp(seconds));
}

fn timestamp(seconds: u64) -> String {
    let days = (seconds / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let shifted_month = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * shifted_month + 2) / 5 + 1;
    let month = if shifted_month < 10 { shifted_month + 3 } else { shifted_month - 9 };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", seconds % 86_400 / 3600, seconds % 3600 / 60, seconds % 60)
}

fn clear_text(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else { return !url.starts_with("https://") };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
    let host = if host.starts_with('[') { host.split(']').next().map_or(host, |h| &h[1..]) } else { host.split(':').next().unwrap_or(host) };
    !(host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_start_on_the_clock() {
        let interval = Duration::from_secs(30);
        assert_eq!(until_tick(Duration::from_millis(1_700_000_012_300), interval), Duration::from_millis(27_700));
        assert_eq!(until_tick(Duration::from_secs(1_700_000_010), interval), interval);
        assert_eq!(until_tick(Duration::from_secs(301), Duration::from_secs(300)), Duration::from_secs(299));
    }

    #[test]
    fn loopback_is_not_clear_text() {
        assert!(!clear_text("https://argus.example.com/api/v1/ingest"));
        assert!(!clear_text("http://127.0.0.1:8090/api/v1/ingest"));
        assert!(!clear_text("http://localhost/api/v1/ingest"));
        assert!(!clear_text("http://[::1]:8090/api/v1/ingest"));
        assert!(clear_text("http://10.0.0.5:8090/api/v1/ingest"));
        assert!(clear_text("http://127.0.0.1.example.com/api/v1/ingest"));
    }

    #[test]
    fn timestamps() {
        assert_eq!(timestamp(0), "1970-01-01T00:00:00Z");
        assert_eq!(timestamp(1_700_000_000), "2023-11-14T22:13:20Z");
        assert_eq!(timestamp(951_782_400), "2000-02-29T00:00:00Z");
    }

    #[test]
    fn node_checks_never_run_commands_or_shadow_local_ones() {
        let config = Config::parse("site = \"home\"\ninterval = 30\n[[checks]]\nname = \"Web\"\ntype = \"ping\"\nhost = \"a\"").expect("config");
        let remote: Vec<Check> = serde_json::from_str(
            r#"[{"name":"Web","type":"ping","host":"b"},{"name":"Run","type":"exec","command":["id"]},{"name":"Slow","type":"tcp","host":"c","port":1,"timeout":"5m"}]"#,
        )
        .expect("checks");
        let merged = merge_checks(&config, remote);
        assert_eq!(merged.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["Web", "Slow"]);
        assert_eq!(merged[1].kind.timeout(), Some(Duration::from_secs(29)));
    }

    #[test]
    fn node_checks_follow_the_checks_switch_and_surface_problems() {
        let reply = br#"{"checks":[{"name":"Web","type":"ping","host":"a"}]}"#;
        assert_eq!(node_checks(true, 200, reply).expect("ok").map(|c| c.len()), Some(1));
        assert_eq!(node_checks(false, 200, reply).expect("ok"), None);
        assert!(node_checks(true, 200, b"{\"checks\":[{\"name\":\"X\",\"type\":\"teleport\"}]}").is_err());
        assert_eq!(node_checks(true, 422, b"unknown site").expect_err("fails"), "status 422, unknown site");
    }

    #[test]
    fn report_is_gzip_json() {
        let report = Report { time: 42, ..Default::default() };
        let compressed = encode(&report).expect("encodes");
        let decoded: Report = serde_json::from_reader(flate2::read::GzDecoder::new(&compressed[..])).expect("decodes");
        assert_eq!(decoded, report);
    }
}
