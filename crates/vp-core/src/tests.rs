use super::*;
use std::process::Command;
use std::time::Duration;
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
    assert!(default.policy.enabled);
    assert_eq!(default.policy.cooldown_secs, 60);
    assert!(!default.policy.dispatch_tasks);
    assert!(default.policy.include_intake);
    assert_eq!(default.policy.max_intake_parallel, 2);

    let saved = engine
        .set_task_scheduler_policy(
            &project.id,
            vp_protocol::types::TaskSchedulerPolicy {
                enabled: true,
                cooldown_secs: 12,
                dispatch_tasks: true,
                include_intake: true,
                max_intake_parallel: 2,
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
                    include_intake: true,
                    max_intake_parallel: 2,
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
                    include_intake: true,
                    max_intake_parallel: 2,
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
                    ..MemoryPolicy::default()
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
                    ..MemoryPolicy::default()
                },
            )
            .await
            .is_err()
    );
    let policy = MemoryPolicy {
        enabled: true,
        max_items: Some(1),
        max_age_days: None,
        ..MemoryPolicy::default()
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
        .record_agent_usage_with_cache(
            &session.id,
            "agent-1",
            "Worker, One",
            "worker",
            Some("T1".into()),
            Some("impl".into()),
            "test-model",
            10,
            5,
            4,
            1,
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
            .contains(",worker,T1,impl,test-model,10,4,1,5,15,true,0.250000,0.400000")
    );
}

#[tokio::test]
async fn cache_status_is_empty_then_populated_and_survives_replay() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("cache status".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "measure cache".into(), None)
        .await
        .unwrap();

    let empty = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(empty.input_tokens, 0);
    assert_eq!(empty.cache_read_tokens, 0);
    assert_eq!(empty.cache_write_tokens, 0);
    assert_eq!(empty.output_tokens, 0);
    assert_eq!(empty.cache_hit_ratio, 0.0);
    assert!(empty.prompt_prefix.is_none());
    assert!(empty.preamble_tokens_estimate.is_none());
    assert!(empty.last_call_at.is_none());

    engine
        .record_agent_usage_with_cache(
            &session.id,
            "agent-1",
            "Worker One",
            "worker",
            Some("T1".into()),
            Some("impl".into()),
            "test-model",
            1_000,
            100,
            600,
            100,
            false,
            0.25,
        )
        .await
        .unwrap();
    engine
        .append_event(
            &session.id,
            "context.prompt_prefix",
            serde_json::json!({
                "version": 1,
                "model": "test-model",
                "prefix_hash": "prefix-abc",
                "stable_prefix_chars": 4_000,
                "preamble_tokens_estimate": 7_000_u64,
            }),
        )
        .await
        .unwrap();

    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(status.input_tokens, 1_000);
    assert_eq!(status.output_tokens, 100);
    assert_eq!(status.cache_read_tokens, 600);
    assert_eq!(status.cache_write_tokens, 100);
    assert!((status.cache_hit_ratio - 0.6).abs() < 1e-9);
    assert_eq!(status.prompt_prefix.as_deref(), Some("prefix-abc"));
    assert_eq!(status.preamble_tokens_estimate, Some(7_000));
    assert!(status.last_call_at.is_some());

    let report = engine.project_usage_report(&project.id).await.unwrap();
    assert_eq!(report.sessions.len(), 1);
    assert!((report.sessions[0].cache_hit_ratio - 0.6).abs() < 1e-9);
    assert!((report.totals.cache_hit_ratio - 0.6).abs() < 1e-9);

    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.consistent, "cache usage replay drifted: {replay:?}");
    let derived = engine.usage_ledger(&session.id).await.unwrap();
    assert!((derived.totals.cache_hit_ratio - 0.6).abs() < 1e-9);
    assert!((derived.totals.by_role[0].cache_hit_ratio - 0.6).abs() < 1e-9);
}

#[tokio::test]
async fn cache_status_falls_back_to_legacy_prefix_character_estimate() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("legacy prefix".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "legacy prefix".into(), None)
        .await
        .unwrap();
    engine
        .append_event(
            &session.id,
            "context.prompt_prefix",
            serde_json::json!({
                "prefix_hash": "legacy-prefix",
                "stable_prefix_chars": 4_000,
            }),
        )
        .await
        .unwrap();
    let status = engine.session_cache_status(&session.id).await.unwrap();
    assert_eq!(status.prompt_prefix.as_deref(), Some("legacy-prefix"));
    assert_eq!(status.preamble_tokens_estimate, Some(1_000));
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
async fn session_options_keep_priority_and_budgets_together() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("options".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session_with_options(
            project.id,
            "priority and budgets".into(),
            Some(4_000),
            Some(1.5),
            Some(300),
            25,
        )
        .await
        .unwrap();
    assert_eq!(session.priority, 25);
    assert_eq!(session.token_budget, Some(4_000));
    assert_eq!(session.cost_budget_usd, Some(1.5));
    assert_eq!(session.time_budget_secs, Some(300));
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
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);
    assert!(
        engine
            .create_session_with_limits(
                project.id.clone(),
                "bad tokens".into(),
                Some(0),
                None,
                None,
            )
            .await
            .is_err()
    );
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

static PR_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

struct EnvGuard {
    key: &'static str,
    previous: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let previous = std::env::var_os(key);
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        unsafe {
            match &self.previous {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

struct PrFixture {
    engine: Engine,
    project: Project,
    session: Session,
    _data: TempDir,
    _repo: TempDir,
    _remote: TempDir,
}

async fn pr_fixture(name: &str) -> PrFixture {
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
        .add_project(repo.path().to_string_lossy().to_string(), Some(name.into()))
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "PR me".into(), None)
        .await
        .unwrap();
    let worktree = session.worktree.clone().unwrap();
    std::fs::write(Path::new(&worktree).join("README.md"), format!("{name}\n")).unwrap();
    engine
        .commit_session(&session.id, "session change")
        .await
        .unwrap();
    PrFixture {
        engine,
        project,
        session,
        _data: data,
        _repo: repo,
        _remote: remote,
    }
}

fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, contents).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

#[tokio::test]
async fn opens_a_gitea_pull_request_with_tea_from_provider_env() {
    let _lock = PR_ENV_LOCK.lock().await;
    let fixture = pr_fixture("gitea").await;
    let log = fixture._data.path().join("tea-args");
    let cwd_log = fixture._data.path().join("tea-cwd");
    let tea = fixture._data.path().join("fake-tea");
    write_executable(
        &tea,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"login\" ]; then exit 0; fi\nif [ \"$1\" = \"pr\" ]; then\nprintf '%s\\n' \"$@\" > '{}'\npwd > '{}'\necho https://gitea.example/owner/repo/pulls/1\nexit 0\nfi\nexit 1\n",
            log.display(),
            cwd_log.display()
        ),
    );
    let _provider = EnvGuard::set("VERLINPARK_PR_PROVIDER", "gitea");
    let _bin = EnvGuard::set("VERLINPARK_TEA_BIN", tea.to_str().unwrap());

    let result = fixture
        .engine
        .open_pull_request(&fixture.session.id)
        .await
        .unwrap();
    assert!(result.pushed);
    assert_eq!(
        result.url.as_deref(),
        Some("https://gitea.example/owner/repo/pulls/1")
    );
    assert!(result.detail.contains("PR opened"));

    let branch = fixture.session.branch.clone().unwrap();
    let remote_ref = Command::new("git")
        .arg("--git-dir")
        .arg(fixture._remote.path())
        .args(["rev-parse", &format!("refs/heads/{branch}")])
        .output()
        .unwrap();
    assert!(remote_ref.status.success());

    let args = std::fs::read_to_string(&log).unwrap();
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(&args[..3], &["pr", "create", "--head"]);
    assert_eq!(args[3], branch);
    assert_eq!(args[4], "--base");
    assert_eq!(args[5], fixture.project.default_branch);
    assert_eq!(args[6], "--title");
    assert_eq!(args[7], "PR me");
    assert_eq!(args[8], "--description");
    assert!(args[9].starts_with("Opened from VerlinPark session"));

    let cli_cwd = std::fs::read_to_string(&cwd_log).unwrap();
    let cli_cwd = std::fs::canonicalize(cli_cwd.trim()).unwrap();
    let session_worktree =
        std::fs::canonicalize(fixture.session.worktree.as_deref().unwrap()).unwrap();
    assert_eq!(cli_cwd, session_worktree);
}

#[tokio::test]
async fn forgejo_provider_maps_to_tea() {
    let _lock = PR_ENV_LOCK.lock().await;
    let fixture = pr_fixture("forgejo").await;
    let tea = fixture._data.path().join("fake-tea");
    write_executable(
        &tea,
        "#!/bin/sh\nif [ \"$1\" = \"login\" ]; then exit 0; fi\nif [ \"$1\" = \"pr\" ]; then echo https://forgejo.example/owner/repo/pulls/2; exit 0; fi\nexit 1\n",
    );
    let _provider = EnvGuard::set("VERLINPARK_PR_PROVIDER", "forgejo");
    let _bin = EnvGuard::set("VERLINPARK_TEA_BIN", tea.to_str().unwrap());

    let result = fixture
        .engine
        .open_pull_request(&fixture.session.id)
        .await
        .unwrap();
    assert!(result.pushed);
    assert_eq!(
        result.url.as_deref(),
        Some("https://forgejo.example/owner/repo/pulls/2")
    );
    assert!(result.detail.contains("PR opened"));
}

#[tokio::test]
async fn gitea_login_failure_is_clear_and_does_not_push() {
    let _lock = PR_ENV_LOCK.lock().await;
    let fixture = pr_fixture("gitea-auth").await;
    let marker = fixture._data.path().join("tea-create-called");
    let tea = fixture._data.path().join("fake-tea");
    write_executable(
        &tea,
        &format!(
            "#!/bin/sh\nif [ \"$1\" = \"login\" ]; then echo 'no login found' >&2; exit 1; fi\nif [ \"$1\" = \"pr\" ]; then touch '{}'; echo https://gitea.example/owner/repo/pulls/3; exit 0; fi\nexit 1\n",
            marker.display()
        ),
    );
    let _provider = EnvGuard::set("VERLINPARK_PR_PROVIDER", "gitea");
    let _bin = EnvGuard::set("VERLINPARK_TEA_BIN", tea.to_str().unwrap());

    let error = fixture
        .engine
        .open_pull_request(&fixture.session.id)
        .await
        .unwrap_err();
    let message = format!("{error:#}");
    assert!(message.contains("tea is not authenticated"), "{message}");
    assert!(message.contains("no login found"), "{message}");
    assert!(!marker.exists());

    let branch = fixture.session.branch.clone().unwrap();
    let remote_ref = Command::new("git")
        .arg("--git-dir")
        .arg(fixture._remote.path())
        .args(["rev-parse", &format!("refs/heads/{branch}")])
        .output()
        .unwrap();
    assert!(!remote_ref.status.success());
    let events = fixture
        .engine
        .list_events(fixture.session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(!events.iter().any(|event| event.kind == "session.pushed"));
    assert!(!events.iter().any(|event| event.kind == "session.pr_opened"));
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
    let leader_id = engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .update_agent_status(&leader_id, "running")
        .await
        .unwrap();
    engine
        .update_session_status(&session.id, "waiting_input")
        .await
        .unwrap();
    let (question, _question_receiver) = engine
        .request_question(
            &session.id,
            &leader_id,
            "Leader",
            "Which branch should I use?",
            vec!["main".into(), "release".into()],
        )
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
    let questions = engine
        .list_questions(Some(session.id.clone()))
        .await
        .unwrap();
    assert_eq!(questions.questions[0].id, question.id);
    assert_eq!(questions.questions[0].status, "expired");
    assert!(events.iter().any(|event| {
        event.kind == "question.expired"
            && event.payload["id"] == question.id
            && event.payload["status"] == "expired"
    }));
    let agents = engine.list_agents(&session.id).await.unwrap();
    assert_eq!(agents[0].status, "failed");
    assert!(events.iter().any(|event| {
        event.kind == "agent.updated"
            && event.payload["agent"]["id"] == leader_id
            && event.payload["agent"]["status"] == "failed"
    }));
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);
    assert!(!engine.resolve_approval(&approval.id, true).await.unwrap());
    assert!(
        !engine
            .resolve_approval("missing-approval", true)
            .await
            .unwrap()
    );
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
            None,
            None,
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
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
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
        .set_agent_task(&agent.id, Some("T1".into()), Some("impl".into()))
        .await
        .unwrap();
    let agents = engine.list_agents(&session.id).await.unwrap();
    assert_eq!(agents[0].task_id.as_deref(), Some("T1"));
    assert_eq!(agents[0].department.as_deref(), Some("impl"));
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);

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
        .record_agent_usage_with_cache(
            &session.id,
            &agent.id,
            &agent.name,
            "worker",
            Some("T1".into()),
            Some("impl".into()),
            "test",
            11,
            5,
            6,
            1,
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

    engine
        .set_session_unlimited(&session.id, true)
        .await
        .unwrap();
    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.version, 1);
    assert_eq!(bundle.session.id, session.id);
    assert!(bundle.session.unlimited);
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
    let mut traversal = bundle.clone();
    traversal.session.id = "../escape".into();
    let error = imported_engine.import_session(traversal).await.unwrap_err();
    assert!(error.to_string().contains("safe storage"));
    let mut cross_session = bundle.clone();
    cross_session.agents[0].session_id = "another-session".into();
    let error = imported_engine
        .import_session(cross_session)
        .await
        .unwrap_err();
    assert!(
        error.to_string().contains("different session")
            || error.to_string().contains("belongs to session")
    );
    let imported = imported_engine.import_session(bundle).await.unwrap();
    assert_eq!(imported.session_id, session.id);
    assert_eq!(imported.status, "imported");
    assert!(
        imported_engine
            .store()
            .get_session(&session.id)
            .unwrap()
            .unwrap()
            .unlimited
    );
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
    assert_eq!(imported_agents[0].task_id.as_deref(), Some("T1"));
    assert_eq!(imported_agents[0].department.as_deref(), Some("impl"));
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
    assert!(imported_usage.entries.iter().any(|entry| {
        entry.input_tokens == 11 && entry.cache_read_tokens == 6 && entry.cache_write_tokens == 1
    }));
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

#[tokio::test]
async fn validates_remote_acp_transport_configuration() {
    let data = TempDir::new().unwrap();
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let base = |transport: AcpTransport| AcpAgentConfig {
        name: "remote".into(),
        command: String::new(),
        args: Vec::new(),
        env: Default::default(),
        enabled: true,
        auto_approve: false,
        timeout_secs: 900,
        transport,
        endpoint: Some("https://agent.example.test/acp".into()),
        auth_header_env: Some("ACP_TOKEN".into()),
        headers: std::collections::BTreeMap::from([("x-project".into(), "demo".into())]),
        connect_timeout_secs: 15,
        allow_remote: true,
    };

    let mut config = base(AcpTransport::StreamableHttp);
    config.allow_remote = false;
    assert!(
        engine.add_acp_agent_config(config).await.is_err(),
        "remote without allow_remote must fail"
    );

    let mut config = base(AcpTransport::StreamableHttp);
    config.command = "npx".into();
    assert!(engine.add_acp_agent_config(config).await.is_err());

    let mut config = base(AcpTransport::StreamableHttp);
    config.endpoint = Some("https://user:pass@agent.example.test/acp".into());
    assert!(engine.add_acp_agent_config(config).await.is_err());

    let mut config = base(AcpTransport::StreamableHttp);
    config.endpoint = Some("wss://agent.example.test/acp".into());
    assert!(engine.add_acp_agent_config(config).await.is_err());

    let mut config = base(AcpTransport::StreamableHttp);
    config.headers =
        std::collections::BTreeMap::from([("authorization".into(), "Bearer inline-secret".into())]);
    assert!(engine.add_acp_agent_config(config).await.is_err());

    let mut config = base(AcpTransport::StreamableHttp);
    config.auth_header_env = Some("INVALID-NAME".into());
    assert!(engine.add_acp_agent_config(config).await.is_err());

    let mut config = base(AcpTransport::Stdio);
    config.name = "local".into();
    config.command = "npx".into();
    config.endpoint = None;
    config.auth_header_env = None;
    config.headers.clear();
    config.allow_remote = false;
    let stdio = engine.add_acp_agent_config(config).await.unwrap();
    assert_eq!(stdio.transport, AcpTransport::Stdio);
    assert_eq!(stdio.connect_timeout_secs, 30);

    let mut config = base(AcpTransport::Websocket);
    config.name = "remote-ws".into();
    config.endpoint = Some("wss://agent.example.test/acp".into());
    let remote = engine.add_acp_agent_config(config).await.unwrap();
    assert_eq!(remote.transport, AcpTransport::Websocket);
    assert_eq!(
        remote.endpoint.as_deref(),
        Some("wss://agent.example.test/acp")
    );
    assert!(remote.allow_remote);
    assert_eq!(engine.list_acp_agents().await.unwrap().agents.len(), 2);
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

fn checkpoint_ref_exists(project_root: &Path, session_id: &str, checkpoint_id: &str) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(project_root)
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/verlinpark/checkpoints/{session_id}/{checkpoint_id}"),
        ])
        .stdout(std::process::Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[tokio::test]
async fn checkpoint_delete_hides_it_and_removes_the_snapshot_ref() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("delete".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "delete source".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(worktree.join("snapshot.txt"), "dirty\n").unwrap();
    let checkpoint = engine
        .checkpoint_session(&session.id, Some("delete me".into()))
        .await
        .unwrap();
    assert!(checkpoint_ref_exists(
        repo.path(),
        &session.id,
        &checkpoint.id
    ));

    let deleted = engine
        .delete_checkpoint(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert!(deleted.removed);
    assert!(
        engine
            .list_checkpoints(&session.id)
            .await
            .unwrap()
            .checkpoints
            .is_empty()
    );
    assert!(!checkpoint_ref_exists(
        repo.path(),
        &session.id,
        &checkpoint.id
    ));
    let error = engine
        .fork_session(&session.id, Some(&checkpoint.id), None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("checkpoint not found"));
    assert!(
        !engine
            .delete_checkpoint(&session.id, &checkpoint.id)
            .await
            .unwrap()
            .removed
    );

    engine
        .update_session_status(&session.id, "imported")
        .await
        .unwrap();
    assert!(
        engine
            .delete_checkpoint(&session.id, &checkpoint.id)
            .await
            .unwrap_err()
            .to_string()
            .contains("read-only")
    );
    assert!(
        engine
            .prune_checkpoints(&session.id, None, Some(1), false)
            .await
            .unwrap_err()
            .to_string()
            .contains("read-only")
    );
}

#[tokio::test]
async fn checkpoint_prune_keeps_the_latest_and_can_dry_run() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("prune".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "prune source".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    let mut checkpoints = Vec::new();
    for index in 0..3 {
        std::fs::write(worktree.join("snapshot.txt"), format!("dirty {index}\n")).unwrap();
        checkpoints.push(
            engine
                .checkpoint_session(&session.id, Some(format!("checkpoint {index}")))
                .await
                .unwrap(),
        );
    }
    let dry = engine
        .prune_checkpoints(&session.id, None, Some(1), true)
        .await
        .unwrap();
    assert_eq!(dry.candidates, 2);
    assert_eq!(dry.removed, 0);
    assert_eq!(
        engine
            .list_checkpoints(&session.id)
            .await
            .unwrap()
            .checkpoints
            .len(),
        3
    );

    let pruned = engine
        .prune_checkpoints(&session.id, None, Some(1), false)
        .await
        .unwrap();
    assert_eq!(pruned.candidates, 2);
    assert_eq!(pruned.removed, 2);
    let remaining = engine
        .list_checkpoints(&session.id)
        .await
        .unwrap()
        .checkpoints;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].id, checkpoints[2].id);
    assert!(!checkpoint_ref_exists(
        repo.path(),
        &session.id,
        &checkpoints[0].id
    ));
    assert!(!checkpoint_ref_exists(
        repo.path(),
        &session.id,
        &checkpoints[1].id
    ));
    assert!(checkpoint_ref_exists(
        repo.path(),
        &session.id,
        &checkpoints[2].id
    ));
}

#[tokio::test]
async fn checkpoint_policy_round_trips_and_sweeps_old_snapshots() {
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
    let session = engine
        .create_session(project.id.clone(), "policy source".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    for index in 0..2 {
        std::fs::write(worktree.join("snapshot.txt"), format!("dirty {index}\n")).unwrap();
        engine
            .checkpoint_session(&session.id, Some(format!("checkpoint {index}")))
            .await
            .unwrap();
    }
    assert_eq!(
        engine.get_checkpoint_policy(&project.id).await.unwrap(),
        CheckpointRetentionPolicy::default()
    );
    let policy = CheckpointRetentionPolicy {
        enabled: true,
        older_than_secs: 0,
        keep_latest: 1,
    };
    let saved = engine
        .set_checkpoint_policy(&project.id, &policy)
        .await
        .unwrap();
    assert_eq!(saved.policy, policy);
    assert_eq!(engine.sweep_checkpoint_retention().await.unwrap(), 1);
    let remaining = engine
        .list_checkpoints(&session.id)
        .await
        .unwrap()
        .checkpoints;
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].label, "checkpoint 1");

    let error = engine
        .set_checkpoint_policy(
            &project.id,
            &CheckpointRetentionPolicy {
                enabled: true,
                older_than_secs: 366 * 86_400,
                keep_latest: 5,
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("365 days"));
    let error = engine
        .set_checkpoint_policy(
            &project.id,
            &CheckpointRetentionPolicy {
                enabled: true,
                older_than_secs: 86_400,
                keep_latest: 101,
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("keep_latest"));
}

#[tokio::test]
async fn session_lineage_reports_parent_siblings_and_children() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("lineage".into()),
        )
        .await
        .unwrap();
    let source = engine
        .create_session(project.id, "lineage root".into(), None)
        .await
        .unwrap();
    let first = engine
        .fork_session(&source.id, None, Some("first fork".into()))
        .await
        .unwrap();
    let second = engine
        .fork_session(&source.id, None, Some("second fork".into()))
        .await
        .unwrap();

    let root = engine.session_lineage(&source.id).await.unwrap();
    assert!(root.parent.is_none());
    assert!(root.siblings.is_empty());
    assert_eq!(root.children.len(), 2);
    assert!(root.children.iter().all(|entry| entry.commit.is_some()));
    assert!(
        root.children
            .iter()
            .any(|entry| entry.session.id == first.session.id)
    );
    assert!(
        root.children
            .iter()
            .any(|entry| entry.session.id == second.session.id)
    );

    let fork = engine.session_lineage(&first.session.id).await.unwrap();
    assert_eq!(
        fork.parent
            .as_ref()
            .map(|parent| parent.session.id.as_str()),
        Some(source.id.as_str())
    );
    assert_eq!(fork.siblings.len(), 1);
    assert_eq!(fork.siblings[0].session.id, second.session.id);
    assert!(fork.children.is_empty());
}

