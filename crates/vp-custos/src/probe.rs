//! Read-only SQLite health probes.
//!
//! Custos opens the live database with `SQLITE_OPEN_READ_ONLY` and never calls
//! `Store::open()`; opening the daemon store would create/migrate schema and a
//! daemon identity. A missing or locked database is reported, not repaired.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use serde::{Deserialize, Serialize};

use crate::protocol::{Counts, ProbeInfo};

pub const EXPECTED_SCHEMA_VERSION: i64 = 1;
const BUSY_TIMEOUT: Duration = Duration::from_millis(500);
const MAX_ERRORS: usize = 16;
const MAX_TABLE_KEYS: usize = 32;

const REQUIRED_TABLES: &[&str] = &[
    "sessions",
    "events",
    "runtime_leases",
    "session_runs",
    "plan_tasks",
    "merge_queue",
    "effect_receipts",
    "approvals",
    "supervised_processes",
];

const STATUS_TABLES: &[(&str, &str)] = &[
    ("sessions", "status"),
    ("session_runs", "status"),
    ("plan_tasks", "status"),
    ("merge_queue", "status"),
    ("effect_receipts", "state"),
    ("approvals", "status"),
    ("supervised_processes", "state"),
];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProbeReport {
    pub counts: Counts,
    pub info: ProbeInfo,
}

pub fn database_path(data_dir: &Path) -> PathBuf {
    data_dir.join("verlinpark.db")
}

pub fn probe(data_dir: &Path) -> ProbeReport {
    let started = Instant::now();
    let path = database_path(data_dir);
    let mut info = ProbeInfo {
        db_present: path.is_file(),
        db_open: false,
        user_version: None,
        journal_mode: None,
        events_total: 0,
        event_age_secs: None,
        table_counts: BTreeMap::new(),
        missing_tables: Vec::new(),
        errors: Vec::new(),
        probe_ms: 0,
    };
    let mut counts = Counts::default();
    if let Ok(metadata) = std::fs::metadata(&path) {
        counts.db_bytes = metadata.len();
    }
    if let Ok(metadata) = std::fs::metadata(path.with_extension("db-wal")) {
        counts.wal_bytes = metadata.len();
    }
    counts.disk_free_bytes = fs2::available_space(data_dir).unwrap_or(0);
    counts.disk_total_bytes = fs2::total_space(data_dir).unwrap_or(0);

    if !info.db_present {
        info.errors.push("database missing".to_string());
        info.probe_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
        return ProbeReport { counts, info };
    }

    let connection = match open_read_only(&path) {
        Ok(connection) => connection,
        Err(error) => {
            push_error(&mut info.errors, format!("database open failed: {error}"));
            info.probe_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
            return ProbeReport { counts, info };
        }
    };
    info.db_open = true;
    info.user_version = query_i64(&connection, "PRAGMA user_version").ok();
    info.journal_mode = query_string(&connection, "PRAGMA journal_mode").ok();

    for table in REQUIRED_TABLES {
        if !table_exists(&connection, table) {
            info.missing_tables.push((*table).to_string());
        }
    }

    for (table, column) in STATUS_TABLES {
        if info.missing_tables.iter().any(|missing| missing == table) {
            continue;
        }
        match count_by_status(&connection, table, column) {
            Ok(values) => {
                info.table_counts.insert((*table).to_string(), values);
            }
            Err(error) => {
                push_error(
                    &mut info.errors,
                    format!("{table}.{column} probe failed: {error}"),
                );
            }
        }
    }

    if !info.missing_tables.iter().any(|table| table == "events") {
        info.events_total = count_total(&connection, "events").unwrap_or(0);
        info.event_age_secs = event_age_secs(&connection).ok().flatten();
    }
    counts.events_total = info.events_total;
    counts.event_age_secs = info.event_age_secs;
    counts.sessions_total = count_total(&connection, "sessions").unwrap_or(0);
    counts.sessions_running = status_sum(&info, "sessions", &["running"]);
    counts.sessions_paused = status_sum(&info, "sessions", &["paused"]);
    counts.sessions_failed = status_sum(&info, "sessions", &["failed"]);
    counts.runs_running = status_sum(&info, "session_runs", &["running"]);
    counts.runs_interrupted = status_sum(&info, "session_runs", &["interrupted"]);
    counts.tasks_pending = status_sum(&info, "plan_tasks", &["pending"]);
    counts.tasks_running = status_sum(&info, "plan_tasks", &["running"]);
    counts.tasks_blocked = status_sum(&info, "plan_tasks", &["blocked"]);
    counts.merges_queued = status_sum(&info, "merge_queue", &["queued"]);
    counts.merges_conflict = status_sum(&info, "merge_queue", &["conflict", "failed"]);
    counts.effects_unknown = status_sum(&info, "effect_receipts", &["unknown"]);
    counts.approvals_pending = status_sum(&info, "approvals", &["pending"]);
    counts.processes_live = status_sum(
        &info,
        "supervised_processes",
        &["running", "live", "starting"],
    );
    counts.leases_live = query_count(
        &connection,
        "SELECT COUNT(*) FROM runtime_leases \
         WHERE released_at IS NULL AND expires_at IS NOT NULL \
         AND datetime(expires_at) > datetime('now')",
    )
    .unwrap_or(0);
    counts.leases_expired = query_count(
        &connection,
        "SELECT COUNT(*) FROM runtime_leases \
         WHERE released_at IS NULL AND expires_at IS NOT NULL \
         AND datetime(expires_at) <= datetime('now')",
    )
    .unwrap_or(0);

    info.probe_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    ProbeReport { counts, info }
}

