//! Read-only sampling of daemon-known processes.
//!
//! The sampler only looks at pids recorded in `supervised_processes`; it never
//! enumerates or signals foreign processes. It produces a bounded shadow view
//! for the GUI/CLI; no action is executed.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use anyhow::Result;
use rusqlite::{Connection, OpenFlags};
use sysinfo::{Pid, ProcessesToUpdate, System};

use crate::capacity::MachineCapacity;
use crate::protocol::ProcessPressure;
use crate::score::{ProcessSample, ProcessWindow, RecommendedAction, evaluate};

const MAX_TRACKED: usize = 32;
const MAX_SAMPLES: usize = 16;
const CACHE_TTL: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, Default)]
pub struct ProcessReport {
    pub tracked: u64,
    pub pressure: u64,
    pub shadow_stop: u64,
    pub samples: Vec<ProcessPressure>,
}

#[derive(Clone, Debug)]
struct SupervisedRow {
    pid: u32,
    session_id: String,
    label: String,
    heartbeat_age_secs: u64,
}

pub struct ProcessSampler {
    system: System,
    last_refresh: Instant,
}

impl Default for ProcessSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl ProcessSampler {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            last_refresh: Instant::now(),
        }
    }

    pub fn sample(&mut self, data_dir: &Path, capacity: &MachineCapacity) -> ProcessReport {
        let rows = match list_rows(data_dir) {
            Ok(rows) => rows,
            Err(_) => return ProcessReport::default(),
        };
        if rows.is_empty() {
            return ProcessReport::default();
        }
        let elapsed = self.last_refresh.elapsed().max(Duration::from_millis(500));
        self.last_refresh = Instant::now();
        self.system.refresh_processes(ProcessesToUpdate::All, true);
        let child_counts = child_counts(&self.system);
        let mut samples = Vec::new();
        let mut pressure = 0u64;
        let mut shadow_stop = 0u64;
        for row in &rows {
            let Some(process) = self.system.process(Pid::from_u32(row.pid)) else {
                continue;
            };
            let elapsed_ms = elapsed.as_millis().min(u64::MAX as u128) as u64;
            let cpu_ms = ((process.cpu_usage() as f64 / 100.0)
                * elapsed_ms as f64
                * capacity.logical_cores.max(1) as f64)
                .round() as u64;
            let now_ms = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map(|value| value.as_millis().min(u64::MAX as u128) as u64)
                .unwrap_or(0);
            let window = ProcessWindow {
                first: ProcessSample {
                    at_ms: now_ms.saturating_sub(elapsed_ms),
                    cpu_time_ms: 0,
                    rss_bytes: process.memory(),
                    children: child_counts.get(&row.pid).copied().unwrap_or(0),
                    ..ProcessSample::default()
                },
                last: ProcessSample {
                    at_ms: now_ms,
                    cpu_time_ms: cpu_ms,
                    rss_bytes: process.memory(),
                    children: child_counts.get(&row.pid).copied().unwrap_or(0),
                    ..ProcessSample::default()
                },
                last_progress_age_secs: row.heartbeat_age_secs,
                heartbeat_age_secs: row.heartbeat_age_secs,
                term_ignored_secs: 0,
                declared_expected_secs: None,
                calibrated_p95_secs: None,
                workload_samples: 0,
            };
            let verdict = evaluate(&window, capacity);
            if verdict.action != RecommendedAction::None {
                pressure += 1;
            }
            if verdict.action == RecommendedAction::Stop {
                shadow_stop += 1;
            }
            if samples.len() < MAX_SAMPLES {
                samples.push(ProcessPressure {
                    pid: row.pid,
                    session_id: row.session_id.clone(),
                    label: row.label.clone(),
                    state: verdict.state.as_str().to_string(),
                    score: verdict.score,
                    action: verdict.action.as_str().to_string(),
                    reason: verdict.reason,
                    rss_bytes: window.last.rss_bytes,
                    children: window.last.children,
                    cpu_share: verdict.cpu_share,
                    no_progress_secs: verdict.no_progress_secs,
                    learning_mode: verdict.learning_mode,
                });
            }
        }
        ProcessReport {
            tracked: rows.len() as u64,
            pressure,
            shadow_stop,
            samples,
        }
    }
}