#[tokio::test]
async fn session_compare_reports_committed_branch_difference() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("compare".into()),
        )
        .await
        .unwrap();
    let source = engine
        .create_session(project.id, "compare source".into(), None)
        .await
        .unwrap();
    let fork = engine
        .fork_session(&source.id, None, Some("compare fork".into()))
        .await
        .unwrap();
    let fork_worktree = PathBuf::from(fork.session.worktree.clone().unwrap());
    commit_worktree_file(&fork_worktree, "fork-change.txt", "from the fork\n");

    let comparison = engine
        .compare_sessions(&source.id, &fork.session.id)
        .await
        .unwrap();
    assert_eq!(comparison.session_id, source.id);
    assert_eq!(comparison.other_session_id, fork.session.id);
    assert_ne!(comparison.left_commit, comparison.right_commit);
    assert!(
        comparison
            .files
            .iter()
            .any(|file| file.path == "fork-change.txt" && file.additions > 0)
    );
    assert!(comparison.patch.contains("fork-change.txt"));
    assert!(!comparison.truncated);
    let error = engine
        .compare_sessions(&source.id, &source.id)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("different sessions"));
}

#[tokio::test]
async fn session_fork_tree_recurses_through_grandchildren() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("tree".into()),
        )
        .await
        .unwrap();
    let source = engine
        .create_session(project.id, "tree root".into(), None)
        .await
        .unwrap();
    let first = engine
        .fork_session(&source.id, None, Some("first child".into()))
        .await
        .unwrap();
    let grandchild = engine
        .fork_session(&first.session.id, None, Some("grandchild".into()))
        .await
        .unwrap();
    let second = engine
        .fork_session(&source.id, None, Some("second child".into()))
        .await
        .unwrap();

    let tree = engine.session_fork_tree(&source.id).await.unwrap();
    assert_eq!(tree.total, 4);
    assert!(!tree.truncated);
    assert_eq!(tree.root.children.len(), 2);
    let first_node = tree
        .root
        .children
        .iter()
        .find(|node| node.session.id == first.session.id)
        .expect("first fork in tree");
    assert_eq!(first_node.children.len(), 1);
    assert_eq!(first_node.children[0].session.id, grandchild.session.id);
    assert!(
        tree.root
            .children
            .iter()
            .any(|node| node.session.id == second.session.id)
    );
}

#[tokio::test]
async fn project_fork_forest_includes_all_roots_and_descendants() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("forest".into()),
        )
        .await
        .unwrap();
    let source = engine
        .create_session(project.id.clone(), "forest root".into(), None)
        .await
        .unwrap();
    let first = engine
        .fork_session(&source.id, None, Some("first".into()))
        .await
        .unwrap();
    let _grandchild = engine
        .fork_session(&first.session.id, None, Some("grandchild".into()))
        .await
        .unwrap();
    let second = engine
        .fork_session(&source.id, None, Some("second".into()))
        .await
        .unwrap();
    let unrelated = engine
        .create_session(project.id, "unrelated root".into(), None)
        .await
        .unwrap();

    let forest = engine
        .project_fork_forest(&source.project_id)
        .await
        .unwrap();
    assert_eq!(forest.total, 5);
    assert_eq!(forest.roots.len(), 2);
    assert!(forest.checkpoints >= 3);
    assert!(!forest.truncated);
    let source_node = forest
        .roots
        .iter()
        .find(|node| node.session.id == source.id)
        .expect("source root");
    assert_eq!(source_node.children.len(), 2);
    assert!(
        source_node
            .children
            .iter()
            .any(|node| node.session.id == first.session.id && node.children.len() == 1)
    );
    assert!(
        forest
            .roots
            .iter()
            .any(|node| node.session.id == second.session.id)
            || source_node
                .children
                .iter()
                .any(|node| node.session.id == second.session.id)
    );
    assert!(
        forest
            .roots
            .iter()
            .any(|node| node.session.id == unrelated.id)
    );
}

#[tokio::test]
async fn fork_audit_detects_missing_checkpoint_ref_and_repairs_it() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("audit".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "audit source".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(worktree.join("snapshot.txt"), "dirty\n").unwrap();
    let checkpoint = engine
        .checkpoint_session(&session.id, Some("audit checkpoint".into()))
        .await
        .unwrap();
    crate::worktree::delete_checkpoint_ref(repo.path(), &session.id, &checkpoint.id).unwrap();
    assert!(!crate::worktree::checkpoint_ref_exists(
        repo.path(),
        &session.id,
        &checkpoint.id
    ));

    let audit = engine.audit_project_forks(&project.id).await.unwrap();
    assert!(audit.issues.iter().any(|issue| {
        issue.kind == "missing_checkpoint_ref"
            && issue.repairable
            && issue.checkpoint_id.as_deref() == Some(checkpoint.id.as_str())
    }));
    let repair = engine.repair_project_forks(&project.id).await.unwrap();
    assert_eq!(repair.repaired, 1);
    assert_eq!(repair.failed, 0);
    assert!(crate::worktree::checkpoint_ref_exists(
        repo.path(),
        &session.id,
        &checkpoint.id
    ));
    let audit = engine.audit_project_forks(&project.id).await.unwrap();
    assert!(
        !audit
            .issues
            .iter()
            .any(|issue| issue.kind == "missing_checkpoint_ref")
    );
}

#[tokio::test]
async fn model_presets_round_trip_and_validate() {
    let data = TempDir::new().unwrap();
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let preset = engine
        .create_model_preset(vp_protocol::types::CreateModelPresetParams {
            name: "Local 8k".into(),
            provider: "openai-compatible".into(),
            model: "local-8k".into(),
            context_window: Some(8_000),
            input_usd_per_million: Some(0.0),
            output_usd_per_million: Some(0.0),
            notes: "local".into(),
        })
        .await
        .unwrap();
    assert!(!preset.id.is_empty());
    let listed = engine.list_model_presets().await.unwrap();
    assert_eq!(listed.presets.len(), 1);
    assert_eq!(listed.presets[0].model, "local-8k");

    let updated = engine
        .update_model_preset(vp_protocol::types::UpdateModelPresetParams {
            id: preset.id.clone(),
            name: "Local 16k".into(),
            provider: "openai-compatible".into(),
            model: "local-16k".into(),
            context_window: Some(16_000),
            input_usd_per_million: Some(0.5),
            output_usd_per_million: Some(1.5),
            notes: "updated".into(),
        })
        .await
        .unwrap();
    assert_eq!(updated.name, "Local 16k");
    assert_eq!(updated.context_window, Some(16_000));

    let error = engine
        .create_model_preset(vp_protocol::types::CreateModelPresetParams {
            name: "bad".into(),
            provider: "other".into(),
            model: "bad".into(),
            context_window: None,
            input_usd_per_million: None,
            output_usd_per_million: None,
            notes: String::new(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("provider"));
    let error = engine
        .create_model_preset(vp_protocol::types::CreateModelPresetParams {
            name: "bad price".into(),
            provider: "anthropic".into(),
            model: "claude".into(),
            context_window: Some(200_000),
            input_usd_per_million: Some(-1.0),
            output_usd_per_million: None,
            notes: String::new(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("prices"));

    assert!(engine.delete_model_preset(&preset.id).await.unwrap());
    assert!(
        engine
            .list_model_presets()
            .await
            .unwrap()
            .presets
            .is_empty()
    );
    assert!(!engine.delete_model_preset(&preset.id).await.unwrap());
}

#[tokio::test]
async fn checkpoint_stats_report_dirty_refs_and_snapshot_bytes() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("stats".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "stats source".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(worktree.join("stats.txt"), "some snapshot bytes\n").unwrap();
    engine
        .checkpoint_session(&session.id, Some("stats checkpoint".into()))
        .await
        .unwrap();
    let stats = engine.checkpoint_stats(&session.id).await.unwrap();
    assert_eq!(stats.checkpoints, 1);
    assert_eq!(stats.dirty, 1);
    assert_eq!(stats.refs, 1);
    assert!(stats.commit_bytes > 0);
    assert!(stats.snapshot_bytes > 0);
    assert!(stats.unique_blobs > 0);
}

#[tokio::test]
async fn fork_audit_removes_a_stale_ref_for_a_deleted_checkpoint() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("stale".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "stale source".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(worktree.join("snapshot.txt"), "dirty\n").unwrap();
    let checkpoint = engine
        .checkpoint_session(&session.id, Some("stale checkpoint".into()))
        .await
        .unwrap();
    engine
        .delete_checkpoint(&session.id, &checkpoint.id)
        .await
        .unwrap();
    crate::worktree::ensure_checkpoint_ref(
        repo.path(),
        &session.id,
        &checkpoint.id,
        &checkpoint.commit,
    )
    .unwrap();
    let audit = engine.audit_project_forks(&project.id).await.unwrap();
    assert!(audit.issues.iter().any(|issue| {
        issue.kind == "stale_checkpoint_ref"
            && issue.repairable
            && issue.checkpoint_id.as_deref() == Some(checkpoint.id.as_str())
    }));
    let repair = engine.repair_project_forks(&project.id).await.unwrap();
    assert!(repair.repaired >= 1);
    assert!(!crate::worktree::checkpoint_ref_exists(
        repo.path(),
        &session.id,
        &checkpoint.id
    ));
}

#[tokio::test]
async fn checkpoint_restore_resets_the_source_worktree_branch() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("restore".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "restore source".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().unwrap());
    std::fs::write(worktree.join("restore.txt"), "checkpoint state\n").unwrap();
    let checkpoint = engine
        .checkpoint_session(&session.id, Some("restore point".into()))
        .await
        .unwrap();
    commit_worktree_file(&worktree, "later.txt", "later state\n");
    std::fs::write(worktree.join("restore.txt"), "later state\n").unwrap();
    commit_worktree_file(&worktree, "restore.txt", "later state\n");
    let previous = crate::worktree::head_commit(&worktree).unwrap();
    assert_ne!(previous, checkpoint.commit);

    let preview = engine
        .diff_checkpoint(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert_eq!(preview.current_commit, previous);
    assert_eq!(preview.checkpoint_commit, checkpoint.commit);
    assert_eq!(preview.current_branch, session.branch.clone().unwrap());
    assert!(preview.clean);
    assert!(!preview.files.is_empty());
    assert!(!preview.patch.is_empty());
    assert!(!preview.truncated);
    assert!(
        preview
            .files
            .iter()
            .any(|file| file.path == "restore.txt" || file.path == "later.txt")
    );

    let result = engine
        .restore_checkpoint(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert_eq!(result.previous_commit, previous);
    assert_eq!(result.restored_commit, checkpoint.commit);
    assert_eq!(
        crate::worktree::head_commit(&worktree).unwrap(),
        checkpoint.commit
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("restore.txt"))
            .unwrap()
            .trim(),
        "checkpoint state"
    );
    assert!(!worktree.join("later.txt").exists());
    std::fs::write(worktree.join("untracked.txt"), "dirty\n").unwrap();
    let dirty_preview = engine
        .diff_checkpoint(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert!(!dirty_preview.clean);
    let dirty = engine
        .restore_checkpoint(&session.id, &checkpoint.id)
        .await
        .unwrap_err();
    assert!(dirty.to_string().contains("uncommitted changes"));
    std::fs::remove_file(worktree.join("untracked.txt")).unwrap();
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "session.checkpoint.restored"
            && event.payload["checkpoint_id"] == checkpoint.id
    }));
}

#[tokio::test]
async fn dashboard_snapshot_aggregates_pending_work_and_filters_by_project() {
    let data = TempDir::new().unwrap();
    let repo_a = TempDir::new().unwrap();
    let repo_b = TempDir::new().unwrap();
    init_repo(repo_a.path());
    init_repo(repo_b.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project_a = engine
        .add_project(
            repo_a.path().to_string_lossy().to_string(),
            Some("alpha".into()),
        )
        .await
        .unwrap();
    let project_b = engine
        .add_project(
            repo_b.path().to_string_lossy().to_string(),
            Some("beta".into()),
        )
        .await
        .unwrap();
    let session_a = engine
        .create_session(project_a.id.clone(), "alpha session".into(), None)
        .await
        .unwrap();
    let session_b = engine
        .create_session(project_b.id.clone(), "beta session".into(), None)
        .await
        .unwrap();
    let (_approval, _approval_rx) = engine
        .request_approval(
            &session_a.id,
            "call-1",
            "write_file",
            serde_json::json!({"path": "README.md"}),
            None,
        )
        .await
        .unwrap();
    let (_question, _question_rx) = engine
        .request_question(
            &session_a.id,
            "agent-1",
            "Leader",
            "Proceed?",
            vec!["yes".into()],
        )
        .await
        .unwrap();
    let worktree_a = PathBuf::from(session_a.worktree.clone().unwrap());
    std::fs::write(worktree_a.join("checkpoint.txt"), "snapshot\n").unwrap();
    let checkpoint = engine
        .checkpoint_session(&session_a.id, Some("dashboard".into()))
        .await
        .unwrap();

    let all = engine.dashboard_snapshot(None).await.unwrap();
    assert_eq!(all.summary.projects, 2);
    assert_eq!(all.summary.sessions, 2);
    assert_eq!(all.summary.pending_approvals, 1);
    assert_eq!(all.summary.pending_questions, 1);
    assert_eq!(all.summary.checkpoints, 1);
    assert_eq!(all.summary.dirty_checkpoints, 1);
    assert_eq!(all.summary.fork_roots, 2);
    assert_eq!(all.summary.fork_sessions, 0);
    assert_eq!(all.summary.project_summaries.len(), 2);
    let alpha = all
        .summary
        .project_summaries
        .iter()
        .find(|project| project.project_id == project_a.id)
        .unwrap();
    assert_eq!(alpha.sessions, 1);
    assert_eq!(alpha.pending_approvals, 1);
    assert_eq!(alpha.pending_questions, 1);
    assert_eq!(alpha.checkpoints, 1);
    assert_eq!(alpha.dirty_checkpoints, 1);
    assert_eq!(alpha.fork_roots, 1);
    assert_eq!(alpha.fork_sessions, 0);
    assert_eq!(all.summary.project_summaries[0].project_id, project_a.id);
    assert!(!all.alerts_truncated);
    assert!(all.alerts.iter().any(|alert| alert.kind == "approval"));
    assert!(all.alerts.iter().any(|alert| alert.kind == "question"));
    assert_eq!(all.summary.sessions_by_status.len(), 1);

    let only_b = engine
        .dashboard_snapshot(Some(&project_b.id))
        .await
        .unwrap();
    assert_eq!(only_b.summary.projects, 1);
    assert_eq!(only_b.summary.sessions, 1);
    assert_eq!(only_b.summary.pending_approvals, 0);
    assert_eq!(only_b.summary.pending_questions, 0);
    assert_eq!(only_b.summary.checkpoints, 0);
    assert_eq!(only_b.summary.project_summaries.len(), 1);
    assert_eq!(only_b.summary.project_summaries[0].project_id, project_b.id);
    assert_eq!(only_b.summary.project_summaries[0].sessions, 1);
    assert!(only_b.alerts.is_empty());
    assert_eq!(only_b.summary.sessions_by_status[0].label, "draft");
    assert!(
        engine
            .dashboard_snapshot(Some("missing-project"))
            .await
            .is_err()
    );

    let session_events = engine
        .list_events(session_b.id.clone(), 0, 10)
        .await
        .unwrap();
    assert!(
        session_events
            .iter()
            .any(|event| event.kind == "session.created")
    );
    assert_eq!(checkpoint.session_id, session_a.id);
}

#[tokio::test]
async fn mcp_server_transport_configs_are_validated_and_persisted() {
    let data = TempDir::new().unwrap();
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let server = engine
        .add_mcp_server(vp_protocol::types::McpServerConfig {
            name: "remote".into(),
            transport: vp_protocol::types::McpTransport::Http,
            command: String::new(),
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            url: Some("https://mcp.example.test/acp".into()),
            headers: std::collections::BTreeMap::from([(
                "Authorization".into(),
                "Bearer ${MCP_TOKEN}".into(),
            )]),
            enabled: true,
        })
        .await
        .unwrap();
    assert_eq!(server.transport, vp_protocol::types::McpTransport::Http);
    assert_eq!(
        server.headers.get("authorization").map(String::as_str),
        Some("Bearer ${MCP_TOKEN}")
    );
    let listed = engine.list_mcp_servers().await.unwrap();
    assert_eq!(listed.servers.len(), 1);
    assert_eq!(
        listed.servers[0].transport,
        vp_protocol::types::McpTransport::Http
    );

    let invalid = engine
        .add_mcp_server(vp_protocol::types::McpServerConfig {
            name: "bad".into(),
            transport: vp_protocol::types::McpTransport::Stdio,
            command: "runner".into(),
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            url: Some("https://mcp.example.test".into()),
            headers: std::collections::BTreeMap::new(),
            enabled: true,
        })
        .await;
    assert!(invalid.is_err());

    let updated = engine
        .update_mcp_server(
            &server.id,
            vp_protocol::types::McpServerConfig {
                name: "remote-sse".into(),
                transport: vp_protocol::types::McpTransport::Sse,
                command: String::new(),
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                url: Some("https://mcp.example.test/sse".into()),
                headers: std::collections::BTreeMap::new(),
                enabled: false,
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.transport, vp_protocol::types::McpTransport::Sse);
    assert!(!updated.enabled);
    assert!(engine.remove_mcp_server(&server.id).await.unwrap());
}

#[tokio::test]
async fn steering_targets_a_running_leader_orders_and_replays() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("steering".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "steer".into(), None)
        .await
        .unwrap();
    let leader_id = engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .update_agent_status(&leader_id, "idle")
        .await
        .unwrap();

    let error = engine
        .steer_session(&session.id, "too early", None, None, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no running Leader"));
    engine
        .update_agent_status(&leader_id, "running")
        .await
        .unwrap();

    let first = engine
        .steer_session(&session.id, "first instruction", None, None, None)
        .await
        .unwrap()
        .steering;
    let second = engine
        .steer_session(
            &session.id,
            "second instruction",
            Some(&leader_id),
            None,
            None,
        )
        .await
        .unwrap()
        .steering;
    let listed = engine
        .session_steering(&session.id, Some("pending"), Some(100))
        .await
        .unwrap();
    assert_eq!(
        listed
            .steering
            .iter()
            .map(|message| message.id.clone())
            .collect::<Vec<_>>(),
        vec![first.id.clone(), second.id.clone()]
    );

    let delivered = engine
        .deliver_pending_steering(&session.id, &leader_id, None, true)
        .await
        .unwrap();
    assert_eq!(delivered.len(), 2);
    assert_eq!(delivered[0].steering.id, first.id);
    assert_eq!(delivered[1].steering.id, second.id);
    assert!(delivered[0].content.starts_with("[Operator steering #"));
    assert!(delivered[0].content.contains("first instruction"));
    assert!(delivered[0].message.is_some());
    let messages = engine.list_messages(&session.id).await.unwrap();
    assert!(messages.iter().any(|message| message.role == "user"
        && message.content.starts_with("[Operator steering #")
        && message.content.contains("first instruction")));
    let listed = engine
        .session_steering(&session.id, None, Some(100))
        .await
        .unwrap();
    assert!(
        listed
            .steering
            .iter()
            .all(|message| message.status == "delivered")
    );

    let events = engine
        .list_events(session.id.clone(), 0, 500)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .filter(|event| event.kind == "steering.delivered")
            .count()
            == 2
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.steering_matches);
    assert!(replay.consistent, "{replay:?}");

    engine
        .store()
        .replace_steering_messages(&session.id, &[])
        .unwrap();
    let drifted = engine.replay_session(&session.id).await.unwrap();
    assert!(!drifted.steering_matches);
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.steering_matches);
    assert!(repaired.consistent, "{repaired:?}");
    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.steering.len(), 2);
}

#[tokio::test]
async fn steering_rejects_wrong_targets_scopes_tasks_and_cleans_up_runs() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("steering-scope".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "scope".into(), None)
        .await
        .unwrap();
    let other = engine
        .create_session(project.id.clone(), "other".into(), None)
        .await
        .unwrap();
    let leader_id = engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .update_agent_status(&leader_id, "running")
        .await
        .unwrap();
    let worker = engine
        .create_agent(&session.id, "worker", "Worker One", None, None, "test")
        .await
        .unwrap();
    engine
        .set_agent_task(&worker.id, Some("T1".into()), Some("impl".into()))
        .await
        .unwrap();
    engine
        .update_agent_status(&worker.id, "running")
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &orchestration_test_plan())
        .await
        .unwrap();

    let error = engine
        .steer_session(&other.id, "wrong session", Some(&leader_id), None, None)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not in this session"));
    let error = engine
        .steer_session(
            &session.id,
            "wrong task",
            Some(&worker.id),
            Some("T2"),
            None,
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not working on task T2"));

    let cancelled = engine
        .steer_session(&session.id, "cancel me", None, None, None)
        .await
        .unwrap()
        .steering;
    let cancelled = engine.cancel_steering(&cancelled.id).await.unwrap();
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.steering.status, "cancelled");
    assert!(
        engine
            .cancel_steering(&cancelled.steering.id)
            .await
            .is_err()
    );

    let scoped = engine
        .steer_session(&session.id, "task only", Some(&worker.id), Some("T1"), None)
        .await
        .unwrap()
        .steering;
    assert!(
        engine
            .deliver_pending_steering(&session.id, &worker.id, Some("T2"), false)
            .await
            .unwrap()
            .is_empty()
    );
    let delivered = engine
        .deliver_pending_steering(&session.id, &worker.id, Some("T1"), false)
        .await
        .unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].steering.id, scoped.id);
    assert!(delivered[0].message.is_none());

    let pending = engine
        .steer_session(&session.id, "run is ending", None, None, None)
        .await
        .unwrap()
        .steering;
    let cancelled = engine
        .cancel_pending_steering_for_agent(&session.id, &leader_id, "run ended")
        .await
        .unwrap();
    assert_eq!(cancelled, 1);
    let listed = engine
        .session_steering(&session.id, None, Some(100))
        .await
        .unwrap();
    let pending = listed
        .steering
        .iter()
        .find(|message| message.id == pending.id)
        .unwrap();
    assert_eq!(pending.status, "cancelled");
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.steering_matches, "{replay:?}");
}