fn open_read_only(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(BUSY_TIMEOUT)?;
    connection.execute_batch("PRAGMA query_only = ON;")?;
    Ok(connection)
}

fn table_exists(connection: &Connection, table: &str) -> bool {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [table],
            |_| Ok(()),
        )
        .is_ok()
}

fn count_total(connection: &Connection, table: &str) -> Result<u64> {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    let value: i64 = connection.query_row(&sql, [], |row| row.get(0))?;
    Ok(value.max(0) as u64)
}

fn count_by_status(
    connection: &Connection,
    table: &str,
    column: &str,
) -> Result<BTreeMap<String, u64>> {
    let sql = format!(
        "SELECT {column}, COUNT(*) FROM {table} \
         GROUP BY {column} ORDER BY COUNT(*) DESC LIMIT {MAX_TABLE_KEYS}"
    );
    let mut statement = connection.prepare(&sql)?;
    let rows = statement.query_map([], |row| {
        let key: Option<String> = row.get(0)?;
        let count: i64 = row.get(1)?;
        Ok((
            key.unwrap_or_else(|| "(null)".to_string()),
            count.max(0) as u64,
        ))
    })?;
    let mut values = BTreeMap::new();
    for row in rows {
        let (key, count) = row?;
        values.insert(key, count);
    }
    Ok(values)
}

fn query_count(connection: &Connection, sql: &str) -> Result<u64> {
    let value: i64 = connection.query_row(sql, [], |row| row.get(0))?;
    Ok(value.max(0) as u64)
}

fn query_i64(connection: &Connection, sql: &str) -> Result<i64> {
    Ok(connection.query_row(sql, [], |row| row.get(0))?)
}

fn query_string(connection: &Connection, sql: &str) -> Result<String> {
    Ok(connection.query_row(sql, [], |row| row.get(0))?)
}

fn event_age_secs(connection: &Connection) -> Result<Option<u64>> {
    let value: Option<i64> = connection.query_row(
        "SELECT CAST(strftime('%s','now') AS INTEGER) \
         - CAST(strftime('%s', MAX(created_at)) AS INTEGER) FROM events",
        [],
        |row| row.get(0),
    )?;
    Ok(value.and_then(|value| u64::try_from(value.max(0)).ok()))
}

