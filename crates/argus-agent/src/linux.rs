use std::collections::HashMap;
use std::fs::{self, File};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::time::Duration;

use argus_model::{Raid, Sensor, Smart};
use zbus::zvariant::OwnedObjectPath;

use crate::collect::{CpuTimes, Memory, Mount};
use crate::linux_parse::{self, Hwmon};

type Unit = (String, String, String, String, String, String, OwnedObjectPath, u32, String, OwnedObjectPath);

#[repr(C)]
#[derive(Default)]
struct NvmeAdminCommand {
    opcode: u8,
    flags: u8,
    reserved: u16,
    nsid: u32,
    cdw2: u32,
    cdw3: u32,
    metadata: u64,
    addr: u64,
    metadata_len: u32,
    data_len: u32,
    cdw10: u32,
    cdw11: u32,
    cdw12: u32,
    cdw13: u32,
    cdw14: u32,
    cdw15: u32,
    timeout_ms: u32,
    result: u32,
}

const _: () = assert!(std::mem::size_of::<NvmeAdminCommand>() == 72);
const NVME_IOCTL_ADMIN_CMD: u32 = 0xC048_4E41;

fn read(path: &str) -> Option<String> {
    fs::read_to_string(path).ok()
}

pub fn cpu_times() -> Option<CpuTimes> {
    linux_parse::cpu_times(&read("/proc/stat")?)
}

pub fn memory() -> Option<Memory> {
    linux_parse::memory(&read("/proc/meminfo")?)
}

pub fn load() -> Option<[f64; 3]> {
    linux_parse::load(&read("/proc/loadavg")?)
}

pub fn uptime() -> Option<u64> {
    linux_parse::uptime(&read("/proc/uptime")?)
}

pub fn mounts(root: &str) -> Vec<Mount> {
    read(&format!("{root}/proc/1/mounts")).or_else(|| read("/proc/mounts")).map(|text| linux_parse::mounts(&text)).unwrap_or_default()
}

pub fn net_bytes() -> Vec<(String, u64, u64)> {
    read("/proc/net/dev").map(|text| linux_parse::net_dev(&text)).unwrap_or_default()
}

pub fn interface_descriptions() -> HashMap<String, String> {
    HashMap::new()
}

pub fn raid() -> Vec<Raid> {
    read("/proc/mdstat").map(|text| linux_parse::mdstat(&text)).unwrap_or_default()
}

pub fn temperatures() -> Vec<(Sensor, f64)> {
    let Ok(entries) = fs::read_dir("/sys/class/hwmon") else {
        return Vec::new();
    };
    let hwmons = entries
        .flatten()
        .filter_map(|entry| {
            let dir = entry.path();
            let name = fs::read_to_string(dir.join("name")).ok()?.trim().to_string();
            let device = fs::read_link(dir.join("device")).ok().and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned())).unwrap_or_default();
            let mut temps: Vec<(u32, Option<String>, f64)> = fs::read_dir(&dir)
                .ok()?
                .flatten()
                .filter_map(|file| {
                    let file_name = file.file_name().into_string().ok()?;
                    let index: u32 = file_name.strip_prefix("temp")?.strip_suffix("_input")?.parse().ok()?;
                    let millidegrees: f64 = fs::read_to_string(file.path()).ok()?.trim().parse().ok()?;
                    let label = fs::read_to_string(dir.join(format!("temp{index}_label"))).ok().map(|l| l.trim().to_string());
                    Some((index, label, millidegrees / 1000.0))
                })
                .collect();
            temps.sort_by_key(|t| t.0);
            Some(Hwmon { name, device, temps: temps.into_iter().map(|(_, label, value)| (label, value)).collect() })
        })
        .collect();
    linux_parse::sensors(hwmons)
}

pub fn smart(device: &str) -> Result<Smart, String> {
    let file = File::open(device).map_err(|e| e.to_string())?;
    let mut log = [0u8; 512];
    let mut command = NvmeAdminCommand {
        opcode: 0x02,
        nsid: 0xFFFF_FFFF,
        addr: log.as_mut_ptr() as u64,
        data_len: log.len() as u32,
        cdw10: 0x02 | ((log.len() as u32 / 4 - 1) << 16),
        timeout_ms: 5_000,
        ..Default::default()
    };
    let status = unsafe { libc::ioctl(file.as_raw_fd(), NVME_IOCTL_ADMIN_CMD as _, &mut command) };
    match status {
        0 => Ok(linux_parse::smart_log(&Path::new(device).file_name().map_or_else(|| device.into(), |n| n.to_string_lossy()), &log)),
        status if status < 0 => Err(std::io::Error::last_os_error().to_string()),
        status => Err(format!("NVMe status {status:#x}")),
    }
}

pub fn failed_units(root: &str) -> Result<Vec<String>, String> {
    let builder = if root.is_empty() {
        zbus::blocking::connection::Builder::system()
    } else {
        zbus::blocking::connection::Builder::address(format!("unix:path={root}/run/dbus/system_bus_socket").as_str())
    };
    let connection = builder.and_then(|b| b.method_timeout(Duration::from_secs(5)).build()).map_err(|e| e.to_string())?;
    let reply = connection
        .call_method(
            Some("org.freedesktop.systemd1"),
            "/org/freedesktop/systemd1",
            Some("org.freedesktop.systemd1.Manager"),
            "ListUnitsFiltered",
            &(vec!["failed"],),
        )
        .map_err(|e| e.to_string())?;
    let units: Vec<Unit> = reply.body().deserialize().map_err(|e| e.to_string())?;
    Ok(units.into_iter().map(|unit| unit.0).collect())
}