#[tokio::test]
async fn plan_task_stall_sweep_is_observability_only() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("stalls".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "stall".into(), None)
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &orchestration_test_plan())
        .await
        .unwrap();
    let queue = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap();
    let task = queue
        .tasks
        .iter()
        .find(|task| task.task_id == "T1")
        .unwrap()
        .clone();
    assert_eq!(queue.progress.len(), 0);
    engine
        .store()
        .claim_plan_tasks(
            &session.id,
            task.plan_seq,
            std::slice::from_ref(&task.task_id),
        )
        .unwrap();
    let progress = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap()
        .progress;
    assert_eq!(progress.len(), 1);
    assert_eq!(progress[0].stall_count, 0);

    let threshold = Duration::from_millis(200);
    let mut live_events = engine.subscribe();
    assert_eq!(engine.sweep_task_stalls(threshold).await.unwrap(), 0);
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(engine.sweep_task_stalls(threshold).await.unwrap(), 1);
    let broadcast = tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            let event = live_events.recv().await.unwrap();
            if event.kind == "orchestration.stall" {
                return event;
            }
        }
    })
    .await
    .expect("stall event was not broadcast to live subscribers");
    assert_eq!(broadcast.payload["task_id"], "T1");
    assert_eq!(broadcast.payload["stall_count"], 1);
    let stalls = engine.session_stalls(&session.id).await.unwrap();
    assert_eq!(stalls.stalls.len(), 1);
    assert_eq!(stalls.stalls[0].task_id, "T1");
    assert_eq!(stalls.stalls[0].stall_count, 1);
    assert!(!stalls.stalls[0].last_progress_at.is_empty());
    // The observation timestamp was refreshed, so the same window is quiet.
    assert_eq!(engine.sweep_task_stalls(threshold).await.unwrap(), 0);

    // A real progress observation resets the window: no second event yet.
    engine
        .touch_task_progress(&session.id, Some("T1"), 2, Some("read_file"))
        .await
        .unwrap();
    assert_eq!(engine.sweep_task_stalls(threshold).await.unwrap(), 0);
    let progress = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap()
        .progress;
    assert_eq!(progress[0].last_turn, Some(2));
    assert_eq!(progress[0].last_tool.as_deref(), Some("read_file"));
    assert_eq!(progress[0].stall_count, 1);

    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(engine.sweep_task_stalls(threshold).await.unwrap(), 1);
    let progress = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap()
        .progress;
    assert_eq!(progress[0].stall_count, 2);

    // Terminal tasks never produce a stall observation.
    engine
        .store()
        .finish_plan_task(&session.id, task.plan_seq, &task.task_id, "completed", None)
        .unwrap();
    let before = engine
        .recent_events(&session.id, 1_000)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "orchestration.stall")
        .count();
    tokio::time::sleep(Duration::from_millis(250)).await;
    assert_eq!(engine.sweep_task_stalls(threshold).await.unwrap(), 0);
    let after = engine
        .recent_events(&session.id, 1_000)
        .await
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == "orchestration.stall")
        .count();
    assert_eq!(before, after);

    assert_eq!(parse_task_stall_threshold(Some("0")), 0);
    assert_eq!(parse_task_stall_threshold(Some("bad")), 1_800);
    assert_eq!(parse_task_stall_threshold(None), 1_800);
    assert_eq!(engine.sweep_task_stalls(Duration::ZERO).await.unwrap(), 0);
}

async fn stall_recovery_fixture(
    policy: OrchestrationPolicy,
    token_budget: Option<u64>,
) -> (
    TempDir,
    TempDir,
    Engine,
    vp_protocol::types::Project,
    Session,
    ScheduledTask,
) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("stall recovery".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "recover stalls".into(), token_budget)
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &orchestration_test_plan())
        .await
        .unwrap();
    engine
        .set_orchestration_policy(&project.id, &policy)
        .await
        .unwrap();
    let queue = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap();
    let task = queue
        .tasks
        .iter()
        .find(|task| task.task_id == "T1")
        .unwrap()
        .clone();
    let (claimed, _events) = engine
        .scheduler()
        .claim(
            &session.id,
            task.plan_seq,
            std::slice::from_ref(&task.task_id),
        )
        .await
        .unwrap();
    assert_eq!(claimed.len(), 1);
    (data, repo, engine, project, session, task)
}

fn recovery_policy(mode: vp_protocol::types::StallRecoveryMode) -> OrchestrationPolicy {
    OrchestrationPolicy {
        stall_recovery: mode,
        stall_min_observations: 1,
        stall_max_attempts: 1,
        stall_grace_secs: 300,
        ..Default::default()
    }
}

#[tokio::test]
async fn stall_recovery_observe_mutates_no_recovery_state() {
    let (_data, _repo, engine, _project, session, task) =
        stall_recovery_fixture(OrchestrationPolicy::default(), None).await;
    let now = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    assert_eq!(
        engine
            .sweep_task_stalls_at(Duration::from_secs(60), now)
            .await
            .unwrap(),
        1
    );
    let recoveries = engine.orchestration_recoveries(&session.id).await.unwrap();
    assert!(recoveries.recoveries.is_empty());
    let events = engine.recent_events(&session.id, 1_000).await.unwrap();
    assert!(
        events
            .iter()
            .all(|event| !event.kind.starts_with("orchestration.recovery_"))
    );
    assert!(
        engine
            .coordination_notes(&session.id, 100)
            .await
            .unwrap()
            .iter()
            .all(|note| note.action != "escalate")
    );
    assert_eq!(
        engine
            .store()
            .plan_task_status(&session.id, &task.task_id)
            .unwrap()
            .as_deref(),
        Some("running")
    );
}

#[tokio::test]
async fn stall_recovery_escalate_is_once_per_stall_window() {
    let (_data, _repo, engine, project, session, _task) = stall_recovery_fixture(
        recovery_policy(vp_protocol::types::StallRecoveryMode::Escalate),
        None,
    )
    .await;
    let now = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    assert_eq!(
        engine
            .sweep_task_stalls_at(Duration::from_secs(60), now)
            .await
            .unwrap(),
        1
    );
    let recovered = engine
        .orchestration_recoveries(&session.id)
        .await
        .unwrap()
        .recoveries;
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].status, "escalated");
    assert_eq!(recovered[0].action, "escalate");
    assert_eq!(recovered[0].last_stall_count, 1);

    let stall = engine
        .session_stalls(&session.id)
        .await
        .unwrap()
        .stalls
        .into_iter()
        .find(|stall| stall.task_id == "T1")
        .unwrap();
    assert!(
        engine
            .store()
            .escalate_stall_recovery(&stall, "duplicate same window")
            .unwrap()
            .is_none()
    );

    let later = now + time::Duration::hours(1);
    assert_eq!(
        engine
            .sweep_task_stalls_at(Duration::from_secs(60), later)
            .await
            .unwrap(),
        1
    );
    let recovered = engine
        .orchestration_recoveries(&session.id)
        .await
        .unwrap()
        .recoveries;
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].last_stall_count, 2);
    let events = engine.recent_events(&session.id, 1_000).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "orchestration.recovery_escalated")
            .count(),
        2
    );
    assert_eq!(
        engine
            .coordination_notes(&session.id, 100)
            .await
            .unwrap()
            .iter()
            .filter(|note| note.action == "escalate")
            .count(),
        2
    );
    assert_eq!(
        engine
            .store()
            .plan_task_status(&session.id, "T1")
            .unwrap()
            .as_deref(),
        Some("running")
    );
    let overview = engine
        .orchestration_overview(Some(&project.id), 10)
        .await
        .unwrap();
    assert_eq!(overview.totals.recoveries, 1);
    assert_eq!(overview.sessions[0].recoveries, 1);
    assert!(
        overview.sessions[0]
            .last_recovery
            .as_deref()
            .is_some_and(|detail| detail.contains("escalated"))
    );
    let snapshot = engine.orchestration_snapshot(&session.id).await.unwrap();
    assert_eq!(snapshot.recoveries.len(), 1);
    assert_eq!(
        snapshot
            .tasks
            .iter()
            .find(|task| task.task_id == "T1")
            .and_then(|task| task.recovery.as_ref())
            .map(|recovery| recovery.status.as_str()),
        Some("escalated")
    );
}

#[tokio::test]
async fn stall_recovery_retry_persists_intent_and_requests_cooperative_cancel() {
    let (_data, _repo, engine, _project, session, task) = stall_recovery_fixture(
        recovery_policy(vp_protocol::types::StallRecoveryMode::Retry),
        None,
    )
    .await;
    let now = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    assert_eq!(
        engine
            .sweep_task_stalls_at(Duration::from_secs(60), now)
            .await
            .unwrap(),
        1
    );
    let recovered = engine
        .orchestration_recoveries(&session.id)
        .await
        .unwrap()
        .recoveries;
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].status, "waiting_cancel");
    assert_eq!(recovered[0].action, "retry");
    assert_eq!(recovered[0].attempts, 1);
    assert!(recovered[0].next_action_at.is_some());
    assert_eq!(
        engine
            .store()
            .plan_task_status(&session.id, &task.task_id)
            .unwrap()
            .as_deref(),
        Some("cancelling")
    );
    let events = engine.recent_events(&session.id, 1_000).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "orchestration.recovery_requested")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "orchestration.recovery_waiting_cancel")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "plan_task.cancelling")
            .count(),
        1
    );

    let stall = engine
        .session_stalls(&session.id)
        .await
        .unwrap()
        .stalls
        .into_iter()
        .find(|stall| stall.task_id == "T1")
        .unwrap();
    assert!(
        engine
            .store()
            .begin_stall_retry_recovery(&stall, 1, 300, "duplicate")
            .unwrap()
            .is_none()
    );
    let recovered = engine
        .orchestration_recoveries(&session.id)
        .await
        .unwrap()
        .recoveries;
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].attempts, 1);
}

#[tokio::test]
async fn stall_recovery_exhaustion_fails_instead_of_looping() {
    let (_data, _repo, engine, _project, session, task) = stall_recovery_fixture(
        recovery_policy(vp_protocol::types::StallRecoveryMode::Retry),
        None,
    )
    .await;
    let seed = TaskStall {
        session_id: session.id.clone(),
        project_id: session.project_id.clone(),
        plan_seq: task.plan_seq,
        task_id: task.task_id.clone(),
        department: task.department.clone(),
        title: task.title.clone(),
        status: "running".to_string(),
        attempts: task.attempts,
        age_secs: 600,
        stall_count: 0,
        last_progress_at: task.started_at.clone().unwrap_or_default(),
        last_turn: None,
        last_tool: None,
        observed_at: now_rfc3339(),
    };
    assert!(
        engine
            .store()
            .begin_stall_retry_recovery(&seed, 1, 300, "seed recovery attempt")
            .unwrap()
            .is_some()
    );
    assert!(
        engine
            .store()
            .complete_stall_recovery_retry(&session.id, task.plan_seq, &task.task_id)
            .unwrap()
            .is_some()
    );
    let now = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    assert_eq!(
        engine
            .sweep_task_stalls_at(Duration::from_secs(60), now)
            .await
            .unwrap(),
        1
    );
    let recoveries = engine
        .orchestration_recoveries(&session.id)
        .await
        .unwrap()
        .recoveries;
    assert_eq!(recoveries.len(), 1);
    assert_eq!(recoveries[0].status, "failed");
    assert_eq!(recoveries[0].attempts, 1);
    let events = engine.recent_events(&session.id, 1_000).await.unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "orchestration.recovery_failed")
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "plan_task.cancelling")
            .count(),
        0
    );
    assert!(
        engine
            .coordination_notes(&session.id, 100)
            .await
            .unwrap()
            .iter()
            .any(|note| note.action == "escalate")
    );
}

#[tokio::test]
async fn stall_recovery_replay_repair_and_bundle_round_trip() {
    let (_data, _repo, engine, _project, session, _task) = stall_recovery_fixture(
        recovery_policy(vp_protocol::types::StallRecoveryMode::Escalate),
        None,
    )
    .await;
    engine
        .update_session_status(&session.id, "idle")
        .await
        .unwrap();
    let now = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    assert_eq!(
        engine
            .sweep_task_stalls_at(Duration::from_secs(60), now)
            .await
            .unwrap(),
        1
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.recoveries_match, "{replay:?}");

    engine
        .store()
        .replace_orchestration_recoveries(&session.id, &[])
        .unwrap();
    let drifted = engine.replay_session(&session.id).await.unwrap();
    assert!(!drifted.recoveries_match, "{drifted:?}");
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.recoveries_match, "{repaired:?}");

    let mut bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.recoveries.len(), 1);
    assert_eq!(bundle.recoveries[0].status, "escalated");
    // Import rebuilds the projection from the event stream even when an
    // archive carries an empty recovery list.
    bundle.recoveries.clear();

    let imported_data = TempDir::new().unwrap();
    let imported_engine = Engine::new(Store::open(imported_data.path()).unwrap());
    let imported = imported_engine.import_session(bundle).await.unwrap();
    assert_eq!(imported.session_id, session.id);
    let imported_recoveries = imported_engine
        .orchestration_recoveries(&session.id)
        .await
        .unwrap();
    assert_eq!(imported_recoveries.recoveries.len(), 1);
    assert_eq!(imported_recoveries.recoveries[0].status, "escalated");
    let imported_replay = imported_engine.replay_session(&session.id).await.unwrap();
    assert!(imported_replay.recoveries_match, "{imported_replay:?}");
}

fn orchestration_test_plan() -> TeamPlan {
    use vp_protocol::types::{PlanDepartment, PlanTask};
    TeamPlan {
        goal: "ship orchestration".into(),
        departments: vec![PlanDepartment {
            name: "impl".into(),
            manager_title: "Implementation".into(),
            profile: "coding".into(),
            objective: "build".into(),
            token_budget: None,
            cost_budget_usd: None,
        }],
        tasks: vec![
            PlanTask {
                id: "T1".into(),
                department: "impl".into(),
                title: "Foundation".into(),
                kind: "coding".into(),
                priority: 10,
                profile: None,
                deps: vec![],
                write_scope: vec!["src/one".into()],
                acceptance: vec!["done".into()],
                token_budget: None,
                cost_budget_usd: None,
            },
            PlanTask {
                id: "T2".into(),
                department: "impl".into(),
                title: "Follow-up".into(),
                kind: "coding".into(),
                priority: 0,
                profile: None,
                deps: vec!["T1".into()],
                write_scope: vec!["src/two".into()],
                acceptance: vec!["done".into()],
                token_budget: None,
                cost_budget_usd: None,
            },
        ],
    }
}

#[tokio::test]
async fn orchestration_advance_schedules_a_durable_retry_once() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("orch".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "orchestrate".into(), None)
        .await
        .unwrap();
    let plan = orchestration_test_plan();
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    let queue = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap()
        .tasks;
    let plan_seq = queue[0].plan_seq;
    engine
        .scheduler()
        .claim(&session.id, plan_seq, &["T1".into()])
        .await
        .unwrap();
    engine
        .scheduler()
        .finish(&session.id, plan_seq, "T1", "failed", Some("compile error"))
        .await
        .unwrap();

    let preview = engine.preview_orchestration(&session.id).await.unwrap();
    assert!(
        preview
            .actions
            .iter()
            .any(|action| action.action == "retry" && action.task_id == "T1")
    );
    let untouched = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap()
        .tasks;
    assert_eq!(
        untouched
            .iter()
            .find(|task| task.task_id == "T1")
            .unwrap()
            .status,
        "failed"
    );

    let decisions = engine.advance_orchestration(&session.id).await.unwrap();
    assert!(
        decisions
            .iter()
            .any(|decision| decision.action == "retry" && decision.task_id.as_deref() == Some("T1"))
    );
    let queue = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap()
        .tasks;
    let t1 = queue.iter().find(|task| task.task_id == "T1").unwrap();
    assert_eq!(t1.status, "pending");
    assert!(t1.next_attempt_at.is_some());

    let again = engine.advance_orchestration(&session.id).await.unwrap();
    assert!(
        !again
            .iter()
            .any(|decision| decision.action == "retry" && decision.task_id.as_deref() == Some("T1"))
    );
    let events = engine.recent_events(&session.id, 100).await.unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "plan_task.retry_scheduled")
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "orchestration.decision")
    );
}

#[tokio::test]
async fn orchestration_exhaustion_blocks_dependents_and_escalates() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("orch".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "orchestrate".into(), None)
        .await
        .unwrap();
    let mut policy = engine.orchestration_policy(&project.id).await.unwrap();
    policy.max_attempts = 1;
    policy.retry_backoff_secs = 0;
    engine
        .set_orchestration_policy(&project.id, &policy)
        .await
        .unwrap();
    let plan = orchestration_test_plan();
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    let queue = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap()
        .tasks;
    let plan_seq = queue[0].plan_seq;
    engine
        .scheduler()
        .claim(&session.id, plan_seq, &["T1".into()])
        .await
        .unwrap();
    engine
        .scheduler()
        .finish(&session.id, plan_seq, "T1", "failed", Some("fatal"))
        .await
        .unwrap();

    let decisions = engine.advance_orchestration(&session.id).await.unwrap();
    assert!(
        decisions
            .iter()
            .any(|decision| decision.action == "escalate")
    );
    assert!(decisions.iter().any(|decision| decision.action == "block"));
    let queue = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap()
        .tasks;
    assert_eq!(
        queue
            .iter()
            .find(|task| task.task_id == "T2")
            .unwrap()
            .status,
        "blocked"
    );
    let snapshot = engine.orchestration_snapshot(&session.id).await.unwrap();
    assert!(snapshot.metrics.blocked_tasks >= 1);
    assert!(
        snapshot
            .blockers
            .iter()
            .any(|blocker| blocker.kind == "task_failed")
    );
    assert!(
        snapshot
            .notes
            .iter()
            .any(|note| note.action == "escalate" && note.task_id.as_deref() == Some("T1"))
    );
}

#[tokio::test]
async fn plan_revision_records_a_diff_and_rejects_protected_tasks() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("orch".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "revise".into(), None)
        .await
        .unwrap();
    let plan = orchestration_test_plan();
    engine.set_team_plan(&session.id, &plan).await.unwrap();
    let queue = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap()
        .tasks;
    let plan_seq = queue[0].plan_seq;
    engine
        .scheduler()
        .finish(&session.id, plan_seq, "T1", "completed", Some("landed"))
        .await
        .unwrap();

    let mut unsafe_plan = plan.clone();
    unsafe_plan.tasks.retain(|task| task.id != "T1");
    let error = engine
        .revise_team_plan(
            &session.id,
            unsafe_plan,
            "remove old work",
            "leader",
            None,
            5,
        )
        .await
        .unwrap_err();
    assert!(!error.to_string().is_empty());

    let mut revised = plan.clone();
    revised.tasks[1].title = "Follow-up revised".into();
    revised.tasks.push(vp_protocol::types::PlanTask {
        id: "T3".into(),
        department: "impl".into(),
        title: "New verification".into(),
        kind: "verify".into(),
        priority: 5,
        profile: None,
        deps: vec!["T2".into()],
        write_scope: vec![],
        acceptance: vec!["tests pass".into()],
        token_budget: None,
        cost_budget_usd: None,
    });
    let result = engine
        .revise_team_plan(
            &session.id,
            revised,
            "add verification",
            "leader",
            Some(1),
            5,
        )
        .await
        .unwrap();
    assert_eq!(result.revision.revision, 2);
    assert_eq!(result.revision.changed, vec!["T2"]);
    assert_eq!(result.revision.added, vec!["T3"]);
    let revisions = engine.plan_revisions(&session.id).await.unwrap();
    assert_eq!(revisions.len(), 2);
    assert_eq!(revisions[1].reason.as_deref(), Some("add verification"));
    let conflict = engine
        .revise_team_plan(&session.id, plan, "stale revision", "leader", Some(1), 5)
        .await
        .unwrap_err();
    assert!(conflict.to_string().contains("revision conflict"));
}

#[tokio::test]
async fn session_leases_serialize_two_daemons_and_release_cleanly() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let first = Engine::new(store.clone());
    let second = Engine::new(store);
    let project = first
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("leases".into()),
        )
        .await
        .unwrap();
    let session = first
        .create_session(project.id.clone(), "lease me".into(), None)
        .await
        .unwrap();

    let lease = first
        .acquire_session_lease(&session.id, std::time::Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(lease.session_id, session.id);
    let blocked = second
        .acquire_session_lease(&session.id, std::time::Duration::from_secs(60))
        .await;
    let error = match blocked {
        Ok(_) => panic!("second daemon acquired a held session lease"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("already running"));
    let leases = first.list_runtime_leases().await.unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].owner_id, first.runtime_id());

    lease.release().await;
    let leases = first.list_runtime_leases().await.unwrap();
    assert!(leases.is_empty());
    let second_lease = second
        .acquire_session_lease(&session.id, std::time::Duration::from_secs(60))
        .await
        .unwrap();
    second_lease.release().await;
    let events = first.recent_events(&session.id, 100).await.unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "session.lease.acquired")
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "session.lease.released")
    );
}

#[tokio::test]
async fn computer_leases_serialize_browser_surfaces_and_release_cleanly() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let first = Engine::new(store.clone());
    let second = Engine::new(store);
    let project = first
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("computer lease".into()),
        )
        .await
        .unwrap();
    let session = first
        .create_session(project.id, "browser lease".into(), None)
        .await
        .unwrap();
    let lease = first
        .acquire_computer_lease(
            "browser",
            &session.id,
            "agent-a",
            "Agent A",
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(lease.surface(), "browser");
    assert_eq!(lease.lease.owner_id, first.runtime_id());

    let blocked = second
        .acquire_computer_lease(
            "browser",
            &session.id,
            "agent-b",
            "Agent B",
            std::time::Duration::from_secs(60),
        )
        .await;
    let error = match blocked {
        Ok(_) => panic!("second engine acquired a held browser lease"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("busy"));
    let leases = first.list_computer_leases().await.unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].session_id, session.id);
    assert_eq!(leases[0].agent_name, "Agent A");

    lease.release().await;
    assert!(first.list_computer_leases().await.unwrap().is_empty());
    let second_lease = second
        .acquire_computer_lease(
            "browser",
            &session.id,
            "agent-b",
            "Agent B",
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(second_lease.lease.agent_name, "Agent B");
    assert!(
        first
            .release_computer_lease(second_lease.lease_id())
            .await
            .unwrap()
    );
    assert!(first.list_computer_leases().await.unwrap().is_empty());

    let events = first.recent_events(&session.id, 100).await.unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "computer.lease.acquired")
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "computer.lease.denied")
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "computer.lease.released")
    );
}

#[tokio::test]
async fn expired_computer_leases_are_swept_by_the_engine() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("computer sweep".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "expire".into(), None)
        .await
        .unwrap();
    let attempt = store
        .acquire_computer_lease("browser", "owner-a", &session.id, "agent-a", "Agent A", 0)
        .unwrap();
    match attempt {
        vp_store::store::ComputerLeaseAttempt::Acquired { .. } => {}
        vp_store::store::ComputerLeaseAttempt::Busy { .. } => panic!("lease was busy"),
    }
    let removed = engine.sweep_computer_leases().await.unwrap();
    assert_eq!(removed, 1);
    assert!(engine.list_computer_leases().await.unwrap().is_empty());
    let events = engine.recent_events(&session.id, 50).await.unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "computer.lease.expired")
    );
}

