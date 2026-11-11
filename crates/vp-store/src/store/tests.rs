use super::*;
use sha2::{Digest, Sha256};
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
                read_jail: false,
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
                read_jail: false,
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
            &alpha.id, "a1", "Alpha", "worker", None, None, "m", 10, 5, 4, 1, false, 0.5,
        )
        .unwrap();
    store
        .add_usage_entry_with_events(
            &alpha.id, "a1", "Alpha", "worker", None, None, "m", 2, 1, 0, 0, true, 0.0,
        )
        .unwrap();
    store
        .add_usage_entry_with_events(
            &beta.id, "b1", "Beta", "leader", None, None, "m", 7, 3, 2, 0, false, 0.25,
        )
        .unwrap();

    let usage = store.project_usage_by_session(&project.id).unwrap();
    assert_eq!(usage.len(), 2);
    let alpha_usage = usage.iter().find(|row| row.session_id == alpha.id).unwrap();
    assert_eq!(alpha_usage.calls, 2);
    assert_eq!(alpha_usage.input_tokens, 12);
    assert_eq!(alpha_usage.output_tokens, 6);
    assert_eq!(alpha_usage.cache_read_tokens, 4);
    assert_eq!(alpha_usage.cache_write_tokens, 1);
    assert!((alpha_usage.cost_usd - 0.5).abs() < 1e-9);
    let beta_usage = usage.iter().find(|row| row.session_id == beta.id).unwrap();
    assert_eq!(beta_usage.calls, 1);
    assert_eq!(beta_usage.input_tokens, 7);
    assert_eq!(beta_usage.cache_read_tokens, 2);
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
        .add_mcp_server(&vp_protocol::types::McpServerConfig {
            name: "files".into(),
            transport: vp_protocol::types::McpTransport::Stdio,
            command: "mcp-files".into(),
            args: vec!["--root".into(), "/tmp".into()],
            env: env.clone(),
            url: None,
            headers: Default::default(),
            enabled: true,
        })
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
            &vp_protocol::types::McpServerConfig {
                name: "files-2".into(),
                transport: vp_protocol::types::McpTransport::Http,
                command: String::new(),
                args: Vec::new(),
                env: Default::default(),
                url: Some("https://mcp.example.test/acp".into()),
                headers: std::collections::BTreeMap::from([(
                    "authorization".into(),
                    "Bearer ${MCP_TOKEN}".into(),
                )]),
                enabled: false,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(updated.name, "files-2");
    assert!(!updated.enabled);
    assert_eq!(updated.transport, vp_protocol::types::McpTransport::Http);
    assert_eq!(updated.url.as_deref(), Some("https://mcp.example.test/acp"));
    assert_eq!(
        updated.headers.get("authorization").map(String::as_str),
        Some("Bearer ${MCP_TOKEN}")
    );
    assert!(store.remove_mcp_server(&server.id).unwrap());
    assert!(store.list_mcp_servers().unwrap().is_empty());
}

#[test]
fn manages_acp_agent_configs() {
    let (_data, store, _repo) = test_store();
    let mut env = std::collections::BTreeMap::new();
    env.insert("API_KEY".to_string(), "hidden".to_string());
    let agent = store
        .add_acp_agent(
            "gemini",
            "npx",
            &["-y".to_string(), "gemini".to_string()],
            &env,
            true,
            false,
            900,
        )
        .unwrap();
    assert_eq!(agent.name, "gemini");
    assert_eq!(agent.timeout_secs, 900);
    assert!(!agent.auto_approve);
    assert_eq!(store.list_acp_agents().unwrap().len(), 1);
    assert_eq!(
        store.find_acp_agent("gemini").unwrap().unwrap().args,
        vec!["-y", "gemini"]
    );
    assert!(store.find_acp_agent("missing").unwrap().is_none());

    let updated = store
        .update_acp_agent(
            &agent.id,
            "gemini-cli",
            "npx",
            &[],
            &std::collections::BTreeMap::new(),
            false,
            true,
            120,
        )
        .unwrap()
        .unwrap();
    assert_eq!(updated.name, "gemini-cli");
    assert!(updated.auto_approve);
    assert!(!updated.enabled);
    assert!(store.remove_acp_agent(&agent.id).unwrap());
    assert!(store.list_acp_agents().unwrap().is_empty());
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
fn searches_messages_with_fts_prefix_and_like_fallback() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("search"))
        .unwrap();
    let session = store.create_session(&project.id, "search").unwrap();
    store
        .append_message(
            &session.id,
            "assistant",
            "hyperparameterization works well",
            &serde_json::json!([]),
            None,
        )
        .unwrap();
    store
        .append_message(
            &session.id,
            "user",
            "a wormhole is unrelated",
            &serde_json::json!([]),
            None,
        )
        .unwrap();

    let prefix = store.search("hyper", Some(&project.id), 10).unwrap();
    assert_eq!(prefix.len(), 1);
    assert!(prefix[0].snippet.contains("hyperparameterization"));

    // No FTS token starts with "param"; the LIKE fallback still finds the
    // substring inside "hyperparameterization".
    let substring = store.search("param", Some(&project.id), 10).unwrap();
    assert_eq!(substring.len(), 1);
    assert!(substring[0].snippet.contains("hyperparameterization"));

    store
        .append_message(
            &session.id,
            "user",
            "部署流程已经完成",
            &serde_json::json!([]),
            None,
        )
        .unwrap();
    let cjk = store.search("流程", Some(&project.id), 10).unwrap();
    assert_eq!(cjk.len(), 1);
    assert!(cjk[0].snippet.contains("部署流程"));
}

#[test]
fn rebuilds_the_search_index_for_an_existing_database() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("legacy"))
        .unwrap();
    let session = store.create_session(&project.id, "legacy").unwrap();
    store
        .append_message(
            &session.id,
            "user",
            "archived telescope notes",
            &serde_json::json!([]),
            None,
        )
        .unwrap();
    drop(store);

    let conn = Connection::open(data.path().join("verlinpark.db")).unwrap();
    for statement in [
        "DROP TRIGGER IF EXISTS messages_fts_insert",
        "DROP TRIGGER IF EXISTS messages_fts_delete",
        "DROP TRIGGER IF EXISTS messages_fts_update",
        "DROP TRIGGER IF EXISTS events_fts_insert",
        "DROP TRIGGER IF EXISTS events_fts_delete",
        "DROP TRIGGER IF EXISTS events_fts_update",
        "DROP TABLE IF EXISTS messages_fts",
        "DROP TABLE IF EXISTS events_fts",
        "DELETE FROM store_meta WHERE key = 'search_fts'",
    ] {
        conn.execute(statement, []).unwrap();
    }
    drop(conn);

    let reopened = Store::open(data.path()).unwrap();
    let hits = reopened.search("telescope", Some(&project.id), 10).unwrap();
    assert_eq!(hits.len(), 1);
    assert!(hits[0].snippet.contains("telescope"));
}

#[test]
fn searches_event_payloads_and_filters_projects() {
    let (_data, store, repo) = test_store();
    let first = store
        .add_project(repo.path().to_str().unwrap(), Some("one"))
        .unwrap();
    let second_repo = TempDir::new().unwrap();
    let second = store
        .add_project(second_repo.path().to_str().unwrap(), Some("two"))
        .unwrap();
    let first_session = store.create_session(&first.id, "one").unwrap();
    let second_session = store.create_session(&second.id, "two").unwrap();
    store
        .append_event(
            &first_session.id,
            "tool.finished",
            serde_json::json!({"output": "quasar reached"}),
        )
        .unwrap();
    store
        .append_event(
            &second_session.id,
            "tool.finished",
            serde_json::json!({"output": "quasar elsewhere"}),
        )
        .unwrap();

    let filtered = store.search("quasar", Some(&first.id), 10).unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].source, "event");
    assert_eq!(filtered[0].session_id, first_session.id);
    assert_eq!(filtered[0].kind.as_deref(), Some("tool.finished"));

    let all = store.search("quasar", None, 10).unwrap();
    assert_eq!(all.len(), 2);
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
fn prune_never_removes_pinned_memories() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "memory pins").unwrap();
    let mut memories = Vec::new();
    for index in 0..3 {
        let (memory, _) = store
            .create_memory_with_event(
                &session.id,
                &project.id,
                "agent-1",
                "Leader",
                "note",
                &format!("pinned {index}"),
                &format!("body {index}"),
                &[],
            )
            .unwrap();
        memories.push(memory);
    }
    let (pinned, event) = store
        .update_memory_with_event(&memories[2].id, None, Some(true), None)
        .unwrap()
        .unwrap();
    assert!(pinned.pinned);
    assert_eq!(event.kind, "memory.updated");

    // The pinned newest row is excluded before `keep_latest` is applied, so
    // only the row beyond the unpinned budget is a prune candidate.
    let ids = store
        .memory_ids_for_prune(&project.id, None, Some(1))
        .unwrap();
    assert_eq!(ids, vec![memories[0].id.clone()]);

    // Age-based pruning must also leave the pinned row alone.
    {
        let conn = store.lock().unwrap();
        for memory in &memories {
            conn.execute(
                "UPDATE memories SET created_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
                params![memory.id],
            )
            .unwrap();
        }
    }
    let mut by_age = store
        .memory_ids_for_prune(&project.id, Some(30), None)
        .unwrap();
    let mut expected = vec![memories[0].id.clone(), memories[1].id.clone()];
    by_age.sort();
    expected.sort();
    assert_eq!(by_age, expected);
    assert!(by_age.iter().all(|id| id != &memories[2].id));
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
        task_result_verify_days: Some(14),
        default_verify_days: None,
        auto_supersede: false,
        brief_max_chars: Some(2048),
        sweep_stale: true,
    };
    store.set_memory_policy(&project.id, &policy).unwrap();
    assert_eq!(store.get_memory_policy(&project.id).unwrap(), policy);
    let enabled = store.list_enabled_memory_policies().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].0, project.id);
    assert_eq!(enabled[0].1, policy);
    let sweeping = store.list_memory_sweep_policies().unwrap();
    assert_eq!(sweeping.len(), 1);
    assert_eq!(sweeping[0].1, policy);
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
    // Staleness sweeping is independent of the retention toggle.
    assert_eq!(store.list_memory_sweep_policies().unwrap().len(), 1);
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
            Some("workspace_write"),
            Some("shell command inside the sandbox"),
        )
        .unwrap();
    assert_eq!(requested.kind, "approval.requested");
    assert_eq!(requested.payload["agent_id"], "agent-1");
    assert_eq!(requested.payload["approval_risk"], "workspace_write");
    assert_eq!(
        requested.payload["approval_reason"],
        "shell command inside the sandbox"
    );
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
fn migrates_legacy_continuation_rows_as_one_shot() {
    let data = TempDir::new().unwrap();
    {
        let conn = Connection::open(data.path().join("verlinpark.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE session_continuations (
                    id              TEXT PRIMARY KEY,
                    session_id      TEXT NOT NULL,
                    project_id      TEXT NOT NULL,
                    body            TEXT NOT NULL,
                    watch_task_ids  TEXT NOT NULL DEFAULT '[]',
                    watch_mode      TEXT NOT NULL DEFAULT 'all',
                    deadline_at     TEXT,
                    status          TEXT NOT NULL,
                    attempts        INTEGER NOT NULL DEFAULT 0,
                    max_attempts    INTEGER NOT NULL DEFAULT 3,
                    wake_reason     TEXT,
                    outcomes        TEXT NOT NULL DEFAULT '[]',
                    last_error      TEXT,
                    next_attempt_at TEXT,
                    created_at      TEXT NOT NULL,
                    ready_at        TEXT,
                    delivered_at    TEXT,
                    updated_at      TEXT NOT NULL
                );
                INSERT INTO session_continuations
                    (id, session_id, project_id, body, watch_task_ids, watch_mode, status,
                     created_at, updated_at)
                VALUES ('legacy-cont', 'legacy-session', 'legacy-project', 'legacy body', '[]',
                        'all', 'pending', '2026-11-11T00:00:00Z', '2026-11-11T00:00:00Z');",
        )
        .unwrap();
    }
    let store = Store::open(data.path()).unwrap();
    let rows = store
        .list_session_continuations("legacy-session", None, Some(10))
        .unwrap();
    assert_eq!(rows.len(), 1);
    let row = &rows[0];
    assert_eq!(row.id, "legacy-cont");
    assert!(row.series_id.is_empty());
    assert!(row.parent_continuation_id.is_none());
    assert!(row.recurrence_every_secs.is_none());
    assert_eq!(row.recurrence_max_runs, 1);
    assert_eq!(row.run_count, 0);
    assert_eq!(row.status, "pending");
}

