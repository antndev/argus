use std::io::{ErrorKind, Read};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use argus_model::{Check, CheckKind, CheckResult, CheckStatus, JsonAssertion};
use serde::Deserialize;
use serde_json::Value;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};

const PING_COUNT: usize = 3;
const PING_TIMEOUT: Duration = Duration::from_secs(2);

pub fn run_all(checks: &[Check]) -> Vec<CheckResult> {
    let (sender, receiver) = mpsc::channel();
    for (index, check) in checks.iter().enumerate() {
        let sender = sender.clone();
        let check = check.clone();
        thread::spawn(move || {
            let _ = sender.send((index, run(&check)));
        });
    }
    drop(sender);
    let deadline = Instant::now() + checks.iter().map(timeout).max().unwrap_or_default() + Duration::from_secs(2);
    let mut results: Vec<CheckResult> = checks.iter().map(|check| describe(check, outcome(CheckStatus::Failed, "Timed out"))).collect();
    while let Ok((index, result)) = receiver.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        results[index] = result;
    }
    results
}

fn run(check: &Check) -> CheckResult {
    let result = match &check.kind {
        CheckKind::Http { url, status, contains, json, timeout } => http(url, *status, contains.as_deref(), json, *timeout),
        CheckKind::Tcp { host, port, timeout } => tcp(host, *port, *timeout),
        CheckKind::Ping { host } => ping(host),
        CheckKind::FileAge { path, max_age } => file_age(path, *max_age),
        CheckKind::Exec { command, timeout } => exec(command, *timeout),
    };
    describe(check, result)
}

fn describe(check: &Check, result: CheckResult) -> CheckResult {
    let (kind, target) = match &check.kind {
        CheckKind::Http { url, .. } => ("http", url.clone()),
        CheckKind::Tcp { host, port, .. } => ("tcp", format!("{host}:{port}")),
        CheckKind::Ping { host } => ("ping", host.clone()),
        CheckKind::FileAge { path, .. } => ("file_age", path.display().to_string()),
        CheckKind::Exec { command, .. } => ("exec", command.join(" ")),
    };
    CheckResult { name: check.name.clone(), kind: kind.into(), target, ..result }
}

fn timeout(check: &Check) -> Duration {
    match &check.kind {
        CheckKind::Http { timeout, .. } | CheckKind::Tcp { timeout, .. } | CheckKind::Exec { timeout, .. } => *timeout,
        CheckKind::Ping { .. } => PING_TIMEOUT * PING_COUNT as u32 + Duration::from_secs(2),
        CheckKind::FileAge { .. } => Duration::from_secs(5),
    }
}

fn outcome(status: CheckStatus, message: impl Into<String>) -> CheckResult {
    CheckResult { status, message: message.into(), ..Default::default() }
}

fn milliseconds(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn io_text(error: &std::io::Error) -> String {
    match error.kind() {
        ErrorKind::ConnectionRefused => "Connection refused".into(),
        ErrorKind::ConnectionReset => "Connection reset".into(),
        ErrorKind::TimedOut | ErrorKind::WouldBlock => "Timed out".into(),
        ErrorKind::PermissionDenied => "Permission denied".into(),
        ErrorKind::NotFound => "Not found".into(),
        _ => error.to_string(),
    }
}

fn http_text(error: &ureq::Error) -> String {
    match error {
        ureq::Error::Timeout(_) => "Timed out".into(),
        ureq::Error::HostNotFound => "Host not found".into(),
        ureq::Error::Io(error) => io_text(error),
        other => other.to_string(),
    }
}

fn http(url: &str, expected: Option<u16>, contains: Option<&str>, json: &[JsonAssertion], timeout: Duration) -> CheckResult {
    let agent: ureq::Agent = ureq::Agent::config_builder().timeout_global(Some(timeout)).http_status_as_error(false).build().into();
    let start = Instant::now();
    let mut response = match agent.get(url).call() {
        Ok(response) => response,
        Err(error) => return outcome(CheckStatus::Failed, http_text(&error)),
    };
    let latency = milliseconds(start.elapsed());
    let code = response.status().as_u16();
    let failure = if expected.map_or(!(200..400).contains(&code), |expected| expected != code) {
        Some(format!("Status {code}"))
    } else if contains.is_some() || !json.is_empty() {
        match response.body_mut().read_to_string() {
            Ok(body) => body_failure(&body, contains, json),
            Err(error) => Some(http_text(&error)),
        }
    } else {
        None
    };
    CheckResult {
        status: if failure.is_some() { CheckStatus::Failed } else { CheckStatus::Ok },
        message: failure.unwrap_or_else(|| format!("{code} in {latency:.0} ms")),
        latency_ms: Some(latency),
        ..Default::default()
    }
}

fn body_failure(body: &str, contains: Option<&str>, json: &[JsonAssertion]) -> Option<String> {
    if let Some(text) = contains
        && !body.contains(text)
    {
        return Some(format!("Body does not contain {text:?}"));
    }
    if json.is_empty() {
        return None;
    }
    let Ok(document) = serde_json::from_str::<Value>(body) else {
        return Some("Body is not JSON".into());
    };
    json.iter().find_map(|assertion| {
        let actual = lookup(&document, &assertion.path);
        let equal = match (actual, &assertion.equals) {
            (Some(Value::Number(a)), Value::Number(b)) => a.as_f64() == b.as_f64(),
            (Some(a), b) => a == b,
            (None, _) => false,
        };
        (!equal).then(|| match actual {
            Some(_) => format!("{} is not {}", assertion.path, assertion.equals),
            None => format!("{} is missing", assertion.path),
        })
    })
}

fn lookup<'a>(document: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(document, |value, key| match value {
        Value::Array(items) => items.get(key.parse::<usize>().ok()?),
        _ => value.get(key),
    })
}

