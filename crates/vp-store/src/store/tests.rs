use super::*;
use tempfile::TempDir;

fn test_store() -> (TempDir, Store, TempDir) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    (data, store, repo)
}

#[test]
fn adds_and_lists_projects() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    assert_eq!(project.name, "demo");
    let projects = store.list_projects().unwrap();
    assert_eq!(projects.len(), 1);
    assert_eq!(projects[0].id, project.id);
    assert_eq!(store.project_count().unwrap(), 1);
}

#[test]
fn rejects_duplicate_project() {
    let (_data, store, repo) = test_store();
    store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let error = store
        .add_project(repo.path().to_str().unwrap(), Some("again"))
        .unwrap_err();
    assert!(error.to_string().contains("already added"));
}

#[test]
fn creates_sessions_and_events() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ship a feature").unwrap();
    assert_eq!(session.status, "draft");

    let first = store
        .append_event(
            &session.id,
            "session.created",
            serde_json::json!({"goal": "ship a feature"}),
        )
        .unwrap();
    let second = store
        .append_event(&session.id, "note", serde_json::json!({"text": "hello"}))
        .unwrap();
    assert_eq!(first.seq, 1);
    assert_eq!(second.seq, 2);

    let events = store.list_events(&session.id, 1, 10).unwrap();
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, "note");

    let path = store
        .data_dir()
        .join("sessions")
        .join(&session.id)
        .join("events.jsonl");
    let mirror = std::fs::read_to_string(path).unwrap();
    assert_eq!(mirror.lines().count(), 2);
}

#[test]
fn round_trips_session_priority() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let low = store.create_session(&project.id, "low").unwrap();
    assert_eq!(low.priority, 0);
    let high = store
        .create_session_with_limits_and_priority(&project.id, "high", Some(1000), None, None, 42)
        .unwrap();
    assert_eq!(high.priority, 42);
    assert_eq!(store.get_session(&high.id).unwrap().unwrap().priority, 42);
    let sessions = store.list_sessions(Some(&project.id)).unwrap();
    assert_eq!(
        sessions
            .iter()
            .find(|session| session.id == high.id)
            .unwrap()
            .priority,
        42
    );
}

#[test]
fn searches_messages_and_events_with_a_project_filter() {
    let (_data, store, repo) = test_store();
    let other_repo = TempDir::new().unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let other = store
        .add_project(other_repo.path().to_str().unwrap(), Some("other"))
        .unwrap();
    let session = store
        .create_session(&project.id, "add rate limiting")
        .unwrap();
    let other_session = store
        .create_session(&other.id, "other rate limiting work")
        .unwrap();
    store
        .append_message(
            &session.id,
            "user",
            "Implement rate limiting for the API",
            &serde_json::json!([]),
            None,
        )
        .unwrap();
    store
        .append_event(
            &session.id,
            "tool.finished",
            serde_json::json!({"output": "rate limiting enabled"}),
        )
        .unwrap();
    store
        .append_message(
            &other_session.id,
            "user",
            "rate limiting in another project",
            &serde_json::json!([]),
            None,
        )
        .unwrap();
    store
        .append_message(
            &session.id,
            "user",
            "progress is 100% done",
            &serde_json::json!([]),
            None,
        )
        .unwrap();

    let all = store.search("rate limiting", None, 10).unwrap();
    assert_eq!(all.len(), 3);
    let scoped = store
        .search("rate limiting", Some(&project.id), 10)
        .unwrap();
    assert_eq!(scoped.len(), 2);
    assert!(scoped.iter().all(|hit| hit.project_id == project.id));
    assert!(scoped.iter().any(|hit| hit.source == "message"));
    assert!(scoped.iter().any(|hit| hit.source == "event"));

    let literal = store.search("100%", Some(&project.id), 10).unwrap();
    assert_eq!(literal.len(), 1);
    assert!(literal[0].snippet.contains("100%"));
}