#[test]
fn migrates_legacy_agents_without_losing_rows() {
    let data = TempDir::new().unwrap();
    let db_path = data.path().join("verlinpark.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        "CREATE TABLE projects (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            root TEXT NOT NULL UNIQUE,
            default_branch TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            goal TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE agents (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            role TEXT NOT NULL,
            status TEXT NOT NULL,
            model TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        INSERT INTO projects VALUES ('p1','demo','/tmp/demo','main','2026-01-01T00:00:00Z');
        INSERT INTO sessions VALUES ('s1','p1','legacy','idle','2026-01-01T00:00:00Z');
        INSERT INTO agents VALUES ('a1','s1','leader','idle','legacy-model','2026-01-01T00:00:00Z');",
    )
    .unwrap();
    drop(conn);

    let store = Store::open(data.path()).unwrap();
    let agents = store.list_agents("s1").unwrap();
    assert_eq!(agents.len(), 1);
    assert_eq!(agents[0].name, "Agent");
    assert_eq!(agents[0].model, "legacy-model");
    assert_eq!(agents[0].title, None);
    assert_eq!(agents[0].branch, None);
}

#[test]
fn migrates_legacy_approvals_with_risk_columns() {
    let data = TempDir::new().unwrap();
    let db_path = data.path().join("verlinpark.db");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    conn.execute_batch(
        r#"CREATE TABLE projects (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            root TEXT NOT NULL UNIQUE,
            default_branch TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE sessions (
            id TEXT PRIMARY KEY,
            project_id TEXT NOT NULL,
            goal TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL
        );
        CREATE TABLE approvals (
            id TEXT PRIMARY KEY,
            session_id TEXT NOT NULL,
            tool_call_id TEXT NOT NULL,
            tool TEXT NOT NULL,
            input TEXT NOT NULL,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL,
            resolved_at TEXT
        );
        INSERT INTO projects VALUES ('p1','demo','/tmp/demo','main','2026-01-01T00:00:00Z');
        INSERT INTO sessions VALUES ('s1','p1','legacy','idle','2026-01-01T00:00:00Z');
        INSERT INTO approvals VALUES ('a1','s1','call-1','shell','{"command":"true"}','approved','2026-01-01T00:00:00Z','2026-01-01T00:00:01Z');"#,
    )
    .unwrap();
    drop(conn);

    let store = Store::open(data.path()).unwrap();
    let approvals = store.list_approvals(Some("s1")).unwrap();
    assert_eq!(approvals.len(), 1);
    assert_eq!(approvals[0].approval_risk, None);
    assert_eq!(approvals[0].status, "approved");

    let created = store
        .create_approval(
            "s1",
            "call-2",
            "shell",
            &serde_json::json!({"command": "shutdown"}),
            Some("destructive"),
            Some("system power control"),
        )
        .unwrap();
    assert_eq!(created.approval_risk.as_deref(), Some("destructive"));
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
            Some("workspace_write"),
            Some("workspace file write"),
        )
        .unwrap();
    assert_eq!(approval.approval_risk.as_deref(), Some("workspace_write"));
    assert_eq!(
        approval.approval_reason.as_deref(),
        Some("workspace file write")
    );
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
    assert_eq!(entry.pre_merge_commit, None);

    let merging = store
        .begin_merge_queue_entry(&entry.id, "def456")
        .unwrap()
        .unwrap();
    assert_eq!(merging.status, "merging");
    assert_eq!(merging.pre_merge_commit.as_deref(), Some("def456"));

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
    store.begin_merge_queue_entry(&entry.id, "def456").unwrap();
    let interrupted = store.interrupt_merge_queue().unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(interrupted[0].status, "interrupted");
    assert_eq!(interrupted[0].pre_merge_commit.as_deref(), Some("def456"));
    let recoveries = store.interrupted_merge_recoveries().unwrap();
    assert_eq!(recoveries.len(), 1);
    assert_eq!(recoveries[0].id, entry.id);
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
fn agent_task_assignment_is_persisted_in_the_projection_and_event() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("agents"))
        .unwrap();
    let session = store.create_session(&project.id, "task mapping").unwrap();
    let agent = store
        .create_agent(
            &session.id,
            "worker",
            "Layout worker",
            None,
            None,
            "test-model",
        )
        .unwrap();
    let event = store
        .set_agent_task_with_event(&agent.id, Some("T3"), Some("Platform"))
        .unwrap();
    assert_eq!(event.kind, "agent.updated");
    assert_eq!(event.payload["agent"]["task_id"], "T3");
    assert_eq!(event.payload["agent"]["department"], "Platform");
    let stored = store.list_agents(&session.id).unwrap();
    assert_eq!(stored[0].task_id.as_deref(), Some("T3"));
    assert_eq!(stored[0].department.as_deref(), Some("Platform"));
}

#[test]
fn concurrent_event_appends_keep_the_mirror_ordered() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("events"))
        .unwrap();
    let session = store.create_session(&project.id, "mirror").unwrap();
    let mut handles = Vec::new();
    for index in 0..24 {
        let store = store.clone();
        let session_id = session.id.clone();
        handles.push(std::thread::spawn(move || {
            store
                .append_event(
                    &session_id,
                    "test.event",
                    serde_json::json!({"index": index}),
                )
                .unwrap();
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let mirror = store.read_event_mirror(&session.id).unwrap().unwrap();
    assert_eq!(mirror.len(), 24);
    assert!(mirror.windows(2).all(|pair| pair[0].seq < pair[1].seq));
    let database = store.list_events(&session.id, 0, 100).unwrap();
    assert_eq!(mirror.len(), database.len());
    for (mirror, database) in mirror.iter().zip(database.iter()) {
        assert_eq!(mirror.seq, database.seq);
        assert_eq!(mirror.kind, database.kind);
        assert_eq!(mirror.payload, database.payload);
    }
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
    store
        .acquire_computer_lease("browser", "daemon-a", &session.id, "agent-a", "Agent A", 60)
        .unwrap();
    store.update_session_status(&session.id, "running").unwrap();

    let (recovered, events) = store.recover_state().unwrap();
    assert_eq!(recovered, vec![session.id.clone()]);
    assert!(store.list_write_leases(None, None).unwrap().is_empty());
    assert!(store.list_computer_leases().unwrap().is_empty());
    assert!(
        events
            .iter()
            .any(|event| event.kind == "computer.lease.expired")
    );
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

#[test]
fn interrupts_only_the_finished_sessions_tasks() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let first = store.create_session(&project.id, "first").unwrap();
    let second = store.create_session(&project.id, "second").unwrap();
    let first_plan = store
        .append_event(
            &first.id,
            "team_plan.submitted",
            serde_json::json!({"goal": "first"}),
        )
        .unwrap();
    let second_plan = store
        .append_event(
            &second.id,
            "team_plan.submitted",
            serde_json::json!({"goal": "second"}),
        )
        .unwrap();
    store
        .sync_plan_tasks(&first.id, &project.id, first_plan.seq, &scheduled_plan())
        .unwrap();
    store
        .sync_plan_tasks(&second.id, &project.id, second_plan.seq, &scheduled_plan())
        .unwrap();
    store
        .claim_plan_tasks(&first.id, first_plan.seq, &["T1".into()])
        .unwrap();
    store
        .claim_plan_tasks(&second.id, second_plan.seq, &["T1".into()])
        .unwrap();

    let interrupted = store
        .interrupt_session_plan_tasks(&first.id, "session ended")
        .unwrap();
    assert_eq!(interrupted.len(), 1);
    assert_eq!(
        store
            .list_plan_tasks(Some(&project.id), Some(&first.id))
            .unwrap()
            .into_iter()
            .find(|task| task.task_id == "T1")
            .unwrap()
            .status,
        "interrupted"
    );
    assert_eq!(
        store
            .list_plan_tasks(Some(&project.id), Some(&second.id))
            .unwrap()
            .into_iter()
            .find(|task| task.task_id == "T1")
            .unwrap()
            .status,
        "running"
    );
}

#[test]
fn round_trips_task_scheduler_policy_and_manifest() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("scheduler policy"))
        .unwrap();
    let default = store.get_task_scheduler_policy(&project.id).unwrap();
    assert!(default.enabled);
    assert_eq!(default.cooldown_secs, 60);
    assert!(!default.dispatch_tasks);
    assert!(default.include_intake);
    assert_eq!(default.max_intake_parallel, 2);

    store
        .set_task_scheduler_policy(
            &project.id,
            &TaskSchedulerPolicy {
                enabled: true,
                cooldown_secs: 15,
                dispatch_tasks: true,
                include_intake: true,
                max_intake_parallel: 2,
            },
        )
        .unwrap();
    let policy = store.get_task_scheduler_policy(&project.id).unwrap();
    assert!(policy.enabled);
    assert_eq!(policy.cooldown_secs, 15);
    assert!(policy.dispatch_tasks);
    store.record_project_manifest().unwrap();

    let snapshot = read_project_manifest(data.path()).unwrap();
    assert!(snapshot[0].task_scheduler_policy.enabled);
    assert_eq!(snapshot[0].task_scheduler_policy.cooldown_secs, 15);
    assert!(snapshot[0].task_scheduler_policy.dispatch_tasks);
}

#[test]
fn model_presets_round_trip_and_delete() {
    let dir = TempDir::new().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let preset = vp_protocol::types::ModelPreset {
        id: "preset-1".into(),
        name: "Claude".into(),
        provider: "anthropic".into(),
        model: "claude-sonnet".into(),
        context_window: Some(200_000),
        input_usd_per_million: Some(3.0),
        output_usd_per_million: Some(15.0),
        notes: "test".into(),
        created_at: "2026-11-08T00:00:00Z".into(),
    };
    store.create_model_preset(&preset).unwrap();
    let listed = store.list_model_presets().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].model, "claude-sonnet");
    assert_eq!(store.get_model_preset("preset-1").unwrap().unwrap(), preset);

    let updated = vp_protocol::types::ModelPreset {
        name: "Claude fast".into(),
        model: "claude-haiku".into(),
        context_window: Some(100_000),
        ..preset
    };
    assert!(store.update_model_preset(&updated).unwrap());
    assert_eq!(
        store.get_model_preset("preset-1").unwrap().unwrap().name,
        "Claude fast"
    );
    assert!(store.delete_model_preset("preset-1").unwrap());
    assert!(store.list_model_presets().unwrap().is_empty());
    assert!(!store.delete_model_preset("preset-1").unwrap());
}

#[test]
fn migrates_legacy_mcp_servers_to_transport_columns() {
    let data = TempDir::new().unwrap();
    let conn = rusqlite::Connection::open(data.path().join("verlinpark.db")).unwrap();
    conn.execute_batch(
        "CREATE TABLE mcp_servers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL UNIQUE,
            command TEXT NOT NULL,
            args TEXT NOT NULL,
            env TEXT NOT NULL,
            enabled INTEGER NOT NULL DEFAULT 1,
            created_at TEXT NOT NULL
        );
        INSERT INTO mcp_servers (id, name, command, args, env, enabled, created_at)
        VALUES ('legacy', 'legacy', 'mcp-files', '[]', '{}', 1, '2026-01-01T00:00:00Z');",
    )
    .unwrap();
    drop(conn);
    let store = Store::open(data.path()).unwrap();
    let servers = store.list_mcp_servers().unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(
        servers[0].transport,
        vp_protocol::types::McpTransport::Stdio
    );
    assert_eq!(servers[0].command, "mcp-files");
    assert!(servers[0].url.is_none());
    assert!(servers[0].headers.is_empty());
}

