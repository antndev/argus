use std::collections::HashMap;

use argus_model::{Raid, Sensor, Smart};

use crate::collect::{CpuTimes, Memory, Mount};

pub fn cpu_times() -> Option<CpuTimes> {
    None
}

pub fn memory() -> Option<Memory> {
    None
}

pub fn load() -> Option<[f64; 3]> {
    None
}

pub fn uptime() -> Option<u64> {
    None
}

pub fn mounts(_root: &str) -> Vec<Mount> {
    Vec::new()
}

pub fn net_bytes() -> Vec<(String, u64, u64)> {
    Vec::new()
}

pub fn interface_descriptions() -> HashMap<String, String> {
    HashMap::new()
}

pub fn raid() -> Vec<Raid> {
    Vec::new()
}

pub fn temperatures() -> Vec<(Sensor, f64)> {
    Vec::new()
}

pub fn smart(_device: &str) -> Result<Smart, String> {
    Err("SMART is only supported on Linux and FreeBSD".into())
}

pub fn pools() -> Vec<(String, u64, u64)> {
    Vec::new()
}

pub fn failed_units(_root: &str) -> Result<Vec<String>, String> {
    Err("systemd is only supported on Linux".into())
}
