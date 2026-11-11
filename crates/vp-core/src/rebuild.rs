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
    let result: Result<(usize, Vec<String>)> = async {
        let store = Store::open(data_dir)?;
        for snapshot in &snapshots {
            store.insert_project_snapshot(&snapshot.project)?;
            store.set_shell_policy(&snapshot.project.id, &snapshot.shell_policy)?;
            store.set_cache_policy(&snapshot.project.id, &snapshot.cache_policy)?;
            store.set_execution_host(&snapshot.project.id, &snapshot.execution_host)?;
            store.set_integration_policy(&snapshot.project.id, &snapshot.integration_policy)?;
            store.set_gui_policy(&snapshot.project.id, &snapshot.gui_policy)?;
            store.set_artifact_policy(&snapshot.project.id, &snapshot.artifact_policy)?;
            crate::tool_output::validate_tool_output_policy(&snapshot.tool_output_policy)
                .with_context(|| {
                    format!(
                        "tool-output policy for project {} is invalid",
                        snapshot.project.id
                    )
                })?;
            store.set_tool_output_policy(&snapshot.project.id, &snapshot.tool_output_policy)?;
            store.set_mailbox_policy(&snapshot.project.id, &snapshot.mailbox_policy)?;
            store.set_worktree_policy(&snapshot.project.id, &snapshot.worktree_policy)?;
            store.set_checkpoint_policy(&snapshot.project.id, &snapshot.checkpoint_policy)?;
            store.set_context_checkpoint_policy(
                &snapshot.project.id,
                &snapshot.context_checkpoint_policy,
            )?;
            store.set_process_policy(&snapshot.project.id, &snapshot.process_policy)?;
            store.set_project_budget(&snapshot.project.id, &snapshot.budget)?;
            store
                .set_task_scheduler_policy(&snapshot.project.id, &snapshot.task_scheduler_policy)?;
            store.set_scaling_policy(&snapshot.project.id, &snapshot.scaling_policy)?;
            store.set_session_run_policy(&snapshot.project.id, &snapshot.session_run_policy)?;
            store.set_memory_policy(&snapshot.project.id, &snapshot.memory_policy)?;
            crate::source_embedding::validate_source_embedding_policy(
                &snapshot.source_embedding_policy,
            )
            .with_context(|| {
                format!(
                    "source embedding policy for project {} is invalid",
                    snapshot.project.id
                )
            })?;
            store.set_source_embedding_policy(
                &snapshot.project.id,
                &snapshot.source_embedding_policy,
            )?;
            store.set_orchestration_policy(&snapshot.project.id, &snapshot.orchestration_policy)?;
            store.set_critical_path_policy(&snapshot.project.id, &snapshot.critical_path_policy)?;
            store.restore_approval_grants(&snapshot.project.id, &snapshot.approval_grants)?;
            for skill in &snapshot.skills {
                store.insert_skill(skill)?;
            }
            for task in &snapshot.intake_tasks {
                let mut task = task.clone();
                if matches!(task.status.as_str(), "dispatching" | "running") {
                    task.status = "failed".to_string();
                    task.last_error = Some(
                        "rebuilt from the project manifest after a daemon restart".to_string(),
                    );
                    task.next_attempt_at = None;
                }
                store.insert_intake_task_snapshot(&task)?;
            }
            for program in &snapshot.programs {
                store.insert_program_snapshot(program)?;
            }
            for milestone in &snapshot.program_milestones {
                store.insert_program_milestone_snapshot(milestone)?;
            }
            for event in &snapshot.program_events {
                store.insert_program_event_snapshot(event)?;
            }
            for program in &snapshot.programs {
                store.rebuild_program_session_links(&program.id)?;
            }
        }
        let engine = Engine::new(store);
        match engine.restore_sources_from_disk().await {
            Ok(restored) if restored > 0 => {
                tracing::info!(restored, "restored project knowledge sources from disk");
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "failed to restore project knowledge sources");
            }
        }
        let mut sessions = 0usize;
        let mut skipped = skipped.clone();
        for (session, _) in prepared {
            engine.store().insert_session_snapshot(&session)?;
            match engine.restore_session_log(&session.id).await {
                Ok(_) => sessions += 1,
                Err(error) => skipped.push(format!("{}: rebuild failed: {error}", session.id)),
            }
        }
        engine.record_project_manifest().await?;
        Ok((sessions, skipped))
    }
    .await;
    let (sessions, skipped) = match result {
        Ok(result) => result,
        Err(error) => {
            if let Some(backup) = &backup
                && let Err(restore) = restore_backup(data_dir, backup)
            {
                tracing::error!(%restore, "failed to restore the pre-rebuild database");
            }
            return Err(error.context("database rebuild failed; restored the previous database"));
        }
    };
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
        unlimited: payload
            .get("unlimited")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
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
                // A database without its WAL/SHM sidecars is not a complete
                // backup. Roll back whatever was already moved.
                let _ = restore_backup(data_dir, &backup);
                return Err(error)
                    .with_context(|| format!("moving SQLite sidecar {}", source.display()));
            }
        }
    }
    Ok(Some(backup))
}