#[test]
fn orchestration_policy_round_trips_in_the_manifest() {
    use vp_protocol::types::OrchestrationPolicy;
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    assert!(
        store
            .get_orchestration_policy(&project.id)
            .unwrap()
            .auto_retry_failed
    );
    let policy = OrchestrationPolicy {
        auto_retry_failed: false,
        max_attempts: 4,
        retry_backoff_secs: 45,
        retry_priority_boost: 12,
        dependency_failure: vp_protocol::types::DependencyFailurePolicy::Cancel,
        escalate_after_retries: false,
        auto_resolve_blockers: false,
        stall_recovery: vp_protocol::types::StallRecoveryMode::Escalate,
        stall_min_observations: 3,
        stall_max_attempts: 2,
        stall_grace_secs: 60,
    };
    store
        .set_orchestration_policy(&project.id, &policy)
        .unwrap();
    assert_eq!(store.get_orchestration_policy(&project.id).unwrap(), policy);
    store.record_project_manifest().unwrap();
    let manifests = crate::read_project_manifest(store.data_dir()).unwrap();
    let manifest = manifests
        .into_iter()
        .find(|candidate| candidate.project.id == project.id)
        .unwrap();
    assert_eq!(manifest.orchestration_policy, policy);
}

#[test]
fn plan_task_block_unblock_and_cancel_are_replayable() {
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
    store
        .sync_plan_tasks(&session.id, &project.id, submitted.seq, &scheduled_plan())
        .unwrap();

    let (blocked, event) = store
        .block_plan_task_with_event(
            &session.id,
            submitted.seq,
            "T2",
            "dependency T1 failed",
            "T1",
        )
        .unwrap()
        .unwrap();
    assert_eq!(blocked.status, "blocked");
    assert_eq!(event.kind, "plan_task.blocked");
    assert_eq!(event.payload["dependency"], "T1");

    let (unblocked, event) = store
        .unblock_plan_task_with_event(&session.id, submitted.seq, "T2", "dependency recovered")
        .unwrap()
        .unwrap();
    assert_eq!(unblocked.status, "pending");
    assert_eq!(event.kind, "plan_task.unblocked");

    let (cancelled, event) = store
        .cancel_plan_task_with_event(
            &session.id,
            submitted.seq,
            "T2",
            "dependency failed permanently",
            "T1",
        )
        .unwrap()
        .unwrap();
    assert_eq!(cancelled.status, "cancelled");
    assert!(cancelled.finished_at.is_some());
    assert_eq!(event.kind, "plan_task.cancelled");

    let rebuilt = store
        .sync_plan_tasks(&session.id, &project.id, submitted.seq, &scheduled_plan())
        .unwrap();
    let t2 = rebuilt.iter().find(|task| task.task_id == "T2").unwrap();
    assert_eq!(t2.status, "cancelled");
    assert_eq!(
        t2.last_error.as_deref(),
        Some("dependency failed permanently")
    );
}

#[test]
fn scheduled_retry_is_gated_until_due() {
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
    store
        .sync_plan_tasks(&session.id, &project.id, submitted.seq, &scheduled_plan())
        .unwrap();
    store
        .claim_plan_tasks(&session.id, submitted.seq, &["T1".into()])
        .unwrap();
    store
        .finish_plan_task(&session.id, submitted.seq, "T1", "failed", Some("boom"))
        .unwrap();

    let (scheduled, event) = store
        .schedule_plan_task_retry_with_event(
            &session.id,
            submitted.seq,
            "T1",
            "2999-01-01T00:00:00Z",
            "retry later",
        )
        .unwrap()
        .unwrap();
    assert_eq!(scheduled.status, "pending");
    assert_eq!(
        scheduled.next_attempt_at.as_deref(),
        Some("2999-01-01T00:00:00Z")
    );
    assert_eq!(event.kind, "plan_task.retry_scheduled");
    let claimed = store
        .claim_plan_tasks(&session.id, submitted.seq, &["T1".into()])
        .unwrap();
    assert!(claimed.is_empty());

    let rebuilt = store
        .sync_plan_tasks(&session.id, &project.id, submitted.seq, &scheduled_plan())
        .unwrap();
    let t1 = rebuilt.iter().find(|task| task.task_id == "T1").unwrap();
    assert_eq!(t1.status, "pending");
    assert_eq!(t1.next_attempt_at.as_deref(), Some("2999-01-01T00:00:00Z"));
}

#[test]
fn plan_revision_is_stored_with_an_event() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "schedule").unwrap();
    let mut plan = scheduled_plan();
    plan.tasks[0].title = "One revised".into();
    let event = store
        .set_team_plan_revision_with_event(
            &session.id,
            &serde_json::to_string(&plan).unwrap(),
            "team_plan.revised",
            serde_json::json!({
                "revision": 2,
                "reason": "tighten scope",
                "author": "user",
                "changed": ["T1"]
            }),
        )
        .unwrap();
    assert_eq!(event.kind, "team_plan.revised");
    let stored: TeamPlan =
        serde_json::from_str(&store.get_team_plan(&session.id).unwrap().unwrap()).unwrap();
    assert_eq!(stored.tasks[0].title, "One revised");
    let events = store.recent_events(&session.id, 10).unwrap();
    assert!(events.iter().any(|event| event.kind == "team_plan.revised"));
}

#[test]
fn runtime_leases_are_owned_renewable_and_releasable() {
    let (_data, store, _repo) = test_store();
    assert!(
        store
            .try_acquire_runtime_lease("session:s1", "daemon-a", "session", Some("s1"), 60)
            .unwrap()
    );
    // The same owner can refresh, another owner cannot take an unexpired lease.
    assert!(
        store
            .try_acquire_runtime_lease("session:s1", "daemon-a", "session", Some("s1"), 60)
            .unwrap()
    );
    assert!(
        !store
            .try_acquire_runtime_lease("session:s1", "daemon-b", "session", Some("s1"), 60)
            .unwrap()
    );
    let lease = store.runtime_lease_owner("session:s1").unwrap().unwrap();
    assert_eq!(lease.owner_id, "daemon-a");
    assert_eq!(lease.kind, "session");
    assert_eq!(lease.session_id.as_deref(), Some("s1"));

    assert!(
        store
            .renew_runtime_lease("session:s1", "daemon-a", 120)
            .unwrap()
    );
    assert!(
        !store
            .renew_runtime_lease("session:s1", "daemon-b", 120)
            .unwrap()
    );
    assert_eq!(store.list_runtime_leases().unwrap().len(), 1);
    assert!(
        !store
            .release_runtime_lease("session:s1", "daemon-b")
            .unwrap()
    );
    assert!(
        store
            .release_runtime_lease("session:s1", "daemon-a")
            .unwrap()
    );
    assert!(store.runtime_lease_owner("session:s1").unwrap().is_none());
    assert!(
        store
            .try_acquire_runtime_lease("session:s1", "daemon-b", "session", Some("s1"), 60)
            .unwrap()
    );
}

#[test]
fn expired_runtime_leases_can_be_swept_or_taken_over() {
    let (_data, store, _repo) = test_store();
    assert!(
        store
            .try_acquire_runtime_lease("session:s2", "daemon-a", "session", Some("s2"), 0)
            .unwrap()
    );
    // A zero-TTL lease is effectively expired; a second owner can take it.
    assert!(
        store
            .try_acquire_runtime_lease("session:s2", "daemon-b", "session", Some("s2"), 60)
            .unwrap()
    );
    let lease = store.runtime_lease_owner("session:s2").unwrap().unwrap();
    assert_eq!(lease.owner_id, "daemon-b");
    let removed = store.clear_expired_runtime_leases().unwrap();
    assert_eq!(removed, 0);
}

#[test]
fn lease_expiry_comparison_handles_rfc3339_fractional_seconds() {
    let (_data, store, _repo) = test_store();
    assert!(
        store
            .try_acquire_runtime_lease("session:fractional", "daemon-a", "session", Some("s"), 60)
            .unwrap()
    );
    {
        let conn = store.lock().unwrap();
        // RFC3339 may include fractional seconds. A plain string comparison
        // would sort "2020-01-01T00:00:00.500Z" *after* "...00Z" because '.'
        // sorts below 'Z', leaving an expired lease permanently stuck.
        conn.execute(
            "UPDATE runtime_leases SET expires_at = '2020-01-01T00:00:00.500Z'
             WHERE lease_key = ?1",
            params!["session:fractional"],
        )
        .unwrap();
    }
    assert!(
        store
            .try_acquire_runtime_lease("session:fractional", "daemon-b", "session", Some("s"), 60)
            .unwrap()
    );
    let lease = store
        .runtime_lease_owner("session:fractional")
        .unwrap()
        .unwrap();
    assert_eq!(lease.owner_id, "daemon-b");

    assert!(
        store
            .try_acquire_runtime_lease(
                "session:fractional-2",
                "daemon-a",
                "session",
                Some("s2"),
                60
            )
            .unwrap()
    );
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE runtime_leases SET expires_at = '2020-01-01T00:00:00.500Z'
             WHERE lease_key = ?1",
            params!["session:fractional-2"],
        )
        .unwrap();
    }
    assert_eq!(store.clear_expired_runtime_leases().unwrap(), 1);
    assert!(
        store
            .runtime_lease_owner("session:fractional-2")
            .unwrap()
            .is_none()
    );
}

#[test]
fn computer_leases_are_exclusive_owned_and_audited() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("browser"))
        .unwrap();
    let first = store.create_session(&project.id, "first").unwrap();
    let second = store.create_session(&project.id, "second").unwrap();
    let acquired = store
        .acquire_computer_lease("browser", "owner-a", &first.id, "agent-a", "Agent A", 60)
        .unwrap();
    let lease = match acquired {
        ComputerLeaseAttempt::Acquired { lease, events } => {
            assert!(
                events
                    .iter()
                    .any(|event| event.kind == "computer.lease.acquired")
            );
            lease
        }
        ComputerLeaseAttempt::Busy { .. } => panic!("first computer lease was busy"),
    };
    assert_eq!(lease.surface, "browser");
    assert_eq!(lease.session_id, first.id);
    assert_eq!(store.list_computer_leases().unwrap().len(), 1);

    let busy = store
        .acquire_computer_lease("browser", "owner-b", &second.id, "agent-b", "Agent B", 60)
        .unwrap();
    match busy {
        ComputerLeaseAttempt::Acquired { .. } => panic!("second computer lease was acquired"),
        ComputerLeaseAttempt::Busy { holder, events } => {
            assert_eq!(
                holder.as_ref().map(|lease| lease.session_id.as_str()),
                Some(first.id.as_str())
            );
            assert!(
                events
                    .iter()
                    .any(|event| event.kind == "computer.lease.denied")
            );
        }
    }
    assert!(
        !store
            .renew_computer_lease(&lease.lease_id, "owner-b", 60)
            .unwrap()
    );
    assert!(
        store
            .renew_computer_lease(&lease.lease_id, "owner-a", 60)
            .unwrap()
    );
    assert!(
        store
            .release_computer_lease(&lease.lease_id, Some("owner-b"))
            .unwrap()
            .is_none()
    );
    let released = store
        .release_computer_lease(&lease.lease_id, Some("owner-a"))
        .unwrap()
        .expect("release event");
    assert_eq!(released.kind, "computer.lease.released");
    assert!(store.list_computer_leases().unwrap().is_empty());

    let events = store.recent_events(&first.id, 20).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "computer.lease.acquired")
    );
    assert!(
        events
            .iter()
            .any(|event| event.kind == "computer.lease.released")
    );
    let attempts = store.recent_events(&second.id, 20).unwrap();
    assert!(
        attempts
            .iter()
            .any(|event| event.kind == "computer.lease.denied")
    );
}