fn tcp(host: &str, port: u16, timeout: Duration) -> CheckResult {
    let Ok(addresses) = (host, port).to_socket_addrs() else {
        return outcome(CheckStatus::Failed, "Host not found");
    };
    let addresses: Vec<_> = addresses.collect();
    let deadline = Instant::now() + timeout;
    let mut result = outcome(CheckStatus::Failed, "Host not found");
    for (index, address) in addresses.iter().enumerate() {
        let share = deadline.saturating_duration_since(Instant::now()) / (addresses.len() - index) as u32;
        if share.is_zero() {
            break;
        }
        let start = Instant::now();
        match TcpStream::connect_timeout(address, share) {
            Ok(_) => {
                let latency = milliseconds(start.elapsed());
                return CheckResult { message: format!("Connected in {latency:.0} ms"), latency_ms: Some(latency), ..outcome(CheckStatus::Ok, "") };
            }
            Err(error) => result = outcome(CheckStatus::Failed, io_text(&error)),
        }
    }
    result
}

fn ping(host: &str) -> CheckResult {
    let Some(address) = (host, 0).to_socket_addrs().ok().and_then(|mut a| a.next()) else {
        return outcome(CheckStatus::Failed, "Host not found");
    };
    let (domain, protocol, request, reply) =
        if address.is_ipv4() { (Domain::IPV4, Protocol::ICMPV4, 8, 0) } else { (Domain::IPV6, Protocol::ICMPV6, 128, 129) };
    let socket = match Socket::new(domain, Type::DGRAM, Some(protocol)).or_else(|_| Socket::new(domain, Type::RAW, Some(protocol))) {
        Ok(socket) => socket,
        Err(error) => return outcome(CheckStatus::Failed, io_text(&error)),
    };
    let target = SockAddr::from(address);
    let token = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos().to_be_bytes();
    let mut round_trips = Vec::new();
    for sequence in 0..PING_COUNT as u16 {
        let start = Instant::now();
        let packet = echo_request(request, sequence, &token);
        if socket.send_to(&packet, &target).is_ok() && wait_reply(&socket, reply, sequence, &token, start + PING_TIMEOUT) {
            round_trips.push(milliseconds(start.elapsed()));
        }
        if usize::from(sequence) + 1 < PING_COUNT {
            thread::sleep(Duration::from_secs(1).saturating_sub(start.elapsed()));
        }
    }
    let status = match round_trips.len() {
        0 => CheckStatus::Failed,
        PING_COUNT => CheckStatus::Ok,
        _ => CheckStatus::Warning,
    };
    CheckResult {
        status,
        message: format!("{} of {PING_COUNT} replies", round_trips.len()),
        latency_ms: (!round_trips.is_empty()).then(|| round_trips.iter().sum::<f64>() / round_trips.len() as f64),
        ..Default::default()
    }
}

