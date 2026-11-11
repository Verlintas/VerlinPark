//! Operation ledger for idempotency and durable audit.
//!
//! Every mutating helper operation carries a caller-supplied `operation_id`.
//! The helper records the request fingerprint and result in an append-only
//! JSONL file before replying. A retry with the same id and payload returns the
//! recorded result instead of repeating the side effect. A different payload
//! under the same id is a conflict, never a silent overwrite.
//!
//! Retention is bounded: the in-memory index keeps at most `max_entries`, and
//! the file is compacted to the same retained window. The file is a local
//! operational audit, not a session event projection (ADR-0156).

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::protocol::{LedgerEntrySummary, RemoteError, RemoteErrorCode, validate_operation_id};

pub const DEFAULT_LEDGER_ENTRIES: usize = 512;
pub const MAX_LEDGER_ENTRIES: usize = 8_192;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub operation_id: String,
    pub method: String,
    pub request_hash: String,
    pub success: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RemoteError>,
    pub started_at: String,
    pub completed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerReplay {
    Result(Value),
    Error(RemoteError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LedgerBegin {
    Fresh,
    Replay(LedgerReplay),
    Conflict,
    InProgress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LedgerStats {
    pub entries: usize,
    pub max_entries: usize,
    pub persisted: bool,
    pub file_bytes: u64,
}

#[derive(Debug)]
pub struct OperationLedger {
    path: Option<PathBuf>,
    max_entries: usize,
    entries: HashMap<String, LedgerEntry>,
    order: VecDeque<String>,
    inflight: HashSet<String>,
    file_bytes: u64,
}

impl OperationLedger {
    pub fn in_memory(max_entries: usize) -> Self {
        Self {
            path: None,
            max_entries: max_entries.clamp(1, MAX_LEDGER_ENTRIES),
            entries: HashMap::new(),
            order: VecDeque::new(),
            inflight: HashSet::new(),
            file_bytes: 0,
        }
    }

    pub fn open(path: impl AsRef<Path>, max_entries: usize) -> Result<Self, RemoteError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            let mut builder = std::fs::DirBuilder::new();
            builder.recursive(true);
            #[cfg(unix)]
            builder.mode(0o700);
            builder.create(parent).map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::LedgerError,
                    format!(
                        "cannot create ledger directory {}: {error}",
                        parent.display()
                    ),
                )
            })?;
            #[cfg(unix)]
            let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
        }
        let mut ledger = Self {
            path: Some(path.clone()),
            max_entries: max_entries.clamp(1, MAX_LEDGER_ENTRIES),
            entries: HashMap::new(),
            order: VecDeque::new(),
            inflight: HashSet::new(),
            file_bytes: 0,
        };
        let file = match File::open(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(ledger),
            Err(error) => {
                return Err(RemoteError::new(
                    RemoteErrorCode::LedgerError,
                    format!("cannot open operation ledger {}: {error}", path.display()),
                ));
            }
        };
        #[cfg(unix)]
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        let mut lines = 0usize;
        for (index, line) in BufReader::new(file).lines().enumerate() {
            let line = line.map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::LedgerError,
                    format!("cannot read operation ledger {}: {error}", path.display()),
                )
            })?;
            if line.trim().is_empty() {
                continue;
            }
            lines += 1;
            match serde_json::from_str::<LedgerEntry>(&line) {
                Ok(entry) => {
                    if validate_operation_id(&entry.operation_id).is_err() {
                        continue;
                    }
                    if ledger.entries.contains_key(&entry.operation_id) {
                        ledger.order.retain(|id| id != &entry.operation_id);
                    }
                    ledger.order.push_back(entry.operation_id.clone());
                    ledger.entries.insert(entry.operation_id.clone(), entry);
                }
                Err(error) => {
                    // A torn final line from a crash is expected. Never abort
                    // the helper because of one malformed audit row.
                    tracing::warn!(
                        path = %path.display(),
                        line = index + 1,
                        %error,
                        "skipping malformed remote operation ledger line"
                    );
                }
            }
        }
        ledger.file_bytes = std::fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        ledger.trim_memory();
        if lines > ledger.max_entries.saturating_mul(2) {
            ledger.compact().map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::LedgerError,
                    format!(
                        "cannot compact operation ledger {}: {error}",
                        path.display()
                    ),
                )
            })?;
        }
        Ok(ledger)
    }

    pub fn request_hash(method: &str, params: &Value) -> String {
        let mut hasher = Sha256::new();
        hasher.update(method.as_bytes());
        hasher.update([0u8]);
        hasher.update(serde_json::to_vec(params).unwrap_or_default());
        format!("{:x}", hasher.finalize())
    }

    pub fn begin(
        &mut self,
        operation_id: &str,
        method: &str,
        request_hash: &str,
    ) -> Result<LedgerBegin, RemoteError> {
        validate_operation_id(operation_id)?;
        if self.inflight.contains(operation_id) {
            return Ok(LedgerBegin::InProgress);
        }
        if let Some(entry) = self.entries.get(operation_id) {
            if entry.method != method || entry.request_hash != request_hash {
                return Ok(LedgerBegin::Conflict);
            }
            if let Some(error) = &entry.error {
                return Ok(LedgerBegin::Replay(LedgerReplay::Error(error.clone())));
            }
            return Ok(LedgerBegin::Replay(LedgerReplay::Result(
                entry.result.clone().unwrap_or(Value::Null),
            )));
        }
        self.inflight.insert(operation_id.to_string());
        Ok(LedgerBegin::Fresh)
    }

    pub fn complete(&mut self, mut entry: LedgerEntry) -> Result<(), RemoteError> {
        validate_operation_id(&entry.operation_id)?;
        self.inflight.remove(&entry.operation_id);
        if entry.success && entry.error.is_some() {
            return Err(RemoteError::new(
                RemoteErrorCode::LedgerError,
                "ledger entry cannot be successful and carry an error",
            ));
        }
        if self.entries.contains_key(&entry.operation_id) {
            self.order.retain(|id| id != &entry.operation_id);
        }
        entry.completed_at = vp_protocol::now_rfc3339();
        self.order.push_back(entry.operation_id.clone());
        self.entries
            .insert(entry.operation_id.clone(), entry.clone());
        self.trim_memory();
        if self.path.is_some() {
            self.append(&entry)?;
        }
        Ok(())
    }

    /// Release an in-flight marker after a failure that happened before a
    /// result was produced. The failure itself is recorded so a retry with the
    /// same id does not repeat a possibly-partial side effect.
    pub fn abandon(
        &mut self,
        operation_id: &str,
        method: &str,
        request_hash: &str,
        error: RemoteError,
    ) -> Result<(), RemoteError> {
        self.complete(LedgerEntry {
            operation_id: operation_id.to_string(),
            method: method.to_string(),
            request_hash: request_hash.to_string(),
            success: false,
            result: None,
            error: Some(error),
            started_at: vp_protocol::now_rfc3339(),
            completed_at: vp_protocol::now_rfc3339(),
        })
    }

    pub fn get(&self, operation_id: &str) -> Option<&LedgerEntry> {
        self.entries.get(operation_id)
    }

    pub fn tail(&self, limit: usize) -> Vec<LedgerEntrySummary> {
        let mut entries = self
            .order
            .iter()
            .rev()
            .filter_map(|operation_id| self.entries.get(operation_id))
            .take(limit.min(200))
            .map(|entry| LedgerEntrySummary {
                operation_id: entry.operation_id.clone(),
                method: entry.method.clone(),
                success: entry.success,
                completed_at: entry.completed_at.clone(),
            })
            .collect::<Vec<_>>();
        entries.reverse();
        entries
    }

    pub fn stats(&self) -> LedgerStats {
        LedgerStats {
            entries: self.entries.len(),
            max_entries: self.max_entries,
            persisted: self.path.is_some(),
            file_bytes: self.file_bytes,
        }
    }

    fn trim_memory(&mut self) {
        while self.entries.len() > self.max_entries {
            let Some(oldest) = self.order.pop_front() else {
                break;
            };
            self.entries.remove(&oldest);
        }
        let retained = self.entries.keys().cloned().collect::<HashSet<_>>();
        self.order.retain(|id| retained.contains(id));
    }

    fn append(&mut self, entry: &LedgerEntry) -> Result<(), RemoteError> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        let bytes = serde_json::to_vec(entry).map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::LedgerError,
                format!("cannot encode operation ledger entry: {error}"),
            )
        })?;
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&path).map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::LedgerError,
                format!("cannot append operation ledger {}: {error}", path.display()),
            )
        })?;
        file.write_all(&bytes).map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::LedgerError,
                format!("cannot append operation ledger {}: {error}", path.display()),
            )
        })?;
        file.write_all(b"\n").map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::LedgerError,
                format!("cannot append operation ledger {}: {error}", path.display()),
            )
        })?;
        file.sync_data().map_err(|error| {
            RemoteError::new(
                RemoteErrorCode::LedgerError,
                format!("cannot sync operation ledger {}: {error}", path.display()),
            )
        })?;
        self.file_bytes = self.file_bytes.saturating_add(bytes.len() as u64 + 1);
        if self.file_bytes > (self.max_entries as u64).saturating_mul(64 * 1024) {
            self.compact().map_err(|error| {
                RemoteError::new(
                    RemoteErrorCode::LedgerError,
                    format!(
                        "cannot compact operation ledger {}: {error}",
                        path.display()
                    ),
                )
            })?;
        }
        Ok(())
    }

    fn compact(&mut self) -> std::io::Result<()> {
        let Some(path) = self.path.clone() else {
            return Ok(());
        };
        let temp = path.with_extension(format!("tmp-{}", std::process::id()));
        {
            let mut options = OpenOptions::new();
            options.create(true).truncate(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            let mut file = options.open(&temp)?;
            for operation_id in self.order.clone() {
                if let Some(entry) = self.entries.get(&operation_id) {
                    serde_json::to_writer(&mut file, entry)?;
                    file.write_all(b"\n")?;
                }
            }
            file.sync_all()?;
        }
        std::fs::rename(&temp, &path)?;
        self.file_bytes = std::fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn entry(id: &str, method: &str, hash: &str, result: Value) -> LedgerEntry {
        LedgerEntry {
            operation_id: id.to_string(),
            method: method.to_string(),
            request_hash: hash.to_string(),
            success: true,
            result: Some(result),
            error: None,
            started_at: vp_protocol::now_rfc3339(),
            completed_at: vp_protocol::now_rfc3339(),
        }
    }

    #[test]
    fn replays_completed_operations() {
        let mut ledger = OperationLedger::in_memory(8);
        let hash = OperationLedger::request_hash("write_file", &serde_json::json!({"a": 1}));
        assert_eq!(
            ledger.begin("op-1", "write_file", &hash).unwrap(),
            LedgerBegin::Fresh
        );
        ledger
            .complete(entry(
                "op-1",
                "write_file",
                &hash,
                serde_json::json!({"ok": true}),
            ))
            .unwrap();
        assert_eq!(
            ledger.begin("op-1", "write_file", &hash).unwrap(),
            LedgerBegin::Replay(LedgerReplay::Result(serde_json::json!({"ok": true})))
        );
    }

    #[test]
    fn detects_conflicting_payloads() {
        let mut ledger = OperationLedger::in_memory(8);
        let first = OperationLedger::request_hash("write_file", &serde_json::json!({"a": 1}));
        let second = OperationLedger::request_hash("write_file", &serde_json::json!({"a": 2}));
        ledger.begin("op-1", "write_file", &first).unwrap();
        ledger
            .complete(entry("op-1", "write_file", &first, serde_json::json!(1)))
            .unwrap();
        assert_eq!(
            ledger.begin("op-1", "write_file", &second).unwrap(),
            LedgerBegin::Conflict
        );
        assert_eq!(
            ledger.begin("op-1", "rename", &first).unwrap(),
            LedgerBegin::Conflict
        );
    }

    #[test]
    fn concurrent_duplicate_is_in_progress() {
        let mut ledger = OperationLedger::in_memory(8);
        let hash = OperationLedger::request_hash("exec", &serde_json::json!({}));
        assert_eq!(
            ledger.begin("op", "exec", &hash).unwrap(),
            LedgerBegin::Fresh
        );
        assert_eq!(
            ledger.begin("op", "exec", &hash).unwrap(),
            LedgerBegin::InProgress
        );
    }

    #[test]
    fn persisted_entries_survive_reopen() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let hash = OperationLedger::request_hash("exec", &serde_json::json!({"cmd": "true"}));
        {
            let mut ledger = OperationLedger::open(&path, 8).unwrap();
            ledger.begin("op-1", "exec", &hash).unwrap();
            ledger
                .complete(entry("op-1", "exec", &hash, serde_json::json!({"exit": 0})))
                .unwrap();
        }
        let ledger = OperationLedger::open(&path, 8).unwrap();
        assert_eq!(
            ledger.get("op-1").unwrap().result,
            Some(serde_json::json!({"exit": 0}))
        );
        assert!(path.exists());
    }

    #[test]
    fn retention_is_bounded_and_compacted() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ledger.jsonl");
        {
            let mut ledger = OperationLedger::open(&path, 3).unwrap();
            for index in 0..20 {
                let id = format!("op-{index}");
                let hash = OperationLedger::request_hash("exec", &serde_json::json!({"i": index}));
                ledger.begin(&id, "exec", &hash).unwrap();
                ledger
                    .complete(entry(&id, "exec", &hash, serde_json::json!(index)))
                    .unwrap();
            }
            assert!(ledger.stats().entries <= 3);
        }
        let ledger = OperationLedger::open(&path, 3).unwrap();
        assert!(ledger.stats().entries <= 3);
        assert!(ledger.get("op-0").is_none());
        assert!(ledger.get("op-19").is_some());
    }

    #[test]
    fn failures_are_replayed_instead_of_repeated() {
        let mut ledger = OperationLedger::in_memory(8);
        let hash = OperationLedger::request_hash("exec", &serde_json::json!({}));
        ledger.begin("op", "exec", &hash).unwrap();
        ledger
            .abandon(
                "op",
                "exec",
                &hash,
                RemoteError::new(RemoteErrorCode::Timeout, "timed out"),
            )
            .unwrap();
        assert!(matches!(
            ledger.begin("op", "exec", &hash).unwrap(),
            LedgerBegin::Replay(LedgerReplay::Error(_))
        ));
    }
}