#[test]
fn expired_computer_leases_are_swept_or_taken_over() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("browser"))
        .unwrap();
    let first = store.create_session(&project.id, "first").unwrap();
    let second = store.create_session(&project.id, "second").unwrap();
    let acquired = store
        .acquire_computer_lease("browser", "owner-a", &first.id, "agent-a", "Agent A", 0)
        .unwrap();
    let lease = match acquired {
        ComputerLeaseAttempt::Acquired { lease, .. } => lease,
        ComputerLeaseAttempt::Busy { .. } => panic!("zero-ttl computer lease was busy"),
    };
    let takeover = store
        .acquire_computer_lease("browser", "owner-b", &second.id, "agent-b", "Agent B", 60)
        .unwrap();
    let new_lease = match takeover {
        ComputerLeaseAttempt::Acquired { lease, events } => {
            assert!(
                events
                    .iter()
                    .any(|event| event.kind == "computer.lease.expired")
            );
            lease
        }
        ComputerLeaseAttempt::Busy { .. } => panic!("expired computer lease was not taken over"),
    };
    assert_ne!(lease.lease_id, new_lease.lease_id);
    assert_eq!(
        store.list_computer_leases().unwrap()[0].owner_id,
        "owner-b".to_string()
    );
    let (removed, events) = store.clear_expired_computer_leases().unwrap();
    assert_eq!(removed, 0);
    assert!(events.is_empty());
}

#[test]
fn coordination_notes_and_unblock_are_projected_atomically() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "coordinate").unwrap();
    let note_event = store
        .record_coordination_note_with_event(
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
        .unwrap();
    let note_id = note_event.payload["note_id"].as_str().unwrap();
    assert_eq!(note_event.kind, "coordination.note");
    let notes = store.list_coordination_notes(&session.id, 10).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].status, "open");
    assert_eq!(notes[0].task_id.as_deref(), Some("T1"));

    let unblock = store
        .record_coordination_note_with_event(
            &session.id,
            "unblock",
            "info",
            Some("T1"),
            None,
            Some(note_id),
            "user",
            "User",
            Some("user"),
            "resolved",
        )
        .unwrap();
    assert_eq!(unblock.kind, "coordination.note");
    let notes = store.list_coordination_notes(&session.id, 10).unwrap();
    assert_eq!(notes[0].status, "resolved");
    assert!(notes[0].resolved_at.is_some());

    // A second unblock has no open blocker to resolve and fails.
    let error = store
        .record_coordination_note_with_event(
            &session.id,
            "unblock",
            "info",
            Some("T1"),
            None,
            None,
            "user",
            "User",
            Some("user"),
            "again",
        )
        .unwrap_err();
    assert!(error.to_string().contains("no open coordination blocker"));
}

#[test]
fn orchestration_decisions_are_projected_with_events() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "decide").unwrap();
    let event = store
        .record_orchestration_decision_with_event(
            &session.id,
            "retry",
            Some("T1"),
            "attempt 1 failed",
            1,
            "orchestrator",
        )
        .unwrap();
    assert_eq!(event.kind, "orchestration.decision");
    let decisions = store.list_orchestration_decisions(&session.id, 10).unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(decisions[0].action, "retry");
    assert_eq!(decisions[0].attempt, 1);
    assert_eq!(decisions[0].task_id.as_deref(), Some("T1"));

    let events = store.list_all_events(&session.id).unwrap();
    store
        .replace_orchestration_decisions(&session.id, &[], &events)
        .unwrap();
    assert!(
        store
            .list_orchestration_decisions(&session.id, 10)
            .unwrap()
            .is_empty()
    );
    store
        .replace_orchestration_decisions(&session.id, &decisions, &events)
        .unwrap();
    assert_eq!(
        store.list_orchestration_decisions(&session.id, 10).unwrap(),
        decisions
    );
}

#[test]
fn unblock_can_target_a_note_without_a_task() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "coordinate").unwrap();
    let blocker = store
        .record_coordination_note_with_event(
            &session.id,
            "blocker",
            "warning",
            None,
            None,
            None,
            "agent-1",
            "Worker One",
            Some("worker"),
            "blocked without a task",
        )
        .unwrap();
    let note_id = blocker.payload["note_id"].as_str().unwrap();
    store
        .record_coordination_note_with_event(
            &session.id,
            "unblock",
            "info",
            None,
            None,
            Some(note_id),
            "user",
            "User",
            Some("user"),
            "resolved by note",
        )
        .unwrap();
    let notes = store.list_coordination_notes(&session.id, 10).unwrap();
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].status, "resolved");
    assert_eq!(notes[0].task_id, None);
}

#[test]
fn approval_grants_are_scoped_and_deletable() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let grant = store
        .create_approval_grant(&project.id, "write_file", "src", true, "user")
        .unwrap();
    assert!(grant.include_children);
    let grants = store.list_approval_grants(&project.id).unwrap();
    assert_eq!(grants.len(), 1);
    assert_eq!(grants[0].path_prefix, "src");
    store.record_project_manifest().unwrap();
    let manifest = crate::read_project_manifest(store.data_dir()).unwrap();
    let snapshot = manifest
        .into_iter()
        .find(|snapshot| snapshot.project.id == project.id)
        .unwrap();
    assert_eq!(snapshot.approval_grants.len(), 1);
    assert!(store.delete_approval_grant(&grant.id).unwrap());
    assert!(store.list_approval_grants(&project.id).unwrap().is_empty());
}

#[test]
fn enqueue_merge_with_event_persists_pre_merge_column() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "merge event").unwrap();
    let (entry, event) = store
        .enqueue_merge_with_event(
            &session.id,
            "agent_1",
            "Worker 1",
            Some("T1"),
            "verlinpark/task",
            "abc123",
            "def456",
            Some("approved"),
        )
        .unwrap();
    assert_eq!(entry.status, "queued");
    assert_eq!(entry.pre_merge_commit, None);
    assert_eq!(event.kind, "merge_queue.enqueued");
}

fn intake_draft(project_id: &str, title: &str) -> NewIntakeTask {
    NewIntakeTask {
        project_id: project_id.to_string(),
        title: title.to_string(),
        instructions: format!("Implement {title} and verify it."),
        profile: "coding".to_string(),
        kind: "coding".to_string(),
        priority: 5,
        write_scope: vec!["src".to_string()],
        acceptance: vec!["tests pass".to_string()],
        deps: Vec::new(),
        token_budget: Some(20_000),
        cost_budget_usd: Some(1.5),
        time_budget_secs: Some(600),
        max_attempts: 2,
        origin: "test".to_string(),
        labels: vec!["nightly".to_string()],
    }
}

#[test]
fn creates_lists_and_reports_intake_tasks() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("intake"))
        .unwrap();
    let task = store
        .create_intake_task(&intake_draft(&project.id, "Fix login"))
        .unwrap();
    assert_eq!(task.status, "queued");
    assert_eq!(task.attempts, 0);
    assert_eq!(task.max_attempts, 2);
    assert_eq!(task.write_scope, vec!["src"]);
    assert_eq!(task.acceptance, vec!["tests pass"]);
    assert_eq!(task.labels, vec!["nightly"]);
    assert_eq!(store.count_active_intake_tasks(&project.id).unwrap(), 0);

    let listed = store
        .list_intake_tasks(Some(&project.id), None, None)
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, task.id);
    assert_eq!(
        store
            .list_intake_tasks(Some(&project.id), Some("completed"), None)
            .unwrap()
            .len(),
        0
    );

    store.record_project_manifest().unwrap();
    let snapshots = read_project_manifest(data.path()).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].intake_tasks.len(), 1);
    assert_eq!(snapshots[0].intake_tasks[0].title, "Fix login");
}

#[test]
fn intake_claim_is_atomic_and_attempts_are_bounded() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("claim"))
        .unwrap();
    let task = store
        .create_intake_task(&intake_draft(&project.id, "One"))
        .unwrap();

    let claimed = store.claim_intake_task(&task.id).unwrap().unwrap();
    assert_eq!(claimed.status, "dispatching");
    assert_eq!(claimed.attempts, 1);
    assert_eq!(store.count_active_intake_tasks(&project.id).unwrap(), 1);
    assert!(store.claim_intake_task(&task.id).unwrap().is_none());
    assert!(
        store
            .runnable_intake_tasks(&now_rfc3339())
            .unwrap()
            .is_empty()
    );

    let started = store
        .start_intake_task(&task.id, "session-1", "T1")
        .unwrap();
    assert_eq!(started.status, "running");
    assert_eq!(started.session_id.as_deref(), Some("session-1"));

    let failed = store
        .settle_intake_task(
            &task.id,
            "failed",
            None,
            Some("provider failed"),
            Some("9999-01-01T00:00:00Z"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.last_error.as_deref(), Some("provider failed"));
    assert_eq!(
        failed.next_attempt_at.as_deref(),
        Some("9999-01-01T00:00:00Z")
    );
    assert!(
        store
            .runnable_intake_tasks(&now_rfc3339())
            .unwrap()
            .is_empty()
    );

    let retried = store
        .settle_intake_task(&task.id, "failed", None, Some("again"), None)
        .unwrap()
        .unwrap();
    assert_eq!(retried.attempts, 1);
    let claimed = store.claim_intake_task(&task.id).unwrap().unwrap();
    assert_eq!(claimed.attempts, 2);
    assert_eq!(claimed.status, "dispatching");
    store
        .settle_intake_task(&task.id, "failed", None, Some("last"), None)
        .unwrap();
    assert!(store.claim_intake_task(&task.id).unwrap().is_none());
    assert!(store.retry_intake_task(&task.id).unwrap().is_some());
    assert_eq!(
        store.get_intake_task(&task.id).unwrap().unwrap().attempts,
        0
    );
}

#[test]
fn intake_cancel_delete_and_recovery_guard_active_rows() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("lifecycle"))
        .unwrap();
    let queued = store
        .create_intake_task(&intake_draft(&project.id, "Queued"))
        .unwrap();
    assert!(store.cancel_intake_task(&queued.id).unwrap().is_some());
    assert!(store.delete_intake_task(&queued.id).unwrap());

    let dispatching = store
        .create_intake_task(&intake_draft(&project.id, "Dispatching"))
        .unwrap();
    store.claim_intake_task(&dispatching.id).unwrap();
    assert!(!store.delete_intake_task(&dispatching.id).unwrap());
    assert!(store.cancel_intake_task(&dispatching.id).unwrap().is_none());

    let running = store
        .create_intake_task(&intake_draft(&project.id, "Running"))
        .unwrap();
    store.claim_intake_task(&running.id).unwrap();
    store
        .start_intake_task(&running.id, "session-recover", "T1")
        .unwrap();
    assert!(!store.delete_intake_task(&running.id).unwrap());
    assert!(store.cancel_intake_task(&running.id).unwrap().is_none());

    let (sessions, _events) = store.recover_state().unwrap();
    assert!(sessions.is_empty());
    let recovered = store.get_intake_task(&running.id).unwrap().unwrap();
    assert_eq!(recovered.status, "failed");
    assert!(
        recovered
            .last_error
            .as_deref()
            .unwrap_or_default()
            .contains("daemon restarted")
    );
}

#[test]
fn intake_dependencies_round_trip_and_edits_requeue() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("deps"))
        .unwrap();
    let first = store
        .create_intake_task(&intake_draft(&project.id, "First"))
        .unwrap();
    let mut draft = intake_draft(&project.id, "Second");
    draft.deps = vec![first.id.clone()];
    let second = store.create_intake_task(&draft).unwrap();
    assert_eq!(second.deps, vec![first.id.clone()]);
    assert_eq!(
        store.get_intake_task(&second.id).unwrap().unwrap().deps,
        vec![first.id.clone()]
    );

    let mut edited = intake_draft(&project.id, "Second edited");
    edited.deps = Vec::new();
    edited.priority = 30;
    let updated = store
        .update_intake_task(&second.id, &edited)
        .unwrap()
        .unwrap();
    assert_eq!(updated.title, "Second edited");
    assert_eq!(updated.priority, 30);
    assert!(updated.deps.is_empty());
    assert_eq!(updated.status, "queued");

    store.claim_intake_task(&second.id).unwrap();
    assert!(
        store
            .update_intake_task(&second.id, &edited)
            .unwrap()
            .is_none()
    );
}

