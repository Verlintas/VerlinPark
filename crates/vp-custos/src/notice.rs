//! Local Custos notice outbox.
//!
//! Custos writes sealed, one-way notices here; the daemon drains them at a safe
//! agent turn boundary. The outbox is metadata only and never contains prompts,
//! messages, tool output, file contents, screen data, paths, or secrets.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use vp_protocol::custos_notice::{CUSTOS_NOTICE_SCHEMA, CustosNotice};

const MAX_NOTICE_BYTES: usize = 32 * 1024;

pub fn notices_root(data_dir: &Path) -> PathBuf {
    data_dir.join("custos").join("notices")
}

pub fn outbox_dir(data_dir: &Path) -> PathBuf {
    notices_root(data_dir).join("outbox")
}

pub fn delivered_dir(data_dir: &Path) -> PathBuf {
    notices_root(data_dir).join("delivered")
}

pub fn new_notice_id() -> String {
    format!("cn_{}", uuid::Uuid::new_v4().simple())
}

/// Write one notice atomically into the outbox. The caller must have already
/// validated the recipient and built the notice from deterministic evidence.
pub fn write_notice(data_dir: &Path, notice: &CustosNotice) -> Result<PathBuf> {
    notice
        .validate_shape()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let bytes = serde_json::to_vec_pretty(notice)?;
    if bytes.len() > MAX_NOTICE_BYTES {
        bail!("notice exceeds {MAX_NOTICE_BYTES} bytes");
    }
    let dir = outbox_dir(data_dir);
    fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    set_mode(&dir, 0o700)?;
    let stamp = notice
        .created_at
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(20)
        .collect::<String>();
    let name = format!("{stamp}_{}.json", safe_component(&notice.notice_id)?);
    let path = dir.join(name);
    let tmp = dir.join(".notice.tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        set_mode(&tmp, 0o600)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, &path).with_context(|| format!("renaming {}", path.display()))?;
    Ok(path)
}

/// Read one outbox file for tests/diagnostics. Delivery is implemented by the
/// daemon, not by Custos.
pub fn read_notice(path: &Path) -> Result<CustosNotice> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let notice: CustosNotice = serde_json::from_slice(&bytes)?;
    if notice.schema != CUSTOS_NOTICE_SCHEMA {
        bail!("unexpected notice schema");
    }
    notice
        .validate_shape()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(notice)
}

fn safe_component(value: &str) -> Result<String> {
    if value.is_empty()
        || value.len() > 80
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        bail!("notice id is not a safe filename component");
    }
    Ok(value.to_string())
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
    use vp_protocol::custos_notice::CustosNoticeObserved;

    fn notice() -> CustosNotice {
        CustosNotice {
            schema: CUSTOS_NOTICE_SCHEMA.to_string(),
            notice_id: new_notice_id(),
            created_at: vp_protocol::now_rfc3339(),
            severity: "warning".to_string(),
            session_id: "session-1".to_string(),
            agent_id: None,
            task_id: None,
            subject: "worker-1".to_string(),
            action: "final_notice".to_string(),
            observed: CustosNoticeObserved {
                pid: 123,
                cpu_share: 0.8,
                rss_bytes: 1024 * 1024 * 1024,
                children: 2,
                no_progress_secs: 620,
                score: 94,
                state: "suspected_stall".to_string(),
            },
            requested_action: "stop or reduce".to_string(),
            grace_secs: 120,
            consequence: "none in shadow mode".to_string(),
            reply: "forbidden".to_string(),
            charter_hash: "sha256:charter".to_string(),
            build_hash: "sha256:dev".to_string(),
        }
    }

    #[test]
    fn writes_and_reads_private_notice() {
        let dir = tempfile::TempDir::new().unwrap();
        let written = write_notice(dir.path(), &notice()).unwrap();
        assert!(written.is_file());
        let parsed = read_notice(&written).unwrap();
        assert_eq!(parsed.session_id, "session-1");
        assert_eq!(parsed.reply, "forbidden");
    }

    #[test]
    fn rejects_unsafe_notice_ids() {
        let dir = tempfile::TempDir::new().unwrap();
        let mut value = notice();
        value.notice_id = "../escape".to_string();
        assert!(write_notice(dir.path(), &value).is_err());
    }
}