#[tokio::test]
#[ignore = "requires the built macOS WKWebView helper and a logged-in GUI session"]
async fn browser_helper_opens_local_page_and_saves_screenshot() {
    use axum::Router;
    use axum::routing::get;

    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("browser helper".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "browser smoke".into(), None)
        .await
        .unwrap();
    let worktree = PathBuf::from(session.worktree.clone().expect("session worktree"));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new().route(
        "/",
        get(|| async {
            axum::response::Html(
                "<!doctype html><html><head><title>Browser Test</title></head><body>\
                 <h1 id=\"title\">Browser Test</h1>\
                 <button id=\"press\" onclick=\"document.getElementById('title').textContent='Clicked'\">Press</button>\
                 </body></html>",
            )
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let opened = engine
        .browser_action(
            &session.id,
            &worktree,
            "agent-browser",
            "Agent Browser",
            serde_json::json!({"action": "open", "url": format!("http://{address}/")}),
        )
        .await
        .unwrap();
    assert!(!opened.is_error, "{}", opened.content);
    let snapshot = engine
        .browser_action(
            &session.id,
            &worktree,
            "agent-browser",
            "Agent Browser",
            serde_json::json!({"action": "snapshot"}),
        )
        .await
        .unwrap();
    assert!(!snapshot.is_error, "{}", snapshot.content);
    assert!(
        snapshot.content.contains("Browser Test"),
        "{}",
        snapshot.content
    );
    assert!(
        snapshot.content.contains("[e1] button \"Press\""),
        "{}",
        snapshot.content
    );

    let clicked = engine
        .browser_action(
            &session.id,
            &worktree,
            "agent-browser",
            "Agent Browser",
            serde_json::json!({"action": "click", "selector": "#press"}),
        )
        .await
        .unwrap();
    assert!(!clicked.is_error, "{}", clicked.content);
    let after_click = engine
        .browser_action(
            &session.id,
            &worktree,
            "agent-browser",
            "Agent Browser",
            serde_json::json!({"action": "snapshot"}),
        )
        .await
        .unwrap();
    assert!(
        after_click.content.contains("Clicked"),
        "{}",
        after_click.content
    );

    let evaluated = engine
        .browser_action(
            &session.id,
            &worktree,
            "agent-browser",
            "Agent Browser",
            serde_json::json!({
                "action": "evaluate",
                "script": "console.warn('browser smoke'); return document.title;",
            }),
        )
        .await
        .unwrap();
    assert!(!evaluated.is_error, "{}", evaluated.content);
    assert!(
        evaluated.content.contains("Browser Test"),
        "{}",
        evaluated.content
    );
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let console = engine
        .browser_action(
            &session.id,
            &worktree,
            "agent-browser",
            "Agent Browser",
            serde_json::json!({"action": "console"}),
        )
        .await
        .unwrap();
    assert!(
        console.content.contains("browser smoke"),
        "{}",
        console.content
    );
    let waited = engine
        .browser_action(
            &session.id,
            &worktree,
            "agent-browser",
            "Agent Browser",
            serde_json::json!({
                "action": "wait_for",
                "condition": "selector",
                "value": "#title",
            }),
        )
        .await
        .unwrap();
    assert!(!waited.is_error, "{}", waited.content);

    let missing = engine
        .browser_action(
            &session.id,
            &worktree,
            "agent-browser",
            "Agent Browser",
            serde_json::json!({"action": "click", "selector": "#missing"}),
        )
        .await
        .unwrap();
    assert!(missing.is_error);
    let still_open = engine
        .browser_action(
            &session.id,
            &worktree,
            "agent-browser",
            "Agent Browser",
            serde_json::json!({"action": "snapshot"}),
        )
        .await
        .unwrap();
    assert!(!still_open.is_error, "{}", still_open.content);

    let screenshot = engine
        .browser_action(
            &session.id,
            &worktree,
            "agent-browser",
            "Agent Browser",
            serde_json::json!({"action": "screenshot", "path": "browser/smoke.png"}),
        )
        .await
        .unwrap();
    assert!(!screenshot.is_error, "{}", screenshot.content);
    assert!(worktree.join("browser/smoke.png").is_file());

    let closed = engine
        .browser_action(
            &session.id,
            &worktree,
            "agent-browser",
            "Agent Browser",
            serde_json::json!({"action": "close"}),
        )
        .await
        .unwrap();
    assert!(!closed.is_error, "{}", closed.content);
    assert!(engine.list_computer_leases().await.unwrap().is_empty());
    assert!(
        !data
            .path()
            .join("sandbox")
            .join(&session.id)
            .join("browser-helper")
            .exists()
    );
    server.abort();
}

#[tokio::test]
async fn coordination_and_decision_projections_replay_and_repair() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let project = engine
        .add_project(
            repo.path().to_str().unwrap().to_string(),
            Some("replay".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "coordination replay".into(), None)
        .await
        .unwrap();
    let event = engine
        .record_coordination_note(
            &session.id,
            "blocker",
            "warning",
            Some("T1"),
            None,
            None,
            "agent-1",
            "Worker One",
            Some("worker"),
            "schema is unclear",
        )
        .await
        .unwrap();
    let note_id = event.payload["note_id"].as_str().unwrap().to_string();
    engine
        .record_orchestration_decision(&session.id, "escalate", Some("T1"), "manual test", 1)
        .await
        .unwrap();

    let notes = engine.coordination_notes(&session.id, 10).await.unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].note_id, note_id);
    assert_eq!(notes[0].status, "open");
    let decisions = engine
        .orchestration_decisions(&session.id, 10)
        .await
        .unwrap();
    assert_eq!(decisions.len(), 1);
    let snapshot = engine.orchestration_snapshot(&session.id).await.unwrap();
    assert_eq!(snapshot.notes.len(), 1);
    assert_eq!(snapshot.decisions.len(), 1);

    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);

    // Tamper with the derived projections; replay must notice and repair must
    // restore them from the event stream.
    store
        .replace_coordination_notes(&session.id, &[], &[])
        .unwrap();
    store
        .replace_orchestration_decisions(&session.id, &[], &[])
        .unwrap();
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(!report.consistent);
    assert!(
        report
            .mismatches
            .iter()
            .any(|mismatch| mismatch.contains("coordination notes"))
    );
    assert!(
        report
            .mismatches
            .iter()
            .any(|mismatch| mismatch.contains("orchestration decisions"))
    );
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.consistent, "{:?}", repaired.mismatches);
    assert_eq!(
        engine
            .coordination_notes(&session.id, 10)
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        engine
            .orchestration_decisions(&session.id, 10)
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn revision_detail_task_edit_and_restore_are_optimistic() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("revisions".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "revise".into(), None)
        .await
        .unwrap();
    let plan = orchestration_test_plan();
    engine.set_team_plan(&session.id, &plan).await.unwrap();

    let first = engine.plan_revision_detail(&session.id, 1).await.unwrap();
    assert_eq!(first.revision.revision, 1);
    assert_eq!(first.plan.tasks.len(), 2);
    assert_eq!(first.plan.tasks[0].id, "T1");

    let mut edited = first.plan.tasks[1].clone();
    edited.title = "Follow-up edited".into();
    edited.priority = 42;
    edited.deps = vec![];
    let edit = engine
        .edit_plan_task(&session.id, edited, "raise priority", Some(1), 5)
        .await
        .unwrap();
    assert_eq!(edit.revision.revision, 2);
    assert_eq!(edit.revision.changed, vec!["T2"]);
    let second = engine.plan_revision_detail(&session.id, 2).await.unwrap();
    assert_eq!(second.plan.tasks[1].title, "Follow-up edited");
    assert_eq!(second.plan.tasks[1].priority, 42);
    assert!(second.plan.tasks[1].deps.is_empty());

    let restored = engine
        .restore_team_plan_revision(&session.id, 1, "restore the original DAG", Some(2), 5)
        .await
        .unwrap();
    assert_eq!(restored.revision.revision, 3);
    assert_eq!(restored.revision.changed, vec!["T2"]);
    assert_eq!(restored.plan.tasks[1].title, "Follow-up");
    assert_eq!(restored.plan.tasks[1].deps, vec!["T1"]);

    // A stale expected revision is rejected before mutation.
    let error = engine
        .restore_team_plan_revision(&session.id, 1, "stale", Some(2), 5)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("revision conflict"));
    let revisions = engine.plan_revisions(&session.id).await.unwrap();
    assert_eq!(revisions.len(), 3);
    assert_eq!(
        revisions[2].reason.as_deref(),
        Some("restore the original DAG")
    );
}

#[tokio::test]
async fn targeted_coordination_notes_reach_the_agent_mailbox() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("coord".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "coordinate".into(), None)
        .await
        .unwrap();
    let worker = engine
        .create_agent(
            &session.id,
            "worker",
            "Worker One",
            Some("Implementation".into()),
            None,
            "test-model",
        )
        .await
        .unwrap();
    let note = engine
        .record_coordination_note(
            &session.id,
            "handoff",
            "info",
            Some("T1"),
            Some(&worker.id),
            None,
            "leader",
            "Leader",
            Some("leader"),
            "T1 is ready; start with the parser.",
        )
        .await
        .unwrap();
    let note_id = note.payload["note_id"].as_str().unwrap();
    let messages = engine
        .list_agent_messages(&session.id, Some(worker.id.clone()), true)
        .await
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].kind, "coordination");
    assert_eq!(messages[0].body, "T1 is ready; start with the parser.");
    assert_eq!(messages[0].payload["note_id"].as_str(), Some(note_id));
    let notes = engine
        .coordination_notes_for_agent(&session.id, &worker.id, Some("T1"), 10)
        .await
        .unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(
        notes[0].target_agent_id.as_deref(),
        Some(worker.id.as_str())
    );

    // A global handoff is visible to every agent; an unrelated note is not.
    engine
        .record_coordination_note(
            &session.id,
            "handoff",
            "info",
            None,
            None,
            None,
            "manager-1",
            "Manager One",
            Some("manager"),
            "The API contract is frozen.",
        )
        .await
        .unwrap();
    engine
        .record_coordination_note(
            &session.id,
            "note",
            "info",
            Some("T2"),
            None,
            None,
            "worker-2",
            "Worker Two",
            Some("worker"),
            "Unrelated local note.",
        )
        .await
        .unwrap();
    let notes = engine
        .coordination_notes_for_agent(&session.id, &worker.id, Some("T1"), 10)
        .await
        .unwrap();
    assert_eq!(notes.len(), 2);
    assert!(
        notes
            .iter()
            .any(|note| note.message.contains("API contract"))
    );
    assert!(!notes.iter().any(|note| note.message.contains("Unrelated")));
}

#[tokio::test]
async fn orchestration_overview_ranks_attention_sessions() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("overview".into()),
        )
        .await
        .unwrap();
    let calm = engine
        .create_session(project.id.clone(), "calm plan".into(), None)
        .await
        .unwrap();
    let stressed = engine
        .create_session(project.id.clone(), "stressed plan".into(), None)
        .await
        .unwrap();
    let plan = orchestration_test_plan();
    engine.set_team_plan(&calm.id, &plan).await.unwrap();
    engine.set_team_plan(&stressed.id, &plan).await.unwrap();
    let queue = engine
        .task_queue(None, Some(stressed.id.clone()))
        .await
        .unwrap()
        .tasks;
    let plan_seq = queue[0].plan_seq;
    engine
        .scheduler()
        .claim(&stressed.id, plan_seq, &["T1".into()])
        .await
        .unwrap();
    engine
        .scheduler()
        .finish(&stressed.id, plan_seq, "T1", "failed", Some("boom"))
        .await
        .unwrap();
    engine.advance_orchestration(&stressed.id).await.unwrap();

    let overview = engine
        .orchestration_overview(Some(&project.id), 10)
        .await
        .unwrap();
    assert_eq!(overview.totals.sessions, 2);
    assert_eq!(overview.sessions[0].session_id, stressed.id);
    assert!(overview.sessions[0].metrics.retrying_tasks >= 1);
    assert!(overview.sessions[0].last_decision.is_some());
    assert_eq!(overview.totals.retrying_tasks, 1);
    assert!(overview.sessions[1].metrics.retrying_tasks == 0);
}

#[tokio::test]
async fn approval_grants_match_paths_and_subfolders() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("grants".into()),
        )
        .await
        .unwrap();
    engine
        .create_approval_grant(&project.id, "write_file", "src/feature", true, "user")
        .await
        .unwrap();
    let matching = serde_json::json!({"path": "src/feature/lib.rs"});
    let sibling = serde_json::json!({"path": "src/other/lib.rs"});
    assert!(
        engine
            .approval_grant_matches(&project.id, "write_file", &matching)
            .await
            .unwrap()
    );
    assert!(
        !engine
            .approval_grant_matches(&project.id, "write_file", &sibling)
            .await
            .unwrap()
    );
    assert!(
        !engine
            .approval_grant_matches(&project.id, "edit_file", &matching)
            .await
            .unwrap()
    );
    assert!(
        !engine
            .approval_grant_matches(
                &project.id,
                "write_file",
                &serde_json::json!({"command": "ls"})
            )
            .await
            .unwrap()
    );
}

#[tokio::test]
async fn integration_runtime_leases_serialize_merge_work() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let first = Engine::new(store.clone());
    let second = Engine::new(store);
    let project = first
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("leases".into()),
        )
        .await
        .unwrap();
    let session = first
        .create_session(project.id, "integration lease".into(), None)
        .await
        .unwrap();
    let lease = first
        .acquire_runtime_lease(
            "integration",
            &session.id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(lease.kind, "integration");
    let blocked = second
        .acquire_runtime_lease(
            "integration",
            &session.id,
            std::time::Duration::from_secs(60),
        )
        .await;
    let error = match blocked {
        Ok(_) => panic!("second engine acquired the same integration lease"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("busy"));
    let leases = first.list_runtime_leases().await.unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].kind, "integration");
    assert_eq!(leases[0].session_id.as_deref(), Some(session.id.as_str()));
    lease.release().await;
    let second_lease = second
        .acquire_runtime_lease(
            "integration",
            &session.id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    second_lease.release().await;
}

#[tokio::test]
async fn intake_validation_and_one_task_plan_are_deterministic() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("intake".into()),
        )
        .await
        .unwrap();

    let bad_scope = IntakeCreateParams {
        project_id: project.id.clone(),
        title: "Escape".into(),
        instructions: "write outside".into(),
        profile: Some("coding".into()),
        kind: Some("coding".into()),
        priority: None,
        write_scope: vec!["../etc".into()],
        acceptance: Vec::new(),
        deps: Vec::new(),
        token_budget: None,
        cost_budget_usd: None,
        time_budget_secs: None,
        max_attempts: None,
        labels: Vec::new(),
        origin: Some("test".into()),
    };
    assert!(engine.create_intake_task(bad_scope).await.is_err());

    let bad_profile = IntakeCreateParams {
        project_id: project.id.clone(),
        title: "Read only coding".into(),
        instructions: "do it".into(),
        profile: Some("verification".into()),
        kind: Some("coding".into()),
        priority: None,
        write_scope: Vec::new(),
        acceptance: Vec::new(),
        deps: Vec::new(),
        token_budget: None,
        cost_budget_usd: None,
        time_budget_secs: None,
        max_attempts: None,
        labels: Vec::new(),
        origin: None,
    };
    assert!(engine.create_intake_task(bad_profile).await.is_err());

    let task = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Review the release notes".into(),
            instructions: "Read the docs and return a concise review.".into(),
            profile: Some("research".into()),
            kind: Some("research".into()),
            priority: Some(9),
            write_scope: Vec::new(),
            acceptance: vec!["the review cites concrete sections".into()],
            deps: Vec::new(),
            token_budget: Some(30_000),
            cost_budget_usd: Some(2.0),
            time_budget_secs: Some(900),
            max_attempts: Some(3),
            labels: vec!["docs".into()],
            origin: Some("api".into()),
        })
        .await
        .unwrap();
    assert_eq!(task.status, "queued");
    assert_eq!(task.priority, 9);
    assert_eq!(task.profile, "research");
    let plan = engine.intake_plan(&task);
    crate::agent::validate_team_plan(&plan, 5).unwrap();
    assert_eq!(plan.tasks.len(), 1);
    assert_eq!(plan.tasks[0].id, "T1");
    assert_eq!(plan.tasks[0].department, plan.departments[0].name);
    assert_eq!(plan.tasks[0].token_budget, Some(30_000));

    let snapshots = vp_store::store::read_project_manifest(data.path()).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].intake_tasks.len(), 1);
    assert_eq!(snapshots[0].intake_tasks[0].id, task.id);

    let parent = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Parent".into(),
            instructions: "Do the parent work.".into(),
            profile: Some("coding".into()),
            kind: Some("coding".into()),
            priority: Some(1),
            write_scope: vec!["src/parent".into()],
            acceptance: Vec::new(),
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: None,
            labels: Vec::new(),
            origin: Some("test".into()),
        })
        .await
        .unwrap();
    let missing_dep = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Missing dependency".into(),
            instructions: "Wait for a task that does not exist.".into(),
            profile: Some("coding".into()),
            kind: Some("coding".into()),
            priority: None,
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: vec!["does-not-exist".into()],
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: None,
            labels: Vec::new(),
            origin: None,
        })
        .await;
    assert!(missing_dep.is_err());
    let self_dep = IntakeUpdateParams {
        id: parent.id.clone(),
        title: parent.title.clone(),
        instructions: parent.instructions.clone(),
        profile: Some(parent.profile.clone()),
        kind: Some(parent.kind.clone()),
        priority: Some(parent.priority),
        write_scope: parent.write_scope.clone(),
        acceptance: parent.acceptance.clone(),
        deps: vec![parent.id.clone()],
        token_budget: None,
        cost_budget_usd: None,
        time_budget_secs: None,
        max_attempts: None,
        labels: Vec::new(),
    };
    assert!(engine.update_intake_task(self_dep).await.is_err());
    let child = engine
        .create_intake_task(IntakeCreateParams {
            project_id: project.id.clone(),
            title: "Child".into(),
            instructions: "Depends on the parent.".into(),
            profile: Some("coding".into()),
            kind: Some("coding".into()),
            priority: None,
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: vec![parent.id.clone()],
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: None,
            labels: Vec::new(),
            origin: None,
        })
        .await
        .unwrap();
    let cycle = IntakeUpdateParams {
        id: parent.id.clone(),
        title: parent.title.clone(),
        instructions: parent.instructions.clone(),
        profile: Some(parent.profile.clone()),
        kind: Some(parent.kind.clone()),
        priority: Some(parent.priority),
        write_scope: parent.write_scope.clone(),
        acceptance: parent.acceptance.clone(),
        deps: vec![child.id.clone()],
        token_budget: None,
        cost_budget_usd: None,
        time_budget_secs: None,
        max_attempts: None,
        labels: Vec::new(),
    };
    let error = engine.update_intake_task(cycle).await.unwrap_err();
    assert!(error.to_string().contains("cycle"));

    let error = engine
        .set_task_scheduler_policy(
            &project.id,
            TaskSchedulerPolicy {
                enabled: true,
                cooldown_secs: 5,
                dispatch_tasks: false,
                include_intake: true,
                max_intake_parallel: 0,
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("intake"));
}

#[tokio::test]
async fn sources_index_search_read_and_delete() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    std::fs::create_dir_all(repo.path().join("docs")).unwrap();
    std::fs::write(
        repo.path().join("docs/architecture.md"),
        "# Runtime\n\nThe scheduler uses deterministic leases.\n\n## Sources\n\nCitations are attached to the final answer.\n",
    )
    .unwrap();
    std::fs::write(repo.path().join("src_note.txt"), "A binary-ish placeholder").unwrap();
    std::fs::write(repo.path().join("image.bin"), b"abc\0def").unwrap();
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("knowledge".into()),
        )
        .await
        .unwrap();

    let note = engine
        .add_source_note(SourceCreateNoteParams {
            project_id: project.id.clone(),
            collection: Some("runtime".into()),
            title: "Integration notes".into(),
            content: "The integrator runs git diff --check and rolls back on failure.".into(),
            labels: vec!["runtime".into()],
        })
        .await
        .unwrap();
    assert_eq!(note.status, "indexed");
    assert!(note.chunk_count >= 1);

    let file = engine
        .add_source_file(SourceCreateFileParams {
            project_id: project.id.clone(),
            collection: Some("docs".into()),
            path: "docs/architecture.md".into(),
            title: None,
            labels: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(file.kind, "file");
    assert!(file.uri.as_deref().unwrap() == "docs/architecture.md");
    let imported = engine
        .import_sources(SourceImportParams {
            project_id: project.id.clone(),
            collection: Some("imported".into()),
            path: Some(".".into()),
            max_files: Some(10),
            labels: vec!["imported".into()],
        })
        .await
        .unwrap();
    assert_eq!(imported.imported.len(), 2);
    assert!(imported.skipped >= 1);

    let list = engine
        .list_sources(project.id.clone(), None, None, None)
        .await
        .unwrap();
    assert_eq!(list.sources.len(), 4);
    assert_eq!(list.stats.indexed, 4);

    let search = engine
        .search_sources(
            project.id.clone(),
            "deterministic leases".into(),
            Vec::new(),
            Some(5),
        )
        .await
        .unwrap();
    assert!(!search.hits.is_empty());
    assert!(search.hits.iter().any(|hit| hit.source_id == file.id));

    let page = engine
        .get_source(file.id.clone(), Some(0), Some(10))
        .await
        .unwrap();
    assert_eq!(page.source.id, file.id);
    assert!(!page.chunks.is_empty());
    assert!(page.total_chunks >= page.chunks.len());

    let brief = engine.source_brief(&project.id).await.unwrap();
    assert!(brief.contains("Integration notes"));

    let deleted = engine.delete_source(note.id.clone()).await.unwrap();
    assert!(deleted.deleted);
    assert!(
        engine
            .list_sources(project.id, None, None, None)
            .await
            .unwrap()
            .sources
            .iter()
            .all(|source| source.id != note.id)
    );
}

#[tokio::test]
async fn source_file_paths_and_oversized_content_are_rejected() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("paths".into()),
        )
        .await
        .unwrap();
    for path in ["../outside.txt", "/etc/passwd", ".git/config"] {
        let result = engine
            .add_source_file(SourceCreateFileParams {
                project_id: project.id.clone(),
                collection: None,
                path: path.into(),
                title: None,
                labels: Vec::new(),
            })
            .await;
        assert!(result.is_err(), "path should be rejected: {path}");
    }
    let error = engine
        .add_source_note(SourceCreateNoteParams {
            project_id: project.id.clone(),
            collection: None,
            title: "empty".into(),
            content: "   ".into(),
            labels: Vec::new(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("empty"));
    let error = engine
        .add_source_note(SourceCreateNoteParams {
            project_id: project.id,
            collection: None,
            title: "Bad\nTitle".into(),
            content: "content".into(),
            labels: Vec::new(),
        })
        .await
        .unwrap_err();
    assert!(error.to_string().contains("control"));
}

#[tokio::test]
async fn skills_create_import_filter_and_toggle() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    std::fs::write(
        repo.path().join("review.skill.md"),
        "---\nname: Imported Review\ndescription: Review a change\ntools: read_file, grep\nprofiles: [coding]\nroles: [leader, worker]\n---\nInspect the diff, run tests, and report evidence.\n",
    )
    .unwrap();
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("skills".into()),
        )
        .await
        .unwrap();

    let skill = engine
        .create_skill(SkillCreateParams {
            project_id: project.id.clone(),
            name: "Release Review".into(),
            description: "Review a release".into(),
            when_to_use: "Before tagging".into(),
            version: Some("1.2.0".into()),
            instructions: "Read the diff and verify the release.".into(),
            tool_allow: vec!["read_file".into()],
            tool_deny: Vec::new(),
            profiles: vec!["coding".into()],
            roles: vec!["leader".into()],
        })
        .await
        .unwrap();
    assert_eq!(skill.slug, "release-review");
    assert!(
        engine
            .skill_catalog(&project.id, "leader", "coding")
            .await
            .unwrap()
            .contains("Release Review")
    );
    assert!(
        engine
            .skill_catalog(&project.id, "worker", "coding")
            .await
            .unwrap()
            .is_empty()
    );

    let imported = engine
        .import_skill(SkillImportParams {
            project_id: project.id.clone(),
            path: "review.skill.md".into(),
        })
        .await
        .unwrap();
    assert_eq!(imported.name, "Imported Review");
    assert_eq!(imported.tool_allow, vec!["read_file", "grep"]);
    assert_eq!(imported.roles, vec!["leader", "worker"]);
    assert!(
        engine
            .import_skill(SkillImportParams {
                project_id: project.id.clone(),
                path: "review.skill.md".into(),
            })
            .await
            .is_err()
    );

    let updated = engine
        .update_skill(SkillUpdateParams {
            id: skill.id.clone(),
            name: "Release Review".into(),
            description: "Updated description".into(),
            when_to_use: "Before tagging".into(),
            version: Some("1.3.0".into()),
            instructions: "Updated instructions.".into(),
            tool_allow: Vec::new(),
            tool_deny: vec!["shell".into()],
            profiles: vec!["coding".into()],
            roles: vec!["leader".into()],
        })
        .await
        .unwrap();
    assert_eq!(updated.version, "1.3.0");
    assert_eq!(updated.tool_deny, vec!["shell"]);

    let disabled = engine
        .set_skill_enabled(SkillEnabledParams {
            id: skill.id.clone(),
            enabled: false,
        })
        .await
        .unwrap();
    assert!(!disabled.enabled);
    let catalog = engine
        .skill_catalog(&project.id, "leader", "coding")
        .await
        .unwrap();
    assert!(!catalog.contains("Release Review"));
    assert!(engine.delete_skill(skill.id).await.unwrap().deleted);
}

