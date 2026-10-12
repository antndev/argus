use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

pub const SECOND: i64 = 1_000;
pub const MINUTE: i64 = 60 * SECOND;
pub const HOUR: i64 = 60 * MINUTE;
pub const DAY: i64 = 24 * HOUR;

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or_default()
}

pub fn random_id(length: usize) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789";
    let mut bytes = vec![0u8; length];
    getrandom::fill(&mut bytes).expect("operating system random source");
    bytes.iter().map(|b| ALPHABET[*b as usize % ALPHABET.len()] as char).collect()
}

pub fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data).iter().map(|b| format!("{b:02x}")).collect()
}

pub fn glob(pattern: &str, text: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = text.strip_prefix(first) else {
        return false;
    };
    let parts: Vec<&str> = parts.collect();
    let Some((last, middle)) = parts.split_last() else {
        return rest.is_empty();
    };
    for part in middle {
        match rest.find(part) {
            Some(index) => rest = &rest[index + part.len()..],
            None => return false,
        }
    }
    rest.ends_with(last) && rest.len() >= last.len()
}

pub fn matches_hosts(patterns: &[String], host: &str) -> bool {
    let mut matched = false;
    for pattern in patterns {
        match pattern.strip_prefix('!') {
            Some(excluded) if glob(excluded, host) => return false,
            Some(_) => {}
            None => matched |= glob(pattern, host),
        }
    }
    matched
}

pub fn duration_text(ms: i64) -> String {
    let minutes = (ms / MINUTE).max(0);
    match minutes {
        0 => format!("{} s", (ms / SECOND).max(1)),
        1..=59 => format!("{minutes} min"),
        _ if minutes < 24 * 60 => match minutes % 60 {
            0 => format!("{} h", minutes / 60),
            rest => format!("{} h {rest} min", minutes / 60),
        },
        _ => format!("{} d {} h", minutes / (24 * 60), minutes / 60 % 24),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn globs() {
        assert!(glob("*", "anything"));
        assert!(glob("temp|*|value", "temp|NVMe 0|value"));
        assert!(glob("temp|NVMe*|value", "temp|NVMe|value"));
        assert!(!glob("temp|NVMe*|value", "temp|CPU package|value"));
        assert!(glob("fs|/|used", "fs|/|used"));
        assert!(!glob("fs|/|used", "fs|/mnt|used"));
        assert!(glob("a*b*c", "a-b-b-c"));
        assert!(!glob("ab*ba", "aba"));
    }

    #[test]
    fn host_patterns_with_exclusions() {
        let patterns = vec!["*".to_string(), "!opnsense".to_string()];
        assert!(matches_hosts(&patterns, "homelab"));
        assert!(!matches_hosts(&patterns, "opnsense"));
        assert!(!matches_hosts(&["nas".to_string()], "homelab"));
    }

    #[test]
    fn durations() {
        assert_eq!(duration_text(30 * SECOND), "30 s");
        assert_eq!(duration_text(14 * MINUTE), "14 min");
        assert_eq!(duration_text(2 * HOUR + 14 * MINUTE), "2 h 14 min");
        assert_eq!(duration_text(26 * HOUR), "1 d 2 h");
    }
}