#[test]
fn creates_lists_and_updates_agents() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "team").unwrap();
    let leader = store.create_leader_agent(&session.id, "model").unwrap();
    let worker = store
        .create_agent(
            &session.id,
            "worker",
            "Worker 1",
            None,
            Some(&leader),
            "model",
        )
        .unwrap();
    assert_eq!(worker.status, "running");
    store.update_agent_status(&worker.id, "idle").unwrap();
    let agents = store.list_agents(&session.id).unwrap();
    assert_eq!(agents.len(), 2);
    let leader_agent = agents.iter().find(|agent| agent.role == "leader").unwrap();
    let worker_agent = agents.iter().find(|agent| agent.role == "worker").unwrap();
    assert_eq!(worker_agent.name, "Worker 1");
    assert_eq!(worker_agent.status, "idle");
    assert_eq!(worker_agent.parent_id.as_deref(), Some(leader.as_str()));
    assert_eq!(leader_agent.id, leader);
}

#[test]
fn replaces_session_todos() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "plan").unwrap();
    store
        .set_todos(
            &session.id,
            &[
                TodoItem {
                    title: "Inspect".into(),
                    status: "completed".into(),
                },
                TodoItem {
                    title: "Implement".into(),
                    status: "in_progress".into(),
                },
            ],
        )
        .unwrap();
    let todos = store.list_todos(&session.id).unwrap();
    assert_eq!(todos.len(), 2);
    assert_eq!(todos[1].title, "Implement");
    store
        .set_todos(
            &session.id,
            &[TodoItem {
                title: "Ship".into(),
                status: "pending".into(),
            }],
        )
        .unwrap();
    let todos = store.list_todos(&session.id).unwrap();
    assert_eq!(todos.len(), 1);
    assert_eq!(todos[0].title, "Ship");
}

#[test]
fn accumulates_session_usage() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "usage").unwrap();
    store.add_session_usage(&session.id, 10, 5).unwrap();
    store.add_session_usage(&session.id, 20, 7).unwrap();
    let session = store.get_session(&session.id).unwrap().unwrap();
    assert_eq!(session.tokens_in, 30);
    assert_eq!(session.tokens_out, 12);
}

#[test]
fn round_trips_worktree_retention_policy() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    assert_eq!(
        store.get_worktree_policy(&project.id).unwrap(),
        WorktreeRetentionPolicy::default()
    );
    let policy = WorktreeRetentionPolicy {
        enabled: true,
        older_than_secs: 86_400,
        delete_branches: true,
        keep_failed: false,
    };
    store.set_worktree_policy(&project.id, &policy).unwrap();
    assert_eq!(store.get_worktree_policy(&project.id).unwrap(), policy);
    let enabled = store.list_enabled_worktree_policies().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].0, project.id);
    store
        .set_worktree_policy(
            &project.id,
            &WorktreeRetentionPolicy {
                enabled: false,
                ..policy
            },
        )
        .unwrap();
    assert!(store.list_enabled_worktree_policies().unwrap().is_empty());
}

#[test]
fn records_and_reads_the_latest_project_manifest_snapshot() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    store
        .set_shell_policy(
            &project.id,
            &ShellPolicy {
                network: "deny".into(),
                read_deny: vec!["~/secret".into()],
                restrict_home: true,
            },
        )
        .unwrap();
    store
        .set_project_budget(
            &project.id,
            &ProjectBudget {
                token_budget: Some(1234),
                cost_budget_usd: Some(2.5),
            },
        )
        .unwrap();
    store.record_project_manifest().unwrap();
    store
        .set_shell_policy(
            &project.id,
            &ShellPolicy {
                network: "allow".into(),
                read_deny: Vec::new(),
                restrict_home: false,
            },
        )
        .unwrap();
    store.record_project_manifest().unwrap();

    let snapshots = read_project_manifest(data.path()).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].project.id, project.id);
    assert_eq!(snapshots[0].shell_policy.network, "allow");
    assert!(!snapshots[0].shell_policy.restrict_home);
    assert_eq!(snapshots[0].budget.token_budget, Some(1234));
    assert_eq!(snapshots[0].budget.cost_budget_usd, Some(2.5));
}

