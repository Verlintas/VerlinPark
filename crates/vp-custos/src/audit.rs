//! Append-only, hash-chained Custos audit log.
//!
//! The audit is metadata only: probes, vault state, ring lifecycle, and
//! (later) notices/actions. Callers must never put prompts, messages, tool
//! output, file contents, screen data, or secrets in a payload.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const AUDIT_SCHEMA: &str = "verlinpark.custos_audit/v1";
pub const AUDIT_FILE: &str = "events.jsonl";
const MAX_AUDIT_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PAYLOAD_BYTES: usize = 8 * 1024;
const MAX_KIND_BYTES: usize = 80;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub schema: String,
    pub seq: u64,
    pub at: String,
    pub kind: String,
    pub payload: Value,
    pub prev_hash: String,
    pub hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditReport {
    pub schema: String,
    pub state: String,
    pub path: String,
    pub entries: u64,
    pub last_hash: Option<String>,
    pub errors: Vec<String>,
}

pub fn audit_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("custos").join("audit")
}

pub fn audit_path(data_dir: &Path) -> PathBuf {
    audit_dir(data_dir).join(AUDIT_FILE)
}

/// Append one entry with an exclusive file lock. The file is 0600 and the
/// directory is 0700. A full/oversized audit fails closed instead of silently
/// dropping history.
pub fn append(data_dir: &Path, kind: &str, payload: Value) -> Result<AuditEntry> {
    if kind.is_empty()
        || kind.len() > MAX_KIND_BYTES
        || !kind.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '_' | '-')
        })
    {
        bail!("invalid audit kind");
    }
    let payload_bytes = serde_json::to_vec(&payload)?;
    if payload_bytes.len() > MAX_PAYLOAD_BYTES {
        bail!("audit payload exceeds {MAX_PAYLOAD_BYTES} bytes");
    }
    let dir = audit_dir(data_dir);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    set_mode(&dir, 0o700)?;
    let path = audit_path(data_dir);
    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    set_mode(&path, 0o600)?;
    file.lock_exclusive()?;
    let result = (|| -> Result<AuditEntry> {
        let length = file.metadata()?.len();
        if length > MAX_AUDIT_BYTES {
            bail!("audit log exceeds {MAX_AUDIT_BYTES} bytes");
        }
        let (seq, prev_hash) = read_tail(&mut file)?;
        let entry = AuditEntry {
            schema: AUDIT_SCHEMA.to_string(),
            seq: seq.saturating_add(1),
            at: vp_protocol::now_rfc3339(),
            kind: kind.to_string(),
            payload,
            prev_hash,
            hash: String::new(),
        };
        let hash = entry_hash(&entry)?;
        let entry = AuditEntry { hash, ..entry };
        let mut line = serde_json::to_vec(&entry)?;
        line.push(b'\n');
        file.seek(SeekFrom::End(0))?;
        file.write_all(&line)?;
        file.sync_data()?;
        Ok(entry)
    })();
    let _ = file.unlock();
    result
}

/// Verify the complete chain. Missing file is `missing`; malformed or broken
/// chain is `invalid`; otherwise `ok`.
pub fn verify(data_dir: &Path) -> Result<AuditReport> {
    let path = audit_path(data_dir);
    let mut report = AuditReport {
        schema: "verlinpark.custos_audit_report/v1".to_string(),
        state: "missing".to_string(),
        path: path.to_string_lossy().to_string(),
        entries: 0,
        last_hash: None,
        errors: Vec::new(),
    };
    let metadata = match fs::metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    if metadata.len() > MAX_AUDIT_BYTES {
        report.state = "invalid".to_string();
        report.errors.push("audit log exceeds size cap".to_string());
        return Ok(report);
    }
    let text = fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut expected_seq = 1u64;
    let mut prev = "genesis".to_string();
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: AuditEntry = match serde_json::from_str(line) {
            Ok(entry) => entry,
            Err(error) => {
                report.state = "invalid".to_string();
                report
                    .errors
                    .push(format!("line {} invalid: {error}", index + 1));
                return Ok(report);
            }
        };
        if entry.schema != AUDIT_SCHEMA {
            report.state = "invalid".to_string();
            report
                .errors
                .push(format!("line {} schema mismatch", index + 1));
            return Ok(report);
        }
        if entry.seq != expected_seq {
            report.state = "invalid".to_string();
            report.errors.push(format!(
                "line {} sequence {} != {}",
                index + 1,
                entry.seq,
                expected_seq
            ));
            return Ok(report);
        }
        if entry.prev_hash != prev {
            report.state = "invalid".to_string();
            report
                .errors
                .push(format!("line {} previous hash mismatch", index + 1));
            return Ok(report);
        }
        if entry_hash(&entry)? != entry.hash {
            report.state = "invalid".to_string();
            report
                .errors
                .push(format!("line {} hash mismatch", index + 1));
            return Ok(report);
        }
        prev = entry.hash.clone();
        expected_seq = expected_seq.saturating_add(1);
        report.entries = report.entries.saturating_add(1);
        report.last_hash = Some(entry.hash);
    }
    report.state = if report.entries == 0 {
        "missing".to_string()
    } else {
        "ok".to_string()
    };
    Ok(report)
}

fn read_tail(file: &mut File) -> Result<(u64, String)> {
    file.seek(SeekFrom::Start(0))?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    for line in text.lines().rev() {
        if line.trim().is_empty() {
            continue;
        }
        let entry: AuditEntry = serde_json::from_str(line).context("audit tail is malformed")?;
        return Ok((entry.seq, entry.hash));
    }
    Ok((0, "genesis".to_string()))
}

fn entry_hash(entry: &AuditEntry) -> Result<String> {
    let body = serde_json::json!({
        "schema": entry.schema,
        "seq": entry.seq,
        "at": entry.at,
        "kind": entry.kind,
        "payload": entry.payload,
        "prev_hash": entry.prev_hash,
    });
    let digest = Sha256::digest(serde_json::to_vec(&body)?);
    Ok(format!("sha256:{}", hex(&digest)))
}

fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn append_and_verify_chain() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(verify(dir.path()).unwrap().state, "missing");
        append(
            dir.path(),
            "custos.start",
            serde_json::json!({"role":"core"}),
        )
        .unwrap();
        append(dir.path(), "vault.check", serde_json::json!({"state":"ok"})).unwrap();
        let report = verify(dir.path()).unwrap();
        assert_eq!(report.state, "ok");
        assert_eq!(report.entries, 2);
        assert!(report.last_hash.unwrap().starts_with("sha256:"));
    }

    #[test]
    fn tampering_breaks_the_chain() {
        let dir = tempfile::TempDir::new().unwrap();
        append(dir.path(), "one", serde_json::json!({})).unwrap();
        append(dir.path(), "two", serde_json::json!({})).unwrap();
        let path = audit_path(dir.path());
        let text = fs::read_to_string(&path).unwrap();
        let tampered = text.replacen("\"kind\":\"one\"", "\"kind\":\"bad\"", 1);
        fs::write(&path, tampered).unwrap();
        let report = verify(dir.path()).unwrap();
        assert_eq!(report.state, "invalid");
    }

    #[test]
    fn oversized_payload_is_rejected() {
        let dir = tempfile::TempDir::new().unwrap();
        let payload = serde_json::json!({"blob": "x".repeat(MAX_PAYLOAD_BYTES + 1)});
        assert!(append(dir.path(), "too.big", payload).is_err());
    }
}