#[derive(Clone, Default)]
pub struct ProcessCache {
    inner: Arc<Mutex<Option<(Instant, ProcessReport)>>>,
    sampler: Arc<Mutex<ProcessSampler>>,
}

impl ProcessCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn get(&self, data_dir: &Path, capacity: &MachineCapacity) -> ProcessReport {
        if let Ok(cache) = self.inner.lock()
            && let Some((at, report)) = cache.as_ref()
            && at.elapsed() <= CACHE_TTL
        {
            return report.clone();
        }
        let report = self
            .sampler
            .lock()
            .map(|mut sampler| sampler.sample(data_dir, capacity))
            .unwrap_or_default();
        if let Ok(mut cache) = self.inner.lock() {
            *cache = Some((Instant::now(), report.clone()));
        }
        report
    }
}

fn list_rows(data_dir: &Path) -> Result<Vec<SupervisedRow>> {
    let path = data_dir.join("verlinpark.db");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_millis(500))?;
    let mut statement = connection.prepare(
        "SELECT pid, session_id, COALESCE(name, ''), \
         COALESCE(CAST(strftime('%s','now') AS INTEGER) \
         - CAST(strftime('%s', heartbeat_at) AS INTEGER), 0) \
         FROM supervised_processes \
         WHERE state IN ('running','live','starting') AND pid IS NOT NULL \
         ORDER BY updated_at DESC LIMIT ?1",
    )?;
    let rows = statement.query_map([MAX_TRACKED as i64], |row| {
        let pid: i64 = row.get(0)?;
        let session_id: String = row.get(1)?;
        let name: String = row.get(2)?;
        let age: i64 = row.get(3)?;
        Ok(SupervisedRow {
            pid: u32::try_from(pid.max(0)).unwrap_or(0),
            session_id,
            label: if name.is_empty() {
                "supervised process".to_string()
            } else {
                name
            },
            heartbeat_age_secs: u64::try_from(age.max(0)).unwrap_or(0),
        })
    })?;
    let mut result = Vec::new();
    for row in rows {
        result.push(row?);
    }
    Ok(result)
}

fn child_counts(system: &System) -> HashMap<u32, u32> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    for process in system.processes().values() {
        if let Some(parent) = process.parent() {
            children
                .entry(parent.as_u32())
                .or_default()
                .push(process.pid().as_u32());
        }
    }
    let mut counts = HashMap::new();
    for pid in system.processes().keys().map(|pid| pid.as_u32()) {
        counts.insert(pid, descendant_count(pid, &children, 0));
    }
    counts
}

fn descendant_count(pid: u32, children: &HashMap<u32, Vec<u32>>, depth: u8) -> u32 {
    if depth >= 8 {
        return 0;
    }
    let mut total = 0u32;
    if let Some(child_list) = children.get(&pid) {
        for child in child_list {
            total = total.saturating_add(1);
            total = total.saturating_add(descendant_count(*child, children, depth + 1));
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_database_has_no_samples() {
        let dir = tempfile::TempDir::new().unwrap();
        let capacity = MachineCapacity {
            physical_memory_bytes: 16 * 1024 * 1024 * 1024,
            logical_cores: 8,
            disk_free_bytes: 100 * 1024 * 1024 * 1024,
            disk_total_bytes: 200 * 1024 * 1024 * 1024,
            memory_pressure: crate::capacity::MemoryPressure::Normal,
            thermal: crate::capacity::ThermalState::Nominal,
            low_power: false,
        };
        let mut sampler = ProcessSampler::new();
        let report = sampler.sample(dir.path(), &capacity);
        assert_eq!(report.tracked, 0);
        assert!(report.samples.is_empty());
    }

    #[test]
    fn child_count_handles_missing_parents() {
        let system = System::new();
        let counts = child_counts(&system);
        assert!(counts.is_empty());
    }
}
