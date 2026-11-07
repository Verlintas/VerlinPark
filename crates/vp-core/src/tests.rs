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
async fn searches_sessions_and_validates_the_query() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("search".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "find the needle".into(), None)
        .await
        .unwrap();
    engine
        .append_message(
            &session.id,
            "user",
            "the needle is here",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    engine
        .append_message(
            &session.id,
            "assistant",
            "needle found",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();

    assert!(engine.search("   ", Some(&project.id), 10).await.is_err());
    assert!(
        engine
            .search(&"x".repeat(201), Some(&project.id), 10)
            .await
            .is_err()
    );
    let result = engine.search("needle", Some(&project.id), 1).await.unwrap();
    assert_eq!(result.hits.len(), 1);
    assert!(result.truncated);
    assert_eq!(result.hits[0].project_id, project.id);
    assert!(
        !engine
            .search("needle", Some("missing-project"), 10)
            .await
            .unwrap()
            .hits
            .iter()
            .any(|hit| hit.session_id == session.id)
    );
}

#[tokio::test]
async fn questions_round_trip_and_repair_with_events() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("questions".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "ask".into(), None)
        .await
        .unwrap();
    let (question, receiver) = engine
        .request_question(
            &session.id,
            "agent-1",
            "Leader",
            "Proceed?",
            vec!["yes".into(), "no".into()],
        )
        .await
        .unwrap();
    assert!(engine.answer_question(&question.id, "yes").await.unwrap());
    assert_eq!(receiver.await.unwrap(), "yes");
    let listed = engine
        .list_questions(Some(session.id.clone()))
        .await
        .unwrap();
    assert_eq!(listed.questions.len(), 1);
    assert_eq!(listed.questions[0].status, "answered");
    assert_eq!(listed.questions[0].answer.as_deref(), Some("yes"));

    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.questions_matches);
    assert!(replay.consistent);
    engine
        .store()
        .replace_session_questions(&session.id, &[])
        .unwrap();
    assert!(
        !engine
            .replay_session(&session.id)
            .await
            .unwrap()
            .questions_matches
    );
    assert!(engine.repair_session(&session.id).await.unwrap().consistent);
    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.questions.len(), 1);
}

#[tokio::test]
async fn session_priority_is_created_and_validated() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("priority".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session_with_priority(project.id.clone(), "urgent".into(), None, 50)
        .await
        .unwrap();
    assert_eq!(session.priority, 50);
    let events = engine.list_events(session.id.clone(), 0, 20).await.unwrap();
    assert_eq!(events[0].kind, "session.created");
    assert_eq!(events[0].payload["priority"], 50);
    assert!(
        engine
            .create_session_with_priority(project.id.clone(), "bad".into(), None, 101)
            .await
            .is_err()
    );
    assert!(
        engine
            .create_session_with_priority(project.id, "bad".into(), None, -101)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn skips_and_retries_plan_tasks_with_replay() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("task actions".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "task actions".into(), None)
        .await
        .unwrap();
    let plan: TeamPlan = serde_json::from_value(serde_json::json!({
        "goal": "task actions",
        "departments": [{
            "name": "impl",
            "manager_title": "Implementation",
            "profile": "coding",
            "objective": "build"
        }],
        "tasks": [{
            "id": "T1",
            "department": "impl",
            "title": "Build",
            "kind": "coding",
            "deps": [],
            "acceptance": ["done"]
        }]
    }))
    .unwrap();
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();

    let skipped = engine.skip_plan_task(&session.id, "T1").await.unwrap();
    assert_eq!(skipped.status, "skipped");
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.task_queue_matches, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
    assert!(engine.skip_plan_task(&session.id, "T1").await.is_err());
    let retried = engine.retry_plan_task(&session.id, "T1").await.unwrap();
    assert_eq!(retried.status, "pending");
    let error = engine.retry_plan_task(&session.id, "T1").await.unwrap_err();
    assert!(error.to_string().contains("not retryable"));

    let queue = engine
        .task_queue(Some(project.id.clone()), Some(session.id.clone()))
        .await
        .unwrap();
    assert_eq!(queue.tasks[0].status, "pending");
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(events.iter().any(|event| event.kind == "plan_task.skipped"));
    assert!(events.iter().any(|event| event.kind == "plan_task.retried"));

    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    assert!(engine.skip_plan_task(&session.id, "T1").await.is_err());
}

#[tokio::test]
async fn task_scheduler_policy_round_trips_and_validates() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("scheduler policy".into()),
        )
        .await
        .unwrap();
    let default = engine.get_task_scheduler_policy(&project.id).await.unwrap();
    assert!(!default.policy.enabled);
    assert_eq!(default.policy.cooldown_secs, 60);
    assert!(!default.policy.dispatch_tasks);

    let saved = engine
        .set_task_scheduler_policy(
            &project.id,
            vp_protocol::types::TaskSchedulerPolicy {
                enabled: true,
                cooldown_secs: 12,
                dispatch_tasks: true,
            },
        )
        .await
        .unwrap();
    assert!(saved.policy.enabled);
    assert_eq!(saved.policy.cooldown_secs, 12);
    assert!(saved.policy.dispatch_tasks);
    assert_eq!(
        engine.get_task_scheduler_policy(&project.id).await.unwrap(),
        saved
    );
    assert!(
        engine
            .set_task_scheduler_policy(
                &project.id,
                vp_protocol::types::TaskSchedulerPolicy {
                    enabled: true,
                    cooldown_secs: 0,
                    dispatch_tasks: false,
                },
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .set_task_scheduler_policy(
                "missing-project",
                vp_protocol::types::TaskSchedulerPolicy {
                    enabled: true,
                    cooldown_secs: 60,
                    dispatch_tasks: false,
                },
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn records_memories_and_repairs_their_projection() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("memory".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "memory".into(), None)
        .await
        .unwrap();
    let memory = engine
        .create_memory_as(
            &session.id,
            "agent-1",
            "Leader",
            "decision",
            "Keep merges deterministic",
            "The Integrator must not depend on an LLM.",
            vec!["architecture".into()],
        )
        .await
        .unwrap();
    let listed = engine
        .list_memories(&project.id, Some("deterministic".into()), None, 10)
        .await
        .unwrap();
    assert_eq!(listed.memories.len(), 1);
    assert_eq!(listed.memories[0].id, memory.id);

    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.memories_matches);
    assert!(replay.consistent);
    engine
        .store()
        .replace_session_memories(&session.id, &[])
        .unwrap();
    let drifted = engine.replay_session(&session.id).await.unwrap();
    assert!(!drifted.memories_matches);
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.consistent);
    assert_eq!(
        engine
            .list_memories(&project.id, None, None, 10)
            .await
            .unwrap()
            .memories
            .len(),
        1
    );
    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.memories.len(), 1);
}

#[tokio::test]
async fn deletes_and_prunes_memories_with_replay_support() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("memory retention".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "memory".into(), None)
        .await
        .unwrap();
    let mut memories = Vec::new();
    for index in 0..3 {
        memories.push(
            engine
                .create_memory_as(
                    &session.id,
                    "agent-1",
                    "Leader",
                    "note",
                    &format!("memory {index}"),
                    &format!("body {index}"),
                    Vec::new(),
                )
                .await
                .unwrap(),
        );
    }

    let deleted = engine.delete_memory(&memories[0].id).await.unwrap();
    assert!(deleted.removed);
    assert_eq!(deleted.memory_id, memories[0].id);
    assert_eq!(
        engine
            .list_memories(&project.id, None, None, 10)
            .await
            .unwrap()
            .memories
            .len(),
        2
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.memories_matches);

    engine
        .store()
        .replace_session_memories(&session.id, &memories)
        .unwrap();
    assert!(
        !engine
            .replay_session(&session.id)
            .await
            .unwrap()
            .memories_matches
    );
    assert!(engine.repair_session(&session.id).await.unwrap().consistent);
    assert_eq!(
        engine
            .list_memories(&project.id, None, None, 10)
            .await
            .unwrap()
            .memories
            .len(),
        2
    );

    let preview = engine
        .prune_memories(&project.id, None, Some(1), true)
        .await
        .unwrap();
    assert_eq!(preview.candidates, 1);
    assert_eq!(preview.removed, 0);
    assert!(preview.dry_run);
    let pruned = engine
        .prune_memories(&project.id, None, Some(1), false)
        .await
        .unwrap();
    assert_eq!(pruned.candidates, 1);
    assert_eq!(pruned.removed, 1);
    assert!(!pruned.dry_run);
    let remaining = engine
        .list_memories(&project.id, None, None, 10)
        .await
        .unwrap();
    assert_eq!(remaining.memories.len(), 1);
    assert_eq!(remaining.memories[0].id, memories[2].id);
    assert!(
        engine
            .prune_memories(&project.id, None, None, false)
            .await
            .is_err()
    );

    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "memory.deleted")
            .count(),
        2
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.memories_matches);
    assert!(replay.consistent);
    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.memories.len(), 1);
}