#[test]
fn task_scheduler_policy_defaults_to_autonomous_intake() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("policy"))
        .unwrap();
    let policy = store.get_task_scheduler_policy(&project.id).unwrap();
    assert!(policy.enabled);
    assert!(policy.include_intake);
    assert_eq!(policy.max_intake_parallel, 2);
    assert!(!store.has_task_scheduler_policy(&project.id).unwrap());

    store
        .set_task_scheduler_policy(
            &project.id,
            &TaskSchedulerPolicy {
                enabled: true,
                cooldown_secs: 5,
                dispatch_tasks: true,
                include_intake: false,
                max_intake_parallel: 7,
            },
        )
        .unwrap();
    assert!(store.has_task_scheduler_policy(&project.id).unwrap());
    let policy = store.get_task_scheduler_policy(&project.id).unwrap();
    assert!(!policy.include_intake);
    assert_eq!(policy.max_intake_parallel, 7);
}

fn source_fixture(
    project_id: &str,
    id: &str,
    title: &str,
    content: &str,
) -> (Source, Vec<SourceChunk>) {
    let now = now_rfc3339();
    let source = Source {
        id: id.to_string(),
        project_id: project_id.to_string(),
        kind: "note".to_string(),
        collection: None,
        title: title.to_string(),
        uri: None,
        mime_type: Some("text/markdown".to_string()),
        status: "indexed".to_string(),
        size_bytes: content.len() as u64,
        sha256: format!("{:x}", Sha256::digest(content.as_bytes())),
        revision: 1,
        chunk_count: 1,
        char_count: content.chars().count(),
        token_count: 4,
        error: None,
        labels: vec!["test".to_string()],
        created_at: now.clone(),
        updated_at: now.clone(),
        indexed_at: Some(now),
    };
    let chunk = SourceChunk {
        id: format!("{id}:0001:abcdef123456"),
        source_id: id.to_string(),
        chunk_no: 1,
        heading: Some("Testing".to_string()),
        content: content.to_string(),
        start_offset: 0,
        end_offset: content.len(),
        token_count: 4,
    };
    (source, vec![chunk])
}

fn source_search(query: &str) -> SourceSearchQuery {
    SourceSearchQuery {
        all: format!("\"{query}\"*"),
        any: format!("\"{query}\"*"),
        terms: vec![query.to_lowercase()],
        raw: query.to_string(),
        prefer_like: false,
        collections: Vec::new(),
        labels: Vec::new(),
    }
}

#[test]
fn sources_round_trip_chunks_search_and_stats() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("sources"))
        .unwrap();
    let (source, chunks) = source_fixture(
        &project.id,
        "source-1",
        "Architecture notes",
        "The runtime uses deterministic leases for integration.",
    );
    let inserted = store.insert_source(&source, &chunks).unwrap();
    assert_eq!(inserted.title, "Architecture notes");
    assert_eq!(
        store.get_source("source-1").unwrap().unwrap().labels,
        vec!["test"]
    );
    assert_eq!(
        store
            .list_sources(&project.id, None, None, None)
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .list_sources(&project.id, Some("architecture"), None, None)
            .unwrap()
            .len(),
        1
    );
    let page = store.source_chunks("source-1", 0, 10).unwrap();
    assert_eq!(page.len(), 1);
    assert_eq!(page[0].heading.as_deref(), Some("Testing"));
    let stats = store.source_stats(&project.id).unwrap();
    assert_eq!(stats.sources, 1);
    assert_eq!(stats.indexed, 1);
    assert_eq!(stats.chunks, 1);

    let (hits, degraded) = store
        .search_source_chunks(&project.id, &[], &source_search("deterministic"), Some(8))
        .unwrap();
    assert!(!hits.is_empty());
    assert!(!degraded);
    assert_eq!(hits[0].source_title, "Architecture notes");
}

#[test]
fn source_search_falls_back_for_cjk_and_deletes_cascade() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("sources"))
        .unwrap();
    let (source, chunks) = source_fixture(
        &project.id,
        "source-cjk",
        "中文说明",
        "确定性租约用于集成。",
    );
    store.insert_source(&source, &chunks).unwrap();
    let query = SourceSearchQuery {
        all: "\"no-such-fts-term\"*".to_string(),
        any: "\"no-such-fts-term\"*".to_string(),
        terms: vec!["确定性".to_string()],
        raw: "确定性".to_string(),
        prefer_like: true,
        collections: Vec::new(),
        labels: Vec::new(),
    };
    let (hits, degraded) = store
        .search_source_chunks(&project.id, &[], &query, Some(8))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(degraded);
    assert!(store.delete_source("source-cjk").unwrap());
    assert!(store.get_source("source-cjk").unwrap().is_none());
    assert!(store.source_chunks("source-cjk", 0, 10).unwrap().is_empty());
}

#[test]
fn source_snapshots_are_durable_and_listed() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("snapshot"))
        .unwrap();
    let (source, chunks) =
        source_fixture(&project.id, "source-snap", "Snapshot", "Durable content.");
    store.insert_source(&source, &chunks).unwrap();
    let snapshot = SourceSnapshot {
        version: 1,
        source: source.clone(),
        content: "Durable content.".to_string(),
    };
    store.write_source_snapshot(&snapshot).unwrap();
    let read = store
        .read_source_snapshot(&project.id, "source-snap")
        .unwrap()
        .unwrap();
    assert_eq!(read.content, "Durable content.");
    assert_eq!(store.list_source_snapshots().unwrap().len(), 1);
    store
        .delete_source_snapshot(&project.id, "source-snap")
        .unwrap();
    assert!(
        store
            .read_source_snapshot(&project.id, "source-snap")
            .unwrap()
            .is_none()
    );
    assert!(
        !data
            .path()
            .join("sources")
            .join(&project.id)
            .join("source-snap.json")
            .exists()
    );
}

fn skill_fixture(project_id: &str, name: &str) -> Skill {
    let now = now_rfc3339();
    Skill {
        id: format!("skill-{name}"),
        project_id: project_id.to_string(),
        name: name.to_string(),
        slug: name.to_lowercase().replace(' ', "-"),
        description: "A test skill".to_string(),
        when_to_use: "When testing".to_string(),
        version: "1.0.0".to_string(),
        instructions: "Read the diff and report evidence.".to_string(),
        tool_allow: vec!["read_file".to_string()],
        tool_deny: Vec::new(),
        profiles: vec!["coding".to_string()],
        roles: vec!["worker".to_string()],
        enabled: true,
        source: "test".to_string(),
        sha256: format!("{:x}", Sha256::digest(name.as_bytes())),
        created_at: now.clone(),
        updated_at: now,
    }
}

#[test]
fn skills_round_trip_filter_and_toggle() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("skills"))
        .unwrap();
    let skill = store
        .insert_skill(&skill_fixture(&project.id, "Review"))
        .unwrap();
    assert_eq!(skill.name, "Review");
    assert_eq!(skill.tool_allow, vec!["read_file"]);
    assert_eq!(
        store
            .get_skill_by_slug(&project.id, "review")
            .unwrap()
            .unwrap()
            .id,
        skill.id
    );
    assert_eq!(
        store.list_skills(Some(&project.id), false).unwrap().len(),
        1
    );
    let disabled = store.set_skill_enabled(&skill.id, false).unwrap().unwrap();
    assert!(!disabled.enabled);
    assert!(
        store
            .list_skills(Some(&project.id), false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.list_skills(Some(&project.id), true).unwrap().len(), 1);
    assert!(store.delete_skill(&skill.id).unwrap());
    assert!(store.get_skill(&skill.id).unwrap().is_none());
}

#[test]
fn inline_artifact_versions_round_trip_and_conflict() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("artifacts"))
        .unwrap();
    let session = store.create_session(&project.id, "versions").unwrap();
    let (created, _) = store
        .create_artifact_with_event(
            &session.id,
            "agent-1",
            "Leader",
            None,
            "Report",
            "note",
            "text/plain",
            "version one",
            "inline",
            11,
            "sha-one",
            None,
        )
        .unwrap();
    assert_eq!(created.version, 1);
    let (updated, event) = store
        .update_artifact_with_event(
            &created.id,
            Some(1),
            "Report",
            "note",
            "text/plain",
            "version two",
        )
        .unwrap()
        .unwrap();
    assert_eq!(updated.version, 2);
    assert_eq!(updated.content, "version two");
    assert_eq!(event.kind, "artifact.version_published");
    assert!(
        store
            .update_artifact_with_event(
                &created.id,
                Some(1),
                "Report",
                "note",
                "text/plain",
                "stale",
            )
            .is_err()
    );
    let versions = store.list_artifact_versions(&created.id).unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].content, "version one");
    assert_eq!(versions[1].content, "version two");
    let (restored, event) = store
        .restore_artifact_version(&created.id, 1)
        .unwrap()
        .unwrap();
    assert_eq!(restored.version, 3);
    assert_eq!(restored.content, "version one");
    assert_eq!(event.kind, "artifact.version_restored");
    assert_eq!(store.list_artifact_versions(&created.id).unwrap().len(), 3);
    assert_eq!(store.get_artifact(&created.id).unwrap().unwrap().version, 3);
}

#[test]
fn source_collections_and_scoped_search_filter() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("collections"))
        .unwrap();
    let (alpha, alpha_chunks) = source_fixture(
        &project.id,
        "source-alpha",
        "Alpha runtime",
        "Deterministic leases integrate verified work.",
    );
    let (beta, beta_chunks) = source_fixture(
        &project.id,
        "source-beta",
        "Beta research",
        "Deterministic retrieval supports source citations.",
    );
    store.insert_source(&alpha, &alpha_chunks).unwrap();
    store.insert_source(&beta, &beta_chunks).unwrap();
    store
        .set_source_collection("source-alpha", Some("engineering"))
        .unwrap();
    store
        .set_source_collection("source-beta", Some("research"))
        .unwrap();
    store
        .set_source_labels(
            "source-beta",
            &["citation".to_string(), "retrieval".to_string()],
        )
        .unwrap();

    let collections = store.list_source_collections(&project.id).unwrap();
    assert_eq!(collections.len(), 2);
    assert_eq!(collections[0].name.as_deref(), Some("engineering"));
    assert_eq!(collections[0].sources, 1);
    assert_eq!(collections[1].name.as_deref(), Some("research"));
    assert_eq!(collections[1].labels, vec!["citation", "retrieval"]);

    let mut query = source_search("deterministic");
    query.collections = vec!["research".to_string()];
    let (hits, _) = store
        .search_source_chunks(&project.id, &[], &query, Some(8))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].source_id, "source-beta");
    assert_eq!(hits[0].collection.as_deref(), Some("research"));
    assert_eq!(hits[0].labels, vec!["citation", "retrieval"]);

    query.labels = vec!["citation".to_string()];
    let (hits, _) = store
        .search_source_chunks(&project.id, &[], &query, Some(8))
        .unwrap();
    assert_eq!(hits.len(), 1);
    query.labels = vec!["missing".to_string()];
    let (hits, _) = store
        .search_source_chunks(&project.id, &[], &query, Some(8))
        .unwrap();
    assert!(hits.is_empty());

    let updated = store
        .update_source_metadata(&Source {
            collection: Some("architecture".to_string()),
            updated_at: now_rfc3339(),
            ..alpha.clone()
        })
        .unwrap();
    assert_eq!(updated.collection.as_deref(), Some("architecture"));
    assert_eq!(
        store
            .set_source_collection("source-missing", Some("x"))
            .unwrap(),
        None
    );
}

#[test]
fn legacy_artifacts_synthesize_current_version_and_backfill_on_update() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("legacy-artifacts"))
        .unwrap();
    let session = store
        .create_session(&project.id, "legacy versions")
        .unwrap();
    let (created, _) = store
        .create_artifact_with_event(
            &session.id,
            "agent-1",
            "Leader",
            None,
            "Legacy Report",
            "note",
            "text/plain",
            "legacy content",
            "inline",
            14,
            "legacy-sha",
            None,
        )
        .unwrap();
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "DELETE FROM artifact_versions WHERE artifact_id = ?1",
            [&created.id],
        )
        .unwrap();
    }
    let versions = store.list_artifact_versions(&created.id).unwrap();
    assert_eq!(versions.len(), 1);
    assert_eq!(versions[0].version, 1);
    assert_eq!(versions[0].content, "legacy content");
    assert_eq!(
        store
            .get_artifact_version(&created.id, 1)
            .unwrap()
            .unwrap()
            .name,
        "Legacy Report"
    );
    assert!(
        store
            .get_artifact_version(&created.id, 99)
            .unwrap()
            .is_none()
    );

    let (updated, _) = store
        .update_artifact_with_event(
            &created.id,
            Some(1),
            "Legacy Report",
            "note",
            "text/plain",
            "updated content",
        )
        .unwrap()
        .unwrap();
    assert_eq!(updated.version, 2);
    let versions = store.list_artifact_versions(&created.id).unwrap();
    assert_eq!(versions.len(), 2);
    assert_eq!(versions[0].content, "legacy content");
    assert_eq!(versions[1].content, "updated content");
}