fn restore_backup(data_dir: &Path, backup: &Path) -> Result<()> {
    let database = data_dir.join("verlinpark.db");
    for suffix in ["", "-wal", "-shm"] {
        let live = PathBuf::from(format!("{}{suffix}", database.display()));
        if live.exists() {
            std::fs::remove_file(&live).with_context(|| format!("removing {}", live.display()))?;
        }
        let saved = PathBuf::from(format!("{}{suffix}", backup.display()));
        if saved.exists() {
            std::fs::rename(&saved, &live)
                .with_context(|| format!("restoring {}", live.display()))?;
        }
    }
    Ok(())
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
            let plan: vp_protocol::types::TeamPlan = serde_json::from_value(serde_json::json!({
                "goal": "rebuild me",
                "departments": [{
                    "name": "impl",
                    "manager_title": "Implementation",
                    "profile": "coding",
                    "objective": "build"
                }],
                "tasks": [{
                    "id": "T1",
                    "department": "impl",
                    "title": "Rebuild task",
                    "kind": "coding",
                    "deps": [],
                    "acceptance": ["done"]
                }]
            }))
            .unwrap();
            engine.set_team_plan(&session.id, &plan).await.unwrap();
            engine
                .set_task_scheduler_policy(
                    &project.id,
                    vp_protocol::types::TaskSchedulerPolicy {
                        enabled: true,
                        cooldown_secs: 30,
                        dispatch_tasks: false,
                        include_intake: true,
                        max_intake_parallel: 2,
                    },
                )
                .await
                .unwrap();
            engine
                .create_intake_task(vp_protocol::types::IntakeCreateParams {
                    project_id: project.id.clone(),
                    title: "Rebuild intake".into(),
                    instructions: "Survive a manifest rebuild.".into(),
                    profile: Some("coding".into()),
                    kind: Some("coding".into()),
                    priority: Some(7),
                    write_scope: vec!["src/rebuild".into()],
                    acceptance: vec!["restored".into()],
                    deps: Vec::new(),
                    token_budget: None,
                    cost_budget_usd: None,
                    time_budget_secs: None,
                    max_attempts: Some(2),
                    labels: vec!["rebuild".into()],
                    origin: Some("test".into()),
                })
                .await
                .unwrap();
            engine
                .add_source_note(vp_protocol::types::SourceCreateNoteParams {
                    project_id: project.id.clone(),
                    collection: Some("rebuild".into()),
                    title: "Rebuild source".into(),
                    content: "Knowledge survives a database rebuild.".into(),
                    labels: vec!["rebuild".into()],
                })
                .await
                .unwrap();
            engine
                .create_skill(vp_protocol::types::SkillCreateParams {
                    project_id: project.id.clone(),
                    name: "Rebuild skill".into(),
                    description: "Survive a manifest rebuild.".into(),
                    when_to_use: "When rebuilding.".into(),
                    version: Some("1.1.0".into()),
                    instructions: "Follow the rebuilt checklist.".into(),
                    tool_allow: vec!["read_file".into()],
                    tool_deny: Vec::new(),
                    profiles: vec!["coding".into()],
                    roles: vec!["leader".into()],
                })
                .await
                .unwrap();
            engine
                .append_event(
                    &session.id,
                    "plan_task.started",
                    serde_json::json!({"task_id": "T1", "task_status": "running"}),
                )
                .await
                .unwrap();
            engine
                .append_event(
                    &session.id,
                    "plan_task.finished",
                    serde_json::json!({"task_id": "T1", "task_status": "completed"}),
                )
                .await
                .unwrap();
            engine
                .record_agent_usage(
                    &session.id,
                    "worker-rebuild",
                    "Worker",
                    "worker",
                    Some("T1".into()),
                    Some("impl".into()),
                    "test",
                    100,
                    50,
                    false,
                    0.75,
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
        assert!((session.cost_spent_usd - 0.75).abs() < 1e-9);
        assert_eq!(session.tokens_in, 100);
        assert_eq!(session.tokens_out, 50);
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
        let tasks = store.list_plan_tasks(None, Some(&session_id)).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].task_id, "T1");
        assert_eq!(tasks[0].status, "completed");
        assert_eq!(tasks[0].attempts, 1);
        let policy = store.get_task_scheduler_policy(&project_id).unwrap();
        assert!(policy.enabled);
        assert_eq!(policy.cooldown_secs, 30);
        let skills = store.list_skills(Some(&project_id), true).unwrap();
        assert_eq!(skills.len(), 1);
        assert_eq!(skills[0].name, "Rebuild skill");
        assert_eq!(skills[0].version, "1.1.0");
        assert_eq!(skills[0].tool_allow, vec!["read_file"]);
        let sources = store.list_sources(&project_id, None, None, None).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].title, "Rebuild source");
        assert!(sources[0].chunk_count > 0);
        let source_page = store.source_chunks(&sources[0].id, 0, 10).unwrap();
        assert!(!source_page.is_empty());
        let intake = store
            .list_intake_tasks(Some(&project_id), None, None)
            .unwrap();
        assert_eq!(intake.len(), 1);
        assert_eq!(intake[0].title, "Rebuild intake");
        assert_eq!(intake[0].priority, 7);
        assert_eq!(intake[0].labels, vec!["rebuild"]);
        assert_eq!(intake[0].status, "queued");
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