#[tokio::test]
async fn sweeps_enabled_memory_policies() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("memory policy".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "memory policy".into(), None)
        .await
        .unwrap();
    for index in 0..3 {
        engine
            .create_memory_as(
                &session.id,
                "agent-1",
                "Leader",
                "note",
                &format!("memory {index}"),
                "body",
                Vec::new(),
            )
            .await
            .unwrap();
    }
    assert!(
        engine
            .set_memory_policy(
                &project.id,
                MemoryPolicy {
                    enabled: true,
                    max_items: None,
                    max_age_days: None,
                },
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .set_memory_policy(
                &project.id,
                MemoryPolicy {
                    enabled: true,
                    max_items: Some(0),
                    max_age_days: None,
                },
            )
            .await
            .is_err()
    );
    let policy = MemoryPolicy {
        enabled: true,
        max_items: Some(1),
        max_age_days: None,
    };
    engine
        .set_memory_policy(&project.id, policy.clone())
        .await
        .unwrap();
    assert_eq!(
        engine.get_memory_policy(&project.id).await.unwrap().policy,
        policy
    );
    assert_eq!(engine.sweep_memory_retention().await.unwrap(), 2);
    assert_eq!(
        engine
            .list_memories(&project.id, None, None, 10)
            .await
            .unwrap()
            .memories
            .len(),
        1
    );
    engine
        .set_memory_policy(
            &project.id,
            MemoryPolicy {
                enabled: false,
                ..policy
            },
        )
        .await
        .unwrap();
    assert_eq!(engine.sweep_memory_retention().await.unwrap(), 0);
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.memories_matches);
    assert!(replay.consistent);
}

#[tokio::test]
async fn reports_project_usage_by_session() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("usage report".into()),
        )
        .await
        .unwrap();
    let first = engine
        .create_session(project.id.clone(), "first".into(), None)
        .await
        .unwrap();
    let second = engine
        .create_session(project.id.clone(), "second".into(), None)
        .await
        .unwrap();
    engine
        .record_agent_usage(
            &first.id, "agent-1", "Worker", "worker", None, None, "model", 10, 5, false, 0.5,
        )
        .await
        .unwrap();
    engine
        .record_agent_usage(
            &second.id, "agent-2", "Leader", "leader", None, None, "model", 3, 1, false, 0.1,
        )
        .await
        .unwrap();

    let report = engine.project_usage_report(&project.id).await.unwrap();
    assert_eq!(report.sessions.len(), 2);
    assert_eq!(report.totals.calls, 2);
    assert_eq!(report.totals.input_tokens, 13);
    assert_eq!(report.totals.output_tokens, 6);
    assert!((report.totals.cost_usd - 0.6).abs() < 1e-9);
    assert!(engine.project_usage_report("missing").await.is_err());
}

#[tokio::test]
async fn exports_usage_as_escaped_csv() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("usage".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "measure usage".into(), None)
        .await
        .unwrap();
    engine
        .record_agent_usage(
            &session.id,
            "agent-1",
            "Worker, One",
            "worker",
            Some("T1".into()),
            Some("impl".into()),
            "test-model",
            10,
            5,
            true,
            0.25,
        )
        .await
        .unwrap();

    let result = engine.usage_csv(&session.id).await.unwrap();
    assert_eq!(result.session_id, session.id);
    assert!(result.csv.starts_with("created_at,agent_name,role,"));
    assert!(result.csv.contains("\"Worker, One\""));
    assert!(
        result
            .csv
            .contains(",worker,T1,impl,test-model,10,5,15,true,0.250000")
    );
}