#[test]
fn groups_project_usage_by_session() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let alpha = store.create_session(&project.id, "alpha").unwrap();
    let beta = store.create_session(&project.id, "beta").unwrap();
    store
        .add_usage_entry_with_events(
            &alpha.id, "a1", "Alpha", "worker", None, None, "m", 10, 5, false, 0.5,
        )
        .unwrap();
    store
        .add_usage_entry_with_events(
            &alpha.id, "a1", "Alpha", "worker", None, None, "m", 2, 1, true, 0.0,
        )
        .unwrap();
    store
        .add_usage_entry_with_events(
            &beta.id, "b1", "Beta", "leader", None, None, "m", 7, 3, false, 0.25,
        )
        .unwrap();

    let usage = store.project_usage_by_session(&project.id).unwrap();
    assert_eq!(usage.len(), 2);
    let alpha_usage = usage.iter().find(|row| row.session_id == alpha.id).unwrap();
    assert_eq!(alpha_usage.calls, 2);
    assert_eq!(alpha_usage.input_tokens, 12);
    assert_eq!(alpha_usage.output_tokens, 6);
    assert!((alpha_usage.cost_usd - 0.5).abs() < 1e-9);
    let beta_usage = usage.iter().find(|row| row.session_id == beta.id).unwrap();
    assert_eq!(beta_usage.calls, 1);
    assert_eq!(beta_usage.input_tokens, 7);
}

#[test]
fn creates_answers_and_expires_questions() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "questions").unwrap();
    let (question, event) = store
        .create_question_with_event(
            &session.id,
            "agent-1",
            "Leader",
            "Which database should I use?",
            &["SQLite".to_string(), "Postgres".to_string()],
        )
        .unwrap();
    assert_eq!(event.kind, "question.requested");
    assert_eq!(question.status, "pending");
    assert_eq!(store.list_questions(Some(&session.id)).unwrap().len(), 1);

    let (answered, answer_event) = store
        .answer_question_with_event(&question.id, "SQLite")
        .unwrap()
        .unwrap();
    assert_eq!(answered.status, "answered");
    assert_eq!(answered.answer.as_deref(), Some("SQLite"));
    assert_eq!(answer_event.kind, "question.answered");
    assert!(
        store
            .answer_question_with_event(&question.id, "again")
            .unwrap()
            .is_none()
    );

    let (expired, expired_event) = store
        .create_question_with_event(&session.id, "agent-1", "Leader", "Continue?", &[])
        .unwrap();
    let closed = store
        .close_question_with_event(&expired.id, "expired")
        .unwrap()
        .unwrap();
    assert_eq!(closed.0.status, "expired");
    assert_eq!(expired_event.kind, "question.requested");
    assert_eq!(closed.1.kind, "question.expired");
}

#[test]
fn manages_mcp_server_configs() {
    let (_data, store, _repo) = test_store();
    let mut env = std::collections::BTreeMap::new();
    env.insert("TOKEN".to_string(), "hidden".to_string());
    let server = store
        .add_mcp_server(
            "files",
            "mcp-files",
            &["--root".to_string(), "/tmp".to_string()],
            &env,
            true,
        )
        .unwrap();
    assert_eq!(server.name, "files");
    assert_eq!(store.list_mcp_servers().unwrap().len(), 1);
    assert_eq!(
        store.get_mcp_server(&server.id).unwrap().unwrap().args,
        vec!["--root", "/tmp"]
    );
    let updated = store
        .update_mcp_server(
            &server.id,
            "files-2",
            "mcp-files",
            &[],
            &std::collections::BTreeMap::new(),
            false,
        )
        .unwrap()
        .unwrap();
    assert_eq!(updated.name, "files-2");
    assert!(!updated.enabled);
    assert!(store.remove_mcp_server(&server.id).unwrap());
    assert!(store.list_mcp_servers().unwrap().is_empty());
}

