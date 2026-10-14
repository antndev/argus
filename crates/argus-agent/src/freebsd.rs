use std::collections::HashMap;
use std::ffi::{CStr, CString};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use argus_model::{Raid, Sensor, Smart};

use crate::collect::{CpuTimes, Memory, Mount};
use crate::freebsd_parse;

const RC_SCRIPT: &str = r#"#!/bin/sh

# PROVIDE: argus_agent
# REQUIRE: LOGIN NETWORKING
# KEYWORD: shutdown

. /etc/rc.subr

name="argus_agent"
rcvar="argus_agent_enable"

load_rc_config $name

: ${argus_agent_enable:="NO"}
: ${argus_agent_config:="@CONFIG@"}

pidfile="/var/run/${name}.pid"
command="/usr/sbin/daemon"
command_args="-r -P ${pidfile} -o /var/log/argus-agent.log @BINARY@ --config ${argus_agent_config}"

run_rc_command "$1"
"#;
const RC_PATH: &str = "/usr/local/etc/rc.d/argus_agent";

fn sysctl(name: &str) -> Option<Vec<u8>> {
    let name = CString::new(name).ok()?;
    let mut size = 0usize;
    if unsafe { libc::sysctlbyname(name.as_ptr(), std::ptr::null_mut(), &mut size, std::ptr::null(), 0) } != 0 {
        return None;
    }
    let mut buffer = vec![0u8; size];
    if unsafe { libc::sysctlbyname(name.as_ptr(), buffer.as_mut_ptr().cast(), &mut size, std::ptr::null(), 0) } != 0 {
        return None;
    }
    buffer.truncate(size);
    Some(buffer)
}

fn unsigned(raw: &[u8]) -> Option<u64> {
    match raw.len() {
        4 => Some(u64::from(u32::from_ne_bytes(raw.try_into().ok()?))),
        8 => Some(u64::from_ne_bytes(raw.try_into().ok()?)),
        _ => None,
    }
}

fn sysctl_u64(name: &str) -> Option<u64> {
    unsigned(&sysctl(name)?)
}

fn text(field: &[libc::c_char]) -> String {
    unsafe { CStr::from_ptr(field.as_ptr()) }.to_string_lossy().into_owned()
}

pub fn cpu_times() -> Option<CpuTimes> {
    let raw = sysctl("kern.cp_time")?;
    let values: Vec<u64> = raw.as_chunks::<{ std::mem::size_of::<libc::c_long>() }>().0.iter().map(|chunk| unsigned(chunk)).collect::<Option<_>>()?;
    Some(CpuTimes { total: values.iter().sum(), idle: *values.get(4)?, iowait: 0 })
}

pub fn memory() -> Option<Memory> {
    let pages = sysctl_u64("vm.stats.vm.v_page_count").filter(|p| *p > 0)?;
    let free = sysctl_u64("vm.stats.vm.v_free_count").unwrap_or(0);
    let inactive = sysctl_u64("vm.stats.vm.v_inactive_count").unwrap_or(0);
    let cache = sysctl_u64("vm.stats.vm.v_cache_count").unwrap_or(0);
    let laundry = sysctl_u64("vm.stats.vm.v_laundry_count").unwrap_or(0);
    let arc = sysctl_u64("kstat.zfs.misc.arcstats.size").unwrap_or(0) / sysctl_u64("hw.pagesize").filter(|s| *s > 0).unwrap_or(4096);
    let reclaimable = inactive + cache + laundry + arc;
    let percent = |count: u64| count as f64 * 100.0 / pages as f64;
    Some(Memory { total_bytes: sysctl_u64("hw.physmem")?, used: percent(pages.saturating_sub(free + reclaimable)), cache: percent(reclaimable.min(pages)) })
}

pub fn load() -> Option<[f64; 3]> {
    let mut load = [0f64; 3];
    (unsafe { libc::getloadavg(load.as_mut_ptr(), 3) } == 3).then_some(load)
}