#[tokio::test]
async fn exports_a_markdown_transcript() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("transcript".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "write the transcript".into(), None)
        .await
        .unwrap();
    engine
        .append_message(
            &session.id,
            "user",
            "hello transcript",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    let plan = TeamPlan {
        goal: "write the transcript".into(),
        departments: vec![vp_protocol::types::PlanDepartment {
            name: "docs".into(),
            manager_title: "Documentation".into(),
            profile: "coding".into(),
            objective: "document it".into(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks: vec![vp_protocol::types::PlanTask {
            id: "T1".into(),
            department: "docs".into(),
            title: "Write the file".into(),
            kind: "coding".into(),
            priority: 5,
            profile: Some("coding".into()),
            deps: Vec::new(),
            write_scope: vec!["docs".into()],
            acceptance: vec!["done".into()],
            token_budget: None,
            cost_budget_usd: None,
        }],
    };
    engine.set_team_plan(&session.id, &plan).await.unwrap();

    let transcript = engine.session_transcript(&session.id).await.unwrap();
    assert_eq!(transcript.session_id, session.id);
    assert!(transcript.markdown.contains("# write the transcript"));
    assert!(transcript.markdown.contains("## Team Plan"));
    assert!(transcript.markdown.contains("**T1** Write the file"));
    assert!(transcript.markdown.contains("hello transcript"));
    assert!(engine.session_transcript("missing").await.is_err());
}

#[tokio::test]
async fn shell_policy_round_trips_and_validates() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("policy".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        engine.get_shell_policy(&project.id).await.unwrap(),
        ShellPolicy::default()
    );
    assert!(engine.get_shell_policy("missing-project").await.is_err());
    let deny_path = data.path().join("secret.txt");
    let saved = engine
        .set_shell_policy(
            &project.id,
            ShellPolicy {
                network: "deny".into(),
                read_deny: vec![deny_path.to_string_lossy().to_string()],
                restrict_home: true,
                read_jail: true,
            },
        )
        .await
        .unwrap();
    assert_eq!(saved.network, "deny");
    assert!(saved.read_jail);
    assert_eq!(engine.get_shell_policy(&project.id).await.unwrap(), saved);
    assert!(
        engine
            .set_shell_policy(
                &project.id,
                ShellPolicy {
                    network: "sometimes".into(),
                    read_deny: Vec::new(),
                    restrict_home: false,
                    read_jail: false,
                },
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .set_shell_policy(
                &project.id,
                ShellPolicy {
                    network: "allow".into(),
                    read_deny: vec!["relative/path".into()],
                    restrict_home: false,
                    read_jail: false,
                },
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn session_budgets_round_trip() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("budgets".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session_with_limits(
            project.id.clone(),
            "budgeted".into(),
            Some(1_000),
            Some(2.5),
            Some(120),
        )
        .await
        .unwrap();
    assert_eq!(session.token_budget, Some(1_000));
    assert_eq!(session.cost_budget_usd, Some(2.5));
    assert_eq!(session.time_budget_secs, Some(120));
    assert_eq!(session.cost_spent_usd, 0.0);
    let stored = engine
        .list_sessions(Some(project.id.clone()))
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.id == session.id)
        .unwrap();
    assert_eq!(stored.cost_budget_usd, Some(2.5));
    assert_eq!(stored.time_budget_secs, Some(120));
    engine.record_cost(&session.id, 1.25).await.unwrap();
    let stored = engine
        .list_sessions(Some(project.id.clone()))
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.id == session.id)
        .unwrap();
    assert_eq!(stored.cost_spent_usd, 1.25);
    assert!(
        engine
            .create_session_with_limits(
                project.id.clone(),
                "bad cost".into(),
                None,
                Some(0.0),
                None,
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .create_session_with_limits(project.id, "bad time".into(), None, None, Some(0),)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn binary_artifacts_and_gc_retention() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("artifacts".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "artifacts".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(worktree.join("binary.bin"), b"\x00\x01\x02").unwrap();
    let first = engine
        .publish_artifact_file(
            &session.id,
            "agent-a",
            "Worker",
            Some("T1".into()),
            "binary.bin",
            "binary",
            "application/octet-stream",
            &worktree.join("binary.bin"),
            &worktree,
        )
        .await
        .unwrap();
    assert_eq!(first.storage, "file");
    assert_eq!(first.compression, "zstd");
    assert_eq!(first.size_bytes, 3);
    assert!(!first.sha256.is_empty());
    let compressed = std::fs::read(first.path.as_ref().unwrap()).unwrap();
    assert_eq!(&compressed[..4], &[0x28, 0xb5, 0x2f, 0xfd]);
    let materialized = engine
        .materialize_artifact(&first.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(std::fs::read(&materialized.path).unwrap(), b"\x00\x01\x02");

    std::fs::write(worktree.join("second.bin"), b"second").unwrap();
    let second = engine
        .publish_artifact_file(
            &session.id,
            "agent-a",
            "Worker",
            None,
            "second.bin",
            "binary",
            "application/octet-stream",
            &worktree.join("second.bin"),
            &worktree,
        )
        .await
        .unwrap();
    let policy = engine
        .set_artifact_policy(
            &project.id,
            ArtifactPolicy {
                auto_gc: false,
                max_bytes: None,
                max_age_secs: None,
                max_count: Some(1),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        engine.get_artifact_policy(&project.id).await.unwrap(),
        policy
    );
    assert!(
        engine
            .set_artifact_policy(
                &project.id,
                ArtifactPolicy {
                    auto_gc: false,
                    max_bytes: None,
                    max_age_secs: None,
                    max_count: Some(0),
                },
            )
            .await
            .is_err()
    );
    let gc = engine.gc_artifacts(&project.id, &policy).await.unwrap();
    assert_eq!(gc.removed, 1);
    assert!(
        !std::path::Path::new(&materialized.path).exists(),
        "materialized cache must follow blob GC"
    );
    let remaining = engine.list_artifacts(&session.id).await.unwrap();
    assert_eq!(remaining.len(), 1);
    let kept = &remaining[0];
    assert!(kept.id == first.id || kept.id == second.id);
    assert!(
        std::path::Path::new(first.path.as_ref().unwrap()).exists()
            || std::path::Path::new(second.path.as_ref().unwrap()).exists()
    );
    let pruned_id = if kept.id == first.id {
        &second.id
    } else {
        &first.id
    };
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "artifact.pruned" && event.payload["artifact_id"] == *pruned_id
    }));
}

#[tokio::test]
async fn artifact_blobs_dedupe_and_keep_a_reference_count() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("blobs".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "blobs".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(worktree.join("same.bin"), b"same bytes").unwrap();
    let first = engine
        .publish_artifact_file(
            &session.id,
            "agent-a",
            "Worker",
            Some("T1".into()),
            "first.bin",
            "binary",
            "application/octet-stream",
            &worktree.join("same.bin"),
            &worktree,
        )
        .await
        .unwrap();
    let second = engine
        .publish_artifact_file(
            &session.id,
            "agent-a",
            "Worker",
            Some("T1".into()),
            "second.bin",
            "binary",
            "application/octet-stream",
            &worktree.join("same.bin"),
            &worktree,
        )
        .await
        .unwrap();
    assert_eq!(first.path, second.path);
    assert_eq!(first.sha256, second.sha256);
    let blob = std::path::PathBuf::from(first.path.as_ref().unwrap());
    assert!(blob.is_file());
    assert_eq!(
        blob.file_name().unwrap().to_string_lossy(),
        format!("{}.zst", first.sha256)
    );

    let policy = ArtifactPolicy {
        auto_gc: false,
        max_bytes: None,
        max_age_secs: None,
        max_count: Some(1),
    };
    let gc = engine.gc_artifacts(&project.id, &policy).await.unwrap();
    assert_eq!(gc.removed, 1);
    assert_eq!(gc.bytes_freed, 0, "a shared blob must not be freed early");
    assert!(blob.is_file());
    let remaining = engine.list_artifacts(&session.id).await.unwrap();
    assert_eq!(remaining.len(), 1);
    let (event, remove_path) = engine
        .store()
        .delete_artifact_with_event(&remaining[0].id)
        .unwrap()
        .unwrap();
    assert_eq!(event.kind, "artifact.pruned");
    assert_eq!(
        remove_path.as_deref(),
        Some(blob.to_string_lossy().as_ref())
    );
    std::fs::remove_file(&blob).unwrap();
    assert!(!blob.exists());
}

#[tokio::test]
async fn backfills_legacy_artifact_events_for_repair() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("backfill".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "backfill".into(), None)
        .await
        .unwrap();
    let artifact = engine
        .publish_artifact(
            &session.id,
            "agent-a",
            "Worker",
            Some("T1".into()),
            "legacy-note.md",
            "note",
            "text/markdown",
            "legacy content",
        )
        .await
        .unwrap();

    let mut events = engine.store().list_all_events(&session.id).unwrap();
    for event in &mut events {
        if event.kind == "artifact.published" {
            event.payload = serde_json::json!({
                "artifact_id": artifact.id,
                "name": artifact.name,
                "kind": artifact.kind,
                "agent_id": artifact.agent_id,
                "agent_name": artifact.agent_name,
            });
        }
    }
    engine.store().replace_events(&session.id, &events).unwrap();
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.artifacts_matches, "{:?}", report.mismatches);

    let result = engine.backfill_artifact_events(&session.id).await.unwrap();
    assert_eq!(result.scanned, 1);
    assert_eq!(result.backfilled, 1);
    assert_eq!(
        engine
            .backfill_artifact_events(&session.id)
            .await
            .unwrap()
            .backfilled,
        0
    );
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);

    engine.store().delete_artifact(&artifact.id).unwrap();
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(!report.artifacts_matches);
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.artifacts_matches);
    let restored = engine
        .list_artifacts(&session.id)
        .await
        .unwrap()
        .into_iter()
        .find(|candidate| candidate.id == artifact.id)
        .unwrap();
    assert_eq!(restored.content, "legacy content");
}

#[tokio::test]
async fn bundle_import_rejects_artifact_sha_mismatch() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("bundle-sha".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "bundle-sha".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(worktree.join("payload.bin"), b"payload").unwrap();
    engine
        .publish_artifact_file(
            &session.id,
            "agent-a",
            "Worker",
            Some("T1".into()),
            "payload.bin",
            "binary",
            "application/octet-stream",
            &worktree.join("payload.bin"),
            &worktree,
        )
        .await
        .unwrap();
    let mut bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.artifacts.len(), 1);
    bundle.artifacts[0].sha256 = "deadbeef".to_string();
    let import_data = TempDir::new().unwrap();
    let imported_engine = Engine::new(Store::open(import_data.path()).unwrap());
    let error = imported_engine.import_session(bundle).await.unwrap_err();
    assert!(error.to_string().contains("sha256"), "{error}");
}

#[tokio::test]
async fn mailbox_retention_protects_unread_messages() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("mailbox".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "mailbox".into(), None)
        .await
        .unwrap();
    for index in 0..3 {
        engine
            .send_agent_message(
                &session.id,
                "sender",
                "Sender",
                "receiver",
                "Receiver",
                "note",
                &format!("message {index}"),
                serde_json::json!({}),
            )
            .await
            .unwrap();
    }
    let messages = engine
        .list_agent_messages(&session.id, None, false)
        .await
        .unwrap();
    assert_eq!(messages.len(), 3);

    let policy = engine
        .set_mailbox_policy(
            &project.id,
            MailboxPolicy {
                max_age_secs: None,
                max_count: Some(2),
                delete_unread: false,
                auto_gc: false,
            },
        )
        .await
        .unwrap();
    let gc = engine.gc_mailbox(&project.id, &policy).await.unwrap();
    assert_eq!(gc.removed, 0);
    assert_eq!(
        engine
            .list_agent_messages(&session.id, None, false)
            .await
            .unwrap()
            .len(),
        3
    );

    engine
        .mark_agent_message_read(&messages[0].id)
        .await
        .unwrap();
    let gc = engine.gc_mailbox(&project.id, &policy).await.unwrap();
    assert_eq!(gc.removed, 1);
    let remaining = engine
        .list_agent_messages(&session.id, None, false)
        .await
        .unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(!remaining.iter().any(|message| message.id == messages[0].id));

    let policy = engine
        .set_mailbox_policy(
            &project.id,
            MailboxPolicy {
                max_age_secs: None,
                max_count: Some(1),
                delete_unread: true,
                auto_gc: false,
            },
        )
        .await
        .unwrap();
    let gc = engine.gc_mailbox(&project.id, &policy).await.unwrap();
    assert_eq!(gc.removed, 1);
    assert_eq!(
        engine
            .list_agent_messages(&session.id, None, false)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(
        engine
            .set_mailbox_policy(
                &project.id,
                MailboxPolicy {
                    max_age_secs: None,
                    max_count: Some(0),
                    delete_unread: false,
                    auto_gc: false,
                },
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn mailbox_auto_gc_respects_unread_protection() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("auto-mailbox".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "auto-mailbox".into(), None)
        .await
        .unwrap();
    engine
        .set_mailbox_policy(
            &project.id,
            MailboxPolicy {
                max_age_secs: None,
                max_count: Some(1),
                delete_unread: true,
                auto_gc: true,
            },
        )
        .await
        .unwrap();
    for index in 0..2 {
        engine
            .send_agent_message(
                &session.id,
                "sender",
                "Sender",
                "receiver",
                "Receiver",
                "note",
                &format!("auto {index}"),
                serde_json::json!({}),
            )
            .await
            .unwrap();
    }
    let messages = engine
        .list_agent_messages(&session.id, None, false)
        .await
        .unwrap();
    assert_eq!(messages.len(), 1, "auto GC should trim to max_count");

    engine
        .set_mailbox_policy(
            &project.id,
            MailboxPolicy {
                max_age_secs: None,
                max_count: Some(2),
                delete_unread: false,
                auto_gc: true,
            },
        )
        .await
        .unwrap();
    for index in 0..2 {
        engine
            .send_agent_message(
                &session.id,
                "sender",
                "Sender",
                "receiver",
                "Receiver",
                "note",
                &format!("protected {index}"),
                serde_json::json!({}),
            )
            .await
            .unwrap();
    }
    let messages = engine
        .list_agent_messages(&session.id, None, false)
        .await
        .unwrap();
    assert_eq!(messages.len(), 3, "unread messages must not be deleted");
    engine
        .mark_agent_message_read(&messages[0].id)
        .await
        .unwrap();
    let messages = engine
        .list_agent_messages(&session.id, None, false)
        .await
        .unwrap();
    assert_eq!(messages.len(), 2, "auto GC should trim after a read");
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .filter(|event| event.kind == "mailbox.pruned")
            .count()
            >= 2
    );
}

#[tokio::test]
async fn mailbox_replay_tracks_reads_prunes_and_repairs() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("mailbox replay".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "mailbox replay".into(), None)
        .await
        .unwrap();
    let first = engine
        .send_agent_message(
            &session.id,
            "sender",
            "Sender",
            "receiver",
            "Receiver",
            "note",
            "first",
            serde_json::json!({}),
        )
        .await
        .unwrap();
    engine
        .send_agent_message(
            &session.id,
            "sender",
            "Sender",
            "receiver",
            "Receiver",
            "note",
            "second",
            serde_json::json!({}),
        )
        .await
        .unwrap();
    engine.mark_agent_message_read(&first.id).await.unwrap();

    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.mailbox_matches, "{:?}", report.mismatches);

    // Bypass the event path: replay must expose the missing row.
    engine.store().delete_agent_message(&first.id).unwrap();
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(!report.mailbox_matches);
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.consistent, "{:?}", repaired.mismatches);
    assert!(repaired.mailbox_matches);
    let messages = engine
        .list_agent_messages(&session.id, None, false)
        .await
        .unwrap();
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages
            .iter()
            .find(|message| message.id == first.id)
            .unwrap()
            .status,
        "read"
    );

    let policy = engine
        .set_mailbox_policy(
            &project.id,
            MailboxPolicy {
                max_age_secs: None,
                max_count: Some(1),
                delete_unread: false,
                auto_gc: false,
            },
        )
        .await
        .unwrap();
    let gc = engine.gc_mailbox(&project.id, &policy).await.unwrap();
    assert_eq!(gc.removed, 1);
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "mailbox.pruned" && event.payload["message_id"] == first.id
    }));
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);
    assert!(report.mailbox_matches);
}

