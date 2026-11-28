//! Root-owned hash-chained audit log.
//!
//! Each JSONL entry contains its sequence number, the previous entry hash, and
//! a SHA-256 over the domain-separated canonical entry. Verification walks the
//! whole chain, so a modified field, sequence, or previous hash fails closed.
//! Appends take an advisory lock on `audit/audit.lock` and are fsynced; this is
//! same-host, single-executor coordination, not a distributed lock.
//!
//! The chain is not externally anchored in this slice. A root/admin attacker can
//! rewrite the whole file; ADR-0247 treats root as outside the guarantee. The
//! local monotonic epoch and future portable anchor are separate work.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead as _, BufReader, Write as _};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::canonical::{canonical_json, is_lower_hex, sha256_hex};
use crate::intent::Role;
use crate::layout::{
    FILE_PRIVATE_MODE, LayoutError, RootLayout, validate_existing_dir, validate_existing_file,
};

/// Audit schema version.
pub const AUDIT_SCHEMA: &str = "vp-custos-root-audit-v1";
/// Domain separator for audit entry hashes.
pub const AUDIT_DOMAIN: &[u8] = b"VP-CUSTOS-ROOT-AUDIT-V1\0";
/// Genesis previous hash.
pub const GENESIS_HASH: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// Bound on one JSONL record.
pub const MAX_AUDIT_LINE_BYTES: usize = 64 * 1024;
/// Bound on chain length for one verification pass.
pub const MAX_AUDIT_ENTRIES: u64 = 1_000_000;

/// One audited event.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditEntry {
    pub schema: String,
    pub seq: u64,
    pub prev_hash: String,
    pub recorded_at_unix: i64,
    pub kind: String,
    pub action_id: String,
    pub intent_id: String,
    pub epoch: u64,
    pub target_hash: String,
    pub signer_roles: Vec<Role>,
    pub receipt_file: String,
    pub receipt_sha256: String,
}

/// The persisted envelope: entry plus its canonical hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuditRecord {
    pub entry: AuditEntry,
    pub hash: String,
}

/// Result of verifying a chain.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditChain {
    pub entries: u64,
    pub head_hash: String,
}

/// Input for a new audit entry; sequence and previous hash are filled by
/// [`append_audit`].
#[derive(Clone, Debug)]
pub struct AuditEvent {
    pub recorded_at_unix: i64,
    pub kind: String,
    pub action_id: String,
    pub intent_id: String,
    pub epoch: u64,
    pub target_hash: String,
    pub signer_roles: Vec<Role>,
    pub receipt_file: String,
    pub receipt_sha256: String,
}

/// Append one entry after validating the existing chain.
pub fn append_audit(
    layout: &RootLayout,
    expected_uid: u32,
    event: AuditEvent,
) -> Result<AuditRecord, AuditError> {
    validate_existing_dir(
        &layout.audit_dir(),
        expected_uid,
        crate::layout::DIR_PRIVATE_MODE,
    )?;
    let lock = open_audit_lock(layout, expected_uid)?;
    fs2::FileExt::lock_exclusive(&lock).map_err(|source| AuditError::Io {
        path: layout.audit_lock(),
        source,
    })?;

    let result = append_locked(layout, expected_uid, event);
    let _ = fs2::FileExt::unlock(&lock);
    result
}

fn open_audit_lock(layout: &RootLayout, expected_uid: u32) -> Result<File, AuditError> {
    let path = layout.audit_lock();
    let create = OpenOptions::new()
        .read(true)
        .write(true)
        .create_new(true)
        .mode(FILE_PRIVATE_MODE)
        .open(&path);
    let file = match create {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            validate_existing_file(&path, expected_uid, FILE_PRIVATE_MODE)?;
            OpenOptions::new()
                .read(true)
                .write(true)
                .open(&path)
                .map_err(|source| AuditError::Io {
                    path: path.clone(),
                    source,
                })?
        }
        Err(source) => {
            return Err(AuditError::Io {
                path: path.clone(),
                source,
            });
        }
    };
    file.set_permissions(fs::Permissions::from_mode(FILE_PRIVATE_MODE))
        .map_err(|source| AuditError::Io { path, source })?;
    Ok(file)
}

