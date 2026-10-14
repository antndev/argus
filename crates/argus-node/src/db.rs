use std::collections::BTreeMap;

use argus_model::{Alert, AlertState, BackupSchedule, Event, HostSnapshot, Message, Sample, SeriesData};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::config::Retention;
use crate::util::{DAY, HOUR, MINUTE, glob};

pub type Db = Connection;

const SCHEMA: &str = "
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
CREATE TABLE IF NOT EXISTS series (id INTEGER PRIMARY KEY, host TEXT NOT NULL, key TEXT NOT NULL, UNIQUE (host, key));
CREATE TABLE IF NOT EXISTS samples (series INTEGER NOT NULL, time INTEGER NOT NULL, value REAL NOT NULL, PRIMARY KEY (series, time)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS samples_5m (series INTEGER NOT NULL, time INTEGER NOT NULL, value REAL NOT NULL, PRIMARY KEY (series, time)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS samples_1h (series INTEGER NOT NULL, time INTEGER NOT NULL, value REAL NOT NULL, PRIMARY KEY (series, time)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS samples_1d (series INTEGER NOT NULL, time INTEGER NOT NULL, value REAL NOT NULL, PRIMARY KEY (series, time)) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS hosts (id TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS alerts (key TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS events (id TEXT PRIMARY KEY, time INTEGER NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS messages (id TEXT PRIMARY KEY, time INTEGER NOT NULL, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS sent (id TEXT PRIMARY KEY, time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS secrets (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS receipts (id TEXT PRIMARY KEY, time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS deliveries (id TEXT PRIMARY KEY, time INTEGER NOT NULL);
CREATE TABLE IF NOT EXISTS backups (name TEXT PRIMARY KEY, data TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value INTEGER NOT NULL);
CREATE INDEX IF NOT EXISTS events_time ON events (time);
CREATE INDEX IF NOT EXISTS messages_time ON messages (time);
";

const TIERS: [(&str, i64); 4] = [("samples", 0), ("samples_5m", 5 * MINUTE), ("samples_1h", HOUR), ("samples_1d", DAY)];
const CLEARED: &str = "history_cleared";

pub fn open(path: &str) -> rusqlite::Result<Db> {
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch(SCHEMA)?;
    Ok(db)
}

fn json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

fn parse<T: DeserializeOwned>(text: String) -> Option<T> {
    serde_json::from_str(&text).ok()
}

fn all<T: DeserializeOwned>(db: &Db, sql: &str) -> Vec<T> {
    let Ok(mut statement) = db.prepare_cached(sql) else {
        return Vec::new();
    };
    statement.query_map([], |row| row.get::<_, String>(0)).map(|rows| rows.filter_map(Result::ok).filter_map(parse).collect()).unwrap_or_default()
}

pub fn series_id(db: &Db, host: &str, key: &str) -> rusqlite::Result<i64> {
    db.prepare_cached("INSERT OR IGNORE INTO series (host, key) VALUES (?1, ?2)")?.execute(params![host, key])?;
    db.prepare_cached("SELECT id FROM series WHERE host = ?1 AND key = ?2")?.query_row(params![host, key], |row| row.get(0))
}

pub fn insert_samples(db: &mut Db, samples: &[Sample]) -> rusqlite::Result<()> {
    let tx = db.transaction()?;
    for sample in samples {
        let id = series_id(&tx, &sample.host, &sample.key)?;
        tx.prepare_cached("INSERT OR REPLACE INTO samples (series, time, value) VALUES (?1, ?2, ?3)")?.execute(params![id, sample.time, sample.value])?;
    }
    tx.commit()
}

pub fn samples_since(db: &Db, hosts: &[String], after: i64, limit: usize) -> Vec<Sample> {
    let Ok(mut statement) = db.prepare_cached(
        "SELECT s.host, s.key, v.time, v.value FROM samples v JOIN series s ON s.id = v.series WHERE s.host = ?1 AND v.time > ?2 ORDER BY v.time LIMIT ?3",
    ) else {
        return Vec::new();
    };
    let mut result = Vec::new();
    for host in hosts {
        let rows = statement
            .query_map(params![host, after, limit as i64 + 1], |row| Ok(Sample { host: row.get(0)?, key: row.get(1)?, time: row.get(2)?, value: row.get(3)? }));
        if let Ok(rows) = rows {
            result.extend(rows.filter_map(Result::ok));
        }
    }
    result.sort_by_key(|s| s.time);
    if result.len() > limit {
        result.truncate(limit);
        let last = result.last().map_or(0, |s| s.time);
        if result.first().is_some_and(|s| s.time < last) {
            result.retain(|s| s.time < last);
        }
    }
    result
}

pub fn reopen_rollups(db: &Db, from: i64) -> rusqlite::Result<()> {
    for (target, _) in &TIERS[1..] {
        db.execute("UPDATE meta SET value = min(value, ?2) WHERE key = ?1", params![format!("rollup_{target}"), from])?;
    }
    Ok(())
}

pub fn query_series(db: &Db, host: &str, keys: &[String], from: i64, to: i64, retention: &Retention) -> SeriesData {
    let span = (to - from).max(MINUTE);
    let (table, resolution) = if from >= to - retention.raw.0 {
        TIERS[0]
    } else if from >= to - retention.medium.0 {
        TIERS[1]
    } else if from >= to - retention.long.0 {
        TIERS[2]
    } else {
        TIERS[3]
    };
    let step = (span / 360).max(resolution).max(30_000);
    let start = from / step * step;
    let times: Vec<i64> = (0..).map(|i| start + i * step).take_while(|t| *t <= to).collect();
    let sql = format!("SELECT (time / ?2) * ?2 AS bucket, avg(value) FROM {table} WHERE series = ?1 AND time >= ?3 AND time <= ?4 GROUP BY bucket");
    let known: Vec<(i64, String)> = db
        .prepare_cached("SELECT id, key FROM series WHERE host = ?1")
        .and_then(|mut s| s.query_map([host], |row| Ok((row.get(0)?, row.get(1)?))).map(|rows| rows.filter_map(Result::ok).collect()))
        .unwrap_or_default();
    let values = keys
        .iter()
        .map(|key| {
            let mut buckets: BTreeMap<i64, f64> = BTreeMap::new();
            for (id, _) in known.iter().filter(|(_, name)| if key.contains('*') { glob(key, name) } else { name == key }) {
                if let Ok(mut statement) = db.prepare_cached(&sql)
                    && let Ok(rows) = statement.query_map(params![id, step, start, to], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?)))
                {
                    for (bucket, value) in rows.filter_map(Result::ok) {
                        *buckets.entry(bucket).or_default() += value;
                    }
                }
            }
            times.iter().map(|t| buckets.get(t).copied()).collect()
        })
        .collect();
    SeriesData { times, values }
}

pub fn compact(db: &mut Db, now: i64, retention: &Retention) -> rusqlite::Result<()> {
    let tx = db.transaction()?;
    let ids: Vec<i64> = tx.prepare("SELECT id FROM series")?.query_map([], |row| row.get(0))?.filter_map(Result::ok).collect();
    for window in TIERS.windows(2) {
        let ((source, _), (target, size)) = (window[0], window[1]);
        let mark = format!("rollup_{target}");
        let done: i64 = tx.query_row("SELECT value FROM meta WHERE key = ?1", [&mark], |row| row.get(0)).optional()?.unwrap_or(0);
        let until = now / size * size;
        let from = (done - 2 * size).max(0);
        let mut statement = tx.prepare(&format!(
            "INSERT OR REPLACE INTO {target} (series, time, value) SELECT series, (time / ?2) * ?2, avg(value) FROM {source} WHERE series = ?1 AND time >= ?3 AND time < ?4 GROUP BY (time / ?2)"
        ))?;
        for id in &ids {
            statement.execute(params![id, size, from, until])?;
        }
        tx.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", params![mark, until])?;
    }
    for ((table, _), keep) in TIERS.iter().zip([retention.raw.0, retention.medium.0, retention.long.0, retention.daily.0]) {
        let mut statement = tx.prepare(&format!("DELETE FROM {table} WHERE series = ?1 AND time < ?2"))?;
        for id in &ids {
            statement.execute(params![id, now - keep])?;
        }
    }
    tx.execute("DELETE FROM messages WHERE time < ?1", [now - 7 * DAY])?;
    tx.execute("DELETE FROM sent WHERE time < ?1", [now - 7 * DAY])?;
    tx.execute("DELETE FROM receipts WHERE time < ?1", [now - 7 * DAY])?;
    tx.execute("DELETE FROM deliveries WHERE time < ?1", [now - 7 * DAY])?;
    tx.execute("DELETE FROM events WHERE time < ?1", [now - 30 * DAY])?;
    tx.commit()
}

pub fn hosts(db: &Db) -> Vec<HostSnapshot> {
    all(db, "SELECT data FROM hosts")
}

pub fn save_host(db: &Db, snapshot: &HostSnapshot) -> rusqlite::Result<()> {
    db.prepare_cached("INSERT OR REPLACE INTO hosts (id, data) VALUES (?1, ?2)")?.execute(params![snapshot.report.host.id, json(snapshot)])?;
    Ok(())
}

pub fn delete_host(db: &Db, host: &str) -> rusqlite::Result<()> {
    db.prepare_cached("DELETE FROM hosts WHERE id = ?1")?.execute([host])?;
    db.prepare_cached("DELETE FROM alerts WHERE substr(key, 1, length(?1) + 1) = ?1 || '|'")?.execute([host])?;
    Ok(())
}

pub fn alerts(db: &Db) -> Vec<Alert> {
    all(db, "SELECT data FROM alerts")
}

pub fn save_alert(db: &Db, alert: &Alert) -> rusqlite::Result<()> {
    db.prepare_cached("INSERT OR REPLACE INTO alerts (key, data) VALUES (?1, ?2)")?.execute(params![alert.key, json(alert)])?;
    Ok(())
}

pub fn delete_alert(db: &Db, key: &str) -> rusqlite::Result<()> {
    db.prepare_cached("DELETE FROM alerts WHERE key = ?1")?.execute([key])?;
    Ok(())
}

pub fn prune(db: &Db, now: i64) {
    let stale: Vec<String> =
        alerts(db).into_iter().filter(|a| a.state == AlertState::Resolved && a.resolved_at.is_some_and(|t| t < now - DAY)).map(|a| a.key).collect();
    for key in stale {
        let _ = delete_alert(db, &key);
    }
}

pub fn events(db: &Db, limit: usize) -> Vec<Event> {
    all(db, &format!("SELECT data FROM events ORDER BY time DESC LIMIT {limit}"))
}

pub fn save_event(db: &Db, event: &Event) -> rusqlite::Result<()> {
    let cleared = history_cleared(db);
    if cleared > 0 && event.time <= cleared {
        return Ok(());
    }
    db.prepare_cached("INSERT OR IGNORE INTO events (id, time, data) VALUES (?1, ?2, ?3)")?.execute(params![event.id, event.time, json(event)])?;
    Ok(())
}

pub fn history_cleared(db: &Db) -> i64 {
    meta(db, CLEARED)
}

pub fn clear_history(db: &Db, until: i64) -> rusqlite::Result<()> {
    if until <= history_cleared(db) {
        return Ok(());
    }
    set_meta(db, CLEARED, until)?;
    db.execute("DELETE FROM events WHERE time <= ?1", [until])?;
    Ok(())
}

pub fn messages(db: &Db) -> Vec<Message> {
    let Ok(mut statement) = db.prepare_cached("SELECT m.data, r.time FROM messages m LEFT JOIN receipts r ON r.id = m.id ORDER BY m.rowid") else {
        return Vec::new();
    };
    statement
        .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?)))
        .map(|rows| {
            rows.filter_map(Result::ok).filter_map(|(data, delivered)| parse::<Message>(data).map(|message| Message { delivered, ..message })).collect()
        })
        .unwrap_or_default()
}

pub fn unsent(db: &Db, from: i64, to: i64) -> Vec<Message> {
    let Ok(mut statement) = db.prepare_cached("SELECT data FROM messages WHERE time >= ?1 AND time <= ?2 AND id NOT IN (SELECT id FROM sent) ORDER BY rowid")
    else {
        return Vec::new();
    };
    statement
        .query_map(params![from, to], |row| row.get::<_, String>(0))
        .map(|rows| rows.filter_map(Result::ok).filter_map(parse).collect())
        .unwrap_or_default()
}

pub fn mark_sent(db: &Db, id: &str, at: i64) -> rusqlite::Result<()> {
    db.prepare_cached("INSERT OR IGNORE INTO sent (id, time) VALUES (?1, ?2)")?.execute(params![id, at])?;
    Ok(())
}

pub fn mark_delivered(db: &Db, id: &str, at: i64) -> rusqlite::Result<()> {
    db.prepare_cached("INSERT OR IGNORE INTO receipts (id, time) VALUES (?1, ?2)")?.execute(params![id, at])?;
    Ok(())
}

pub fn delivered(db: &Db, key: &str) -> bool {
    db.query_row("SELECT 1 FROM deliveries WHERE id = ?1", [key], |_| Ok(())).optional().ok().flatten().is_some()
}

pub fn mark_delivery(db: &Db, key: &str, time: i64) -> rusqlite::Result<()> {
    db.execute("INSERT OR IGNORE INTO deliveries (id, time) VALUES (?1, ?2)", params![key, time])?;
    Ok(())
}

pub fn marks(db: &Db, table: Marks, since: i64) -> Vec<(String, i64)> {
    let sql = match table {
        Marks::Sent => "SELECT id, time FROM sent WHERE time > ?1",
        Marks::Receipts => "SELECT r.id, r.time FROM receipts r JOIN messages m ON m.id = r.id WHERE r.time > ?1",
    };
    let Ok(mut statement) = db.prepare_cached(sql) else { return Vec::new() };
    statement.query_map([since], |row| Ok((row.get(0)?, row.get(1)?))).map(|rows| rows.filter_map(Result::ok).collect()).unwrap_or_default()
}

#[derive(Clone, Copy)]
pub enum Marks {
    Sent,
    Receipts,
}

pub fn save_message(db: &Db, message: &Message) -> rusqlite::Result<bool> {
    let inserted =
        db.prepare_cached("INSERT OR IGNORE INTO messages (id, time, data) VALUES (?1, ?2, ?3)")?.execute(params![message.id, message.time, json(message)])?;
    Ok(inserted > 0)
}

pub fn backups(db: &Db) -> Vec<BackupSchedule> {
    all(db, "SELECT data FROM backups ORDER BY name")
}

pub fn save_backup(db: &Db, backup: &BackupSchedule) -> rusqlite::Result<()> {
    db.prepare_cached("INSERT OR REPLACE INTO backups (name, data) VALUES (?1, ?2)")?.execute(params![backup.name, json(backup)])?;
    Ok(())
}

pub fn secret(db: &Db, key: &str) -> Option<String> {
    db.prepare_cached("SELECT value FROM secrets WHERE key = ?1").ok()?.query_row([key], |row| row.get(0)).optional().ok().flatten()
}

pub fn create_secret(db: &Db, key: &str, value: &str) -> rusqlite::Result<bool> {
    Ok(db.prepare_cached("INSERT OR IGNORE INTO secrets (key, value) VALUES (?1, ?2)")?.execute(params![key, value])? > 0)
}

pub fn set_secret(db: &Db, key: &str, value: &str) -> rusqlite::Result<()> {
    db.prepare_cached("INSERT OR REPLACE INTO secrets (key, value) VALUES (?1, ?2)")?.execute(params![key, value])?;
    Ok(())
}

pub fn delete_secret(db: &Db, key: &str) -> rusqlite::Result<()> {
    db.prepare_cached("DELETE FROM secrets WHERE key = ?1")?.execute([key])?;
    Ok(())
}

pub fn meta(db: &Db, key: &str) -> i64 {
    db.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| row.get(0)).optional().ok().flatten().unwrap_or(0)
}

pub fn set_meta(db: &Db, key: &str, value: i64) -> rusqlite::Result<()> {
    db.execute("INSERT OR REPLACE INTO meta (key, value) VALUES (?1, ?2)", params![key, value])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Span;

    fn retention() -> Retention {
        Retention { raw: Span(7 * DAY), medium: Span(90 * DAY), long: Span(730 * DAY), daily: Span(3650 * DAY) }
    }

    fn sample(time: i64, value: f64) -> Sample {
        Sample { host: "h".into(), key: "cpu||total".into(), time, value }
    }

    #[test]
    fn stores_queries_and_rolls_up() {
        let mut db = open(":memory:").expect("db");
        let base = 1_000 * DAY;
        let samples: Vec<Sample> = (0..120).map(|i| sample(base + i * 30_000, (i % 10) as f64)).collect();
        insert_samples(&mut db, &samples).expect("insert");
        let data = query_series(&db, "h", &["cpu||total".into(), "missing".into()], base, base + HOUR, &retention());
        assert_eq!(data.values.len(), 2);
        assert!(data.values[0].iter().flatten().count() > 0);
        assert!(data.values[1].iter().all(Option::is_none));
        compact(&mut db, base + 2 * HOUR, &retention()).expect("compact");
        let rolled: i64 = db.query_row("SELECT count(*) FROM samples_5m", [], |r| r.get(0)).expect("count");
        assert_eq!(rolled, 12);
        compact(&mut db, base + 8 * DAY, &retention()).expect("compact");
        let raw: i64 = db.query_row("SELECT count(*) FROM samples", [], |r| r.get(0)).expect("count");
        assert_eq!(raw, 0);
        assert_eq!(samples_since(&db, &["h".into()], 0, 10).len(), 0);
    }

    #[test]
    fn wildcard_keys_sum_their_series() {
        let mut db = open(":memory:").expect("db");
        let base = 1_000 * DAY;
        let memory = |name: &str, value: f64| Sample { host: "h".into(), key: format!("docker|{name}|memory"), time: base, value };
        insert_samples(&mut db, &[memory("web", 100.0), memory("db", 250.0), sample(base, 5.0)]).expect("insert");
        let data = query_series(&db, "h", &["docker|*|memory".into(), "cpu||total".into()], base, base + HOUR, &retention());
        assert_eq!(data.values[0].iter().flatten().copied().collect::<Vec<_>>(), [350.0]);
        assert_eq!(data.values[1].iter().flatten().copied().collect::<Vec<_>>(), [5.0]);
    }

    #[test]
    fn deliveries_are_remembered_per_device_for_a_week() {
        let mut db = open(":memory:").expect("db");
        mark_delivery(&db, "m1|a", 1_000).expect("mark");
        assert!(delivered(&db, "m1|a"));
        assert!(!delivered(&db, "m1|b"));
        compact(&mut db, 1_000 + 8 * DAY, &retention()).expect("compact");
        assert!(!delivered(&db, "m1|a"));
    }

    #[test]
    fn cleared_history_stays_cleared() {
        let db = open(":memory:").expect("db");
        let event = |id: &str, time: i64| Event { id: id.into(), time, ..Default::default() };
        save_event(&db, &event("a", 10)).expect("save");
        clear_history(&db, 20).expect("clear");
        clear_history(&db, 5).expect("older clear");
        save_event(&db, &event("b", 15)).expect("save");
        save_event(&db, &event("c", 30)).expect("save");
        assert_eq!(events(&db, 10).into_iter().map(|e| e.id).collect::<Vec<_>>(), ["c"]);
        assert_eq!(history_cleared(&db), 20);
    }

    #[test]
    fn samples_since_respects_cursor_and_limit() {
        let mut db = open(":memory:").expect("db");
        insert_samples(&mut db, &[sample(10, 1.0), sample(20, 2.0), sample(30, 3.0)]).expect("insert");
        let after = samples_since(&db, &["h".into()], 10, 1);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].time, 20);
    }

    #[test]
    fn a_full_page_never_splits_one_report() {
        let mut db = open(":memory:").expect("db");
        let at = |time, key: &str| Sample { host: "h".into(), key: key.into(), time, value: 1.0 };
        insert_samples(&mut db, &[at(10, "a"), at(20, "a"), at(20, "b"), at(20, "c")]).expect("insert");
        let page = samples_since(&db, &["h".into()], 0, 2);
        assert_eq!(page.iter().map(|s| s.time).collect::<Vec<_>>(), [10]);
        assert_eq!(samples_since(&db, &["h".into()], 10, 10).len(), 3);
    }

    #[test]
    fn duplicate_messages_are_reported() {
        let db = open(":memory:").expect("db");
        let message = Message { id: "m".into(), ..Default::default() };
        assert!(save_message(&db, &message).expect("save"));
        assert!(!save_message(&db, &message).expect("save"));
    }

    #[test]
    fn late_samples_reopen_rollups() {
        let mut db = open(":memory:").expect("db");
        compact(&mut db, 10 * DAY, &retention()).expect("compact");
        insert_samples(&mut db, &[sample(DAY, 4.0)]).expect("insert");
        reopen_rollups(&db, DAY).expect("reopen");
        compact(&mut db, 10 * DAY, &retention()).expect("compact");
        let rolled: i64 = db.query_row("SELECT count(*) FROM samples_5m", [], |r| r.get(0)).expect("count");
        assert_eq!(rolled, 1);
    }
}