#[tokio::test]
async fn doctor_reports_missing_worktrees_and_clean_mirrors() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("doctor".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "doctor".into(), None)
        .await
        .unwrap();
    let checks = engine.doctor_checks().await.unwrap();
    assert!(checks.iter().all(|check| check.status != "error"));
    assert!(
        checks
            .iter()
            .any(|check| check.name == "event mirrors" && check.status == "ok")
    );
    let worktree = session.worktree.clone().unwrap();
    std::fs::remove_dir_all(&worktree).unwrap();
    let checks = engine.doctor_checks().await.unwrap();
    let sessions = checks
        .iter()
        .find(|check| check.name == "sessions")
        .unwrap();
    assert!(sessions.detail.contains("1 missing worktree"));
}

#[tokio::test]
async fn project_budget_round_trips_and_reports_spend() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("project-budget".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "budget".into(), None)
        .await
        .unwrap();
    let budget = engine
        .set_project_budget(
            &project.id,
            ProjectBudget {
                token_budget: Some(1000),
                cost_budget_usd: Some(0.5),
            },
        )
        .await
        .unwrap();
    assert_eq!(budget.budget.token_budget, Some(1000));
    assert_eq!(budget.spent_tokens, 0);
    engine
        .record_agent_usage(
            &session.id,
            "worker-1",
            "Worker",
            "worker",
            Some("T1".into()),
            Some("impl".into()),
            "test",
            100,
            50,
            false,
            0.25,
        )
        .await
        .unwrap();
    let budget = engine.get_project_budget(&project.id).await.unwrap();
    assert_eq!(budget.spent_tokens, 150);
    assert!((budget.spent_cost_usd - 0.25).abs() < 1e-9);
    assert!(
        engine
            .set_project_budget(
                &project.id,
                ProjectBudget {
                    token_budget: Some(0),
                    cost_budget_usd: None,
                },
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn integration_policy_round_trips_and_validates() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("integration-policy".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        engine.get_integration_policy(&project.id).await.unwrap(),
        IntegrationPolicy::default()
    );
    let saved = engine
        .set_integration_policy(
            &project.id,
            IntegrationPolicy {
                auto_resolve: true,
                max_auto_attempts: 3,
                max_resolve_tokens: Some(25_000),
            },
        )
        .await
        .unwrap();
    assert!(saved.auto_resolve);
    assert_eq!(
        engine.get_integration_policy(&project.id).await.unwrap(),
        saved
    );
    assert!(
        engine
            .set_integration_policy(
                &project.id,
                IntegrationPolicy {
                    auto_resolve: true,
                    max_auto_attempts: 6,
                    max_resolve_tokens: None,
                },
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .set_integration_policy(
                &project.id,
                IntegrationPolicy {
                    auto_resolve: true,
                    max_auto_attempts: 1,
                    max_resolve_tokens: Some(10),
                },
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn discards_uncommitted_worktree_changes() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("demo".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "discard".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(worktree.join("README.md"), "dirty\n").unwrap();
    std::fs::write(worktree.join("scratch.txt"), "untracked\n").unwrap();

    engine.discard_session(&session.id).await.unwrap();
    assert_eq!(
        std::fs::read_to_string(worktree.join("README.md")).unwrap(),
        "hello\n"
    );
    assert!(!worktree.join("scratch.txt").exists());
    let diff = engine.session_diff(&session.id).await.unwrap();
    assert!(diff.status.is_empty());
    assert!(diff.patch.is_empty());
    let events = engine.list_events(session.id, 0, 100).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "session.discarded"));
}

#[tokio::test]
async fn attributes_approvals_to_agents() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("demo".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "approve".into(), None)
        .await
        .unwrap();
    let (_approval, _receiver) = engine
        .request_approval(
            &session.id,
            "call-1",
            "write_file",
            serde_json::json!({"path": "README.md"}),
            Some(serde_json::json!({
                "agent_id": "worker-1",
                "agent_name": "Worker 1",
                "agent_role": "worker"
            })),
        )
        .await
        .unwrap();
    let events = engine.list_events(session.id, 0, 10).await.unwrap();
    let event = events
        .iter()
        .find(|event| event.kind == "approval.requested")
        .unwrap();
    assert_eq!(event.payload["agent_name"], "Worker 1");
    assert_eq!(event.payload["agent_role"], "worker");
}

#[tokio::test]
async fn commits_worktree_changes_once() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("demo".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "commit".into(), None)
        .await
        .unwrap();
    let worktree = session.worktree.clone().unwrap();
    std::fs::write(Path::new(&worktree).join("README.md"), "committed\n").unwrap();

    let commit = engine
        .commit_session(&session.id, "update readme")
        .await
        .unwrap();
    assert_eq!(commit.commit.len(), 7);
    assert_eq!(commit.message, "update readme");
    let diff = engine.session_diff(&session.id).await.unwrap();
    assert!(diff.status.is_empty());
    assert!(diff.patch.contains("+committed"));

    let error = engine
        .commit_session(&session.id, "again")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no changes"));
    let events = engine.list_events(session.id, 0, 100).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "session.committed"));
}

#[tokio::test]
async fn lands_a_committed_session_into_the_default_branch() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("demo".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "land".into(), None)
        .await
        .unwrap();
    let worktree = session.worktree.clone().unwrap();
    std::fs::write(Path::new(&worktree).join("README.md"), "landed\n").unwrap();
    engine
        .commit_session(&session.id, "session change")
        .await
        .unwrap();

    let landed = engine.land_session(&session.id).await.unwrap();
    assert_eq!(landed.target_branch, project.default_branch);
    assert_eq!(
        std::fs::read_to_string(repo.path().join("README.md")).unwrap(),
        "landed\n"
    );
    let sessions = engine.list_sessions(Some(project.id)).await.unwrap();
    assert_eq!(sessions[0].status, "landed");
    let events = engine.list_events(session.id, 0, 100).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "session.landed"));
}