#[test]
fn source_search_fuses_mixed_scripts_and_non_cjk_fallback() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("mixed-search"))
        .unwrap();
    let (source, chunks) = source_fixture(
        &project.id,
        "source-mixed",
        "Login notes",
        "Login failure investigation. 登录失败需要检查会话。",
    );
    store.insert_source(&source, &chunks).unwrap();

    let mixed = SourceSearchQuery {
        all: "\"login\"*".to_string(),
        any: "\"login\"*".to_string(),
        terms: vec!["login".to_string(), "登录".to_string()],
        raw: "login 登录".to_string(),
        prefer_like: true,
        collections: Vec::new(),
        labels: Vec::new(),
    };
    let (hits, degraded) = store
        .search_source_chunks(&project.id, &[], &mixed, Some(5))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(degraded);
    assert!(hits[0].content.contains("登录失败"));

    let fallback = SourceSearchQuery {
        all: "\"definitely-missing-term\"*".to_string(),
        any: "\"definitely-missing-term\"*".to_string(),
        terms: vec!["login".to_string(), "failure".to_string()],
        raw: "login failure".to_string(),
        prefer_like: false,
        collections: Vec::new(),
        labels: Vec::new(),
    };
    let (hits, degraded) = store
        .search_source_chunks(&project.id, &[], &fallback, Some(5))
        .unwrap();
    assert_eq!(hits.len(), 1);
    assert!(degraded);
    assert_eq!(hits[0].source_id, "source-mixed");
}

#[test]
fn session_unlimited_flag_round_trips_with_event() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("unlimited"))
        .unwrap();
    let session = store
        .create_session_with_limits_and_priority(
            &project.id,
            "large project",
            Some(100),
            None,
            None,
            0,
        )
        .unwrap();
    assert!(!session.unlimited);
    let event = store
        .set_session_unlimited_with_event(&session.id, true)
        .unwrap();
    assert_eq!(event.kind, "session.budget_mode_changed");
    assert_eq!(event.payload["unlimited"], serde_json::json!(true));
    let loaded = store.get_session(&session.id).unwrap().unwrap();
    assert!(loaded.unlimited);
    assert_eq!(loaded.token_budget, Some(100));
    assert!(store.list_sessions(None).unwrap()[0].unlimited);
    store
        .set_session_unlimited_with_event(&session.id, false)
        .unwrap();
    assert!(!store.get_session(&session.id).unwrap().unwrap().unlimited);
    assert!(
        store
            .set_session_unlimited_with_event("missing", true)
            .is_err()
    );
}

fn memory_evidence(kind: &str, reference: &str) -> MemoryEvidence {
    MemoryEvidence {
        kind: kind.to_string(),
        reference: reference.to_string(),
        note: None,
    }
}

#[test]
fn memory_create_is_idempotent_and_supersedes_atomically() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("memory v2"))
        .unwrap();
    let session = store.create_session(&project.id, "memory v2").unwrap();
    let tags = vec!["T3".to_string(), "architecture".to_string()];
    let evidence = vec![
        memory_evidence("task", "T3"),
        memory_evidence("path", "src/api"),
    ];
    let first = store
        .create_memory_with_metadata_event(NewMemory {
            session_id: &session.id,
            project_id: &project.id,
            agent_id: "agent-1",
            agent_name: "Leader",
            kind: "decision",
            title: "Keep merges deterministic",
            body: "The Integrator must not depend on an LLM.",
            tags: &tags,
            source: "operator",
            evidence: &evidence,
            pinned: true,
            verify_after: Some("2999-01-01T00:00:00Z"),
            dedupe: true,
            auto_supersede: true,
        })
        .unwrap();
    assert!(first.created);
    assert_eq!(first.events.len(), 1);
    assert_eq!(first.events[0].kind, "memory.created");
    assert_eq!(first.memory.source, "operator");
    assert_eq!(first.memory.status, "active");
    assert_eq!(first.memory.evidence, evidence);
    assert!(first.memory.pinned);
    assert_eq!(
        first.memory.verify_after.as_deref(),
        Some("2999-01-01T00:00:00Z")
    );

    let duplicate = store
        .create_memory_with_metadata_event(NewMemory {
            session_id: &session.id,
            project_id: &project.id,
            agent_id: "agent-1",
            agent_name: "Leader",
            kind: "Decision",
            title: "  Keep   merges deterministic ",
            body: "The Integrator must not depend on an LLM.",
            tags: &tags,
            source: "operator",
            evidence: &evidence,
            pinned: false,
            verify_after: None,
            dedupe: true,
            auto_supersede: true,
        })
        .unwrap();
    assert!(!duplicate.created);
    assert!(duplicate.events.is_empty());
    assert_eq!(duplicate.memory.id, first.memory.id);

    let second = store
        .create_memory_with_metadata_event(NewMemory {
            session_id: &session.id,
            project_id: &project.id,
            agent_id: "agent-1",
            agent_name: "Leader",
            kind: "decision",
            title: "Keep merges deterministic",
            body: "The Integrator is deterministic and verified.",
            tags: &tags,
            source: "operator",
            evidence: &evidence,
            pinned: false,
            verify_after: Some("2999-01-01T00:00:00Z"),
            dedupe: true,
            auto_supersede: true,
        })
        .unwrap();
    assert!(second.created);
    assert_eq!(second.events.len(), 2);
    assert_eq!(second.events[0].kind, "memory.superseded");
    assert_eq!(second.events[0].payload["id"], first.memory.id);
    assert_eq!(second.events[0].payload["status"], "superseded");
    assert_eq!(second.events[1].kind, "memory.created");
    assert_eq!(second.memory.supersedes, vec![first.memory.id.clone()]);

    let old = store.get_memory(&first.memory.id).unwrap().unwrap();
    assert_eq!(old.status, "superseded");
    assert_eq!(
        old.superseded_by.as_deref(),
        Some(second.memory.id.as_str())
    );
    let active = store.list_memories(&project.id, None, None, 10).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].id, second.memory.id);
    let superseded = store
        .list_memories_filtered(&project.id, None, None, Some("superseded"), false, 10)
        .unwrap();
    assert_eq!(superseded.len(), 1);
    assert_eq!(superseded[0].id, first.memory.id);

    // Conflicting content without auto-supersede is deduped, never duplicated.
    let blocked = store
        .create_memory_with_metadata_event(NewMemory {
            session_id: &session.id,
            project_id: &project.id,
            agent_id: "agent-1",
            agent_name: "Leader",
            kind: "decision",
            title: "Keep merges deterministic",
            body: "Conflicting unverified claim.",
            tags: &tags,
            source: "agent",
            evidence: &[],
            pinned: false,
            verify_after: None,
            dedupe: true,
            auto_supersede: false,
        })
        .unwrap();
    assert!(!blocked.created);
    assert!(blocked.events.is_empty());
    assert_eq!(blocked.memory.id, second.memory.id);
    assert_eq!(
        store
            .list_memories(&project.id, None, None, 10)
            .unwrap()
            .len(),
        1
    );

    // Explicit dedupe=false is the escape hatch for deliberate duplicates.
    let forced = store
        .create_memory_with_metadata_event(NewMemory {
            session_id: &session.id,
            project_id: &project.id,
            agent_id: "agent-1",
            agent_name: "Leader",
            kind: "decision",
            title: "Keep merges deterministic",
            body: "Deliberate duplicate row.",
            tags: &tags,
            source: "operator",
            evidence: &[],
            pinned: false,
            verify_after: None,
            dedupe: false,
            auto_supersede: false,
        })
        .unwrap();
    assert!(forced.created);
    assert_eq!(forced.events.len(), 1);
}

#[test]
fn memory_update_verify_stale_and_usage_are_event_backed() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("memory v2"))
        .unwrap();
    let session = store.create_session(&project.id, "memory v2").unwrap();
    let created = store
        .create_memory_with_metadata_event(NewMemory {
            session_id: &session.id,
            project_id: &project.id,
            agent_id: "agent-1",
            agent_name: "Leader",
            kind: "fact",
            title: "Build command",
            body: "cargo test --workspace",
            tags: &[],
            source: "agent",
            evidence: &[],
            pinned: false,
            verify_after: None,
            dedupe: true,
            auto_supersede: true,
        })
        .unwrap();
    let id = created.memory.id;
    let (updated, event) = store
        .update_memory_with_event(&id, Some("stale"), Some(true), Some(&["build".to_string()]))
        .unwrap()
        .unwrap();
    assert_eq!(event.kind, "memory.updated");
    assert_eq!(updated.status, "stale");
    assert!(updated.pinned);
    assert_eq!(
        updated.stale_reason.as_deref(),
        Some("operator marked stale")
    );
    assert_eq!(updated.tags, vec!["build".to_string()]);

    let (verified, event) = store
        .mark_memory_verified_with_event(&id, "2026-11-11T00:00:00Z", Some("2027-11-11T00:00:00Z"))
        .unwrap()
        .unwrap();
    assert_eq!(event.kind, "memory.verified");
    assert_eq!(verified.status, "active");
    assert!(verified.stale_reason.is_none());
    assert_eq!(
        verified.last_verified_at.as_deref(),
        Some("2026-11-11T00:00:00Z")
    );

    let (stale, event) = store
        .mark_memory_stale_with_event(&id, "verification expired")
        .unwrap()
        .unwrap();
    assert_eq!(event.kind, "memory.stale");
    assert_eq!(stale.status, "stale");
    assert_eq!(stale.stale_reason.as_deref(), Some("verification expired"));

    let other_session = store.create_session(&project.id, "other").unwrap();
    let (used, event) = store
        .record_memory_use_with_event(&id, &other_session.id)
        .unwrap()
        .unwrap();
    assert_eq!(event.kind, "memory.used");
    assert_eq!(used.use_count, 1);
    assert!(used.last_used_at.is_some());
    assert!(
        store
            .record_memory_use_with_event(&id, &other_session.id)
            .unwrap()
            .is_none()
    );
    let (used_again, _) = store
        .record_memory_use_with_event(&id, &session.id)
        .unwrap()
        .unwrap();
    assert_eq!(used_again.use_count, 2);
}

#[test]
fn memory_list_hides_stale_by_default_and_stats_count_lifecycle() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("memory v2"))
        .unwrap();
    let session = store.create_session(&project.id, "memory v2").unwrap();
    let create = |title: &str, pinned: bool, verify_after: Option<&str>| {
        store
            .create_memory_with_metadata_event(NewMemory {
                session_id: &session.id,
                project_id: &project.id,
                agent_id: "agent-1",
                agent_name: "Leader",
                kind: "fact",
                title,
                body: "body",
                tags: &[],
                source: "agent",
                evidence: &[],
                pinned,
                verify_after,
                dedupe: true,
                auto_supersede: true,
            })
            .unwrap()
            .memory
    };
    let expired = create("expired", false, Some("2020-01-01T00:00:00Z"));
    let future = create("future", false, Some("2999-01-01T00:00:00Z"));
    let pinned = create("pinned", true, Some("2020-01-01T00:00:00Z"));

    let candidates = store.memory_sweep_candidates(&project.id, None).unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].id, expired.id);

    store
        .mark_memory_stale_with_event(&expired.id, "verification expired")
        .unwrap();
    let active = store.list_memories(&project.id, None, None, 10).unwrap();
    assert_eq!(active.len(), 2);
    assert!(!active.iter().any(|memory| memory.id == expired.id));
    let with_stale = store
        .list_memories_filtered(&project.id, None, None, None, true, 10)
        .unwrap();
    assert_eq!(with_stale.len(), 3);
    let only_stale = store
        .list_memories_filtered(&project.id, None, None, Some("stale"), false, 10)
        .unwrap();
    assert_eq!(only_stale.len(), 1);

    let stats = store.memory_stats(&project.id).unwrap();
    assert_eq!(stats.total, 3);
    assert!(
        stats
            .by_status
            .iter()
            .any(|count| count.key == "stale" && count.count == 1)
    );
    assert!(
        stats
            .by_source
            .iter()
            .any(|count| count.key == "agent" && count.count == 3)
    );
    assert!(
        stats
            .by_kind
            .iter()
            .any(|count| count.key == "fact" && count.count == 3)
    );
    assert!(!future.id.is_empty());
    assert!(pinned.pinned);
}