#[test]
fn creates_and_filters_project_memories() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "memory").unwrap();
    let (first, event) = store
        .create_memory_with_event(
            &session.id,
            &project.id,
            "agent-1",
            "Leader",
            "decision",
            "Use deterministic merge",
            "Keep the Integrator deterministic.",
            &["architecture".to_string()],
        )
        .unwrap();
    assert_eq!(event.kind, "memory.created");
    assert_eq!(event.payload["title"], "Use deterministic merge");
    store
        .create_memory_with_event(
            &session.id,
            &project.id,
            "agent-1",
            "Leader",
            "source",
            "Hermes paper",
            "A source about agent memory.",
            &["research".to_string()],
        )
        .unwrap();

    let all = store.list_memories(&project.id, None, None, 10).unwrap();
    assert_eq!(all.len(), 2);
    let decisions = store
        .list_memories(&project.id, Some("deterministic"), None, 10)
        .unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].id, first.id);
    let sources = store
        .list_memories(&project.id, None, Some("source"), 10)
        .unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(store.list_session_memories(&session.id).unwrap().len(), 2);

    store.replace_session_memories(&session.id, &[]).unwrap();
    assert!(store.list_session_memories(&session.id).unwrap().is_empty());
}

#[test]
fn searches_memories_with_fts_and_falls_back_to_like() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "memory search").unwrap();
    store
        .create_memory_with_event(
            &session.id,
            &project.id,
            "agent-1",
            "Leader",
            "decision",
            "Caching layer",
            "Use SQLite FTS5 for recall.",
            &["architecture".to_string()],
        )
        .unwrap();
    store
        .create_memory_with_event(
            &session.id,
            &project.id,
            "agent-1",
            "Leader",
            "fact",
            "本地检索",
            "项目记忆支持本地检索。",
            &["memory".to_string()],
        )
        .unwrap();

    let fts = store
        .list_memories(&project.id, Some("FTS5"), None, 10)
        .unwrap();
    assert_eq!(fts.len(), 1);
    assert_eq!(fts[0].title, "Caching layer");
    let fallback = store
        .list_memories(&project.id, Some("本地检索"), None, 10)
        .unwrap();
    assert_eq!(fallback.len(), 1);
    assert_eq!(fallback[0].title, "本地检索");
    assert!(
        store
            .list_memories(&project.id, Some("missing-term"), None, 10)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn deletes_and_prunes_memories_with_events() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store
        .create_session(&project.id, "memory retention")
        .unwrap();
    let mut memories = Vec::new();
    for index in 0..3 {
        let (memory, _) = store
            .create_memory_with_event(
                &session.id,
                &project.id,
                "agent-1",
                "Leader",
                "note",
                &format!("memory {index}"),
                &format!("retention body {index}"),
                &[],
            )
            .unwrap();
        memories.push(memory);
    }

    let removed = store
        .delete_memories_with_events(&[memories[0].id.clone()])
        .unwrap();
    assert_eq!(removed.len(), 1);
    assert_eq!(removed[0].0.id, memories[0].id);
    assert_eq!(removed[0].1.kind, "memory.deleted");
    assert_eq!(removed[0].1.payload["id"], memories[0].id);
    assert!(
        store
            .list_memories(&project.id, Some("retention"), None, 10)
            .unwrap()
            .iter()
            .all(|memory| memory.id != memories[0].id)
    );

    let ids = store
        .memory_ids_for_prune(&project.id, None, Some(1))
        .unwrap();
    assert_eq!(ids.len(), 1);
    assert_eq!(ids[0], memories[1].id);
    let pruned = store.delete_memories_with_events(&ids).unwrap();
    assert_eq!(pruned.len(), 1);
    assert_eq!(
        store
            .list_memories(&project.id, None, None, 10)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(pruned[0].1.kind, "memory.deleted");
}

#[test]
fn prunes_memories_older_than_a_cutoff() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "memory age").unwrap();
    let (old, _) = store
        .create_memory_with_event(
            &session.id,
            &project.id,
            "agent-1",
            "Leader",
            "note",
            "old memory",
            "body",
            &[],
        )
        .unwrap();
    store
        .create_memory_with_event(
            &session.id,
            &project.id,
            "agent-1",
            "Leader",
            "note",
            "new memory",
            "body",
            &[],
        )
        .unwrap();
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE memories SET created_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
            params![old.id],
        )
        .unwrap();
    }
    let ids = store
        .memory_ids_for_prune(&project.id, Some(30), None)
        .unwrap();
    assert_eq!(ids, vec![old.id]);
}