#[tokio::test]
async fn pushes_a_branch_and_opens_a_pull_request() {
    use std::os::unix::fs::PermissionsExt;

    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let remote = TempDir::new().unwrap();
    init_repo(repo.path());
    let status = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(remote.path())
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["remote", "add", "origin", remote.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("demo".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "PR me".into(), None)
        .await
        .unwrap();
    let worktree = session.worktree.clone().unwrap();
    std::fs::write(Path::new(&worktree).join("README.md"), "pr\n").unwrap();
    engine
        .commit_session(&session.id, "session change")
        .await
        .unwrap();

    let gh = data.path().join("fake-gh");
    std::fs::write(
            &gh,
            "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then exit 0; fi\nif [ \"$1\" = \"pr\" ]; then echo https://example.test/pr/1; exit 0; fi\nexit 0\n",
        )
        .unwrap();
    let mut permissions = std::fs::metadata(&gh).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&gh, permissions).unwrap();

    let result = engine
        .open_pull_request_with_cli(&session.id, gh.to_str().unwrap(), PrCli::GitHub)
        .await
        .unwrap();
    assert!(result.pushed);
    assert_eq!(result.url.as_deref(), Some("https://example.test/pr/1"));
    let branch = session.branch.clone().unwrap();
    let remote_ref = Command::new("git")
        .arg("--git-dir")
        .arg(remote.path())
        .args(["rev-parse", &format!("refs/heads/{branch}")])
        .output()
        .unwrap();
    assert!(remote_ref.status.success());
    let events = engine.list_events(session.id, 0, 100).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "session.pushed"));
    assert!(events.iter().any(|event| event.kind == "session.pr_opened"));
}

#[tokio::test]
async fn opens_a_gitlab_merge_request_with_glab() {
    use std::os::unix::fs::PermissionsExt;

    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let remote = TempDir::new().unwrap();
    init_repo(repo.path());
    let status = Command::new("git")
        .args(["init", "--bare", "-q"])
        .arg(remote.path())
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args(["remote", "add", "origin", remote.path().to_str().unwrap()])
        .status()
        .unwrap();
    assert!(status.success());

    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("gitlab".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "MR me".into(), None)
        .await
        .unwrap();
    let worktree = session.worktree.clone().unwrap();
    std::fs::write(Path::new(&worktree).join("README.md"), "mr\n").unwrap();
    engine
        .commit_session(&session.id, "session change")
        .await
        .unwrap();

    let glab = data.path().join("fake-glab");
    std::fs::write(
        &glab,
        "#!/bin/sh\nif [ \"$1\" = \"auth\" ]; then exit 0; fi\nif [ \"$1\" = \"mr\" ]; then echo https://gitlab.example/mr/1; exit 0; fi\nexit 0\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&glab).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&glab, permissions).unwrap();

    let result = engine
        .open_pull_request_with_cli(&session.id, glab.to_str().unwrap(), PrCli::GitLab)
        .await
        .unwrap();
    assert!(result.pushed);
    assert_eq!(result.url.as_deref(), Some("https://gitlab.example/mr/1"));
    assert!(result.detail.contains("MR opened"));
}

#[tokio::test]
async fn recovers_interrupted_sessions_and_pending_approvals() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("demo".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "run".into(), None)
        .await
        .unwrap();
    engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    let (approval, _receiver) = engine
        .request_approval(
            &session.id,
            "call-1",
            "write_file",
            serde_json::json!({}),
            None,
        )
        .await
        .unwrap();
    let queue_entry = engine
        .enqueue_merge(
            &session.id,
            "agent-1",
            "Worker 1",
            Some("T1".into()),
            "verlinpark/task",
            "abc123",
            "def456",
            None,
        )
        .await
        .unwrap();
    engine
        .update_merge_queue_entry(&queue_entry.id, "merging", None)
        .await
        .unwrap();
    let recovered = engine.recover_interrupted_sessions().await.unwrap();
    assert_eq!(recovered, 1);
    let sessions = engine
        .list_sessions(Some(session.project_id.clone()))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "failed");
    let approvals = engine
        .list_approvals(Some(session.id.clone()))
        .await
        .unwrap();
    assert_eq!(approvals[0].status, "denied");
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(events.iter().any(|event| event.kind == "agent.failed"));
    assert!(
        events.iter().any(|event| event.kind == "approval.resolved"
            && event.payload["approval_id"] == approval.id)
    );
    let queue = engine.list_merge_queue(&session.id).await.unwrap();
    assert_eq!(queue[0].status, "interrupted");
    assert!(events.iter().any(|event| {
        event.kind == "merge_queue.updated"
            && event.payload["entry"]["id"] == queue_entry.id
            && event.payload["entry"]["status"] == "interrupted"
    }));
}

#[tokio::test]
async fn events_cover_critical_state_transitions() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("demo".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "events".into(), None)
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    engine.record_usage(&session.id, 10, 5).await.unwrap();
    let agent = engine
        .create_agent(&session.id, "worker", "Worker 1", None, None, "test")
        .await
        .unwrap();
    engine
        .update_agent_status(&agent.id, "running")
        .await
        .unwrap();
    let entry = engine
        .enqueue_merge(
            &session.id,
            &agent.id,
            &agent.name,
            Some("T1".into()),
            "verlinpark/task",
            "abc123",
            "def456",
            None,
        )
        .await
        .unwrap();
    engine
        .update_merge_queue_entry(&entry.id, "merged", Some("ok".into()))
        .await
        .unwrap();

    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    let last_status = events
        .iter()
        .rev()
        .find(|event| event.kind == "session.updated")
        .and_then(|event| event.payload["status"].as_str());
    assert_eq!(last_status, Some("running"));
    let usage = events
        .iter()
        .filter(|event| event.kind == "agent.usage")
        .map(|event| {
            event.payload["input_tokens"].as_u64().unwrap_or(0)
                + event.payload["output_tokens"].as_u64().unwrap_or(0)
        })
        .sum::<u64>();
    assert_eq!(usage, 15);
    assert!(events.iter().any(|event| {
        event.kind == "agent.updated"
            && event.payload["agent_id"] == agent.id
            && event.payload["status"] == "running"
    }));
    let queue_status = events
        .iter()
        .filter(|event| event.kind == "merge_queue.enqueued" || event.kind == "merge_queue.updated")
        .filter_map(|event| {
            Some((
                event.payload["entry"]["id"].as_str()?.to_string(),
                event.payload["entry"]["status"].as_str()?.to_string(),
            ))
        })
        .fold(std::collections::HashMap::new(), |mut map, (id, status)| {
            map.insert(id, status);
            map
        });
    assert_eq!(
        queue_status.get(&entry.id).map(String::as_str),
        Some("merged")
    );
}

#[tokio::test]
async fn cleans_up_terminal_worktrees_without_losing_branches_by_default() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("cleanup".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "cleanup".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    let branch = session.branch.clone().unwrap();
    let sandbox = data.path().join("sandbox").join(&session.id).join("tmp");
    std::fs::create_dir_all(&sandbox).unwrap();
    std::fs::write(sandbox.join("cache.bin"), "cache").unwrap();
    std::fs::write(worktree.join("dirty.txt"), "dirty\n").unwrap();
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();

    let active = engine
        .create_session(project.id.clone(), "active".into(), None)
        .await
        .unwrap();
    engine
        .update_session_status(&active.id, "running")
        .await
        .unwrap();

    let protected = engine
        .cleanup_worktrees(&project.id, 0, false, false, false, false)
        .await
        .unwrap();
    assert_eq!(protected.removed, 0);
    assert_eq!(protected.skipped, 1);
    assert!(worktree.exists());

    let dry = engine
        .cleanup_worktrees(&project.id, 0, true, true, false, false)
        .await
        .unwrap();
    assert_eq!(dry.removed, 0);
    assert_eq!(dry.entries[0].reason, "would remove");
    assert!(worktree.exists());

    let cleaned = engine
        .cleanup_worktrees(&project.id, 0, false, true, false, false)
        .await
        .unwrap();
    assert_eq!(cleaned.scanned, 1, "active session must be skipped");
    assert_eq!(cleaned.removed, 1);
    assert!(!worktree.exists());
    assert!(!data.path().join("sandbox").join(&session.id).exists());
    assert!(
        worktree::branch_exists(repo.path(), &branch).unwrap(),
        "cleanup must keep the branch unless explicitly asked"
    );
    let active_worktree = active.worktree.clone().unwrap();
    assert!(std::path::Path::new(&active_worktree).exists());
}

#[tokio::test]
async fn keeps_failed_worktrees_when_requested() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("cleanup failed".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "failed cleanup".into(), None)
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "failed")
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());

    let kept = engine
        .cleanup_worktrees(&project.id, 0, false, false, false, true)
        .await
        .unwrap();
    assert_eq!(kept.removed, 0);
    assert_eq!(kept.skipped, 1);
    assert_eq!(kept.entries[0].reason, "kept failed/cancelled worktree");
    assert!(worktree.exists());

    let removed = engine
        .cleanup_worktrees(&project.id, 0, false, true, false, false)
        .await
        .unwrap();
    assert_eq!(removed.removed, 1);
    assert!(!worktree.exists());
}