#[tokio::test]
async fn artifact_versions_and_restore_are_optimistic() {
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
        .create_session(project.id, "artifact versions".into(), None)
        .await
        .unwrap();
    let artifact = engine
        .publish_artifact(
            &session.id,
            "agent-1",
            "Leader",
            None,
            "Report",
            "note",
            "text/plain",
            "first",
        )
        .await
        .unwrap();
    assert_eq!(artifact.version, 1);
    let updated = engine
        .publish_artifact_version(
            &artifact.id,
            Some(1),
            "Report",
            "note",
            "text/plain",
            "second",
        )
        .await
        .unwrap();
    assert_eq!(updated.version, 2);
    assert!(
        engine
            .publish_artifact_version(
                &artifact.id,
                Some(1),
                "Report",
                "note",
                "text/plain",
                "stale",
            )
            .await
            .is_err()
    );
    let versions = engine.artifact_versions(&artifact.id).await.unwrap();
    assert_eq!(versions.versions.len(), 2);
    let restored = engine
        .restore_artifact_version(&artifact.id, 1)
        .await
        .unwrap();
    assert_eq!(restored.version, 3);
    assert_eq!(restored.content, "first");
}

#[tokio::test]
async fn source_refresh_rereads_file_content_and_bumps_revision() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let path = repo.path().join("notes.md");
    std::fs::write(&path, "# Notes\n\nFirst version mentions alpha.").unwrap();
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("refresh".into()),
        )
        .await
        .unwrap();
    let source = engine
        .add_source_file(SourceCreateFileParams {
            project_id: project.id.clone(),
            collection: Some("notes".into()),
            path: "notes.md".into(),
            title: None,
            labels: Vec::new(),
        })
        .await
        .unwrap();
    assert_eq!(source.revision, 1);
    std::fs::write(&path, "# Notes\n\nSecond version mentions beta.").unwrap();
    let refreshed = engine.refresh_source(source.id.clone()).await.unwrap();
    assert_eq!(refreshed.source.revision, 2);
    assert_eq!(refreshed.source.title, "notes.md");
    let search = engine
        .search_sources(project.id, "beta".into(), Vec::new(), Some(5))
        .await
        .unwrap();
    assert!(search.hits.iter().any(|hit| hit.source_id == source.id));
    let page = engine.get_source(source.id, None, None).await.unwrap();
    assert!(
        page.chunks
            .iter()
            .any(|chunk| chunk.content.contains("beta"))
    );
}

#[tokio::test]
async fn artifact_request_keys_are_idempotent_per_session() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("idempotent".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "artifact keys".into(), None)
        .await
        .unwrap();
    let first = engine
        .publish_artifact_keyed(
            &session.id,
            "agent-1",
            "Leader",
            None,
            "Report",
            "note",
            "text/plain",
            "first",
            "report-1",
        )
        .await
        .unwrap();
    let reused = engine
        .publish_artifact_keyed(
            &session.id,
            "agent-1",
            "Leader",
            None,
            "Report",
            "note",
            "text/plain",
            "second",
            "report-1",
        )
        .await
        .unwrap();
    assert_eq!(reused.id, first.id);
    assert_eq!(reused.content, "first");
    let other_session = engine
        .create_session(project.id, "artifact keys 2".into(), None)
        .await
        .unwrap();
    let other = engine
        .publish_artifact_keyed(
            &other_session.id,
            "agent-1",
            "Leader",
            None,
            "Report",
            "note",
            "text/plain",
            "other",
            "report-1",
        )
        .await
        .unwrap();
    assert_ne!(other.id, first.id);
    assert!(
        engine
            .publish_artifact_keyed(
                &session.id,
                "agent-1",
                "Leader",
                None,
                "Report",
                "note",
                "text/plain",
                "unsafe",
                "bad key",
            )
            .await
            .is_err()
    );
}

#[tokio::test]
async fn source_collections_metadata_and_scoped_search() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("collections".into()),
        )
        .await
        .unwrap();
    let engineering = engine
        .add_source_note(SourceCreateNoteParams {
            project_id: project.id.clone(),
            collection: Some("engineering".into()),
            title: "Runtime".into(),
            content: "The scheduler uses deterministic leases.".into(),
            labels: vec!["runtime".into()],
        })
        .await
        .unwrap();
    let research = engine
        .add_source_note(SourceCreateNoteParams {
            project_id: project.id.clone(),
            collection: Some("research".into()),
            title: "Retrieval".into(),
            content: "Deterministic retrieval supports citations.".into(),
            labels: vec!["citation".into()],
        })
        .await
        .unwrap();
    let collections = engine.source_collections(project.id.clone()).await.unwrap();
    assert_eq!(collections.total_sources, 2);
    assert_eq!(collections.collections.len(), 2);
    let engineering_group = collections
        .collections
        .iter()
        .find(|collection| collection.name.as_deref() == Some("engineering"))
        .unwrap();
    assert_eq!(engineering_group.sources, 1);
    assert_eq!(engineering_group.labels, vec!["runtime"]);

    let scoped = engine
        .search_sources_scoped(
            project.id.clone(),
            "deterministic".into(),
            Vec::new(),
            vec!["research".into()],
            Vec::new(),
            Some(8),
        )
        .await
        .unwrap();
    assert_eq!(scoped.hits.len(), 1);
    assert_eq!(scoped.hits[0].source_id, research.id);
    assert_eq!(scoped.hits[0].collection.as_deref(), Some("research"));

    let label_scoped = engine
        .search_sources_scoped(
            project.id.clone(),
            "deterministic".into(),
            Vec::new(),
            Vec::new(),
            vec!["runtime".into()],
            Some(8),
        )
        .await
        .unwrap();
    assert_eq!(label_scoped.hits.len(), 1);
    assert_eq!(label_scoped.hits[0].source_id, engineering.id);

    let moved = engine
        .set_source_collection(research.id.clone(), Some("engineering".into()))
        .await
        .unwrap();
    assert_eq!(moved.collection.as_deref(), Some("engineering"));
    let snapshot = Store::open(data.path())
        .unwrap()
        .read_source_snapshot(&project.id, &research.id)
        .unwrap()
        .unwrap();
    assert_eq!(snapshot.source.collection.as_deref(), Some("engineering"));

    let labelled = engine
        .set_source_labels(
            research.id.clone(),
            vec!["citation".into(), "verified".into()],
        )
        .await
        .unwrap();
    assert_eq!(labelled.labels, vec!["citation", "verified"]);
    let cleared = engine
        .set_source_collection(research.id.clone(), None)
        .await
        .unwrap();
    assert!(cleared.collection.is_none());
    assert!(
        engine
            .set_source_collection(engineering.id.clone(), Some("x".repeat(81)))
            .await
            .is_err()
    );
    assert!(
        engine
            .set_source_collection(engineering.id.clone(), Some("bad\ncollection".into()))
            .await
            .is_err()
    );
    assert!(
        engine
            .set_source_labels(engineering.id, vec!["bad\nlabel".into()])
            .await
            .is_err()
    );
}

#[tokio::test]
async fn artifact_diff_reports_hunks_and_rejects_unknown_versions() {
    use vp_protocol::types::ArtifactDiffParams;

    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("diff".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "artifact diff".into(), None)
        .await
        .unwrap();
    let artifact = engine
        .publish_artifact(
            &session.id,
            "agent-1",
            "Leader",
            None,
            "Report",
            "note",
            "text/plain",
            "alpha\nbeta\ngamma\n",
        )
        .await
        .unwrap();
    let updated = engine
        .publish_artifact_version(
            &artifact.id,
            Some(1),
            "Report",
            "note",
            "text/plain",
            "alpha\ndelta\ngamma\nepsilon\n",
        )
        .await
        .unwrap();
    let diff = engine
        .artifact_diff(ArtifactDiffParams {
            artifact_id: artifact.id.clone(),
            from_version: 1,
            to_version: Some(2),
            context: Some(1),
        })
        .await
        .unwrap();
    assert_eq!(diff.from_version, 1);
    assert_eq!(diff.to_version, 2);
    assert_eq!(diff.diff.additions, 2);
    assert_eq!(diff.diff.deletions, 1);
    assert!(!diff.diff.identical);
    assert!(!diff.diff.hunks.is_empty());
    assert!(
        diff.diff
            .hunks
            .iter()
            .flat_map(|hunk| &hunk.lines)
            .any(|line| line.kind == "add" && line.text.contains("epsilon"))
    );

    let current = engine
        .artifact_diff(ArtifactDiffParams {
            artifact_id: artifact.id.clone(),
            from_version: 1,
            to_version: None,
            context: None,
        })
        .await
        .unwrap();
    assert_eq!(current.to_version, updated.version);
    assert_eq!(current.diff.additions, 2);

    let identical = engine
        .artifact_diff(ArtifactDiffParams {
            artifact_id: artifact.id.clone(),
            from_version: 1,
            to_version: Some(1),
            context: None,
        })
        .await
        .unwrap();
    assert!(identical.diff.identical);

    assert!(
        engine
            .artifact_diff(ArtifactDiffParams {
                artifact_id: artifact.id,
                from_version: 9,
                to_version: None,
                context: None,
            })
            .await
            .is_err()
    );
}

#[tokio::test]
async fn citation_audit_exposes_dangling_and_unused_keys() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("audit".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "citation audit".into(), None)
        .await
        .unwrap();
    engine
        .append_event(
            &session.id,
            "agent.message",
            serde_json::json!({
                "message_id": "message-1",
                "agent_name": "Worker",
                "text": "The runtime uses leases [c9].",
                "citations": [{
                    "key": "c1",
                    "source_id": "source-1",
                    "source_title": "Runtime notes",
                    "chunk_id": "chunk-1",
                    "chunk_no": 2,
                    "heading": "Leases",
                    "excerpt": "deterministic leases",
                    "score": 2.5,
                    "origin": "search"
                }]
            }),
        )
        .await
        .unwrap();
    engine
        .append_event(
            &session.id,
            "agent.message",
            serde_json::json!({
                "message_id": "message-2",
                "text": "A summary without an evidence marker."
            }),
        )
        .await
        .unwrap();
    let audit = engine
        .session_citation_audit(&session.id)
        .await
        .unwrap()
        .audit;
    assert_eq!(audit.messages.len(), 2);
    assert_eq!(audit.messages_with_citations, 1);
    assert_eq!(audit.messages_without_citations, 1);
    assert_eq!(audit.total_citations, 1);
    assert_eq!(audit.unique_sources, 1);
    assert_eq!(audit.dangling_key_count, 1);
    assert_eq!(audit.unused_citation_count, 1);
    let first = audit
        .messages
        .iter()
        .find(|message| message.message_id == "message-1")
        .unwrap();
    assert_eq!(first.referenced_keys, vec!["c9"]);
    assert_eq!(first.dangling_keys, vec!["c9"]);
    assert_eq!(first.unused_keys, vec!["c1"]);
    let second = audit
        .messages
        .iter()
        .find(|message| message.message_id == "message-2")
        .unwrap();
    assert_eq!(second.citation_count, 0);
    assert!(second.referenced_keys.is_empty());
    assert!(second.dangling_keys.is_empty());
}

#[tokio::test]
async fn unlimited_budget_mode_is_event_backed_and_reversible() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("unlimited".into()),
        )
        .await
        .unwrap();
    let invalid = engine
        .create_session_with_options_and_unlimited(
            project.id.clone(),
            "conflicting budget".into(),
            Some(100),
            None,
            None,
            0,
            true,
        )
        .await;
    assert!(invalid.is_err());

    let session = engine
        .create_session_with_options_and_unlimited(
            project.id.clone(),
            "large project".into(),
            Some(100),
            None,
            None,
            0,
            false,
        )
        .await
        .unwrap();
    assert!(!session.unlimited);
    assert_eq!(session.token_budget, Some(100));
    let shared_flag = engine.unlimited_flag(&session.id, session.unlimited);
    assert!(!shared_flag.load(std::sync::atomic::Ordering::SeqCst));

    let toggled = engine
        .set_session_unlimited(&session.id, true)
        .await
        .unwrap();
    assert!(toggled.unlimited);
    assert!(
        shared_flag.load(std::sync::atomic::Ordering::SeqCst),
        "a live run holding the shared flag must see the toggle"
    );
    let loaded = engine.store().get_session(&session.id).unwrap().unwrap();
    assert!(loaded.unlimited);
    assert_eq!(
        loaded.token_budget,
        Some(100),
        "unlimited mode preserves stored limits for later restoration"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "session.budget_mode_changed"
            && event.payload["unlimited"] == serde_json::json!(true)
    }));
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(
        report.consistent,
        "replay mismatches: {:?}",
        report.mismatches
    );

    let restored = engine
        .set_session_unlimited(&session.id, false)
        .await
        .unwrap();
    assert!(!restored.unlimited);
    assert!(!shared_flag.load(std::sync::atomic::Ordering::SeqCst));
    let loaded = engine.store().get_session(&session.id).unwrap().unwrap();
    assert!(!loaded.unlimited);
    assert_eq!(loaded.token_budget, Some(100));
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(
        report.consistent,
        "replay mismatches: {:?}",
        report.mismatches
    );
}

