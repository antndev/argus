use std::collections::HashMap;

use argus_model::{Raid, Sensor, Smart};

use crate::collect::{CpuTimes, Memory, Mount, nvme_name};

pub struct Hwmon {
    pub name: String,
    pub device: String,
    pub temps: Vec<(Option<String>, f64)>,
}

pub fn cpu_times(stat: &str) -> Option<CpuTimes> {
    let values: Vec<u64> = stat.lines().next()?.strip_prefix("cpu ")?.split_whitespace().take(8).map(|v| v.parse().ok()).collect::<Option<_>>()?;
    let (idle, iowait) = (*values.get(3)?, *values.get(4)?);
    Some(CpuTimes { total: values.iter().sum(), idle: idle + iowait, iowait })
}

pub fn memory(meminfo: &str) -> Option<Memory> {
    let field = |name: &str| meminfo.lines().find_map(|line| line.strip_prefix(name)?.strip_prefix(':')?.split_whitespace().next()?.parse::<u64>().ok());
    let total = field("MemTotal").filter(|t| *t > 0)?;
    let available = field("MemAvailable")?.min(total);
    let cache = field("Cached").unwrap_or(0) + field("Buffers").unwrap_or(0);
    let percent = |kib: u64| kib as f64 * 100.0 / total as f64;
    Some(Memory { total_bytes: total * 1024, used: percent(total - available), cache: percent(cache) })
}

pub fn load(loadavg: &str) -> Option<[f64; 3]> {
    let mut values = loadavg.split_whitespace().map(|v| v.parse::<f64>().ok());
    Some([values.next()??, values.next()??, values.next()??])
}

pub fn uptime(text: &str) -> Option<u64> {
    text.split_whitespace().next()?.parse::<f64>().ok().map(|s| s as u64)
}

pub fn net_dev(text: &str) -> Vec<(String, u64, u64)> {
    text.lines()
        .skip(2)
        .filter_map(|line| {
            let (name, rest) = line.split_once(':')?;
            let fields: Vec<&str> = rest.split_whitespace().collect();
            Some((name.trim().to_string(), fields.first()?.parse().ok()?, fields.get(8)?.parse().ok()?))
        })
        .collect()
}

pub fn mounts(text: &str) -> Vec<Mount> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.split_whitespace();
            Some(Mount { device: unescape(parts.next()?), mount: unescape(parts.next()?), fstype: parts.next()?.to_string() })
        })
        .collect()
}