#[test]
fn round_trips_memory_retention_policy() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    assert_eq!(
        store.get_memory_policy(&project.id).unwrap(),
        MemoryPolicy::default()
    );
    let policy = MemoryPolicy {
        enabled: true,
        max_items: Some(200),
        max_age_days: Some(30),
    };
    store.set_memory_policy(&project.id, &policy).unwrap();
    assert_eq!(store.get_memory_policy(&project.id).unwrap(), policy);
    let enabled = store.list_enabled_memory_policies().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].0, project.id);
    assert_eq!(enabled[0].1, policy);
    store
        .set_memory_policy(
            &project.id,
            &MemoryPolicy {
                enabled: false,
                ..policy
            },
        )
        .unwrap();
    assert!(store.list_enabled_memory_policies().unwrap().is_empty());
}

#[test]
fn keeps_queued_sessions_and_their_prompts_across_recovery() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "queued").unwrap();
    store
        .append_event(
            &session.id,
            "agent.queued",
            serde_json::json!({"prompt": "continue the queue"}),
        )
        .unwrap();
    store.update_session_status(&session.id, "queued").unwrap();
    let urgent = store
        .create_session_with_limits_and_priority(&project.id, "urgent queued", None, None, None, 10)
        .unwrap();
    store
        .append_event(
            &urgent.id,
            "agent.queued",
            serde_json::json!({"prompt": "urgent work"}),
        )
        .unwrap();
    store.update_session_status(&urgent.id, "queued").unwrap();
    let (recovered, _) = store.recover_state().unwrap();
    assert!(recovered.is_empty());
    assert_eq!(
        store.get_session(&session.id).unwrap().unwrap().status,
        "queued"
    );
    let prompts = store.queued_session_prompts().unwrap();
    assert_eq!(
        prompts,
        vec![
            (urgent.id, project.id.clone(), "urgent work".to_string(), 10),
            (session.id, project.id, "continue the queue".to_string(), 0)
        ]
    );
}

#[test]
fn writes_approval_projections_and_events_atomically() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "approval").unwrap();
    let (approval, requested) = store
        .create_approval_with_event(
            &session.id,
            "call-1",
            "shell",
            &serde_json::json!({"command": "true"}),
            Some(&serde_json::json!({"agent_id": "agent-1"})),
        )
        .unwrap();
    assert_eq!(requested.kind, "approval.requested");
    assert_eq!(requested.payload["agent_id"], "agent-1");
    let (resolved, event) = store
        .resolve_approval_with_event(&approval.id, true)
        .unwrap()
        .unwrap();
    assert_eq!(resolved.status, "approved");
    assert_eq!(event.kind, "approval.resolved");
    assert_eq!(event.payload["approved"], true);
    assert!(
        store
            .resolve_approval_with_event(&approval.id, false)
            .unwrap()
            .is_none()
    );
    let events = store.list_all_events(&session.id).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "approval.requested")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "approval.resolved")
            .count(),
        1
    );
}

#[test]
fn interrupts_running_sessions_on_recovery() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "run").unwrap();
    store.update_session_status(&session.id, "running").unwrap();
    let paused = store.create_session(&project.id, "pause").unwrap();
    store.update_session_status(&paused.id, "paused").unwrap();
    let queued = store.create_session(&project.id, "queue").unwrap();
    store.update_session_status(&queued.id, "queued").unwrap();
    let interrupted = store.interrupt_running_sessions().unwrap();
    assert!(interrupted.contains(&session.id));
    assert!(interrupted.contains(&paused.id));
    assert!(interrupted.contains(&queued.id));
    assert_eq!(
        store.get_session(&session.id).unwrap().unwrap().status,
        "failed"
    );
    assert_eq!(
        store.get_session(&paused.id).unwrap().unwrap().status,
        "failed"
    );
    assert_eq!(
        store.get_session(&queued.id).unwrap().unwrap().status,
        "failed"
    );
    assert!(store.interrupt_running_sessions().unwrap().is_empty());
}