#[tokio::test]
async fn worktree_policy_round_trips_and_validates() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("retention".into()),
        )
        .await
        .unwrap();
    assert_eq!(
        engine.get_worktree_policy(&project.id).await.unwrap(),
        WorktreeRetentionPolicy::default()
    );
    let policy = WorktreeRetentionPolicy {
        enabled: true,
        older_than_secs: 86_400,
        delete_branches: true,
        keep_failed: false,
    };
    let saved = engine
        .set_worktree_policy(&project.id, &policy)
        .await
        .unwrap();
    assert_eq!(saved.policy, policy);
    assert_eq!(
        engine.get_worktree_policy(&project.id).await.unwrap(),
        policy
    );
    let mut too_old = policy.clone();
    too_old.older_than_secs = 366 * 86_400;
    assert!(
        engine
            .set_worktree_policy(&project.id, &too_old)
            .await
            .is_err()
    );
    assert!(engine.get_worktree_policy("missing").await.is_err());
}

#[tokio::test]
async fn restores_the_event_table_from_the_jsonl_mirror() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("mirror".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "mirror".into(), None)
        .await
        .unwrap();
    let (approval, _receiver) = engine
        .request_approval(
            &session.id,
            "call-1",
            "shell",
            serde_json::json!({"command": "true"}),
            None,
        )
        .await
        .unwrap();
    engine.resolve_approval(&approval.id, true).await.unwrap();
    engine
        .send_agent_message(
            &session.id,
            "sender",
            "Sender",
            "receiver",
            "Receiver",
            "note",
            "mirror message",
            serde_json::json!({}),
        )
        .await
        .unwrap();

    let status = engine.session_log_status(&session.id).await.unwrap();
    assert!(status.matches, "{status:?}");
    assert!(status.mirror_events > 0);
    assert_eq!(status.mirror_events, status.database_events);

    engine.store().replace_events(&session.id, &[]).unwrap();
    let damaged = engine.session_log_status(&session.id).await.unwrap();
    assert!(!damaged.matches);
    assert!(damaged.mirror_events > 0);
    assert_eq!(damaged.database_events, 0);

    let restored = engine.restore_session_log(&session.id).await.unwrap();
    assert!(restored.matches, "{restored:?}");
    assert_eq!(restored.database_events, restored.mirror_events);
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);
    assert_eq!(
        engine
            .list_approvals(Some(session.id.clone()))
            .await
            .unwrap()[0]
            .status,
        "approved"
    );
    assert_eq!(
        engine
            .list_agent_messages(&session.id, None, false)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn replay_checks_critical_projections() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("demo".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "replay".into(), None)
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    engine
        .record_agent_usage(
            &session.id,
            "agent-1",
            "Worker",
            "worker",
            Some("T1".into()),
            Some("impl".into()),
            "test",
            7,
            3,
            false,
            0.25,
        )
        .await
        .unwrap();
    let plan: TeamPlan = serde_json::from_value(serde_json::json!({
        "goal": "replay",
        "departments": [{
            "name": "impl",
            "manager_title": "Implementation",
            "profile": "coding",
            "objective": "build"
        }],
        "tasks": [{
            "id": "T1",
            "department": "impl",
            "title": "Build",
            "kind": "coding",
            "deps": [],
            "acceptance": ["done"]
        }]
    }))
    .unwrap();
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    engine
        .set_todos(
            &session.id,
            vec![TodoItem {
                title: "Build".into(),
                status: "in_progress".into(),
            }],
        )
        .await
        .unwrap();
    engine
        .append_message(&session.id, "user", "hello", serde_json::json!([]), None)
        .await
        .unwrap();
    let (approval, _approval_rx) = engine
        .request_approval(
            &session.id,
            "call-approval",
            "shell",
            serde_json::json!({"command": "true"}),
            None,
        )
        .await
        .unwrap();
    engine.resolve_approval(&approval.id, true).await.unwrap();
    let agent = engine
        .create_agent(&session.id, "worker", "Worker", None, None, "test")
        .await
        .unwrap();
    let agent_task = engine
        .create_task_worktree(
            &project.root,
            &project.id,
            &session.id,
            "agent-task",
            session.branch.as_deref().unwrap(),
        )
        .await
        .unwrap();
    engine
        .set_agent_worktree(
            &agent.id,
            &agent_task.path.to_string_lossy(),
            &agent_task.branch,
        )
        .await
        .unwrap();
    let entry = engine
        .enqueue_merge(
            &session.id,
            &agent.id,
            &agent.name,
            Some("T1".into()),
            "verlinpark/task",
            "abc123",
            "def456",
            None,
        )
        .await
        .unwrap();
    engine
        .update_merge_queue_entry(&entry.id, "merged", None)
        .await
        .unwrap();

    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);
    assert!(report.status_matches);
    assert!(report.usage_matches);
    assert!(report.ledger_matches);
    assert!(report.approvals_matches);
    assert!(report.plan_matches);
    assert!(report.todos_matches);
    assert!(report.messages_matches);
    assert!(report.queue_matches);
    assert!(report.task_queue_matches);

    // A plan lifecycle event without its projection update must show up as
    // task-queue drift, and repair must rebuild the row from the event.
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

    // Bypass the event path deliberately: replay must expose the drift.
    engine
        .store()
        .update_session_status(&session.id, "idle")
        .unwrap();
    engine
        .store()
        .replace_usage_ledger(&session.id, &[])
        .unwrap();
    engine
        .store()
        .create_approval(
            &session.id,
            "orphan-approval",
            "shell",
            &serde_json::json!({"command": "true"}),
        )
        .unwrap();
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(!report.consistent);
    assert!(!report.status_matches);
    assert!(!report.ledger_matches);
    assert!(!report.approvals_matches);
    assert!(!report.task_queue_matches);

    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.repaired);
    assert!(repaired.consistent, "{:?}", repaired.mismatches);
    assert!(repaired.ledger_matches);
    assert!(repaired.approvals_matches);
    assert!(repaired.task_queue_matches);
    let repaired_tasks = engine
        .task_queue(Some(session.project_id.clone()), Some(session.id.clone()))
        .await
        .unwrap();
    assert_eq!(repaired_tasks.tasks.len(), 1);
    assert_eq!(repaired_tasks.tasks[0].status, "completed");
    assert_eq!(repaired_tasks.tasks[0].attempts, 1);
    assert_eq!(
        engine
            .list_approvals(Some(session.id.clone()))
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        engine
            .usage_ledger(&session.id)
            .await
            .unwrap()
            .entries
            .len(),
        1
    );
    let sessions = engine
        .list_sessions(Some(session.project_id.clone()))
        .await
        .unwrap();
    assert_eq!(sessions[0].status, "running");

    engine
        .store()
        .update_agent_status(&agent.id, "cancelled")
        .unwrap();
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(!report.agents_matches);
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.consistent, "{:?}", repaired.mismatches);
    let agents = engine.list_agents(&session.id).await.unwrap();
    assert_eq!(agents[0].status, "running");

    engine
        .send_agent_message(
            &session.id,
            &agent.id,
            &agent.name,
            &agent.id,
            &agent.name,
            "note",
            "bundle mailbox note",
            serde_json::json!({"bundle": true}),
        )
        .await
        .unwrap();
    engine
        .publish_artifact(
            &session.id,
            &agent.id,
            &agent.name,
            Some("T1".into()),
            "bundle-note.md",
            "note",
            "text/markdown",
            "artifact content",
        )
        .await
        .unwrap();
    let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(session_worktree.join("bundle.bin"), b"binary bundle").unwrap();
    engine
        .publish_artifact_file(
            &session.id,
            &agent.id,
            &agent.name,
            Some("T1".into()),
            "bundle.bin",
            "binary",
            "application/octet-stream",
            &session_worktree.join("bundle.bin"),
            &session_worktree,
        )
        .await
        .unwrap();
    engine
        .record_agent_usage(
            &session.id,
            &agent.id,
            &agent.name,
            "worker",
            Some("T1".into()),
            Some("impl".into()),
            "test",
            11,
            5,
            false,
            0.125,
        )
        .await
        .unwrap();

    let artifacts = engine.list_artifacts(&session.id).await.unwrap();
    let note = artifacts
        .iter()
        .find(|artifact| artifact.name == "bundle-note.md")
        .unwrap();
    let note_id = note.id.clone();
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);
    assert!(report.artifacts_matches);
    engine.store().delete_artifact(&note_id).unwrap();
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(!report.artifacts_matches);
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.consistent, "{:?}", repaired.mismatches);
    assert!(repaired.artifacts_matches);
    let restored = engine
        .list_artifacts(&session.id)
        .await
        .unwrap()
        .into_iter()
        .find(|artifact| artifact.id == note_id)
        .unwrap();
    assert_eq!(restored.content, "artifact content");
    assert_eq!(restored.mime_type, "text/markdown");

    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.version, 1);
    assert_eq!(bundle.session.id, session.id);
    assert_eq!(bundle.agents.len(), 1);
    assert_eq!(bundle.messages.len(), 1);
    assert_eq!(bundle.todos.len(), 1);
    assert_eq!(bundle.approvals.len(), 1);
    assert_eq!(bundle.approvals[0].status, "approved");
    assert_eq!(bundle.merge_queue.len(), 1);
    assert_eq!(bundle.agent_messages.len(), 1);
    assert_eq!(bundle.artifacts.len(), 2);
    assert_eq!(bundle.usage.len(), 2);
    assert!(bundle.usage.iter().any(|usage| {
        usage.agent_name == agent.name && usage.department.as_deref() == Some("impl")
    }));
    assert!(bundle.team_plan.is_some());
    assert_eq!(bundle.tasks.len(), 1);
    assert_eq!(bundle.tasks[0].task_id, "T1");
    assert_eq!(bundle.tasks[0].status, "completed");
    assert!(!bundle.events.is_empty());
    let rebuilt = engine.rebuild_session(&session.id).await.unwrap();
    assert!(rebuilt.repaired);
    assert!(rebuilt.consistent, "{:?}", rebuilt.mismatches);
    let events = engine
        .list_events(session.id.clone(), 0, 1000)
        .await
        .unwrap();
    assert!(events.iter().any(|event| event.kind == "session.rebuilt"));

    let original_worktree = session.worktree.clone().unwrap();
    let original_branch = session.branch.clone().unwrap();
    let import_data = TempDir::new().unwrap();
    let imported_engine = Engine::new(Store::open(import_data.path()).unwrap());
    let imported = imported_engine.import_session(bundle).await.unwrap();
    assert_eq!(imported.session_id, session.id);
    assert_eq!(imported.status, "imported");
    assert_eq!(
        imported_engine
            .list_agent_messages(&session.id, None, false)
            .await
            .unwrap()
            .len(),
        1
    );
    let imported_artifacts = imported_engine.list_artifacts(&session.id).await.unwrap();
    assert_eq!(imported_artifacts.len(), 2);
    let imported_binary = imported_artifacts
        .iter()
        .find(|artifact| artifact.name == "bundle.bin")
        .unwrap();
    assert_eq!(imported_binary.storage, "file");
    assert_eq!(imported_binary.compression, "zstd");
    let imported_materialized = imported_engine
        .materialize_artifact(&imported_binary.id)
        .await
        .unwrap()
        .unwrap();
    let imported_bytes = std::fs::read(imported_materialized.path).unwrap();
    assert_eq!(imported_bytes, b"binary bundle");
    let imported_sessions = imported_engine
        .list_sessions(Some(imported.project_id.clone()))
        .await
        .unwrap();
    assert_eq!(imported_sessions[0].status, "imported");
    assert!(imported_sessions[0].worktree.is_none());
    let imported_agents = imported_engine.list_agents(&session.id).await.unwrap();
    assert_eq!(imported_agents.len(), 1);
    let imported_messages = imported_engine.list_messages(&session.id).await.unwrap();
    assert_eq!(imported_messages.len(), 1);
    let imported_tasks = imported_engine
        .task_queue(Some(imported.project_id.clone()), Some(session.id.clone()))
        .await
        .unwrap();
    assert_eq!(imported_tasks.tasks.len(), 1);
    assert_eq!(imported_tasks.tasks[0].status, "completed");
    let imported_usage = imported_engine.usage_ledger(&session.id).await.unwrap();
    assert_eq!(imported_usage.entries.len(), 2);
    assert!(
        imported_usage
            .entries
            .iter()
            .any(|entry| entry.cost_usd == 0.125)
    );
    assert_eq!(imported_usage.totals.input_tokens, 18);
    let imported_approvals = imported_engine
        .list_approvals(Some(session.id.clone()))
        .await
        .unwrap();
    assert_eq!(imported_approvals.len(), 1);
    assert_eq!(imported_approvals[0].status, "approved");
    let imported_report = imported_engine.replay_session(&session.id).await.unwrap();
    assert!(
        imported_report.consistent,
        "{:?}",
        imported_report.mismatches
    );
    let imported_diff = imported_engine.session_diff(&session.id).await.unwrap();
    assert!(imported_diff.status.contains("imported archive"));
    assert!(imported_diff.worktree.is_empty());
    let error = imported_engine
        .discard_session(&session.id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("read-only"));

    let task_remove = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .args([
            "worktree",
            "remove",
            "--force",
            agent_task.path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(task_remove.status.success());
    worktree::remove(
        repo.path(),
        std::path::Path::new(&original_worktree),
        &original_branch,
    );
    let rehydrated_worktree = import_data.path().join("rehydrated");
    let rehydrated = imported_engine
        .rehydrate_session(
            &session.id,
            Some(rehydrated_worktree.to_string_lossy().to_string()),
        )
        .await
        .unwrap();
    assert_eq!(rehydrated.status, "idle");
    assert_eq!(rehydrated.worktree, rehydrated_worktree.to_string_lossy());
    assert!(rehydrated_worktree.join(".git").is_file());
    assert!(rehydrated.agents_remapped >= 1, "{:?}", rehydrated.warnings);
    let remapped_agents = imported_engine.list_agents(&session.id).await.unwrap();
    let remapped_agent = remapped_agents
        .iter()
        .find(|candidate| candidate.id == agent.id)
        .unwrap();
    let remapped_path = remapped_agent.worktree.clone().unwrap();
    assert_ne!(remapped_path, agent_task.path.to_string_lossy().to_string());
    assert!(std::path::Path::new(&remapped_path).join(".git").is_file());
    let rehydrated_report = imported_engine.replay_session(&session.id).await.unwrap();
    assert!(
        rehydrated_report.consistent,
        "{:?}",
        rehydrated_report.mismatches
    );
    let rehydrated_diff = imported_engine.session_diff(&session.id).await.unwrap();
    assert!(!rehydrated_diff.status.contains("imported archive"));
    let error = imported_engine
        .rehydrate_session(&session.id, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("only imported"));
}

#[tokio::test]
async fn creates_session_and_broadcasts_event() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store);
    let mut rx = engine.subscribe();
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("demo".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "ship it".into(), Some(100_000))
        .await
        .unwrap();
    assert!(session.worktree.is_some());
    assert!(session.branch.is_some());
    assert_eq!(session.token_budget, Some(100_000));
    let event = rx.recv().await.unwrap();
    assert_eq!(event.session_id, session.id);
    assert_eq!(event.kind, "session.created");
    let worktree = session.worktree.clone().unwrap();
    std::fs::write(Path::new(&worktree).join("README.md"), "changed\n").unwrap();
    let diff = engine.session_diff(&session.id).await.unwrap();
    assert!(diff.patch.contains("-hello"));
    assert!(diff.patch.contains("+changed"));
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.consistent, "{:?}", replay.mismatches);

    let events = engine.list_events(session.id, 0, 10).await.unwrap();
    assert_eq!(events.len(), 1);
}

