use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use argus_model::{Filesystem, HostInfo, Interface, Inventory, Metric, Sensor};

use crate::config::Config;
use crate::docker::Docker;
use crate::{log, platform};

type Counters = HashMap<String, (u64, u64)>;

const REAL_FILESYSTEMS: &[&str] = &["ext2", "ext3", "ext4", "xfs", "btrfs", "zfs", "ufs", "vfat", "exfat", "f2fs", "fuseblk", "ntfs3"];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CpuTimes {
    pub total: u64,
    pub idle: u64,
    pub iowait: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Memory {
    pub total_bytes: u64,
    pub used: f64,
    pub cache: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Mount {
    pub device: String,
    pub mount: String,
    pub fstype: String,
}

pub struct Collector {
    cpu: Option<CpuTimes>,
    net: Option<(Instant, Counters)>,
    docker: Docker,
    warned: HashSet<String>,
}

impl Collector {
    pub fn new() -> Self {
        Self { cpu: None, net: None, docker: Docker::default(), warned: HashSet::new() }
    }

    pub fn collect(&mut self, config: &Config) -> (HostInfo, Vec<Metric>, Inventory) {
        let root = config.root.as_str();
        let collectors = &config.collectors;
        let mut metrics = Vec::new();
        let mut inventory = Inventory::default();

        if let Some(current) = platform::cpu_times() {
            if let Some((total, iowait)) = self.cpu.and_then(|previous| cpu_percent(previous, current)) {
                metrics.push(Metric::new("cpu", "", "total", total));
                metrics.push(Metric::new("cpu", "", "iowait", iowait));
            }
            self.cpu = Some(current);
        }

        let memory = platform::memory();
        if let Some(memory) = memory {
            metrics.push(Metric::new("memory", "", "used", memory.used));
            metrics.push(Metric::new("memory", "", "cache", memory.cache));
        }

        if let Some(load) = platform::load() {
            for (field, value) in ["1", "5", "15"].into_iter().zip(load) {
                metrics.push(Metric::new("load", "", field, value));
            }
        }

        let pools = platform::pools();
        for (pool, size, used) in &pools {
            metrics.push(Metric::new("fs", pool, "used", *used as f64 * 100.0 / (*size).max(1) as f64));
            inventory.filesystems.push(Filesystem { mount: pool.clone(), fstype: "zpool".into(), size_bytes: *size, used_bytes: *used });
        }
        let mounts = platform::mounts(root).into_iter().filter(|m| pools.is_empty() || m.fstype != "zfs").collect();
        for mount in select_mounts(mounts, &collectors.filesystems) {
            let Some((size, used, available)) = fs_usage(&format!("{root}{}", mount.mount)) else {
                continue;
            };
            if size == 0 {
                continue;
            }
            metrics.push(Metric::new("fs", &mount.mount, "used", used as f64 * 100.0 / (used + available).max(1) as f64));
            inventory.filesystems.push(Filesystem { mount: mount.mount, fstype: mount.fstype, size_bytes: size, used_bytes: used });
        }

        self.network(config, &mut metrics, &mut inventory);

        for device in &collectors.smart {
            match platform::smart(device) {
                Ok(smart) => inventory.smart.push(smart),
                Err(error) => self.warn_once(format!("SMART {device}: {error}")),
            }
        }
        for smart in &inventory.smart {
            if let Some(temperature) = smart.temperature {
                metrics.push(Metric::new("smart", &smart.device, "temp", temperature));
            }
            if let Some(used) = smart.percent_used {
                metrics.push(Metric::new("smart", &smart.device, "used", used));
            }
        }

        let mut sensors = platform::temperatures();
        if cfg!(target_os = "freebsd") {
            let count = inventory.smart.len();
            sensors.extend(
                inventory
                    .smart
                    .iter()
                    .enumerate()
                    .filter_map(|(index, smart)| Some((Sensor { name: nvme_name(index, count), group: "NVMe".into() }, smart.temperature?))),
            );
        }
        for (sensor, value) in sensors {
            metrics.push(Metric::new("temp", &sensor.name, "value", value));
            inventory.sensors.push(sensor);
        }

        inventory.raid = platform::raid();

        if let Some(data) = &collectors.docker {
            let data = if data.starts_with("http") { "/var/lib/docker" } else { data.as_str() };
            match self.docker.collect(root, data) {
                Ok(sample) => {
                    for container in sample.containers.iter().filter(|c| c.state == "running") {
                        if sample.measured.contains(&container.name) {
                            metrics.push(Metric::new("docker", &container.name, "cpu", container.cpu));
                        }
                        metrics.push(Metric::new("docker", &container.name, "memory", container.memory_bytes as f64 / 1_048_576.0));
                    }
                    inventory.containers = sample.containers;
                }
                Err(error) => self.warn_once(format!("Docker: {error}")),
            }
        }

        if collectors.systemd {
            match platform::failed_units(root) {
                Ok(units) => inventory.failed_units = Some(units),
                Err(error) => self.warn_once(format!("systemd: {error}")),
            }
        }

        inventory.reboot_required = Path::new(&format!("{root}/run")).is_dir().then(|| Path::new(&format!("{root}/run/reboot-required")).exists());

        (host_info(config, memory.map_or(0, |m| m.total_bytes)), metrics, inventory)
    }

    fn network(&mut self, config: &Config, metrics: &mut Vec<Metric>, inventory: &mut Inventory) {
        let now = Instant::now();
        let ignore = &config.collectors.ignore_interfaces;
        let counters: Counters = platform::net_bytes()
            .into_iter()
            .filter(|(name, ..)| !ignore.iter().any(|prefix| name.starts_with(prefix.as_str())))
            .map(|(name, rx, tx)| (name, (rx, tx)))
            .collect();
        let descriptions = platform::interface_descriptions();
        let mut entries: Vec<(&String, &(u64, u64))> = counters.iter().collect();
        entries.sort();
        for (name, &(rx, tx)) in entries {
            let description = config.collectors.interfaces.get(name).or_else(|| descriptions.get(name)).cloned();
            inventory.interfaces.push(Interface { name: name.clone(), description });
            if let Some((then, previous)) = &self.net
                && let Some(&(previous_rx, previous_tx)) = previous.get(name)
            {
                let seconds = now.duration_since(*then).as_secs_f64();
                let rate = |current: u64, previous: u64| current.saturating_sub(previous) as f64 * 8.0 / seconds / 1_000_000.0;
                if seconds > 0.0 {
                    metrics.push(Metric::new("net", name, "rx", rate(rx, previous_rx)));
                    metrics.push(Metric::new("net", name, "tx", rate(tx, previous_tx)));
                }
            }
        }
        self.net = Some((now, counters));
    }

    fn warn_once(&mut self, message: String) {
        if self.warned.insert(message.clone()) {
            log(&message);
        }
    }
}

pub fn cpu_percent(previous: CpuTimes, current: CpuTimes) -> Option<(f64, f64)> {
    let total = current.total.checked_sub(previous.total).filter(|t| *t > 0)? as f64;
    let idle = current.idle.saturating_sub(previous.idle) as f64;
    let iowait = current.iowait.saturating_sub(previous.iowait) as f64;
    Some(((total - idle).max(0.0) * 100.0 / total, iowait * 100.0 / total))
}

pub fn select_mounts(mounts: Vec<Mount>, allow: &[String]) -> Vec<Mount> {
    let mut devices = HashSet::new();
    mounts
        .into_iter()
        .filter(|m| REAL_FILESYSTEMS.contains(&m.fstype.as_str()) && !m.mount.starts_with("/etc/") && (allow.is_empty() || allow.contains(&m.mount)))
        .filter(|m| devices.insert(m.device.clone()))
        .collect()
}

pub fn nvme_name(index: usize, count: usize) -> String {
    if count == 1 { "NVMe".into() } else { format!("NVMe {index}") }
}

fn host_info(config: &Config, memory_bytes: u64) -> HostInfo {
    let root = config.root.as_str();
    let uname = uname();
    HostInfo {
        id: config.id.clone().unwrap_or_else(|| hostname(root, uname.as_ref())),
        site: config.site.clone(),
        role: config.role.clone(),
        os: config
            .os
            .clone()
            .or_else(|| std::fs::read_to_string(format!("{root}/etc/os-release")).ok().and_then(|text| pretty_name(&text)))
            .or_else(|| uname.as_ref().map(|u| format!("{} {}", u.0, u.1)))
            .unwrap_or_else(|| std::env::consts::OS.into()),
        kernel: uname.as_ref().map(|u| u.1.clone()).unwrap_or_default(),
        arch: std::env::consts::ARCH.into(),
        cores: std::thread::available_parallelism().map_or(0, |n| n.get() as u32),
        memory_bytes,
        uptime_s: platform::uptime().unwrap_or(0),
        agent: config.agent.clone(),
        version: env!("CARGO_PKG_VERSION").into(),
    }
}

fn hostname(root: &str, uname: Option<&(String, String, String)>) -> String {
    std::fs::read_to_string(format!("{root}/etc/hostname"))
        .ok()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .or_else(|| uname.map(|u| u.2.clone()))
        .or_else(|| std::env::var("COMPUTERNAME").ok())
        .unwrap_or_else(|| "unknown".into())
}

pub fn pretty_name(os_release: &str) -> Option<String> {
    os_release
        .lines()
        .find_map(|line| line.strip_prefix("PRETTY_NAME="))
        .map(|value| value.trim().trim_matches('"').trim_matches('\'').to_string())
        .filter(|name| !name.is_empty())
}

#[cfg(unix)]
fn uname() -> Option<(String, String, String)> {
    let mut names: libc::utsname = unsafe { std::mem::zeroed() };
    if unsafe { libc::uname(&mut names) } != 0 {
        return None;
    }
    let text = |field: &[libc::c_char]| unsafe { std::ffi::CStr::from_ptr(field.as_ptr()) }.to_string_lossy().into_owned();
    Some((text(&names.sysname), text(&names.release), text(&names.nodename)))
}

#[cfg(not(unix))]
fn uname() -> Option<(String, String, String)> {
    None
}

#[cfg(unix)]
fn fs_usage(path: &str) -> Option<(u64, u64, u64)> {
    let path = std::ffi::CString::new(path).ok()?;
    let mut stats: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(path.as_ptr(), &mut stats) } != 0 {
        return None;
    }
    let block = stats.f_frsize as u64;
    let size = stats.f_blocks as u64 * block;
    let free = stats.f_bfree as u64 * block;
    Some((size, size.saturating_sub(free), stats.f_bavail as u64 * block))
}

#[cfg(not(unix))]
fn fs_usage(_path: &str) -> Option<(u64, u64, u64)> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mount(device: &str, mount: &str, fstype: &str) -> Mount {
        Mount { device: device.into(), mount: mount.into(), fstype: fstype.into() }
    }

    #[test]
    fn cpu_percent_from_deltas() {
        let previous = CpuTimes { total: 1000, idle: 800, iowait: 50 };
        let current = CpuTimes { total: 2000, idle: 1500, iowait: 150 };
        assert_eq!(cpu_percent(previous, current), Some((30.0, 10.0)));
        assert_eq!(cpu_percent(current, current), None);
    }

    #[test]
    fn container_file_mounts_are_skipped_so_the_data_volume_wins() {
        let mounts = vec![mount("/dev/sda8", "/etc/hostname", "ext4"), mount("/dev/sda8", "/etc/resolv.conf", "ext4"), mount("/dev/sda8", "/data", "ext4")];
        let selected: Vec<String> = select_mounts(mounts, &[]).into_iter().map(|m| m.mount).collect();
        assert_eq!(selected, ["/data"]);
    }

    #[test]
    fn mounts_keep_real_filesystems_once_per_device() {
        let mounts = vec![
            mount("/dev/sda1", "/", "ext4"),
            mount("proc", "/proc", "proc"),
            mount("overlay", "/var/lib/docker/overlay2/x/merged", "overlay"),
            mount("/dev/sda1", "/var/lib/docker", "ext4"),
            mount("/dev/md0", "/mnt/raid", "ext4"),
            mount("/dev/nvme0n1p1", "/boot/firmware", "vfat"),
        ];
        let mounts_selected: Vec<String> = select_mounts(mounts.clone(), &[]).into_iter().map(|m| m.mount).collect();
        assert_eq!(mounts_selected, ["/", "/mnt/raid", "/boot/firmware"]);
        let allowed: Vec<String> = select_mounts(mounts, &["/mnt/raid".into()]).into_iter().map(|m| m.mount).collect();
        assert_eq!(allowed, ["/mnt/raid"]);
    }

    #[test]
    fn os_release_pretty_name() {
        let text = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.1 LTS\"\nID=ubuntu\n";
        assert_eq!(pretty_name(text).as_deref(), Some("Ubuntu 24.04.1 LTS"));
        assert_eq!(pretty_name("PRETTY_NAME=FreeBSD\n").as_deref(), Some("FreeBSD"));
        assert_eq!(pretty_name("NAME=x\n"), None);
    }

    #[test]
    fn nvme_names() {
        assert_eq!(nvme_name(0, 1), "NVMe");
        assert_eq!(nvme_name(1, 2), "NVMe 1");
    }
}