#[test]
fn migrates_legacy_sessions_table_with_worktree_columns() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    {
        let conn = Connection::open(data.path().join("verlinpark.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE sessions (
                    id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    goal TEXT NOT NULL,
                    status TEXT NOT NULL,
                    created_at TEXT NOT NULL
                );",
        )
        .unwrap();
    }
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "legacy").unwrap();
    assert!(session.worktree.is_none());
    assert!(session.branch.is_none());
    store
        .set_session_worktree(&session.id, "/tmp/worktree", "verlinpark/test")
        .unwrap();
    let loaded = store.get_session(&session.id).unwrap().unwrap();
    assert_eq!(loaded.worktree.as_deref(), Some("/tmp/worktree"));
    assert_eq!(loaded.branch.as_deref(), Some("verlinpark/test"));
}

#[test]
fn fresh_schema_keeps_artifact_and_mailbox_policy_columns_distinct() {
    let (_data, store, _repo) = test_store();
    let conn = store.lock().unwrap();
    let columns = |table: &str| {
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        stmt.query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>()
    };
    let artifact = columns("project_artifact_policies");
    assert_eq!(
        artifact
            .iter()
            .filter(|column| column.as_str() == "auto_gc")
            .count(),
        1
    );
    assert!(!artifact.iter().any(|column| column == "delete_unread"));
    let mailbox = columns("project_mailbox_policies");
    assert_eq!(
        mailbox
            .iter()
            .filter(|column| column.as_str() == "auto_gc")
            .count(),
        1
    );
    assert!(mailbox.iter().any(|column| column == "delete_unread"));
}

#[test]
fn integrity_check_reports_ok() {
    let (_data, store, _repo) = test_store();
    assert_eq!(store.integrity_check().unwrap(), "ok");
}

#[test]
fn persists_mailbox_and_artifacts() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "mailbox").unwrap();
    let (message, event) = store
        .create_agent_message_with_event(
            &session.id,
            "agent-a",
            "Leader",
            "agent-b",
            "Worker",
            "handoff",
            "Please implement the API",
            &serde_json::json!({"task": "T1"}),
        )
        .unwrap();
    assert_eq!(message.status, "unread");
    assert_eq!(event.kind, "mailbox.message_sent");
    let messages = store
        .list_agent_messages(&session.id, Some("agent-b"), true)
        .unwrap();
    assert_eq!(messages.len(), 1);
    assert_eq!(messages[0].body, "Please implement the API");
    let (read, event) = store.mark_agent_message_read(&message.id).unwrap().unwrap();
    assert_eq!(read.status, "read");
    assert_eq!(event.unwrap().kind, "mailbox.message_read");
    assert!(
        store
            .list_agent_messages(&session.id, Some("agent-b"), true)
            .unwrap()
            .is_empty()
    );

    let (artifact, event) = store
        .create_artifact_with_event(
            &session.id,
            "agent-b",
            "Worker",
            Some("T1"),
            "api-notes.md",
            "note",
            "text/markdown",
            "# API notes\n",
            "inline",
            12,
            "test-sha",
            None,
        )
        .unwrap();
    assert_eq!(event.kind, "artifact.published");
    let artifacts = store.list_artifacts(&session.id).unwrap();
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].name, "api-notes.md");
    assert_eq!(
        store.get_artifact(&artifact.id).unwrap().unwrap().content,
        "# API notes\n"
    );
}