fn create_conflict_memory(
    store: &Store,
    project_id: &str,
    session_id: &str,
    kind: &str,
    title: &str,
    body: &str,
    evidence: Vec<MemoryEvidence>,
) -> MemoryItem {
    store
        .create_memory_with_metadata_event(NewMemory {
            session_id,
            project_id,
            agent_id: "agent-1",
            agent_name: "Leader",
            kind,
            title,
            body,
            tags: &[],
            source: "operator",
            evidence: &evidence,
            pinned: false,
            verify_after: None,
            dedupe: false,
            auto_supersede: false,
        })
        .unwrap()
        .memory
}

#[test]
fn detects_shared_path_conflicts_for_distinct_bodies_only() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("memory conflicts"))
        .unwrap();
    let session = store.create_session(&project.id, "conflicts").unwrap();
    let path = || vec![memory_evidence("path", "src/lib.rs")];
    let first = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "decision",
        "Use tabs",
        "Indent with tabs.",
        path(),
    );
    let second = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "convention",
        "Use spaces",
        "Indent with four spaces.",
        path(),
    );
    // Same normalized body is not a conflict even with shared scope.
    let same = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "fact",
        "Tabs again",
        "  INDENT   WITH TABS. ",
        vec![memory_evidence("path", "src/same.rs")],
    );
    let same_copy = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "fact",
        "Tabs same body",
        "Indent with tabs.",
        vec![memory_evidence("path", "src/same.rs")],
    );

    let scan = store.scan_memory_conflicts(&project.id).unwrap();
    assert_eq!(scan.new.len(), 1);
    assert_eq!(scan.open.len(), 1);
    assert_eq!(scan.events.len(), 1);
    let conflict = &scan.new[0];
    assert_eq!(conflict.scope_kind, "path");
    assert_eq!(conflict.scope_value, "src/lib.rs");
    assert_eq!(conflict.status, "open");
    assert_eq!(conflict.session_id, session.id);
    assert_eq!(conflict.memory_a_id, first.id);
    assert_eq!(conflict.memory_b_id, second.id);
    assert_eq!(conflict.memory_a_title, "Use tabs");
    assert_eq!(conflict.memory_b_title, "Use spaces");
    assert_eq!(conflict.memory_a_kind, "decision");
    assert_eq!(conflict.memory_b_kind, "convention");
    assert!(conflict.reason.contains("src/lib.rs"));
    assert!(conflict.memory_a_excerpt.contains("tabs"));
    assert_eq!(conflict.id.len(), 32);
    assert!(
        conflict
            .id
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    );
    assert_eq!(
        conflict.id,
        memory_conflict_id(&project.id, "path", "src/lib.rs", &first, &second)
    );
    assert!(!conflict.memory_a_id.is_empty());
    assert!(same.id != same_copy.id);

    // Duplicate scans never duplicate rows or events.
    let again = store.scan_memory_conflicts(&project.id).unwrap();
    assert!(again.new.is_empty());
    assert!(again.events.is_empty());
    assert_eq!(again.open.len(), 1);
    let all = store
        .list_memory_conflicts(&project.id, None, None)
        .unwrap();
    assert_eq!(all.len(), 1);
    let events = store.list_all_events(&session.id).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "memory.conflict.detected")
            .count(),
        1
    );
}

#[test]
fn ignores_inactive_memories_and_detects_shared_task_conflicts() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("memory conflicts"))
        .unwrap();
    let session = store.create_session(&project.id, "conflicts").unwrap();
    let task = || vec![memory_evidence("task", "T9")];
    let stale = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "decision",
        "Stale decision",
        "Old answer.",
        task(),
    );
    let superseded = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "convention",
        "Superseded convention",
        "Older answer.",
        task(),
    );
    let archived = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "fact",
        "Archived fact",
        "Archived answer.",
        task(),
    );
    store
        .update_memory_with_event(&stale.id, Some("stale"), None, None)
        .unwrap();
    store
        .update_memory_with_event(&superseded.id, Some("superseded"), None, None)
        .unwrap();
    store
        .update_memory_with_event(&archived.id, Some("archived"), None, None)
        .unwrap();
    let _ = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "decision",
        "Decision on task",
        "Decisions do not join task conflicts.",
        task(),
    );

    let verification = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "verification",
        "Verification passed",
        "The verifier approved the change.",
        task(),
    );
    let result = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "task_result",
        "Task result differs",
        "The worker reported a different outcome.",
        task(),
    );
    let scan = store.scan_memory_conflicts(&project.id).unwrap();
    assert_eq!(scan.new.len(), 1);
    let conflict = &scan.new[0];
    assert_eq!(conflict.scope_kind, "task");
    assert_eq!(conflict.scope_value, "T9");
    assert_eq!(conflict.memory_a_id, verification.id);
    assert_eq!(conflict.memory_b_id, result.id);
    assert!(conflict.reason.contains("task T9"));
    assert!(conflict.memory_a_excerpt.contains("verifier"));
}

#[test]
fn memory_conflict_ids_and_ordering_are_deterministic() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("memory conflicts"))
        .unwrap();
    let session = store.create_session(&project.id, "conflicts").unwrap();
    for (title, body, reference) in [
        ("Zeta", "Zeta body.", "z/path.rs"),
        ("Omega", "Omega body.", "z/path.rs"),
        ("Alpha", "Alpha body.", "a/path.rs"),
        ("Beta", "Beta body.", "a/path.rs"),
    ] {
        create_conflict_memory(
            &store,
            &project.id,
            &session.id,
            "fact",
            title,
            body,
            vec![memory_evidence("path", reference)],
        );
    }
    let scan = store.scan_memory_conflicts(&project.id).unwrap();
    assert_eq!(scan.new.len(), 2);
    assert_eq!(scan.new[0].scope_value, "a/path.rs");
    assert_eq!(scan.new[1].scope_value, "z/path.rs");
    let first_ids = scan
        .new
        .iter()
        .map(|conflict| conflict.id.clone())
        .collect::<Vec<_>>();
    let again = store.scan_memory_conflicts(&project.id).unwrap();
    assert!(again.new.is_empty());
    let all = store
        .list_memory_conflicts(&project.id, Some("open"), None)
        .unwrap();
    assert_eq!(
        all.iter()
            .map(|conflict| conflict.id.clone())
            .collect::<Vec<_>>(),
        first_ids
    );
    // Content participates in the id: changing either body creates a new
    // review row instead of silently reopening the resolved one.
    let mut changed = store.get_memory(&scan.new[0].memory_b_id).unwrap().unwrap();
    changed.body = "A materially different body.".to_string();
    let changed_id = memory_conflict_id(
        &project.id,
        "path",
        "a/path.rs",
        &store.get_memory(&scan.new[0].memory_a_id).unwrap().unwrap(),
        &changed,
    );
    assert_ne!(changed_id, scan.new[0].id);
}

#[test]
fn resolves_memory_conflicts_with_explicit_actions_atomically() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("memory conflicts"))
        .unwrap();
    let other_repo = TempDir::new().unwrap();
    let other_project = store
        .add_project(other_repo.path().to_str().unwrap(), Some("other"))
        .unwrap();
    let session = store.create_session(&project.id, "conflicts").unwrap();
    let pair = |reference: &str, left: &str, right: &str| {
        let evidence = vec![memory_evidence("path", reference)];
        let a = create_conflict_memory(
            &store,
            &project.id,
            &session.id,
            "fact",
            &format!("{reference} A"),
            left,
            evidence.clone(),
        );
        let b = create_conflict_memory(
            &store,
            &project.id,
            &session.id,
            "fact",
            &format!("{reference} B"),
            right,
            evidence,
        );
        (a, b)
    };
    pair("src/keep-a.rs", "Keep A answer.", "Lose B answer.");
    pair("src/keep-b.rs", "Lose A answer.", "Keep B answer.");
    pair("src/both.rs", "Both A answer.", "Both B answer.");
    pair("src/archive.rs", "Archive A answer.", "Archive B answer.");
    let scan = store.scan_memory_conflicts(&project.id).unwrap();
    assert_eq!(scan.new.len(), 4);
    let find = |scope: &str| {
        scan.new
            .iter()
            .find(|conflict| conflict.scope_value == scope)
            .unwrap()
            .clone()
    };

    let keep_a = find("src/keep-a.rs");
    let resolution = store
        .resolve_memory_conflict(&project.id, &keep_a.id, "keep_a", Some("A wins"))
        .unwrap();
    assert_eq!(resolution.conflict.status, "resolved");
    assert_eq!(resolution.conflict.resolution.as_deref(), Some("keep_a"));
    assert_eq!(resolution.conflict.note.as_deref(), Some("A wins"));
    assert!(resolution.conflict.resolved_at.is_some());
    assert_eq!(resolution.memories.len(), 2);
    assert_eq!(
        store
            .get_memory(&keep_a.memory_a_id)
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
    let superseded = store.get_memory(&keep_a.memory_b_id).unwrap().unwrap();
    assert_eq!(superseded.status, "superseded");
    assert_eq!(
        superseded.superseded_by.as_deref(),
        Some(keep_a.memory_a_id.as_str())
    );
    assert!(
        resolution
            .events
            .iter()
            .any(|event| event.kind == "memory.superseded")
    );
    assert!(
        resolution
            .events
            .iter()
            .any(|event| event.kind == "memory.conflict.resolved")
    );

    let keep_b = find("src/keep-b.rs");
    let resolution = store
        .resolve_memory_conflict(&project.id, &keep_b.id, "keep_b", None)
        .unwrap();
    assert_eq!(
        store
            .get_memory(&keep_b.memory_a_id)
            .unwrap()
            .unwrap()
            .status,
        "superseded"
    );
    assert_eq!(
        store
            .get_memory(&keep_b.memory_b_id)
            .unwrap()
            .unwrap()
            .status,
        "active"
    );
    assert_eq!(resolution.conflict.resolution.as_deref(), Some("keep_b"));

    let both = find("src/both.rs");
    let resolution = store
        .resolve_memory_conflict(&project.id, &both.id, "keep_both", None)
        .unwrap();
    assert_eq!(
        store.get_memory(&both.memory_a_id).unwrap().unwrap().status,
        "active"
    );
    assert_eq!(
        store.get_memory(&both.memory_b_id).unwrap().unwrap().status,
        "active"
    );
    assert!(
        !resolution
            .events
            .iter()
            .any(|event| event.kind == "memory.superseded")
    );
    assert_eq!(resolution.events.len(), 1);

    let archive = find("src/archive.rs");
    let resolution = store
        .resolve_memory_conflict(&project.id, &archive.id, "archive_both", None)
        .unwrap();
    assert_eq!(
        store
            .get_memory(&archive.memory_a_id)
            .unwrap()
            .unwrap()
            .status,
        "archived"
    );
    assert_eq!(
        store
            .get_memory(&archive.memory_b_id)
            .unwrap()
            .unwrap()
            .status,
        "archived"
    );
    assert_eq!(
        resolution
            .events
            .iter()
            .filter(|event| event.kind == "memory.updated")
            .count(),
        2
    );

    // Resolving twice fails; cross-project ids fail; invalid actions fail.
    assert!(
        store
            .resolve_memory_conflict(&project.id, &keep_a.id, "keep_a", None)
            .is_err()
    );
    assert!(
        store
            .resolve_memory_conflict(&other_project.id, &keep_b.id, "keep_a", None)
            .is_err()
    );
    assert!(
        store
            .resolve_memory_conflict(&project.id, &both.id, "invented", None)
            .is_err()
    );

    // A deleted memory tombstones its conflicts; resolution cannot resurrect it.
    let deleted = find("src/both.rs");
    store
        .delete_memories_with_events(std::slice::from_ref(&deleted.memory_a_id))
        .unwrap();
    let error = store
        .resolve_memory_conflict(&project.id, &deleted.id, "keep_b", None)
        .unwrap_err();
    assert!(error.to_string().contains("no longer exists"));
    assert!(
        store
            .list_memory_conflicts(&project.id, Some("resolved"), None)
            .unwrap()
            .iter()
            .all(|conflict| conflict.id != deleted.id)
    );
}