#[tokio::test]
async fn deferred_continuations_validate_creation_and_replay_repair_bundle() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("continuations".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "defer work".into(), None)
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &orchestration_test_plan())
        .await
        .unwrap();

    assert!(
        engine
            .defer_session(
                &session.id,
                "   ",
                Vec::new(),
                None,
                None,
                None,
                None,
                None,
                None,
                None
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .defer_session(
                &session.id,
                &"x".repeat(4_001),
                vec!["T1".into()],
                None,
                None,
                None,
                None,
                None,
                None,
                None
            )
            .await
            .is_err()
    );
    let no_trigger = engine
        .defer_session(
            &session.id,
            "later",
            Vec::new(),
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(no_trigger.to_string().contains("watch task or a deadline"));
    let missing = engine
        .defer_session(
            &session.id,
            "later",
            vec!["T9".into()],
            Some("all"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(missing.to_string().contains("not in the stored TeamPlan"));
    let bad_mode = engine
        .defer_session(
            &session.id,
            "later",
            vec!["T1".into()],
            Some("most"),
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(bad_mode.to_string().contains("watch mode"));
    let past = (time::OffsetDateTime::now_utc() - time::Duration::hours(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    assert!(
        engine
            .defer_session(
                &session.id,
                "later",
                Vec::new(),
                None,
                Some(&past),
                None,
                None,
                None,
                None,
                None
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .defer_session(
                &session.id,
                "later",
                Vec::new(),
                None,
                None,
                Some(31 * 24 * 60 * 60),
                None,
                None,
                None,
                None
            )
            .await
            .is_err()
    );
    let tomorrow = (time::OffsetDateTime::now_utc() + time::Duration::days(1))
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap();
    assert!(
        engine
            .defer_session(
                &session.id,
                "later",
                Vec::new(),
                None,
                Some(&tomorrow),
                Some(5),
                None,
                None,
                None,
                None
            )
            .await
            .is_err()
    );

    let created = engine
        .defer_session(
            &session.id,
            "summarize T1 when it lands",
            vec!["T1".into(), "T1".into()],
            Some("all"),
            Some(&tomorrow),
            None,
            Some(9),
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    assert_eq!(created.status, "pending");
    assert_eq!(created.watch_task_ids, vec!["T1".to_string()]);
    assert_eq!(created.watch_mode, "all");
    assert_eq!(created.max_attempts, 3);
    assert_eq!(created.deadline_at.as_deref(), Some(tomorrow.as_str()));
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "session.continuation.created")
    );

    engine.skip_plan_task(&session.id, "T2").await.unwrap();
    let terminal = engine
        .defer_session(
            &session.id,
            "use the T2 result",
            vec!["T2".into()],
            None,
            None,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(terminal.to_string().contains("already skipped"));

    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);

    engine
        .store()
        .replace_session_continuations(&session.id, &[])
        .unwrap();
    let drifted = engine.replay_session(&session.id).await.unwrap();
    assert!(!drifted.continuations_match);
    assert!(!drifted.consistent);
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.continuations_match, "{:?}", repaired.mismatches);
    assert!(repaired.consistent, "{:?}", repaired.mismatches);
    let rebuilt = engine.rebuild_session(&session.id).await.unwrap();
    assert!(rebuilt.rebuilt);
    assert!(rebuilt.continuations_match, "{:?}", rebuilt.mismatches);
    assert!(rebuilt.consistent, "{:?}", rebuilt.mismatches);

    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.continuations.len(), 1);
    assert_eq!(bundle.continuations[0].id, created.id);
    assert_eq!(bundle.continuations[0].body, "summarize T1 when it lands");
    let import_data = TempDir::new().unwrap();
    let imported_engine = Engine::new(Store::open(import_data.path()).unwrap());
    imported_engine.import_session(bundle).await.unwrap();
    let imported = imported_engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap();
    assert_eq!(imported.continuations.len(), 1);
    assert_eq!(imported.continuations[0].id, created.id);
    assert_eq!(imported.continuations[0].watch_task_ids, vec!["T1"]);
}

#[test]
fn deferred_continuation_wake_rules_cover_all_any_deadline_and_missing() {
    use vp_protocol::types::{ContinuationOutcome, ScheduledTask};
    fn task(task_id: &str, status: &str) -> ScheduledTask {
        ScheduledTask {
            session_id: "session".into(),
            project_id: "project".into(),
            plan_seq: 1,
            task_id: task_id.into(),
            department: "impl".into(),
            title: format!("Task {task_id}"),
            priority: 0,
            status: status.into(),
            deps: Vec::new(),
            write_scope: Vec::new(),
            attempts: 0,
            enqueued_at: "2026-01-01T00:00:00Z".into(),
            started_at: None,
            finished_at: None,
            last_error: None,
            next_attempt_at: None,
        }
    }
    let now = time::OffsetDateTime::parse(
        "2026-01-01T00:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let deadline = Some("2026-01-02T00:00:00Z");
    let pending = vec![task("T1", "pending"), task("T2", "failed")];
    assert!(
        crate::continuation::evaluate_wake(
            &["T1".into(), "T2".into()],
            "all",
            deadline,
            &pending,
            now
        )
        .is_none()
    );
    let any = crate::continuation::evaluate_wake(
        &["T1".into(), "T2".into()],
        "any",
        deadline,
        &pending,
        now,
    )
    .unwrap();
    assert_eq!(any.reason, "watch");
    assert_eq!(
        any.outcomes,
        vec![
            ContinuationOutcome {
                task_id: "T1".into(),
                title: "Task T1".into(),
                status: "pending".into()
            },
            ContinuationOutcome {
                task_id: "T2".into(),
                title: "Task T2".into(),
                status: "failed".into()
            }
        ]
    );

    let settled = vec![task("T1", "completed"), task("T2", "interrupted")];
    assert_eq!(
        crate::continuation::evaluate_wake(
            &["T1".into(), "T2".into()],
            "all",
            deadline,
            &settled,
            now
        )
        .unwrap()
        .reason,
        "watch"
    );

    let deadline_wake =
        crate::continuation::evaluate_wake(&[], "all", Some("2025-12-31T23:59:59Z"), &[], now)
            .unwrap();
    assert_eq!(deadline_wake.reason, "deadline");
    assert!(deadline_wake.outcomes.is_empty());
    assert!(crate::continuation::evaluate_wake(&[], "all", deadline, &[], now).is_none());

    let missing =
        crate::continuation::evaluate_wake(&["T9".into()], "all", deadline, &settled, now).unwrap();
    assert_eq!(missing.reason, "watch");
    assert_eq!(missing.outcomes[0].status, "missing");
}

#[tokio::test]
async fn deferred_continuations_can_be_cancelled_once() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("continuation cancel".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "cancel continuation".into(), None)
        .await
        .unwrap();
    let continuation = engine
        .defer_session(
            &session.id,
            "wait for a deadline",
            Vec::new(),
            None,
            None,
            Some(60),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    let cancelled = engine
        .cancel_session_continuation(&continuation.id)
        .await
        .unwrap();
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.continuation.status, "cancelled");
    assert_eq!(
        cancelled.continuation.last_error.as_deref(),
        Some("cancelled by operator")
    );
    assert!(
        engine
            .cancel_session_continuation(&continuation.id)
            .await
            .is_err()
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn recurring_continuation_delivery_schedules_child_waits_for_interval_and_stops_at_max_runs()
{
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("recurring continuation".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "recurring".into(), None)
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &orchestration_test_plan())
        .await
        .unwrap();

    assert!(
        engine
            .defer_session(
                &session.id,
                "too fast",
                Vec::new(),
                None,
                None,
                Some(60),
                None,
                Some(59),
                None,
                None,
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .defer_session(
                &session.id,
                "too long",
                Vec::new(),
                None,
                None,
                Some(60),
                None,
                Some(crate::continuation::MAX_RECURRENCE_SECS + 1),
                None,
                None,
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .defer_session(
                &session.id,
                "one run",
                Vec::new(),
                None,
                None,
                Some(60),
                None,
                Some(60),
                Some(1),
                None,
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .defer_session(
                &session.id,
                "too many runs",
                Vec::new(),
                None,
                None,
                Some(60),
                None,
                Some(60),
                Some(101),
                None,
            )
            .await
            .is_err()
    );
    assert!(
        engine
            .defer_session(
                &session.id,
                "no interval",
                Vec::new(),
                None,
                None,
                Some(60),
                None,
                None,
                Some(2),
                None,
            )
            .await
            .is_err()
    );

    let created = engine
        .defer_session(
            &session.id,
            "check the build",
            vec!["T1".into()],
            None,
            None,
            None,
            None,
            Some(60),
            Some(2),
            None,
        )
        .await
        .unwrap()
        .continuation;
    assert_eq!(created.status, "pending");
    assert_eq!(created.series_id, created.id);
    assert!(created.parent_continuation_id.is_none());
    assert_eq!(created.recurrence_every_secs, Some(60));
    assert_eq!(created.recurrence_max_runs, 2);
    assert_eq!(created.run_count, 0);

    engine.skip_plan_task(&session.id, "T1").await.unwrap();
    engine
        .mark_session_continuation_ready(&created.id, "watch", &[])
        .await
        .unwrap()
        .unwrap();
    engine
        .claim_session_continuation(&created.id)
        .await
        .unwrap()
        .unwrap();
    let delivered = engine
        .mark_session_continuation_delivered(&created.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.status, "delivered");
    assert_eq!(delivered.run_count, 1);
    assert_eq!(delivered.recurrence_max_runs, 2);
    assert!(delivered.delivered_at.is_some());

    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows.len(), 2, "exactly one child was created");
    let child = rows
        .iter()
        .find(|row| row.parent_continuation_id.as_deref() == Some(created.id.as_str()))
        .expect("child continuation exists");
    assert_eq!(child.status, "pending");
    assert_eq!(child.watch_mode, "all");
    assert!(child.watch_task_ids.is_empty());
    assert_eq!(child.series_id, created.series_id);
    assert_eq!(child.run_count, 1);
    assert_eq!(child.recurrence_every_secs, Some(60));
    assert_eq!(child.recurrence_max_runs, 2);
    assert_eq!(child.body, created.body);
    let child_deadline = time::OffsetDateTime::parse(
        child.deadline_at.as_deref().unwrap(),
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let now = time::OffsetDateTime::now_utc();
    assert!(child_deadline > now + time::Duration::seconds(50));
    assert!(child_deadline <= now + time::Duration::seconds(70));

    // The original watch task is terminal, but the child has no watch ids, so
    // it can only wake on its own future deadline.
    let tasks = engine
        .task_queue(None, Some(session.id.clone()))
        .await
        .unwrap()
        .tasks;
    assert!(
        crate::continuation::evaluate_wake(
            &child.watch_task_ids,
            &child.watch_mode,
            child.deadline_at.as_deref(),
            &tasks,
            now,
        )
        .is_none()
    );

    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    let scheduled = events
        .iter()
        .filter(|event| event.kind == "session.continuation.scheduled")
        .collect::<Vec<_>>();
    assert_eq!(scheduled.len(), 1);
    let payload = scheduled[0].payload.clone();
    let scheduled_child: SessionContinuation = serde_json::from_value(payload).unwrap();
    assert_eq!(scheduled_child, *child);
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivered")
            .count(),
        1
    );
    let delivered_event = events
        .iter()
        .find(|event| event.kind == "session.continuation.delivered")
        .unwrap();
    let delivered_payload: SessionContinuation =
        serde_json::from_value(delivered_event.payload.clone()).unwrap();
    assert_eq!(delivered_payload.run_count, 1);
    assert_eq!(delivered_payload.series_id, created.series_id);

    // Repeated delivery of the same row is idempotent: no duplicate child and
    // no duplicate events.
    assert!(
        engine
            .mark_session_continuation_delivered(&created.id)
            .await
            .unwrap()
            .is_none()
    );
    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows.len(), 2);
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.scheduled")
            .count(),
        1
    );

    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);

    // The second delivered run reaches max_runs and must not schedule a third.
    engine
        .mark_session_continuation_ready(&child.id, "deadline", &[])
        .await
        .unwrap()
        .unwrap();
    engine
        .claim_session_continuation(&child.id)
        .await
        .unwrap()
        .unwrap();
    let second = engine
        .mark_session_continuation_delivered(&child.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.status, "delivered");
    assert_eq!(second.run_count, 2);
    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows.len(), 2, "max_runs stops the chain");
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.scheduled")
            .count(),
        1
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn cancelling_one_recurring_row_leaves_the_other_rows_untouched() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("recurring cancel".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "recurring cancel".into(), None)
        .await
        .unwrap();
    let created = engine
        .defer_session(
            &session.id,
            "cancel one run",
            Vec::new(),
            None,
            None,
            Some(60),
            None,
            Some(60),
            Some(3),
            None,
        )
        .await
        .unwrap()
        .continuation;
    engine
        .mark_session_continuation_ready(&created.id, "deadline", &[])
        .await
        .unwrap()
        .unwrap();
    engine
        .claim_session_continuation(&created.id)
        .await
        .unwrap()
        .unwrap();
    engine
        .mark_session_continuation_delivered(&created.id)
        .await
        .unwrap()
        .unwrap();
    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    let child = rows
        .iter()
        .find(|row| row.id != created.id)
        .unwrap()
        .clone();
    let cancelled = engine.cancel_session_continuation(&child.id).await.unwrap();
    assert!(cancelled.cancelled);
    assert_eq!(cancelled.continuation.status, "cancelled");

    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows.len(), 2);
    let parent = rows.iter().find(|row| row.id == created.id).unwrap();
    assert_eq!(parent.status, "delivered");
    assert_eq!(parent.run_count, 1);
    let child = rows.iter().find(|row| row.id == child.id).unwrap();
    assert_eq!(child.status, "cancelled");
    assert_eq!(child.run_count, 1);
    assert!(
        engine
            .store()
            .list_open_session_continuations()
            .unwrap()
            .is_empty()
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn recurring_continuation_bundle_repair_and_db_rebuild_fold_scheduled_children() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    {
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("recurring rebuild".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id, "recurring rebuild".into(), None)
            .await
            .unwrap();
        let created = engine
            .defer_session(
                &session.id,
                "recurring rebuild body",
                Vec::new(),
                None,
                None,
                Some(60),
                None,
                Some(120),
                Some(4),
                None,
            )
            .await
            .unwrap()
            .continuation;
        engine
            .mark_session_continuation_ready(&created.id, "deadline", &[])
            .await
            .unwrap()
            .unwrap();
        engine
            .claim_session_continuation(&created.id)
            .await
            .unwrap()
            .unwrap();
        engine
            .mark_session_continuation_delivered(&created.id)
            .await
            .unwrap()
            .unwrap();
        let rows = engine
            .session_continuations(&session.id, None, Some(10))
            .await
            .unwrap()
            .continuations;
        assert_eq!(rows.len(), 2);
        let child = rows
            .iter()
            .find(|row| row.id != created.id)
            .unwrap()
            .clone();
        assert_eq!(
            child.parent_continuation_id.as_deref(),
            Some(created.id.as_str())
        );

        let replay = engine.replay_session(&session.id).await.unwrap();
        assert!(replay.continuations_match, "{:?}", replay.mismatches);

        // Drop the projection and prove repair rebuilds both rows from events.
        engine
            .store()
            .replace_session_continuations(&session.id, &[])
            .unwrap();
        let drifted = engine.replay_session(&session.id).await.unwrap();
        assert!(!drifted.continuations_match);
        let repaired = engine.repair_session(&session.id).await.unwrap();
        assert!(repaired.continuations_match, "{:?}", repaired.mismatches);
        assert!(repaired.consistent, "{:?}", repaired.mismatches);
        let rebuilt = engine.rebuild_session(&session.id).await.unwrap();
        assert!(rebuilt.continuations_match, "{:?}", rebuilt.mismatches);

        let bundle = engine.export_session(&session.id).await.unwrap();
        assert_eq!(bundle.continuations.len(), 2);
        let bundled_child = bundle
            .continuations
            .iter()
            .find(|row| row.id == child.id)
            .unwrap();
        assert_eq!(bundled_child.run_count, 1);
        assert_eq!(bundled_child.recurrence_every_secs, Some(120));
        assert_eq!(bundled_child.recurrence_max_runs, 4);
        assert_eq!(bundled_child.series_id, created.series_id);

        let import_data = TempDir::new().unwrap();
        let imported_engine = Engine::new(Store::open(import_data.path()).unwrap());
        imported_engine.import_session(bundle).await.unwrap();
        let imported = imported_engine
            .session_continuations(&session.id, None, Some(10))
            .await
            .unwrap()
            .continuations;
        assert_eq!(imported.len(), 2);
        let imported_child = imported.iter().find(|row| row.id == child.id).unwrap();
        assert_eq!(
            imported_child.parent_continuation_id.as_deref(),
            Some(created.id.as_str())
        );
        assert_eq!(imported_child.run_count, 1);
        assert_eq!(imported_child.series_id, created.series_id);

        engine.record_project_manifest().await.unwrap();
    }

    let report = crate::rebuild::rebuild_database(data.path(), false)
        .await
        .unwrap();
    assert_eq!(report.projects, 1);
    assert_eq!(report.sessions, 1);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let store = Store::open(data.path()).unwrap();
    let sessions = store.list_sessions(None).unwrap();
    assert_eq!(sessions.len(), 1);
    let rows = store
        .list_session_continuations(&sessions[0].id, None, Some(10))
        .unwrap();
    assert_eq!(rows.len(), 2);
    let delivered = rows.iter().find(|row| row.status == "delivered").unwrap();
    assert_eq!(delivered.run_count, 1);
    assert_eq!(delivered.recurrence_every_secs, Some(120));
    assert_eq!(delivered.recurrence_max_runs, 4);
    assert_eq!(delivered.series_id, delivered.id);
    let child = rows.iter().find(|row| row.status == "pending").unwrap();
    assert_eq!(
        child.parent_continuation_id.as_deref(),
        Some(delivered.id.as_str())
    );
    assert_eq!(child.run_count, 1);
    assert_eq!(child.series_id, delivered.series_id);
    assert!(child.watch_task_ids.is_empty());
}

#[tokio::test]
async fn recurring_continuation_crash_before_delivery_fails_closed_without_child() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("recurring crash".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "crash".into(), None)
        .await
        .unwrap();
    engine
        .set_team_plan(&session.id, &orchestration_test_plan())
        .await
        .unwrap();
    let created = engine
        .defer_session(
            &session.id,
            "crash before delivery",
            vec!["T1".into()],
            None,
            None,
            None,
            None,
            Some(60),
            Some(3),
            None,
        )
        .await
        .unwrap()
        .continuation;
    engine.skip_plan_task(&session.id, "T1").await.unwrap();
    engine
        .mark_session_continuation_ready(&created.id, "watch", &[])
        .await
        .unwrap()
        .unwrap();
    engine
        .claim_session_continuation(&created.id)
        .await
        .unwrap()
        .unwrap();
    engine.recover_interrupted_sessions().await.unwrap();

    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].status, "delivering");
    assert_eq!(rows[0].run_count, 0);
    assert!(rows[0].lease_owner.is_some());

    let expired_at = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    let reclaimed = engine
        .store()
        .reclaim_expired_session_continuation_at(&created.id, "test expiry", expired_at)
        .unwrap()
        .expect("expired claim was reclaimable");
    assert_eq!(reclaimed.0.status, "ready");
    assert!(reclaimed.0.lease_owner.is_none());
    assert!(
        engine
            .mark_session_continuation_delivered(&created.id)
            .await
            .unwrap()
            .is_none(),
        "a stale delivery token must not write a terminal state"
    );
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "session.continuation.reclaimed" && event.payload["reason"] == "test expiry"
    }));
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.continuation.delivered")
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.continuation.scheduled")
    );
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn evidence_backed_memory_ranks_briefs_verifies_and_sweeps() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let head = String::from_utf8_lossy(
        &Command::new("git")
            .arg("-C")
            .arg(repo.path())
            .args(["rev-parse", "HEAD"])
            .output()
            .unwrap()
            .stdout,
    )
    .trim()
    .to_string();
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("memory v2".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "parser work".into(), None)
        .await
        .unwrap();

    let task_memory = engine
        .create_memory_with_metadata(
            &session.id,
            "worker-1",
            "Worker",
            "decision",
            "Parser design",
            "Parser uses an explicit tokenizer and a deterministic grammar.",
            vec!["T1".to_string(), "architecture".to_string()],
            "operator",
            vec![
                MemoryEvidence {
                    kind: "task".to_string(),
                    reference: "T1".to_string(),
                    note: None,
                },
                MemoryEvidence {
                    kind: "path".to_string(),
                    reference: "src/api".to_string(),
                    note: None,
                },
            ],
            true,
            None,
        )
        .await
        .unwrap();
    engine
        .create_memory_as(
            &session.id,
            "agent-1",
            "Leader",
            "fact",
            "Unrelated note",
            "The build uses cargo.",
            Vec::new(),
        )
        .await
        .unwrap();

    let search = engine
        .memory_search(MemorySearchParams {
            project_id: project.id.clone(),
            query: "parser tokenizer".to_string(),
            kind: None,
            kinds: Vec::new(),
            task_id: Some("T1".to_string()),
            department: None,
            paths: vec!["src/api/handlers".to_string()],
            limit: Some(5),
            include_stale: false,
            max_chars: None,
            lexical_only: false,
        })
        .await
        .unwrap();
    assert!(!search.memories.is_empty());
    assert_eq!(search.memories[0].memory.id, task_memory.id);
    assert!(
        search.memories[0]
            .reasons
            .iter()
            .any(|reason| reason == "task:T1")
    );
    assert!(
        search.memories[0]
            .reasons
            .iter()
            .any(|reason| reason == "path:src/api/handlers")
    );
    assert!(
        search.memories[0]
            .reasons
            .iter()
            .any(|reason| reason == "pinned")
    );

    // Structural task evidence is a candidate path even without lexical
    // overlap with the query text.
    let structural = engine
        .memory_search(MemorySearchParams {
            project_id: project.id.clone(),
            query: "zzz-no-lexical-hit".to_string(),
            kind: None,
            kinds: Vec::new(),
            task_id: Some("T1".to_string()),
            department: None,
            paths: Vec::new(),
            limit: Some(5),
            include_stale: false,
            max_chars: None,
            lexical_only: false,
        })
        .await
        .unwrap();
    assert!(
        structural
            .memories
            .iter()
            .any(|memory| memory.memory.id == task_memory.id)
    );

    let brief = engine.memory_brief(&session.id, Some("T1")).await.unwrap();
    assert!(brief.text.contains("Parser design"));
    assert!(
        brief
            .memories
            .iter()
            .any(|memory| memory.memory.id == task_memory.id)
    );
    let used = engine.store().get_memory(&task_memory.id).unwrap().unwrap();
    assert_eq!(used.use_count, 1);
    // Usage is observed once per (memory, session), even across repeated briefs.
    engine.memory_brief(&session.id, Some("T1")).await.unwrap();
    let used_again = engine.store().get_memory(&task_memory.id).unwrap().unwrap();
    assert_eq!(used_again.use_count, 1);

    let updated = engine
        .memory_update(
            &task_memory.id,
            Some("archived"),
            Some(false),
            Some(vec!["parser".to_string()]),
        )
        .await
        .unwrap();
    assert_eq!(updated.memory.status, "archived");
    assert!(!updated.memory.pinned);
    assert_eq!(updated.memory.tags, vec!["parser".to_string()]);
    engine
        .memory_update(&task_memory.id, Some("active"), Some(true), None)
        .await
        .unwrap();

    let verifiable = engine
        .create_memory_with_metadata(
            &session.id,
            "worker-1",
            "Worker",
            "fact",
            "Verified checkout",
            "README exists at the project root.",
            Vec::new(),
            "task_result",
            vec![
                MemoryEvidence {
                    kind: "commit".to_string(),
                    reference: head.clone(),
                    note: None,
                },
                MemoryEvidence {
                    kind: "path".to_string(),
                    reference: "README.md".to_string(),
                    note: None,
                },
            ],
            false,
            None,
        )
        .await
        .unwrap();
    let verified = engine.memory_verify(&verifiable.id).await.unwrap();
    assert_eq!(verified.memory.status, "active");
    assert!(verified.memory.last_verified_at.is_some());
    assert!(verified.memory.verify_after.is_some());
    assert!(
        verified
            .checks
            .iter()
            .all(|check| check.status == "verified")
    );

    let broken = engine
        .create_memory_with_metadata(
            &session.id,
            "worker-1",
            "Worker",
            "fact",
            "Broken evidence",
            "This commit does not exist.",
            Vec::new(),
            "agent",
            vec![MemoryEvidence {
                kind: "commit".to_string(),
                reference: "deadbeefdeadbeefdeadbeefdeadbeefdeadbeef".to_string(),
                note: None,
            }],
            false,
            None,
        )
        .await
        .unwrap();
    let failed = engine.memory_verify(&broken.id).await.unwrap();
    assert_eq!(failed.memory.status, "stale");
    assert!(
        failed
            .memory
            .stale_reason
            .as_deref()
            .is_some_and(|reason| reason.contains("verification failed"))
    );
    assert!(failed.checks.iter().any(|check| check.status == "missing"));

    let url_only = engine
        .create_memory_with_metadata(
            &session.id,
            "worker-1",
            "Worker",
            "source",
            "Remote reference",
            "A URL that local verification cannot fetch.",
            Vec::new(),
            "agent",
            vec![MemoryEvidence {
                kind: "url".to_string(),
                reference: "https://example.test/spec".to_string(),
                note: None,
            }],
            false,
            None,
        )
        .await
        .unwrap();
    let unchecked = engine.memory_verify(&url_only.id).await.unwrap();
    assert_eq!(unchecked.memory.status, "active");
    assert!(unchecked.memory.last_verified_at.is_none());
    assert!(
        unchecked
            .checks
            .iter()
            .all(|check| check.status == "unchecked")
    );

    let expired = engine
        .create_memory_with_metadata(
            &session.id,
            "agent-1",
            "Leader",
            "fact",
            "Expired fact",
            "An expired active fact.",
            Vec::new(),
            "agent",
            Vec::new(),
            false,
            Some("2020-01-01T00:00:00Z".to_string()),
        )
        .await
        .unwrap();
    let pinned_expired = engine
        .create_memory_with_metadata(
            &session.id,
            "agent-1",
            "Leader",
            "fact",
            "Pinned expired fact",
            "Pinned memories are never swept.",
            Vec::new(),
            "agent",
            Vec::new(),
            true,
            Some("2020-01-01T00:00:00Z".to_string()),
        )
        .await
        .unwrap();
    let dry_run = engine.memory_sweep(&project.id, None, true).await.unwrap();
    assert!(dry_run.candidates >= 1);
    assert_eq!(dry_run.marked_stale, 0);
    assert_eq!(
        engine
            .store()
            .get_memory(&expired.id)
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
    let swept = engine.memory_sweep(&project.id, None, false).await.unwrap();
    assert!(swept.marked_stale >= 1);
    assert_eq!(
        engine
            .store()
            .get_memory(&expired.id)
            .unwrap()
            .unwrap()
            .status,
        "stale"
    );
    assert_eq!(
        engine
            .store()
            .get_memory(&pinned_expired.id)
            .unwrap()
            .unwrap()
            .status,
        "active"
    );

    let stats = engine.memory_stats(&project.id).await.unwrap();
    assert!(stats.total >= 5);
    assert!(stats.by_status.iter().any(|count| count.key == "stale"));
    assert!(stats.by_source.iter().any(|count| count.key == "operator"));

    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.memories_matches);
    assert!(replay.consistent);
}

#[tokio::test]
async fn memory_projection_folds_lifecycle_events_repairs_and_bundles() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("memory replay".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "replay".into(), None)
        .await
        .unwrap();
    let _first = engine
        .create_memory_with_metadata(
            &session.id,
            "agent-1",
            "Leader",
            "decision",
            "Replay decision",
            "First body.",
            vec!["architecture".to_string()],
            "agent",
            vec![MemoryEvidence {
                kind: "path".to_string(),
                reference: "README.md".to_string(),
                note: None,
            }],
            false,
            Some("2999-01-01T00:00:00Z".to_string()),
        )
        .await
        .unwrap();
    let second = engine
        .create_memory_with_metadata(
            &session.id,
            "agent-1",
            "Leader",
            "decision",
            "Replay decision",
            "Second body supersedes the first.",
            vec!["architecture".to_string()],
            "operator",
            Vec::new(),
            true,
            Some("2999-01-01T00:00:00Z".to_string()),
        )
        .await
        .unwrap();
    engine.memory_verify(&second.id).await.unwrap();
    engine.memory_brief(&session.id, None).await.unwrap();
    engine
        .memory_update(&second.id, Some("stale"), None, None)
        .await
        .unwrap();
    engine
        .memory_update(&second.id, Some("active"), None, None)
        .await
        .unwrap();

    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.memories_matches);
    assert!(replay.consistent);

    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.memories.len(), 2);
    let imported_dir = TempDir::new().unwrap();
    let imported = Store::open(imported_dir.path()).unwrap();
    imported.import_session_bundle(&bundle).unwrap();
    let mut expected = bundle.memories.clone();
    expected.sort_by(|left, right| left.id.cmp(&right.id));
    let mut actual = imported.list_session_memories(&session.id).unwrap();
    actual.sort_by(|left, right| left.id.cmp(&right.id));
    assert_eq!(actual, expected);
    assert!(
        actual
            .iter()
            .any(|memory| memory.status == "superseded" && memory.superseded_by.is_some())
    );
    assert!(
        actual
            .iter()
            .any(|memory| memory.use_count > 0 && memory.last_used_at.is_some())
    );

    engine
        .store()
        .replace_session_memories(&session.id, &[])
        .unwrap();
    let drifted = engine.replay_session(&session.id).await.unwrap();
    assert!(!drifted.memories_matches);
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.consistent);
    let repaired_memories = engine.store().list_session_memories(&session.id).unwrap();
    assert_eq!(repaired_memories.len(), 2);
}