fn unescape(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes.get(i..i + 4) {
            Some(&[b'\\', a, b, c]) if [a, b, c].iter().all(|d| (b'0'..=b'7').contains(d)) => {
                out.push(((u16::from(a - b'0') << 6) | (u16::from(b - b'0') << 3) | u16::from(c - b'0')) as u8);
                i += 4;
            }
            _ => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub fn mdstat(text: &str) -> Vec<Raid> {
    let mut arrays: Vec<Raid> = Vec::new();
    for line in text.lines() {
        if let Some((name, rest)) = line.split_once(" : ")
            && name.starts_with("md")
        {
            let mut tokens = rest.split_whitespace().filter(|t| !t.starts_with('('));
            let state = tokens.next().unwrap_or_default().to_string();
            let mut level = String::new();
            let mut members = Vec::new();
            for token in tokens {
                if level.is_empty() && members.is_empty() && !token.contains('[') {
                    level = token.to_string();
                } else {
                    members.push(token);
                }
            }
            let degraded = state != "active";
            arrays.push(Raid { name: name.trim().to_string(), level, state, devices: members.join(" "), degraded });
        } else if let Some(raid) = arrays.last_mut()
            && !line.trim().is_empty()
        {
            let tokens: Vec<&str> = line.split_whitespace().collect();
            if let Some(position) = tokens.iter().position(|t| is_member_pattern(t)) {
                let pattern = tokens[position];
                raid.devices = match position.checked_sub(1).map(|p| tokens[p]) {
                    Some(counts) if counts.contains('/') => format!("{counts} {pattern}"),
                    _ => pattern.to_string(),
                };
                raid.degraded |= pattern.contains('_');
            }
            if let Some(action) = ["recovery", "resync", "reshape", "check"].into_iter().find(|a| line.contains(&format!("{a} ="))) {
                raid.state = action.to_string();
            }
        }
    }
    arrays
}

fn is_member_pattern(token: &str) -> bool {
    token.len() > 2 && token.starts_with('[') && token.ends_with(']') && token[1..token.len() - 1].chars().all(|c| c == 'U' || c == '_')
}

pub fn sensors(mut hwmons: Vec<Hwmon>) -> Vec<(Sensor, f64)> {
    hwmons.sort_by(|a, b| (&a.name, &a.device).cmp(&(&b.name, &b.device)));
    let nvme_count = hwmons.iter().filter(|h| h.name == "nvme").count();
    let mut nvme_index = 0;
    let mut out = Vec::new();
    let mut push = |name: String, group: &str, value: Option<f64>| {
        if let Some(value) = value {
            out.push((Sensor { name, group: group.to_string() }, value));
        }
    };
    for hwmon in &hwmons {
        let preferred = |labels: &[&str]| {
            labels
                .iter()
                .find_map(|wanted| hwmon.temps.iter().find(|(label, _)| label.as_deref() == Some(*wanted)))
                .or(hwmon.temps.first())
                .map(|(_, value)| *value)
        };
        match hwmon.name.as_str() {
            "coretemp" | "k10temp" | "zenpower" => push("CPU package".into(), "CPU", preferred(&["Package id 0", "Tdie", "Tctl"])),
            "nvme" => {
                push(nvme_name(nvme_index, nvme_count), "NVMe", preferred(&["Composite"]));
                nvme_index += 1;
            }
            "cpu_thermal" | "soc_thermal" => push("SoC".into(), "SoC", preferred(&[])),
            "rp1_adc" => push("RP1".into(), "SoC", preferred(&[])),
            name => {
                for (index, (label, value)) in hwmon.temps.iter().enumerate() {
                    let item = match (hwmon.temps.len(), label) {
                        (1, _) => name.to_string(),
                        (_, Some(label)) => format!("{name} {label}"),
                        (_, None) => format!("{name} {}", index + 1),
                    };
                    push(item, name, Some(*value));
                }
            }
        }
    }
    let mut seen: HashMap<String, usize> = HashMap::new();
    for (sensor, _) in &mut out {
        let count = seen.entry(sensor.name.clone()).or_default();
        *count += 1;
        if *count > 1 {
            sensor.name = format!("{} {count}", sensor.name);
        }
    }
    out
}

pub fn smart_log(device: &str, log: &[u8; 512]) -> Smart {
    let counter = |start: usize| u64::try_from(u128::from_le_bytes(log[start..start + 16].try_into().unwrap_or_default())).unwrap_or(u64::MAX);
    let kelvin = u16::from_le_bytes([log[1], log[2]]);
    Smart {
        device: device.to_string(),
        model: String::new(),
        passed: log[0] == 0,
        percent_used: Some(f64::from(log[5])),
        temperature: (kelvin > 0).then(|| f64::from(kelvin) - 273.15),
        media_errors: Some(counter(160)),
        power_on_hours: Some(counter(128)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_stat() {
        let text = "cpu  4705 150 1120 16250 520 0 30 0 0 0\ncpu0 1 2 3 4 5 6 7 8 9 10\n";
        assert_eq!(cpu_times(text), Some(CpuTimes { total: 22775, idle: 16770, iowait: 520 }));
        assert_eq!(cpu_times("intr 1 2 3"), None);
    }

    #[test]
    fn proc_meminfo() {
        let text = "MemTotal:       16000000 kB\nMemFree:         2000000 kB\nMemAvailable:    8000000 kB\nBuffers:          400000 kB\nCached:          3600000 kB\nSwapCached:            0 kB\n";
        let memory = memory(text).expect("parses");
        assert_eq!(memory.total_bytes, 16_384_000_000);
        assert_eq!(memory.used, 50.0);
        assert_eq!(memory.cache, 25.0);
        assert_eq!(super::memory("MemFree: 1 kB"), None);
    }

    #[test]
    fn proc_loadavg_and_uptime() {
        assert_eq!(load("0.52 0.58 0.59 2/1234 5678\n"), Some([0.52, 0.58, 0.59]));
        assert_eq!(load("0.52"), None);
        assert_eq!(uptime("350735.47 234388.90\n"), Some(350_735));
    }

    #[test]
    fn proc_net_dev() {
        let text = "Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 1000      10    0    0    0     0          0         0     1000      10    0    0    0     0       0          0
  eth0:123456789 100000    0    0    0     0          0       100 987654321  90000    0    0    0     0       0          0
";
        assert_eq!(net_dev(text), vec![("lo".to_string(), 1000, 1000), ("eth0".to_string(), 123_456_789, 987_654_321)]);
    }

    #[test]
    fn proc_mounts() {
        let text = "/dev/sda2 / ext4 rw,relatime 0 0\n/dev/sdb1 /mnt/my\\040disk ext4 rw 0 0\nproc /proc proc rw 0 0\n";
        let mounts = mounts(text);
        assert_eq!(mounts.len(), 3);
        assert_eq!(mounts[1].mount, "/mnt/my disk");
        assert_eq!(mounts[0], Mount { device: "/dev/sda2".into(), mount: "/".into(), fstype: "ext4".into() });
    }

    #[test]
    fn mdstat_clean_degraded_and_inactive() {
        let text = "Personalities : [raid1] [raid6]
md0 : active raid1 nvme1n1[1] nvme0n1[0]
      1953382464 blocks super 1.2 [2/2] [UU]
      bitmap: 0/15 pages [0KB], 65536KB chunk

md1 : active raid1 sdb1[2] sda1[0]
      976630464 blocks super 1.2 [2/1] [U_]
      [=>...................]  recovery =  8.5% (83093504/976630464) finish=76.3min speed=195000K/sec

md2 : inactive sdc[0](S)
      976630464 blocks super 1.2

md3 : active (auto-read-only) raid1 sdd[0] sde[1]
      100 blocks [2/2] [UU]

unused devices: <none>
";
        let arrays = mdstat(text);
        assert_eq!(arrays.len(), 4);
        assert_eq!(arrays[0], Raid { name: "md0".into(), level: "raid1".into(), state: "active".into(), devices: "[2/2] [UU]".into(), degraded: false });
        assert!(arrays[1].degraded);
        assert_eq!(arrays[1].state, "recovery");
        assert_eq!(arrays[1].devices, "[2/1] [U_]");
        assert!(arrays[2].degraded);
        assert_eq!(arrays[2].level, "");
        assert_eq!(arrays[2].devices, "sdc[0](S)");
        assert_eq!(arrays[3].level, "raid1");
        assert!(!arrays[3].degraded);
    }

    fn hwmon(name: &str, device: &str, temps: &[(Option<&str>, f64)]) -> Hwmon {
        Hwmon { name: name.into(), device: device.into(), temps: temps.iter().map(|(l, v)| (l.map(String::from), *v)).collect() }
    }

    fn names(sensors: &[(Sensor, f64)]) -> Vec<(String, String, f64)> {
        sensors.iter().map(|(s, v)| (s.name.clone(), s.group.clone(), *v)).collect()
    }

    #[test]
    fn hwmon_naming() {
        let sensors = sensors(vec![
            hwmon("nvme", "nvme1", &[(Some("Composite"), 41.85), (Some("Sensor 1"), 50.0)]),
            hwmon("coretemp", "coretemp.0", &[(Some("Core 0"), 50.0), (Some("Package id 0"), 55.0)]),
            hwmon("nvme", "nvme0", &[(Some("Composite"), 39.85)]),
            hwmon("acpitz", "", &[(None, 27.8), (None, 29.8)]),
            hwmon("iwlwifi_1", "", &[(None, 40.0)]),
            hwmon("drivetemp", "0:0:0:0", &[(None, 30.0)]),
            hwmon("drivetemp", "1:0:0:0", &[(None, 31.0)]),
        ]);
        assert_eq!(
            names(&sensors),
            [
                ("acpitz 1".into(), "acpitz".into(), 27.8),
                ("acpitz 2".into(), "acpitz".into(), 29.8),
                ("CPU package".into(), "CPU".into(), 55.0),
                ("drivetemp".into(), "drivetemp".into(), 30.0),
                ("drivetemp 2".into(), "drivetemp".into(), 31.0),
                ("iwlwifi_1".into(), "iwlwifi_1".into(), 40.0),
                ("NVMe 0".into(), "NVMe".into(), 39.85),
                ("NVMe 1".into(), "NVMe".into(), 41.85),
            ]
        );
    }

    #[test]
    fn hwmon_amd_and_raspberry_pi() {
        let amd = sensors(vec![
            hwmon("k10temp", "0000:00:18.3", &[(Some("Tctl"), 60.0), (Some("Tdie"), 50.0), (Some("Tccd1"), 45.0)]),
            hwmon("nvme", "nvme0", &[(Some("Composite"), 40.0)]),
        ]);
        assert_eq!(names(&amd), [("CPU package".into(), "CPU".into(), 50.0), ("NVMe".into(), "NVMe".into(), 40.0)]);
        let pi = sensors(vec![hwmon("rp1_adc", "", &[(None, 48.0)]), hwmon("cpu_thermal", "", &[(None, 52.5)]), hwmon("pwmfan", "", &[])]);
        assert_eq!(names(&pi), [("SoC".into(), "SoC".into(), 52.5), ("RP1".into(), "SoC".into(), 48.0)]);
    }

    #[test]
    fn nvme_smart_log() {
        let mut log = [0u8; 512];
        log[1..3].copy_from_slice(&318u16.to_le_bytes());
        log[5] = 4;
        log[128..136].copy_from_slice(&12_345u64.to_le_bytes());
        log[160] = 2;
        let smart = smart_log("nvme0", &log);
        assert!(smart.passed);
        assert_eq!(smart.percent_used, Some(4.0));
        assert!((smart.temperature.unwrap_or_default() - 44.85).abs() < 0.001);
        assert_eq!(smart.power_on_hours, Some(12_345));
        assert_eq!(smart.media_errors, Some(2));
        log[0] = 0x04;
        assert!(!smart_log("nvme0", &log).passed);
    }
}