#[cfg(test)]
mod additional_tests {
    use std::io::Write;

    use tempfile::TempDir;

    use super::*;

    #[test]
    fn tail_is_newest_last_and_bounded() {
        let mut ledger = OperationLedger::in_memory(32);
        for index in 0..5 {
            let id = format!("op-{index}");
            let hash = OperationLedger::request_hash("exec", &serde_json::json!({"i": index}));
            ledger.begin(&id, "exec", &hash).unwrap();
            ledger
                .complete(LedgerEntry {
                    operation_id: id,
                    method: "exec".into(),
                    request_hash: hash,
                    success: true,
                    result: Some(serde_json::json!(index)),
                    error: None,
                    started_at: vp_protocol::now_rfc3339(),
                    completed_at: vp_protocol::now_rfc3339(),
                })
                .unwrap();
        }
        let tail = ledger.tail(2);
        assert_eq!(tail.len(), 2);
        assert_eq!(tail[0].operation_id, "op-3");
        assert_eq!(tail[1].operation_id, "op-4");
        assert_eq!(ledger.tail(500).len(), 5);
    }

    #[test]
    fn malformed_rows_are_skipped_without_losing_valid_rows() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("ledger.jsonl");
        let hash = OperationLedger::request_hash("exec", &serde_json::json!({}));
        {
            let mut ledger = OperationLedger::open(&path, 8).unwrap();
            ledger.begin("op-good", "exec", &hash).unwrap();
            ledger
                .complete(LedgerEntry {
                    operation_id: "op-good".into(),
                    method: "exec".into(),
                    request_hash: hash,
                    success: true,
                    result: Some(serde_json::json!("ok")),
                    error: None,
                    started_at: vp_protocol::now_rfc3339(),
                    completed_at: vp_protocol::now_rfc3339(),
                })
                .unwrap();
        }
        let mut file = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(file, "{{not json").unwrap();
        let ledger = OperationLedger::open(&path, 8).unwrap();
        assert!(ledger.get("op-good").is_some());
        assert_eq!(ledger.stats().entries, 1);
    }

    #[test]
    fn request_hash_changes_with_method_and_payload() {
        let a = OperationLedger::request_hash("write_file", &serde_json::json!({"path": "a"}));
        let b = OperationLedger::request_hash("write_file", &serde_json::json!({"path": "b"}));
        let c = OperationLedger::request_hash("exec", &serde_json::json!({"path": "a"}));
        assert_ne!(a, b);
        assert_ne!(a, c);
        let again = OperationLedger::request_hash("write_file", &serde_json::json!({"path": "a"}));
        assert_eq!(a, again);
    }

    #[test]
    fn operation_id_validation_precedes_ledger_state() {
        let mut ledger = OperationLedger::in_memory(8);
        let hash = OperationLedger::request_hash("exec", &serde_json::json!({}));
        assert!(ledger.begin("", "exec", &hash).is_err());
        assert!(ledger.begin("bad id", "exec", &hash).is_err());
        assert!(ledger.begin(&"x".repeat(200), "exec", &hash).is_err());
    }

    #[test]
    fn stats_describe_persistence_and_bounds() {
        let dir = TempDir::new().unwrap();
        let ledger = OperationLedger::open(dir.path().join("l.jsonl"), 4).unwrap();
        let stats = ledger.stats();
        assert!(stats.persisted);
        assert_eq!(stats.max_entries, 4);
        assert_eq!(stats.entries, 0);
        let memory = OperationLedger::in_memory(0);
        assert_eq!(memory.stats().max_entries, 1);
        assert!(!memory.stats().persisted);
    }
}