#[tokio::test]
async fn updates_session_priority_and_rebuilds_the_projection() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("priority update".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "priority update".into(), None)
        .await
        .unwrap();
    let result = engine.set_session_priority(&session.id, 50).await.unwrap();
    assert_eq!(result.priority, 50);
    assert_eq!(
        engine
            .list_sessions(None)
            .await
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == session.id)
            .unwrap()
            .priority,
        50
    );
    assert!(engine.set_session_priority(&session.id, 101).await.is_err());

    engine.store().set_session_priority(&session.id, 0).unwrap();
    engine.rebuild_session(&session.id).await.unwrap();
    let events = engine.list_events(session.id.clone(), 0, 50).await.unwrap();
    assert!(events.iter().any(|event| event.kind == "session.priority"));
    assert_eq!(
        engine
            .list_sessions(None)
            .await
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == session.id)
            .unwrap()
            .priority,
        50
    );
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn manages_and_probes_acp_agents() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("acp".into()),
        )
        .await
        .unwrap();

    assert!(
        engine
            .add_acp_agent(
                "",
                "npx",
                Vec::new(),
                std::collections::BTreeMap::new(),
                true,
                false,
                900,
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .add_acp_agent(
                "short-timeout",
                "npx",
                Vec::new(),
                std::collections::BTreeMap::new(),
                true,
                false,
                10,
            )
            .await
            .is_err()
    );
    let agent = engine
        .add_acp_agent(
            "fake",
            "/usr/bin/python3",
            vec![script_path(&data, &project).to_string_lossy().to_string()],
            std::collections::BTreeMap::new(),
            true,
            true,
            120,
        )
        .await
        .unwrap();
    assert_eq!(engine.list_acp_agents().await.unwrap().agents.len(), 1);
    assert_eq!(
        engine.resolve_acp_agent("fake").await.unwrap().unwrap().id,
        agent.id
    );
    assert!(
        engine
            .add_acp_agent(
                "fake",
                "npx",
                Vec::new(),
                std::collections::BTreeMap::new(),
                true,
                false,
                900,
            )
            .await
            .is_err(),
        "ACP agent names are unique"
    );
    let updated = engine
        .update_acp_agent(
            &agent.id,
            "fake-2",
            "/usr/bin/python3",
            agent.args.clone(),
            std::collections::BTreeMap::new(),
            true,
            true,
            120,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.name, "fake-2");
    assert!(updated.auto_approve);

    let probe = engine
        .probe_acp_agent("fake-2", Some(project.id.clone()))
        .await
        .unwrap();
    assert_eq!(probe.agent_name, "probe-acp");
    assert_eq!(probe.protocol_version, 1);
    assert!(probe.session_created);
    assert!(engine.remove_acp_agent(&agent.id).await.unwrap());
    assert!(engine.list_acp_agents().await.unwrap().agents.is_empty());
}