#[tokio::test]
async fn memory_conflicts_scan_broadcast_resolve_health_and_bundle() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("memory conflicts".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "conflicts".into(), None)
        .await
        .unwrap();
    let path_evidence = vec![MemoryEvidence {
        kind: "path".to_string(),
        reference: "README.md".to_string(),
        note: None,
    }];
    let first = engine
        .create_memory_with_metadata(
            &session.id,
            "agent-1",
            "Leader",
            "decision",
            "README says hello",
            "The README is a greeting.",
            Vec::new(),
            "operator",
            path_evidence.clone(),
            false,
            None,
        )
        .await
        .unwrap();
    let second = engine
        .create_memory_with_metadata(
            &session.id,
            "agent-1",
            "Leader",
            "convention",
            "README says goodbye",
            "The README is a farewell.",
            Vec::new(),
            "operator",
            path_evidence,
            false,
            None,
        )
        .await
        .unwrap();

    let mut receiver = engine.subscribe();
    while receiver.try_recv().is_ok() {}
    let scan = engine.memory_conflict_scan(&project.id).await.unwrap();
    assert_eq!(scan.detected, 1);
    assert_eq!(scan.open, 1);
    assert_eq!(scan.conflicts.len(), 1);
    let conflict_id = scan.conflicts[0].id.clone();
    let event = receiver.recv().await.unwrap();
    assert_eq!(event.kind, "memory.conflict.detected");
    assert_eq!(event.payload["id"], conflict_id.clone());
    assert_eq!(event.payload["memory_a_id"], first.id.clone());
    assert_eq!(event.payload["memory_b_id"], second.id.clone());

    // Daemon tick shape: every project is scanned, already-known ids are not
    // recreated, and the open count is reported without resolving anything.
    let (projects, detected, open) = engine.scan_all_memory_conflicts().await.unwrap();
    assert_eq!(projects, 1);
    assert_eq!(detected, 0);
    assert_eq!(open, 1);

    let listed = engine
        .memory_conflicts(&project.id, Some("open"), Some(10))
        .await
        .unwrap();
    assert_eq!(listed.conflicts.len(), 1);
    let health = engine.memory_health(&project.id).await.unwrap();
    assert_eq!(health.active, 2);
    assert_eq!(health.open_conflicts, 1);
    assert_eq!(health.evidence_covered, 2);

    let resolution = engine
        .memory_conflict_resolve(&conflict_id, "keep_a", Some("greeting wins"))
        .await
        .unwrap();
    assert_eq!(resolution.conflict.status, "resolved");
    assert_eq!(
        resolution
            .memories
            .iter()
            .find(|memory| memory.id == second.id)
            .unwrap()
            .status,
        "superseded"
    );
    let event = receiver.recv().await.unwrap();
    assert_eq!(event.kind, "memory.superseded");
    let event = receiver.recv().await.unwrap();
    assert_eq!(event.kind, "memory.conflict.resolved");
    assert!(
        engine
            .memory_conflict_resolve(&conflict_id, "keep_a", None)
            .await
            .is_err()
    );
    assert!(
        engine
            .memory_conflict_resolve(&conflict_id, "invalid", None)
            .await
            .is_err()
    );

    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.memory_conflicts_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.memory_conflicts.len(), 1);
    assert_eq!(bundle.memory_conflicts[0].status, "resolved");

    let imported_dir = TempDir::new().unwrap();
    let imported = Store::open(imported_dir.path()).unwrap();
    imported.import_session_bundle(&bundle).unwrap();
    let imported_conflicts = imported.list_session_memory_conflicts(&session.id).unwrap();
    assert_eq!(imported_conflicts, bundle.memory_conflicts);

    engine
        .store()
        .replace_session_memory_conflicts(&session.id, &[])
        .unwrap();
    let drifted = engine.replay_session(&session.id).await.unwrap();
    assert!(!drifted.memory_conflicts_match);
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.consistent);
    assert_eq!(
        engine
            .store()
            .list_session_memory_conflicts(&session.id)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn deferred_continuation_operation_replays_and_retries_after_failure() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("defer operation".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "defer operation".into(), None)
        .await
        .unwrap();
    let error = engine
        .defer_session(
            &session.id,
            "follow up once",
            vec!["T1".into()],
            None,
            None,
            None,
            None,
            None,
            None,
            Some("op-defer-1"),
        )
        .await
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("session has no stored TeamPlan to watch")
    );
    assert!(
        engine
            .session_continuations(&session.id, None, Some(10))
            .await
            .unwrap()
            .continuations
            .is_empty()
    );

    engine
        .set_team_plan(&session.id, &orchestration_test_plan())
        .await
        .unwrap();
    let first = engine
        .defer_session(
            &session.id,
            "follow up once",
            vec!["T1".into()],
            None,
            None,
            None,
            None,
            None,
            None,
            Some("op-defer-1"),
        )
        .await
        .unwrap()
        .continuation;
    let replay = engine
        .defer_session(
            &session.id,
            "follow up once",
            vec!["T1".into()],
            None,
            None,
            None,
            None,
            None,
            None,
            Some("op-defer-1"),
        )
        .await
        .unwrap()
        .continuation;
    assert_eq!(first.id, replay.id);
    let conflict = engine
        .defer_session(
            &session.id,
            "different follow up",
            vec!["T1".into()],
            None,
            None,
            None,
            None,
            None,
            None,
            Some("op-defer-1"),
        )
        .await
        .unwrap_err();
    assert!(conflict.to_string().contains("different method or payload"));

    let listed = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap();
    assert_eq!(listed.continuations.len(), 1);
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.created")
            .count(),
        1,
        "replay must not create a duplicate continuation event"
    );
}

#[tokio::test]
async fn steering_operation_replays_and_retries_after_failure() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("steer operation".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "steer operation".into(), None)
        .await
        .unwrap();
    let leader_id = engine
        .create_leader_agent(&session.id, "test")
        .await
        .unwrap();
    engine
        .update_agent_status(&leader_id, "idle")
        .await
        .unwrap();
    let error = engine
        .steer_session(
            &session.id,
            "inspect the tests first",
            None,
            None,
            Some("op-steer-1"),
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no running Leader"));

    engine
        .update_agent_status(&leader_id, "running")
        .await
        .unwrap();
    let first = engine
        .steer_session(
            &session.id,
            "inspect the tests first",
            None,
            None,
            Some("op-steer-1"),
        )
        .await
        .unwrap()
        .steering;
    let replay = engine
        .steer_session(
            &session.id,
            "inspect the tests first",
            None,
            None,
            Some("op-steer-1"),
        )
        .await
        .unwrap()
        .steering;
    assert_eq!(first.id, replay.id);
    let conflict = engine
        .steer_session(
            &session.id,
            "change the tests",
            None,
            None,
            Some("op-steer-1"),
        )
        .await
        .unwrap_err();
    assert!(conflict.to_string().contains("different method or payload"));

    let listed = engine
        .session_steering(&session.id, None, Some(10))
        .await
        .unwrap();
    assert_eq!(listed.steering.len(), 1);
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "steering.created")
            .count(),
        1,
        "replay must not create a duplicate steering event"
    );
}

#[tokio::test]
async fn daemon_identity_is_shared_but_runtime_owners_are_distinct() {
    let data = TempDir::new().unwrap();
    let first = Engine::new(Store::open(data.path()).unwrap());
    let second = Engine::new(Store::open(data.path()).unwrap());
    assert_eq!(first.daemon_identity(), second.daemon_identity());
    assert_ne!(first.runtime_id(), second.runtime_id());
    assert!(
        first
            .runtime_id()
            .starts_with(&format!("{}:", first.daemon_identity().instance_id))
    );
    assert!(
        first
            .runtime_id()
            .ends_with(&first.owner_instance()[first.owner_instance().len()..])
    );
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let project = first
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("identity".into()),
        )
        .await
        .unwrap();
    let session = first
        .create_session(project.id, "identity".into(), None)
        .await
        .unwrap();
    let lease = first
        .acquire_runtime_lease(
            "integration",
            &session.id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let listed = first.list_runtime_leases().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].owner_id, first.runtime_id());
    assert_eq!(listed[0].owner_instance, first.owner_instance());
    assert_eq!(listed[0].fencing_token, lease.generation());
    lease.release().await;
    assert!(first.list_runtime_leases().await.unwrap().is_empty());
}

#[tokio::test]
async fn runtime_lease_guard_release_on_drop_removes_the_row() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("drop".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "drop".into(), None)
        .await
        .unwrap();
    let lease = engine
        .acquire_runtime_lease(
            "integration",
            &session.id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(engine.list_runtime_leases().await.unwrap().len(), 1);
    drop(lease);
    assert!(
        engine.list_runtime_leases().await.unwrap().is_empty(),
        "dropping a guard releases its fenced row"
    );
}

#[tokio::test]
async fn stale_runtime_guard_cannot_assert_or_delete_a_new_generation() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let first = Engine::new(Store::open(data.path()).unwrap());
    let second = Engine::new(Store::open(data.path()).unwrap());
    let project = first
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("stale guard".into()),
        )
        .await
        .unwrap();
    let session = first
        .create_session(project.id, "stale guard".into(), None)
        .await
        .unwrap();
    let old = first
        .acquire_runtime_lease_at_for_test(
            "integration",
            &session.id,
            0,
            time::OffsetDateTime::now_utc() - time::Duration::seconds(120),
        )
        .await
        .unwrap();
    assert!(old.assert_held().await.is_err());
    assert!(old.is_lost());
    let current = second
        .acquire_runtime_lease(
            "integration",
            &session.id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(current.generation(), old.generation() + 1);
    old.release().await;
    let leases = second.list_runtime_leases().await.unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].owner_id, second.runtime_id());
    assert_eq!(leases[0].fencing_token, current.generation());
    current.release().await;
}

#[tokio::test]
async fn runtime_lease_heartbeat_renews_an_unexpired_guard() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("heartbeat".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "heartbeat".into(), None)
        .await
        .unwrap();
    let lease = engine
        .acquire_runtime_lease(
            "integration",
            &session.id,
            std::time::Duration::from_secs(10),
        )
        .await
        .unwrap();
    let first = engine.list_runtime_leases().await.unwrap()[0].clone();
    tokio::time::sleep(std::time::Duration::from_millis(6_200)).await;
    let renewed = engine.list_runtime_leases().await.unwrap()[0].clone();
    assert_ne!(renewed.renewed_at, first.renewed_at);
    assert!(renewed.expires_at > first.expires_at);
    assert!(!lease.is_lost());
    lease.release().await;
}

#[tokio::test]
async fn two_engines_claim_one_continuation_and_only_the_winner_acknowledges() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let first = Engine::new(Store::open(data.path()).unwrap());
    let second = Engine::new(Store::open(data.path()).unwrap());
    let project = first
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("claim race".into()),
        )
        .await
        .unwrap();
    let session = first
        .create_session(project.id, "claim race".into(), None)
        .await
        .unwrap();
    let continuation = first
        .defer_session(
            &session.id,
            "deliver once",
            Vec::new(),
            None,
            None,
            Some(3_600),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    first
        .mark_session_continuation_ready(&continuation.id, "deadline", &[])
        .await
        .unwrap()
        .unwrap();

    let (left, right) = tokio::join!(
        first.claim_session_continuation_for_delivery(&continuation.id, 60),
        second.claim_session_continuation_for_delivery(&continuation.id, 60)
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert!(left.is_some() ^ right.is_some());
    let winner = left.or(right).unwrap();
    assert_eq!(winner.continuation.status, "delivering");
    assert_eq!(winner.continuation.delivery_attempt, 1);
    let session_id = winner.continuation.session_id.clone();
    let delivered = first
        .ack_session_continuation_delivered(&winner)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.status, "delivered");
    let events = first.list_events(session_id, 0, 200).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivered")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivering")
            .count(),
        1
    );
}

#[tokio::test]
async fn stale_continuation_owner_cannot_write_terminal_state_after_reclaim() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let first = Engine::new(Store::open(data.path()).unwrap());
    let second = Engine::new(Store::open(data.path()).unwrap());
    let project = first
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("stale continuation".into()),
        )
        .await
        .unwrap();
    let session = first
        .create_session(project.id, "stale continuation".into(), None)
        .await
        .unwrap();
    let continuation = first
        .defer_session(
            &session.id,
            "stale owner",
            Vec::new(),
            None,
            None,
            Some(3_600),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    first
        .mark_session_continuation_ready(&continuation.id, "deadline", &[])
        .await
        .unwrap()
        .unwrap();
    let stale = first
        .claim_session_continuation_for_delivery(&continuation.id, 0)
        .await
        .unwrap()
        .expect("stale claimant owns the first generation");
    let current = second
        .claim_session_continuation_for_delivery(&continuation.id, 60)
        .await
        .unwrap()
        .expect("expired claim is reclaimable");
    assert!(current.reclaimed);
    assert_eq!(current.delivery_attempt, 2);

    let stale_ack = first
        .ack_session_continuation_delivered(&stale)
        .await
        .unwrap_err();
    assert!(matches!(
        stale_ack,
        SessionContinuationAckError::Lost { .. }
    ));
    assert!(
        first
            .ack_session_continuation_failed(&stale, "stale failure")
            .await
            .is_err()
    );
    assert!(
        first
            .ack_session_continuation_retry(&stale, "stale retry", "2999-01-01T00:00:00Z")
            .await
            .is_err()
    );
    let rows = first
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows[0].status, "delivering");
    assert_eq!(
        rows[0].lease_owner.as_deref(),
        Some(current.owner_id.as_str())
    );
    let delivered = second
        .ack_session_continuation_delivered(&current)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.status, "delivered");
}

#[tokio::test]
async fn restart_recovery_leaves_live_foreign_continuation_claims_alone() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let first = Engine::new(Store::open(data.path()).unwrap());
    let project = first
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("live foreign claim".into()),
        )
        .await
        .unwrap();
    let session = first
        .create_session(project.id, "live foreign claim".into(), None)
        .await
        .unwrap();
    let continuation = first
        .defer_session(
            &session.id,
            "live foreign claim",
            Vec::new(),
            None,
            None,
            Some(3_600),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    first
        .mark_session_continuation_ready(&continuation.id, "deadline", &[])
        .await
        .unwrap()
        .unwrap();
    let claim = first
        .claim_session_continuation_for_delivery(&continuation.id, 60)
        .await
        .unwrap()
        .unwrap();

    let restarted = Engine::new(Store::open(data.path()).unwrap());
    restarted.recover_interrupted_sessions().await.unwrap();
    let rows = restarted
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows[0].status, "delivering");
    assert_eq!(
        rows[0].lease_owner.as_deref(),
        Some(claim.owner_id.as_str())
    );
    assert!(
        restarted
            .claim_session_continuation_for_delivery(&continuation.id, 60)
            .await
            .unwrap()
            .is_none()
    );
    let replay = restarted.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn restart_recovery_reclaims_an_expired_continuation_claim() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let first = Engine::new(Store::open(data.path()).unwrap());
    let project = first
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("expired foreign claim".into()),
        )
        .await
        .unwrap();
    let session = first
        .create_session(project.id, "expired foreign claim".into(), None)
        .await
        .unwrap();
    let continuation = first
        .defer_session(
            &session.id,
            "expired foreign claim",
            Vec::new(),
            None,
            None,
            Some(3_600),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    first
        .mark_session_continuation_ready(&continuation.id, "deadline", &[])
        .await
        .unwrap()
        .unwrap();
    let stale = first
        .claim_session_continuation_for_delivery(&continuation.id, 0)
        .await
        .unwrap()
        .unwrap();
    let restarted = Engine::new(Store::open(data.path()).unwrap());
    restarted.recover_interrupted_sessions().await.unwrap();
    let rows = restarted
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows[0].status, "ready");
    assert!(rows[0].lease_owner.is_none());
    assert!(
        restarted
            .ack_session_continuation_delivered(&stale)
            .await
            .is_err()
    );
    let events = restarted
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "session.continuation.reclaimed")
    );
}

#[tokio::test]
async fn restart_recovery_does_not_fail_a_session_with_a_live_foreign_lease() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let first = Engine::new(Store::open(data.path()).unwrap());
    let project = first
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("foreign session".into()),
        )
        .await
        .unwrap();
    let session = first
        .create_session(project.id, "foreign session".into(), None)
        .await
        .unwrap();
    first
        .update_session_status(&session.id, "running")
        .await
        .unwrap();
    let store = first.store().clone();
    let foreign = store
        .acquire_runtime_lease(
            &format!("session:{}", session.id),
            "foreign-owner",
            "foreign-instance:999",
            "session",
            Some(&session.id),
            60,
        )
        .unwrap();
    let restarted = Engine::new(Store::open(data.path()).unwrap());
    restarted.recover_interrupted_sessions().await.unwrap();
    let after = restarted.store().get_session(&session.id).unwrap().unwrap();
    assert_eq!(after.status, "running");
    assert!(
        !restarted
            .list_events(session.id.clone(), 0, 100)
            .await
            .unwrap()
            .iter()
            .any(|event| event.kind == "agent.failed")
    );
    store
        .release_runtime_lease(
            &format!("session:{}", session.id),
            "foreign-owner",
            foreign.token(),
        )
        .unwrap();
    restarted.recover_interrupted_sessions().await.unwrap();
    let after = restarted.store().get_session(&session.id).unwrap().unwrap();
    assert_eq!(after.status, "failed");
}

#[tokio::test]
async fn continuation_claim_metadata_survives_replay_repair_bundle_and_rebuild() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("claim replay".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "claim replay".into(), None)
        .await
        .unwrap();
    let continuation = engine
        .defer_session(
            &session.id,
            "claim replay body",
            Vec::new(),
            None,
            None,
            Some(3_600),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    engine
        .mark_session_continuation_ready(&continuation.id, "deadline", &[])
        .await
        .unwrap()
        .unwrap();
    let claim = engine
        .claim_session_continuation_for_delivery(&continuation.id, 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim.continuation.delivery_attempt, 1);

    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
    let events = engine
        .list_events(session.id.clone(), 0, 200)
        .await
        .unwrap();
    let delivering = events
        .iter()
        .find(|event| event.kind == "session.continuation.delivering")
        .unwrap();
    assert_eq!(
        delivering.payload["lease_owner"].as_str(),
        Some(claim.owner_id.as_str())
    );
    assert_eq!(delivering.payload["delivery_attempt"], 1);
    assert!(delivering.payload.get("lease_token").is_none());
    assert!(!delivering.payload.to_string().contains("lease_token"));

    engine
        .store()
        .replace_session_continuations(&session.id, &[])
        .unwrap();
    let drifted = engine.replay_session(&session.id).await.unwrap();
    assert!(!drifted.continuations_match);
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(repaired.continuations_match, "{:?}", repaired.mismatches);
    let rebuilt = engine.rebuild_session(&session.id).await.unwrap();
    assert!(rebuilt.continuations_match, "{:?}", rebuilt.mismatches);

    let bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.continuations.len(), 1);
    assert_eq!(
        bundle.continuations[0].lease_owner.as_deref(),
        Some(claim.owner_id.as_str())
    );
    assert_eq!(bundle.continuations[0].delivery_attempt, 1);
    assert!(bundle.continuations[0].lease_expires_at.is_some());

    let import_data = TempDir::new().unwrap();
    let imported_engine = Engine::new(Store::open(import_data.path()).unwrap());
    imported_engine.import_session(bundle).await.unwrap();
    let imported = imported_engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(imported.len(), 1);
    assert_eq!(
        imported[0].lease_owner.as_deref(),
        Some(claim.owner_id.as_str())
    );
    assert_eq!(imported[0].delivery_attempt, 1);
    let imported_replay = imported_engine.replay_session(&session.id).await.unwrap();
    assert!(
        imported_replay.continuations_match,
        "{:?}",
        imported_replay.mismatches
    );

    engine.record_project_manifest().await.unwrap();
    let report = crate::rebuild::rebuild_database(data.path(), false)
        .await
        .unwrap();
    assert_eq!(report.sessions, 1);
    let store = Store::open(data.path()).unwrap();
    let rows = store
        .list_session_continuations(&session.id, None, Some(10))
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].delivery_attempt, 1);
    assert_eq!(
        rows[0].lease_owner.as_deref(),
        Some(claim.owner_id.as_str())
    );
}

#[tokio::test]
async fn lost_runtime_guard_notifies_and_wakes_a_waiting_critical_section() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("lost signal".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "lost signal".into(), None)
        .await
        .unwrap();
    let stale = engine
        .acquire_runtime_lease_at_for_test(
            "integration",
            &session.id,
            0,
            time::OffsetDateTime::now_utc() - time::Duration::seconds(60),
        )
        .await
        .unwrap();
    assert!(stale.assert_held().await.is_err());
    tokio::time::timeout(std::time::Duration::from_millis(100), stale.lost_signal())
        .await
        .expect("lost guard wakes its waiter immediately");

    let live = engine
        .acquire_runtime_lease(
            "integration",
            &session.id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), live.lost_signal())
            .await
            .is_err(),
        "a live guard does not signal loss"
    );
    live.release().await;
}

#[tokio::test]
async fn continuation_reclaim_retry_and_failure_events_replay_without_tokens() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("claim event replay".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "claim event replay".into(), None)
        .await
        .unwrap();
    let first = engine
        .defer_session(
            &session.id,
            "reclaim replay",
            Vec::new(),
            None,
            None,
            Some(3_600),
            None,
            None,
            None,
            None,
        )
        .await
        .unwrap()
        .continuation;
    engine
        .mark_session_continuation_ready(&first.id, "deadline", &[])
        .await
        .unwrap()
        .unwrap();
    let stale = engine
        .claim_session_continuation_for_delivery(&first.id, 0)
        .await
        .unwrap()
        .unwrap();
    engine
        .reclaim_session_continuation(&first.id, Some("reclaim for replay"))
        .await
        .unwrap();
    let second = engine
        .claim_session_continuation_for_delivery(&first.id, 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.delivery_attempt, 2);
    assert!(
        !second.reclaimed,
        "an explicit reclaim resets the row to ready"
    );
    engine
        .ack_session_continuation_retry(&second, "provider down", "2999-01-01T00:00:00Z")
        .await
        .unwrap()
        .unwrap();
    let third = engine
        .claim_session_continuation_for_delivery(&first.id, 60)
        .await
        .unwrap()
        .unwrap();
    engine
        .ack_session_continuation_failed(&third, "model refused")
        .await
        .unwrap()
        .unwrap();
    assert!(
        engine
            .ack_session_continuation_delivered(&stale)
            .await
            .is_err()
    );

    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
    let rows = engine
        .session_continuations(&session.id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows[0].status, "failed");
    assert_eq!(rows[0].delivery_attempt, 3);
    assert_eq!(rows[0].attempts, 2);
    assert!(rows[0].lease_owner.is_none());
    let events = engine
        .list_events(session.id.clone(), 0, 300)
        .await
        .unwrap();
    for event in events
        .iter()
        .filter(|event| event.kind.starts_with("session.continuation."))
    {
        assert!(
            event.payload.get("lease_token").is_none(),
            "{} leaked a token",
            event.kind
        );
        assert!(!event.payload.to_string().contains("lease_token"));
    }
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.reclaimed")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.retrying")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.failed")
            .count(),
        1
    );
}

#[tokio::test]
async fn session_lease_events_and_public_leases_never_carry_tokens() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("lease redaction".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id, "lease redaction".into(), None)
        .await
        .unwrap();
    let lease = engine
        .acquire_session_lease(&session.id, std::time::Duration::from_secs(60))
        .await
        .unwrap();
    let lease_text = serde_json::to_string(&engine.list_runtime_leases().await.unwrap()).unwrap();
    assert!(!lease_text.contains("lease_token"));
    assert!(!lease_text.contains("\"token\""));
    lease.release().await;
    let events = engine
        .list_events(session.id.clone(), 0, 100)
        .await
        .unwrap();
    let acquired = events
        .iter()
        .find(|event| event.kind == "session.lease.acquired")
        .unwrap();
    assert!(acquired.payload.get("lease_token").is_none());
    assert!(acquired.payload["fencing_token"].as_u64().is_some());
    assert!(acquired.payload["owner_instance"].as_str().is_some());
    let released = events
        .iter()
        .find(|event| event.kind == "session.lease.released")
        .unwrap();
    assert!(released.payload.get("lease_token").is_none());
    assert_eq!(released.payload["lease_lost"], false);
}

