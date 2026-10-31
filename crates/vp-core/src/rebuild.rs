use std::io::BufRead;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;
use vp_protocol::types::{Event, Session};
use vp_store::Store;

use crate::Engine;

#[derive(Debug, Clone, Serialize)]
pub struct RebuildReport {
    pub dry_run: bool,
    pub projects: usize,
    pub sessions: usize,
    pub events: usize,
    pub skipped: Vec<String>,
    pub backup: Option<String>,
}

/// Rebuild the metadata database from the project manifest and per-session
/// event mirrors. Worktrees, branches, and artifact blobs are intentionally
/// left on disk; sessions can be reattached with `rehydrate`/`ensure_worktree`.
pub async fn rebuild_database(data_dir: &Path, dry_run: bool) -> Result<RebuildReport> {
    let snapshots = vp_store::read_project_manifest(data_dir)?;
    if snapshots.is_empty() {
        anyhow::bail!(
            "project manifest is empty or missing; start the daemon once to write projects.jsonl"
        );
    }
    let projects = snapshots
        .iter()
        .map(|snapshot| snapshot.project.id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mirrors = read_mirrors(data_dir)?;
    let mut prepared = Vec::new();
    let mut skipped = Vec::new();
    let mut events = 0usize;
    for (directory_id, mirror) in mirrors {
        let Some(created) = mirror.iter().find(|event| event.kind == "session.created") else {
            skipped.push(format!("{directory_id}: no session.created event"));
            continue;
        };
        let Some(session) = session_from_created(created) else {
            skipped.push(format!("{directory_id}: malformed session.created event"));
            continue;
        };
        if session.id != directory_id {
            skipped.push(format!(
                "{directory_id}: session.created id {} does not match directory",
                session.id
            ));
            continue;
        }
        if !projects.contains(&session.project_id) {
            skipped.push(format!(
                "{directory_id}: project {} is not in the manifest",
                session.project_id
            ));
            continue;
        }
        events += mirror.len();
        prepared.push((session, mirror));
    }

    let report = RebuildReport {
        dry_run,
        projects: snapshots.len(),
        sessions: prepared.len(),
        events,
        skipped: skipped.clone(),
        backup: None,
    };
    if dry_run {
        return Ok(report);
    }

    let backup = backup_database(data_dir)?;
    let store = Store::open(data_dir)?;
    for snapshot in &snapshots {
        store.insert_project_snapshot(&snapshot.project)?;
        store.set_shell_policy(&snapshot.project.id, &snapshot.shell_policy)?;
        store.set_integration_policy(&snapshot.project.id, &snapshot.integration_policy)?;
        store.set_artifact_policy(&snapshot.project.id, &snapshot.artifact_policy)?;
        store.set_mailbox_policy(&snapshot.project.id, &snapshot.mailbox_policy)?;
        store.set_worktree_policy(&snapshot.project.id, &snapshot.worktree_policy)?;
        store.set_project_budget(&snapshot.project.id, &snapshot.budget)?;
    }
    let engine = Engine::new(store);
    let mut sessions = 0usize;
    for (session, _) in prepared {
        engine.store().insert_session_snapshot(&session)?;
        match engine.restore_session_log(&session.id).await {
            Ok(_) => sessions += 1,
            Err(error) => skipped.push(format!("{}: rebuild failed: {error}", session.id)),
        }
    }
    engine.record_project_manifest().await?;
    Ok(RebuildReport {
        sessions,
        skipped,
        backup: backup.map(|path| path.to_string_lossy().to_string()),
        ..report
    })
}

fn session_from_created(event: &Event) -> Option<Session> {
    let payload = event.payload.as_object()?;
    Some(Session {
        id: event.session_id.clone(),
        project_id: payload.get("project_id")?.as_str()?.to_string(),
        goal: payload.get("goal")?.as_str()?.to_string(),
        status: "draft".to_string(),
        priority: payload
            .get("priority")
            .and_then(serde_json::Value::as_i64)
            .and_then(|value| i32::try_from(value).ok())
            .unwrap_or(0),
        worktree: payload
            .get("worktree")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        branch: payload
            .get("branch")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        token_budget: payload
            .get("token_budget")
            .and_then(serde_json::Value::as_u64),
        cost_budget_usd: payload
            .get("cost_budget_usd")
            .and_then(serde_json::Value::as_f64),
        time_budget_secs: payload
            .get("time_budget_secs")
            .and_then(serde_json::Value::as_u64),
        cost_spent_usd: 0.0,
        base_commit: payload
            .get("base_commit")
            .and_then(serde_json::Value::as_str)
            .map(str::to_string),
        tokens_in: 0,
        tokens_out: 0,
        created_at: payload
            .get("created_at")
            .and_then(serde_json::Value::as_str)
            .unwrap_or(&event.created_at)
            .to_string(),
    })
}

fn read_mirrors(data_dir: &Path) -> Result<Vec<(String, Vec<Event>)>> {
    let sessions_dir = data_dir.join("sessions");
    if !sessions_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut mirrors = Vec::new();
    for entry in std::fs::read_dir(&sessions_dir)
        .with_context(|| format!("reading {}", sessions_dir.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let id = entry.file_name().to_string_lossy().to_string();
        let path = entry.path().join("events.jsonl");
        if !path.is_file() {
            continue;
        }
        let file = std::fs::File::open(&path)
            .with_context(|| format!("opening event mirror {}", path.display()))?;
        let mut events = Vec::new();
        for line in std::io::BufReader::new(file).lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let event: Event = serde_json::from_str(&line).context("parsing event mirror line")?;
            events.push(event);
        }
        if !events.is_empty() {
            mirrors.push((id, events));
        }
    }
    Ok(mirrors)
}

fn backup_database(data_dir: &Path) -> Result<Option<PathBuf>> {
    let database = data_dir.join("verlinpark.db");
    if !database.is_file() {
        return Ok(None);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or_default();
    let backup = data_dir.join(format!("verlinpark.db.pre-rebuild-{stamp}"));
    if backup.exists() {
        return Err(anyhow!(
            "refusing to overwrite existing rebuild backup {}",
            backup.display()
        ));
    }
    std::fs::rename(&database, &backup)
        .with_context(|| format!("moving {} to {}", database.display(), backup.display()))?;
    for suffix in ["-wal", "-shm"] {
        let source = PathBuf::from(format!("{}{suffix}", database.display()));
        if source.is_file() {
            let target = PathBuf::from(format!("{}{suffix}", backup.display()));
            if let Err(error) = std::fs::rename(&source, &target) {
                tracing::warn!(%error, path = %source.display(), "failed to move SQLite sidecar");
            }
        }
    }
    Ok(Some(backup))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
    use tempfile::TempDir;

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    #[tokio::test]
    async fn rebuilds_projects_sessions_and_events_from_manifest_and_mirrors() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let (project_id, session_id) = {
            let engine = Engine::new(Store::open(data.path()).unwrap());
            let project = engine
                .add_project(
                    repo.path().to_string_lossy().to_string(),
                    Some("rebuild".into()),
                )
                .await
                .unwrap();
            let session = engine
                .create_session(project.id.clone(), "rebuild me".into(), Some(1000))
                .await
                .unwrap();
            engine
                .append_message(
                    &session.id,
                    "user",
                    "hello rebuild",
                    serde_json::json!([]),
                    None,
                )
                .await
                .unwrap();
            engine.record_project_manifest().await.unwrap();
            (project.id, session.id)
        };

        let report = rebuild_database(data.path(), false).await.unwrap();
        assert_eq!(report.projects, 1);
        assert_eq!(report.sessions, 1);
        assert!(report.events >= 2);
        assert!(report.backup.is_some());
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);

        let store = Store::open(data.path()).unwrap();
        let project = store.get_project(&project_id).unwrap().unwrap();
        assert_eq!(project.name, "rebuild");
        let session = store.get_session(&session_id).unwrap().unwrap();
        assert_eq!(session.goal, "rebuild me");
        assert_eq!(session.token_budget, Some(1000));
        assert!(
            store
                .list_messages(&session_id)
                .unwrap()
                .iter()
                .any(|message| message.content == "hello rebuild")
        );
        assert!(
            store
                .list_all_events(&session_id)
                .unwrap()
                .iter()
                .any(|event| event.kind == "session.created")
        );
    }

    #[tokio::test]
    async fn dry_run_does_not_move_the_database() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        {
            let engine = Engine::new(Store::open(data.path()).unwrap());
            engine
                .add_project(
                    repo.path().to_string_lossy().to_string(),
                    Some("dry".into()),
                )
                .await
                .unwrap();
            engine.record_project_manifest().await.unwrap();
        }
        let database = data.path().join("verlinpark.db");
        let before = std::fs::metadata(&database).unwrap().len();
        let report = rebuild_database(data.path(), true).await.unwrap();
        assert!(report.dry_run);
        assert!(report.backup.is_none());
        assert!(database.exists());
        assert_eq!(std::fs::metadata(&database).unwrap().len(), before);
    }
}