fn append_locked(
    layout: &RootLayout,
    expected_uid: u32,
    event: AuditEvent,
) -> Result<AuditRecord, AuditError> {
    let chain = scan_chain(layout, expected_uid)?;
    if chain.entries >= MAX_AUDIT_ENTRIES {
        return Err(AuditError::TooManyEntries);
    }
    let entry = AuditEntry {
        schema: AUDIT_SCHEMA.to_string(),
        seq: chain.entries + 1,
        prev_hash: if chain.entries == 0 {
            GENESIS_HASH.to_string()
        } else {
            chain.head_hash
        },
        recorded_at_unix: event.recorded_at_unix,
        kind: event.kind,
        action_id: event.action_id,
        intent_id: event.intent_id,
        epoch: event.epoch,
        target_hash: event.target_hash,
        signer_roles: event.signer_roles,
        receipt_file: event.receipt_file,
        receipt_sha256: event.receipt_sha256,
    };
    let hash = audit_hash(&entry)?;
    let record = AuditRecord { entry, hash };
    let mut line = serde_json::to_vec(&record).map_err(AuditError::Json)?;
    line.push(b'\n');
    let path = layout.audit_log();
    let mut file = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(FILE_PRIVATE_MODE)
        .open(&path)
        .map_err(|source| AuditError::Io {
            path: path.clone(),
            source,
        })?;
    file.set_permissions(fs::Permissions::from_mode(FILE_PRIVATE_MODE))
        .map_err(|source| AuditError::Io {
            path: path.clone(),
            source,
        })?;
    file.write_all(&line)
        .and_then(|()| file.sync_all())
        .map_err(|source| AuditError::Io { path, source })?;
    Ok(record)
}

/// Verify the complete audit chain. A missing log is a valid empty chain.
pub fn verify_audit_chain(
    layout: &RootLayout,
    expected_uid: u32,
) -> Result<AuditChain, AuditError> {
    if let Err(error) = fs::symlink_metadata(layout.audit_dir()) {
        return if error.kind() == io::ErrorKind::NotFound {
            Ok(empty_chain())
        } else {
            Err(AuditError::Io {
                path: layout.audit_dir(),
                source: error,
            })
        };
    }
    validate_existing_dir(
        &layout.audit_dir(),
        expected_uid,
        crate::layout::DIR_PRIVATE_MODE,
    )?;
    let lock = existing_lock(layout, expected_uid)?;
    if let Some(lock) = &lock {
        fs2::FileExt::lock_shared(lock).map_err(|source| AuditError::Io {
            path: layout.audit_lock(),
            source,
        })?;
    }
    let result = scan_chain(layout, expected_uid);
    if let Some(lock) = &lock {
        let _ = fs2::FileExt::unlock(lock);
    }
    result
}

fn existing_lock(layout: &RootLayout, expected_uid: u32) -> Result<Option<File>, AuditError> {
    let path = layout.audit_lock();
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            validate_existing_file(&path, expected_uid, FILE_PRIVATE_MODE)?;
            let file = File::open(&path).map_err(|source| AuditError::Io {
                path: path.clone(),
                source,
            })?;
            Ok(Some(file))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(AuditError::Io { path, source }),
    }
}