fn echo_request(kind: u8, sequence: u16, payload: &[u8]) -> Vec<u8> {
    let mut packet = vec![kind, 0, 0, 0];
    packet.extend_from_slice(&(std::process::id() as u16).to_be_bytes());
    packet.extend_from_slice(&sequence.to_be_bytes());
    packet.extend_from_slice(payload);
    let checksum = checksum(&packet);
    packet[2..4].copy_from_slice(&checksum.to_be_bytes());
    packet
}

fn checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = data.chunks(2).map(|pair| u32::from(u16::from_be_bytes([pair[0], pair.get(1).copied().unwrap_or(0)]))).sum();
    while sum > 0xffff {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn wait_reply(mut socket: &Socket, kind: u8, sequence: u16, payload: &[u8], deadline: Instant) -> bool {
    let mut buffer = [0u8; 1500];
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining < Duration::from_millis(1) || socket.set_read_timeout(Some(remaining)).is_err() {
            return false;
        }
        match socket.read(&mut buffer) {
            Ok(length) if is_reply(&buffer[..length], kind, sequence, payload) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
}

fn is_reply(packet: &[u8], kind: u8, sequence: u16, payload: &[u8]) -> bool {
    let icmp = match packet.first() {
        Some(first) if first >> 4 == 4 => packet.get(usize::from(first & 0x0f) * 4..).unwrap_or_default(),
        _ => packet,
    };
    icmp.first() == Some(&kind) && icmp.get(6..8) == Some(&sequence.to_be_bytes()[..]) && icmp.get(8..).is_some_and(|p| p.starts_with(payload))
}

fn file_age(path: &Path, max_age: Duration) -> CheckResult {
    let modified = match std::fs::metadata(path).and_then(|m| m.modified()) {
        Ok(modified) => modified,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return outcome(CheckStatus::Failed, "File not found");
        }
        Err(error) => return outcome(CheckStatus::Failed, io_text(&error)),
    };
    let age = SystemTime::now().duration_since(modified).unwrap_or_default();
    CheckResult {
        status: if age > max_age { CheckStatus::Failed } else { CheckStatus::Ok },
        message: format!("Updated {} ago", human_age(age)),
        value: Some(age.as_secs() as f64),
        ..Default::default()
    }
}

fn human_age(age: Duration) -> String {
    match age.as_secs() {
        seconds @ 0..120 => format!("{seconds} s"),
        seconds @ 120..7200 => format!("{} min", seconds / 60),
        seconds @ 7200..172_800 => format!("{} h", seconds / 3600),
        seconds => format!("{} d", seconds / 86_400),
    }
}

#[derive(Deserialize)]
struct ExecOutput {
    status: CheckStatus,
    #[serde(default)]
    message: String,
    value: Option<f64>,
}

fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL);
    }
    let _ = child.kill();
    let _ = child.wait();
}

fn exec(command: &[String], timeout: Duration) -> CheckResult {
    let Some((program, arguments)) = command.split_first() else {
        return outcome(CheckStatus::Failed, "Empty command");
    };
    let mut process = Command::new(program);
    process.args(arguments).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null());
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut process, 0);
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return outcome(CheckStatus::Failed, io_text(&error)),
    };
    let stdout = child.stdout.take();
    let (sender, receiver) = mpsc::channel();
    thread::spawn(move || {
        let mut text = String::new();
        if let Some(stdout) = stdout {
            let _ = stdout.take(1 << 20).read_to_string(&mut text);
        }
        let _ = sender.send(text);
    });
    let deadline = Instant::now() + timeout;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Ok(None) => {
                kill_group(&mut child);
                return outcome(CheckStatus::Failed, "Timed out");
            }
            Err(error) => return outcome(CheckStatus::Failed, io_text(&error)),
        }
    };
    let text = match receiver.recv_timeout(deadline.saturating_duration_since(Instant::now()).max(Duration::from_millis(100))) {
        Ok(text) => text,
        Err(_) => {
            kill_group(&mut child);
            receiver.recv_timeout(Duration::from_secs(1)).unwrap_or_default()
        }
    };
    exec_result(&text, code)
}