fn durable_context_checkpoint(
    session: &Session,
    project_id: &str,
    id: &str,
    through_message_id: Option<&str>,
) -> vp_protocol::types::ContextCheckpoint {
    vp_protocol::types::ContextCheckpoint {
        id: id.into(),
        session_id: session.id.clone(),
        project_id: project_id.into(),
        label: format!("checkpoint {id}"),
        mode: if through_message_id.is_some() {
            "summary".into()
        } else {
            "manual".into()
        },
        summary: through_message_id.map(|_| "Earlier conversation summary\n\nstate".to_string()),
        through_message_id: through_message_id.map(str::to_string),
        message_cursor: usize::from(through_message_id.is_some()),
        plan_revision: 0,
        provider: "queue".into(),
        model: "test".into(),
        prompt_prefix_hash: "prefix-hash".into(),
        worktree_head: String::new(),
        tokens_before: 5_000,
        tokens_after: 500,
        structured_state: Some(vp_protocol::types::ContextStructuredState {
            goal: "durable context".into(),
            plan_revision: 0,
            ..vp_protocol::types::ContextStructuredState::default()
        }),
        source_compaction_seq: None,
        automatic: false,
        created_at: vp_protocol::now_rfc3339(),
    }
}

#[tokio::test]
async fn context_checkpoints_replay_repair_and_event_log_restore() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("context checkpoints".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "replay checkpoints".into(), None)
        .await
        .unwrap();
    let message = engine
        .append_message(
            &session.id,
            "user",
            "boundary message",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    let checkpoint = durable_context_checkpoint(
        &session,
        &project.id,
        "ctx-replay-1",
        Some(message.id.as_str()),
    );
    let event = engine
        .record_context_checkpoint(checkpoint.clone())
        .await
        .unwrap();
    assert_eq!(event.kind, "context.checkpointed");
    assert_eq!(
        event.payload["checkpoint_id"].as_str(),
        Some(checkpoint.id.as_str())
    );
    let stored = engine
        .list_context_checkpoints(&session.id, Some(10))
        .await
        .unwrap();
    assert_eq!(stored.checkpoints.len(), 1);
    assert_eq!(stored.checkpoints[0].message_cursor, 1);
    assert_eq!(
        stored.checkpoints[0].structured_state,
        checkpoint.structured_state
    );

    // Append a restore boundary and verify event ordering and message safety.
    let message_count = engine.list_messages(&session.id).await.unwrap().len();
    let (compacted, restored) = engine
        .append_context_restore_events(
            &session.id,
            serde_json::json!({
                "mode": "summary",
                "through_message_id": message.id,
                "summary": "Earlier conversation summary\n\nstate",
                "estimated_tokens_before": 5_000,
                "estimated_tokens_after": 500,
                "context_checkpoint_id": checkpoint.id,
                "restored_from_checkpoint": checkpoint.id,
                "restored": true
            }),
            serde_json::json!({
                "checkpoint_id": checkpoint.id,
                "mode": "summary"
            }),
        )
        .await
        .unwrap();
    assert_eq!(restored.seq, compacted.seq + 1);
    assert_eq!(restored.payload["compaction_seq"], compacted.seq);
    assert_eq!(
        engine.list_messages(&session.id).await.unwrap().len(),
        message_count
    );

    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.context_checkpoints_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);

    // Dropping the projection is repaired from the full-payload events.
    engine
        .store()
        .replace_context_checkpoints(&session.id, &[])
        .unwrap();
    let drifted = engine.replay_session(&session.id).await.unwrap();
    assert!(!drifted.context_checkpoints_match);
    assert!(!drifted.consistent);
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(
        repaired.context_checkpoints_match,
        "{:?}",
        repaired.mismatches
    );
    assert!(repaired.consistent, "{:?}", repaired.mismatches);
    assert_eq!(
        engine
            .list_context_checkpoints(&session.id, Some(10))
            .await
            .unwrap()
            .checkpoints
            .len(),
        1
    );

    // The full event table can also be restored from its JSONL mirror.
    engine.store().replace_events(&session.id, &[]).unwrap();
    let restored_log = engine.restore_session_log(&session.id).await.unwrap();
    assert!(restored_log.matches, "{restored_log:?}");
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.context_checkpoints_match, "{:?}", report.mismatches);
    assert_eq!(
        engine
            .list_context_checkpoints(&session.id, Some(10))
            .await
            .unwrap()
            .checkpoints
            .len(),
        1
    );
    let status = engine.session_context_status(&session.id).await.unwrap();
    assert_eq!(status.checkpoint_count, 1);
    assert_eq!(status.latest_checkpoint_id.as_deref(), Some("ctx-replay-1"));
}

#[tokio::test]
async fn context_checkpoints_travel_in_bundles_and_survive_database_rebuild() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let session_id = {
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("context bundle".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "bundle checkpoints".into(), None)
            .await
            .unwrap();
        let message = engine
            .append_message(
                &session.id,
                "user",
                "bundle boundary message",
                serde_json::json!([]),
                None,
            )
            .await
            .unwrap();
        let checkpoint = durable_context_checkpoint(
            &session,
            &project.id,
            "ctx-bundle-1",
            Some(message.id.as_str()),
        );
        engine.record_context_checkpoint(checkpoint).await.unwrap();
        engine.record_project_manifest().await.unwrap();

        let bundle = engine.export_session(&session.id).await.unwrap();
        assert_eq!(bundle.context_checkpoints.len(), 1);
        assert_eq!(bundle.context_checkpoints[0].id, "ctx-bundle-1");
        assert!(
            bundle
                .events
                .iter()
                .any(|event| event.kind == "context.checkpointed")
        );

        let import_data = TempDir::new().unwrap();
        let imported_engine = Engine::new(Store::open(import_data.path()).unwrap());
        imported_engine.import_session(bundle).await.unwrap();
        let imported = imported_engine
            .list_context_checkpoints(&session.id, Some(10))
            .await
            .unwrap();
        assert_eq!(imported.checkpoints.len(), 1);
        assert_eq!(imported.checkpoints[0].id, "ctx-bundle-1");
        assert_eq!(
            imported_engine
                .store()
                .get_session(&session.id)
                .unwrap()
                .unwrap()
                .status,
            "imported"
        );
        let imported_replay = imported_engine.replay_session(&session.id).await.unwrap();
        assert!(
            imported_replay.context_checkpoints_match,
            "{:?}",
            imported_replay.mismatches
        );
        session.id.clone()
    };

    let report = crate::rebuild::rebuild_database(data.path(), false)
        .await
        .unwrap();
    assert_eq!(report.projects, 1);
    assert_eq!(report.sessions, 1);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let store = Store::open(data.path()).unwrap();
    let rows = store.list_context_checkpoints(&session_id, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "ctx-bundle-1");
    assert_eq!(
        rows[0].summary.as_deref(),
        Some("Earlier conversation summary\n\nstate")
    );
    assert_eq!(
        rows[0].structured_state.as_ref().unwrap().goal,
        "durable context"
    );
}

#[tokio::test]
async fn context_checkpoint_policy_is_project_scoped_and_bounded() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("context policy".into()),
        )
        .await
        .unwrap();
    let policy = engine
        .get_context_checkpoint_policy(&project.id)
        .await
        .unwrap();
    assert!(!policy.enabled);
    assert_eq!(policy.max_checkpoints, 20);
    assert_eq!(policy.max_age_days, 30);
    let saved = engine
        .set_context_checkpoint_policy(
            &project.id,
            &vp_protocol::types::ContextCheckpointPolicy {
                enabled: true,
                max_checkpoints: 5,
                max_age_days: 7,
            },
        )
        .await
        .unwrap();
    assert!(saved.policy.enabled);
    assert_eq!(saved.policy.max_checkpoints, 5);
    assert_eq!(saved.policy.max_age_days, 7);
    engine.record_project_manifest().await.unwrap();
    let snapshots = vp_store::read_project_manifest(data.path()).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert!(snapshots[0].context_checkpoint_policy.enabled);
    assert_eq!(snapshots[0].context_checkpoint_policy.max_checkpoints, 5);
    let error = engine
        .set_context_checkpoint_policy(
            &project.id,
            &vp_protocol::types::ContextCheckpointPolicy {
                enabled: true,
                max_checkpoints: 0,
                max_age_days: 7,
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("between 1 and"), "{error}");
    let error = engine
        .set_context_checkpoint_policy(
            &project.id,
            &vp_protocol::types::ContextCheckpointPolicy {
                enabled: true,
                max_checkpoints: 5,
                max_age_days: 366,
            },
        )
        .await
        .unwrap_err();
    assert!(error.to_string().contains("at most 365"), "{error}");
}

#[tokio::test]
async fn session_transcript_lists_context_checkpoints_and_the_active_boundary() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("transcript checkpoints".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "transcript checkpoints".into(), None)
        .await
        .unwrap();
    let message = engine
        .append_message(
            &session.id,
            "user",
            "checkpoint transcript message",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    let initial = engine.session_transcript(&session.id).await.unwrap();
    assert!(!initial.markdown.contains("## Context Checkpoints"));

    let checkpoint = durable_context_checkpoint(
        &session,
        &project.id,
        "ctx-transcript-1",
        Some(message.id.as_str()),
    );
    engine
        .record_context_checkpoint(checkpoint.clone())
        .await
        .unwrap();
    engine
        .append_context_restore_events(
            &session.id,
            serde_json::json!({
                "mode": "summary",
                "through_message_id": message.id,
                "summary": "Earlier conversation summary",
                "context_checkpoint_id": checkpoint.id,
                "restored_from_checkpoint": checkpoint.id
            }),
            serde_json::json!({"checkpoint_id": checkpoint.id}),
        )
        .await
        .unwrap();
    let transcript = engine.session_transcript(&session.id).await.unwrap();
    assert!(transcript.markdown.contains("## Context Checkpoints"));
    assert!(transcript.markdown.contains("ctx-transcript-1"));
    assert!(
        transcript
            .markdown
            .contains("| summary | 0 | 5000 → 500 | yes |")
    );
    assert!(transcript.markdown.contains("do not reset the worktree"));
}

#[tokio::test]
async fn importing_a_bundle_without_the_checkpoint_row_rebuilds_it_from_events() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("bundle rebuild".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "bundle rebuild".into(), None)
        .await
        .unwrap();
    let message = engine
        .append_message(
            &session.id,
            "user",
            "bundle rebuild message",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    let checkpoint = durable_context_checkpoint(
        &session,
        &project.id,
        "ctx-bundle-rebuild",
        Some(message.id.as_str()),
    );
    engine.record_context_checkpoint(checkpoint).await.unwrap();
    let mut bundle = engine.export_session(&session.id).await.unwrap();
    assert_eq!(bundle.context_checkpoints.len(), 1);
    // Simulate a producer that predates the projection field but still carries
    // the full checkpoint event.
    bundle.context_checkpoints.clear();

    let import_data = TempDir::new().unwrap();
    let imported_engine = Engine::new(Store::open(import_data.path()).unwrap());
    imported_engine.import_session(bundle).await.unwrap();
    let rebuilt = imported_engine
        .list_context_checkpoints(&session.id, Some(10))
        .await
        .unwrap();
    assert_eq!(rebuilt.checkpoints.len(), 1);
    assert_eq!(rebuilt.checkpoints[0].id, "ctx-bundle-rebuild");
    let replay = imported_engine.replay_session(&session.id).await.unwrap();
    assert!(replay.context_checkpoints_match, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn malformed_context_checkpoint_events_fail_replay_closed() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("malformed checkpoint".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "malformed checkpoint".into(), None)
        .await
        .unwrap();
    let checkpoint = durable_context_checkpoint(&session, &project.id, "ctx-valid-1", None);
    engine.record_context_checkpoint(checkpoint).await.unwrap();
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(report.consistent, "{:?}", report.mismatches);
    engine
        .append_event(
            &session.id,
            "context.checkpointed",
            serde_json::json!({"id": "ctx-malformed"}),
        )
        .await
        .unwrap();
    let drifted = engine.replay_session(&session.id).await.unwrap();
    assert!(!drifted.context_checkpoints_match);
    assert!(!drifted.consistent);
    assert!(
        drifted
            .mismatches
            .iter()
            .any(|mismatch| mismatch.starts_with("context checkpoints:"))
    );
    // Repair cannot invent a row from a malformed event; the mismatch stays.
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(!repaired.context_checkpoints_match);
    assert!(!repaired.consistent);
}

#[tokio::test]
async fn restore_events_rewind_the_active_conversation_and_can_be_superseded() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("restore events".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "restore events".into(), None)
        .await
        .unwrap();
    let mut messages = Vec::new();
    for index in 0..5 {
        messages.push(
            engine
                .append_message(
                    &session.id,
                    if index % 2 == 0 { "user" } else { "assistant" },
                    &format!("message {index}"),
                    serde_json::json!([]),
                    None,
                )
                .await
                .unwrap(),
        );
    }
    let first = durable_context_checkpoint(
        &session,
        &project.id,
        "ctx-early",
        Some(messages[1].id.as_str()),
    );
    let second = durable_context_checkpoint(
        &session,
        &project.id,
        "ctx-late",
        Some(messages[3].id.as_str()),
    );
    engine
        .record_context_checkpoint(first.clone())
        .await
        .unwrap();
    engine
        .record_context_checkpoint(second.clone())
        .await
        .unwrap();

    for (checkpoint, expected_start) in [(&first, 2), (&second, 4)] {
        engine
            .append_context_restore_events(
                &session.id,
                serde_json::json!({
                    "mode": "summary",
                    "through_message_id": checkpoint.through_message_id.clone(),
                    "summary": checkpoint.summary.clone(),
                    "context_checkpoint_id": checkpoint.id.clone(),
                    "restored_from_checkpoint": checkpoint.id.clone(),
                    "restored": true
                }),
                serde_json::json!({"checkpoint_id": checkpoint.id}),
            )
            .await
            .unwrap();
        let events = engine.all_events(&session.id).await.unwrap();
        let (active, ids) = crate::agent::conversation_from_events(&events, &session.id).unwrap();
        assert_eq!(active[0].content, checkpoint.summary.clone().unwrap());
        assert_eq!(active.len(), 1 + (5 - expected_start));
        assert!(ids[0].is_none());
        assert_eq!(
            ids[1].as_deref(),
            Some(messages[expected_start].id.as_str())
        );
    }
    assert_eq!(engine.list_messages(&session.id).await.unwrap().len(), 5);
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.context_checkpoints_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
    let active_id = engine
        .all_events(&session.id)
        .await
        .unwrap()
        .into_iter()
        .rev()
        .find(|event| event.kind == "context.compacted")
        .and_then(|event| {
            event
                .payload
                .get("restored_from_checkpoint")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        });
    assert_eq!(active_id.as_deref(), Some(second.id.as_str()));
}

#[tokio::test]
async fn deleting_a_context_checkpoint_does_not_rewind_or_change_messages() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("delete restore".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "delete restore".into(), None)
        .await
        .unwrap();
    let message = engine
        .append_message(
            &session.id,
            "user",
            "delete restore message",
            serde_json::json!([]),
            None,
        )
        .await
        .unwrap();
    let checkpoint = durable_context_checkpoint(
        &session,
        &project.id,
        "ctx-delete-restore",
        Some(message.id.as_str()),
    );
    engine
        .record_context_checkpoint(checkpoint.clone())
        .await
        .unwrap();
    engine
        .append_context_restore_events(
            &session.id,
            serde_json::json!({
                "mode": "summary",
                "through_message_id": message.id,
                "summary": "Earlier conversation summary",
                "restored_from_checkpoint": checkpoint.id
            }),
            serde_json::json!({"checkpoint_id": checkpoint.id}),
        )
        .await
        .unwrap();
    let before = engine.all_events(&session.id).await.unwrap();
    let (before_active, _) = crate::agent::conversation_from_events(&before, &session.id).unwrap();
    let deleted = engine
        .delete_context_checkpoint(&session.id, &checkpoint.id)
        .await
        .unwrap();
    assert!(deleted.removed);
    let after = engine.all_events(&session.id).await.unwrap();
    let (after_active, _) = crate::agent::conversation_from_events(&after, &session.id).unwrap();
    assert_eq!(
        before_active
            .iter()
            .map(|message| message.content.clone())
            .collect::<Vec<_>>(),
        after_active
            .iter()
            .map(|message| message.content.clone())
            .collect::<Vec<_>>()
    );
    assert_eq!(engine.list_messages(&session.id).await.unwrap().len(), 1);
    let replay = engine.replay_session(&session.id).await.unwrap();
    assert!(replay.context_checkpoints_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
    assert!(
        engine
            .list_context_checkpoints(&session.id, Some(10))
            .await
            .unwrap()
            .checkpoints
            .is_empty()
    );
}

#[tokio::test]
async fn bundle_round_trip_preserves_checkpoint_state_and_source_sequence() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some("checkpoint state".into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), "checkpoint state".into(), None)
        .await
        .unwrap();
    let mut checkpoint = durable_context_checkpoint(&session, &project.id, "ctx-state-1", None);
    checkpoint.structured_state = Some(vp_protocol::types::ContextStructuredState {
        goal: "preserve state".into(),
        plan_revision: 3,
        active_task_id: Some("T3".into()),
        active_task_title: Some("state task".into()),
        active_task_department: Some("runtime".into()),
        active_task_acceptance: vec!["tests".into(), "adr".into()],
        active_task_write_scope: vec!["crates/vp-core".into()],
        recent_completed_task_ids: vec!["T1".into(), "T2".into()],
        generated_at: "2026-11-11T00:00:00Z".into(),
    });
    checkpoint.plan_revision = 3;
    checkpoint.source_compaction_seq = Some(17);
    engine
        .record_context_checkpoint(checkpoint.clone())
        .await
        .unwrap();
    let bundle = engine.export_session(&session.id).await.unwrap();
    let bundled = bundle
        .context_checkpoints
        .iter()
        .find(|row| row.id == "ctx-state-1")
        .unwrap();
    assert_eq!(bundled.plan_revision, 3);
    assert_eq!(bundled.source_compaction_seq, Some(17));
    assert_eq!(
        bundled
            .structured_state
            .as_ref()
            .unwrap()
            .active_task_acceptance,
        vec!["tests".to_string(), "adr".to_string()]
    );

    let import_data = TempDir::new().unwrap();
    let imported_engine = Engine::new(Store::open(import_data.path()).unwrap());
    imported_engine.import_session(bundle).await.unwrap();
    let imported = imported_engine
        .list_context_checkpoints(&session.id, Some(10))
        .await
        .unwrap();
    assert_eq!(imported.checkpoints[0].plan_revision, 3);
    assert_eq!(imported.checkpoints[0].source_compaction_seq, Some(17));
    assert_eq!(
        imported.checkpoints[0]
            .structured_state
            .as_ref()
            .unwrap()
            .active_task_id
            .as_deref(),
        Some("T3")
    );
}

#[tokio::test]
async fn database_rebuild_preserves_context_checkpoint_policy() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let session_id = {
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("policy rebuild".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "policy rebuild".into(), None)
            .await
            .unwrap();
        engine
            .set_context_checkpoint_policy(
                &project.id,
                &vp_protocol::types::ContextCheckpointPolicy {
                    enabled: true,
                    max_checkpoints: 9,
                    max_age_days: 14,
                },
            )
            .await
            .unwrap();
        let checkpoint =
            durable_context_checkpoint(&session, &project.id, "ctx-policy-rebuild", None);
        engine.record_context_checkpoint(checkpoint).await.unwrap();
        engine.record_project_manifest().await.unwrap();
        session.id
    };
    let report = crate::rebuild::rebuild_database(data.path(), false)
        .await
        .unwrap();
    assert_eq!(report.sessions, 1);
    assert!(report.skipped.is_empty(), "{:?}", report.skipped);
    let store = Store::open(data.path()).unwrap();
    let projects = store.list_projects().unwrap();
    let policy = store
        .get_context_checkpoint_policy(&projects[0].id)
        .unwrap();
    assert!(policy.enabled);
    assert_eq!(policy.max_checkpoints, 9);
    assert_eq!(policy.max_age_days, 14);
    assert_eq!(
        store
            .list_context_checkpoints(&session_id, 10)
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn dashboard_alerts_surface_gui_contention_and_out_of_scope_holders() {
    use crate::gui::{GuiAcquire, GuiSurface};
    use vp_protocol::types::GuiPolicy;

    let data = TempDir::new().unwrap();
    let repo_a = TempDir::new().unwrap();
    let repo_b = TempDir::new().unwrap();
    init_repo(repo_a.path());
    init_repo(repo_b.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project_a = engine
        .add_project(
            repo_a.path().to_string_lossy().to_string(),
            Some("gui alpha".into()),
        )
        .await
        .unwrap();
    let project_b = engine
        .add_project(
            repo_b.path().to_string_lossy().to_string(),
            Some("gui beta".into()),
        )
        .await
        .unwrap();
    let holder = engine
        .create_session(project_a.id.clone(), "holder".into(), None)
        .await
        .unwrap();
    let waiter = engine
        .create_session(project_a.id.clone(), "waiter".into(), None)
        .await
        .unwrap();
    let other = engine
        .create_session(project_b.id.clone(), "other".into(), None)
        .await
        .unwrap();
    let policy = GuiPolicy {
        grace_secs: 30,
        wait_ms: 3_000,
        ..GuiPolicy::default()
    };
    let ticket = match engine
        .gui_begin_action_with_policy(
            &policy,
            GuiSurface::Computer,
            &holder.id,
            "agent-a",
            "Agent A",
        )
        .await
        .unwrap()
    {
        GuiAcquire::Acquired(ticket) => ticket,
        GuiAcquire::Busy(busy) => panic!("holder was busy: {busy}"),
    };
    let waiter_engine = engine.clone();
    let waiter_policy = policy.clone();
    let waiter_session = waiter.id.clone();
    let waiting = tokio::spawn(async move {
        waiter_engine
            .gui_begin_action_with_policy(
                &waiter_policy,
                GuiSurface::Browser,
                &waiter_session,
                "agent-b",
                "Agent B",
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let snapshot = engine
        .dashboard_snapshot(Some(&project_a.id))
        .await
        .unwrap();
    let queue_alert = snapshot
        .alerts
        .iter()
        .find(|alert| alert.kind == "gui_queue")
        .expect("gui queue alert");
    assert_eq!(queue_alert.severity, "warning");
    assert_eq!(queue_alert.action, "team");
    assert!(queue_alert.detail.contains("bounded wait queue"));
    assert_eq!(queue_alert.session_id.as_deref(), Some(holder.id.as_str()));

    // Filtering to the other project must not claim the held surface belongs
    // to that project's active set.
    let other_snapshot = engine
        .dashboard_snapshot(Some(&project_b.id))
        .await
        .unwrap();
    let stale = other_snapshot
        .alerts
        .iter()
        .find(|alert| alert.kind == "gui_stale_holder")
        .expect("stale holder alert");
    // The filtered dashboard does not resolve the out-of-scope session's
    // project; the holder session id and expiry are still actionable.
    assert_eq!(stale.project_id, None);
    assert!(stale.detail.contains("Agent A"));

    assert!(engine.gui_end_session(&holder.id, "handoff").await);
    let waiter_ticket = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    match waiter_ticket {
        GuiAcquire::Acquired(ticket) => assert_eq!(ticket.session_id, waiter.id),
        GuiAcquire::Busy(busy) => panic!("waiter timed out: {busy}"),
    }
    engine.gui_end_session(&waiter.id, "cleanup").await;
    let _ = (ticket, other);
    let after = engine
        .dashboard_snapshot(Some(&project_a.id))
        .await
        .unwrap();
    assert!(
        !after
            .alerts
            .iter()
            .any(|alert| alert.kind == "gui_queue" || alert.kind == "gui_stale_holder")
    );
}