#[test]
fn persists_approvals_and_resolves_once() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ship a feature").unwrap();
    let approval = store
        .create_approval(
            &session.id,
            "call_1",
            "write_file",
            &serde_json::json!({"path": "README.md"}),
        )
        .unwrap();
    assert_eq!(store.list_approvals(Some(&session.id)).unwrap().len(), 1);

    let resolved = store.resolve_approval(&approval.id, true).unwrap().unwrap();
    assert_eq!(resolved.status, "approved");
    assert!(
        store
            .resolve_approval(&approval.id, true)
            .unwrap()
            .is_none()
    );
    let listed = store.list_approvals(Some(&session.id)).unwrap();
    assert_eq!(listed[0].status, "approved");
}

#[test]
fn persists_and_updates_merge_queue_entries() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "merge").unwrap();
    let entry = store
        .enqueue_merge(
            &session.id,
            "agent_1",
            "Worker 1",
            Some("T1"),
            "verlinpark/task",
            "abc123",
            "def456",
            Some("verifier approved"),
        )
        .unwrap();
    assert_eq!(entry.status, "queued");
    assert_eq!(entry.attempt, 1);
    assert_eq!(entry.task_id.as_deref(), Some("T1"));

    let updated = store
        .update_merge_queue_entry(&entry.id, "merged", Some("ok"))
        .unwrap()
        .unwrap();
    assert_eq!(updated.status, "merged");
    assert_eq!(updated.detail.as_deref(), Some("ok"));
    let listed = store.list_merge_queue(&session.id).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, entry.id);
}

#[test]
fn interrupts_in_flight_merge_queue_entries() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "recover").unwrap();
    let entry = store
        .enqueue_merge(
            &session.id,
            "agent_1",
            "Worker 1",
            None,
            "verlinpark/task",
            "abc123",
            "def456",
            None,
        )
        .unwrap();
    store
        .update_merge_queue_entry(&entry.id, "merging", None)
        .unwrap();
    let interrupted = store.interrupt_merge_queue().unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].status, "interrupted");
    assert!(store.interrupt_merge_queue().unwrap().is_empty());
}

#[test]
fn lists_sessions_by_project() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    store.create_session(&project.id, "one").unwrap();
    store.create_session(&project.id, "two").unwrap();
    let sessions = store.list_sessions(Some(&project.id)).unwrap();
    assert_eq!(sessions.len(), 2);
    assert_eq!(store.list_sessions(None).unwrap().len(), 2);
}

#[test]
fn updates_session_priority_with_an_event() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "priority").unwrap();
    let event = store
        .set_session_priority_with_event(&session.id, 42)
        .unwrap();
    assert_eq!(event.kind, "session.priority");
    assert_eq!(event.payload["priority"], 42);
    assert_eq!(
        store.get_session(&session.id).unwrap().unwrap().priority,
        42
    );
    let events = store.list_events(&session.id, 1, 10).unwrap();
    assert!(events.iter().any(|event| event.kind == "session.priority"));
}