fn exec_result(stdout: &str, code: Option<i32>) -> CheckResult {
    let parsed = serde_json::from_str::<ExecOutput>(stdout.trim());
    match (code, parsed) {
        (Some(0), Ok(output)) => CheckResult { status: output.status, message: output.message, value: output.value, ..Default::default() },
        (Some(0), Err(_)) => outcome(CheckStatus::Failed, "Output is not valid JSON"),
        (code, parsed) => {
            let fallback = code.map_or_else(|| "Killed by signal".to_string(), |code| format!("Exit code {code}"));
            outcome(CheckStatus::Failed, parsed.ok().map(|output| output.message).filter(|m| !m.is_empty()).unwrap_or(fallback))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn assertion(path: &str, equals: Value) -> JsonAssertion {
        JsonAssertion { path: path.into(), equals }
    }

    #[test]
    fn json_path_assertions() {
        let body = r#"{"status": "ok", "db": {"connected": true, "latency": 3}, "nodes": [{"name": "a"}]}"#;
        let passing = [
            assertion("status", json!("ok")),
            assertion("db.connected", json!(true)),
            assertion("db.latency", json!(3.0)),
            assertion("nodes.0.name", json!("a")),
        ];
        assert_eq!(body_failure(body, Some("connected"), &passing), None);
        assert_eq!(body_failure(body, None, &[assertion("status", json!("down"))]).as_deref(), Some("status is not \"down\""));
        assert_eq!(body_failure(body, None, &[assertion("db.missing", json!(1))]).as_deref(), Some("db.missing is missing"));
        assert_eq!(body_failure(body, Some("nope"), &[]).as_deref(), Some("Body does not contain \"nope\""));
        assert_eq!(body_failure("<html>", None, &passing).as_deref(), Some("Body is not JSON"));
    }

    #[test]
    fn exec_output() {
        let ok = exec_result(r#"{"status":"warning","message":"2 updates","value":2}"#, Some(0));
        assert_eq!((ok.status, ok.message.as_str(), ok.value), (CheckStatus::Warning, "2 updates", Some(2.0)));
        let failed = exec_result(r#"{"status":"ok","message":"disk gone"}"#, Some(1));
        assert_eq!((failed.status, failed.message.as_str()), (CheckStatus::Failed, "disk gone"));
        assert_eq!(exec_result("", Some(3)).message, "Exit code 3");
        assert_eq!(exec_result("garbage", Some(0)).message, "Output is not valid JSON");
        assert_eq!(exec_result(r#"{"status":"maybe"}"#, Some(0)).status, CheckStatus::Failed);
    }

    #[test]
    fn ages() {
        assert_eq!(human_age(Duration::from_secs(42)), "42 s");
        assert_eq!(human_age(Duration::from_secs(600)), "10 min");
        assert_eq!(human_age(Duration::from_secs(4 * 3600 + 59)), "4 h");
        assert_eq!(human_age(Duration::from_secs(3 * 86_400)), "3 d");
    }

    #[test]
    fn file_age_check() {
        let path = std::env::temp_dir().join(format!("argus-agent-test-{}", std::process::id()));
        assert_eq!(file_age(&path, Duration::from_secs(60)).message, "File not found");
        std::fs::write(&path, "x").expect("temp file");
        let fresh = file_age(&path, Duration::from_secs(60));
        let _ = std::fs::remove_file(&path);
        assert_eq!(fresh.status, CheckStatus::Ok);
        assert!(fresh.message.starts_with("Updated "));
    }

    #[test]
    fn icmp_packets() {
        let packet = echo_request(8, 2, b"token");
        assert_eq!(checksum(&packet), 0);
        let mut reply = packet.clone();
        reply[0] = 0;
        assert!(is_reply(&reply, 0, 2, b"token"));
        assert!(!is_reply(&reply, 0, 1, b"token"));
        assert!(!is_reply(&packet, 0, 2, b"token"));
        let mut with_ip_header = vec![0x45; 1];
        with_ip_header.extend_from_slice(&[0; 19]);
        with_ip_header.extend_from_slice(&reply);
        assert!(is_reply(&with_ip_header, 0, 2, b"token"));
    }

    #[test]
    fn tcp_and_check_metadata() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("address").port();
        let check = Check { name: "Local".into(), kind: CheckKind::Tcp { host: "127.0.0.1".into(), port, timeout: Duration::from_secs(2) } };
        let result = run_all(std::slice::from_ref(&check)).remove(0);
        assert_eq!((result.status, result.kind.as_str(), result.target), (CheckStatus::Ok, "tcp", format!("127.0.0.1:{port}")));
        assert!(result.latency_ms.is_some());
        drop(listener);
        assert_eq!(tcp("127.0.0.1", port, Duration::from_secs(2)).status, CheckStatus::Failed);
    }
}