fn status_sum(info: &ProbeInfo, table: &str, statuses: &[&str]) -> u64 {
    info.table_counts
        .get(table)
        .map(|values| {
            statuses
                .iter()
                .filter_map(|status| values.get(*status))
                .sum()
        })
        .unwrap_or(0)
}

fn push_error(errors: &mut Vec<String>, message: String) {
    if errors.len() < MAX_ERRORS {
        errors.push(message.chars().take(300).collect());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::TempDir::new().unwrap();
        let path = database_path(dir.path());
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch(
                "
                CREATE TABLE sessions(status TEXT);
                CREATE TABLE events(type TEXT, created_at TEXT);
                CREATE TABLE runtime_leases(released_at TEXT, expires_at TEXT);
                CREATE TABLE session_runs(status TEXT);
                CREATE TABLE plan_tasks(status TEXT);
                CREATE TABLE merge_queue(status TEXT);
                CREATE TABLE effect_receipts(state TEXT);
                CREATE TABLE approvals(status TEXT);
                CREATE TABLE supervised_processes(state TEXT);
                INSERT INTO sessions VALUES ('running'), ('running'), ('paused'), ('failed');
                INSERT INTO events VALUES ('agent.message', datetime('now'));
                INSERT INTO session_runs VALUES ('running'), ('interrupted');
                INSERT INTO plan_tasks VALUES ('pending'), ('pending'), ('running'), ('blocked');
                INSERT INTO merge_queue VALUES ('queued'), ('conflict');
                INSERT INTO effect_receipts VALUES ('unknown'), ('done');
                INSERT INTO approvals VALUES ('pending'), ('approved');
                INSERT INTO supervised_processes VALUES ('running'), ('exited');
                INSERT INTO runtime_leases VALUES (NULL, datetime('now', '+10 minutes'));
                INSERT INTO runtime_leases VALUES (NULL, datetime('now', '-10 minutes'));
                ",
            )
            .unwrap();
        (dir, path)
    }

    #[test]
    fn counts_core_tables_without_writing() {
        let (dir, _path) = fixture();
        let report = probe(dir.path());
        assert!(report.info.db_open);
        assert_eq!(report.info.user_version, Some(0));
        assert_eq!(report.counts.sessions_total, 4);
        assert_eq!(report.counts.sessions_running, 2);
        assert_eq!(report.counts.sessions_paused, 1);
        assert_eq!(report.counts.sessions_failed, 1);
        assert_eq!(report.counts.events_total, 1);
        assert!(report.counts.event_age_secs.unwrap_or(99) <= 5);
        assert_eq!(report.counts.leases_live, 1);
        assert_eq!(report.counts.leases_expired, 1);
        assert_eq!(report.counts.runs_interrupted, 1);
        assert_eq!(report.counts.tasks_pending, 2);
        assert_eq!(report.counts.tasks_blocked, 1);
        assert_eq!(report.counts.merges_queued, 1);
        assert_eq!(report.counts.merges_conflict, 1);
        assert_eq!(report.counts.effects_unknown, 1);
        assert_eq!(report.counts.approvals_pending, 1);
        assert_eq!(report.counts.processes_live, 1);
        assert!(report.info.missing_tables.is_empty());
    }

    #[test]
    fn missing_database_is_reported_not_created() {
        let dir = tempfile::TempDir::new().unwrap();
        let report = probe(dir.path());
        assert!(!report.info.db_present);
        assert!(!report.info.db_open);
        assert!(!database_path(dir.path()).exists());
    }

    #[test]
    fn missing_tables_are_reported() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = database_path(dir.path());
        let connection = Connection::open(&path).unwrap();
        connection
            .execute_batch("CREATE TABLE sessions(status TEXT);")
            .unwrap();
        let report = probe(dir.path());
        assert!(
            report
                .info
                .missing_tables
                .iter()
                .any(|table| table == "events")
        );
        assert_eq!(report.counts.sessions_total, 0);
    }
}