#[cfg(test)]
mod program_rebuild_tests {
    use super::*;
    use crate::Engine;
    use crate::program::ProgramController;
    use std::path::Path;
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
    async fn rebuilds_program_ledger_and_events_from_the_project_manifest() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let (project_id, program_id) = {
            let engine = Engine::new(Store::open(data.path()).unwrap());
            let project = engine
                .add_project(
                    repo.path().to_string_lossy().to_string(),
                    Some("program-rebuild".into()),
                )
                .await
                .unwrap();
            let program = engine
                .create_program(vp_protocol::types::ProgramCreateParams {
                    project_id: project.id.clone(),
                    goal: "Rebuild the program ledger".into(),
                    policy: Some(vp_protocol::types::ProgramPolicy {
                        auto_dispatch: false,
                        max_attempts: 3,
                        retry_backoff_secs: 30,
                        dependency_failure: vp_protocol::types::DependencyFailurePolicy::Block,
                        token_budget: Some(50_000),
                        cost_budget_usd: Some(5.0),
                        max_parallel_milestones: 2,
                    }),
                    token_budget: Some(100_000),
                    cost_budget_usd: Some(10.0),
                    activate: true,
                })
                .await
                .unwrap();
            engine
                .create_program_milestone(vp_protocol::types::ProgramMilestoneCreateParams {
                    program_id: program.id.clone(),
                    key: Some("1".into()),
                    title: "Survives rebuild".into(),
                    objective: "Keep the ledger event-backed.".into(),
                    acceptance: vec!["row and event exist".into()],
                    deps: Vec::new(),
                    priority: Some(4),
                    max_attempts: Some(3),
                    profile: Some("coding".into()),
                    write_scope: vec!["src".into()],
                    token_budget: Some(10_000),
                    cost_budget_usd: None,
                })
                .await
                .unwrap();
            engine.record_project_manifest().await.unwrap();
            (project.id, program.id)
        };