pub fn uptime() -> Option<u64> {
    let boot = sysctl("kern.boottime")?;
    let seconds = i64::from_ne_bytes(boot.get(..8)?.try_into().ok()?);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    now.checked_sub(u64::try_from(seconds).ok()?)
}

pub fn mounts(_root: &str) -> Vec<Mount> {
    let mut entries: *mut libc::statfs = std::ptr::null_mut();
    let count = unsafe { libc::getmntinfo(&mut entries, libc::MNT_NOWAIT) };
    if count <= 0 || entries.is_null() {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(entries, count as usize) }
        .iter()
        .map(|fs| Mount { device: text(&fs.f_mntfromname), mount: text(&fs.f_mntonname), fstype: text(&fs.f_fstypename) })
        .collect()
}

pub fn net_bytes() -> Vec<(String, u64, u64)> {
    let mut addresses: *mut libc::ifaddrs = std::ptr::null_mut();
    if unsafe { libc::getifaddrs(&mut addresses) } != 0 {
        return Vec::new();
    }
    let mut counters = Vec::new();
    let mut current = addresses;
    while let Some(entry) = unsafe { current.as_ref() } {
        let is_link = unsafe { entry.ifa_addr.as_ref() }.is_some_and(|a| i32::from(a.sa_family) == libc::AF_LINK);
        if is_link && let Some(data) = unsafe { entry.ifa_data.cast::<libc::if_data>().as_ref() } {
            counters.push((unsafe { CStr::from_ptr(entry.ifa_name) }.to_string_lossy().into_owned(), data.ifi_ibytes, data.ifi_obytes));
        }
        current = entry.ifa_next;
    }
    unsafe { libc::freeifaddrs(addresses) };
    counters
}

pub fn interface_descriptions() -> HashMap<String, String> {
    std::fs::read_to_string("/conf/config.xml").map(|xml| freebsd_parse::interface_descriptions(&xml)).unwrap_or_default()
}

pub fn raid() -> Vec<Raid> {
    Vec::new()
}

pub fn temperatures() -> Vec<(Sensor, f64)> {
    (0..)
        .map_while(|core| {
            let value = freebsd_parse::cpu_temperature(&sysctl(&format!("dev.cpu.{core}.temperature"))?)?;
            Some((Sensor { name: format!("CPU core {core}"), group: "CPU".into() }, value))
        })
        .collect()
}

pub fn smart(device: &str) -> Result<Smart, String> {
    let name = Path::new(device).file_name().map_or_else(|| device.into(), |n| n.to_string_lossy());
    let output = Command::new("nvmecontrol").args(["logpage", "-p", "2", &name]).output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }
    Ok(freebsd_parse::nvmecontrol_health(&name, &String::from_utf8_lossy(&output.stdout)))
}

pub fn pools() -> Vec<(String, u64, u64)> {
    Command::new("zpool")
        .args(["list", "-Hp", "-o", "name,size,allocated"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| freebsd_parse::zpool_list(&String::from_utf8_lossy(&output.stdout)))
        .unwrap_or_default()
}

pub fn failed_units(_root: &str) -> Result<Vec<String>, String> {
    Err("systemd is not available on FreeBSD".into())
}

pub fn install(config: &str) {
    let binary = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_else(|_| "/usr/local/bin/argus-agent".into());
    let config = std::path::absolute(config).map(|p| p.display().to_string()).unwrap_or_else(|_| config.into());
    let script = RC_SCRIPT.replace("@BINARY@", &binary).replace("@CONFIG@", &config);
    let written = std::fs::write(RC_PATH, script).and_then(|()| {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(RC_PATH, std::fs::Permissions::from_mode(0o755))
    });
    match written {
        Ok(()) => println!("Wrote {RC_PATH}\nEnable and start with: sysrc argus_agent_enable=YES && service argus_agent start"),
        Err(error) => {
            eprintln!("cannot write {RC_PATH}: {error}");
            std::process::exit(1);
        }
    }
}