fn scheduled_plan() -> TeamPlan {
    use vp_protocol::types::{PlanDepartment, PlanTask};
    TeamPlan {
        goal: "schedule".into(),
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
                title: "One".into(),
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
                title: "Two".into(),
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

#[test]
fn persists_task_queue_claims_and_completions() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "schedule").unwrap();
    let submitted = store
        .append_event(
            &session.id,
            "team_plan.submitted",
            serde_json::json!({"goal": "schedule"}),
        )
        .unwrap();
    let tasks = store
        .sync_plan_tasks(&session.id, &project.id, submitted.seq, &scheduled_plan())
        .unwrap();
    assert_eq!(tasks.len(), 2);
    assert!(tasks.iter().all(|task| task.status == "pending"));
    assert_eq!(tasks[0].task_id, "T1");

    let claimed = store
        .claim_plan_tasks(&session.id, submitted.seq, &["T1".into()])
        .unwrap();
    assert_eq!(claimed.len(), 1);
    assert_eq!(claimed[0].status, "running");
    assert_eq!(claimed[0].attempts, 1);
    assert!(claimed[0].started_at.is_some());

    let finished = store
        .finish_plan_task(&session.id, submitted.seq, "T1", "completed", None)
        .unwrap()
        .unwrap();
    assert_eq!(finished.status, "completed");
    assert!(finished.finished_at.is_some());
    let queue = store.list_plan_tasks(Some(&project.id), None).unwrap();
    assert_eq!(
        queue
            .iter()
            .find(|task| task.task_id == "T1")
            .unwrap()
            .status,
        "completed"
    );

    // A rebuild from events preserves completed dependencies and queued tasks.
    store
        .append_event(
            &session.id,
            "plan_task.finished",
            serde_json::json!({"task_id": "T1", "task_status": "completed"}),
        )
        .unwrap();
    store
        .append_event(
            &session.id,
            "plan_task.started",
            serde_json::json!({"task_id": "T2", "task_status": "running"}),
        )
        .unwrap();
    let rebuilt = store
        .sync_plan_tasks(&session.id, &project.id, submitted.seq, &scheduled_plan())
        .unwrap();
    assert_eq!(
        rebuilt
            .iter()
            .find(|task| task.task_id == "T1")
            .unwrap()
            .status,
        "completed"
    );
    assert_eq!(
        rebuilt
            .iter()
            .find(|task| task.task_id == "T2")
            .unwrap()
            .status,
        "interrupted"
    );
}

#[test]
fn persistent_write_leases_block_overlapping_projects_and_survive_reopen() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let first = store.create_session(&project.id, "first").unwrap();
    let second = store.create_session(&project.id, "second").unwrap();
    assert!(
        store
            .try_acquire_write_lease(&project.id, &first.id, "agent-a", &["src/api".into()])
            .unwrap()
    );
    // Same-session workers may share a worktree.
    assert!(
        store
            .try_acquire_write_lease(
                &project.id,
                &first.id,
                "agent-a2",
                &["src/api/handler".into()]
            )
            .unwrap()
    );
    assert!(
        !store
            .try_acquire_write_lease(&project.id, &second.id, "agent-b", &["src/api".into()])
            .unwrap()
    );
    assert!(
        store
            .try_acquire_write_lease(&project.id, &second.id, "agent-b", &["docs".into()])
            .unwrap()
    );

    drop(store);
    let reopened = Store::open(data.path()).unwrap();
    let leases = reopened.list_write_leases(None, None).unwrap();
    assert_eq!(leases.len(), 3);
    assert!(leases.iter().any(|lease| lease.agent_id == "agent-a"));
    assert!(leases.iter().any(|lease| lease.agent_id == "agent-b"));
    reopened.release_write_lease("agent-a").unwrap();
    reopened.release_write_lease("agent-a2").unwrap();
    assert!(
        reopened
            .try_acquire_write_lease(&project.id, &second.id, "agent-c", &["src/api".into()])
            .unwrap()
    );
}

#[test]
fn recovery_interrupts_tasks_and_clears_leases() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "recover tasks").unwrap();
    let submitted = store
        .append_event(
            &session.id,
            "team_plan.submitted",
            serde_json::json!({"goal": "recover"}),
        )
        .unwrap();
    store
        .sync_plan_tasks(&session.id, &project.id, submitted.seq, &scheduled_plan())
        .unwrap();
    store
        .claim_plan_tasks(&session.id, submitted.seq, &["T1".into()])
        .unwrap();
    store
        .try_acquire_write_lease(&project.id, &session.id, "agent-a", &["src/one".into()])
        .unwrap();
    store.update_session_status(&session.id, "running").unwrap();

    let (recovered, events) = store.recover_state().unwrap();
    assert_eq!(recovered, vec![session.id.clone()]);
    assert!(store.list_write_leases(None, None).unwrap().is_empty());
    let tasks = store.list_plan_tasks(Some(&project.id), None).unwrap();
    assert_eq!(
        tasks
            .iter()
            .find(|task| task.task_id == "T1")
            .unwrap()
            .status,
        "interrupted"
    );
    assert!(events.iter().any(|event| {
        event.kind == "plan_task.interrupted" && event.payload["task_id"] == "T1"
    }));
}