#[test]
fn memory_health_counts_lifecycle_evidence_and_verification_due() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("memory health"))
        .unwrap();
    let session = store.create_session(&project.id, "health").unwrap();
    let active = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "fact",
        "Active evidenced",
        "Active body.",
        vec![memory_evidence("path", "src/health.rs")],
    );
    let due = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "fact",
        "Active due",
        "Due body.",
        vec![memory_evidence("path", "src/due.rs")],
    );
    let pinned = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "decision",
        "Pinned due",
        "Pinned body.",
        Vec::new(),
    );
    let stale = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "convention",
        "Stale row",
        "Stale body.",
        Vec::new(),
    );
    let superseded = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "task_result",
        "Superseded row",
        "Superseded body.",
        Vec::new(),
    );
    let archived = create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "verification",
        "Archived row",
        "Archived body.",
        Vec::new(),
    );
    // Expired TTLs and explicit verifies use the normal event-backed paths.
    store
        .mark_memory_verified_with_event(
            &due.id,
            "2026-01-01T00:00:00Z",
            Some("2026-02-01T00:00:00Z"),
        )
        .unwrap();
    store
        .mark_memory_verified_with_event(
            &pinned.id,
            "2026-01-01T00:00:00Z",
            Some("2026-02-01T00:00:00Z"),
        )
        .unwrap();
    store
        .update_memory_with_event(&pinned.id, None, Some(true), None)
        .unwrap();
    store
        .mark_memory_stale_with_event(&stale.id, "verification expired")
        .unwrap();
    store
        .update_memory_with_event(&superseded.id, Some("superseded"), None, None)
        .unwrap();
    store
        .update_memory_with_event(&archived.id, Some("archived"), None, None)
        .unwrap();
    create_conflict_memory(
        &store,
        &project.id,
        &session.id,
        "fact",
        "Another active same scope",
        "Different body.",
        vec![memory_evidence("path", "src/health.rs")],
    );
    let scan = store.scan_memory_conflicts(&project.id).unwrap();
    assert_eq!(scan.open.len(), 1);

    let health = store.memory_health(&project.id).unwrap();
    assert_eq!(health.total, 7);
    assert_eq!(health.active, 4);
    assert_eq!(health.stale, 1);
    assert_eq!(health.superseded, 1);
    assert_eq!(health.archived, 1);
    assert_eq!(health.pinned, 1);
    assert_eq!(health.evidence_covered, 3);
    assert_eq!(health.verified, 2);
    assert_eq!(health.verification_due, 1);
    assert_eq!(health.open_conflicts, 1);
    assert!(
        health
            .by_kind
            .iter()
            .any(|count| count.key == "fact" && count.count == 3)
    );
    assert!(!active.id.is_empty());
}

#[test]
fn daemon_metrics_snapshot_is_empty_for_a_fresh_store() {
    let (_data, store, _repo) = test_store();
    let snapshot = store.daemon_metrics_snapshot().unwrap();
    assert_eq!(snapshot.projects, 0);
    assert!(snapshot.sessions.is_empty());
    assert!(snapshot.plan_tasks.is_empty());
    assert!(snapshot.merge_queue.is_empty());
    assert!(snapshot.intake.is_empty());
    assert!(snapshot.continuations.is_empty());
    assert!(snapshot.recoveries.is_empty());
    assert!(snapshot.memories.is_empty());
    assert!(snapshot.memory_conflicts.is_empty());
    assert!(snapshot.runtime_leases.is_empty());
    assert_eq!(snapshot.tokens_in, 0);
    assert_eq!(snapshot.tokens_out, 0);
    assert_eq!(snapshot.cache_read_tokens, 0);
    assert_eq!(snapshot.cache_write_tokens, 0);
    assert_eq!(snapshot.cost_spent_usd, 0.0);
}

#[test]
fn daemon_metrics_snapshot_counts_statuses_and_usage_totals() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let draft = store.create_session(&project.id, "draft").unwrap();
    let running_a = store.create_session(&project.id, "running a").unwrap();
    let running_b = store.create_session(&project.id, "running b").unwrap();
    store
        .update_session_status(&running_a.id, "running")
        .unwrap();
    store
        .update_session_status(&running_b.id, "running")
        .unwrap();

    store
        .add_usage_entry_with_events(
            &draft.id,
            "agent-1",
            "Leader",
            "leader",
            None,
            None,
            "provider-model",
            100,
            50,
            20,
            10,
            false,
            0.25,
        )
        .unwrap();
    store
        .add_usage_entry_with_events(
            &running_a.id,
            "agent-2",
            "Worker",
            "worker",
            Some("T1"),
            Some("engineering"),
            "provider-model",
            200,
            100,
            0,
            0,
            false,
            0.5,
        )
        .unwrap();

    {
        let conn = store.lock().unwrap();
        let now = "2026-11-11T00:00:00Z";
        for (task_id, status) in [("T1", "pending"), ("T2", "completed"), ("T3", "pending")] {
            conn.execute(
                "INSERT INTO plan_tasks
                 (session_id, plan_seq, task_id, project_id, department, title, status,
                  enqueued_at, updated_at)
                 VALUES (?1, 1, ?2, ?3, 'engineering', ?2, ?4, ?5, ?5)",
                params![draft.id, task_id, project.id, status, now],
            )
            .unwrap();
        }
        for (id, status) in [
            ("merge-1", "queued"),
            ("merge-2", "merged"),
            ("merge-3", "failed"),
            ("merge-4", "merged"),
        ] {
            conn.execute(
                "INSERT INTO merge_queue
                 (id, session_id, agent_id, agent_name, task_id, branch, commit_hash,
                  base_commit, status, created_at, updated_at)
                 VALUES (?1, ?2, 'agent-1', 'Worker', NULL, ?1, 'commit', 'base', ?3, ?4, ?4)",
                params![id, draft.id, status, now],
            )
            .unwrap();
        }
        for (id, status) in [
            ("intake-1", "queued"),
            ("intake-2", "failed"),
            ("intake-3", "queued"),
        ] {
            conn.execute(
                "INSERT INTO intake_tasks
                 (id, project_id, title, instructions, profile, kind, status,
                  created_at, updated_at)
                 VALUES (?1, ?2, ?1, 'do it', 'coding', 'task', ?3, ?4, ?4)",
                params![id, project.id, status, now],
            )
            .unwrap();
        }
        for (id, status) in [("cont-1", "pending"), ("cont-2", "delivered")] {
            conn.execute(
                "INSERT INTO session_continuations
                 (id, session_id, project_id, body, status, created_at, updated_at)
                 VALUES (?1, ?2, ?3, 'continue', ?4, ?5, ?5)",
                params![id, draft.id, project.id, status, now],
            )
            .unwrap();
        }
        for (task_id, status) in [("T1", "waiting_cancel"), ("T2", "retry_requested")] {
            conn.execute(
                "INSERT INTO orchestration_recoveries
                 (session_id, plan_seq, task_id, project_id, department, action,
                  status, requested_at, updated_at)
                 VALUES (?1, 1, ?2, ?3, 'engineering', 'retry', ?4, ?5, ?5)",
                params![draft.id, task_id, project.id, status, now],
            )
            .unwrap();
        }
        for (id, status) in [("mem-a", "active"), ("mem-b", "active"), ("mem-c", "stale")] {
            conn.execute(
                "INSERT INTO memories
                 (id, session_id, project_id, agent_id, agent_name, kind, title, body,
                  tags, created_at, status)
                 VALUES (?1, ?2, ?3, 'agent-1', 'Leader', 'fact', ?1, 'body',
                         '[]', ?4, ?5)",
                params![id, draft.id, project.id, now, status],
            )
            .unwrap();
        }
        for (id, status) in [("conflict-1", "open"), ("conflict-2", "resolved")] {
            conn.execute(
                "INSERT INTO memory_conflicts
                 (id, project_id, session_id, memory_a_id, memory_b_id, memory_a_title,
                  memory_b_title, memory_a_kind, memory_b_kind, memory_a_excerpt,
                  memory_b_excerpt, scope_kind, scope_value, reason, status, detected_at)
                 VALUES (?1, ?2, ?3, 'mem-a', 'mem-b', 'A', 'B', 'fact', 'fact',
                         'a', 'b', 'path', 'src/lib.rs', 'different', ?4, ?5)",
                params![id, project.id, draft.id, status, now],
            )
            .unwrap();
        }
        for (key, kind) in [
            ("lease-tool", "tool"),
            ("lease-session-a", "session"),
            ("lease-session-b", "session"),
        ] {
            conn.execute(
                "INSERT INTO runtime_leases
                 (lease_key, owner_id, kind, session_id, acquired_at, heartbeat_at,
                  expires_at)
                 VALUES (?1, 'owner', ?2, ?3, ?4, ?4, ?4)",
                params![key, kind, draft.id, now],
            )
            .unwrap();
        }
    }

    let snapshot = store.daemon_metrics_snapshot().unwrap();
    assert_eq!(snapshot.projects, 1);
    assert_eq!(
        snapshot.sessions,
        vec![
            MetricCount {
                key: "draft".to_string(),
                count: 1,
            },
            MetricCount {
                key: "running".to_string(),
                count: 2,
            },
        ]
    );
    assert_eq!(
        snapshot.plan_tasks,
        vec![
            MetricCount {
                key: "completed".to_string(),
                count: 1,
            },
            MetricCount {
                key: "pending".to_string(),
                count: 2,
            },
        ]
    );
    assert_eq!(
        snapshot.merge_queue,
        vec![
            MetricCount {
                key: "failed".to_string(),
                count: 1,
            },
            MetricCount {
                key: "merged".to_string(),
                count: 2,
            },
            MetricCount {
                key: "queued".to_string(),
                count: 1,
            },
        ]
    );
    assert_eq!(
        snapshot.intake,
        vec![
            MetricCount {
                key: "failed".to_string(),
                count: 1,
            },
            MetricCount {
                key: "queued".to_string(),
                count: 2,
            },
        ]
    );
    assert_eq!(
        snapshot.continuations,
        vec![
            MetricCount {
                key: "delivered".to_string(),
                count: 1,
            },
            MetricCount {
                key: "pending".to_string(),
                count: 1,
            },
        ]
    );
    assert_eq!(
        snapshot.recoveries,
        vec![
            MetricCount {
                key: "retry_requested".to_string(),
                count: 1,
            },
            MetricCount {
                key: "waiting_cancel".to_string(),
                count: 1,
            },
        ]
    );
    assert_eq!(
        snapshot.memories,
        vec![
            MetricCount {
                key: "active".to_string(),
                count: 2,
            },
            MetricCount {
                key: "stale".to_string(),
                count: 1,
            },
        ]
    );
    assert_eq!(
        snapshot.memory_conflicts,
        vec![
            MetricCount {
                key: "open".to_string(),
                count: 1,
            },
            MetricCount {
                key: "resolved".to_string(),
                count: 1,
            },
        ]
    );
    assert_eq!(
        snapshot.runtime_leases,
        vec![
            MetricCount {
                key: "session".to_string(),
                count: 2,
            },
            MetricCount {
                key: "tool".to_string(),
                count: 1,
            },
        ]
    );
    assert_eq!(snapshot.tokens_in, 300);
    assert_eq!(snapshot.tokens_out, 150);
    assert_eq!(snapshot.cache_read_tokens, 20);
    assert_eq!(snapshot.cache_write_tokens, 10);
    assert!((snapshot.cost_spent_usd - 0.75).abs() < 1e-9);
}