fn script_path(data: &TempDir, _project: &vp_protocol::types::Project) -> std::path::PathBuf {
    let path = data.path().join("probe-acp-agent.py");
    std::fs::write(
        &path,
        r#"#!/usr/bin/env python3
import json, sys

def reply(req, result):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": req.get("id"), "result": result}) + "\n")
    sys.stdout.flush()

for line in sys.stdin:
    if not line.strip():
        continue
    req = json.loads(line)
    method = req.get("method")
    if method == "initialize":
        reply(req, {"protocolVersion": 1, "agentCapabilities": {}, "authMethods": [], "agentInfo": {"name": "probe-acp", "version": "1.2.3"}})
    elif method == "session/new":
        reply(req, {"sessionId": "probe-session"})
    else:
        sys.stdout.write(json.dumps({"jsonrpc": "2.0", "id": req.get("id"), "error": {"code": -32601, "message": "not found"}}) + "\n")
        sys.stdout.flush()
"#,
    )
    .unwrap();
    path
}

fn commit_worktree_file(worktree: &Path, name: &str, content: &str) {
    std::fs::write(worktree.join(name), content).unwrap();
    for args in [vec!["add", name], vec!["commit", "-q", "-m", "checkpoint"]] {
        let status = Command::new("git")
            .arg("-C")
            .arg(worktree)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success());
    }
}

#[tokio::test]
async fn checkpoints_and_forks_preserve_committed_branch_and_active_conversation() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("fork".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "fork source".into(), None)
        .await
        .unwrap();
    engine
        .append_message(&session.id, "user", "first", serde_json::json!([]), None)
        .await
        .unwrap();
    engine
        .append_message(
            &session.id,
            "assistant",
            "answer one",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    engine
        .append_message(
            &session.id,
            "assistant",
            "",
            serde_json::json!([{
                "id": "call_1",
                "name": "read_file",
                "arguments": "{\"path\":\"README.md\"}"
            }]),
            None,
        )
        .await
        .unwrap();
    engine
        .append_message(
            &session.id,
            "tool",
            "file body",
            serde_json::json!([]),
            Some("call_1".into()),
        )
        .await
        .unwrap();
    engine
        .append_message(&session.id, "user", "second", serde_json::json!([]), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    commit_worktree_file(&worktree, "checkpoint.txt", "committed state\n");
    let commit = crate::worktree::head_commit(&worktree).unwrap();
    let source_messages = engine.list_messages(&session.id).await.unwrap();

    let checkpoint = engine
        .checkpoint_session(&session.id, Some("before fork".into()))
        .await
        .unwrap();
    assert_eq!(checkpoint.commit, commit);
    assert_eq!(checkpoint.label, "before fork");
    assert_eq!(checkpoint.message_count, source_messages.len());
    assert_eq!(checkpoint.summary, None);
    assert!(!checkpoint.dirty);
    assert_eq!(
        checkpoint.start_message_id.as_deref(),
        Some(source_messages.first().unwrap().id.as_str())
    );
    assert_eq!(
        checkpoint.through_message_id.as_deref(),
        Some(source_messages.last().unwrap().id.as_str())
    );
    assert_eq!(
        engine
            .list_checkpoints(&session.id)
            .await
            .unwrap()
            .checkpoints
            .len(),
        1
    );

    engine
        .append_message(
            &session.id,
            "user",
            "after checkpoint",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    let fork = engine
        .fork_session(
            &session.id,
            Some(&checkpoint.id),
            Some("explore alternative".into()),
        )
        .await
        .unwrap();
    assert_ne!(fork.session.id, session.id);
    assert_eq!(fork.session.goal, "explore alternative");
    assert_eq!(fork.session.status, "draft");
    assert_eq!(fork.session.base_commit.as_deref(), Some(commit.as_str()));
    assert_ne!(fork.session.branch, session.branch);
    let fork_worktree = PathBuf::from(fork.session.worktree.clone().unwrap());
    assert!(fork_worktree.is_dir());
    assert_eq!(
        crate::worktree::head_commit(&fork_worktree).unwrap(),
        commit
    );

    let copied = engine.list_messages(&fork.session.id).await.unwrap();
    assert_eq!(copied.len(), source_messages.len());
    for (source, target) in source_messages.iter().zip(&copied) {
        assert_eq!(source.role, target.role);
        assert_eq!(source.content, target.content);
        assert_eq!(source.tool_call_id, target.tool_call_id);
    }
    assert!(
        !copied
            .iter()
            .any(|message| message.content == "after checkpoint")
    );
    let fork_events = engine
        .list_events(fork.session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        fork_events
            .iter()
            .any(|event| event.kind == "session.forked")
    );
    assert!(fork_events.iter().any(|event| event.kind == "tool.started"));
    assert!(
        fork_events
            .iter()
            .any(|event| event.kind == "tool.finished")
    );
    assert!(
        fork_events
            .iter()
            .any(|event| event.kind == "agent.message")
    );
    let source_events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(source_events.iter().any(|event| {
        event.kind == "session.checkpoint" && event.payload["label"] == "before fork"
    }));
    assert!(source_events.iter().any(|event| {
        event.kind == "session.forked" && event.payload["fork_session_id"] == fork.session.id
    }));
}

#[tokio::test]
async fn forks_copy_a_compaction_summary_and_tail_only() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("summary fork".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "summary source".into(), None)
        .await
        .unwrap();
    let _one = engine
        .append_message(&session.id, "user", "one", serde_json::json!([]), None)
        .await
        .unwrap();
    let two = engine
        .append_message(&session.id, "assistant", "two", serde_json::json!([]), None)
        .await
        .unwrap();
    let three = engine
        .append_message(&session.id, "user", "three", serde_json::json!([]), None)
        .await
        .unwrap();
    let four = engine
        .append_message(
            &session.id,
            "assistant",
            "four",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    let summary = "Earlier conversation summary (auto-compacted from 2 messages):\nkeep this";
    engine
        .append_event(
            &session.id,
            "context.compacted",
            serde_json::json!({
                "mode": "summary",
                "through_message_id": two.id,
                "summary": summary,
                "summarized_messages": 2,
                "dropped_messages": 0,
                "kept_messages": 2,
                "estimated_tokens_before": 100,
                "estimated_tokens_after": 50,
            }),
        )
        .await
        .unwrap();
    let checkpoint = engine.checkpoint_session(&session.id, None).await.unwrap();
    assert_eq!(checkpoint.summary.as_deref(), Some(summary));
    assert_eq!(checkpoint.message_count, 2);
    assert!(!checkpoint.dirty);
    assert_eq!(
        checkpoint.start_message_id.as_deref(),
        Some(three.id.as_str())
    );
    assert_eq!(
        checkpoint.through_message_id.as_deref(),
        Some(four.id.as_str())
    );

    let fork = engine
        .fork_session(&session.id, Some(&checkpoint.id), None)
        .await
        .unwrap();
    let copied = engine.list_messages(&fork.session.id).await.unwrap();
    assert_eq!(copied.len(), 3);
    assert_eq!(copied[0].role, "user");
    assert_eq!(copied[0].content, summary);
    assert_eq!(copied[1].content, "three");
    assert_eq!(copied[2].content, "four");
}

#[tokio::test]
async fn checkpoints_snapshot_dirty_worktrees_and_reject_active_sessions() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("dirty".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "dirty source".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    let head_before = crate::worktree::head_commit(&worktree).unwrap();
    std::fs::write(worktree.join("dirty.txt"), "not committed\n").unwrap();
    let checkpoint = engine
        .checkpoint_session(&session.id, Some("dirty snapshot".into()))
        .await
        .unwrap();
    assert!(checkpoint.dirty);
    assert_ne!(checkpoint.commit, head_before);
    assert_eq!(
        crate::worktree::head_commit(&worktree).unwrap(),
        head_before,
        "snapshot must not move the source HEAD"
    );
    let fork = engine
        .fork_session(&session.id, Some(&checkpoint.id), None)
        .await
        .unwrap();
    let fork_worktree = PathBuf::from(fork.session.worktree.clone().unwrap());
    assert_eq!(
        std::fs::read_to_string(fork_worktree.join("dirty.txt")).unwrap(),
        "not committed\n"
    );
    assert_eq!(
        crate::worktree::head_commit(&fork_worktree).unwrap(),
        checkpoint.commit
    );

    engine
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    let error = engine
        .checkpoint_session(&session.id, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("session must not be queued"));
    let error = engine
        .fork_session(&session.id, None, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("session must not be queued"));
}
