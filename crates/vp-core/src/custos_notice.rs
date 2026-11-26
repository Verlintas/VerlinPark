//! Delivery of sealed one-way Custos notices at agent turn boundaries.
//!
//! Custos writes JSON files into `<data-dir>/custos/notices/outbox`. The agent
//! loop drains at most two matching notices before a provider call, appends
//! them as persisted `system` messages, and moves the files to `delivered`.
//! There is no reply path and no tool surface.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use vp_protocol::custos_notice::{CUSTOS_NOTICE_SCHEMA, CustosNotice};

const MAX_DRAIN: usize = 2;

pub fn notices_root(data_dir: &Path) -> PathBuf {
    data_dir.join("custos").join("notices")
}

pub fn outbox_dir(data_dir: &Path) -> PathBuf {
    notices_root(data_dir).join("outbox")
}

pub fn delivered_dir(data_dir: &Path) -> PathBuf {
    notices_root(data_dir).join("delivered")
}

/// Drain matching notices for one agent. A notice with no `agent_id` is
/// intentionally leader-only until process ownership is persisted per agent.
pub fn drain_for_agent(
    data_dir: &Path,
    session_id: &str,
    agent_id: Option<&str>,
    is_leader: bool,
    max: usize,
) -> Result<Vec<CustosNotice>> {
    let outbox = outbox_dir(data_dir);
    let mut paths = match fs::read_dir(&outbox) {
        Ok(entries) => entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "json")
            })
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", outbox.display())),
    };
    paths.sort();
    let delivered = delivered_dir(data_dir);
    let mut notices = Vec::new();
    for path in paths {
        if notices.len() >= max.min(MAX_DRAIN) {
            break;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(_) => continue,
        };
        let notice: CustosNotice = match serde_json::from_slice(&bytes) {
            Ok(notice) => notice,
            Err(_) => continue,
        };
        if notice.schema != CUSTOS_NOTICE_SCHEMA
            || notice.validate_shape().is_err()
            || notice.session_id != session_id
        {
            continue;
        }
        let matches_agent = match notice.agent_id.as_deref() {
            Some(target) => agent_id == Some(target),
            None => is_leader,
        };
        if !matches_agent {
            continue;
        }
        fs::create_dir_all(&delivered)?;
        set_mode(&delivered, 0o700)?;
        let name = path
            .file_name()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(format!("{}.json", notice.notice_id)));
        let target = delivered.join(name);
        if fs::rename(&path, &target).is_err() {
            // Another agent won the race, or the outbox changed. Try the next
            // file rather than duplicating a delivered notice.
            continue;
        }
        notices.push(notice);
    }
    Ok(notices)
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

    fn write_notice(dir: &Path, session_id: &str, agent_id: Option<&str>) {
        let outbox = outbox_dir(dir);
        fs::create_dir_all(&outbox).unwrap();
        let notice = CustosNotice {
            schema: CUSTOS_NOTICE_SCHEMA.to_string(),
            notice_id: format!("cn_{}", uuid::Uuid::new_v4().simple()),
            created_at: vp_protocol::now_rfc3339(),
            severity: "warning".to_string(),
            session_id: session_id.to_string(),
            agent_id: agent_id.map(str::to_string),
            task_id: None,
            subject: "worker-1".to_string(),
            action: "final_notice".to_string(),
            observed: CustosNoticeObserved {
                pid: 123,
                cpu_share: 0.8,
                rss_bytes: 1024,
                children: 1,
                no_progress_secs: 600,
                score: 90,
                state: "suspected_stall".to_string(),
            },
            requested_action: "stop or reduce".to_string(),
            grace_secs: 120,
            consequence: "none".to_string(),
            reply: "forbidden".to_string(),
            charter_hash: "sha256:charter".to_string(),
            build_hash: "sha256:dev".to_string(),
        };
        fs::write(
            outbox.join(format!("{}.json", notice.notice_id)),
            serde_json::to_vec(&notice).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn leader_drains_session_notice_once() {
        let dir = tempfile::TempDir::new().unwrap();
        write_notice(dir.path(), "session-1", None);
        let first = drain_for_agent(dir.path(), "session-1", Some("leader"), true, 2).unwrap();
        assert_eq!(first.len(), 1);
        let second = drain_for_agent(dir.path(), "session-1", Some("leader"), true, 2).unwrap();
        assert!(second.is_empty());
        assert!(delivered_dir(dir.path()).is_dir());
    }

    #[test]
    fn worker_does_not_consume_an_unaddressed_leader_notice() {
        let dir = tempfile::TempDir::new().unwrap();
        write_notice(dir.path(), "session-1", None);
        let drained = drain_for_agent(dir.path(), "session-1", Some("worker"), false, 2).unwrap();
        assert!(drained.is_empty());
        assert!(!outbox_dir(dir.path()).join("unused").exists());
    }

    #[test]
    fn exact_agent_notice_is_delivered_to_that_agent() {
        let dir = tempfile::TempDir::new().unwrap();
        write_notice(dir.path(), "session-1", Some("worker-7"));
        let drained = drain_for_agent(dir.path(), "session-1", Some("worker-7"), false, 2).unwrap();
        assert_eq!(drained.len(), 1);
    }
}