fn scan_chain(layout: &RootLayout, expected_uid: u32) -> Result<AuditChain, AuditError> {
    let path = layout.audit_log();
    match fs::symlink_metadata(&path) {
        Ok(_) => {
            validate_existing_file(&path, expected_uid, FILE_PRIVATE_MODE)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(empty_chain()),
        Err(source) => return Err(AuditError::Io { path, source }),
    }
    let file = File::open(&path).map_err(|source| AuditError::Io {
        path: path.clone(),
        source,
    })?;

    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut expected_seq = 1u64;
    let mut previous = GENESIS_HASH.to_string();
    let mut line_number = 0u64;
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|source| AuditError::Io {
                path: path.clone(),
                source,
            })?;
        if read == 0 {
            break;
        }
        line_number += 1;
        if line.len() > MAX_AUDIT_LINE_BYTES {
            return Err(AuditError::LineTooLong { line: line_number });
        }
        if line_number > MAX_AUDIT_ENTRIES {
            return Err(AuditError::TooManyEntries);
        }
        let record: AuditRecord =
            serde_json::from_slice(&line).map_err(|source| AuditError::JsonAt {
                line: line_number,
                source,
            })?;
        if record.entry.schema != AUDIT_SCHEMA {
            return Err(AuditError::InvalidSchema {
                line: line_number,
                schema: record.entry.schema,
            });
        }
        if record.entry.seq != expected_seq {
            return Err(AuditError::Sequence {
                line: line_number,
                expected: expected_seq,
                actual: record.entry.seq,
            });
        }
        if record.entry.prev_hash != previous {
            return Err(AuditError::PreviousHash {
                line: line_number,
                expected: previous,
                actual: record.entry.prev_hash,
            });
        }
        let recomputed = audit_hash(&record.entry)?;
        if record.hash != recomputed {
            return Err(AuditError::HashMismatch {
                line: line_number,
                stored: record.hash,
                computed: recomputed,
            });
        }
        previous = record.hash;
        expected_seq += 1;
    }
    Ok(AuditChain {
        entries: expected_seq - 1,
        head_hash: previous,
    })
}

fn empty_chain() -> AuditChain {
    AuditChain {
        entries: 0,
        head_hash: GENESIS_HASH.to_string(),
    }
}

/// Domain-separated canonical hash of an audit entry.
pub fn audit_hash(entry: &AuditEntry) -> Result<String, AuditError> {
    let body = canonical_json(entry)?;
    let mut message = Vec::with_capacity(AUDIT_DOMAIN.len() + body.len());
    message.extend_from_slice(AUDIT_DOMAIN);
    message.extend_from_slice(&body);
    Ok(sha256_hex(&message))
}

/// True when `hash` is a well-formed audit hash.
pub fn valid_audit_hash(hash: &str) -> bool {
    is_lower_hex(hash, 64, 64)
}

/// Typed audit failures.
#[derive(Debug, Error)]
pub enum AuditError {
    #[error(transparent)]
    Layout(#[from] LayoutError),
    #[error("canonical audit hash error: {0}")]
    Canonical(#[from] crate::canonical::CanonicalError),
    #[error("audit JSON error: {0}")]
    Json(#[source] serde_json::Error),
    #[error("audit JSON error on line {line}: {source}")]
    JsonAt {
        line: u64,
        #[source]
        source: serde_json::Error,
    },
    #[error("audit I/O failed: {path}: {source}")]
    Io {
        path: std::path::PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("audit line {line} exceeds {MAX_AUDIT_LINE_BYTES} bytes")]
    LineTooLong { line: u64 },
    #[error("audit chain exceeds {MAX_AUDIT_ENTRIES} entries")]
    TooManyEntries,
    #[error("audit line {line} has schema {schema:?}")]
    InvalidSchema { line: u64, schema: String },
    #[error("audit sequence mismatch on line {line}: expected {expected}, got {actual}")]
    Sequence {
        line: u64,
        expected: u64,
        actual: u64,
    },
    #[error("audit previous-hash mismatch on line {line}: expected {expected}, got {actual}")]
    PreviousHash {
        line: u64,
        expected: String,
        actual: String,
    },
    #[error("audit hash mismatch on line {line}: stored {stored}, computed {computed}")]
    HashMismatch {
        line: u64,
        stored: String,
        computed: String,
    },
}