        let report = rebuild_database(data.path(), false).await.unwrap();
        assert_eq!(report.projects, 1);
        assert!(report.skipped.is_empty(), "{:?}", report.skipped);
        let store = Store::open(data.path()).unwrap();
        let program = store.get_program(&program_id).unwrap().unwrap();
        assert_eq!(program.project_id, project_id);
        assert_eq!(program.goal, "Rebuild the program ledger");
        assert_eq!(program.token_budget, Some(100_000));
        assert_eq!(program.status, vp_protocol::types::ProgramStatus::Active);
        assert!(!program.archived);
        let milestones = store.list_program_milestones(&program_id).unwrap();
        assert_eq!(milestones.len(), 1);
        assert_eq!(milestones[0].title, "Survives rebuild");
        assert_eq!(milestones[0].max_attempts, 3);
        let events = store.list_program_events(&program_id, Some(100)).unwrap();
        assert!(events.iter().any(|event| event.kind == "program.created"));
        assert!(
            events
                .iter()
                .any(|event| event.kind == "program.milestone.created")
        );
        let controller = ProgramController::new(store);
        let replay = controller.replay(&program_id, false).unwrap();
        assert!(replay.consistent, "{:?}", replay.mismatches);
    }
}

#[cfg(test)]
mod run_journal_rebuild_tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_protocol::types::{RunBudgetSnapshot, SessionRunPolicy};
    use vp_store::Store;

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(path)
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        }
    }

    #[tokio::test]
    async fn rebuild_restores_run_journal_policy_and_projections() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let (project_id, session_id, run_id) = {
            let engine = Engine::new(Store::open(data.path()).unwrap());
            let project = engine
                .add_project(
                    repo.path().to_string_lossy().to_string(),
                    Some("rebuild journal".into()),
                )
                .await
                .unwrap();
            let session = engine
                .create_session(project.id.clone(), "rebuild journal".into(), Some(10_000))
                .await
                .unwrap();
            let worktree = session.worktree.clone().unwrap();
            let journal = crate::run_journal::RunJournal::begin(
                &engine,
                &session,
                &project.id,
                "leader",
                "Leader",
                "leader",
                None,
                None,
                "rebuild journal",
                "test",
                "test-model",
                Path::new(&worktree),
                RunBudgetSnapshot::default(),
                true,
            )
            .await
            .unwrap();
            journal
                .checkpoint(&engine, 1, RunBudgetSnapshot::default())
                .await
                .unwrap();
            journal.finish_completed(&engine).await.unwrap();
            engine
                .set_session_run_policy(
                    &project.id,
                    SessionRunPolicy {
                        auto_resume: true,
                        max_attempts: 3,
                        max_age_hours: 12,
                    },
                )
                .await
                .unwrap();
            engine.record_project_manifest().await.unwrap();
            (project.id, session.id, journal.id().to_string())
        };

        let report = rebuild_database(data.path(), false).await.unwrap();
        assert_eq!(report.sessions, 1);
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let runs = engine.session_runs(&session_id, 10).await.unwrap().runs;
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, run_id);
        assert_eq!(runs[0].status, "completed");
        let detail = engine.session_run_detail(&run_id).await.unwrap();
        assert_eq!(detail.checkpoints.len(), 1);
        let policy = engine.session_run_policy(&project_id).await.unwrap();
        assert_eq!(
            policy.policy,
            SessionRunPolicy {
                auto_resume: true,
                max_attempts: 3,
                max_age_hours: 12,
            }
        );
        let replay = engine.replay_session(&session_id).await.unwrap();
        assert!(replay.runs_match, "{:?}", replay.mismatches);
        assert!(replay.run_checkpoints_match, "{:?}", replay.mismatches);
    }
}
