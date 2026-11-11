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
fn persists_cache_policy_and_records_it_in_the_manifest() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let default = store.get_cache_policy(&project.id).unwrap();
    assert!(default.enabled);
    assert_eq!(default.ttl, "5m");
    assert!(default.automatic_caching);
    assert!(!default.warm_on_resume);
    assert!(default.compact_aware);

    let mut policy = CachePolicy {
        enabled: true,
        ttl: "1h".into(),
        automatic_caching: false,
        warm_on_resume: true,
        compact_aware: false,
        role_ttl: std::collections::BTreeMap::from([("worker".to_string(), "5m".to_string())]),
    };
    policy = policy.normalized().unwrap();
    store.set_cache_policy(&project.id, &policy).unwrap();
    assert_eq!(store.get_cache_policy(&project.id).unwrap(), policy);

    // Replacing the row updates one project-scoped policy; it never appends a
    // per-call row or duplicates state.
    let mut updated = policy.clone();
    updated.ttl = "5m".into();
    updated.warm_on_resume = false;
    store.set_cache_policy(&project.id, &updated).unwrap();
    assert_eq!(store.get_cache_policy(&project.id).unwrap(), updated);

    store.record_project_manifest().unwrap();
    let snapshots = read_project_manifest(data.path()).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].cache_policy, updated);
}

#[test]
fn cache_policy_survives_reopening_the_database() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let project_id = {
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("demo"))
            .unwrap();
        let policy = CachePolicy {
            enabled: false,
            ttl: "1h".into(),
            automatic_caching: false,
            warm_on_resume: true,
            compact_aware: false,
            role_ttl: std::collections::BTreeMap::from([("worker".to_string(), "5m".to_string())]),
        };
        store.set_cache_policy(&project.id, &policy).unwrap();
        project.id
    };
    let reopened = Store::open(data.path()).unwrap();
    let policy = reopened.get_cache_policy(&project_id).unwrap();
    assert!(!policy.enabled);
    assert_eq!(policy.ttl, "1h");
    assert!(!policy.automatic_caching);
    assert!(policy.warm_on_resume);
    assert!(!policy.compact_aware);
    assert_eq!(
        policy.role_ttl.get("worker").map(String::as_str),
        Some("5m")
    );
}

#[test]
fn legacy_manifest_without_cache_policy_decodes_the_default() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    store.record_project_manifest().unwrap();
    let mut snapshots = read_project_manifest(data.path()).unwrap();
    let snapshot = snapshots.remove(0);
    let mut value = serde_json::to_value(&snapshot).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("cache_policy")
        .expect("new manifest field is present");
    let legacy: ProjectManifestSnapshot = serde_json::from_value(value).unwrap();
    assert_eq!(legacy.project.id, project.id);
    assert!(legacy.cache_policy.enabled);
    assert_eq!(legacy.cache_policy.ttl, "5m");
    assert!(legacy.cache_policy.automatic_caching);
    assert!(!legacy.cache_policy.warm_on_resume);
    assert!(legacy.cache_policy.compact_aware);
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
fn migrates_legacy_acp_agent_rows_to_stdio_defaults() {
    let data = TempDir::new().unwrap();
    let database = data.path().join("verlinpark.db");
    {
        let conn = rusqlite::Connection::open(&database).unwrap();
        conn.execute_batch(
            "CREATE TABLE acp_agents (
                id           TEXT PRIMARY KEY,
                name         TEXT NOT NULL UNIQUE,
                command      TEXT NOT NULL,
                args         TEXT NOT NULL,
                env          TEXT NOT NULL,
                enabled      INTEGER NOT NULL DEFAULT 1,
                auto_approve INTEGER NOT NULL DEFAULT 0,
                timeout_secs INTEGER NOT NULL DEFAULT 1800,
                created_at   TEXT NOT NULL
            );
            INSERT INTO acp_agents
                (id, name, command, args, env, enabled, auto_approve, timeout_secs, created_at)
            VALUES
                ('legacy', 'legacy', 'npx', '[]', '{}', 1, 0, 900, '2026-11-01T00:00:00Z');",
        )
        .unwrap();
    }
    let store = Store::open(data.path()).unwrap();
    let agent = store.find_acp_agent("legacy").unwrap().unwrap();
    assert_eq!(agent.command, "npx");
    assert_eq!(agent.transport, vp_protocol::types::AcpTransport::Stdio);
    assert!(agent.endpoint.is_none());
    assert!(agent.auth_header_env.is_none());
    assert!(agent.headers.is_empty());
    assert_eq!(agent.connect_timeout_secs, 30);
    assert!(!agent.allow_remote);
    assert_eq!(agent.timeout_secs, 900);
}

#[test]
fn persists_remote_acp_transport_configuration() {
    let (_data, store, _repo) = test_store();
    let agent = store
        .add_acp_agent_config(&vp_protocol::types::AcpAgentConfig {
            name: "remote".into(),
            command: String::new(),
            args: Vec::new(),
            env: std::collections::BTreeMap::new(),
            enabled: true,
            auto_approve: false,
            timeout_secs: 900,
            transport: vp_protocol::types::AcpTransport::StreamableHttp,
            endpoint: Some("https://agent.example.test/acp".into()),
            auth_header_env: Some("ACP_TOKEN".into()),
            headers: std::collections::BTreeMap::from([("x-project".into(), "demo".into())]),
            connect_timeout_secs: 15,
            allow_remote: true,
        })
        .unwrap();
    assert_eq!(
        agent.transport,
        vp_protocol::types::AcpTransport::StreamableHttp
    );
    assert_eq!(
        agent.endpoint.as_deref(),
        Some("https://agent.example.test/acp")
    );
    assert_eq!(agent.auth_header_env.as_deref(), Some("ACP_TOKEN"));
    assert_eq!(agent.connect_timeout_secs, 15);
    assert!(agent.allow_remote);

    let loaded = store.get_acp_agent(&agent.id).unwrap().unwrap();
    assert_eq!(loaded, agent);
    assert_eq!(
        loaded.headers.get("x-project").map(String::as_str),
        Some("demo")
    );

    let updated = store
        .update_acp_agent_config(
            &agent.id,
            &vp_protocol::types::AcpAgentConfig {
                name: "remote-ws".into(),
                command: String::new(),
                args: Vec::new(),
                env: std::collections::BTreeMap::new(),
                enabled: false,
                auto_approve: true,
                timeout_secs: 300,
                transport: vp_protocol::types::AcpTransport::Websocket,
                endpoint: Some("wss://agent.example.test/acp".into()),
                auth_header_env: None,
                headers: std::collections::BTreeMap::new(),
                connect_timeout_secs: 5,
                allow_remote: true,
            },
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        updated.transport,
        vp_protocol::types::AcpTransport::Websocket
    );
    assert_eq!(
        updated.endpoint.as_deref(),
        Some("wss://agent.example.test/acp")
    );
    assert!(updated.auth_header_env.is_none());
    assert!(!updated.enabled);
    assert!(updated.headers.is_empty());
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
        embedding: None,
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
fn runtime_leases_are_owned_fenced_renewable_and_releasable() {
    let (_data, store, _repo) = test_store();
    let first = store
        .acquire_runtime_lease(
            "session:s1",
            "daemon-a",
            "instance-a:1",
            "session",
            Some("s1"),
            60,
        )
        .unwrap();
    assert_eq!(first.fencing_token(), 1);
    // A live lease rejects a competitor with a typed busy error.
    let busy = store
        .acquire_runtime_lease(
            "session:s1",
            "daemon-b",
            "instance-b:2",
            "session",
            Some("s1"),
            60,
        )
        .unwrap_err();
    assert!(matches!(busy, RuntimeLeaseError::Busy { .. }));
    assert_eq!(
        store
            .runtime_lease_owner("session:s1")
            .unwrap()
            .unwrap()
            .owner_id,
        "daemon-a"
    );

    // Renewal is fenced on the acquisition token.
    let renewed = store
        .renew_runtime_lease("session:s1", "daemon-a", first.token(), 120)
        .unwrap();
    assert_eq!(renewed.fencing_token, 1);
    assert!(!renewed.renewed_at.is_empty());

    // Reacquisition by the same owner creates a new generation and token; the
    // previous guard becomes stale immediately.
    let second = store
        .acquire_runtime_lease(
            "session:s1",
            "daemon-a",
            "instance-a:1",
            "session",
            Some("s1"),
            60,
        )
        .unwrap();
    assert_eq!(second.fencing_token(), 2);
    assert!(matches!(
        store
            .renew_runtime_lease("session:s1", "daemon-a", first.token(), 60)
            .unwrap_err(),
        RuntimeLeaseError::Lost {
            reason: LeaseLostReason::TokenMismatch,
            ..
        }
    ));
    assert!(matches!(
        store
            .release_runtime_lease("session:s1", "daemon-a", first.token())
            .unwrap_err(),
        RuntimeLeaseError::Lost {
            reason: LeaseLostReason::TokenMismatch,
            ..
        }
    ));

    // Release is a tombstone internally, so it cannot erase the generation.
    assert!(
        store
            .release_runtime_lease("session:s1", "daemon-a", second.token())
            .unwrap()
    );
    assert!(store.runtime_lease_owner("session:s1").unwrap().is_none());
    assert!(store.list_runtime_leases().unwrap().is_empty());
    assert_eq!(
        store.runtime_lease_fencing_token("session:s1").unwrap(),
        Some(2)
    );

    let third = store
        .acquire_runtime_lease(
            "session:s1",
            "daemon-b",
            "instance-b:2",
            "session",
            Some("s1"),
            60,
        )
        .unwrap();
    assert_eq!(third.fencing_token(), 3);
    assert_eq!(store.list_runtime_leases().unwrap().len(), 1);
    // Releasing with the previous generation must not delete the new owner.
    assert!(
        store
            .release_runtime_lease("session:s1", "daemon-a", second.token())
            .is_err()
    );
    assert_eq!(
        store
            .runtime_lease_owner("session:s1")
            .unwrap()
            .unwrap()
            .owner_id,
        "daemon-b"
    );

    // The public projection never carries the token.
    let encoded =
        serde_json::to_value(store.runtime_lease_owner("session:s1").unwrap().unwrap()).unwrap();
    assert!(encoded.get("lease_token").is_none());
    assert!(encoded.get("token").is_none());
    assert_eq!(encoded["fencing_token"], 3);
}

#[test]
fn expired_runtime_leases_can_be_swept_or_taken_over() {
    let (_data, store, _repo) = test_store();
    let first = store
        .acquire_runtime_lease(
            "session:s2",
            "daemon-a",
            "instance-a:1",
            "session",
            Some("s2"),
            0,
        )
        .unwrap();
    assert_eq!(first.fencing_token(), 1);
    assert_eq!(
        store
            .runtime_lease_owner("session:s2")
            .unwrap()
            .unwrap()
            .state(),
        RuntimeLeaseState::Stale
    );
    // A zero-TTL lease is effectively expired immediately; a second owner can
    // take it over without waiting for a sweep.
    let second = store
        .acquire_runtime_lease(
            "session:s2",
            "daemon-b",
            "instance-b:2",
            "session",
            Some("s2"),
            60,
        )
        .unwrap();
    assert_eq!(second.fencing_token(), 2);
    let lease = store.runtime_lease_owner("session:s2").unwrap().unwrap();
    assert_eq!(lease.owner_id, "daemon-b");
    assert_eq!(lease.state(), RuntimeLeaseState::Live);
    assert_eq!(store.clear_expired_runtime_leases().unwrap(), 0);
}

#[test]
fn lease_expiry_comparison_handles_rfc3339_fractional_seconds() {
    let (_data, store, _repo) = test_store();
    store
        .acquire_runtime_lease(
            "session:fractional",
            "daemon-a",
            "instance-a:1",
            "session",
            Some("s"),
            60,
        )
        .unwrap();
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
    let second = store
        .acquire_runtime_lease(
            "session:fractional",
            "daemon-b",
            "instance-b:2",
            "session",
            Some("s"),
            60,
        )
        .unwrap();
    assert_eq!(second.fencing_token(), 2);
    let lease = store
        .runtime_lease_owner("session:fractional")
        .unwrap()
        .unwrap();
    assert_eq!(lease.owner_id, "daemon-b");

    store
        .acquire_runtime_lease(
            "session:fractional-2",
            "daemon-a",
            "instance-a:1",
            "session",
            Some("s2"),
            60,
        )
        .unwrap();
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
fn concurrent_runtime_lease_acquisition_has_exactly_one_winner() {
    let data = TempDir::new().unwrap();
    Store::open(data.path()).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let mut handles = Vec::new();
    for index in 0..8 {
        let data = data.path().to_path_buf();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            let store = Store::open(&data).unwrap();
            barrier.wait();
            store.acquire_runtime_lease(
                "session:race",
                &format!("daemon-{index}"),
                &format!("instance-{index}:{index}"),
                "session",
                Some("race"),
                60,
            )
        }));
    }
    let results = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    let winners = results.iter().filter(|result| result.is_ok()).count();
    let busy = results
        .iter()
        .filter(|result| matches!(result, Err(RuntimeLeaseError::Busy { .. })))
        .count();
    assert_eq!(winners, 1, "{results:?}");
    assert_eq!(busy, 7);
    let store = Store::open(data.path()).unwrap();
    let leases = store.list_runtime_leases().unwrap();
    assert_eq!(leases.len(), 1);
    assert_eq!(leases[0].fencing_token, 1);
}

#[test]
fn stale_assert_renew_and_release_fail_after_reacquisition() {
    let (_data, store, _repo) = test_store();
    let stale = store
        .acquire_runtime_lease(
            "integration:s1",
            "daemon-a",
            "instance-a:1",
            "integration",
            Some("s1"),
            0,
        )
        .unwrap();
    let current = store
        .acquire_runtime_lease(
            "integration:s1",
            "daemon-b",
            "instance-b:2",
            "integration",
            Some("s1"),
            60,
        )
        .unwrap();
    assert_eq!(current.fencing_token(), 2);
    assert!(matches!(
        store
            .assert_runtime_lease("integration:s1", "daemon-a", stale.token())
            .unwrap_err(),
        RuntimeLeaseError::Lost { .. }
    ));
    assert!(
        store
            .renew_runtime_lease("integration:s1", "daemon-a", stale.token(), 60)
            .is_err()
    );
    assert!(
        store
            .release_runtime_lease("integration:s1", "daemon-a", stale.token())
            .is_err()
    );
    let holder = store
        .assert_runtime_lease("integration:s1", "daemon-b", current.token())
        .unwrap();
    assert_eq!(holder.owner_id, "daemon-b");
}

#[test]
fn fencing_generation_survives_row_cleanup() {
    let (_data, store, _repo) = test_store();
    store
        .acquire_runtime_lease(
            "session:s3",
            "daemon-a",
            "instance-a:1",
            "session",
            Some("s3"),
            0,
        )
        .unwrap();
    assert_eq!(store.clear_expired_runtime_leases().unwrap(), 1);
    assert!(store.runtime_lease_owner("session:s3").unwrap().is_none());
    let next = store
        .acquire_runtime_lease(
            "session:s3",
            "daemon-b",
            "instance-b:2",
            "session",
            Some("s3"),
            60,
        )
        .unwrap();
    assert_eq!(next.fencing_token(), 2);
}

#[test]
fn startup_cleanup_only_removes_this_process_leases() {
    let (_data, store, _repo) = test_store();
    let own_instance = store.process_owner_instance();
    store
        .acquire_runtime_lease(
            "session:own",
            "daemon-a",
            &own_instance,
            "session",
            Some("own"),
            60,
        )
        .unwrap();
    store
        .acquire_runtime_lease(
            "session:foreign",
            "daemon-b",
            "foreign-instance:999",
            "session",
            Some("foreign"),
            60,
        )
        .unwrap();
    assert_eq!(store.clear_own_process_runtime_leases().unwrap(), 1);
    assert!(store.runtime_lease_owner("session:own").unwrap().is_none());
    assert!(
        store
            .runtime_lease_owner("session:foreign")
            .unwrap()
            .is_some()
    );
}

#[test]
fn daemon_identity_is_stable_across_store_instances() {
    let data = TempDir::new().unwrap();
    let first = Store::open(data.path()).unwrap();
    let second = Store::open(data.path()).unwrap();
    assert_eq!(first.daemon_identity(), second.daemon_identity());
    assert_eq!(first.daemon_identity().instance_id.len(), 36);
    assert!(!first.owner_id_for_nonce("nonce").is_empty());
    assert_ne!(first.owner_id_for_nonce("a"), first.owner_id_for_nonce("b"));
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
    assert!(snapshot.preamble_tokens_estimate.is_none());
    assert_eq!(snapshot.cost_spent_usd, 0.0);
}

#[test]
fn daemon_metrics_snapshot_reads_latest_preamble_estimate() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("cache"))
        .unwrap();
    let session = store.create_session(&project.id, "cache").unwrap();
    store
        .append_event(
            &session.id,
            "context.prompt_prefix",
            serde_json::json!({
                "prefix_hash": "hash-1",
                "preamble_tokens_estimate": 7_000_u64,
            }),
        )
        .unwrap();
    assert_eq!(
        store
            .daemon_metrics_snapshot()
            .unwrap()
            .preamble_tokens_estimate,
        Some(7_000)
    );

    store
        .append_event(
            &session.id,
            "context.prompt_prefix",
            serde_json::json!({
                "prefix_hash": "hash-2",
                "stable_prefix_chars": 4_000_u64,
            }),
        )
        .unwrap();
    assert_eq!(
        store
            .daemon_metrics_snapshot()
            .unwrap()
            .preamble_tokens_estimate,
        Some(1_000)
    );
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

#[test]
fn session_operation_claims_replay_conflict_and_report_in_progress() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("operations"))
        .unwrap();
    let session = store
        .create_session(&project.id, "idempotent prompt")
        .unwrap();

    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-1", "session.prompt", "fingerprint-a")
            .unwrap(),
        Claim::New
    );
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-1", "session.prompt", "fingerprint-a")
            .unwrap(),
        Claim::InProgress
    );
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-1", "session.prompt", "fingerprint-b")
            .unwrap(),
        Claim::Conflict
    );
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-1", "session.steer", "fingerprint-a")
            .unwrap(),
        Claim::Conflict
    );

    let result = serde_json::json!({"accepted": true}).to_string();
    store
        .complete_session_operation(&session.id, "op-1", &result)
        .unwrap();
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-1", "session.prompt", "fingerprint-a")
            .unwrap(),
        Claim::Replay(result.clone())
    );
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-1", "session.prompt", "fingerprint-b")
            .unwrap(),
        Claim::Conflict
    );

    // The same operation id in another session is a separate receipt.
    let other = store.create_session(&project.id, "other session").unwrap();
    assert_eq!(
        store
            .claim_session_operation(&other.id, "op-1", "session.prompt", "fingerprint-a")
            .unwrap(),
        Claim::New
    );
}

#[test]
fn session_operation_failed_rows_can_be_retried() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("operations"))
        .unwrap();
    let session = store.create_session(&project.id, "retry").unwrap();
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-2", "session.defer", "same")
            .unwrap(),
        Claim::New
    );
    store
        .fail_session_operation(&session.id, "op-2", "session was landed")
        .unwrap();
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-2", "session.defer", "same")
            .unwrap(),
        Claim::New
    );
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-2", "session.defer", "different")
            .unwrap(),
        Claim::Conflict
    );
    store
        .complete_session_operation(&session.id, "op-2", "{\"deferred\":true}")
        .unwrap();
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-2", "session.defer", "same")
            .unwrap(),
        Claim::Replay("{\"deferred\":true}".to_string())
    );
}

#[test]
fn startup_marks_pending_session_operations_unknown() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("operations"))
        .unwrap();
    let session = store.create_session(&project.id, "crash window").unwrap();
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-3", "session.prompt", "same")
            .unwrap(),
        Claim::New
    );
    assert_eq!(store.mark_pending_session_operations_unknown().unwrap(), 1);
    assert_eq!(
        store
            .claim_session_operation(&session.id, "op-3", "session.prompt", "same")
            .unwrap(),
        Claim::Unknown
    );
    assert_eq!(store.mark_pending_session_operations_unknown().unwrap(), 0);
}

fn program_input(project_id: &str, status: ProgramStatus) -> NewProgram {
    NewProgram {
        project_id: project_id.to_string(),
        goal: "Program goal".into(),
        policy: ProgramPolicy {
            auto_dispatch: true,
            max_attempts: 3,
            retry_backoff_secs: 30,
            dependency_failure: vp_protocol::types::DependencyFailurePolicy::Block,
            token_budget: Some(50_000),
            cost_budget_usd: Some(5.0),
            max_parallel_milestones: 2,
        },
        token_budget: Some(100_000),
        cost_budget_usd: Some(10.0),
        status,
        archived: false,
    }
}

fn milestone_input(program_id: &str, key: &str, deps: Vec<String>) -> NewProgramMilestone {
    NewProgramMilestone {
        program_id: program_id.to_string(),
        key: key.to_string(),
        title: format!("Milestone {key}"),
        objective: "Deliver the milestone".into(),
        acceptance: vec!["accepted".into()],
        deps,
        priority: 3,
        max_attempts: 2,
        profile: "coding".into(),
        write_scope: vec!["src".into()],
        token_budget: Some(20_000),
        cost_budget_usd: None,
    }
}

#[test]
fn program_rows_round_trip_with_full_row_events() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("programs"))
        .unwrap();
    let (program, created) = store
        .create_program(&program_input(&project.id, ProgramStatus::Active))
        .unwrap();
    assert_eq!(created.seq, 1);
    assert_eq!(created.kind, "program.created");
    let created_payload: Program = serde_json::from_value(created.payload).unwrap();
    assert_eq!(created_payload, program);

    let (first, first_event) = store
        .create_program_milestone(&milestone_input(&program.id, "1", Vec::new()))
        .unwrap();
    let (second, second_event) = store
        .create_program_milestone(&milestone_input(&program.id, "2", vec![first.id.clone()]))
        .unwrap();
    assert_eq!(first_event.seq, 2);
    assert_eq!(second_event.seq, 3);
    assert_eq!(second.deps, vec![first.id.clone()]);

    let update = ProgramUpdate {
        goal: "Updated program goal".into(),
        policy: program.policy.clone(),
        token_budget: Some(200_000),
        cost_budget_usd: Some(20.0),
    };
    let (updated, update_event) = store.update_program(&program.id, &update).unwrap().unwrap();
    assert_eq!(updated.goal, "Updated program goal");
    assert_eq!(update_event.seq, 4);
    assert_eq!(update_event.kind, "program.updated");

    let listed = store
        .list_programs(Some(&project.id), Some(ProgramStatus::Active), Some(10))
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].token_budget, Some(200_000));
    let milestones = store.list_program_milestones(&program.id).unwrap();
    assert_eq!(milestones.len(), 2);
    assert_eq!(milestones[0].key, "1");
    assert_eq!(milestones[1].key, "2");
    let events = store.list_program_events(&program.id, Some(100)).unwrap();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0].kind, "program.created");
    assert_eq!(events[3].kind, "program.updated");
    assert_eq!(
        events.iter().map(|event| event.seq).collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
}

#[test]
fn program_claim_respects_dependencies_attempts_and_backoff() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("claim"))
        .unwrap();
    let (program, _) = store
        .create_program(&program_input(&project.id, ProgramStatus::Active))
        .unwrap();
    let (first, _) = store
        .create_program_milestone(&milestone_input(&program.id, "1", Vec::new()))
        .unwrap();
    let (second, _) = store
        .create_program_milestone(&milestone_input(&program.id, "2", vec![first.id.clone()]))
        .unwrap();

    assert!(
        store
            .claim_program_milestone_start(&second.id)
            .unwrap()
            .is_none(),
        "dependency must be complete before claiming the child"
    );
    let (claimed, started) = store
        .claim_program_milestone_start(&first.id)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.status, ProgramMilestoneStatus::Running);
    assert_eq!(claimed.attempts, 1);
    assert_eq!(started.kind, "program.milestone.started");
    assert!(
        store
            .claim_program_milestone_start(&first.id)
            .unwrap()
            .is_none(),
        "a running milestone cannot be claimed twice"
    );

    store.mark_program_milestone_verifying(&first.id).unwrap();
    store.complete_program_milestone(&first.id, "done").unwrap();
    let (child, _) = store
        .claim_program_milestone_start(&second.id)
        .unwrap()
        .unwrap();
    assert_eq!(child.status, ProgramMilestoneStatus::Running);
    assert_eq!(child.attempts, 1);

    let (third, _) = store
        .create_program_milestone(&milestone_input(&program.id, "3", Vec::new()))
        .unwrap();
    let _ = store
        .claim_program_milestone_start(&third.id)
        .unwrap()
        .unwrap();
    let future = (time::OffsetDateTime::now_utc() + time::Duration::hours(1))
        .format(&Rfc3339)
        .unwrap();
    store
        .fail_program_milestone(&third.id, "test failure", Some(&future))
        .unwrap();
    assert!(
        store
            .claim_program_milestone_start(&third.id)
            .unwrap()
            .is_none(),
        "a failed milestone inside its backoff window is not runnable"
    );
    let row = store.get_program_milestone(&third.id).unwrap().unwrap();
    assert_eq!(row.status, ProgramMilestoneStatus::Failed);
    assert_eq!(row.next_attempt_at.as_deref(), Some(future.as_str()));

    store.pause_program(&program.id, Some("pause")).unwrap();
    let (fourth, _) = store
        .create_program_milestone(&milestone_input(&program.id, "4", Vec::new()))
        .unwrap();
    assert!(
        store
            .claim_program_milestone_start(&fourth.id)
            .unwrap()
            .is_none(),
        "a paused program cannot claim new work"
    );
    store.resume_program(&program.id).unwrap();
    assert!(
        store
            .claim_program_milestone_start(&fourth.id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn program_usage_counts_each_linked_session_once() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("usage"))
        .unwrap();
    let (program, _) = store
        .create_program(&program_input(&project.id, ProgramStatus::Active))
        .unwrap();
    let session = store
        .create_session(&project.id, "program session")
        .unwrap();
    store.add_session_usage(&session.id, 120, 30).unwrap();
    store.add_session_cost_with_event(&session.id, 1.5).unwrap();
    let mut first = store
        .create_program_milestone(&milestone_input(&program.id, "1", Vec::new()))
        .unwrap()
        .0;
    let mut second = store
        .create_program_milestone(&milestone_input(&program.id, "2", Vec::new()))
        .unwrap()
        .0;
    first.session_id = Some(session.id.clone());
    second.session_id = Some(session.id.clone());
    store.insert_program_milestone_snapshot(&first).unwrap();
    store.insert_program_milestone_snapshot(&second).unwrap();
    let usage = store.program_usage(&program.id).unwrap();
    assert_eq!(usage.token_spent, 150);
    assert!((usage.cost_spent_usd - 1.5).abs() < 1e-9);
    assert_eq!(
        store.count_active_program_milestones(&program.id).unwrap(),
        0
    );
}

#[test]
fn program_usage_keeps_retried_attempts_in_the_budget() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("retry-usage"))
        .unwrap();
    let (program, _) = store
        .create_program(&program_input(&project.id, ProgramStatus::Active))
        .unwrap();
    let (milestone, _) = store
        .create_program_milestone(&milestone_input(&program.id, "1", Vec::new()))
        .unwrap();

    let (claimed, _) = store
        .claim_program_milestone_start(&milestone.id)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.attempts, 1);
    let first = store.create_session(&project.id, "attempt one").unwrap();
    store
        .attach_program_milestone_session(&milestone.id, &first.id, 1)
        .unwrap();
    store.add_session_usage(&first.id, 100, 50).unwrap();
    store.add_session_cost_with_event(&first.id, 0.75).unwrap();

    // A retry clears the current session pointer. The accounting ledger must
    // still remember the first attempt or every retry would reset the program
    // budget.
    store
        .fail_program_milestone(&milestone.id, "attempt failed", None)
        .unwrap();
    store.retry_program_milestone(&milestone.id).unwrap();
    let reset = store.get_program_milestone(&milestone.id).unwrap().unwrap();
    assert!(reset.session_id.is_none());
    assert_eq!(reset.attempts, 0);

    let (claimed, _) = store
        .claim_program_milestone_start(&milestone.id)
        .unwrap()
        .unwrap();
    assert_eq!(claimed.attempts, 1);
    let second = store.create_session(&project.id, "attempt two").unwrap();
    store
        .attach_program_milestone_session(&milestone.id, &second.id, 1)
        .unwrap();
    store.add_session_usage(&second.id, 200, 25).unwrap();
    store.add_session_cost_with_event(&second.id, 0.5).unwrap();

    let usage = store.program_usage(&program.id).unwrap();
    assert_eq!(usage.token_spent, 375);
    assert!((usage.cost_spent_usd - 1.25).abs() < 1e-9);
    let milestone_usage = store.program_milestone_usage(&milestone.id).unwrap();
    assert_eq!(milestone_usage.token_spent, 375);

    // Historical sessions still resolve to their program once the milestone
    // moved on to another session.
    let linked = store.list_programs_for_session(&first.id).unwrap();
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].id, program.id);

    // Rebuilding from events is idempotent and reproduces the same ledger.
    assert_eq!(store.rebuild_program_session_links(&program.id).unwrap(), 0);
    let rebuilt = store.program_usage(&program.id).unwrap();
    assert_eq!(rebuilt.token_spent, 375);
}

#[test]
fn project_manifest_round_trips_program_rows_policy_and_events() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("manifest"))
        .unwrap();
    let (program, _) = store
        .create_program(&program_input(&project.id, ProgramStatus::Active))
        .unwrap();
    store
        .create_program_milestone(&milestone_input(&program.id, "1.1", Vec::new()))
        .unwrap();
    store.record_project_manifest().unwrap();
    let snapshots = read_project_manifest(store.data_dir()).unwrap();
    assert_eq!(snapshots.len(), 1);
    let snapshot = &snapshots[0];
    assert_eq!(snapshot.programs.len(), 1);
    assert_eq!(snapshot.programs[0].id, program.id);
    assert!(snapshot.programs[0].policy.auto_dispatch);
    assert_eq!(snapshot.programs[0].policy.max_attempts, 3);
    assert_eq!(snapshot.program_milestones.len(), 1);
    assert_eq!(snapshot.program_milestones[0].key, "1.1");
    assert!(
        snapshot
            .program_events
            .iter()
            .any(|event| event.kind == "program.created")
    );
}

#[test]
fn execution_hosts_round_trip_and_enter_the_project_manifest() {
    use vp_protocol::remote::{ExecutionHost, ExecutionHostMode};

    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("remote"))
        .unwrap();
    assert_eq!(
        store.get_execution_host(&project.id).unwrap(),
        ExecutionHost::default()
    );
    let config = ExecutionHost {
        mode: ExecutionHostMode::Ssh,
        host: "build.example.com".into(),
        user: Some("agent".into()),
        root: "/srv/project".into(),
        token_env: Some("VERLINPARK_REMOTE_TOKEN".into()),
        ..ExecutionHost::default()
    };
    store.set_execution_host(&project.id, &config).unwrap();
    assert_eq!(store.get_execution_host(&project.id).unwrap(), config);
    assert!(
        store
            .execution_host_updated_at(&project.id)
            .unwrap()
            .is_some()
    );

    store.record_project_manifest().unwrap();
    let snapshots = crate::read_project_manifest(data.path()).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].execution_host, config);

    assert!(store.remove_execution_host(&project.id).unwrap());
    assert_eq!(
        store.get_execution_host(&project.id).unwrap(),
        ExecutionHost::default()
    );
}

#[test]
fn program_delete_tombstones_without_erasing_the_event_log() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("delete"))
        .unwrap();
    let (program, _) = store
        .create_program(&program_input(&project.id, ProgramStatus::Draft))
        .unwrap();
    store
        .create_program_milestone(&milestone_input(&program.id, "1", Vec::new()))
        .unwrap();
    assert!(store.get_program(&program.id).unwrap().is_some());
    assert!(store.delete_program(&program.id).unwrap());
    assert!(store.get_program(&program.id).unwrap().is_none());
    assert!(
        store
            .list_program_milestones(&program.id)
            .unwrap()
            .is_empty()
    );
    let events = store.list_program_events(&program.id, Some(100)).unwrap();
    assert!(events.iter().any(|event| event.kind == "program.deleted"));
    assert!(
        events
            .iter()
            .any(|event| event.kind == "program.milestone.created")
    );
}

#[test]
fn program_event_sequences_are_independent_per_program() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("sequences"))
        .unwrap();
    let (first, _) = store
        .create_program(&program_input(&project.id, ProgramStatus::Active))
        .unwrap();
    let (second, _) = store
        .create_program(&program_input(&project.id, ProgramStatus::Draft))
        .unwrap();
    store
        .create_program_milestone(&milestone_input(&first.id, "1", Vec::new()))
        .unwrap();
    store
        .create_program_milestone(&milestone_input(&second.id, "1", Vec::new()))
        .unwrap();
    store
        .create_program_milestone(&milestone_input(&first.id, "2", Vec::new()))
        .unwrap();
    let first_events = store.list_program_events(&first.id, Some(100)).unwrap();
    let second_events = store.list_program_events(&second.id, Some(100)).unwrap();
    assert_eq!(
        first_events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        second_events
            .iter()
            .map(|event| event.seq)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert!(
        first_events
            .iter()
            .all(|event| event.program_id == first.id)
    );
    assert!(
        second_events
            .iter()
            .all(|event| event.program_id == second.id)
    );
    let project_events = store
        .list_program_events_for_project(&project.id, None)
        .unwrap();
    assert_eq!(project_events.len(), 5);
    let appended = store
        .append_program_event(
            &project.id,
            &first.id,
            "program.updated",
            &serde_json::to_value(&first).unwrap(),
        )
        .unwrap();
    assert_eq!(appended.seq, 4);
    assert_eq!(
        store
            .list_program_events(&second.id, Some(100))
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn execution_host_defaults_are_independent_per_project() {
    use vp_protocol::remote::{ExecutionHost, ExecutionHostMode};

    let (_data, store, repo) = test_store();
    let first = store
        .add_project(repo.path().to_str().unwrap(), Some("one"))
        .unwrap();
    let other = TempDir::new().unwrap();
    let second = store
        .add_project(other.path().to_str().unwrap(), Some("two"))
        .unwrap();
    let config = ExecutionHost {
        mode: ExecutionHostMode::Ssh,
        host: "host".into(),
        root: "/srv".into(),
        token_env: Some("TOKEN".into()),
        ..ExecutionHost::default()
    };
    store.set_execution_host(&first.id, &config).unwrap();
    assert_eq!(store.get_execution_host(&first.id).unwrap(), config);
    assert_eq!(
        store.get_execution_host(&second.id).unwrap(),
        ExecutionHost::default()
    );
    assert!(!store.remove_execution_host(&second.id).unwrap());
    assert_eq!(store.list_execution_hosts().unwrap().len(), 2);
}

fn ready_continuation(store: &Store, repo: &TempDir, label: &str) -> (String, String) {
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some(label))
        .unwrap();
    let session = store.create_session(&project.id, label).unwrap();
    let (continuation, _) = store
        .create_session_continuation_with_event(&NewSessionContinuation {
            session_id: session.id.clone(),
            project_id: project.id.clone(),
            body: "continue the task".into(),
            watch_task_ids: Vec::new(),
            watch_mode: "all".into(),
            deadline_at: None,
            max_attempts: 3,
            series_id: None,
            parent_continuation_id: None,
            recurrence_every_secs: None,
            recurrence_max_runs: 1,
            run_count: 0,
        })
        .unwrap();
    store
        .mark_session_continuation_ready_with_event(&continuation.id, "deadline", &[])
        .unwrap()
        .unwrap();
    (session.id, continuation.id)
}

#[test]
fn continuation_claim_is_atomic_fenced_and_cleared_on_delivery() {
    let (_data, store, repo) = test_store();
    let (session_id, continuation_id) = ready_continuation(&store, &repo, "continuation claim");
    let claim = store
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "daemon-a:1:nonce-a",
            60,
        )
        .unwrap()
        .expect("first claim wins");
    assert_eq!(claim.owner_id, "daemon-a:1:nonce-a");
    assert_eq!(claim.delivery_attempt, 1);
    assert!(!claim.reclaimed);
    assert_eq!(
        claim.continuation.status, "delivering",
        "claim is a projection transition"
    );
    assert_eq!(
        claim.continuation.lease_owner.as_deref(),
        Some("daemon-a:1:nonce-a")
    );
    assert!(claim.continuation.lease_expires_at.is_some());

    // A live claim is not stealable by another process.
    assert!(
        store
            .claim_session_continuation_for_delivery_with_event(
                &continuation_id,
                "daemon-b:2:nonce-b",
                60,
            )
            .unwrap()
            .is_none()
    );
    // Even the same owner cannot double-claim while the current token is live.
    assert!(
        store
            .claim_session_continuation_for_delivery_with_event(
                &continuation_id,
                "daemon-a:1:nonce-a",
                60,
            )
            .unwrap()
            .is_none()
    );

    // Wrong owner with the right token is a stale ack, not a terminal write.
    let wrong_owner = store
        .mark_session_continuation_delivered_with_event(
            &continuation_id,
            "daemon-b:2:nonce-b",
            claim.token(),
        )
        .unwrap_err();
    assert!(matches!(
        wrong_owner,
        SessionContinuationAckError::Lost {
            reason: ContinuationAckLostReason::OwnerMismatch,
            ..
        }
    ));
    assert_eq!(
        store
            .list_session_continuations(&session_id, Some("delivering"), Some(10))
            .unwrap()
            .len(),
        1
    );

    let delivery = store
        .mark_session_continuation_delivered_with_event(
            &continuation_id,
            &claim.owner_id,
            claim.token(),
        )
        .unwrap()
        .expect("owner can acknowledge");
    assert_eq!(delivery.delivered.status, "delivered");
    assert_eq!(delivery.delivered.run_count, 1);
    assert!(delivery.delivered.lease_owner.is_none());
    assert!(delivery.delivered.lease_expires_at.is_none());
    assert!(
        delivery
            .events
            .iter()
            .any(|event| event.kind == "session.continuation.delivered")
    );

    // Repeated acknowledgement is stale and cannot increment run_count twice.
    let repeated = store
        .mark_session_continuation_delivered_with_event(
            &continuation_id,
            &claim.owner_id,
            claim.token(),
        )
        .unwrap_err();
    assert!(matches!(
        repeated,
        SessionContinuationAckError::Lost {
            reason: ContinuationAckLostReason::AlreadyTerminal,
            ..
        }
    ));
    let rows = store
        .list_session_continuations(&session_id, None, Some(10))
        .unwrap();
    assert_eq!(rows[0].run_count, 1);
}

#[test]
fn expired_continuation_claim_is_reclaimed_and_old_owner_cannot_ack() {
    let (_data, store, repo) = test_store();
    let (session_id, continuation_id) = ready_continuation(&store, &repo, "expired claim");
    let stale = store
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "daemon-a:1:nonce-a",
            0,
        )
        .unwrap()
        .expect("first claim wins");
    assert!(stale.expires_at.ends_with('Z'));

    let current = store
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "daemon-b:2:nonce-b",
            60,
        )
        .unwrap()
        .expect("expired claim is reclaimable");
    assert!(current.reclaimed);
    assert_eq!(current.delivery_attempt, 2);

    for result in [
        store
            .mark_session_continuation_delivered_with_event(
                &continuation_id,
                &stale.owner_id,
                stale.token(),
            )
            .map(|_| ()),
        store
            .retry_session_continuation_with_event(
                &continuation_id,
                &stale.owner_id,
                stale.token(),
                "stale retry",
                "2999-01-01T00:00:00Z",
            )
            .map(|_| ()),
        store
            .fail_session_continuation_with_event(
                &continuation_id,
                &stale.owner_id,
                stale.token(),
                "stale failure",
            )
            .map(|_| ()),
    ] {
        assert!(matches!(
            result,
            Err(SessionContinuationAckError::Lost {
                reason: ContinuationAckLostReason::OwnerMismatch,
                ..
            })
        ));
    }
    let rows = store
        .list_session_continuations(&session_id, None, Some(10))
        .unwrap();
    assert_eq!(rows[0].status, "delivering");
    assert_eq!(rows[0].delivery_attempt, 2);
    assert_eq!(rows[0].lease_owner.as_deref(), Some("daemon-b:2:nonce-b"));

    store
        .mark_session_continuation_delivered_with_event(
            &continuation_id,
            &current.owner_id,
            current.token(),
        )
        .unwrap()
        .unwrap();
    let rows = store
        .list_session_continuations(&session_id, None, Some(10))
        .unwrap();
    assert_eq!(rows[0].status, "delivered");
}

#[test]
fn concurrent_continuation_claim_has_exactly_one_winner() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let setup = Store::open(data.path()).unwrap();
    let (_session_id, continuation_id) = ready_continuation(&setup, &repo, "claim race");
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let mut handles = Vec::new();
    for index in 0..8 {
        let data = data.path().to_path_buf();
        let barrier = barrier.clone();
        let continuation_id = continuation_id.clone();
        handles.push(std::thread::spawn(move || {
            let store = Store::open(&data).unwrap();
            barrier.wait();
            store
                .claim_session_continuation_for_delivery_with_event(
                    &continuation_id,
                    &format!("daemon-{index}:{index}:nonce"),
                    60,
                )
                .unwrap()
                .is_some()
        }));
    }
    let winners = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .filter(|winner| *winner)
        .count();
    assert_eq!(winners, 1);
    let claim = setup
        .session_continuation_claim_status(&continuation_id)
        .unwrap()
        .unwrap();
    assert_eq!(claim.status, "delivering");
    assert!(claim.live);
    assert_eq!(claim.delivery_attempt, 1);
}

#[test]
fn continuation_retry_clears_claim_and_next_claim_gets_a_new_token() {
    let (_data, store, repo) = test_store();
    let (session_id, continuation_id) = ready_continuation(&store, &repo, "retry claim");
    let first = store
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "daemon-a:1:nonce-a",
            60,
        )
        .unwrap()
        .unwrap();
    let (retried, _) = store
        .retry_session_continuation_with_event(
            &continuation_id,
            &first.owner_id,
            first.token(),
            "provider unavailable",
            "2999-01-01T00:00:00Z",
        )
        .unwrap()
        .unwrap();
    assert_eq!(retried.status, "ready");
    assert_eq!(retried.attempts, 1);
    assert!(retried.lease_owner.is_none());
    assert!(retried.lease_expires_at.is_none());

    // The first token cannot acknowledge after the retry cleared it.
    assert!(matches!(
        store
            .mark_session_continuation_delivered_with_event(
                &continuation_id,
                &first.owner_id,
                first.token(),
            )
            .unwrap_err(),
        SessionContinuationAckError::Lost { .. }
    ));
    let second = store
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "daemon-b:2:nonce-b",
            60,
        )
        .unwrap()
        .unwrap();
    assert_eq!(second.delivery_attempt, 2);
    assert_eq!(
        store
            .list_session_continuations(&session_id, Some("delivering"), Some(10))
            .unwrap()[0]
            .lease_owner
            .as_deref(),
        Some("daemon-b:2:nonce-b")
    );
}

#[test]
fn continuation_failure_is_fenced_and_clears_the_claim() {
    let (_data, store, repo) = test_store();
    let (session_id, continuation_id) = ready_continuation(&store, &repo, "failure claim");
    let claim = store
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "daemon-a:1:nonce-a",
            60,
        )
        .unwrap()
        .unwrap();
    let (failed, _) = store
        .fail_session_continuation_with_event(
            &continuation_id,
            &claim.owner_id,
            claim.token(),
            "start failed three times",
        )
        .unwrap()
        .unwrap();
    assert_eq!(failed.status, "failed");
    assert_eq!(failed.attempts, 1);
    assert!(failed.lease_owner.is_none());
    assert_eq!(
        failed.last_error.as_deref(),
        Some("start failed three times")
    );
    // A second stale failure does not change the row.
    assert!(matches!(
        store
            .fail_session_continuation_with_event(
                &continuation_id,
                &claim.owner_id,
                claim.token(),
                "again",
            )
            .unwrap_err(),
        SessionContinuationAckError::Lost {
            reason: ContinuationAckLostReason::AlreadyTerminal,
            ..
        }
    ));
    assert_eq!(
        store
            .list_session_continuations(&session_id, None, Some(10))
            .unwrap()[0]
            .status,
        "failed"
    );
}

#[test]
fn reclaim_expired_continuation_emits_ready_event_and_clears_owner() {
    let (_data, store, repo) = test_store();
    let (session_id, continuation_id) = ready_continuation(&store, &repo, "reclaim event");
    store
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "daemon-a:1:nonce-a",
            0,
        )
        .unwrap()
        .unwrap();
    let later = time::OffsetDateTime::now_utc() + time::Duration::seconds(5);
    let (reclaimed, event) = store
        .reclaim_expired_session_continuation_at(
            &continuation_id,
            "claim expired before ack",
            later,
        )
        .unwrap()
        .unwrap();
    assert_eq!(reclaimed.status, "ready");
    assert!(reclaimed.lease_owner.is_none());
    assert!(reclaimed.lease_expires_at.is_none());
    assert_eq!(
        reclaimed.last_error.as_deref(),
        Some("claim expired before ack")
    );
    assert_eq!(event.kind, "session.continuation.reclaimed");
    assert_eq!(event.payload["reason"], "claim expired before ack");
    // Reclaim is a compare-and-set: a second call sees no expired claim.
    assert!(
        store
            .reclaim_expired_session_continuation_at(&continuation_id, "again", later)
            .unwrap()
            .is_none()
    );
    let events = store.recent_events(&session_id, 20).unwrap();
    assert!(
        events
            .iter()
            .any(|event| event.kind == "session.continuation.reclaimed")
    );
}

#[test]
fn live_continuation_claim_survives_unclaimed_failure_and_recovery() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let (session_id, continuation_id) = ready_continuation(&store, &repo, "live recovery");
    let claim = store
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "foreign-daemon:99:nonce",
            60,
        )
        .unwrap()
        .unwrap();
    assert!(
        store
            .fail_unclaimed_session_continuation_with_event(&continuation_id, "operator fail")
            .unwrap()
            .is_none()
    );
    let (_sessions, events) = store.recover_state().unwrap();
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.continuation.failed"
                || event.kind == "session.continuation.reclaimed")
    );
    let rows = store
        .list_session_continuations(&session_id, None, Some(10))
        .unwrap();
    assert_eq!(rows[0].status, "delivering");
    assert_eq!(
        rows[0].lease_owner.as_deref(),
        Some(claim.owner_id.as_str())
    );

    // The same live claim is untouched by a second Store opening the dir.
    let second = Store::open(data.path()).unwrap();
    assert!(
        second
            .claim_session_continuation_for_delivery_with_event(
                &continuation_id,
                "second-daemon:100:nonce",
                60,
            )
            .unwrap()
            .is_none()
    );
    assert_eq!(
        second
            .list_session_continuations(&session_id, None, Some(10))
            .unwrap()[0]
            .status,
        "delivering"
    );
}

#[test]
fn recovery_reclaims_only_expired_continuation_claims() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let (session_id, continuation_id) = ready_continuation(&store, &repo, "expired recovery");
    let expired = store
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "foreign-daemon:99:nonce",
            0,
        )
        .unwrap()
        .unwrap();
    let (_sessions, events) = store.recover_state().unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "session.continuation.reclaimed"
            && event.session_id == session_id
            && event.payload["reason"] == "delivery claim expired before acknowledgement"
    }));
    let rows = store
        .list_session_continuations(&session_id, None, Some(10))
        .unwrap();
    assert_eq!(rows[0].status, "ready");
    assert!(rows[0].lease_owner.is_none());
    assert!(rows[0].lease_expires_at.is_none());
    assert!(
        store
            .mark_session_continuation_delivered_with_event(
                &continuation_id,
                &expired.owner_id,
                expired.token(),
            )
            .is_err()
    );
}

#[test]
fn continuation_claim_status_never_exposes_the_token() {
    let (_data, store, repo) = test_store();
    let (session_id, continuation_id) = ready_continuation(&store, &repo, "claim status");
    let claim = store
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "daemon-a:1:nonce-a",
            60,
        )
        .unwrap()
        .unwrap();
    let status = store
        .session_continuation_claim_status(&continuation_id)
        .unwrap()
        .unwrap();
    assert_eq!(status.owner_id.as_deref(), Some(claim.owner_id.as_str()));
    assert!(status.live);
    assert_eq!(status.delivery_attempt, 1);
    let value = serde_json::to_value(&status).unwrap();
    assert!(value.get("token").is_none());
    assert!(!value.to_string().contains("lease_token"));
    let public = serde_json::to_value(
        &store
            .list_session_continuations(&session_id, None, Some(10))
            .unwrap()[0],
    )
    .unwrap();
    assert!(public.get("lease_token").is_none());
}

#[test]
fn continuation_legacy_delivering_rows_still_fail_closed_on_recovery() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let (session_id, continuation_id) = ready_continuation(&store, &repo, "legacy crash");
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE session_continuations
             SET status = 'delivering', lease_owner = NULL, lease_token = NULL,
                 lease_expires_at = NULL
             WHERE id = ?1",
            params![continuation_id],
        )
        .unwrap();
    }
    let (_sessions, events) = store.recover_state().unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "session.continuation.failed"
            && event.payload["reason"]
                == "daemon restarted while delivering; not retried automatically"
    }));
    let rows = store
        .list_session_continuations(&session_id, None, Some(10))
        .unwrap();
    assert_eq!(rows[0].status, "failed");
    assert_eq!(rows[0].run_count, 0);
}

#[test]
fn runtime_lease_validation_rejects_unsafe_keys_and_caps_ttl() {
    let (_data, store, _repo) = test_store();
    for (key, owner, instance, kind) in [
        ("", "owner", "instance:1", "session"),
        ("bad\nkey", "owner", "instance:1", "session"),
        ("session:s1", "", "instance:1", "session"),
        ("session:s1", "owner", "", "session"),
        ("session:s1", "owner", "instance:1", ""),
    ] {
        let error = store
            .acquire_runtime_lease(key, owner, instance, kind, None, 60)
            .unwrap_err();
        assert!(
            matches!(error, RuntimeLeaseError::Internal(_)),
            "{key} {owner} {instance} {kind}: {error}"
        );
    }
    let long_key = format!("session:{}", "x".repeat(300));
    assert!(
        store
            .acquire_runtime_lease(&long_key, "owner", "instance:1", "session", None, 60)
            .is_err()
    );
    let lease = store
        .acquire_runtime_lease(
            "session:ttl",
            "owner",
            "instance:1",
            "session",
            Some("ttl"),
            u64::MAX,
        )
        .unwrap();
    let acquired = time::OffsetDateTime::parse(
        &lease.lease.acquired_at,
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let expires = time::OffsetDateTime::parse(
        &lease.lease.expires_at,
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    assert_eq!(expires - acquired, time::Duration::seconds(86_400));
}

#[test]
fn runtime_lease_list_hides_released_tombstones_but_audit_sees_them() {
    let (_data, store, _repo) = test_store();
    let first = store
        .acquire_runtime_lease(
            "integration:s1",
            "daemon-a",
            "instance-a:1",
            "integration",
            Some("s1"),
            60,
        )
        .unwrap();
    store
        .release_runtime_lease("integration:s1", "daemon-a", first.token())
        .unwrap();
    assert!(store.list_runtime_leases().unwrap().is_empty());
    let audit = store.list_runtime_leases_including_released().unwrap();
    assert_eq!(audit.len(), 1);
    assert!(audit[0].released_at.is_some());
    assert_eq!(audit[0].state(), RuntimeLeaseState::Released);
    assert_eq!(audit[0].fencing_token, 1);
    store.clear_expired_runtime_leases().unwrap();
    assert!(
        store
            .list_runtime_leases_including_released()
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.runtime_lease_fencing_token("integration:s1").unwrap(),
        Some(1)
    );
}

#[test]
fn release_after_expiry_is_safe_and_never_deletes_a_newer_generation() {
    let (_data, store, _repo) = test_store();
    let expired = store
        .acquire_runtime_lease(
            "session:s9",
            "daemon-a",
            "instance-a:1",
            "session",
            Some("s9"),
            0,
        )
        .unwrap();
    assert!(
        store
            .release_runtime_lease("session:s9", "daemon-a", expired.token())
            .unwrap()
    );
    assert!(store.runtime_lease_owner("session:s9").unwrap().is_none());
    let current = store
        .acquire_runtime_lease(
            "session:s9",
            "daemon-b",
            "instance-b:2",
            "session",
            Some("s9"),
            60,
        )
        .unwrap();
    assert_eq!(current.fencing_token(), 2);
    assert!(
        store
            .release_runtime_lease("session:s9", "daemon-a", expired.token())
            .is_err()
    );
    assert_eq!(
        store
            .runtime_lease_owner("session:s9")
            .unwrap()
            .unwrap()
            .owner_id,
        "daemon-b"
    );
}

#[test]
fn recovery_clears_own_runtime_leases_but_preserves_live_foreign_rows() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("recovery leases"))
        .unwrap();
    let session = store
        .create_session(&project.id, "recovery leases")
        .unwrap();
    let own_instance = store.process_owner_instance();
    store
        .acquire_runtime_lease(
            &format!("session:{}", session.id),
            "own-owner",
            &own_instance,
            "session",
            Some(&session.id),
            60,
        )
        .unwrap();
    store
        .acquire_runtime_lease(
            &format!("integration:{}", session.id),
            "foreign-owner",
            "foreign-instance:999",
            "integration",
            Some(&session.id),
            60,
        )
        .unwrap();
    store.recover_state().unwrap();
    assert!(
        store
            .runtime_lease_owner(&format!("session:{}", session.id))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .runtime_lease_owner(&format!("integration:{}", session.id))
            .unwrap()
            .is_some()
    );
    let audit = store
        .list_runtime_lease_audit(Some(&format!("session:{}", session.id)), Some(10))
        .unwrap();
    assert!(audit.iter().any(|entry| entry.action == "startup_cleanup"));
}

#[test]
fn migrates_legacy_runtime_lease_rows_and_initializes_fencing() {
    let data = TempDir::new().unwrap();
    {
        let conn = rusqlite::Connection::open(data.path().join("verlinpark.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE store_meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE runtime_leases (
                 lease_key     TEXT PRIMARY KEY,
                 owner_id      TEXT NOT NULL,
                 kind          TEXT NOT NULL,
                 session_id    TEXT,
                 acquired_at   TEXT NOT NULL,
                 heartbeat_at  TEXT NOT NULL,
                 expires_at    TEXT NOT NULL
             );
             INSERT INTO runtime_leases
                 (lease_key, owner_id, kind, session_id, acquired_at, heartbeat_at, expires_at)
             VALUES ('session:legacy', 'old-daemon', 'session', 'legacy',
                     '2020-01-01T00:00:00Z', '2020-01-01T00:00:00Z',
                     '2020-01-01T00:00:00Z');",
        )
        .unwrap();
    }
    let store = Store::open(data.path()).unwrap();
    let legacy = store
        .runtime_lease_owner("session:legacy")
        .unwrap()
        .unwrap();
    assert_eq!(legacy.owner_instance, "");
    assert_eq!(legacy.fencing_token, 0);
    assert_eq!(legacy.state(), RuntimeLeaseState::Stale);
    let replacement = store
        .acquire_runtime_lease(
            "session:legacy",
            "new-daemon",
            "new-instance:1",
            "session",
            Some("legacy"),
            60,
        )
        .unwrap();
    assert_eq!(replacement.fencing_token(), 1);
    assert_eq!(store.daemon_identity().pid, std::process::id());
}

#[test]
fn stale_continuation_claim_can_be_reclaimed_only_once_under_contention() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let setup = Store::open(data.path()).unwrap();
    let (_session_id, continuation_id) = ready_continuation(&setup, &repo, "reclaim race");
    setup
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "stale-daemon:1:nonce",
            0,
        )
        .unwrap()
        .unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
    let mut handles = Vec::new();
    for index in 0..6 {
        let data = data.path().to_path_buf();
        let barrier = barrier.clone();
        let continuation_id = continuation_id.clone();
        handles.push(std::thread::spawn(move || {
            let store = Store::open(&data).unwrap();
            barrier.wait();
            store
                .claim_session_continuation_for_delivery_with_event(
                    &continuation_id,
                    &format!("reclaimer-{index}:{index}:nonce"),
                    60,
                )
                .unwrap()
                .is_some()
        }));
    }
    let winners = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .filter(|winner| *winner)
        .count();
    assert_eq!(winners, 1, "only one process may reclaim an expired claim");
}

#[test]
fn recovery_preserves_approvals_questions_and_steering_for_live_foreign_sessions() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("foreign pending"))
        .unwrap();
    let foreign = store.create_session(&project.id, "foreign live").unwrap();
    let own = store.create_session(&project.id, "own dead").unwrap();
    store
        .acquire_runtime_lease(
            &format!("session:{}", foreign.id),
            "foreign-owner",
            "foreign-instance:999",
            "session",
            Some(&foreign.id),
            60,
        )
        .unwrap();
    let foreign_approval = store
        .create_approval(
            &foreign.id,
            "call-foreign",
            "write_file",
            &serde_json::json!({"path": "README.md"}),
            Some("workspace"),
            Some("write"),
        )
        .unwrap();
    let own_approval = store
        .create_approval(
            &own.id,
            "call-own",
            "write_file",
            &serde_json::json!({"path": "README.md"}),
            Some("workspace"),
            Some("write"),
        )
        .unwrap();
    let foreign_question = store
        .create_question_with_event(
            &foreign.id,
            "agent-foreign",
            "Foreign",
            "continue?",
            &["yes".into(), "no".into()],
        )
        .unwrap()
        .0;
    let own_question = store
        .create_question_with_event(&own.id, "agent-own", "Own", "continue?", &[])
        .unwrap()
        .0;
    let foreign_steering = store
        .create_steering_with_event(&foreign.id, "agent-foreign", None, "keep going")
        .unwrap()
        .0;
    let own_steering = store
        .create_steering_with_event(&own.id, "agent-own", None, "stop")
        .unwrap()
        .0;

    store.recover_state().unwrap();

    let approvals = store.list_approvals(Some(foreign.id.as_str())).unwrap();
    assert_eq!(
        approvals
            .iter()
            .find(|approval| approval.id == foreign_approval.id)
            .unwrap()
            .status,
        "pending"
    );
    let own_approvals = store.list_approvals(Some(own.id.as_str())).unwrap();
    assert_eq!(
        own_approvals
            .iter()
            .find(|approval| approval.id == own_approval.id)
            .unwrap()
            .status,
        "denied"
    );
    let questions = store.list_questions(Some(foreign.id.as_str())).unwrap();
    assert_eq!(
        questions
            .iter()
            .find(|question| question.id == foreign_question.id)
            .unwrap()
            .status,
        "pending"
    );
    let own_questions = store.list_questions(Some(own.id.as_str())).unwrap();
    assert_eq!(
        own_questions
            .iter()
            .find(|question| question.id == own_question.id)
            .unwrap()
            .status,
        "expired"
    );
    let steering = store.list_steering(&foreign.id, None, None).unwrap();
    assert_eq!(
        steering
            .iter()
            .find(|message| message.id == foreign_steering.id)
            .unwrap()
            .status,
        "pending"
    );
    let own_steering_rows = store.list_steering(&own.id, None, None).unwrap();
    assert_eq!(
        own_steering_rows
            .iter()
            .find(|message| message.id == own_steering.id)
            .unwrap()
            .status,
        "cancelled"
    );
}

#[test]
fn mixed_acquire_release_stress_keeps_ownership_exclusive() {
    let data = TempDir::new().unwrap();
    Store::open(data.path()).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
    let generations = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let mut handles = Vec::new();
    for index in 0..6 {
        let data = data.path().to_path_buf();
        let barrier = barrier.clone();
        let generations = generations.clone();
        handles.push(std::thread::spawn(move || {
            let store = Store::open(&data).unwrap();
            barrier.wait();
            let mut wins = 0;
            for _ in 0..12 {
                match store.acquire_runtime_lease(
                    "session:stress",
                    &format!("owner-{index}"),
                    &format!("instance-{index}:{index}"),
                    "session",
                    Some("stress"),
                    30,
                ) {
                    Ok(acquisition) => {
                        wins += 1;
                        generations
                            .lock()
                            .unwrap()
                            .push(acquisition.fencing_token());
                        // The winner may immediately assert its own token.
                        store
                            .assert_runtime_lease(
                                "session:stress",
                                &format!("owner-{index}"),
                                acquisition.token(),
                            )
                            .unwrap();
                        let _ = store.release_runtime_lease(
                            "session:stress",
                            &format!("owner-{index}"),
                            acquisition.token(),
                        );
                    }
                    Err(RuntimeLeaseError::Busy { .. }) => {
                        std::thread::yield_now();
                    }
                    Err(error) => panic!("unexpected lease error: {error}"),
                }
            }
            wins
        }));
    }
    let total_wins: usize = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .sum();
    assert!(total_wins > 0);
    let mut generations = generations.lock().unwrap().clone();
    let original_len = generations.len();
    generations.sort_unstable();
    generations.dedup();
    assert_eq!(
        generations.len(),
        original_len,
        "generations must never repeat"
    );
    let store = Store::open(data.path()).unwrap();
    assert!(store.list_runtime_leases().unwrap().len() <= 1);
}

#[test]
fn concurrent_expiry_sweep_and_reacquisition_never_reuses_a_generation() {
    let data = TempDir::new().unwrap();
    Store::open(data.path()).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let mut handles = Vec::new();
    for index in 0..8 {
        let data = data.path().to_path_buf();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            let store = Store::open(&data).unwrap();
            barrier.wait();
            let mut observed = Vec::new();
            for round in 0..8 {
                let ttl = if round % 2 == 0 { 0 } else { 30 };
                match store.acquire_runtime_lease(
                    "lock:expiry-race",
                    &format!("owner-{index}"),
                    &format!("instance-{index}:{index}"),
                    "lock",
                    None,
                    ttl,
                ) {
                    Ok(acquisition) => observed.push(acquisition.fencing_token()),
                    Err(RuntimeLeaseError::Busy { .. }) => {}
                    Err(error) => panic!("unexpected lease error: {error}"),
                }
                let _ = store.clear_expired_runtime_leases();
            }
            observed
        }));
    }
    let mut observed = handles
        .into_iter()
        .flat_map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert!(!observed.is_empty());
    let original_len = observed.len();
    observed.sort_unstable();
    observed.dedup();
    assert_eq!(observed.len(), original_len);
}

#[test]
fn reclaim_and_delivery_race_has_one_terminal_writer() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let setup = Store::open(data.path()).unwrap();
    let (session_id, continuation_id) = ready_continuation(&setup, &repo, "reclaim delivery race");
    setup
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "stale-owner:1:nonce",
            0,
        )
        .unwrap()
        .unwrap();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
    let mut handles = Vec::new();
    for index in 0..6 {
        let data = data.path().to_path_buf();
        let barrier = barrier.clone();
        let continuation_id = continuation_id.clone();
        handles.push(std::thread::spawn(move || {
            let store = Store::open(&data).unwrap();
            barrier.wait();
            store
                .claim_session_continuation_for_delivery_with_event(
                    &continuation_id,
                    &format!("claimer-{index}:{index}:nonce"),
                    0,
                )
                .unwrap()
        }));
    }
    let stale_claims = handles
        .into_iter()
        .filter_map(|handle| handle.join().unwrap())
        .collect::<Vec<_>>();
    assert!(!stale_claims.is_empty());

    // A final live claim wins ownership; every earlier generation is stale.
    let final_claim = setup
        .claim_session_continuation_for_delivery_with_event(
            &continuation_id,
            "final-owner:99:nonce",
            60,
        )
        .unwrap()
        .expect("expired generations are reclaimable");
    for stale in &stale_claims {
        assert!(
            setup
                .mark_session_continuation_delivered_with_event(
                    &continuation_id,
                    &stale.owner_id,
                    stale.token(),
                )
                .is_err(),
            "an earlier generation must not write terminal state"
        );
    }
    let delivery = setup
        .mark_session_continuation_delivered_with_event(
            &continuation_id,
            &final_claim.owner_id,
            final_claim.token(),
        )
        .unwrap()
        .expect("current generation acknowledges");
    assert_eq!(delivery.delivered.status, "delivered");
    assert_eq!(
        delivery.delivered.delivery_attempt,
        stale_claims.len() as u32 + 2,
        "stale claim + every contention winner + final live claim"
    );
    let events = setup.recent_events(&session_id, 200).unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivered")
            .count(),
        1
    );
}

#[test]
fn lease_owner_and_kind_validation_never_partially_writes_a_row() {
    let (_data, store, _repo) = test_store();
    let too_long_owner = "o".repeat(400);
    assert!(
        store
            .acquire_runtime_lease(
                "session:validation",
                &too_long_owner,
                "instance:1",
                "session",
                None,
                60,
            )
            .is_err()
    );
    assert!(
        store
            .runtime_lease_owner("session:validation")
            .unwrap()
            .is_none()
    );
    let too_long_instance = "i".repeat(200);
    assert!(
        store
            .acquire_runtime_lease(
                "session:validation",
                "owner",
                &too_long_instance,
                "session",
                None,
                60,
            )
            .is_err()
    );
    assert!(
        store
            .runtime_lease_fencing_token("session:validation")
            .unwrap()
            .is_none(),
        "validation failures must not consume a generation"
    );
}

#[test]
fn recovery_protects_plan_tasks_merge_queue_and_intake_for_live_foreign_owners() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("protected recovery"))
        .unwrap();
    let foreign = store.create_session(&project.id, "foreign plan").unwrap();
    let own = store.create_session(&project.id, "own plan").unwrap();
    store
        .acquire_runtime_lease(
            &format!("session:{}", foreign.id),
            "foreign-owner",
            "foreign-instance:999",
            "session",
            Some(&foreign.id),
            60,
        )
        .unwrap();
    store
        .acquire_runtime_lease(
            &format!("integration:{}", foreign.id),
            "foreign-integrator",
            "foreign-instance:999",
            "integration",
            Some(&foreign.id),
            60,
        )
        .unwrap();
    {
        let conn = store.lock().unwrap();
        for (session, status, task) in [
            (&foreign.id, "running", "T-foreign"),
            (&own.id, "running", "T-own"),
        ] {
            conn.execute(
                "INSERT INTO plan_tasks
                 (session_id, plan_seq, task_id, project_id, department, title, priority,
                  status, deps, write_scope, attempts, enqueued_at, started_at, updated_at)
                 VALUES (?1, 1, ?2, ?3, 'impl', 'task', 0, ?4, '[]', '[]', 1,
                         '2026-11-11T00:00:00Z', '2026-11-11T00:00:00Z',
                         '2026-11-11T00:00:00Z')",
                params![session, task, project.id, status],
            )
            .unwrap();
        }
    }
    let foreign_entry = store
        .enqueue_merge_with_event(
            &foreign.id,
            "worker-foreign",
            "Worker foreign",
            Some("T-foreign"),
            "verlinpark/foreign",
            "1111111111111111111111111111111111111111",
            "2222222222222222222222222222222222222222",
            Some("verified"),
        )
        .unwrap()
        .0;
    let own_entry = store
        .enqueue_merge_with_event(
            &own.id,
            "worker-own",
            "Worker own",
            Some("T-own"),
            "verlinpark/own",
            "3333333333333333333333333333333333333333",
            "4444444444444444444444444444444444444444",
            Some("verified"),
        )
        .unwrap()
        .0;
    let foreign_intake = store
        .create_intake_task(&NewIntakeTask {
            project_id: project.id.clone(),
            title: "foreign intake".into(),
            instructions: "do work".into(),
            profile: "coding".into(),
            kind: "feature".into(),
            priority: 0,
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: 1,
            origin: "user".into(),
            labels: Vec::new(),
        })
        .unwrap();
    let own_intake = store
        .create_intake_task(&NewIntakeTask {
            project_id: project.id.clone(),
            title: "own intake".into(),
            instructions: "do work".into(),
            profile: "coding".into(),
            kind: "feature".into(),
            priority: 0,
            write_scope: Vec::new(),
            acceptance: Vec::new(),
            deps: Vec::new(),
            token_budget: None,
            cost_budget_usd: None,
            time_budget_secs: None,
            max_attempts: 1,
            origin: "user".into(),
            labels: Vec::new(),
        })
        .unwrap();
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE intake_tasks SET status = 'running', session_id = ?1 WHERE id = ?2",
            params![foreign.id, foreign_intake.id],
        )
        .unwrap();
        conn.execute(
            "UPDATE intake_tasks SET status = 'running', session_id = ?1 WHERE id = ?2",
            params![own.id, own_intake.id],
        )
        .unwrap();
    }

    store.recover_state().unwrap();

    let foreign_tasks = store.list_plan_tasks(None, Some(&foreign.id)).unwrap();
    assert_eq!(foreign_tasks[0].status, "running");
    let own_tasks = store.list_plan_tasks(None, Some(&own.id)).unwrap();
    assert_eq!(own_tasks[0].status, "interrupted");
    let queue = store.list_merge_queue(&foreign.id).unwrap();
    let foreign_entry = queue
        .iter()
        .find(|entry| entry.id == foreign_entry.id)
        .unwrap();
    assert_eq!(foreign_entry.status, "queued");
    let own_queue = store.list_merge_queue(&own.id).unwrap();
    let own_entry = own_queue
        .iter()
        .find(|entry| entry.id == own_entry.id)
        .unwrap();
    assert_eq!(own_entry.status, "interrupted");
    let intake = store.list_intake_tasks(None, None, None).unwrap();
    assert_eq!(
        intake
            .iter()
            .find(|task| task.id == foreign_intake.id)
            .unwrap()
            .status,
        "running"
    );
    assert_eq!(
        intake
            .iter()
            .find(|task| task.id == own_intake.id)
            .unwrap()
            .status,
        "failed"
    );
}

#[test]
fn runtime_lease_audit_records_token_free_ownership_transitions() {
    let (_data, store, _repo) = test_store();
    let first = store
        .acquire_runtime_lease(
            "integration:audit",
            "owner-a",
            "instance-a:1",
            "integration",
            Some("audit"),
            60,
        )
        .unwrap();
    let second = store
        .acquire_runtime_lease(
            "integration:audit",
            "owner-a",
            "instance-a:1",
            "integration",
            Some("audit"),
            60,
        )
        .unwrap();
    assert_eq!(second.fencing_token(), 2);
    store
        .release_runtime_lease("integration:audit", "owner-a", second.token())
        .unwrap();
    let third = store
        .acquire_runtime_lease(
            "integration:audit",
            "owner-b",
            "instance-b:2",
            "integration",
            Some("audit"),
            0,
        )
        .unwrap();
    store.clear_expired_runtime_leases().unwrap();

    let entries = store.list_runtime_lease_audit(None, Some(20)).unwrap();
    let actions = entries
        .iter()
        .map(|entry| entry.action.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        actions,
        vec![
            "expired",
            "reacquired",
            "released",
            "reacquired",
            "acquired"
        ]
    );
    assert_eq!(entries[0].owner_id, "owner-b");
    assert_eq!(entries[0].fencing_token, third.fencing_token());
    assert!(
        entries
            .iter()
            .all(|entry| entry.detail.as_deref() != Some(first.token().as_str())),
        "audit detail must not contain an acquisition token"
    );
    for entry in &entries {
        let value = serde_json::to_value(entry).unwrap();
        assert!(value.get("lease_token").is_none());
        assert!(value.get("token").is_none());
    }

    let filtered = store
        .list_runtime_lease_audit(Some("integration:audit"), Some(2))
        .unwrap();
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].action, "expired");
    assert!(
        store
            .list_runtime_lease_audit(Some("missing:key"), None)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn runtime_lease_audit_is_absent_from_live_lease_projection() {
    let (_data, store, _repo) = test_store();
    store
        .acquire_runtime_lease(
            "session:projection",
            "owner-a",
            "instance-a:1",
            "session",
            Some("projection"),
            60,
        )
        .unwrap();
    let projection = serde_json::to_value(store.list_runtime_leases().unwrap()).unwrap();
    assert!(projection.to_string().contains("fencing_token"));
    assert!(!projection.to_string().contains("audit"));
    let audit = store.list_runtime_lease_audit(None, None).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "acquired");
}

#[test]
fn lease_audit_prune_removes_only_old_rows() {
    let (_data, store, _repo) = test_store();
    store
        .acquire_runtime_lease(
            "session:audit-prune",
            "owner-a",
            "instance-a:1",
            "session",
            Some("audit-prune"),
            60,
        )
        .unwrap();
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE runtime_lease_audit SET occurred_at = '2020-01-01T00:00:00Z'",
            [],
        )
        .unwrap();
    }
    assert_eq!(store.list_runtime_lease_audit(None, None).unwrap().len(), 1);
    let cutoff = time::OffsetDateTime::now_utc() - time::Duration::days(30);
    assert_eq!(store.prune_runtime_lease_audit(cutoff).unwrap(), 1);
    assert!(
        store
            .list_runtime_lease_audit(None, None)
            .unwrap()
            .is_empty()
    );
    // The live lease is operational state, not history.
    assert!(
        store
            .runtime_lease_owner("session:audit-prune")
            .unwrap()
            .is_some()
    );
}

#[test]
fn deterministic_lease_state_machine_matches_the_model() {
    struct Holder {
        owner: String,
        token: RuntimeLeaseToken,
        generation: u64,
        expires: time::OffsetDateTime,
    }
    fn next_random(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        *state
    }

    let (_data, store, _repo) = test_store();
    let base = time::OffsetDateTime::parse(
        "2026-11-11T00:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let mut now = base;
    let mut random = 0x5eed_u64;
    let mut holder: Option<Holder> = None;
    let mut tombstone = false;
    let mut stale_tokens: Vec<RuntimeLeaseToken> = Vec::new();
    let mut last_generation = 0u64;
    let owners = ["owner-a", "owner-b", "owner-c"];
    let instances = ["instance-a:1", "instance-b:2", "instance-c:3"];

    for step in 0..300 {
        now += time::Duration::seconds((next_random(&mut random) % 12) as i64);
        let actor = (next_random(&mut random) % 3) as usize;
        let operation = next_random(&mut random) % 5;
        match operation {
            0 => {
                let ttl = next_random(&mut random) % 25;
                let result = store.acquire_runtime_lease_at(
                    "session:model",
                    owners[actor],
                    instances[actor],
                    "session",
                    Some("model"),
                    ttl,
                    now,
                );
                let live_other = holder
                    .as_ref()
                    .is_some_and(|current| current.expires > now && current.owner != owners[actor]);
                if live_other {
                    assert!(
                        matches!(result, Err(RuntimeLeaseError::Busy { .. })),
                        "step {step}: live owner should reject {actor}, got {result:?}"
                    );
                } else {
                    let acquisition = result.unwrap_or_else(|error| {
                        panic!("step {step}: actor {actor} should acquire: {error}")
                    });
                    assert_eq!(
                        acquisition.fencing_token(),
                        last_generation + 1,
                        "step {step}: generation must advance exactly once"
                    );
                    last_generation = acquisition.fencing_token();
                    if let Some(previous) = holder.take() {
                        stale_tokens.push(previous.token);
                    }
                    tombstone = false;
                    holder = Some(Holder {
                        owner: owners[actor].to_string(),
                        token: acquisition.token().clone(),
                        generation: acquisition.fencing_token(),
                        expires: now + time::Duration::seconds(ttl.min(86_400) as i64),
                    });
                }
            }
            1 => {
                let Some(current) = holder.as_ref() else {
                    continue;
                };
                let stale = if next_random(&mut random).is_multiple_of(3) {
                    stale_tokens.last().cloned()
                } else {
                    Some(current.token.clone())
                };
                let Some(token) = stale else {
                    continue;
                };
                let result =
                    store.renew_runtime_lease_at("session:model", &current.owner, &token, 30, now);
                let current_holder = holder.as_ref().unwrap();
                let owns_current =
                    token.as_str() == current_holder.token.as_str() && current_holder.expires > now;
                if owns_current {
                    assert!(result.is_ok(), "step {step}: live renewal should succeed");
                    holder.as_mut().unwrap().expires = now + time::Duration::seconds(30);
                } else {
                    assert!(
                        result.is_err(),
                        "step {step}: stale renewal must fail for actor {actor}"
                    );
                }
            }
            2 => {
                let Some(current) = holder.as_ref() else {
                    continue;
                };
                let stale = if next_random(&mut random).is_multiple_of(3) {
                    stale_tokens.last().cloned()
                } else {
                    Some(current.token.clone())
                };
                let Some(token) = stale else {
                    continue;
                };
                let result =
                    store.release_runtime_lease_at("session:model", &current.owner, &token, now);
                let current_holder = holder.as_ref().unwrap();
                let owns_current = token.as_str() == current_holder.token.as_str();
                if owns_current {
                    assert!(result.unwrap_or(false), "step {step}: release should apply");
                    holder = None;
                    tombstone = true;
                    stale_tokens.push(token);
                } else {
                    assert!(result.is_err(), "step {step}: stale release must fail");
                }
            }
            3 => {
                let owns_current = holder
                    .as_ref()
                    .is_some_and(|current| current.owner == owners[actor] && current.expires > now);
                let token = if owns_current {
                    holder.as_ref().unwrap().token.clone()
                } else {
                    stale_tokens
                        .last()
                        .cloned()
                        .unwrap_or_else(RuntimeLeaseToken::generate)
                };
                let result =
                    store.assert_runtime_lease_at("session:model", owners[actor], &token, now);
                assert_eq!(
                    result.is_ok(),
                    owns_current,
                    "step {step}: assert ownership mismatch"
                );
            }
            _ => {
                let removed = store.clear_expired_runtime_leases_at(now).unwrap();
                let expired = holder
                    .as_ref()
                    .is_some_and(|current| current.expires <= now);
                let expected = usize::from(expired || tombstone);
                assert_eq!(removed, expected, "step {step}: sweep count mismatch");
                if expired {
                    holder = None;
                }
                if expired || tombstone {
                    tombstone = false;
                }
            }
        }

        match &holder {
            Some(current) => {
                let row = store
                    .runtime_lease_owner("session:model")
                    .unwrap()
                    .expect("model says a row exists");
                assert_eq!(row.owner_id, current.owner, "step {step}");
                assert_eq!(row.fencing_token, current.generation, "step {step}");
                assert_eq!(
                    row.state_at(now),
                    if current.expires > now {
                        RuntimeLeaseState::Live
                    } else {
                        RuntimeLeaseState::Stale
                    },
                    "step {step}"
                );
            }
            None => assert!(
                store
                    .runtime_lease_owner("session:model")
                    .unwrap()
                    .is_none(),
                "step {step}: model says no live row"
            ),
        }
    }
}

#[test]
fn lease_expiry_boundary_is_exclusive_for_renew_and_assert_but_release_is_safe() {
    let (_data, store, _repo) = test_store();
    let base = time::OffsetDateTime::parse(
        "2026-11-11T00:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let acquisition = store
        .acquire_runtime_lease_at(
            "integration:boundary",
            "owner-a",
            "instance-a:1",
            "integration",
            Some("boundary"),
            10,
            base,
        )
        .unwrap();
    let before = base + time::Duration::seconds(9);
    let at_expiry = base + time::Duration::seconds(10);
    // At the exact expiry instant the row is no longer renewable.
    assert!(matches!(
        store
            .renew_runtime_lease_at(
                "integration:boundary",
                "owner-a",
                acquisition.token(),
                10,
                at_expiry
            )
            .unwrap_err(),
        RuntimeLeaseError::Lost { .. }
    ));
    assert!(
        store
            .assert_runtime_lease_at(
                "integration:boundary",
                "owner-a",
                acquisition.token(),
                before
            )
            .is_ok()
    );
    assert!(
        store
            .assert_runtime_lease_at(
                "integration:boundary",
                "owner-a",
                acquisition.token(),
                at_expiry
            )
            .is_err()
    );
    // Release ignores expiry but still requires the token.
    assert!(
        store
            .release_runtime_lease_at(
                "integration:boundary",
                "owner-a",
                acquisition.token(),
                at_expiry
            )
            .unwrap()
    );
}

#[test]
fn continuation_claim_expiry_boundary_is_inclusive_for_reclaim_and_exclusive_for_ack() {
    let (_data, store, repo) = test_store();
    let (_session_id, continuation_id) = ready_continuation(&store, &repo, "claim boundary");
    let base = time::OffsetDateTime::parse(
        "2026-11-11T00:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let claim = store
        .claim_session_continuation_for_delivery_at(&continuation_id, "owner-a", 10, base)
        .unwrap()
        .unwrap();
    let before = base + time::Duration::seconds(9);
    let at_expiry = base + time::Duration::seconds(10);
    assert!(
        store
            .reclaim_expired_session_continuation_at(&continuation_id, "too early", before)
            .unwrap()
            .is_none()
    );
    // Acknowledgement at expiry is stale by the strict `> now` rule.
    assert!(matches!(
        store
            .mark_session_continuation_delivered_at(
                &continuation_id,
                &claim.owner_id,
                claim.token(),
                at_expiry
            )
            .unwrap_err(),
        SessionContinuationAckError::Lost {
            reason: ContinuationAckLostReason::Expired,
            ..
        }
    ));
    let (reclaimed, event) = store
        .reclaim_expired_session_continuation_at(&continuation_id, "boundary", at_expiry)
        .unwrap()
        .expect("expiry is reclaimable at the boundary");
    assert_eq!(reclaimed.status, "ready");
    assert_eq!(event.kind, "session.continuation.reclaimed");
}

#[test]
fn continuation_ack_before_expiry_uses_the_claim_generation() {
    let (_data, store, repo) = test_store();
    let (_session_id, continuation_id) = ready_continuation(&store, &repo, "claim before expiry");
    let base = time::OffsetDateTime::parse(
        "2026-11-11T00:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let claim = store
        .claim_session_continuation_for_delivery_at(&continuation_id, "owner-a", 10, base)
        .unwrap()
        .unwrap();
    let before = base + time::Duration::seconds(9);
    let delivery = store
        .mark_session_continuation_delivered_at(
            &continuation_id,
            &claim.owner_id,
            claim.token(),
            before,
        )
        .unwrap()
        .expect("unexpired claim acknowledges");
    assert_eq!(delivery.delivered.status, "delivered");
    assert_eq!(delivery.delivered.run_count, 1);
    assert!(delivery.delivered.lease_owner.is_none());
}

#[test]
fn deterministic_continuation_claim_state_machine_matches_the_model() {
    use std::collections::VecDeque;

    struct ClaimModel {
        owner: String,
        token: ContinuationClaimToken,
        expires: time::OffsetDateTime,
    }
    fn next_random(state: &mut u64) -> u64 {
        *state = state
            .wrapping_mul(2_862_933_555_777_941_757)
            .wrapping_add(3_037_000_493);
        *state
    }

    let (_data, store, repo) = test_store();
    let (session_id, continuation_id) = ready_continuation(&store, &repo, "claim model");
    let base = time::OffsetDateTime::parse(
        "2026-11-11T00:00:00Z",
        &time::format_description::well_known::Rfc3339,
    )
    .unwrap();
    let mut now = base;
    let mut random = 0x0dd_ba11_u64;
    let mut status = "ready".to_string();
    let mut current: Option<ClaimModel> = None;
    let mut stale: VecDeque<ContinuationClaimToken> = VecDeque::new();
    let mut delivery_attempt = 0u32;
    let mut attempts = 0u32;
    let owners = ["owner-a", "owner-b", "owner-c"];

    for step in 0..200 {
        now += time::Duration::seconds((next_random(&mut random) % 4) as i64);
        let actor = (next_random(&mut random) % 3) as usize;
        let operation = next_random(&mut random) % 4;
        let live = current.as_ref().is_some_and(|claim| claim.expires > now);
        match operation {
            0 => {
                let ttl = next_random(&mut random) % 7;
                let claim = store
                    .claim_session_continuation_for_delivery_at(
                        &continuation_id,
                        owners[actor],
                        ttl,
                        now,
                    )
                    .unwrap();
                let eligible = status == "ready" || (status == "delivering" && !live);
                if eligible {
                    let claim = claim.unwrap_or_else(|| {
                        panic!("step {step}: {status}/{live} should be claimable")
                    });
                    delivery_attempt += 1;
                    assert_eq!(claim.delivery_attempt, delivery_attempt, "step {step}");
                    if let Some(previous) = current.replace(ClaimModel {
                        owner: claim.owner_id.clone(),
                        token: claim.token().clone(),
                        expires: now + time::Duration::seconds(ttl as i64),
                    }) {
                        stale.push_back(previous.token);
                    }
                    status = "delivering".to_string();
                } else {
                    assert!(
                        claim.is_none(),
                        "step {step}: live delivery claim must not be stealable"
                    );
                }
            }
            1 => {
                let Some(claim) = current.as_ref() else {
                    continue;
                };
                let token = if next_random(&mut random).is_multiple_of(4) {
                    stale
                        .front()
                        .cloned()
                        .unwrap_or_else(ContinuationClaimToken::generate)
                } else {
                    claim.token.clone()
                };
                let result = store.retry_session_continuation_at(
                    &continuation_id,
                    &claim.owner,
                    &token,
                    "model retry",
                    "2999-01-01T00:00:00Z",
                    now,
                );
                let owns_current = token.as_str() == claim.token.as_str() && claim.expires > now;
                if owns_current {
                    assert!(result.is_ok(), "step {step}: retry should apply");
                    attempts += 1;
                    status = "ready".to_string();
                    current = None;
                } else {
                    assert!(result.is_err(), "step {step}: stale retry must fail");
                }
            }
            2 => {
                let result = store
                    .reclaim_expired_session_continuation_at(&continuation_id, "model reclaim", now)
                    .unwrap();
                let expired = current.as_ref().is_some_and(|claim| claim.expires <= now);
                if status == "delivering" && expired {
                    assert!(result.is_some(), "step {step}: expired claim reclaimable");
                    status = "ready".to_string();
                    current = None;
                } else {
                    assert!(result.is_none(), "step {step}: live claim not reclaimable");
                }
            }
            _ => {
                let Some(claim) = current.as_ref() else {
                    continue;
                };
                let token = if let Some(stale_token) = stale.front() {
                    stale_token.clone()
                } else {
                    claim.token.clone()
                };
                let result = store.mark_session_continuation_delivered_at(
                    &continuation_id,
                    &claim.owner,
                    &token,
                    now,
                );
                let owns_current = token.as_str() == claim.token.as_str() && claim.expires > now;
                if owns_current {
                    assert!(
                        result.unwrap().is_some(),
                        "step {step}: delivery should apply"
                    );
                    status = "delivered".to_string();
                    break;
                }
                assert!(result.is_err(), "step {step}: stale delivery must fail");
            }
        }

        let rows = store
            .list_session_continuations(&session_id, None, Some(10))
            .unwrap();
        assert_eq!(rows[0].status, status, "step {step}");
        assert_eq!(rows[0].delivery_attempt, delivery_attempt, "step {step}");
        assert_eq!(rows[0].attempts, attempts, "step {step}");
        match &current {
            Some(claim) if status == "delivering" => {
                assert_eq!(
                    rows[0].lease_owner.as_deref(),
                    Some(claim.owner.as_str()),
                    "step {step}"
                );
                assert!(rows[0].lease_expires_at.is_some(), "step {step}");
            }
            _ => assert!(rows[0].lease_owner.is_none(), "step {step}"),
        }
        if status == "delivered" {
            assert_eq!(rows[0].run_count, 1);
            break;
        }
    }
    // Force the terminal transition if the random walk did not reach one.
    if status != "delivered" {
        if status == "delivering" && current.as_ref().is_some_and(|claim| claim.expires <= now) {
            store
                .reclaim_expired_session_continuation_at(&continuation_id, "final reclaim", now)
                .unwrap()
                .expect("expired model claim reclaims");
            status = "ready".to_string();
            current = None;
            delivery_attempt = delivery_attempt.saturating_add(1);
        }
        if status == "delivering" {
            let claim = current.as_ref().unwrap();
            store
                .mark_session_continuation_delivered_at(
                    &continuation_id,
                    &claim.owner,
                    &claim.token,
                    now,
                )
                .unwrap()
                .expect("live final claim delivers");
        } else {
            let final_claim = store
                .claim_session_continuation_for_delivery_at(
                    &continuation_id,
                    "final-owner",
                    60,
                    now,
                )
                .unwrap()
                .expect("ready model row claims");
            delivery_attempt = delivery_attempt.saturating_add(1);
            store
                .mark_session_continuation_delivered_at(
                    &continuation_id,
                    &final_claim.owner_id,
                    final_claim.token(),
                    now,
                )
                .unwrap()
                .expect("final claim delivers");
        }
        status = "delivered".to_string();
    }
    let rows = store
        .list_session_continuations(&session_id, None, Some(10))
        .unwrap();
    assert_eq!(rows[0].status, status);
    assert_eq!(rows[0].delivery_attempt, delivery_attempt);
    assert_eq!(rows[0].run_count, 1);
    assert!(delivery_attempt > 0);
}

#[test]
fn migration_adds_cross_process_columns_and_creates_audit_table() {
    let data = TempDir::new().unwrap();
    {
        let conn = rusqlite::Connection::open(data.path().join("verlinpark.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE store_meta (
                 key TEXT PRIMARY KEY,
                 value TEXT NOT NULL
             );
             CREATE TABLE runtime_leases (
                 lease_key     TEXT PRIMARY KEY,
                 owner_id      TEXT NOT NULL,
                 kind          TEXT NOT NULL,
                 session_id    TEXT,
                 acquired_at   TEXT NOT NULL,
                 heartbeat_at  TEXT NOT NULL,
                 expires_at    TEXT NOT NULL
             );
             CREATE TABLE session_continuations (
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
             INSERT INTO runtime_leases
                 (lease_key, owner_id, kind, session_id, acquired_at, heartbeat_at, expires_at)
             VALUES ('session:old', 'old-owner', 'session', 'old',
                     '2026-11-11T00:00:00Z', '2026-11-11T00:00:00Z',
                     '2999-11-11T00:00:00Z');
             INSERT INTO session_continuations
                 (id, session_id, project_id, body, status, created_at, updated_at)
             VALUES ('cont-old', 'old-session', 'old-project', 'old', 'pending',
                     '2026-11-11T00:00:00Z', '2026-11-11T00:00:00Z');",
        )
        .unwrap();
    }
    let store = Store::open(data.path()).unwrap();
    for (table, columns) in [
        (
            "runtime_leases",
            vec![
                "owner_instance",
                "lease_token",
                "fencing_token",
                "renewed_at",
                "released_at",
            ],
        ),
        (
            "session_continuations",
            vec![
                "lease_owner",
                "lease_token",
                "lease_expires_at",
                "delivery_attempt",
            ],
        ),
    ] {
        let conn = store.lock().unwrap();
        let mut stmt = conn
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap();
        let names = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .filter_map(Result::ok)
            .collect::<Vec<_>>();
        for column in columns {
            assert!(names.iter().any(|name| name == column), "{table}.{column}");
        }
    }
    {
        let conn = store.lock().unwrap();
        let audit_exists: bool = conn
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'runtime_lease_audit'",
                [],
                |_| Ok(true),
            )
            .optional()
            .unwrap()
            .unwrap_or(false);
        assert!(audit_exists);
    }
    let old_lease = store.runtime_lease_owner("session:old").unwrap().unwrap();
    assert_eq!(old_lease.owner_id, "old-owner");
    assert_eq!(old_lease.owner_instance, "");
    assert_eq!(old_lease.fencing_token, 0);
    let rows = store
        .list_session_continuations("old-session", None, Some(10))
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].delivery_attempt, 0);
    assert!(rows[0].lease_owner.is_none());
    assert!(rows[0].lease_expires_at.is_none());
}

#[test]
fn malformed_daemon_identity_fails_closed() {
    let data = TempDir::new().unwrap();
    {
        let conn = rusqlite::Connection::open(data.path().join("verlinpark.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO store_meta (key, value) VALUES
                 ('daemon_instance', 'not-json');",
        )
        .unwrap();
    }
    let error = match Store::open(data.path()) {
        Ok(_) => panic!("malformed identity opened"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("daemon identity"));
}

#[test]
fn daemon_identity_rejects_colons_and_control_characters() {
    let data = TempDir::new().unwrap();
    {
        let conn = rusqlite::Connection::open(data.path().join("verlinpark.db")).unwrap();
        conn.execute_batch(
            "CREATE TABLE store_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);
             INSERT INTO store_meta (key, value) VALUES
                 ('daemon_instance',
                  '{\"instance_id\":\"bad:id\",\"pid\":1,\"started_at\":\"2026-11-11T00:00:00Z\"}');",
        )
        .unwrap();
    }
    let error = match Store::open(data.path()) {
        Ok(_) => panic!("malformed identity opened"),
        Err(error) => error,
    };
    assert!(format!("{error:#}").contains("malformed"));
}

#[test]
fn startup_cleanup_removes_legacy_unfenced_runtime_lease_rows() {
    let data = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO runtime_leases
             (lease_key, owner_id, owner_instance, kind, session_id, lease_token,
              fencing_token, acquired_at, heartbeat_at, renewed_at, expires_at, released_at)
             VALUES ('session:legacy-live', 'old', '', 'session', 'legacy-live', '', 0,
                     '2026-11-11T00:00:00Z', '2026-11-11T00:00:00Z', '',
                     '2999-11-11T00:00:00Z', NULL)",
            [],
        )
        .unwrap();
    }
    store.recover_state().unwrap();
    assert!(
        store
            .runtime_lease_owner("session:legacy-live")
            .unwrap()
            .is_none()
    );
    let audit = store
        .list_runtime_lease_audit(Some("session:legacy-live"), Some(10))
        .unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].action, "startup_cleanup");
}

#[test]
fn lease_audit_preserves_action_ordering_under_concurrent_owners() {
    let data = TempDir::new().unwrap();
    Store::open(data.path()).unwrap();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
    let mut handles = Vec::new();
    for index in 0..4 {
        let data = data.path().to_path_buf();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            let store = Store::open(&data).unwrap();
            barrier.wait();
            for _ in 0..5 {
                if let Ok(acquisition) = store.acquire_runtime_lease(
                    "session:audit-race",
                    &format!("owner-{index}"),
                    &format!("instance-{index}:{index}"),
                    "session",
                    Some("audit-race"),
                    30,
                ) {
                    let _ = store.release_runtime_lease(
                        "session:audit-race",
                        &format!("owner-{index}"),
                        acquisition.token(),
                    );
                }
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    let store = Store::open(data.path()).unwrap();
    let audit = store
        .list_runtime_lease_audit(Some("session:audit-race"), Some(500))
        .unwrap();
    assert!(!audit.is_empty());
    for pair in audit.windows(2) {
        assert!(pair[0].id > pair[1].id, "history is newest-first");
        assert!(pair[0].fencing_token >= pair[1].fencing_token);
    }
    let acquired = audit
        .iter()
        .filter(|entry| matches!(entry.action.as_str(), "acquired" | "reacquired"))
        .count();
    let released = audit
        .iter()
        .filter(|entry| entry.action == "released")
        .count();
    assert_eq!(acquired, released);
}

#[test]
fn lease_error_display_names_the_holder_and_lost_reason() {
    let (_data, store, _repo) = test_store();
    let holder = store
        .acquire_runtime_lease(
            "session:display",
            "owner-a",
            "instance-a:1",
            "session",
            Some("display"),
            60,
        )
        .unwrap();
    let busy = store
        .acquire_runtime_lease(
            "session:display",
            "owner-b",
            "instance-b:2",
            "session",
            Some("display"),
            60,
        )
        .unwrap_err();
    let text = busy.to_string();
    assert!(text.contains("session:display"));
    assert!(text.contains("owner-a"));
    assert!(text.contains("instance-a:1"));
    assert!(!text.contains("redacted"));
    assert!(!text.contains("lease_token"));
    store
        .release_runtime_lease("session:display", "owner-a", holder.token())
        .unwrap();
    let lost = store
        .renew_runtime_lease("session:display", "owner-a", holder.token(), 60)
        .unwrap_err();
    assert!(lost.to_string().contains("was lost"));
    assert!(lost.to_string().contains("released"));
}

#[test]
fn lease_audit_filter_limit_and_missing_key_are_bounded() {
    let (_data, store, _repo) = test_store();
    for index in 0..5 {
        let acquisition = store
            .acquire_runtime_lease(
                &format!("session:audit-{index}"),
                "owner",
                "instance:1",
                "session",
                None,
                60,
            )
            .unwrap();
        store
            .release_runtime_lease(
                &format!("session:audit-{index}"),
                "owner",
                acquisition.token(),
            )
            .unwrap();
    }
    let all = store.list_runtime_lease_audit(None, Some(3)).unwrap();
    assert_eq!(all.len(), 3);
    let filtered = store
        .list_runtime_lease_audit(Some("session:audit-2"), Some(100))
        .unwrap();
    assert_eq!(filtered.len(), 2);
    assert!(
        filtered
            .iter()
            .all(|entry| entry.lease_key == "session:audit-2")
    );
    assert!(
        store
            .list_runtime_lease_audit(Some("session:none"), None)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store.list_runtime_lease_audit(None, Some(0)).unwrap().len(),
        1
    );
    assert_eq!(
        store
            .list_runtime_lease_audit(None, Some(u32::MAX))
            .unwrap()
            .len(),
        10
    );
}

#[test]
fn recovery_is_idempotent_for_continuations_and_runtime_leases() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("idempotent recovery"))
        .unwrap();
    let session = store
        .create_session(&project.id, "idempotent recovery")
        .unwrap();
    let (continuation, _) = store
        .create_session_continuation_with_event(&NewSessionContinuation {
            session_id: session.id.clone(),
            project_id: project.id.clone(),
            body: "recover twice".into(),
            watch_task_ids: Vec::new(),
            watch_mode: "all".into(),
            deadline_at: None,
            max_attempts: 3,
            series_id: None,
            parent_continuation_id: None,
            recurrence_every_secs: None,
            recurrence_max_runs: 1,
            run_count: 0,
        })
        .unwrap();
    store
        .mark_session_continuation_ready_with_event(&continuation.id, "deadline", &[])
        .unwrap()
        .unwrap();
    store
        .claim_session_continuation_for_delivery_with_event(&continuation.id, "foreign:1:nonce", 0)
        .unwrap()
        .unwrap();
    store
        .acquire_runtime_lease(
            &format!("session:{}", session.id),
            "own",
            &store.process_owner_instance(),
            "session",
            Some(&session.id),
            60,
        )
        .unwrap();

    let first = store.recover_state().unwrap();
    let second = store.recover_state().unwrap();
    assert!(
        first
            .1
            .iter()
            .any(|event| event.kind == "session.continuation.reclaimed")
    );
    assert!(
        !second
            .1
            .iter()
            .any(|event| event.kind == "session.continuation.reclaimed"),
        "a second recovery must not reclaim the same claim twice"
    );
    let rows = store
        .list_session_continuations(&session.id, None, Some(10))
        .unwrap();
    assert_eq!(rows[0].status, "ready");
    assert!(rows[0].lease_owner.is_none());
    assert!(store.list_runtime_leases().unwrap().is_empty());
}

#[test]
fn runtime_lease_owner_keeps_expired_rows_visible_for_operators() {
    let (_data, store, _repo) = test_store();
    store
        .acquire_runtime_lease(
            "session:expired-visible",
            "owner",
            "instance:1",
            "session",
            Some("expired-visible"),
            0,
        )
        .unwrap();
    let row = store
        .runtime_lease_owner("session:expired-visible")
        .unwrap()
        .expect("expired row remains visible until sweep");
    assert_eq!(row.state(), RuntimeLeaseState::Stale);
    assert_eq!(store.list_runtime_leases().unwrap().len(), 1);
    assert_eq!(store.clear_expired_runtime_leases().unwrap(), 1);
    assert!(
        store
            .runtime_lease_owner("session:expired-visible")
            .unwrap()
            .is_none()
    );
}

fn embedding_policy(model: &str) -> MemoryPolicy {
    MemoryPolicy {
        enabled: true,
        max_items: Some(100),
        max_age_days: Some(180),
        task_result_verify_days: Some(90),
        default_verify_days: Some(365),
        auto_supersede: true,
        brief_max_chars: Some(4000),
        sweep_stale: false,
        embedding: Some(MemoryEmbeddingPolicy {
            enabled: true,
            endpoint: Some("http://127.0.0.1:9/v1".to_string()),
            model: Some(model.to_string()),
            dimensions: 8,
            batch_size: 4,
            timeout_secs: 5,
            max_candidates: 8,
            min_similarity: 0.3,
            api_key_env: None,
            ..MemoryEmbeddingPolicy::default()
        }),
    }
}

fn tiny_vector(seed: u8) -> Vec<u8> {
    let mut bytes = b"VPEM".to_vec();
    bytes.push(1);
    bytes.extend_from_slice(&8u32.to_le_bytes());
    for index in 0..8 {
        bytes.extend_from_slice(&(seed as f32 + index as f32 * 0.01).to_le_bytes());
    }
    bytes
}

fn create_embedding_memory(
    store: &Store,
    project: &Project,
    session: &Session,
    title: &str,
) -> MemoryItem {
    store
        .create_memory_with_event(
            &session.id,
            &project.id,
            "agent-1",
            "Leader",
            "fact",
            title,
            &format!("body for {title}"),
            &[],
        )
        .unwrap()
        .0
}

#[test]
fn memory_embedding_policy_round_trips_and_queues_writes() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "embedding").unwrap();
    let policy = embedding_policy("embed-small");
    store.set_memory_policy(&project.id, &policy).unwrap();
    assert_eq!(store.get_memory_policy(&project.id).unwrap(), policy);
    let configured = store.list_memory_embedding_policies().unwrap();
    assert_eq!(configured.len(), 1);
    assert_eq!(configured[0].0, project.id);
    assert_eq!(configured[0].1, policy.embedding.clone().unwrap());
    assert_eq!(
        store.list_enabled_memory_policies().unwrap()[0].1,
        policy,
        "retention decoding must preserve the embedding object"
    );

    let memory = create_embedding_memory(&store, &project, &session, "Release handoff");
    let candidates = store
        .memory_embedding_index_candidates(&project.id, "embed-small", 8, 5, 10)
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].memory.id, memory.id);
    assert!(candidates[0].embedded_model.is_none());
    assert_eq!(candidates[0].job_status.as_deref(), Some("pending"));

    assert_eq!(
        store
            .mark_memory_embedding_attempted(&memory.id, "embed-small", false)
            .unwrap(),
        1
    );
    let vector = tiny_vector(1);
    let event = store
        .record_memory_embedding_success(&memory.id, "embed-small", 8, "hash-1", &vector)
        .unwrap()
        .unwrap();
    assert_eq!(event.kind, "memory.embedding.indexed");
    let payload = event.payload.to_string();
    assert!(!payload.contains("VPEM"));
    assert!(!payload.contains("body for"));
    let stored: Vec<u8> = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT vector FROM memory_embeddings WHERE memory_id = ?1",
            params![memory.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, vector);

    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert!(status.enabled);
    assert!(status.configured);
    assert_eq!(status.model.as_deref(), Some("embed-small"));
    assert_eq!(status.dimensions, 8);
    assert_eq!(status.indexable, 1);
    assert_eq!(status.indexed, 1);
    assert_eq!(status.pending, 0);
    assert_eq!(status.coverage, 1.0);
    assert_eq!(status.recent_jobs.len(), 1);
    assert_eq!(status.recent_jobs[0].status, "indexed");
    assert_eq!(status.usage.calls, 0);

    store
        .record_memory_embedding_usage(&project.id, "embed-small", 1, 12, 0, 48, true)
        .unwrap();
    store
        .record_memory_embedding_usage(&project.id, "embed-small", 1, 9, 9, 36, false)
        .unwrap();
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.usage.calls, 2);
    assert_eq!(status.usage.input_tokens, 21);
    assert_eq!(status.usage.estimated_tokens, 9);
    assert_eq!(status.usage.bytes, 84);
    assert_eq!(status.usage.provider_reported_calls, 1);
    assert_eq!(status.recent_jobs[0].title, "Release handoff");
    assert!(!serde_json::to_string(&status).unwrap().contains("VPEM"));

    // A tag update changes the documented content composition and queues a
    // re-index without deleting the previous vector or failing the write.
    store
        .update_memory_with_event(&memory.id, None, None, Some(&["release".to_string()]))
        .unwrap();
    let candidates = store
        .memory_embedding_index_candidates(&project.id, "embed-small", 8, 5, 10)
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].job_status.as_deref(), Some("pending"));
    assert_eq!(
        candidates[0].embedded_content_hash.as_deref(),
        Some("hash-1")
    );
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.indexed, 0, "pending job is not current coverage");
    assert_eq!(status.pending, 1);
    store
        .mark_memory_embedding_attempted(&memory.id, "embed-small", false)
        .unwrap();
    store
        .record_memory_embedding_success(&memory.id, "embed-small", 8, "hash-2", &tiny_vector(2))
        .unwrap();
    let candidates = store
        .memory_embedding_index_candidates(&project.id, "embed-small", 8, 5, 10)
        .unwrap();
    assert!(candidates.is_empty());
}

#[test]
fn memory_embedding_model_change_resets_coverage_and_rebuild_queues_backlog() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store
        .create_session(&project.id, "embedding model")
        .unwrap();
    let policy = embedding_policy("embed-a");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let first = create_embedding_memory(&store, &project, &session, "First");
    let second = create_embedding_memory(&store, &project, &session, "Second");
    for memory in [&first, &second] {
        store
            .mark_memory_embedding_attempted(&memory.id, "embed-a", false)
            .unwrap();
        store
            .record_memory_embedding_success(&memory.id, "embed-a", 8, "hash-a", &tiny_vector(3))
            .unwrap();
    }
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.coverage, 1.0);

    let changed = embedding_policy("embed-b");
    store.set_memory_policy(&project.id, &changed).unwrap();
    let status = store
        .memory_embedding_status(&project.id, changed.embedding.as_ref())
        .unwrap();
    assert_eq!(status.model.as_deref(), Some("embed-b"));
    assert_eq!(status.indexable, 2);
    assert_eq!(status.indexed, 0);
    assert_eq!(status.coverage, 0.0);
    let candidates = store
        .memory_embedding_index_candidates(&project.id, "embed-b", 8, 5, 10)
        .unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(
        candidates
            .iter()
            .all(|candidate| { candidate.embedded_model.as_deref() == Some("embed-a") })
    );

    let cleared = store.clear_memory_embeddings(&project.id).unwrap();
    assert_eq!(cleared, 4);
    assert!(
        store
            .memory_embedding_index_candidates(&project.id, "embed-b", 8, 5, 10)
            .unwrap()
            .len()
            == 2
    );
    let enqueued = store
        .enqueue_memory_embedding_backlog(&project.id, "embed-b")
        .unwrap();
    assert_eq!(enqueued, 2);
    let status = store
        .memory_embedding_status(&project.id, changed.embedding.as_ref())
        .unwrap();
    assert_eq!(status.pending, 2);
    assert_eq!(status.indexed, 0);
    assert_eq!(status.coverage, 0.0);
}

#[test]
fn memory_embedding_failure_retry_and_attempt_cap_are_visible() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store
        .create_session(&project.id, "embedding retry")
        .unwrap();
    let policy = embedding_policy("embed-retry");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let memory = create_embedding_memory(&store, &project, &session, "Retry me");
    assert_eq!(
        store
            .mark_memory_embedding_attempted(&memory.id, "embed-retry", false)
            .unwrap(),
        1
    );
    let event = store
        .record_memory_embedding_failure(
            &memory.id,
            "embed-retry",
            1,
            "HTTP 503: busy",
            Some("2000-01-01T00:00:00Z"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(event.kind, "memory.embedding.failed");
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.failed, 1);
    assert_eq!(status.last_error.as_deref(), Some("HTTP 503: busy"));
    assert_eq!(status.recent_jobs[0].status, "failed");
    assert_eq!(status.recent_jobs[0].attempts, 1);
    // Due retries are selected again.
    assert_eq!(
        store
            .memory_embedding_index_candidates(&project.id, "embed-retry", 8, 5, 10)
            .unwrap()
            .len(),
        1
    );

    // Keep incrementing through the cap and make the next attempt far future.
    let attempts = store
        .mark_memory_embedding_attempted(&memory.id, "embed-retry", false)
        .unwrap();
    assert_eq!(attempts, 2);
    store
        .record_memory_embedding_failure(&memory.id, "embed-retry", attempts, "still down", None)
        .unwrap();
    assert!(
        store
            .memory_embedding_index_candidates(&project.id, "embed-retry", 8, 5, 10)
            .unwrap()
            .is_empty(),
        "exhausted failed jobs are not retried automatically"
    );
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.failed, 1);
    assert_eq!(status.last_error.as_deref(), Some("still down"));
    assert_eq!(status.recent_jobs[0].next_attempt_at, None);
}

#[test]
fn memory_embedding_rows_are_garbage_collected_with_delete_and_prune() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "embedding gc").unwrap();
    let policy = embedding_policy("embed-gc");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let first = create_embedding_memory(&store, &project, &session, "Delete me");
    let second = create_embedding_memory(&store, &project, &session, "Prune me");
    for memory in [&first, &second] {
        store
            .mark_memory_embedding_attempted(&memory.id, "embed-gc", false)
            .unwrap();
        store
            .record_memory_embedding_success(&memory.id, "embed-gc", 8, "hash-gc", &tiny_vector(4))
            .unwrap();
    }
    store
        .delete_memories_with_events(std::slice::from_ref(&first.id))
        .unwrap();
    let remaining_embeddings: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM memory_embeddings WHERE memory_id = ?1",
            params![first.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining_embeddings, 0);
    let remaining_jobs: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM memory_embedding_jobs WHERE memory_id = ?1",
            params![first.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining_jobs, 0);

    let prune_ids = store
        .memory_ids_for_prune(&project.id, None, Some(0))
        .unwrap();
    assert_eq!(prune_ids, vec![second.id.clone()]);
    store.delete_memories_with_events(&prune_ids).unwrap();
    let total: i64 = store
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(total, 0);
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.indexable, 0);
    assert_eq!(status.indexed, 0);
    assert_eq!(status.coverage, 0.0);
}

#[test]
fn archived_memories_are_not_indexed_and_reactivation_requeues() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store
        .create_session(&project.id, "embedding status")
        .unwrap();
    let policy = embedding_policy("embed-status");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let memory = create_embedding_memory(&store, &project, &session, "Lifecycle");
    store
        .update_memory_with_event(&memory.id, Some("archived"), None, None)
        .unwrap();
    assert!(
        store
            .memory_embedding_index_candidates(&project.id, "embed-status", 8, 5, 10)
            .unwrap()
            .is_empty()
    );
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.indexable, 0);

    store
        .update_memory_with_event(&memory.id, Some("active"), None, None)
        .unwrap();
    let candidates = store
        .memory_embedding_index_candidates(&project.id, "embed-status", 8, 5, 10)
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].job_status.as_deref(), Some("pending"));
}

#[test]
fn disabled_or_absent_embedding_policy_does_not_queue_jobs() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "embedding off").unwrap();
    let memory = create_embedding_memory(&store, &project, &session, "Lexical only");
    let jobs: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM memory_embedding_jobs WHERE memory_id = ?1",
            params![memory.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(jobs, 0);
    assert!(store.list_memory_embedding_policies().unwrap().is_empty());

    let mut disabled = embedding_policy("embed-disabled");
    disabled.embedding.as_mut().unwrap().enabled = false;
    store.set_memory_policy(&project.id, &disabled).unwrap();
    assert_eq!(store.list_memory_embedding_policies().unwrap().len(), 1);
    let second = create_embedding_memory(&store, &project, &session, "Configured but off");
    let jobs: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM memory_embedding_jobs WHERE memory_id = ?1",
            params![second.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        jobs, 1,
        "configured-disabled writes stay ready for enablement"
    );
}

#[test]
fn corrupted_vectors_are_counted_invalid_and_requeued() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store
        .create_session(&project.id, "embedding corrupt")
        .unwrap();
    let policy = embedding_policy("embed-corrupt");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let memory = create_embedding_memory(&store, &project, &session, "Corrupt me");
    store
        .mark_memory_embedding_attempted(&memory.id, "embed-corrupt", false)
        .unwrap();
    store
        .record_memory_embedding_success(
            &memory.id,
            "embed-corrupt",
            8,
            "hash-corrupt",
            &tiny_vector(5),
        )
        .unwrap();
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE memory_embeddings SET vector = x'00' WHERE memory_id = ?1",
            params![memory.id],
        )
        .unwrap();
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.indexable, 1);
    assert_eq!(status.indexed, 0);
    assert_eq!(status.invalid, 1);
    assert_eq!(status.coverage, 0.0);
    let candidates = store
        .memory_embedding_index_candidates(&project.id, "embed-corrupt", 8, 5, 10)
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].embedded_content_hash.as_deref(),
        Some("hash-corrupt")
    );
    store
        .mark_memory_embedding_attempted(&memory.id, "embed-corrupt", false)
        .unwrap();
    store
        .record_memory_embedding_success(
            &memory.id,
            "embed-corrupt",
            8,
            "hash-corrupt",
            &tiny_vector(6),
        )
        .unwrap();
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.indexed, 1);
    assert_eq!(status.invalid, 0);
    assert_eq!(status.coverage, 1.0);
}

fn embedded_memory(
    store: &Store,
    project: &Project,
    session: &Session,
    title: &str,
    seed: u8,
    model: &str,
) -> MemoryItem {
    let memory = create_embedding_memory(store, project, session, title);
    store
        .mark_memory_embedding_attempted(&memory.id, model, false)
        .unwrap();
    store
        .record_memory_embedding_success(
            &memory.id,
            model,
            8,
            &format!("hash-{seed}"),
            &tiny_vector(seed),
        )
        .unwrap();
    memory
}

#[test]
fn memory_embedding_index_generations_are_monotonic_and_assignments_are_fenced() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ivf").unwrap();
    let first = embedded_memory(&store, &project, &session, "One", 1, "embed-ivf");
    let second = embedded_memory(&store, &project, &session, "Two", 2, "embed-ivf");
    let third = embedded_memory(&store, &project, &session, "Three", 3, "embed-ivf");

    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-ivf", 8, 0, 0)
        .unwrap();
    assert_eq!(counts.vector_count, 3);
    assert_eq!(counts.assigned_vectors, 0);
    assert!(
        store
            .memory_embedding_index_row(&project.id)
            .unwrap()
            .is_none()
    );

    let generation = store
        .save_memory_embedding_index(&project.id, "embed-ivf", 8, 0, 2, 2, 3, b"centroids-v1")
        .unwrap();
    assert_eq!(generation, 1);
    let row = store
        .memory_embedding_index_row(&project.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.generation, 1);
    assert_eq!(row.model, "embed-ivf");
    assert_eq!(row.nlist, 2);
    assert_eq!(row.cluster_count, 2);
    assert_eq!(row.vector_count, 3);
    assert_eq!(row.centroids, b"centroids-v1");
    assert_eq!(row.fallback_count, 0);

    let assignments = vec![(first.id.clone(), 0u32), (second.id.clone(), 1u32)];
    assert_eq!(
        store
            .save_memory_embedding_assignments(&project.id, 1, &assignments)
            .unwrap(),
        2
    );
    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-ivf", 8, 1, 2)
        .unwrap();
    assert_eq!(counts.vector_count, 3);
    assert_eq!(counts.assigned_vectors, 2);

    // A racing build that asks for the same/lower generation is bumped, and
    // old-generation assignments are deleted in the same transaction.
    let second_generation = store
        .save_memory_embedding_index(&project.id, "embed-ivf", 8, 1, 2, 2, 3, b"centroids-v2")
        .unwrap();
    assert_eq!(second_generation, 2);
    assert_eq!(
        store
            .save_memory_embedding_assignments(&project.id, 1, &assignments)
            .unwrap(),
        0,
        "stale assignment writes are fenced by generation"
    );
    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-ivf", 8, 2, 2)
        .unwrap();
    assert_eq!(counts.assigned_vectors, 0);
    assert_eq!(
        store
            .save_memory_embedding_assignments(&project.id, 2, &[(third.id.clone(), 0u32)])
            .unwrap(),
        1
    );

    // A new vector invalidates only its own assignment; the rest stay current.
    store
        .record_memory_embedding_success(&first.id, "embed-ivf", 8, "hash-new", &tiny_vector(9))
        .unwrap();
    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-ivf", 8, 2, 2)
        .unwrap();
    assert_eq!(counts.assigned_vectors, 1);

    assert_eq!(store.delete_memory_embedding_index(&project.id).unwrap(), 2);
    assert!(
        store
            .memory_embedding_index_row(&project.id)
            .unwrap()
            .is_none()
    );
    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-ivf", 8, 2, 2)
        .unwrap();
    assert_eq!(counts.vector_count, 3);
    assert_eq!(counts.assigned_vectors, 0);
}

#[test]
fn concurrent_index_builds_get_unique_monotonic_generations() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ivf race").unwrap();
    let memory = embedded_memory(&store, &project, &session, "One", 1, "embed-race");
    let base = store
        .save_memory_embedding_index(&project.id, "embed-race", 8, 0, 4, 1, 1, b"generation-0")
        .unwrap();
    assert_eq!(base, 1);
    store
        .save_memory_embedding_assignments(&project.id, base, &[(memory.id.clone(), 0u32)])
        .unwrap();

    let mut handles = Vec::new();
    for worker in 0..4u8 {
        let store = store.clone();
        let project_id = project.id.clone();
        handles.push(std::thread::spawn(move || {
            let mut generations = Vec::new();
            for attempt in 0..5u8 {
                generations.push(
                    store
                        .save_memory_embedding_index(
                            &project_id,
                            "embed-race",
                            8,
                            1,
                            4,
                            1,
                            1,
                            format!("worker-{worker}-attempt-{attempt}").as_bytes(),
                        )
                        .unwrap(),
                );
            }
            generations
        }));
    }
    let mut generations = Vec::new();
    for handle in handles {
        generations.extend(handle.join().unwrap());
    }
    let unique = generations
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        generations.len(),
        "every build must receive a distinct generation"
    );
    assert_eq!(*unique.iter().next().unwrap(), base + 1);
    assert_eq!(
        *unique.iter().next_back().unwrap(),
        base + u64::try_from(generations.len()).unwrap()
    );
    let row = store
        .memory_embedding_index_row(&project.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.generation, *unique.iter().next_back().unwrap());
    assert_eq!(row.fallback_count, 0);
    // Every prior assignment was deleted by the winning generation; the
    // original memory has no assignment in the final generation yet.
    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-race", 8, row.generation, 1)
        .unwrap();
    assert_eq!(counts.vector_count, 1);
    assert_eq!(counts.assigned_vectors, 0);
}

#[test]
fn assignment_writes_are_fenced_when_a_build_races_them() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store
        .create_session(&project.id, "ivf assignment race")
        .unwrap();
    let first = embedded_memory(&store, &project, &session, "One", 1, "embed-fence");
    let second = embedded_memory(&store, &project, &session, "Two", 2, "embed-fence");
    let generation = store
        .save_memory_embedding_index(&project.id, "embed-fence", 8, 0, 4, 1, 2, b"centroids-a")
        .unwrap();
    let writer_store = store.clone();
    let writer_project = project.id.clone();
    let writer_first = first.id.clone();
    let writer_generation = generation;
    let writer = std::thread::spawn(move || {
        let mut written = 0u32;
        for _ in 0..20 {
            written += writer_store
                .save_memory_embedding_assignments(
                    &writer_project,
                    writer_generation,
                    &[(writer_first.clone(), 0u32)],
                )
                .unwrap();
        }
        written
    });
    let upgraded = store
        .save_memory_embedding_index(&project.id, "embed-fence", 8, 0, 4, 1, 2, b"centroids-b")
        .unwrap();
    assert!(upgraded > generation);
    let written = writer.join().unwrap();
    assert!(
        written <= 20,
        "a fenced writer can only report writes that committed before the new generation"
    );
    // No matter the interleaving, no assignment from the old generation is
    // visible to the new generation.
    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-fence", 8, upgraded, 1)
        .unwrap();
    assert_eq!(counts.vector_count, 2);
    assert_eq!(counts.assigned_vectors, 0);
    assert_eq!(
        store
            .save_memory_embedding_assignments(&project.id, upgraded, &[(second.id.clone(), 0u32)])
            .unwrap(),
        1
    );
}

#[test]
fn memory_embedding_index_vectors_are_bounded_and_deterministic() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ivf sample").unwrap();
    let mut memories = Vec::new();
    for index in 0..6u8 {
        memories.push(embedded_memory(
            &store,
            &project,
            &session,
            &format!("Memory {index}"),
            index,
            "embed-sample",
        ));
    }
    // A corrupt header is excluded by the same SQL predicate as status.
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE memory_embeddings SET vector = x'00' WHERE memory_id = ?1",
            params![memories[0].id],
        )
        .unwrap();
    // An archived row is not eligible.
    store
        .update_memory_with_event(&memories[1].id, Some("archived"), None, None)
        .unwrap();

    let first = store
        .memory_embedding_vectors_for_index(&project.id, "embed-sample", 8, 100, 1024 * 1024)
        .unwrap();
    assert_eq!(first.len(), 4);
    let second = store
        .memory_embedding_vectors_for_index(&project.id, "embed-sample", 8, 100, 1024 * 1024)
        .unwrap();
    assert_eq!(
        first.iter().map(|row| &row.memory.id).collect::<Vec<_>>(),
        second.iter().map(|row| &row.memory.id).collect::<Vec<_>>()
    );
    assert!(
        first.iter().all(|row| row.vector.len() == 9 + 4 * 8),
        "only structurally valid vectors are sampled"
    );

    let small = store
        .memory_embedding_vectors_for_index(&project.id, "embed-sample", 8, 100, 48)
        .unwrap();
    assert_eq!(small.len(), 1, "the byte ceiling bounds the sample");
    let limited = store
        .memory_embedding_vectors_for_index(&project.id, "embed-sample", 8, 2, 1024 * 1024)
        .unwrap();
    assert_eq!(limited.len(), 2);
    assert!(
        store
            .memory_embedding_vectors_for_index(&project.id, "embed-other", 8, 100, 1024 * 1024)
            .unwrap()
            .is_empty(),
        "a model change cannot sample the previous model's vectors"
    );
    assert!(
        store
            .memory_embedding_vectors_for_index(&project.id, "embed-sample", 16, 100, 1024 * 1024)
            .unwrap()
            .is_empty(),
        "a dimension change cannot sample mismatched vectors"
    );
}

#[test]
fn unassigned_vectors_follow_new_writes_and_generation_changes() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ivf assignment").unwrap();
    let first = embedded_memory(&store, &project, &session, "One", 1, "embed-assign");
    let second = embedded_memory(&store, &project, &session, "Two", 2, "embed-assign");
    let generation = store
        .save_memory_embedding_index(&project.id, "embed-assign", 8, 0, 2, 2, 2, b"centroids")
        .unwrap();
    assert_eq!(
        store
            .memory_embedding_unassigned_vectors(&project.id, "embed-assign", 8, generation, 10)
            .unwrap()
            .len(),
        2
    );
    store
        .save_memory_embedding_assignments(&project.id, generation, &[(first.id.clone(), 0u32)])
        .unwrap();
    let pending = store
        .memory_embedding_unassigned_vectors(&project.id, "embed-assign", 8, generation, 10)
        .unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].memory.id, second.id);
    assert_eq!(
        store
            .memory_embedding_unassigned_vectors(&project.id, "embed-assign", 8, generation, 0)
            .unwrap()
            .len(),
        1,
        "zero is clamped to one, not interpreted as unbounded"
    );

    // A refreshed vector is unassigned again even though the row still has an
    // old assignment timestamp; the delete happens in the success transaction.
    store
        .record_memory_embedding_success(&first.id, "embed-assign", 8, "hash-new", &tiny_vector(7))
        .unwrap();
    let pending = store
        .memory_embedding_unassigned_vectors(&project.id, "embed-assign", 8, generation, 10)
        .unwrap();
    assert_eq!(pending.len(), 2);

    let newer = store
        .save_memory_embedding_index(&project.id, "embed-assign", 8, 0, 2, 2, 2, b"centroids-v2")
        .unwrap();
    assert_eq!(newer, generation + 1);
    assert!(
        store
            .memory_embedding_unassigned_vectors(&project.id, "embed-assign", 8, generation, 10)
            .unwrap()
            .len()
            == 2
    );
    assert_eq!(
        store
            .memory_embedding_unassigned_vectors(&project.id, "embed-assign", 8, newer, 10)
            .unwrap()
            .len(),
        2
    );
}

#[test]
fn index_counts_require_valid_headers_and_current_status() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ivf counts").unwrap();
    let first = embedded_memory(&store, &project, &session, "One", 1, "embed-counts");
    let second = embedded_memory(&store, &project, &session, "Two", 2, "embed-counts");
    let generation = store
        .save_memory_embedding_index(&project.id, "embed-counts", 8, 0, 4, 2, 2, b"centroids")
        .unwrap();
    store
        .save_memory_embedding_assignments(
            &project.id,
            generation,
            &[(first.id.clone(), 0u32), (second.id.clone(), 1u32)],
        )
        .unwrap();
    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-counts", 8, generation, 2)
        .unwrap();
    assert_eq!(counts.vector_count, 2);
    assert_eq!(counts.assigned_vectors, 2);

    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE memory_embeddings SET vector = x'00' WHERE memory_id = ?1",
            params![first.id],
        )
        .unwrap();
    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-counts", 8, generation, 2)
        .unwrap();
    assert_eq!(counts.vector_count, 1);
    assert_eq!(counts.assigned_vectors, 1);

    store
        .update_memory_with_event(&second.id, Some("archived"), None, None)
        .unwrap();
    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-counts", 8, generation, 2)
        .unwrap();
    assert_eq!(counts.vector_count, 0);
    assert_eq!(counts.assigned_vectors, 0);

    // Out-of-range assignments never inflate the live assigned count.
    let third = embedded_memory(&store, &project, &session, "Three", 3, "embed-counts");
    store
        .save_memory_embedding_assignments(&project.id, generation, &[(third.id, 99u32)])
        .unwrap();
    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-counts", 8, generation, 2)
        .unwrap();
    assert_eq!(counts.vector_count, 1);
    assert_eq!(counts.assigned_vectors, 0);
}

#[test]
fn ivf_candidate_queries_are_deterministic_deduplicated_and_bounded() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ivf ordering").unwrap();
    let mut memories = Vec::new();
    for index in 0..8u8 {
        memories.push(embedded_memory(
            &store,
            &project,
            &session,
            &format!("Memory {index}"),
            index,
            "embed-order",
        ));
    }
    let generation = store
        .save_memory_embedding_index(&project.id, "embed-order", 8, 0, 4, 2, 8, b"centroids")
        .unwrap();
    store
        .save_memory_embedding_assignments(
            &project.id,
            generation,
            &[
                (memories[0].id.clone(), 0u32),
                (memories[1].id.clone(), 0u32),
                (memories[2].id.clone(), 1u32),
                (memories[3].id.clone(), 1u32),
            ],
        )
        .unwrap();
    let first = store
        .memory_ivf_semantic_candidates(
            &project.id,
            "embed-order",
            8,
            generation,
            &[0, 1],
            false,
            100,
        )
        .unwrap();
    let second = store
        .memory_ivf_semantic_candidates(
            &project.id,
            "embed-order",
            8,
            generation,
            &[0, 1],
            false,
            100,
        )
        .unwrap();
    assert_eq!(first.len(), 4);
    assert_eq!(
        first.iter().map(|row| &row.memory.id).collect::<Vec<_>>(),
        second.iter().map(|row| &row.memory.id).collect::<Vec<_>>()
    );
    let mut ids = first
        .iter()
        .map(|row| row.memory.id.clone())
        .collect::<Vec<_>>();
    let unique = ids
        .iter()
        .cloned()
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(ids.len(), unique.len(), "one assignment per memory id");
    ids.sort();
    assert_eq!(ids.len(), 4);
    // Reverse cluster order must produce the same candidate set.
    let reversed = store
        .memory_ivf_semantic_candidates(
            &project.id,
            "embed-order",
            8,
            generation,
            &[1, 0],
            false,
            100,
        )
        .unwrap();
    let mut reversed_ids = reversed
        .iter()
        .map(|row| row.memory.id.clone())
        .collect::<Vec<_>>();
    reversed_ids.sort();
    ids.sort();
    assert_eq!(ids, reversed_ids);
    // The limit is a hard cap, including a zero/oversized hand edit.
    assert_eq!(
        store
            .memory_ivf_semantic_candidates(
                &project.id,
                "embed-order",
                8,
                generation,
                &[0, 1],
                false,
                1,
            )
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        store
            .memory_ivf_semantic_candidates(
                &project.id,
                "embed-order",
                8,
                generation,
                &[0, 1],
                false,
                0,
            )
            .unwrap()
            .len(),
        1,
        "zero is clamped to one, not interpreted as unbounded"
    );
}

#[test]
fn empty_index_operations_are_safe_and_scoped() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    assert!(
        store
            .memory_embedding_index_row(&project.id)
            .unwrap()
            .is_none()
    );
    assert_eq!(store.delete_memory_embedding_index(&project.id).unwrap(), 0);
    assert_eq!(
        store
            .save_memory_embedding_assignments(&project.id, 0, &[])
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .memory_embedding_unassigned_vectors(&project.id, "embed-none", 8, 0, 10)
            .unwrap()
            .len(),
        0
    );
    let audit = store
        .memory_embedding_assignment_audit(&project.id, "embed-none", 8, 0, 0)
        .unwrap();
    assert_eq!(audit.total, 0);
    assert_eq!(audit.invalid(), 0);
    store
        .note_memory_embedding_index_fallback(&project.id, "embed-none", 8, 64, "not_built")
        .unwrap();
    let row = store
        .memory_embedding_index_row(&project.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.generation, 0);
    assert_eq!(row.fallback_count, 1);
    assert_eq!(row.centroids, Vec::<u8>::new());
    // An explicit deletion clears diagnostic-only state too.
    assert_eq!(store.delete_memory_embedding_index(&project.id).unwrap(), 1);
    assert!(
        store
            .memory_embedding_index_row(&project.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn ivf_candidates_are_scoped_by_generation_cluster_and_lifecycle() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ivf candidates").unwrap();
    let first = embedded_memory(&store, &project, &session, "One", 1, "embed-cand");
    let second = embedded_memory(&store, &project, &session, "Two", 2, "embed-cand");
    let stale = embedded_memory(&store, &project, &session, "Stale", 3, "embed-cand");
    store
        .update_memory_with_event(&stale.id, Some("stale"), None, None)
        .unwrap();
    let generation = store
        .save_memory_embedding_index(&project.id, "embed-cand", 8, 0, 2, 2, 3, b"centroids")
        .unwrap();
    store
        .save_memory_embedding_assignments(
            &project.id,
            generation,
            &[
                (first.id.clone(), 0u32),
                (second.id.clone(), 1u32),
                (stale.id.clone(), 0u32),
            ],
        )
        .unwrap();

    let cluster_zero = store
        .memory_ivf_semantic_candidates(&project.id, "embed-cand", 8, generation, &[0], false, 20)
        .unwrap();
    assert_eq!(cluster_zero.len(), 1);
    assert_eq!(cluster_zero[0].memory.id, first.id);
    let cluster_zero_with_stale = store
        .memory_ivf_semantic_candidates(&project.id, "embed-cand", 8, generation, &[0], true, 20)
        .unwrap();
    assert_eq!(cluster_zero_with_stale.len(), 2);
    let cluster_one = store
        .memory_ivf_semantic_candidates(&project.id, "embed-cand", 8, generation, &[1], false, 20)
        .unwrap();
    assert_eq!(cluster_one.len(), 1);
    assert_eq!(cluster_one[0].memory.id, second.id);
    assert!(
        store
            .memory_ivf_semantic_candidates(
                &project.id,
                "embed-cand",
                8,
                generation,
                &[],
                false,
                20,
            )
            .unwrap()
            .is_empty()
    );
    assert!(
        store
            .memory_ivf_semantic_candidates(
                &project.id,
                "embed-cand",
                8,
                generation + 1,
                &[0, 1],
                true,
                20,
            )
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .memory_ivf_semantic_candidates(
                &project.id,
                "embed-cand",
                8,
                generation,
                &[0, 1],
                true,
                1,
            )
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .memory_ivf_semantic_candidates(
                &project.id,
                "embed-cand",
                16,
                generation,
                &[0, 1],
                true,
                20,
            )
            .unwrap()
            .is_empty(),
        "dimension mismatch cannot reuse a current assignment"
    );
}

#[test]
fn assignment_audit_finds_out_of_range_missing_and_stale_rows() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ivf audit").unwrap();
    let first = embedded_memory(&store, &project, &session, "One", 1, "embed-audit");
    let second = embedded_memory(&store, &project, &session, "Two", 2, "embed-audit");
    let third = embedded_memory(&store, &project, &session, "Three", 3, "embed-audit");
    let fourth = embedded_memory(&store, &project, &session, "Four", 4, "embed-audit");
    let generation = store
        .save_memory_embedding_index(&project.id, "embed-audit", 8, 0, 2, 2, 4, b"centroids")
        .unwrap();
    store
        .save_memory_embedding_assignments(
            &project.id,
            generation,
            &[
                (first.id.clone(), 0u32),
                (second.id.clone(), 9u32),
                (third.id.clone(), 1u32),
                (fourth.id.clone(), 0u32),
            ],
        )
        .unwrap();
    // Simulate a partially restored database: one assignment has no valid
    // current vector and one belongs to an older generation.
    store
        .lock()
        .unwrap()
        .execute(
            "DELETE FROM memory_embeddings WHERE memory_id = ?1",
            params![third.id],
        )
        .unwrap();
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE memory_embedding_assignments SET generation = ?2 WHERE memory_id = ?1",
            params![fourth.id, 0i64],
        )
        .unwrap();

    let audit = store
        .memory_embedding_assignment_audit(&project.id, "embed-audit", 8, generation, 2)
        .unwrap();
    assert_eq!(audit.total, 4);
    assert_eq!(audit.out_of_range, 1);
    assert_eq!(audit.missing_vector, 1);
    assert_eq!(audit.stale_generation, 1);
    assert_eq!(audit.invalid(), 3);

    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-audit", 8, generation, 2)
        .unwrap();
    assert_eq!(counts.vector_count, 3);
    assert_eq!(counts.assigned_vectors, 1);

    assert_eq!(
        store
            .delete_invalid_memory_embedding_assignments(
                &project.id,
                "embed-audit",
                8,
                generation,
                2
            )
            .unwrap(),
        3
    );
    let audit = store
        .memory_embedding_assignment_audit(&project.id, "embed-audit", 8, generation, 2)
        .unwrap();
    assert_eq!(audit.total, 1);
    assert_eq!(audit.invalid(), 0);
    let counts = store
        .memory_embedding_index_counts(&project.id, "embed-audit", 8, generation, 2)
        .unwrap();
    assert_eq!(counts.assigned_vectors, 1);
}

#[test]
fn a_successful_generation_preserves_fallback_diagnostics() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ivf fallback").unwrap();
    let memory = embedded_memory(&store, &project, &session, "One", 1, "embed-fallback");
    let first = store
        .save_memory_embedding_index(&project.id, "embed-fallback", 8, 0, 4, 1, 1, b"centroids-a")
        .unwrap();
    store
        .save_memory_embedding_assignments(&project.id, first, &[(memory.id.clone(), 0u32)])
        .unwrap();
    store
        .note_memory_embedding_index_fallback(&project.id, "embed-fallback", 8, 4, "model_mismatch")
        .unwrap();
    let second = store
        .save_memory_embedding_index(&project.id, "embed-fallback", 8, 0, 4, 1, 1, b"centroids-b")
        .unwrap();
    assert!(second > first);
    let row = store
        .memory_embedding_index_row(&project.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.generation, second);
    assert_eq!(row.fallback_count, 1);
    assert_eq!(row.last_fallback_reason.as_deref(), Some("model_mismatch"));
    assert_eq!(row.last_error, None);
}

#[test]
fn memory_embedding_index_errors_preserve_generation_and_count_fallbacks() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ivf error").unwrap();
    let memory = embedded_memory(&store, &project, &session, "One", 1, "embed-error");
    let generation = store
        .save_memory_embedding_index(&project.id, "embed-error", 8, 0, 2, 1, 1, b"centroids")
        .unwrap();
    store
        .save_memory_embedding_assignments(&project.id, generation, &[(memory.id, 0u32)])
        .unwrap();
    store
        .record_memory_embedding_index_error(&project.id, "embed-error", 8, 2, "build failed")
        .unwrap();
    let row = store
        .memory_embedding_index_row(&project.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.generation, generation);
    assert_eq!(row.centroids, b"centroids");
    assert_eq!(row.last_error.as_deref(), Some("build failed"));
    assert_eq!(row.nlist, 2, "error rows do not rewrite current metadata");

    store
        .note_memory_embedding_index_fallback(&project.id, "embed-error", 8, 2, "model_mismatch")
        .unwrap();
    store
        .note_memory_embedding_index_fallback(&project.id, "embed-error", 8, 2, "below_min_vectors")
        .unwrap();
    let row = store
        .memory_embedding_index_row(&project.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.fallback_count, 2);
    assert_eq!(
        row.last_fallback_reason.as_deref(),
        Some("below_min_vectors")
    );
    assert!(row.last_fallback_at.is_some());

    // An error on a project with no index creates diagnostic-only state that
    // cannot be mistaken for a usable generation.
    let other_repo = TempDir::new().unwrap();
    let other = store
        .add_project(other_repo.path().to_str().unwrap(), Some("other"))
        .unwrap();
    store
        .record_memory_embedding_index_error(&other.id, "embed-error", 8, 64, "no vectors")
        .unwrap();
    let row = store
        .memory_embedding_index_row(&other.id)
        .unwrap()
        .unwrap();
    assert_eq!(row.generation, 0);
    assert_eq!(row.centroids, Vec::<u8>::new());
    assert_eq!(row.last_error.as_deref(), Some("no vectors"));
}

#[test]
fn delete_prune_archive_and_clear_remove_assignments_and_index_state() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "ivf gc").unwrap();
    let first = embedded_memory(&store, &project, &session, "Delete", 1, "embed-gc-ivf");
    let second = embedded_memory(&store, &project, &session, "Prune", 2, "embed-gc-ivf");
    let third = embedded_memory(&store, &project, &session, "Archive", 3, "embed-gc-ivf");
    let generation = store
        .save_memory_embedding_index(&project.id, "embed-gc-ivf", 8, 0, 2, 2, 3, b"centroids")
        .unwrap();
    store
        .save_memory_embedding_assignments(
            &project.id,
            generation,
            &[
                (first.id.clone(), 0u32),
                (second.id.clone(), 1u32),
                (third.id.clone(), 0u32),
            ],
        )
        .unwrap();
    let assignment_count = |id: &str| -> i64 {
        store
            .lock()
            .unwrap()
            .query_row(
                "SELECT COUNT(*) FROM memory_embedding_assignments WHERE memory_id = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap()
    };
    assert_eq!(assignment_count(&first.id), 1);

    store
        .delete_memories_with_events(std::slice::from_ref(&first.id))
        .unwrap();
    assert_eq!(assignment_count(&first.id), 0);
    store
        .update_memory_with_event(&third.id, Some("archived"), None, None)
        .unwrap();
    assert_eq!(assignment_count(&third.id), 0);

    let prune_ids = store
        .memory_ids_for_prune(&project.id, None, Some(0))
        .unwrap();
    assert!(prune_ids.contains(&second.id), "the unpinned row is pruned");
    store.delete_memories_with_events(&prune_ids).unwrap();
    assert_eq!(assignment_count(&second.id), 0);

    let cleared = store.clear_memory_embeddings(&project.id).unwrap();
    assert!(cleared >= 1, "the stored index row is derived state too");
    assert!(
        store
            .memory_embedding_index_row(&project.id)
            .unwrap()
            .is_none()
    );
    let remaining: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM memory_embedding_assignments",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(remaining, 0);
}

#[test]
fn index_rows_reject_empty_centroids_and_survive_project_isolation() {
    let (_data, store, repo) = test_store();
    let first = store
        .add_project(repo.path().to_str().unwrap(), Some("first"))
        .unwrap();
    let second_repo = TempDir::new().unwrap();
    let second = store
        .add_project(second_repo.path().to_str().unwrap(), Some("second"))
        .unwrap();
    let session = store.create_session(&first.id, "ivf isolation").unwrap();
    let memory = embedded_memory(&store, &first, &session, "One", 1, "embed-iso");
    let generation = store
        .save_memory_embedding_index(&first.id, "embed-iso", 8, 0, 4, 1, 1, b"first-centroids")
        .unwrap();
    store
        .save_memory_embedding_assignments(&first.id, generation, &[(memory.id.clone(), 0u32)])
        .unwrap();
    // A second project cannot see the first project's counts, and a stale
    // assignment fenced by the first project's generation cannot be reused.
    let counts = store
        .memory_embedding_index_counts(&second.id, "embed-iso", 8, generation, 1)
        .unwrap();
    assert_eq!(counts.vector_count, 0);
    assert_eq!(counts.assigned_vectors, 0);
    assert!(
        store
            .save_memory_embedding_assignments(&second.id, 0, &[(memory.id, 0u32)])
            .unwrap()
            == 0,
        "no index row means assignment writes are ignored"
    );
    assert!(
        store
            .save_memory_embedding_index(&first.id, "embed-iso", 8, 0, 4, 1, 1, b"")
            .is_err()
    );
    assert_eq!(
        store
            .memory_embedding_index_row(&first.id)
            .unwrap()
            .unwrap()
            .centroids,
        b"first-centroids"
    );
}

#[test]
fn malformed_embedding_policy_json_is_ignored_defensively() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let policy = embedding_policy("embed-malformed");
    store.set_memory_policy(&project.id, &policy).unwrap();
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE project_memory_policies SET embedding_json = ?2 WHERE project_id = ?1",
            params![project.id, r#"{"enabled":true,"endpoint":42}"#],
        )
        .unwrap();
    let stored = store.get_memory_policy(&project.id).unwrap();
    assert!(
        stored.embedding.is_none(),
        "a malformed row must not enable network access"
    );
    assert!(store.list_memory_embedding_policies().unwrap().is_empty());
    let status = store.memory_embedding_status(&project.id, None).unwrap();
    assert!(!status.enabled);
    assert!(!status.configured);
    assert!(status.last_error.is_none());
}

#[test]
fn skip_and_backlog_reset_retry_state() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "embedding skip").unwrap();
    let policy = embedding_policy("embed-skip");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let memory = create_embedding_memory(&store, &project, &session, "Skip me");
    store
        .mark_memory_embedding_attempted(&memory.id, "embed-skip", false)
        .unwrap();
    store
        .record_memory_embedding_failure(&memory.id, "embed-skip", 5, "permanent failure", None)
        .unwrap();
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.failed, 1);

    let event = store
        .record_memory_embedding_skip(&memory.id, "embed-skip", "content hash unchanged")
        .unwrap()
        .unwrap();
    assert_eq!(event.kind, "memory.embedding.skipped");
    assert_eq!(event.payload["reason"], "content hash unchanged");
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.failed, 0);
    assert_eq!(status.skipped, 1);
    assert!(status.last_error.is_none());

    // An explicit backlog reset clears exhausted retry state and last_error.
    store
        .mark_memory_embedding_attempted(&memory.id, "embed-skip", false)
        .unwrap();
    store
        .record_memory_embedding_failure(&memory.id, "embed-skip", 5, "again", None)
        .unwrap();
    let enqueued = store
        .enqueue_memory_embedding_backlog(&project.id, "embed-skip")
        .unwrap();
    assert_eq!(enqueued, 1);
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.pending, 1);
    assert_eq!(status.failed, 0);
    assert!(status.last_error.is_none());
    assert_eq!(status.recent_jobs[0].attempts, 0);
    assert_eq!(status.recent_jobs[0].status, "pending");
}

#[test]
fn semantic_candidates_filter_model_dimensions_status_and_limit() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store
        .create_session(&project.id, "semantic candidates")
        .unwrap();
    let policy = embedding_policy("embed-semantic");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let active = create_embedding_memory(&store, &project, &session, "Active");
    let stale = create_embedding_memory(&store, &project, &session, "Stale");
    let archived = create_embedding_memory(&store, &project, &session, "Archived");
    store
        .update_memory_with_event(&stale.id, Some("stale"), None, None)
        .unwrap();
    store
        .update_memory_with_event(&archived.id, Some("archived"), None, None)
        .unwrap();
    for memory in [&active, &stale, &archived] {
        store
            .mark_memory_embedding_attempted(&memory.id, "embed-semantic", false)
            .unwrap();
        store
            .record_memory_embedding_success(
                &memory.id,
                "embed-semantic",
                8,
                "hash-semantic",
                &tiny_vector(7),
            )
            .unwrap();
    }
    // Archived/superseded update deletes the derived rows, so re-add a valid
    // archived row directly to prove the candidate query filters status.
    store
        .lock()
        .unwrap()
        .execute(
            "INSERT OR REPLACE INTO memory_embeddings
             (memory_id, model, dimensions, content_hash, vector, created_at, updated_at)
             VALUES (?1, 'embed-semantic', 8, 'hash-archived', ?2, ?3, ?3)",
            params![archived.id, tiny_vector(8), "2026-11-11T00:00:00Z"],
        )
        .unwrap();

    let active_only = store
        .memory_semantic_candidates(&project.id, "embed-semantic", 8, false, 10)
        .unwrap();
    assert_eq!(active_only.len(), 1);
    assert_eq!(active_only[0].memory.id, active.id);
    let with_stale = store
        .memory_semantic_candidates(&project.id, "embed-semantic", 8, true, 10)
        .unwrap();
    assert_eq!(with_stale.len(), 2);
    assert!(
        with_stale
            .iter()
            .all(|candidate| candidate.memory.status != "archived")
    );
    let wrong_model = store
        .memory_semantic_candidates(&project.id, "other-model", 8, true, 10)
        .unwrap();
    assert!(wrong_model.is_empty());
    let wrong_dimensions = store
        .memory_semantic_candidates(&project.id, "embed-semantic", 16, true, 10)
        .unwrap();
    assert!(wrong_dimensions.is_empty());
    let limited = store
        .memory_semantic_candidates(&project.id, "embed-semantic", 8, true, 1)
        .unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].vector.len(), 9 + 8 * 4);
}

#[test]
fn clear_embeddings_retains_memories_and_usage_history() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store
        .create_session(&project.id, "embedding clear")
        .unwrap();
    let policy = embedding_policy("embed-clear");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let memory = create_embedding_memory(&store, &project, &session, "Keep memory");
    store
        .mark_memory_embedding_attempted(&memory.id, "embed-clear", false)
        .unwrap();
    store
        .record_memory_embedding_success(
            &memory.id,
            "embed-clear",
            8,
            "hash-clear",
            &tiny_vector(9),
        )
        .unwrap();
    store
        .record_memory_embedding_usage(&project.id, "embed-clear", 2, 40, 0, 160, true)
        .unwrap();
    let cleared = store.clear_memory_embeddings(&project.id).unwrap();
    assert_eq!(cleared, 2);
    assert!(store.get_memory(&memory.id).unwrap().is_some());
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.indexable, 1);
    assert_eq!(status.indexed, 0);
    assert_eq!(status.pending, 0);
    assert!(status.recent_jobs.is_empty());
    assert_eq!(status.usage.calls, 2);
    assert_eq!(status.usage.input_tokens, 40);
    assert_eq!(status.usage.bytes, 160);
    assert_eq!(status.usage.provider_reported_calls, 1);
}

#[test]
fn status_bounds_recent_jobs_and_reports_disabled_configured_state() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "embedding jobs").unwrap();
    let mut policy = embedding_policy("embed-jobs");
    policy.embedding.as_mut().unwrap().enabled = false;
    store.set_memory_policy(&project.id, &policy).unwrap();
    for index in 0..25 {
        let _ = create_embedding_memory(&store, &project, &session, &format!("Memory {index:02}"));
    }
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert!(!status.enabled);
    assert!(status.configured);
    assert_eq!(status.indexable, 25);
    assert_eq!(status.pending, 25);
    assert_eq!(status.recent_jobs.len(), 20, "recent jobs are bounded");
    assert!(
        status
            .recent_jobs
            .windows(2)
            .all(|pair| pair[0].updated_at >= pair[1].updated_at),
        "recent jobs are newest first"
    );
    // No embeddings exist, so status does not manufacture coverage.
    assert_eq!(status.indexed, 0);
    assert_eq!(status.coverage, 0.0);
    assert!(status.usage.calls == 0);
}

#[test]
fn status_ignores_jobs_from_an_old_embedding_model() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store
        .create_session(&project.id, "embedding model jobs")
        .unwrap();
    let first = embedding_policy("embed-old");
    store.set_memory_policy(&project.id, &first).unwrap();
    let memory = create_embedding_memory(&store, &project, &session, "Old model failure");
    store
        .mark_memory_embedding_attempted(&memory.id, "embed-old", false)
        .unwrap();
    store
        .record_memory_embedding_failure(&memory.id, "embed-old", 5, "old model down", None)
        .unwrap();
    let status = store
        .memory_embedding_status(&project.id, first.embedding.as_ref())
        .unwrap();
    assert_eq!(status.failed, 1);
    assert_eq!(status.last_error.as_deref(), Some("old model down"));

    let second = embedding_policy("embed-new");
    store.set_memory_policy(&project.id, &second).unwrap();
    let status = store
        .memory_embedding_status(&project.id, second.embedding.as_ref())
        .unwrap();
    assert_eq!(
        status.failed, 0,
        "old-model failures are not current health"
    );
    assert!(status.last_error.is_none());
    assert!(status.recent_jobs.is_empty());
    // The old failed job is still selected because the model changed.
    assert_eq!(
        store
            .memory_embedding_index_candidates(&project.id, "embed-new", 8, 5, 10)
            .unwrap()
            .len(),
        1
    );
    let attempts = store
        .mark_memory_embedding_attempted(&memory.id, "embed-new", true)
        .unwrap();
    assert_eq!(attempts, 1, "a model change resets the retry budget");
    store
        .record_memory_embedding_success(&memory.id, "embed-new", 8, "hash-new", &tiny_vector(10))
        .unwrap();
    let status = store
        .memory_embedding_status(&project.id, second.embedding.as_ref())
        .unwrap();
    assert_eq!(status.indexed, 1);
    assert_eq!(status.failed, 0);
    assert_eq!(status.recent_jobs[0].status, "indexed");
}

#[test]
fn memory_embedding_index_candidates_are_bounded_and_ordered() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store
        .create_session(&project.id, "embedding order")
        .unwrap();
    let policy = embedding_policy("embed-order");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let mut memories = Vec::new();
    for index in 0..6 {
        memories.push(create_embedding_memory(
            &store,
            &project,
            &session,
            &format!("Memory {index}"),
        ));
    }
    let candidates = store
        .memory_embedding_index_candidates(&project.id, "embed-order", 8, 5, 2)
        .unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.job_status.as_deref() == Some("pending"))
    );
    assert!(
        candidates
            .iter()
            .all(|candidate| candidate.job_attempts == 0)
    );
    // All rows are pending and have no job updated_at, so ordering falls back
    // to created_at then id. Repeating the query is stable.
    let again = store
        .memory_embedding_index_candidates(&project.id, "embed-order", 8, 5, 2)
        .unwrap();
    let ids = candidates
        .iter()
        .map(|candidate| candidate.memory.id.as_str())
        .collect::<Vec<_>>();
    let again_ids = again
        .iter()
        .map(|candidate| candidate.memory.id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(ids, again_ids);
}

#[test]
fn embedding_usage_and_clear_are_project_scoped() {
    let (_data, store, repo_a) = test_store();
    let repo_b = TempDir::new().unwrap();
    let first = store
        .add_project(repo_a.path().to_str().unwrap(), Some("first"))
        .unwrap();
    let second = store
        .add_project(repo_b.path().to_str().unwrap(), Some("second"))
        .unwrap();
    let session = store.create_session(&first.id, "usage scope").unwrap();
    let policy = embedding_policy("embed-scope");
    store.set_memory_policy(&first.id, &policy).unwrap();
    let memory = create_embedding_memory(&store, &first, &session, "Scoped");
    store
        .mark_memory_embedding_attempted(&memory.id, "embed-scope", false)
        .unwrap();
    store
        .record_memory_embedding_success(
            &memory.id,
            "embed-scope",
            8,
            "hash-scope",
            &tiny_vector(11),
        )
        .unwrap();
    store
        .record_memory_embedding_usage(&first.id, "embed-scope", 3, 30, 5, 120, true)
        .unwrap();
    let first_status = store
        .memory_embedding_status(&first.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(first_status.usage.calls, 3);
    let second_status = store.memory_embedding_status(&second.id, None).unwrap();
    assert_eq!(second_status.usage.calls, 0);
    assert_eq!(second_status.indexable, 0);

    let cleared = store.clear_memory_embeddings(&second.id).unwrap();
    assert_eq!(cleared, 0);
    let first_status = store
        .memory_embedding_status(&first.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(
        first_status.indexed, 1,
        "clearing one project leaves another intact"
    );
    assert_eq!(first_status.usage.calls, 3);
}

#[test]
fn embedding_job_attempts_increment_and_clear_previous_errors() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "attempt reset").unwrap();
    let policy = embedding_policy("embed-attempt");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let memory = create_embedding_memory(&store, &project, &session, "Attempt");
    assert_eq!(
        store
            .mark_memory_embedding_attempted(&memory.id, "embed-attempt", false)
            .unwrap(),
        1
    );
    store
        .record_memory_embedding_failure(
            &memory.id,
            "embed-attempt",
            1,
            "first failure",
            Some("2000-01-01T00:00:00Z"),
        )
        .unwrap();
    assert_eq!(
        store
            .mark_memory_embedding_attempted(&memory.id, "embed-attempt", false)
            .unwrap(),
        2
    );
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert_eq!(status.pending, 1);
    assert_eq!(status.failed, 0);
    assert!(status.last_error.is_none());
    assert_eq!(status.recent_jobs[0].attempts, 2);
}

#[test]
fn embedding_failure_errors_are_bounded_when_persisted() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "error bound").unwrap();
    let policy = embedding_policy("embed-error");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let memory = create_embedding_memory(&store, &project, &session, "Error bound");
    store
        .mark_memory_embedding_attempted(&memory.id, "embed-error", false)
        .unwrap();
    store
        .record_memory_embedding_failure(
            &memory.id,
            "embed-error",
            1,
            &"x".repeat(5_000),
            Some("2000-01-01T00:00:00Z"),
        )
        .unwrap();
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    let error = status.last_error.as_deref().unwrap();
    assert!(error.chars().count() <= 2_000);
    assert!(error.ends_with('…'));
    assert_eq!(status.recent_jobs[0].last_error.as_deref(), Some(error));
}

#[test]
fn project_manifest_captures_the_embedding_policy_without_secrets() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let mut policy = embedding_policy("embed-manifest");
    let embedding = policy.embedding.as_mut().unwrap();
    embedding.index = vp_protocol::types::MemoryIndexMode::Ivf;
    embedding.ivf_nlist = 128;
    embedding.ivf_nprobe = 12;
    embedding.ivf_min_vectors = 300;
    embedding.ivf_rebuild_vectors = 6000;
    store.set_memory_policy(&project.id, &policy).unwrap();
    store.record_project_manifest().unwrap();
    let manifest = read_project_manifest(data.path()).unwrap();
    assert_eq!(manifest.len(), 1);
    let stored = manifest[0].memory_policy.embedding.as_ref().unwrap();
    assert_eq!(stored.model.as_deref(), Some("embed-manifest"));
    assert_eq!(stored.dimensions, 8);
    assert_eq!(stored.api_key_env, None);
    assert_eq!(stored.index, vp_protocol::types::MemoryIndexMode::Ivf);
    assert_eq!(stored.ivf_nlist, 128);
    assert_eq!(stored.ivf_nprobe, 12);
    assert_eq!(stored.ivf_min_vectors, 300);
    assert_eq!(stored.ivf_rebuild_vectors, 6000);
    let encoded = serde_json::to_string(&manifest).unwrap();
    assert!(!encoded.contains("sk-"));
    assert!(!encoded.contains("api_key\":"));
}

#[test]
fn replacing_session_memories_requeues_derived_embeddings() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "replay import").unwrap();
    let memory = create_embedding_memory(&store, &project, &session, "Imported claim");
    // No policy existed at creation time, so no job row was queued. (The
    // candidate query still reports missing embeddings when a model is
    // supplied; that is the upgrade path.)
    let jobs: i64 = store
        .lock()
        .unwrap()
        .query_row(
            "SELECT COUNT(*) FROM memory_embedding_jobs WHERE memory_id = ?1",
            params![memory.id],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(jobs, 0);
    store
        .set_memory_policy(&project.id, &embedding_policy("embed-import"))
        .unwrap();
    store
        .replace_session_memories(&session.id, std::slice::from_ref(&memory))
        .unwrap();
    let candidates = store
        .memory_embedding_index_candidates(&project.id, "embed-import", 8, 5, 10)
        .unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(candidates[0].memory.id, memory.id);
    assert_eq!(candidates[0].job_status.as_deref(), Some("pending"));
}

#[test]
fn embedding_outcomes_for_a_deleted_memory_do_not_create_orphans() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "orphan").unwrap();
    let policy = embedding_policy("embed-orphan");
    store.set_memory_policy(&project.id, &policy).unwrap();
    let memory = create_embedding_memory(&store, &project, &session, "Deleted mid-flight");
    store
        .mark_memory_embedding_attempted(&memory.id, "embed-orphan", false)
        .unwrap();
    store
        .delete_memories_with_events(std::slice::from_ref(&memory.id))
        .unwrap();
    let success = store
        .record_memory_embedding_success(
            &memory.id,
            "embed-orphan",
            8,
            "hash-orphan",
            &tiny_vector(12),
        )
        .unwrap();
    assert!(success.is_none());
    let failure = store
        .record_memory_embedding_failure(
            &memory.id,
            "embed-orphan",
            1,
            "too late",
            Some("2000-01-01T00:00:00Z"),
        )
        .unwrap();
    assert!(failure.is_none());
    let skip = store
        .record_memory_embedding_skip(&memory.id, "embed-orphan", "too late")
        .unwrap();
    assert!(skip.is_none());
    let embeddings: i64 = store
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM memory_embeddings", [], |row| {
            row.get(0)
        })
        .unwrap();
    let jobs: i64 = store
        .lock()
        .unwrap()
        .query_row("SELECT COUNT(*) FROM memory_embedding_jobs", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(embeddings, 0);
    assert_eq!(jobs, 0);
}

#[test]
fn clear_embeddings_on_an_empty_project_is_a_noop() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    assert_eq!(store.clear_memory_embeddings(&project.id).unwrap(), 0);
    assert_eq!(store.clear_memory_embeddings(&project.id).unwrap(), 0);
    let status = store.memory_embedding_status(&project.id, None).unwrap();
    assert_eq!(status.indexable, 0);
    assert_eq!(status.indexed, 0);
    assert_eq!(status.coverage, 0.0);
}

#[test]
fn incomplete_embedding_policy_is_not_considered_configured() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let policy = MemoryPolicy {
        embedding: Some(MemoryEmbeddingPolicy::default()),
        ..MemoryPolicy::default()
    };
    // The store persists what the validated engine accepts; this shape is a
    // disabled placeholder, not a configured endpoint.
    store.set_memory_policy(&project.id, &policy).unwrap();
    assert!(store.list_memory_embedding_policies().unwrap().is_empty());
    let status = store
        .memory_embedding_status(&project.id, policy.embedding.as_ref())
        .unwrap();
    assert!(!status.enabled);
    assert!(!status.configured);
    assert_eq!(status.dimensions, 1536);
    assert_eq!(status.indexable, 0);
}

#[test]
fn tool_output_policy_round_trips_and_defaults() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("tool-output"))
        .unwrap();
    assert_eq!(
        store.get_tool_output_policy(&project.id).unwrap(),
        ToolOutputPolicy::default()
    );
    let mut policy = ToolOutputPolicy {
        enabled: false,
        spill_threshold_bytes: 8_192,
        preview_bytes: 2_048,
        max_total_bytes_per_turn: 8_192,
        retention_secs: 3_600,
        max_entries: 42,
        ..ToolOutputPolicy::default()
    };
    policy.tool_thresholds.insert("shell".into(), 65_536);
    store.set_tool_output_policy(&project.id, &policy).unwrap();
    assert_eq!(store.get_tool_output_policy(&project.id).unwrap(), policy);
}

#[test]
fn spills_and_pages_content_addressed_tool_output() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("tool-output"))
        .unwrap();
    let session = store.create_session(&project.id, "inspect").unwrap();
    let content = "line one\nline two";
    let spilled = store
        .spill_tool_output(
            &project.id,
            &session.id,
            Some("call_1"),
            "shell",
            content,
            "line one",
            3_600,
        )
        .unwrap();
    assert_eq!(spilled.entry.bytes, content.len() as u64);
    assert_eq!(spilled.entry.lines, 2);
    assert!(spilled.entry.expires_at.is_some());
    let stored = store
        .get_tool_output(&spilled.entry.id)
        .unwrap()
        .expect("handle");
    assert_eq!(
        store.read_tool_output_text(&stored).unwrap(),
        content.to_string()
    );
    let blob = store.tool_output_blob_path(&spilled.entry.sha256).unwrap();
    assert!(blob.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&blob).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "blob must be private");
    }
    assert!(blob.starts_with(data.path().join("tool-outputs")));
    let listed = store
        .list_tool_outputs(Some(&project.id), None, None, None, false, 10)
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].entry.id, spilled.entry.id);
    let stats = store.tool_output_stats(Some(&project.id), None).unwrap();
    assert_eq!(stats.entries, 1);
    assert_eq!(stats.total_bytes, content.len() as u64);
    assert!(stats.active_entries == 0);
    let by_tool = store
        .tool_output_tool_stats(Some(&project.id), None)
        .unwrap();
    assert_eq!(by_tool.len(), 1);
    assert_eq!(by_tool[0].tool_name, "shell");
    assert_eq!(by_tool[0].entries, 1);
    assert_eq!(by_tool[0].bytes, content.len() as u64);
}

#[test]
fn lists_tool_outputs_with_tool_and_time_filters() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("list"))
        .unwrap();
    let session = store.create_session(&project.id, "list").unwrap();
    let shell = store
        .spill_tool_output(
            &project.id,
            &session.id,
            None,
            "shell",
            "shell body",
            "",
            3_600,
        )
        .unwrap();
    let grep = store
        .spill_tool_output(
            &project.id,
            &session.id,
            None,
            "grep",
            "grep body",
            "",
            3_600,
        )
        .unwrap();
    let shell_only = store
        .list_tool_outputs(Some(&project.id), None, Some("shell"), None, false, 10)
        .unwrap();
    assert_eq!(shell_only.len(), 1);
    assert_eq!(shell_only[0].entry.id, shell.entry.id);
    let newest = store
        .list_tool_outputs(Some(&project.id), None, None, None, false, 1)
        .unwrap();
    assert_eq!(newest.len(), 1);
    let older = store
        .list_tool_outputs(
            Some(&project.id),
            None,
            None,
            Some(newest[0].entry.created_at.as_str()),
            false,
            10,
        )
        .unwrap();
    assert_eq!(older.len(), 1);
    assert_eq!(
        older[0].entry.id,
        if newest[0].entry.id == shell.entry.id {
            grep.entry.id
        } else {
            shell.entry.id
        }
    );
}

#[test]
fn rejects_tool_output_path_and_symlink_escapes() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("tool-output"))
        .unwrap();
    let session = store.create_session(&project.id, "inspect").unwrap();
    assert!(store.tool_output_blob_path("../../etc/passwd").is_err());
    assert!(store.tool_output_blob_path("not-a-hash").is_err());
    let content = "escape check";
    let sha = format!("{:x}", Sha256::digest(content.as_bytes()));
    let dir = store.data_dir().join("tool-outputs");
    std::fs::create_dir_all(&dir).unwrap();
    let outside = store.data_dir().join("outside");
    std::fs::write(&outside, "untouched").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, dir.join(format!("{sha}.txt"))).unwrap();
    let error = store
        .spill_tool_output(&project.id, &session.id, None, "shell", content, "", 3_600)
        .unwrap_err();
    assert!(error.to_string().contains("not a regular file"));
    assert_eq!(std::fs::read_to_string(outside).unwrap(), "untouched");
}

#[test]
fn prunes_expired_and_excess_tool_outputs_but_protects_active_sessions() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("tool-output"))
        .unwrap();
    let active = store.create_session(&project.id, "active").unwrap();
    let idle = store.create_session(&project.id, "idle").unwrap();
    store.update_session_status(&active.id, "running").unwrap();
    let active_output = store
        .spill_tool_output(
            &project.id,
            &active.id,
            None,
            "shell",
            "active body",
            "",
            3_600,
        )
        .unwrap();
    let idle_output = store
        .spill_tool_output(&project.id, &idle.id, None, "shell", "idle body", "", 3_600)
        .unwrap();
    let report = store
        .prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project.id.clone()),
            session_id: None,
            older_than_secs: None,
            max_entries: Some(1),
            dry_run: false,
        })
        .unwrap();
    assert!(report.protected_active >= 1);
    assert!(
        store
            .get_tool_output(&active_output.entry.id)
            .unwrap()
            .is_some(),
        "active unexpired handles are protected"
    );
    assert!(
        store
            .get_tool_output(&idle_output.entry.id)
            .unwrap()
            .is_some(),
        "the newest idle handle is kept by the cap"
    );

    let expired = store
        .spill_tool_output(
            &project.id,
            &active.id,
            None,
            "shell",
            "expired body",
            "",
            60,
        )
        .unwrap();
    // Force the expiry into the past without changing the content.
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE tool_outputs SET expires_at = '2000-01-01T00:00:00Z' WHERE id = ?1",
            params![expired.entry.id],
        )
        .unwrap();
    }
    let dry = store
        .prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project.id.clone()),
            session_id: None,
            older_than_secs: Some(60),
            max_entries: None,
            dry_run: true,
        })
        .unwrap();
    assert!(dry.dry_run);
    assert!(dry.removed == 0);
    assert!(dry.candidates >= 1);
    let report = store
        .prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project.id),
            session_id: None,
            older_than_secs: Some(60),
            max_entries: None,
            dry_run: false,
        })
        .unwrap();
    assert!(report.expired_removed >= 1);
    assert!(
        store.get_tool_output(&expired.entry.id).unwrap().is_none(),
        "expired active-session handles may be removed"
    );
    assert!(
        store
            .get_tool_output(&active_output.entry.id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn audits_missing_orphan_and_mismatched_tool_output_blobs() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("audit"))
        .unwrap();
    let session = store.create_session(&project.id, "audit").unwrap();
    let kept = store
        .spill_tool_output(
            &project.id,
            &session.id,
            None,
            "shell",
            "kept body",
            "",
            3_600,
        )
        .unwrap();
    let missing = store
        .spill_tool_output(
            &project.id,
            &session.id,
            None,
            "shell",
            "missing body",
            "",
            3_600,
        )
        .unwrap();
    let corrupted = store
        .spill_tool_output(
            &project.id,
            &session.id,
            None,
            "shell",
            "corrupt body",
            "",
            3_600,
        )
        .unwrap();
    let dir = store.data_dir().join("tool-outputs");
    std::fs::remove_file(store.tool_output_blob_path(&missing.entry.sha256).unwrap()).unwrap();
    let corrupt_path = store
        .tool_output_blob_path(&corrupted.entry.sha256)
        .unwrap();
    std::fs::write(&corrupt_path, "same length").unwrap();
    let orphan_content = "orphan body";
    let orphan_sha = format!("{:x}", Sha256::digest(orphan_content.as_bytes()));
    std::fs::write(dir.join(format!("{orphan_sha}.txt")), orphan_content).unwrap();

    let audit = store
        .audit_tool_outputs(Some(&project.id), true, false)
        .unwrap();
    assert_eq!(audit.rows, 3);
    assert_eq!(audit.blob_files, 3);
    assert_eq!(audit.missing_blobs, 1);
    assert_eq!(audit.hash_mismatches, 1);
    assert_eq!(audit.orphan_blobs, 1);
    assert_eq!(audit.cleaned_orphans, 0);
    assert!(
        store
            .tool_output_blob_path(&kept.entry.sha256)
            .unwrap()
            .is_file()
    );

    // Orphan cleanup has a five-minute grace window so a concurrent spill
    // cannot have its just-written blob removed before the row insert.
    let orphan_path = dir.join(format!("{orphan_sha}.txt"));
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3_600);
    let times = std::fs::FileTimes::new().set_modified(old);
    std::fs::File::open(&orphan_path)
        .unwrap()
        .set_times(times)
        .unwrap();
    let cleaned = store
        .audit_tool_outputs(Some(&project.id), false, true)
        .unwrap();
    assert_eq!(cleaned.cleaned_orphans, 1);
    assert!(!dir.join(format!("{orphan_sha}.txt")).exists());
    assert!(store.get_tool_output(&missing.entry.id).unwrap().is_some());
}

#[test]
fn identical_tool_output_bodies_share_one_blob_until_the_last_reference_goes() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("dedup"))
        .unwrap();
    let first = store.create_session(&project.id, "first").unwrap();
    let second = store.create_session(&project.id, "second").unwrap();
    let content = "identical shared body";
    let one = store
        .spill_tool_output(&project.id, &first.id, None, "shell", content, "", 3_600)
        .unwrap();
    let two = store
        .spill_tool_output(&project.id, &second.id, None, "grep", content, "", 3_600)
        .unwrap();
    assert_eq!(one.entry.sha256, two.entry.sha256);
    let blob = store.tool_output_blob_path(&one.entry.sha256).unwrap();
    assert!(blob.is_file());
    store.update_session_status(&first.id, "idle").unwrap();
    store.update_session_status(&second.id, "idle").unwrap();
    // Delete one metadata row directly, then prune through the public API.
    let report = store
        .prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project.id.clone()),
            session_id: Some(first.id.clone()),
            older_than_secs: Some(0),
            max_entries: None,
            dry_run: false,
        })
        .unwrap();
    assert_eq!(report.removed, 1);
    assert!(blob.is_file(), "the second reference keeps the blob");
    let report = store
        .prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project.id),
            session_id: Some(second.id),
            older_than_secs: Some(0),
            max_entries: None,
            dry_run: false,
        })
        .unwrap();
    assert_eq!(report.removed, 1);
    assert!(!blob.exists(), "last reference removes the blob");
}

#[test]
fn tool_output_stats_and_lists_distinguish_expired_handles() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("stats"))
        .unwrap();
    let session = store.create_session(&project.id, "stats").unwrap();
    let live = store
        .spill_tool_output(&project.id, &session.id, None, "shell", "live", "", 3_600)
        .unwrap();
    let expired = store
        .spill_tool_output(&project.id, &session.id, None, "grep", "expired", "", 60)
        .unwrap();
    store
        .set_tool_output_expiry(&expired.entry.id, "2000-01-01T00:00:00Z")
        .unwrap();
    let stats = store
        .tool_output_stats(Some(&project.id), Some(&session.id))
        .unwrap();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.expired_entries, 1);
    let visible = store
        .list_tool_outputs(Some(&project.id), None, None, None, false, 10)
        .unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].entry.id, live.entry.id);
    let all = store
        .list_tool_outputs(Some(&project.id), None, None, None, true, 10)
        .unwrap();
    assert_eq!(all.len(), 2);
    let dry = store
        .prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project.id.clone()),
            session_id: None,
            older_than_secs: None,
            max_entries: None,
            dry_run: true,
        })
        .unwrap();
    assert_eq!(dry.expired_removed, 1);
    assert_eq!(dry.removed, 0);
    assert!(store.get_tool_output(&expired.entry.id).unwrap().is_some());
}

#[test]
fn tool_output_audit_flags_symlinked_blob_files() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("symlink-audit"))
        .unwrap();
    let content = "symlink audit body";
    let sha = format!("{:x}", Sha256::digest(content.as_bytes()));
    let dir = store.data_dir().join("tool-outputs");
    std::fs::create_dir_all(&dir).unwrap();
    let outside = store.data_dir().join("outside-audit");
    std::fs::write(&outside, content).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, dir.join(format!("{sha}.txt"))).unwrap();
    let audit = store
        .audit_tool_outputs(Some(&project.id), true, false)
        .unwrap();
    assert_eq!(audit.unsafe_blob_files, 1);
    assert_eq!(audit.orphan_blobs, 0);
    assert_eq!(std::fs::read_to_string(outside).unwrap(), content);
    // A symlinked blob is never deleted by clean_orphans.
    let audit = store
        .audit_tool_outputs(Some(&project.id), false, true)
        .unwrap();
    assert_eq!(audit.cleaned_orphans, 0);
    assert!(dir.join(format!("{sha}.txt")).exists());
}

#[test]
fn project_manifest_round_trips_the_tool_output_policy() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("manifest"))
        .unwrap();
    let policy = ToolOutputPolicy {
        enabled: true,
        spill_threshold_bytes: 32_768,
        preview_bytes: 8_192,
        max_total_bytes_per_turn: 131_072,
        retention_secs: 86_400,
        max_entries: 99,
        ..ToolOutputPolicy::default()
    };
    store.set_tool_output_policy(&project.id, &policy).unwrap();
    store.record_project_manifest().unwrap();
    let snapshots = read_project_manifest(data.path()).unwrap();
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.project.id == project.id)
        .expect("manifest snapshot");
    assert_eq!(snapshot.tool_output_policy, policy);
}

#[test]
fn tool_output_prune_dry_run_preserves_rows_and_blobs() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("dry-run"))
        .unwrap();
    let session = store.create_session(&project.id, "dry").unwrap();
    let spill = store
        .spill_tool_output(&project.id, &session.id, None, "shell", "body", "", 3_600)
        .unwrap();
    let blob = store.tool_output_blob_path(&spill.entry.sha256).unwrap();
    let report = store
        .prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project.id.clone()),
            session_id: None,
            older_than_secs: Some(0),
            max_entries: None,
            dry_run: true,
        })
        .unwrap();
    assert_eq!(report.candidates, 1);
    assert_eq!(report.removed, 0);
    assert!(store.get_tool_output(&spill.entry.id).unwrap().is_some());
    assert!(blob.is_file());
}

#[test]
fn tool_output_audit_project_filter_ignores_other_projects() {
    let (_data, store, repo) = test_store();
    let first = store
        .add_project(repo.path().to_str().unwrap(), Some("first"))
        .unwrap();
    let other_repo = TempDir::new().unwrap();
    let second = store
        .add_project(other_repo.path().to_str().unwrap(), Some("second"))
        .unwrap();
    let first_session = store.create_session(&first.id, "one").unwrap();
    let second_session = store.create_session(&second.id, "two").unwrap();
    store
        .spill_tool_output(
            &first.id,
            &first_session.id,
            None,
            "shell",
            "one",
            "",
            3_600,
        )
        .unwrap();
    let broken = store
        .spill_tool_output(
            &second.id,
            &second_session.id,
            None,
            "shell",
            "two",
            "",
            3_600,
        )
        .unwrap();
    std::fs::remove_file(store.tool_output_blob_path(&broken.entry.sha256).unwrap()).unwrap();
    let audit = store
        .audit_tool_outputs(Some(&first.id), true, false)
        .unwrap();
    assert_eq!(audit.rows, 1);
    assert_eq!(audit.missing_blobs, 0);
    let all = store.audit_tool_outputs(None, true, false).unwrap();
    assert_eq!(all.rows, 2);
    assert_eq!(all.missing_blobs, 1);
}

#[test]
fn deleting_a_session_cascades_tool_output_rows_and_leaves_a_gc_orphan() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("cascade"))
        .unwrap();
    let session = store.create_session(&project.id, "cascade").unwrap();
    let spill = store
        .spill_tool_output(
            &project.id,
            &session.id,
            None,
            "shell",
            "cascade",
            "",
            3_600,
        )
        .unwrap();
    let blob = store.tool_output_blob_path(&spill.entry.sha256).unwrap();
    let conn = rusqlite::Connection::open(data.path().join("verlinpark.db")).unwrap();
    conn.execute("DELETE FROM sessions WHERE id = ?1", params![session.id])
        .unwrap();
    drop(conn);
    assert!(store.get_tool_output(&spill.entry.id).unwrap().is_none());
    assert!(blob.is_file(), "blob cleanup is explicit, not cascaded");
    let audit = store
        .audit_tool_outputs(Some(&project.id), false, true)
        .unwrap();
    assert_eq!(audit.orphan_blobs, 1);
    assert_eq!(audit.cleaned_orphans, 0, "fresh blobs have a grace window");
}

#[test]
fn tool_output_policy_survives_reopening_the_database() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("reopen"))
        .unwrap();
    let policy = ToolOutputPolicy {
        enabled: false,
        spill_threshold_bytes: 4_096,
        preview_bytes: 1_024,
        max_total_bytes_per_turn: 4_096,
        retention_secs: 600,
        max_entries: 12,
        ..ToolOutputPolicy::default()
    };
    store.set_tool_output_policy(&project.id, &policy).unwrap();
    drop(store);
    let reopened = Store::open(data.path()).unwrap();
    assert_eq!(
        reopened.get_tool_output_policy(&project.id).unwrap(),
        policy
    );
}

#[test]
fn prune_max_entries_keeps_newest_rows_and_deletes_old_blobs() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("cap"))
        .unwrap();
    let session = store.create_session(&project.id, "cap").unwrap();
    let mut spills = Vec::new();
    for index in 0..4 {
        spills.push(
            store
                .spill_tool_output(
                    &project.id,
                    &session.id,
                    None,
                    "shell",
                    &format!("body {index}"),
                    "",
                    3_600,
                )
                .unwrap(),
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let report = store
        .prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project.id.clone()),
            session_id: None,
            older_than_secs: None,
            max_entries: Some(2),
            dry_run: false,
        })
        .unwrap();
    assert_eq!(report.overflow_removed, 2);
    assert_eq!(report.removed, 2);
    let remaining = store
        .list_tool_outputs(Some(&project.id), None, None, None, true, 10)
        .unwrap();
    assert_eq!(remaining.len(), 2);
    assert!(
        store
            .get_tool_output(&spills[0].entry.id)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_tool_output(&spills[1].entry.id)
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_tool_output(&spills[2].entry.id)
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_tool_output(&spills[3].entry.id)
            .unwrap()
            .is_some()
    );
}

#[test]
fn tool_output_stats_are_scoped_by_session() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("scope"))
        .unwrap();
    let first = store.create_session(&project.id, "first").unwrap();
    let second = store.create_session(&project.id, "second").unwrap();
    store
        .spill_tool_output(&project.id, &first.id, None, "shell", "one", "", 3_600)
        .unwrap();
    store
        .spill_tool_output(&project.id, &second.id, None, "grep", "two", "", 3_600)
        .unwrap();
    let project_stats = store.tool_output_stats(Some(&project.id), None).unwrap();
    assert_eq!(project_stats.entries, 2);
    let first_stats = store
        .tool_output_stats(Some(&project.id), Some(&first.id))
        .unwrap();
    assert_eq!(first_stats.entries, 1);
    assert_eq!(first_stats.total_bytes, 3);
    let first_tools = store
        .tool_output_tool_stats(Some(&project.id), Some(&first.id))
        .unwrap();
    assert_eq!(first_tools.len(), 1);
    assert_eq!(first_tools[0].tool_name, "shell");
}

#[test]
fn tool_output_audit_without_hash_verification_still_checks_size() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("hash"))
        .unwrap();
    let session = store.create_session(&project.id, "hash").unwrap();
    let spill = store
        .spill_tool_output(&project.id, &session.id, None, "shell", "abc", "", 3_600)
        .unwrap();
    std::fs::write(
        store.tool_output_blob_path(&spill.entry.sha256).unwrap(),
        "longer than abc",
    )
    .unwrap();
    let audit = store
        .audit_tool_outputs(Some(&project.id), false, false)
        .unwrap();
    assert_eq!(audit.hash_mismatches, 1);
}

#[test]
fn tool_output_tool_stats_are_sorted_by_name() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("sort"))
        .unwrap();
    let session = store.create_session(&project.id, "sort").unwrap();
    for tool in ["shell", "grep", "read_file", "shell"] {
        store
            .spill_tool_output(
                &project.id,
                &session.id,
                None,
                tool,
                &format!("{tool} body"),
                "",
                3_600,
            )
            .unwrap();
    }
    let stats = store
        .tool_output_tool_stats(Some(&project.id), None)
        .unwrap();
    let names = stats
        .iter()
        .map(|stat| stat.tool_name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, vec!["grep", "read_file", "shell"]);
    let shell = stats.iter().find(|stat| stat.tool_name == "shell").unwrap();
    assert_eq!(shell.entries, 2);
}

#[test]
fn orphan_audit_counts_bytes_and_never_deletes_referenced_blobs() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("orphan-bytes"))
        .unwrap();
    let session = store.create_session(&project.id, "orphan").unwrap();
    let kept = store
        .spill_tool_output(&project.id, &session.id, None, "shell", "keep", "", 3_600)
        .unwrap();
    let dir = store.data_dir().join("tool-outputs");
    let orphan = "orphan bytes";
    let orphan_sha = format!("{:x}", Sha256::digest(orphan.as_bytes()));
    std::fs::write(dir.join(format!("{orphan_sha}.txt")), orphan).unwrap();
    let audit = store
        .audit_tool_outputs(Some(&project.id), false, false)
        .unwrap();
    assert_eq!(audit.orphan_blobs, 1);
    assert_eq!(audit.orphan_bytes, orphan.len() as u64);
    assert!(
        store
            .tool_output_blob_path(&kept.entry.sha256)
            .unwrap()
            .is_file()
    );
}

#[test]
fn prune_age_cutoff_keeps_new_rows_and_respects_session_filter() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("cutoff"))
        .unwrap();
    let first = store.create_session(&project.id, "first").unwrap();
    let second = store.create_session(&project.id, "second").unwrap();
    store
        .spill_tool_output(&project.id, &first.id, None, "shell", "one", "", 3_600)
        .unwrap();
    store
        .spill_tool_output(&project.id, &second.id, None, "shell", "two", "", 3_600)
        .unwrap();
    let fresh = store
        .prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project.id.clone()),
            session_id: None,
            older_than_secs: Some(60),
            max_entries: None,
            dry_run: false,
        })
        .unwrap();
    assert_eq!(fresh.removed, 0);
    let filtered = store
        .prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project.id.clone()),
            session_id: Some(first.id),
            older_than_secs: Some(0),
            max_entries: None,
            dry_run: false,
        })
        .unwrap();
    assert_eq!(filtered.removed, 1);
    let stats = store.tool_output_stats(Some(&project.id), None).unwrap();
    assert_eq!(stats.entries, 1);
    assert_eq!(
        store
            .tool_output_stats(Some(&project.id), Some(&second.id))
            .unwrap()
            .entries,
        1
    );
}

#[test]
fn tool_output_stats_are_zero_safe_on_an_empty_project() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("empty"))
        .unwrap();
    let stats = store.tool_output_stats(Some(&project.id), None).unwrap();
    assert_eq!(stats.entries, 0);
    assert_eq!(stats.total_bytes, 0);
    assert_eq!(stats.active_entries, 0);
    assert!(stats.oldest_created_at.is_none());
    let audit = store
        .audit_tool_outputs(Some(&project.id), true, false)
        .unwrap();
    assert_eq!(audit.rows, 0);
    assert_eq!(audit.blob_files, 0);
    assert_eq!(audit.orphan_blobs, 0);
}

#[test]
fn tool_output_list_limit_is_clamped_and_ordered_newest_first() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("order"))
        .unwrap();
    let session = store.create_session(&project.id, "order").unwrap();
    let mut ids = Vec::new();
    for index in 0..3 {
        ids.push(
            store
                .spill_tool_output(
                    &project.id,
                    &session.id,
                    None,
                    "shell",
                    &format!("body {index}"),
                    "",
                    3_600,
                )
                .unwrap()
                .entry
                .id,
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let rows = store
        .list_tool_outputs(Some(&project.id), None, None, None, true, 0)
        .unwrap();
    assert_eq!(rows.len(), 1, "limit zero clamps to one");
    assert_eq!(rows[0].entry.id, ids[2]);
    let rows = store
        .list_tool_outputs(Some(&project.id), None, None, None, true, 10)
        .unwrap();
    assert_eq!(rows[0].entry.id, ids[2]);
    assert_eq!(rows[2].entry.id, ids[0]);
}

#[test]
fn tool_output_read_verifies_blob_size_and_hash() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("verify"))
        .unwrap();
    let session = store.create_session(&project.id, "verify").unwrap();
    let spill = store
        .spill_tool_output(
            &project.id,
            &session.id,
            None,
            "shell",
            "verify me",
            "",
            3_600,
        )
        .unwrap();
    let stored = store.get_tool_output(&spill.entry.id).unwrap().unwrap();
    let path = store.tool_output_blob_path(&spill.entry.sha256).unwrap();
    std::fs::write(&path, "short").unwrap();
    let error = store.read_tool_output_text(&stored).unwrap_err();
    assert!(error.to_string().contains("size"));
    std::fs::write(&path, "different").unwrap();
    let error = store.read_tool_output_text(&stored).unwrap_err();
    assert!(error.to_string().contains("hash"));
    std::fs::write(&path, "VERIFY ME").unwrap();
    let error = store.read_tool_output_text(&stored).unwrap_err();
    assert!(error.to_string().contains("hash"));
}

#[test]
fn audit_ignores_temp_files_and_counts_only_hash_shaped_txt_blobs() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("temp"))
        .unwrap();
    let session = store.create_session(&project.id, "temp").unwrap();
    store
        .spill_tool_output(&project.id, &session.id, None, "shell", "kept", "", 3_600)
        .unwrap();
    let dir = store.data_dir().join("tool-outputs");
    std::fs::write(dir.join(".not-a-hash.tmp"), "temp").unwrap();
    std::fs::write(dir.join("README"), "notes").unwrap();
    let audit = store
        .audit_tool_outputs(Some(&project.id), false, false)
        .unwrap();
    assert_eq!(audit.blob_files, 1);
    assert_eq!(audit.orphan_blobs, 0);
    assert_eq!(audit.unsafe_blob_files, 0);
}

#[test]
fn tool_output_stats_and_audit_handle_shared_bodies_correctly() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("shared-stats"))
        .unwrap();
    let first = store.create_session(&project.id, "first").unwrap();
    let second = store.create_session(&project.id, "second").unwrap();
    let content = "shared stats body";
    let one = store
        .spill_tool_output(&project.id, &first.id, None, "shell", content, "", 3_600)
        .unwrap();
    let two = store
        .spill_tool_output(&project.id, &second.id, None, "grep", content, "", 3_600)
        .unwrap();
    assert_eq!(one.entry.sha256, two.entry.sha256);
    let stats = store.tool_output_stats(Some(&project.id), None).unwrap();
    assert_eq!(stats.entries, 2);
    assert_eq!(stats.total_bytes, (content.len() * 2) as u64);
    let audit = store
        .audit_tool_outputs(Some(&project.id), true, false)
        .unwrap();
    assert_eq!(audit.rows, 2);
    assert_eq!(audit.blob_files, 1);
    assert_eq!(audit.missing_blobs, 0);
    assert_eq!(audit.hash_mismatches, 0);
}

#[test]
fn protection_count_reports_active_rows_even_when_the_cap_is_exceeded() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("protect"))
        .unwrap();
    let active = store.create_session(&project.id, "active").unwrap();
    let idle = store.create_session(&project.id, "idle").unwrap();
    let idle_older = store.create_session(&project.id, "idle-older").unwrap();
    store
        .update_session_status(&active.id, "waiting_input")
        .unwrap();
    for index in 0..2 {
        store
            .spill_tool_output(
                &project.id,
                &active.id,
                None,
                "shell",
                &format!("active {index}"),
                "",
                3_600,
            )
            .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    store
        .spill_tool_output(&project.id, &idle.id, None, "grep", "idle", "", 3_600)
        .unwrap();
    std::thread::sleep(std::time::Duration::from_millis(2));
    store
        .spill_tool_output(
            &project.id,
            &idle_older.id,
            None,
            "grep",
            "idle older",
            "",
            3_600,
        )
        .unwrap();
    let report = store
        .prune_tool_outputs(&ToolOutputPruneRequest {
            project_id: Some(project.id.clone()),
            session_id: None,
            older_than_secs: None,
            max_entries: Some(1),
            dry_run: false,
        })
        .unwrap();
    assert_eq!(report.protected_active, 2);
    assert_eq!(report.overflow_removed, 1);
    assert_eq!(
        store
            .tool_output_stats(Some(&project.id), None)
            .unwrap()
            .entries,
        3,
        "active rows can exceed the cap; only the oldest idle row is pruned"
    );
}

#[test]
fn audit_missing_blob_is_not_mistaken_for_an_orphan() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("missing-audit"))
        .unwrap();
    let session = store.create_session(&project.id, "missing").unwrap();
    let spill = store
        .spill_tool_output(
            &project.id,
            &session.id,
            None,
            "shell",
            "missing",
            "",
            3_600,
        )
        .unwrap();
    std::fs::remove_file(store.tool_output_blob_path(&spill.entry.sha256).unwrap()).unwrap();
    let audit = store
        .audit_tool_outputs(Some(&project.id), true, false)
        .unwrap();
    assert_eq!(audit.rows, 1);
    assert_eq!(audit.missing_blobs, 1);
    assert_eq!(audit.orphan_blobs, 0);
    assert_eq!(audit.blob_files, 0);
}

#[test]
fn tool_output_tool_stats_report_preview_bytes() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("preview-stats"))
        .unwrap();
    let session = store.create_session(&project.id, "preview").unwrap();
    store
        .spill_tool_output(
            &project.id,
            &session.id,
            None,
            "shell",
            "full body that is longer",
            "preview",
            3_600,
        )
        .unwrap();
    let stats = store
        .tool_output_tool_stats(Some(&project.id), Some(&session.id))
        .unwrap();
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].tool_name, "shell");
    assert_eq!(stats[0].preview_bytes, "preview".len() as u64);
    assert_eq!(stats[0].bytes, "full body that is longer".len() as u64);
}

#[test]
fn tool_output_blob_path_is_hash_only_and_case_sensitive() {
    let (_data, store, _repo) = test_store();
    assert!(store.tool_output_blob_path(&"a".repeat(64)).is_ok());
    assert!(store.tool_output_blob_path(&"A".repeat(64)).is_err());
    assert!(
        store
            .tool_output_blob_path("a".repeat(63).as_str())
            .is_err()
    );
    assert!(
        store
            .tool_output_blob_path(&format!("{}x", "a".repeat(63)))
            .is_err()
    );
}

fn context_checkpoint_row(
    session: &Session,
    project_id: &str,
    id: &str,
    through_message_id: Option<&str>,
) -> ContextCheckpoint {
    ContextCheckpoint {
        id: id.into(),
        session_id: session.id.clone(),
        project_id: project_id.into(),
        label: format!("row {id}"),
        mode: if through_message_id.is_some() {
            "summary".into()
        } else {
            "manual".into()
        },
        summary: through_message_id.map(|_| "Earlier conversation summary".into()),
        through_message_id: through_message_id.map(str::to_string),
        message_cursor: 0,
        plan_revision: 2,
        provider: "queue".into(),
        model: "test".into(),
        prompt_prefix_hash: "prefix".into(),
        worktree_head: "head".into(),
        tokens_before: 9_000,
        tokens_after: 900,
        structured_state: Some(vp_protocol::types::ContextStructuredState {
            goal: "store context".into(),
            plan_revision: 2,
            ..vp_protocol::types::ContextStructuredState::default()
        }),
        source_compaction_seq: None,
        automatic: true,
        created_at: now_rfc3339(),
    }
}

#[test]
fn context_checkpoint_rows_round_trip_and_compute_message_cursor() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store
        .create_session(&project.id, "checkpoint rows")
        .unwrap();
    let first = store
        .append_message(&session.id, "user", "one", &serde_json::json!([]), None)
        .unwrap();
    let second = store
        .append_message(
            &session.id,
            "assistant",
            "two",
            &serde_json::json!([]),
            None,
        )
        .unwrap();
    let checkpoint = context_checkpoint_row(
        &session,
        &project.id,
        "ctx-store-1",
        Some(second.id.as_str()),
    );
    let event = store
        .record_context_checkpoint_with_event(&session.id, &checkpoint)
        .unwrap();
    assert_eq!(event.kind, "context.checkpointed");
    assert_eq!(event.payload["checkpoint_id"], "ctx-store-1");
    let listed = store.list_context_checkpoints(&session.id, 10).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].message_cursor, 2);
    assert_eq!(
        listed[0].through_message_id.as_deref(),
        Some(second.id.as_str())
    );
    assert_eq!(listed[0].structured_state, checkpoint.structured_state);
    assert!(listed[0].automatic);
    assert_eq!(store.context_checkpoint_count(&session.id).unwrap(), 1);
    assert_eq!(
        store
            .latest_context_checkpoint(&session.id)
            .unwrap()
            .unwrap()
            .id,
        "ctx-store-1"
    );
    assert_eq!(
        store
            .get_context_checkpoint(&session.id, "ctx-store-1")
            .unwrap()
            .unwrap()
            .message_cursor,
        2
    );
    assert!(
        store
            .get_context_checkpoint(&session.id, "missing")
            .unwrap()
            .is_none()
    );
    let _ = first;
}

#[test]
fn context_checkpoint_compaction_and_restore_events_are_ordered() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store.create_session(&project.id, "event order").unwrap();
    let message = store
        .append_message(
            &session.id,
            "user",
            "boundary",
            &serde_json::json!([]),
            None,
        )
        .unwrap();
    let checkpoint = context_checkpoint_row(
        &session,
        &project.id,
        "ctx-order-1",
        Some(message.id.as_str()),
    );
    let (compacted, checkpointed) = store
        .append_context_compaction_with_checkpoint(
            &session.id,
            serde_json::json!({
                "mode": "summary",
                "through_message_id": message.id,
                "summary": "Earlier conversation summary",
                "estimated_tokens_before": 9_000,
                "estimated_tokens_after": 900,
            }),
            &checkpoint,
        )
        .unwrap();
    assert_eq!(compacted.kind, "context.compacted");
    assert_eq!(compacted.payload["context_checkpoint_id"], "ctx-order-1");
    assert_eq!(checkpointed.seq, compacted.seq + 1);
    let stored = store
        .get_context_checkpoint(&session.id, "ctx-order-1")
        .unwrap()
        .unwrap();
    assert_eq!(stored.source_compaction_seq, Some(compacted.seq));
    assert_eq!(stored.message_cursor, 1);

    let (restored_compaction, restored) = store
        .append_context_restore_events(
            &session.id,
            serde_json::json!({
                "mode": "summary",
                "through_message_id": message.id,
                "restored_from_checkpoint": "ctx-order-1"
            }),
            serde_json::json!({"checkpoint_id": "ctx-order-1"}),
        )
        .unwrap();
    assert_eq!(restored_compaction.seq, checkpointed.seq + 1);
    assert_eq!(restored.seq, restored_compaction.seq + 1);
    assert_eq!(restored.kind, "context.restored");
    assert_eq!(restored.payload["compaction_seq"], restored_compaction.seq);
    assert_eq!(store.list_messages(&session.id).unwrap().len(), 1);
}

#[test]
fn context_checkpoint_delete_is_event_backed_and_idempotent() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store.create_session(&project.id, "delete").unwrap();
    let checkpoint = context_checkpoint_row(&session, &project.id, "ctx-delete-1", None);
    store
        .record_context_checkpoint_with_event(&session.id, &checkpoint)
        .unwrap();
    let event = store
        .delete_context_checkpoint_with_event(&session.id, "ctx-delete-1", "manual")
        .unwrap()
        .expect("deletion event");
    assert_eq!(event.kind, "context.checkpoint.deleted");
    assert_eq!(event.payload["checkpoint_id"], "ctx-delete-1");
    assert_eq!(event.payload["reason"], "manual");
    assert_eq!(event.payload["checkpoint"]["mode"], "manual");
    assert_eq!(event.payload["checkpoint"]["plan_revision"], 2);
    assert!(
        store
            .get_context_checkpoint(&session.id, "ctx-delete-1")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .delete_context_checkpoint_with_event(&session.id, "ctx-delete-1", "manual")
            .unwrap()
            .is_none()
    );
}

#[test]
fn context_checkpoint_projection_can_be_replaced_from_replay() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store.create_session(&project.id, "replace").unwrap();
    let first = context_checkpoint_row(&session, &project.id, "ctx-replace-1", None);
    let second = context_checkpoint_row(&session, &project.id, "ctx-replace-2", None);
    store
        .record_context_checkpoint_with_event(&session.id, &first)
        .unwrap();
    store
        .replace_context_checkpoints(&session.id, &[first.clone(), second.clone()])
        .unwrap();
    let rows = store.list_context_checkpoints(&session.id, 10).unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0].id, first.id);
    assert_eq!(rows[1].id, second.id);
    // Replacing with a row from another session is refused.
    let other_session = store.create_session(&project.id, "other").unwrap();
    let mut wrong = context_checkpoint_row(&other_session, &project.id, "ctx-wrong", None);
    wrong.session_id = other_session.id.clone();
    let error = store
        .replace_context_checkpoints(&session.id, &[wrong])
        .unwrap_err();
    assert!(error.to_string().contains("different session"));
}

#[test]
fn context_checkpoint_listing_orders_by_instant_not_string() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context ordering"))
        .unwrap();
    let session = store.create_session(&project.id, "ordering").unwrap();
    let mut earlier = context_checkpoint_row(&session, &project.id, "ctx-earlier", None);
    earlier.created_at = "2026-11-12T00:00:00.099Z".into();
    let mut later = context_checkpoint_row(&session, &project.id, "ctx-later", None);
    later.created_at = "2026-11-12T00:00:00.1Z".into();
    store
        .replace_context_checkpoints(&session.id, &[earlier, later])
        .unwrap();
    let rows = store.list_context_checkpoints(&session.id, 10).unwrap();
    let ids = rows.iter().map(|row| row.id.as_str()).collect::<Vec<_>>();
    assert_eq!(ids, vec!["ctx-earlier", "ctx-later"]);
}

#[test]
fn context_checkpoint_rows_reject_oversized_and_mismatched_payloads() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store.create_session(&project.id, "bounds").unwrap();
    let mut checkpoint = context_checkpoint_row(&session, &project.id, "ctx-big", None);
    checkpoint.summary = Some("x".repeat(129 * 1024));
    let error = store
        .record_context_checkpoint_with_event(&session.id, &checkpoint)
        .unwrap_err();
    assert!(error.to_string().contains("128 KiB"), "{error}");

    let mut checkpoint = context_checkpoint_row(&session, &project.id, "ctx-mode", None);
    checkpoint.mode = "unknown".into();
    assert!(
        store
            .record_context_checkpoint_with_event(&session.id, &checkpoint)
            .is_err()
    );

    let other = store.create_session(&project.id, "other").unwrap();
    let checkpoint = context_checkpoint_row(&other, &project.id, "ctx-other", None);
    let error = store
        .record_context_checkpoint_with_event(&session.id, &checkpoint)
        .unwrap_err();
    assert!(error.to_string().contains("different session"));
}

#[test]
fn context_checkpoint_policy_round_trips_and_is_bounded_in_storage() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    assert_eq!(
        store.get_context_checkpoint_policy(&project.id).unwrap(),
        ContextCheckpointPolicy::default()
    );
    store
        .set_context_checkpoint_policy(
            &project.id,
            &ContextCheckpointPolicy {
                enabled: true,
                max_checkpoints: 4,
                max_age_days: 9,
            },
        )
        .unwrap();
    let policy = store.get_context_checkpoint_policy(&project.id).unwrap();
    assert!(policy.enabled);
    assert_eq!(policy.max_checkpoints, 4);
    assert_eq!(policy.max_age_days, 9);
    let enabled = store.list_enabled_context_checkpoint_policies().unwrap();
    assert_eq!(enabled.len(), 1);
    assert_eq!(enabled[0].0, project.id);
    store
        .set_context_checkpoint_policy(
            &project.id,
            &ContextCheckpointPolicy {
                enabled: false,
                max_checkpoints: 1,
                max_age_days: 0,
            },
        )
        .unwrap();
    assert!(
        store
            .list_enabled_context_checkpoint_policies()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn context_checkpoint_bundle_validation_checks_session_and_project() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store.create_session(&project.id, "bundle").unwrap();
    let checkpoint = context_checkpoint_row(&session, &project.id, "ctx-bundle", None);
    let mut bundle = SessionBundle {
        version: 1,
        exported_at: now_rfc3339(),
        project: project.clone(),
        session: session.clone(),
        events: Vec::new(),
        agents: Vec::new(),
        messages: Vec::new(),
        team_plan: None,
        todos: Vec::new(),
        approvals: Vec::new(),
        merge_queue: Vec::new(),
        agent_messages: Vec::new(),
        artifacts: Vec::new(),
        usage: Vec::new(),
        memories: Vec::new(),
        memory_conflicts: Vec::new(),
        questions: Vec::new(),
        tasks: Vec::new(),
        steering: Vec::new(),
        continuations: Vec::new(),
        recoveries: Vec::new(),
        programs: Vec::new(),
        program_milestones: Vec::new(),
        program_events: Vec::new(),
        session_runs: Vec::new(),
        run_steps: Vec::new(),
        run_checkpoints: Vec::new(),
        context_checkpoints: vec![checkpoint.clone()],
        supervised_processes: Vec::new(),
        test_results: Vec::new(),
    };
    validate_session_bundle(&bundle).unwrap();
    bundle.context_checkpoints[0].project_id = "other-project".into();
    let error = validate_session_bundle(&bundle).unwrap_err();
    assert!(error.to_string().contains("different project"), "{error}");
    bundle.context_checkpoints[0].project_id = project.id.clone();
    bundle.context_checkpoints[0].session_id = "other-session".into();
    let error = validate_session_bundle(&bundle).unwrap_err();
    assert!(error.to_string().contains("belongs to session"), "{error}");
    bundle.context_checkpoints[0].session_id = session.id.clone();
    bundle.context_checkpoints.push(checkpoint);
    let error = validate_session_bundle(&bundle).unwrap_err();
    assert!(error.to_string().contains("duplicated"), "{error}");
}

#[test]
fn context_checkpoint_list_limit_returns_the_newest_rows_oldest_first() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store.create_session(&project.id, "list limit").unwrap();
    for index in 0..5 {
        let mut checkpoint =
            context_checkpoint_row(&session, &project.id, &format!("ctx-limit-{index}"), None);
        checkpoint.created_at = format!("2026-11-11T00:00:0{index}Z");
        store
            .record_context_checkpoint_with_event(&session.id, &checkpoint)
            .unwrap();
    }
    let newest = store.list_context_checkpoints(&session.id, 2).unwrap();
    assert_eq!(newest.len(), 2);
    assert_eq!(newest[0].id, "ctx-limit-3");
    assert_eq!(newest[1].id, "ctx-limit-4");
    let all = store.list_context_checkpoints(&session.id, 100).unwrap();
    assert_eq!(all.len(), 5);
    assert_eq!(all[0].id, "ctx-limit-0");
    assert_eq!(
        store
            .latest_context_checkpoint(&session.id)
            .unwrap()
            .unwrap()
            .id,
        "ctx-limit-4"
    );
}

#[test]
fn context_checkpoint_replacement_clears_old_rows() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store.create_session(&project.id, "replace clears").unwrap();
    let first = context_checkpoint_row(&session, &project.id, "ctx-old", None);
    store
        .record_context_checkpoint_with_event(&session.id, &first)
        .unwrap();
    let replacement = context_checkpoint_row(&session, &project.id, "ctx-new", None);
    store
        .replace_context_checkpoints(&session.id, std::slice::from_ref(&replacement))
        .unwrap();
    let rows = store.list_context_checkpoints(&session.id, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "ctx-new");
    store.replace_context_checkpoints(&session.id, &[]).unwrap();
    assert!(
        store
            .list_context_checkpoints(&session.id, 10)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.context_checkpoint_count(&session.id).unwrap(), 0);
    assert!(
        store
            .latest_context_checkpoint(&session.id)
            .unwrap()
            .is_none()
    );
}

#[test]
fn context_checkpoint_message_cursor_handles_first_middle_and_missing_boundaries() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store.create_session(&project.id, "cursor").unwrap();
    let first = store
        .append_message(&session.id, "user", "one", &serde_json::json!([]), None)
        .unwrap();
    let middle = store
        .append_message(
            &session.id,
            "assistant",
            "two",
            &serde_json::json!([]),
            None,
        )
        .unwrap();
    let last = store
        .append_message(&session.id, "user", "three", &serde_json::json!([]), None)
        .unwrap();

    for (id, boundary, expected) in [
        ("ctx-first", first.id.as_str(), 1),
        ("ctx-middle", middle.id.as_str(), 2),
        ("ctx-last", last.id.as_str(), 3),
        ("ctx-missing", "missing-message", 0),
    ] {
        let checkpoint = context_checkpoint_row(&session, &project.id, id, Some(boundary));
        store
            .record_context_checkpoint_with_event(&session.id, &checkpoint)
            .unwrap();
        let stored = store
            .get_context_checkpoint(&session.id, id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.message_cursor, expected, "{id}");
    }
}

#[test]
fn context_checkpoint_events_reach_the_jsonl_mirror() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store.create_session(&project.id, "mirror").unwrap();
    let checkpoint = context_checkpoint_row(&session, &project.id, "ctx-mirror", None);
    store
        .record_context_checkpoint_with_event(&session.id, &checkpoint)
        .unwrap();
    store
        .delete_context_checkpoint_with_event(&session.id, "ctx-mirror", "prune")
        .unwrap();
    let mirror = store.read_event_mirror(&session.id).unwrap().unwrap();
    assert!(
        mirror
            .iter()
            .any(|event| event.kind == "context.checkpointed")
    );
    assert!(
        mirror
            .iter()
            .any(|event| event.kind == "context.checkpoint.deleted")
    );
    assert!(mirror.iter().any(|event| {
        event.kind == "context.checkpointed"
            && event.payload["checkpoint_id"] == "ctx-mirror"
            && event.payload["mode"] == "manual"
    }));
}

#[test]
fn context_compaction_checkpoint_preserves_a_provided_source_sequence() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store.create_session(&project.id, "source seq").unwrap();
    let mut checkpoint = context_checkpoint_row(&session, &project.id, "ctx-source", None);
    checkpoint.source_compaction_seq = Some(99);
    let (compacted, checkpointed) = store
        .append_context_compaction_with_checkpoint(
            &session.id,
            serde_json::json!({"mode": "trim"}),
            &checkpoint,
        )
        .unwrap();
    assert_eq!(compacted.payload["context_checkpoint_id"], "ctx-source");
    assert_eq!(checkpointed.kind, "context.checkpointed");
    let stored = store
        .get_context_checkpoint(&session.id, "ctx-source")
        .unwrap()
        .unwrap();
    assert_eq!(stored.source_compaction_seq, Some(99));
}

#[test]
fn context_checkpoint_policy_read_clamps_storage_corruption() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO project_context_checkpoint_policies
             (project_id, enabled, max_checkpoints, max_age_days)
             VALUES (?1, 1, 0, 999)",
            params![project.id],
        )
        .unwrap();
    }
    let policy = store.get_context_checkpoint_policy(&project.id).unwrap();
    assert!(policy.enabled);
    assert_eq!(policy.max_checkpoints, 1);
    assert_eq!(policy.max_age_days, 365);
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE project_context_checkpoint_policies
             SET max_checkpoints = 500, max_age_days = 0
             WHERE project_id = ?1",
            params![project.id],
        )
        .unwrap();
    }
    let policy = store.get_context_checkpoint_policy(&project.id).unwrap();
    assert_eq!(policy.max_checkpoints, 100);
    assert_eq!(policy.max_age_days, 0);
}

#[test]
fn importing_a_projection_only_bundle_synthesizes_checkpoint_events() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("context"))
        .unwrap();
    let session = store
        .create_session(&project.id, "projection import")
        .unwrap();
    let checkpoint = context_checkpoint_row(&session, &project.id, "ctx-projection-only", None);
    let bundle = SessionBundle {
        version: 1,
        exported_at: now_rfc3339(),
        project: project.clone(),
        session: session.clone(),
        events: Vec::new(),
        agents: Vec::new(),
        messages: Vec::new(),
        team_plan: None,
        todos: Vec::new(),
        approvals: Vec::new(),
        merge_queue: Vec::new(),
        agent_messages: Vec::new(),
        artifacts: Vec::new(),
        usage: Vec::new(),
        memories: Vec::new(),
        memory_conflicts: Vec::new(),
        questions: Vec::new(),
        tasks: Vec::new(),
        steering: Vec::new(),
        continuations: Vec::new(),
        recoveries: Vec::new(),
        programs: Vec::new(),
        program_milestones: Vec::new(),
        program_events: Vec::new(),
        session_runs: Vec::new(),
        run_steps: Vec::new(),
        run_checkpoints: Vec::new(),
        context_checkpoints: vec![checkpoint.clone()],
        supervised_processes: Vec::new(),
        test_results: Vec::new(),
    };
    let imported = Store::open(data.path().join("imported")).unwrap();
    imported.import_session_bundle(&bundle).unwrap();
    let events = imported.list_all_events(&session.id).unwrap();
    assert!(events.iter().any(|event| {
        event.kind == "context.checkpointed"
            && event.payload["checkpoint_id"] == "ctx-projection-only"
            && event.payload["label"] == checkpoint.label
    }));
    let rows = imported.list_context_checkpoints(&session.id, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "ctx-projection-only");
}

#[test]
fn recent_events_by_kind_is_bounded_and_newest_first() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store.create_session(&project.id, "activity").unwrap();
    for index in 0..6 {
        store
            .append_event(
                &session.id,
                "tool.finished",
                serde_json::json!({"tool_call_id": format!("call-{index}")}),
            )
            .unwrap();
    }
    store
        .append_event(&session.id, "other", serde_json::json!({}))
        .unwrap();
    let events = store
        .list_recent_events_by_kind(&session.id, "tool.finished", 3)
        .unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0].seq, 6);
    assert_eq!(events[1].seq, 5);
    assert_eq!(events[2].seq, 4);
    assert!(events.iter().all(|event| event.kind == "tool.finished"));
    let all = store
        .list_recent_events_by_kind(&session.id, "tool.finished", 100)
        .unwrap();
    assert_eq!(all.len(), 6);
    assert_eq!(all[0].seq, 6);
    assert_eq!(all[5].seq, 1);
    let clamped = store
        .list_recent_events_by_kind(&session.id, "tool.finished", 0)
        .unwrap();
    assert_eq!(clamped.len(), 1);
}

#[test]
fn gui_policy_defaults_round_trips_and_normalizes() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("gui policy"))
        .unwrap();
    let default = store.get_gui_policy(&project.id).unwrap();
    assert_eq!(default, GuiPolicy::default());
    let policy = GuiPolicy {
        grace_secs: 12,
        wait_ms: 2_500,
        status_ttl_secs: 15,
        sensitive_mode: vp_protocol::types::GuiSensitiveMode::RequireApproval,
        sensitive_bundles: vec![" COM.Example.Vault ".into(), "".into()],
        sensitive_titles: vec![" My Vault ".into()],
    };
    store.set_gui_policy(&project.id, &policy).unwrap();
    let loaded = store.get_gui_policy(&project.id).unwrap();
    assert_eq!(loaded.grace_secs, 12);
    assert_eq!(loaded.wait_ms, 2_500);
    assert_eq!(loaded.status_ttl_secs, 15);
    assert_eq!(
        loaded.sensitive_mode,
        vp_protocol::types::GuiSensitiveMode::RequireApproval
    );
    assert_eq!(loaded.sensitive_bundles, vec!["com.example.vault"]);
    assert_eq!(loaded.sensitive_titles, vec!["My Vault"]);

    let session = store.create_session(&project.id, "gui session").unwrap();
    assert_eq!(
        store
            .gui_policy_for_session(&session.id)
            .unwrap()
            .grace_secs,
        12
    );
    assert_eq!(
        store
            .gui_policy_for_session("missing-session")
            .unwrap()
            .grace_secs,
        45
    );
}

#[test]
fn project_manifest_carries_the_gui_policy() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("gui manifest"))
        .unwrap();
    let policy = GuiPolicy {
        grace_secs: 5,
        wait_ms: 700,
        status_ttl_secs: 9,
        sensitive_mode: vp_protocol::types::GuiSensitiveMode::Deny,
        sensitive_bundles: vec!["com.example.Secret".into()],
        sensitive_titles: vec!["Secret Vault".into()],
    };
    store.set_gui_policy(&project.id, &policy).unwrap();
    store.record_project_manifest().unwrap();
    let snapshots = read_project_manifest(data.path()).unwrap();
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.project.id == project.id)
        .unwrap();
    assert_eq!(snapshot.gui_policy, policy.clone().normalized());
}

#[test]
fn legacy_browser_and_computer_leases_block_gui_and_can_be_retired() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("legacy lease"))
        .unwrap();
    let session = store.create_session(&project.id, "legacy").unwrap();
    let legacy = store
        .acquire_computer_lease("browser", "owner-a", &session.id, "agent-a", "Agent A", 60)
        .unwrap();
    let ComputerLeaseAttempt::Acquired { lease: legacy, .. } = legacy else {
        panic!("legacy lease was busy");
    };
    assert_eq!(legacy.surface, "browser");
    let blocked = store
        .acquire_computer_lease("gui", "owner-b", &session.id, "agent-b", "Agent B", 60)
        .unwrap();
    match blocked {
        ComputerLeaseAttempt::Busy { holder, .. } => {
            let holder = holder.expect("legacy holder");
            assert_eq!(holder.surface, "browser");
            assert_eq!(holder.agent_name, "Agent A");
        }
        ComputerLeaseAttempt::Acquired { .. } => panic!("gui acquired over a legacy row"),
    }
    let holder = store.gui_lease_holder().unwrap().expect("holder");
    assert_eq!(holder.surface, "browser");
    let (removed, events) = store
        .clear_legacy_gui_leases("legacy surfaces retired")
        .unwrap();
    assert_eq!(removed, 1);
    assert!(
        events
            .iter()
            .any(|event| event.kind == "computer.lease.expired"
                && event.payload["legacy_surface"] == true)
    );
    assert!(store.gui_lease_holder().unwrap().is_none());
    let acquired = store
        .acquire_computer_lease("gui", "owner-b", &session.id, "agent-b", "Agent B", 60)
        .unwrap();
    match acquired {
        ComputerLeaseAttempt::Acquired { lease, .. } => assert_eq!(lease.surface, "gui"),
        ComputerLeaseAttempt::Busy { .. } => panic!("gui should acquire after retirement"),
    }
}

#[test]
fn gui_actions_are_bounded_and_session_filtered() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("timeline"))
        .unwrap();
    let first = store.create_session(&project.id, "first").unwrap();
    let second = store.create_session(&project.id, "second").unwrap();
    for (index, session) in [(0, &first), (1, &first), (2, &second)] {
        let action = GuiActionInfo {
            id: format!("action-{index}"),
            session_id: session.id.clone(),
            agent_id: "agent".into(),
            agent_name: "Agent".into(),
            surface: "computer".into(),
            action: "click".into(),
            target_app: Some("Example".into()),
            target_bundle_id: Some("com.example.App".into()),
            target_window: Some("Main".into()),
            sensitive_target: false,
            snapshot_generation: index as u64 + 1,
            lease_generation: 1,
            queued_ms: 0,
            latency_ms: 5,
            result: "ok".into(),
            error_code: None,
            occurred_at: format!("2026-11-11T00:00:0{index}Z"),
        };
        store
            .append_event(
                &session.id,
                "gui.action",
                serde_json::to_value(&action).unwrap(),
            )
            .unwrap();
    }
    store
        .append_event(&first.id, "note", serde_json::json!({"text": "ignored"}))
        .unwrap();
    let all = store.recent_gui_actions(None, 2).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all.last().unwrap().id, "action-2");
    let scoped = store.recent_gui_actions(Some(&first.id), 10).unwrap();
    assert_eq!(scoped.len(), 2);
    assert!(scoped.iter().all(|action| action.session_id == first.id));
    assert_eq!(scoped[0].snapshot_generation, 1);
    assert_eq!(store.recent_gui_actions(None, 0).unwrap().len(), 1);
}

#[test]
fn computer_lease_generation_is_monotonic_and_survives_reopen() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("generation"))
        .unwrap();
    let session = store.create_session(&project.id, "generation").unwrap();
    let first = match store
        .acquire_computer_lease("gui", "owner-a", &session.id, "agent-a", "Agent A", 60)
        .unwrap()
    {
        ComputerLeaseAttempt::Acquired { lease, .. } => lease,
        ComputerLeaseAttempt::Busy { .. } => panic!("first lease was busy"),
    };
    assert_eq!(first.generation, 1);
    store
        .release_computer_lease(&first.lease_id, Some("owner-a"))
        .unwrap();
    let second = match store
        .acquire_computer_lease("gui", "owner-a", &session.id, "agent-a", "Agent A", 60)
        .unwrap()
    {
        ComputerLeaseAttempt::Acquired { lease, .. } => lease,
        ComputerLeaseAttempt::Busy { .. } => panic!("second lease was busy"),
    };
    assert_eq!(second.generation, 2);
    drop(store);
    let reopened = Store::open(data.path()).unwrap();
    let listed = reopened.list_computer_leases().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].generation, 2);
    assert_eq!(reopened.gui_lease_holder().unwrap().unwrap().generation, 2);
    reopened
        .release_computer_lease(&second.lease_id, None)
        .unwrap();
    let third = match reopened
        .acquire_computer_lease("gui", "owner-b", &session.id, "agent-b", "Agent B", 60)
        .unwrap()
    {
        ComputerLeaseAttempt::Acquired { lease, .. } => lease,
        ComputerLeaseAttempt::Busy { .. } => panic!("third lease was busy"),
    };
    assert_eq!(third.generation, 3);
}

#[test]
fn gui_action_timeline_clamps_the_limit_at_five_hundred() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("timeline cap"))
        .unwrap();
    let session = store.create_session(&project.id, "cap").unwrap();
    for index in 0..510 {
        let action = GuiActionInfo {
            id: format!("action-{index}"),
            session_id: session.id.clone(),
            agent_id: "agent".into(),
            agent_name: "Agent".into(),
            surface: "computer".into(),
            action: "click".into(),
            target_app: Some("Example".into()),
            target_bundle_id: None,
            target_window: None,
            sensitive_target: false,
            snapshot_generation: 1,
            lease_generation: 1,
            queued_ms: 0,
            latency_ms: 1,
            result: "ok".into(),
            error_code: None,
            occurred_at: format!("2026-11-11T00:00:00.{index:03}Z"),
        };
        store
            .append_event(
                &session.id,
                "gui.action",
                serde_json::to_value(action).unwrap(),
            )
            .unwrap();
    }
    let all = store.recent_gui_actions(None, 10_000).unwrap();
    assert_eq!(all.len(), 500);
    assert_eq!(all.last().unwrap().id, "action-509");
    assert_eq!(all.first().unwrap().id, "action-10");
}

#[test]
fn legacy_cleanup_preserves_a_current_gui_lease() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("legacy preserve"))
        .unwrap();
    let session = store.create_session(&project.id, "preserve").unwrap();
    let current = match store
        .acquire_computer_lease("gui", "owner", &session.id, "agent", "Agent", 60)
        .unwrap()
    {
        ComputerLeaseAttempt::Acquired { lease, .. } => lease,
        ComputerLeaseAttempt::Busy { .. } => panic!("gui lease was busy"),
    };
    // Simulate a legacy row from an older daemon. The unique-surface index only
    // covers actual surfaces, so a raw insert is the only way to model both.
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "INSERT INTO computer_leases
             (lease_id, surface, owner_id, session_id, agent_id, agent_name,
              acquired_at, heartbeat_at, expires_at, generation)
             VALUES ('legacy-1', 'browser', 'legacy-owner', ?1, 'legacy-agent',
                     'Legacy Agent', ?2, ?2, ?3, 0)",
            params![
                &session.id,
                now_rfc3339(),
                "2099-01-01T00:00:00Z".to_string()
            ],
        )
        .unwrap();
    }
    let (removed, events) = store.clear_legacy_gui_leases("retire").unwrap();
    assert_eq!(removed, 1);
    assert!(
        events
            .iter()
            .any(|event| event.kind == "computer.lease.expired")
    );
    let remaining = store.list_computer_leases().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].lease_id, current.lease_id);
    assert_eq!(remaining[0].surface, "gui");
    // Deleting the legacy row did not advance or reset the current generation.
    assert_eq!(remaining[0].generation, current.generation);
}

#[test]
fn gui_lease_holder_ignores_expired_rows() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("expiry"))
        .unwrap();
    let session = store.create_session(&project.id, "expiry").unwrap();
    let lease = match store
        .acquire_computer_lease("gui", "owner", &session.id, "agent", "Agent", 0)
        .unwrap()
    {
        ComputerLeaseAttempt::Acquired { lease, .. } => lease,
        ComputerLeaseAttempt::Busy { .. } => panic!("lease was busy"),
    };
    std::thread::sleep(std::time::Duration::from_millis(10));
    assert!(store.gui_lease_holder().unwrap().is_none());
    let (removed, _) = store.clear_expired_computer_leases().unwrap();
    assert_eq!(removed, 1);
    assert!(store.computer_lease(&lease.lease_id).unwrap().is_none());
}

#[test]
fn project_manifest_without_gui_policy_decodes_the_default() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("legacy manifest"))
        .unwrap();
    store
        .set_gui_policy(&project.id, &GuiPolicy::default())
        .unwrap();
    store.record_project_manifest().unwrap();
    let raw = std::fs::read_to_string(data.path().join("projects.jsonl")).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    value.as_object_mut().unwrap().remove("gui_policy");
    let snapshot: ProjectManifestSnapshot = serde_json::from_value(value).unwrap();
    assert_eq!(snapshot.gui_policy, GuiPolicy::default());
    assert_eq!(snapshot.project.id, project.id);
}

#[test]
fn gui_lease_holder_reports_the_oldest_legacy_surface() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("legacy order"))
        .unwrap();
    let session = store.create_session(&project.id, "legacy order").unwrap();
    {
        let conn = store.lock().unwrap();
        for (lease_id, surface, acquired) in [
            ("legacy-browser", "browser", "2026-11-11T00:00:01Z"),
            ("legacy-computer", "computer", "2026-11-11T00:00:02Z"),
        ] {
            conn.execute(
                "INSERT INTO computer_leases
                 (lease_id, surface, owner_id, session_id, agent_id, agent_name,
                  acquired_at, heartbeat_at, expires_at, generation)
                 VALUES (?1, ?2, 'legacy', ?3, 'legacy', 'Legacy', ?4, ?4,
                         '2099-01-01T00:00:00Z', 0)",
                params![lease_id, surface, &session.id, acquired],
            )
            .unwrap();
        }
    }
    let holder = store.gui_lease_holder().unwrap().unwrap();
    assert_eq!(holder.lease_id, "legacy-browser");
    assert_eq!(holder.surface, "browser");
    let blocked = store
        .acquire_computer_lease("gui", "owner", &session.id, "agent", "Agent", 60)
        .unwrap();
    match blocked {
        ComputerLeaseAttempt::Busy { holder, .. } => {
            assert_eq!(holder.unwrap().lease_id, "legacy-browser");
        }
        ComputerLeaseAttempt::Acquired { .. } => panic!("gui acquired over legacy rows"),
    }
    let (removed, _) = store.clear_legacy_gui_leases("order").unwrap();
    assert_eq!(removed, 2);
    assert!(store.gui_lease_holder().unwrap().is_none());
}

fn source_embedding_vector(dimensions: u32, seed: f32) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(9 + dimensions as usize * 4);
    bytes.extend_from_slice(b"VPEM");
    bytes.push(1);
    bytes.extend_from_slice(&dimensions.to_le_bytes());
    for index in 0..dimensions {
        bytes.extend_from_slice(&(seed + index as f32 * 0.01).to_le_bytes());
    }
    bytes
}

fn active_source_policy(model: &str) -> SourceEmbeddingPolicy {
    SourceEmbeddingPolicy {
        enabled: true,
        endpoint_env: Some("VERLINPARK_TEST_SOURCE_ENDPOINT".to_string()),
        model: Some(model.to_string()),
        dimensions: 8,
        batch_size: 2,
        max_chunks_per_source: 2,
        retention_days: 1,
        timeout_secs: 5,
        api_key_env: None,
    }
}

fn source_job(store: &Store, source_id: &str) -> Option<SourceEmbeddingJobRow> {
    let conn = store.lock().unwrap();
    conn.query_row(
        "SELECT j.source_id, s.title, j.project_id, j.generation, j.model, j.status,
                j.attempts, j.chunks_total, j.chunks_done, j.last_error,
                j.next_attempt_at, j.updated_at
         FROM source_embedding_jobs j
         JOIN sources s ON s.id = j.source_id
         WHERE j.source_id = ?1",
        params![source_id],
        |row| {
            Ok(SourceEmbeddingJobRow {
                source_id: row.get(0)?,
                source_title: row.get(1)?,
                project_id: row.get(2)?,
                generation: u64::try_from(row.get::<_, i64>(3)?).unwrap_or(0),
                model: row.get(4)?,
                status: row.get(5)?,
                attempts: u32::try_from(row.get::<_, i64>(6)?).unwrap_or(0),
                chunks_total: u32::try_from(row.get::<_, i64>(7)?).unwrap_or(0),
                chunks_done: u32::try_from(row.get::<_, i64>(8)?).unwrap_or(0),
                last_error: row.get(9)?,
                next_attempt_at: row.get(10)?,
                updated_at: row.get(11)?,
            })
        },
    )
    .optional()
    .unwrap()
}

#[test]
fn source_embedding_generation_and_jobs_follow_snapshot_refresh() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("sources"))
        .unwrap();
    store
        .set_source_embedding_policy(&project.id, &active_source_policy("embed-small"))
        .unwrap();
    let (source, chunks) = source_fixture(
        &project.id,
        "source-gen",
        "Runtime",
        "The runtime uses deterministic leases.",
    );
    store.insert_source(&source, &chunks).unwrap();
    assert_eq!(store.source_embedding_generation("source-gen").unwrap(), 1);
    let job = source_job(&store, "source-gen").expect("pending job");
    assert_eq!(job.status, "pending");
    assert_eq!(job.generation, 1);
    assert_eq!(job.chunks_total, 1);
    assert_eq!(job.model, "embed-small");
    assert_eq!(
        store
            .due_source_embedding_jobs(
                &project.id,
                "embed-small",
                5,
                &now_rfc3339(),
                "2099-01-01T00:00:00Z",
                4
            )
            .unwrap()
            .len(),
        1
    );
    assert!(
        store
            .mark_source_embedding_job_running("source-gen", 1)
            .unwrap()
    );
    assert!(
        store
            .record_source_embedding_success(
                "source-gen",
                &chunks[0].id,
                1,
                1,
                "embed-small",
                8,
                "hash-one",
                &source_embedding_vector(8, 0.2),
            )
            .unwrap()
    );
    store
        .record_source_embedding_job_done("source-gen", 1, 1)
        .unwrap();
    assert_eq!(
        store
            .source_embedding_ready_count(&project.id, "embed-small", 8, 2)
            .unwrap(),
        1
    );

    // Refresh replaces chunks and advances the generation. The old vector is
    // retained as stale metadata but cannot serve retrieval.
    store.insert_source(&source, &chunks).unwrap();
    assert_eq!(store.source_embedding_generation("source-gen").unwrap(), 2);
    assert_eq!(
        store
            .source_embedding_ready_count(&project.id, "embed-small", 8, 2)
            .unwrap(),
        0
    );
    assert_eq!(store.source_embedding_stale_count(&project.id).unwrap(), 1);
    assert_eq!(source_job(&store, "source-gen").unwrap().generation, 2);
    // A late response for the superseded generation is ignored.
    assert!(
        !store
            .record_source_embedding_success(
                "source-gen",
                &chunks[0].id,
                1,
                1,
                "embed-small",
                8,
                "hash-one",
                &source_embedding_vector(8, 0.2),
            )
            .unwrap()
    );
    assert_eq!(
        store
            .source_embedding_ready_count(&project.id, "embed-small", 8, 2)
            .unwrap(),
        0
    );
    assert!(
        store
            .record_source_embedding_success(
                "source-gen",
                &chunks[0].id,
                1,
                2,
                "embed-small",
                8,
                "hash-two",
                &source_embedding_vector(8, 0.4),
            )
            .unwrap()
    );
    assert_eq!(
        store
            .source_embedding_ready_count(&project.id, "embed-small", 8, 2)
            .unwrap(),
        1
    );

    // A title edit changes the exact embedding text, so it advances too.
    let mut renamed = source.clone();
    renamed.title = "Runtime renamed".to_string();
    renamed.updated_at = now_rfc3339();
    store.update_source_metadata(&renamed).unwrap();
    assert_eq!(store.source_embedding_generation("source-gen").unwrap(), 3);
    assert_eq!(source_job(&store, "source-gen").unwrap().status, "pending");

    let (vectors, jobs) = store.clear_source_embeddings(&project.id).unwrap();
    assert_eq!(vectors, 1);
    assert_eq!(jobs, 1);
    assert_eq!(store.source_embedding_generation("source-gen").unwrap(), 0);
}

#[test]
fn source_embedding_jobs_retry_backoff_dead_letter_and_retention() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("sources"))
        .unwrap();
    store
        .set_source_embedding_policy(&project.id, &active_source_policy("embed-small"))
        .unwrap();
    let (source, chunks) = source_fixture(
        &project.id,
        "source-retry",
        "Retry",
        "Retry semantics are bounded and deterministic.",
    );
    store.insert_source(&source, &chunks).unwrap();
    store
        .mark_source_embedding_job_running("source-retry", 1)
        .unwrap();
    let future = "2099-01-01T00:00:00Z";
    store
        .record_source_embedding_failure("source-retry", 1, 1, "http_503", Some(future), false)
        .unwrap();
    let job = source_job(&store, "source-retry").unwrap();
    assert_eq!(job.status, "failed");
    assert_eq!(job.attempts, 1);
    assert_eq!(job.next_attempt_at.as_deref(), Some(future));
    assert!(
        store
            .due_source_embedding_jobs(
                &project.id,
                "embed-small",
                5,
                &now_rfc3339(),
                "2099-01-01T00:00:00Z",
                4
            )
            .unwrap()
            .is_empty(),
        "backoff protects the next tick"
    );
    assert_eq!(
        store
            .due_source_embedding_jobs(
                &project.id,
                "embed-small",
                5,
                "2100-01-01T00:00:00Z",
                "2099-01-01T00:00:00Z",
                4
            )
            .unwrap()
            .len(),
        1
    );
    store
        .record_source_embedding_failure("source-retry", 1, 5, "http_503", None, true)
        .unwrap();
    let job = source_job(&store, "source-retry").unwrap();
    assert_eq!(job.status, "dead");
    assert_eq!(job.attempts, 5);
    assert!(job.next_attempt_at.is_none());
    assert!(
        store
            .due_source_embedding_jobs(
                &project.id,
                "embed-small",
                5,
                "2100-01-01T00:00:00Z",
                "2099-01-01T00:00:00Z",
                4
            )
            .unwrap()
            .is_empty()
    );

    // Retention prunes dead metadata and stale vectors only after the cutoff.
    let conn = store.lock().unwrap();
    conn.execute(
        "UPDATE source_embedding_jobs SET updated_at = '2000-01-01T00:00:00Z'",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE source_embedding_state SET generation = 2 WHERE source_id = 'source-retry'",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO source_embeddings
         (chunk_id, project_id, source_id, chunk_no, generation, model, dimensions,
          content_hash, vector, status, created_at, updated_at)
         VALUES ('old-chunk', ?1, 'source-retry', 1, 1, 'embed-small', 8, 'old-hash',
                 x'00', 'stale', '2000-01-01T00:00:00Z', '2000-01-01T00:00:00Z')",
        params![project.id],
    )
    .unwrap();
    drop(conn);
    let (vectors, jobs) = store
        .sweep_source_embedding_retention(&project.id, 1)
        .unwrap();
    assert_eq!(vectors, 1);
    assert_eq!(jobs, 1);
    assert!(source_job(&store, "source-retry").is_none());
    assert_eq!(store.source_embedding_stale_count(&project.id).unwrap(), 0);
}

#[test]
fn source_semantic_candidates_are_bounded_filtered_and_structural() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("sources"))
        .unwrap();
    store
        .set_source_embedding_policy(&project.id, &active_source_policy("embed-small"))
        .unwrap();
    let (mut first, first_chunks) = source_fixture(
        &project.id,
        "source-a",
        "Alpha",
        "Deterministic leases and integration.",
    );
    first.collection = Some("engineering".to_string());
    first.labels = vec!["runtime".to_string()];
    let (mut second, second_chunks) = source_fixture(
        &project.id,
        "source-b",
        "Beta",
        "Retrieval citations and evidence.",
    );
    second.collection = Some("research".to_string());
    second.labels = vec!["citation".to_string()];
    store.insert_source(&first, &first_chunks).unwrap();
    store.insert_source(&second, &second_chunks).unwrap();
    for (source, chunks) in [(&first, &first_chunks), (&second, &second_chunks)] {
        let generation = store.source_embedding_generation(&source.id).unwrap();
        store
            .record_source_embedding_success(
                &source.id,
                &chunks[0].id,
                1,
                generation,
                "embed-small",
                8,
                "hash",
                &source_embedding_vector(8, 0.1),
            )
            .unwrap();
        store
            .record_source_embedding_job_done(&source.id, generation, 1)
            .unwrap();
    }
    let all = store
        .source_semantic_candidates(&project.id, "embed-small", 8, 200, &[], &[], &[], 512)
        .unwrap();
    assert_eq!(all.len(), 2);
    let engineering = store
        .source_semantic_candidates(
            &project.id,
            "embed-small",
            8,
            200,
            &[],
            &["engineering".to_string()],
            &[],
            512,
        )
        .unwrap();
    assert_eq!(engineering.len(), 1);
    assert_eq!(engineering[0].source_id, "source-a");
    let labels = store
        .source_semantic_candidates(
            &project.id,
            "embed-small",
            8,
            200,
            &[],
            &[],
            &["citation".to_string()],
            512,
        )
        .unwrap();
    assert_eq!(labels.len(), 1);
    assert_eq!(labels[0].source_id, "source-b");
    let source_scoped = store
        .source_semantic_candidates(
            &project.id,
            "embed-small",
            8,
            200,
            &["source-a".to_string()],
            &[],
            &[],
            512,
        )
        .unwrap();
    assert_eq!(source_scoped.len(), 1);
    let capped = store
        .source_semantic_candidates(&project.id, "embed-small", 8, 200, &[], &[], &[], 1)
        .unwrap();
    assert_eq!(capped.len(), 1);

    // A hand-corrupted header is counted as invalid and never returned.
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE source_embeddings SET vector = x'00' WHERE source_id = 'source-a'",
            [],
        )
        .unwrap();
    }
    let integrity = store
        .source_embedding_integrity(&project.id, "embed-small", 8)
        .unwrap();
    assert_eq!(integrity.valid_vectors, 1);
    assert_eq!(integrity.invalid_vectors, 1);
    let all = store
        .source_semantic_candidates(&project.id, "embed-small", 8, 200, &[], &[], &[], 512)
        .unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].source_id, "source-b");

    let status = store
        .source_embedding_status(&project.id, Some(&active_source_policy("embed-small")))
        .unwrap();
    assert_eq!(status.indexable_chunks, 2);
    assert_eq!(status.indexed_chunks, 1);
    assert_eq!(status.invalid_chunks, 1);
    assert_eq!(status.done_jobs, 2);
    assert_eq!(status.coverage, 0.5);
    let encoded = serde_json::to_value(&status).unwrap();
    assert!(encoded.get("vectors").is_none());
    assert!(encoded.get("endpoint").is_none());
}

#[test]
fn source_embedding_policy_manifest_and_backlog_round_trip() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("sources"))
        .unwrap();
    let policy = active_source_policy("embed-manifest");
    store
        .set_source_embedding_policy(&project.id, &policy)
        .unwrap();
    store.record_project_manifest().unwrap();
    let snapshots = crate::read_project_manifest(data.path()).unwrap();
    assert_eq!(snapshots.len(), 1);
    assert_eq!(snapshots[0].source_embedding_policy, policy);
    assert!(
        store
            .list_enabled_source_embedding_policies()
            .unwrap()
            .len()
            == 1
    );

    let (source, chunks) = source_fixture(
        &project.id,
        "source-backlog",
        "Backlog",
        "Backlog source content.",
    );
    store.insert_source(&source, &chunks).unwrap();
    let generation = store.source_embedding_generation("source-backlog").unwrap();
    let enqueued = store
        .enqueue_source_embedding_backlog(&project.id, "embed-manifest", 200)
        .unwrap();
    assert_eq!(enqueued, 1);
    let job = source_job(&store, "source-backlog").unwrap();
    assert_eq!(job.generation, generation + 1);
    assert_eq!(job.status, "pending");
    assert_eq!(job.chunks_done, 0);

    // A malformed stored policy fails closed as disabled instead of enabling
    // an unreviewed endpoint variable.
    {
        let conn = store.lock().unwrap();
        conn.execute(
            "UPDATE project_source_embedding_policies SET policy = '{not-json' WHERE project_id = ?1",
            params![project.id],
        )
        .unwrap();
    }
    let recovered = store.get_source_embedding_policy(&project.id).unwrap();
    assert!(!recovered.enabled);
    assert!(!recovered.is_configured());
    assert!(
        store
            .list_enabled_source_embedding_policies()
            .unwrap()
            .is_empty()
    );
}

#[test]
fn source_embedding_jobs_survive_source_delete_cascade() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("sources"))
        .unwrap();
    store
        .set_source_embedding_policy(&project.id, &active_source_policy("embed-small"))
        .unwrap();
    let (source, chunks) = source_fixture(
        &project.id,
        "source-delete",
        "Delete",
        "A source slated for deletion.",
    );
    store.insert_source(&source, &chunks).unwrap();
    let generation = store.source_embedding_generation("source-delete").unwrap();
    store
        .record_source_embedding_success(
            "source-delete",
            &chunks[0].id,
            1,
            generation,
            "embed-small",
            8,
            "hash",
            &source_embedding_vector(8, 0.3),
        )
        .unwrap();
    assert!(store.delete_source("source-delete").unwrap());
    assert!(source_job(&store, "source-delete").is_none());
    assert_eq!(
        store.source_embedding_generation("source-delete").unwrap(),
        0
    );
    let status = store
        .source_embedding_status(&project.id, Some(&active_source_policy("embed-small")))
        .unwrap();
    assert_eq!(status.indexed_chunks, 0);
    assert_eq!(status.pending_jobs, 0);
    assert_eq!(status.stale_chunks, 0);
}

#[test]
fn source_embedding_usage_and_ready_counts_are_model_scoped() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("sources"))
        .unwrap();
    let policy = active_source_policy("embed-small");
    store
        .set_source_embedding_policy(&project.id, &policy)
        .unwrap();
    let (source, chunks) = source_fixture(
        &project.id,
        "source-usage",
        "Usage",
        "Usage accounting is separate from memory embedding usage.",
    );
    store.insert_source(&source, &chunks).unwrap();
    let generation = store.source_embedding_generation(&source.id).unwrap();
    store
        .record_source_embedding_success(
            &source.id,
            &chunks[0].id,
            1,
            generation,
            "embed-small",
            8,
            "hash",
            &source_embedding_vector(8, 0.5),
        )
        .unwrap();
    store
        .record_source_embedding_job_done(&source.id, generation, 1)
        .unwrap();
    store
        .record_source_embedding_usage(&project.id, "embed-small", 2, 40, 10, 160, true)
        .unwrap();
    store
        .record_source_embedding_usage(&project.id, "embed-small", 1, 20, 20, 80, false)
        .unwrap();
    let status = store
        .source_embedding_status(&project.id, Some(&policy))
        .unwrap();
    assert_eq!(status.usage.calls, 3);
    assert_eq!(status.usage.input_tokens, 60);
    assert_eq!(status.usage.estimated_tokens, 30);
    assert_eq!(status.usage.bytes, 240);
    assert_eq!(status.usage.provider_reported_calls, 1);
    assert_eq!(status.done_jobs, 1);
    assert_eq!(status.indexed_chunks, 1);
    assert_eq!(status.coverage, 1.0);
    assert!(status.last_error.is_none());

    // Ready counts are scoped by model, dimensions, and the chunk cap.
    assert_eq!(
        store
            .source_embedding_ready_count(&project.id, "embed-small", 8, 200)
            .unwrap(),
        1
    );
    assert_eq!(
        store
            .source_embedding_ready_count(&project.id, "other-model", 8, 200)
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .source_embedding_ready_count(&project.id, "embed-small", 16, 200)
            .unwrap(),
        0
    );
    assert_eq!(
        store
            .source_embedding_ready_count(&project.id, "embed-small", 8, 0)
            .unwrap(),
        1,
        "a zero cap clamps to one chunk and chunk 1 is still counted"
    );
    assert_eq!(
        store.source_embedding_chunks(&source.id, 0).unwrap().len(),
        1
    );
    assert!(
        store.source_embedding_chunks(&source.id, 1).unwrap()[0]
            .vector_status
            .as_deref()
            .is_some()
    );
    assert_eq!(
        store
            .source_semantic_candidates(&project.id, "embed-small", 8, 200, &[], &[], &[], 0)
            .unwrap()
            .len(),
        1,
        "limit clamps to at least one"
    );
}

fn supervised_row(session: &str, root: &std::path::Path, id: &str) -> SupervisedProcess {
    SupervisedProcess {
        id: id.to_string(),
        session_id: session.to_string(),
        name: "test".to_string(),
        command: "sleep 1".to_string(),
        argv: vec!["/bin/sh".into(), "-c".into(), "sleep 1".into()],
        cwd: root.to_string_lossy().to_string(),
        worktree: root.to_string_lossy().to_string(),
        env_hash: "env".to_string(),
        command_hash: "command".to_string(),
        state: "starting".to_string(),
        pid: None,
        pgid: None,
        nonce: "nonce".to_string(),
        started_at: now_rfc3339(),
        process_started_at: None,
        heartbeat_at: now_rfc3339(),
        finished_at: None,
        exit_code: None,
        signal: None,
        output_path: String::new(),
        output_bytes: 0,
        output_lines: 0,
        total_output_bytes: 0,
        output_rotations: 0,
        max_output_bytes: 1_048_576,
        max_output_lines: 20_000,
        owner_instance: "instance:1".to_string(),
        retention_deadline: None,
        idle_ttl_secs: 3_600,
        last_access_at: now_rfc3339(),
        detail: None,
    }
}

#[test]
fn supervised_processes_round_trip_with_lifecycle_events() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("processes"))
        .unwrap();
    let session = store.create_session(&project.id, "supervise").unwrap();
    let mut row = supervised_row(&session.id, repo.path(), "psup_store");
    let event = store
        .insert_supervised_process_with_event(&session.id, &row, "process.starting")
        .unwrap();
    assert_eq!(event.kind, "process.starting");
    assert_eq!(
        store
            .count_active_supervised_processes(&session.id)
            .unwrap(),
        1
    );
    let stored = store
        .get_supervised_process(&session.id, "psup_store")
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, "starting");
    assert_eq!(stored.max_output_bytes, 1_048_576);

    row.state = "running".to_string();
    row.pid = Some(4242);
    row.pgid = Some(4242);
    row.process_started_at = Some("Wed Nov 11 00:00:00 2026".to_string());
    let event = store
        .update_supervised_process_with_event(&row, "process.started")
        .unwrap();
    assert_eq!(event.kind, "process.started");
    let stored = store
        .get_supervised_process(&session.id, "psup_store")
        .unwrap()
        .unwrap();
    assert_eq!(stored.state, "running");
    assert_eq!(stored.pid, Some(4242));

    row.output_bytes = 512;
    row.output_lines = 4;
    row.total_output_bytes = 700;
    store.record_supervised_process_telemetry(&row).unwrap();
    let stored = store
        .get_supervised_process(&session.id, "psup_store")
        .unwrap()
        .unwrap();
    assert_eq!(stored.output_bytes, 512);
    assert_eq!(stored.total_output_bytes, 700);

    let list = store
        .list_supervised_processes(&session.id, true, 10)
        .unwrap();
    assert_eq!(list.len(), 1);
    let live = store
        .list_supervised_processes(&session.id, false, 10)
        .unwrap();
    assert_eq!(live.len(), 1);
    assert_eq!(store.supervised_process_output_bytes().unwrap(), 512);
    let counts = store.supervised_process_counts().unwrap();
    assert!(
        counts
            .iter()
            .any(|entry| entry.key == "running" && entry.count == 1)
    );
}

#[test]
fn supervised_process_replacement_and_pruning_are_bounded() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("processes"))
        .unwrap();
    let session = store.create_session(&project.id, "supervise").unwrap();
    let mut row = supervised_row(&session.id, repo.path(), "psup_prune");
    store
        .insert_supervised_process_with_event(&session.id, &row, "process.starting")
        .unwrap();
    let error = store
        .prune_supervised_process_with_event(&session.id, "psup_prune", "retention")
        .unwrap_err();
    assert!(error.to_string().contains("refusing to prune"));
    row.state = "stopped".to_string();
    row.pid = Some(10);
    row.pgid = Some(10);
    row.signal = Some(15);
    store
        .update_supervised_process_with_event(&row, "process.stopped")
        .unwrap();
    let pruned = store
        .prune_supervised_process_with_event(&session.id, "psup_prune", "retention")
        .unwrap()
        .unwrap();
    assert_eq!(pruned.kind, "process.pruned");
    assert!(
        store
            .get_supervised_process(&session.id, "psup_prune")
            .unwrap()
            .is_none()
    );

    let replacement = vec![{
        let mut next = supervised_row(&session.id, repo.path(), "psup_replacement");
        next.state = "unknown".to_string();
        next
    }];
    store
        .replace_supervised_processes(&session.id, &replacement)
        .unwrap();
    let rows = store
        .list_supervised_processes(&session.id, true, 10)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "psup_replacement");
    assert_eq!(
        store
            .count_active_supervised_processes(&session.id)
            .unwrap(),
        1
    );
}

#[test]
fn supervised_process_rows_validate_caps() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("processes"))
        .unwrap();
    let session = store.create_session(&project.id, "supervise").unwrap();
    let mut row = supervised_row(&session.id, repo.path(), "psup_bad");
    row.max_output_bytes = 1;
    let error = store
        .insert_supervised_process_with_event(&session.id, &row, "process.starting")
        .unwrap_err();
    assert!(error.to_string().contains("max_output_bytes"));
    row.max_output_bytes = 4_096;
    row.state = "bogus".to_string();
    let error = store
        .insert_supervised_process_with_event(&session.id, &row, "process.starting")
        .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("invalid supervised process state")
    );
}

#[test]
fn bundle_import_preserves_supervised_process_lifecycle_rows() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("bundle"))
        .unwrap();
    let session = store.create_session(&project.id, "bundle process").unwrap();
    let mut row = supervised_row(&session.id, repo.path(), "psup_bundle");
    row.state = "stopped".to_string();
    row.pid = Some(77);
    row.pgid = Some(77);
    row.signal = Some(15);
    row.exit_code = Some(0);
    row.output_path = "/tmp/archive-output.log".to_string();
    row.finished_at = Some(now_rfc3339());
    row.retention_deadline = Some("2099-01-01T00:00:00Z".to_string());
    let event = Event {
        session_id: session.id.clone(),
        seq: 1,
        kind: "process.stopped".to_string(),
        payload: serde_json::to_value(&row).unwrap(),
        created_at: now_rfc3339(),
    };
    let bundle = SessionBundle {
        version: 1,
        exported_at: now_rfc3339(),
        project: project.clone(),
        session: session.clone(),
        events: vec![event],
        agents: Vec::new(),
        messages: Vec::new(),
        team_plan: None,
        todos: Vec::new(),
        approvals: Vec::new(),
        merge_queue: Vec::new(),
        agent_messages: Vec::new(),
        artifacts: Vec::new(),
        usage: Vec::new(),
        memories: Vec::new(),
        memory_conflicts: Vec::new(),
        questions: Vec::new(),
        tasks: Vec::new(),
        steering: Vec::new(),
        continuations: Vec::new(),
        recoveries: Vec::new(),
        programs: Vec::new(),
        program_milestones: Vec::new(),
        program_events: Vec::new(),
        session_runs: Vec::new(),
        run_steps: Vec::new(),
        run_checkpoints: Vec::new(),
        context_checkpoints: Vec::new(),
        supervised_processes: vec![row.clone()],
        test_results: Vec::new(),
    };
    validate_session_bundle(&bundle).unwrap();
    let data = TempDir::new().unwrap();
    let imported = Store::open(data.path()).unwrap();
    imported.import_session_bundle(&bundle).unwrap();
    let imported_session = imported.get_session(&session.id).unwrap().unwrap();
    assert_eq!(imported_session.status, "imported");
    let rows = imported
        .list_supervised_processes(&session.id, true, 10)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "psup_bundle");
    assert_eq!(rows[0].state, "stopped");
    assert_eq!(rows[0].exit_code, Some(0));
    // Imported rows keep their archived path; the daemon refuses to read a
    // path outside its own managed root. The store never rewrites it.
    assert_eq!(rows[0].output_path, "/tmp/archive-output.log");
}

#[test]
fn supervised_process_bundle_validation_rejects_wrong_session() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("bundle"))
        .unwrap();
    let session = store.create_session(&project.id, "bundle process").unwrap();
    let other = store.create_session(&project.id, "other").unwrap();
    let row = supervised_row(&other.id, repo.path(), "psup_wrong");
    let bundle = SessionBundle {
        version: 1,
        exported_at: now_rfc3339(),
        project,
        session,
        events: Vec::new(),
        agents: Vec::new(),
        messages: Vec::new(),
        team_plan: None,
        todos: Vec::new(),
        approvals: Vec::new(),
        merge_queue: Vec::new(),
        agent_messages: Vec::new(),
        artifacts: Vec::new(),
        usage: Vec::new(),
        memories: Vec::new(),
        memory_conflicts: Vec::new(),
        questions: Vec::new(),
        tasks: Vec::new(),
        steering: Vec::new(),
        continuations: Vec::new(),
        recoveries: Vec::new(),
        programs: Vec::new(),
        program_milestones: Vec::new(),
        program_events: Vec::new(),
        session_runs: Vec::new(),
        run_steps: Vec::new(),
        run_checkpoints: Vec::new(),
        context_checkpoints: Vec::new(),
        supervised_processes: vec![row],
        test_results: Vec::new(),
    };
    let error = validate_session_bundle(&bundle).unwrap_err();
    assert!(error.to_string().contains("belongs to session"), "{error}");
}

#[test]
fn process_policy_round_trips_and_validates() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("process policy"))
        .unwrap();
    let default = store.get_process_policy(&project.id).unwrap();
    assert_eq!(default, ProcessPolicy::default());
    assert!(default.idle_sweep_enabled);
    assert_eq!(default.max_processes, 8);
    assert_eq!(default.idle_ttl_secs, 3_600);

    let policy = ProcessPolicy {
        idle_sweep_enabled: false,
        max_processes: 3,
        idle_ttl_secs: 900,
        max_output_bytes: 65_536,
        max_output_lines: 2_000,
    };
    store.set_process_policy(&project.id, &policy).unwrap();
    assert_eq!(store.get_process_policy(&project.id).unwrap(), policy);
    store.record_project_manifest().unwrap();
    let snapshot = crate::read_project_manifest(store.data_dir()).unwrap();
    let project_snapshot = snapshot
        .into_iter()
        .find(|snapshot| snapshot.project.id == project.id)
        .unwrap();
    assert_eq!(project_snapshot.process_policy, policy);

    let invalid = ProcessPolicy {
        max_processes: 0,
        ..ProcessPolicy::default()
    };
    assert!(store.set_process_policy(&project.id, &invalid).is_err());
    assert_eq!(store.get_process_policy(&project.id).unwrap(), policy);
}

#[test]
fn process_policy_manifest_defaults_for_older_projects() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("legacy"))
        .unwrap();
    let encoded = serde_json::to_value({
        store.record_project_manifest().unwrap();
        crate::read_project_manifest(store.data_dir())
            .unwrap()
            .into_iter()
            .find(|snapshot| snapshot.project.id == project.id)
            .unwrap()
    })
    .unwrap();
    let mut value = encoded;
    value.as_object_mut().unwrap().remove("process_policy");
    let decoded: ProjectManifestSnapshot = serde_json::from_value(value).unwrap();
    assert_eq!(decoded.process_policy, ProcessPolicy::default());
}

#[test]
fn scaling_policy_defaults_disabled_and_round_trips() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("scale"))
        .unwrap();
    assert!(!store.has_scaling_policy(&project.id).unwrap());
    assert!(!store.get_scaling_policy(&project.id).unwrap().enabled);

    let policy = ScalingPolicy {
        enabled: true,
        min_workers: 2,
        max_workers: 6,
        min_labour: 3,
        max_labour: 12,
        expand_ready_threshold: 4,
        expand_critical_delay_secs: 30,
        scale_down_idle_secs: 900,
        cooldown_secs: 45,
        max_step: 3,
        budget_headroom_ratio: Some(0.25),
    };
    store.set_scaling_policy(&project.id, &policy).unwrap();
    assert!(store.has_scaling_policy(&project.id).unwrap());
    assert_eq!(store.get_scaling_policy(&project.id).unwrap(), policy);

    // Stored values are normalized even if a caller bypasses validation.
    let clamped = ScalingPolicy {
        min_workers: 0,
        max_workers: 99,
        max_step: 99,
        ..policy
    };
    store.set_scaling_policy(&project.id, &clamped).unwrap();
    let read = store.get_scaling_policy(&project.id).unwrap();
    assert_eq!(read.min_workers, 1);
    assert_eq!(read.max_workers, 10);
    assert_eq!(read.max_step, 10);
}

#[test]
fn scaling_decisions_are_idempotent_and_rebuildable() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("scale"))
        .unwrap();
    let session = store.create_session(&project.id, "decide").unwrap();
    store
        .append_event(
            &session.id,
            "note",
            serde_json::json!({"text": "before scaling"}),
        )
        .unwrap();
    let decision = ScalingDecision {
        decision_id: String::new(),
        decision_key: "rev1:tick1".into(),
        session_id: session.id.clone(),
        project_id: project.id.clone(),
        plan_revision: 1,
        action: vp_protocol::scaling::ScalingAction::ExpandWorkers,
        current_workers: 2,
        current_labour: 4,
        target_workers: 4,
        target_labour: 4,
        active_workers: 2,
        active_labour: 2,
        ready_tasks: 3,
        running_tasks: 2,
        inputs_hash: "abc".into(),
        reasons: vec!["expand_workers:2->4".into()],
        manager_recommended: false,
        manager_evidence: None,
        applied: true,
        idle_since_workers: None,
        idle_since_labour: None,
        created_at: "2026-11-11T00:00:00Z".into(),
    };
    let (stored, event) = store
        .record_scaling_decision_with_event(&decision)
        .unwrap()
        .expect("insert");
    assert_eq!(event.kind, "scaling.decision");
    assert_eq!(event.payload["decision_id"], stored.decision_id);
    assert!(!stored.decision_id.is_empty());
    assert!(
        store
            .record_scaling_decision_with_event(&decision)
            .unwrap()
            .is_none()
    );
    let decisions = store.list_scaling_decisions(&session.id, 10).unwrap();
    assert_eq!(decisions.len(), 1);
    assert_eq!(
        decisions[0].action,
        vp_protocol::scaling::ScalingAction::ExpandWorkers
    );
    assert_eq!(
        store
            .latest_scaling_decision(&session.id)
            .unwrap()
            .unwrap()
            .decision_id,
        stored.decision_id
    );

    let events = store.list_all_events(&session.id).unwrap();
    store
        .replace_scaling_decisions(&session.id, &[], &events)
        .unwrap();
    assert!(
        store
            .list_scaling_decisions(&session.id, 10)
            .unwrap()
            .is_empty()
    );
    store
        .replace_scaling_decisions(&session.id, &decisions, &events)
        .unwrap();
    assert_eq!(
        store.list_scaling_decisions(&session.id, 10).unwrap(),
        decisions
    );
}

#[test]
fn critical_path_policy_round_trips_and_normalizes() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("governor"))
        .unwrap();
    assert_eq!(
        store.get_critical_path_policy(&project.id).unwrap(),
        CriticalPathPolicy::default()
    );
    assert!(!store.has_critical_path_policy(&project.id).unwrap());
    let stored = CriticalPathPolicy {
        enabled: true,
        reserve_ratio: 3.0,
        throttle_at_ratio: -1.0,
        critical_boost: 500,
        min_task_budget: u64::MAX,
        ..CriticalPathPolicy::default()
    };
    store
        .set_critical_path_policy(&project.id, &stored)
        .unwrap();
    assert!(store.has_critical_path_policy(&project.id).unwrap());
    let loaded = store.get_critical_path_policy(&project.id).unwrap();
    assert!(loaded.enabled);
    assert_eq!(loaded.reserve_ratio, 1.0);
    assert_eq!(loaded.throttle_at_ratio, 0.0);
    assert_eq!(
        loaded.critical_boost,
        CriticalPathPolicy::MAX_CRITICAL_BOOST
    );
    assert_eq!(
        loaded.min_task_budget,
        CriticalPathPolicy::MAX_MIN_TASK_BUDGET
    );
}

#[test]
fn critical_path_policy_round_trips_multi_path_fairness_fields() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("fairness"))
        .unwrap();
    let policy = CriticalPathPolicy {
        enabled: true,
        mode: vp_protocol::types::CriticalPathMode::Multi,
        max_paths: 99,
        department_min_ratio: 0.35,
        observed_weight_ratio: 0.5,
        starvation_after_secs: 8 * 86_400,
        ..CriticalPathPolicy::default()
    };
    store
        .set_critical_path_policy(&project.id, &policy)
        .unwrap();
    let loaded = store.get_critical_path_policy(&project.id).unwrap();
    assert_eq!(loaded.mode, vp_protocol::types::CriticalPathMode::Multi);
    assert_eq!(loaded.max_paths, CriticalPathPolicy::MAX_MAX_PATHS);
    assert_eq!(loaded.department_min_ratio, 0.35);
    assert_eq!(loaded.observed_weight_ratio, 0.5);
    assert_eq!(
        loaded.starvation_after_secs,
        CriticalPathPolicy::MAX_STARVATION_AFTER_SECS
    );
}

#[test]
fn usage_tokens_by_task_sums_input_and_output_and_skips_missing_ids() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("usage"))
        .unwrap();
    let session = store
        .create_session_with_limits(&project.id, "usage", Some(100_000), None, None)
        .unwrap();
    for (task_id, input, output) in [
        (Some("a"), 100u64, 50u64),
        (Some("a"), 25, 5),
        (Some("b"), 10, 0),
        (None, 999, 1),
    ] {
        store
            .add_usage_entry_with_events(
                &session.id,
                "agent",
                "Agent",
                "worker",
                task_id,
                Some("impl"),
                "test",
                input,
                output,
                0,
                0,
                false,
                0.0,
            )
            .unwrap();
    }
    let tokens = store.usage_tokens_by_task(&session.id).unwrap();
    assert_eq!(tokens.get("a"), Some(&180));
    assert_eq!(tokens.get("b"), Some(&10));
    assert_eq!(tokens.len(), 2);
    assert!(!tokens.contains_key(""));
}

#[test]
fn critical_path_policy_survives_project_manifest_rebuild() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("governor manifest"))
        .unwrap();
    let policy = CriticalPathPolicy {
        enabled: true,
        reserve_ratio: 0.4,
        throttle_at_ratio: 0.2,
        critical_boost: 10,
        min_task_budget: 2_000,
        ..CriticalPathPolicy::default()
    };
    store
        .set_critical_path_policy(&project.id, &policy)
        .unwrap();
    store.record_project_manifest().unwrap();
    drop(store);
    let reopened = Store::open(data.path()).unwrap();
    assert_eq!(
        reopened.get_critical_path_policy(&project.id).unwrap(),
        policy
    );
    let snapshots = read_project_manifest(data.path()).unwrap();
    let snapshot = snapshots
        .iter()
        .find(|snapshot| snapshot.project.id == project.id)
        .unwrap();
    assert_eq!(snapshot.critical_path_policy, policy);
}

#[test]
fn legacy_manifest_without_critical_path_policy_decodes_disabled() {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("legacy governor"))
        .unwrap();
    store.record_project_manifest().unwrap();
    let raw = std::fs::read_to_string(data.path().join("projects.jsonl")).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(raw.lines().next().unwrap()).unwrap();
    value
        .as_object_mut()
        .unwrap()
        .remove("critical_path_policy");
    let snapshot: ProjectManifestSnapshot = serde_json::from_value(value).unwrap();
    assert_eq!(snapshot.critical_path_policy, CriticalPathPolicy::default());
    assert!(!snapshot.critical_path_policy.enabled);
    assert_eq!(snapshot.project.id, project.id);
}

#[test]
fn remote_merge_queue_rows_persist_provenance_and_filter_by_project() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("remote-queue"))
        .unwrap();
    let session = store.create_session(&project.id, "remote queue").unwrap();
    let source = MergeQueueSource::remote(
        "build.example.com",
        "feature/remote",
        "a".repeat(64),
        "refs/verlinpark/remote/session/import-1",
    );
    let (entry, event) = store
        .enqueue_merge_with_event_source(
            &session.id,
            "agent_remote",
            "Remote Worker",
            Some("T1"),
            "verlinpark/session--remote-import-1",
            "abcdef1234567890",
            "1234567890abcdef",
            Some("verified locally"),
            &source,
        )
        .unwrap();
    assert_eq!(entry.source, "remote");
    assert_eq!(entry.remote_host.as_deref(), Some("build.example.com"));
    assert_eq!(entry.remote_branch.as_deref(), Some("feature/remote"));
    assert_eq!(
        entry.bundle_sha256.as_deref(),
        Some("a".repeat(64).as_str())
    );
    assert_eq!(
        entry.imported_ref.as_deref(),
        Some("refs/verlinpark/remote/session/import-1")
    );
    assert_eq!(event.kind, "merge_queue.enqueued");
    assert_eq!(event.payload["entry"]["source"], "remote");
    assert_eq!(event.payload["entry"]["bundle_sha256"], "a".repeat(64));
    let (verified, verification_event) = store
        .set_merge_queue_verification_with_event(
            &entry.id,
            Some(true),
            Some("approved remotely"),
            Some("local only"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(verified.verifier_approved, Some(true));
    assert_eq!(
        verified.verifier_summary.as_deref(),
        Some("approved remotely")
    );
    assert_eq!(verified.environment_caveat.as_deref(), Some("local only"));
    assert_eq!(verification_event.kind, "merge_queue.updated");
    assert_eq!(
        verification_event.payload["entry"]["verifier_approved"],
        true
    );
    let listed = store.list_remote_merge_queue(&project.id, None).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, entry.id);
    assert!(
        store
            .list_remote_merge_queue(&project.id, Some("other-session"))
            .unwrap()
            .is_empty()
    );

    let local = store
        .enqueue_merge(
            &session.id,
            "agent_local",
            "Local Worker",
            None,
            "verlinpark/local",
            "fedcba0987654321",
            "1234567890abcdef",
            None,
        )
        .unwrap();
    assert_eq!(local.source, "local");
    assert!(local.remote_host.is_none());
    let remote_only = store.list_remote_merge_queue(&project.id, None).unwrap();
    assert_eq!(remote_only.len(), 1);
    assert_eq!(remote_only[0].id, entry.id);

    store
        .update_merge_queue_entry(&entry.id, "conflict", Some("retry me"))
        .unwrap()
        .unwrap();
    let retried = store
        .retry_merge_queue_entry(
            &entry.id,
            "fedcba0987654321",
            "1234567890abcdef",
            Some("rebased and re-verified"),
        )
        .unwrap()
        .unwrap();
    assert_eq!(retried.status, "queued");
    assert_eq!(retried.source, "remote");
    assert_eq!(retried.remote_host, entry.remote_host);
    assert_eq!(retried.imported_ref, entry.imported_ref);
    assert_eq!(retried.attempt, 2);
    assert_eq!(retried.commit, "fedcba0987654321");
}

#[test]
fn remote_integration_policy_persists_and_decodes_as_disabled() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("remote-policy"))
        .unwrap();
    assert_eq!(
        store.get_remote_integration_policy(&project.id).unwrap(),
        RemoteIntegrationPolicy::default()
    );
    assert!(
        store
            .remote_integration_policy_updated_at(&project.id)
            .unwrap()
            .is_none()
    );
    let policy = RemoteIntegrationPolicy {
        enabled: true,
        max_bundle_bytes: 1024 * 1024,
        require_verifier: false,
        allowed_hosts: vec!["build.example.com".into(), "agent@other.example.com".into()],
        keep_import_refs: true,
    };
    store
        .set_remote_integration_policy(&project.id, &policy)
        .unwrap();
    assert_eq!(
        store.get_remote_integration_policy(&project.id).unwrap(),
        policy
    );
    assert!(
        store
            .remote_integration_policy_updated_at(&project.id)
            .unwrap()
            .is_some()
    );
    store.record_project_manifest().unwrap();
    let snapshot = crate::read_project_manifest(store.data_dir())
        .unwrap()
        .into_iter()
        .find(|snapshot| snapshot.project.id == project.id)
        .unwrap();
    assert_eq!(snapshot.remote_integration_policy, policy);
    let legacy = serde_json::from_value::<crate::ProjectManifestSnapshot>(serde_json::json!({
        "version": 1,
        "recorded_at": "2026-11-11T00:00:00Z",
        "project": snapshot.project,
        "shell_policy": vp_protocol::types::ShellPolicy::default(),
        "integration_policy": vp_protocol::types::IntegrationPolicy::default(),
        "artifact_policy": vp_protocol::types::ArtifactPolicy::default(),
        "mailbox_policy": vp_protocol::types::MailboxPolicy::default(),
        "worktree_policy": vp_protocol::types::WorktreeRetentionPolicy::default(),
        "budget": vp_protocol::types::ProjectBudget::default()
    }))
    .unwrap();
    assert_eq!(
        legacy.remote_integration_policy,
        RemoteIntegrationPolicy::default()
    );
    let invalid = RemoteIntegrationPolicy {
        enabled: true,
        allowed_hosts: Vec::new(),
        ..policy
    };
    assert!(
        store
            .set_remote_integration_policy(&project.id, &invalid)
            .is_err()
    );
}

#[test]
fn test_cache_policy_round_trips_manifest_and_legacy_defaults() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("test cache"))
        .unwrap();
    let default = store.get_test_cache_policy(&project.id).unwrap();
    assert!(default.enabled);
    assert!(!default.include_integration);
    assert_eq!(default, vp_protocol::test_cache::TestCachePolicy::default());
    let policy = vp_protocol::test_cache::TestCachePolicy {
        enabled: true,
        max_entries: 9,
        retention_days: 2,
        max_output_bytes: 32_768,
        reuse_lint: false,
        include_integration: true,
        failed_retention_secs: 180,
    };
    store.set_test_cache_policy(&project.id, &policy).unwrap();
    assert_eq!(store.get_test_cache_policy(&project.id).unwrap(), policy);
    store.record_project_manifest().unwrap();
    let snapshot = crate::read_project_manifest(store.data_dir()).unwrap();
    let snapshot = snapshot
        .into_iter()
        .find(|snapshot| snapshot.project.id == project.id)
        .unwrap();
    assert_eq!(snapshot.test_cache_policy, policy);

    let legacy = serde_json::from_value::<crate::ProjectManifestSnapshot>(serde_json::json!({
        "version": 1,
        "recorded_at": "2026-11-11T00:00:00Z",
        "project": snapshot.project,
        "shell_policy": vp_protocol::types::ShellPolicy::default(),
        "integration_policy": vp_protocol::types::IntegrationPolicy::default(),
        "artifact_policy": vp_protocol::types::ArtifactPolicy::default(),
        "mailbox_policy": vp_protocol::types::MailboxPolicy::default(),
        "worktree_policy": vp_protocol::types::WorktreeRetentionPolicy::default(),
        "budget": vp_protocol::types::ProjectBudget::default()
    }))
    .unwrap();
    assert_eq!(
        legacy.test_cache_policy,
        vp_protocol::test_cache::TestCachePolicy::default()
    );

    // Stored values are normalized even if a caller bypasses strict validation.
    let oversized = vp_protocol::test_cache::TestCachePolicy {
        max_entries: u32::MAX,
        ..policy
    };
    store
        .set_test_cache_policy(&project.id, &oversized)
        .unwrap();
    assert_eq!(
        store
            .get_test_cache_policy(&project.id)
            .unwrap()
            .max_entries,
        vp_protocol::test_cache::HARD_MAX_TEST_CACHE_ENTRIES
    );
}

#[test]
fn test_cache_clear_chunks_prune_events_and_removes_every_row() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("cache clear"))
        .unwrap();
    let session = store.create_session(&project.id, "clear").unwrap();
    let now = now_rfc3339();
    for index in 0..600 {
        let entry = vp_protocol::test_cache::TestCacheEntry {
            id: format!("tr-clear-{index}"),
            project_id: project.id.clone(),
            session_id: session.id.clone(),
            kind: "test".into(),
            command: format!("cargo test {index}"),
            command_source: "Cargo.toml".into(),
            key_version: vp_protocol::test_cache::TEST_CACHE_KEY_VERSION,
            cache_key: format!("v1:{index:064x}"),
            inputs_hash: format!("inputs-{index}"),
            impact_generation: "impact".into(),
            status: "passed".into(),
            exit_code: 0,
            duration_ms: 1,
            output_ref: None,
            output_bytes: 0,
            covered_paths: Vec::new(),
            warnings: Vec::new(),
            warning_count: 0,
            failure_count: 0,
            created_at: now.clone(),
            expires_at: "2999-01-01T00:00:00Z".into(),
            hit_count: 0,
            last_hit_at: None,
        };
        store
            .record_test_result_with_events(&entry, "{}", 1_000)
            .unwrap();
    }
    let sweep = store
        .clear_test_results(&project.id, Some(&session.id), None, &now)
        .unwrap();
    assert_eq!(sweep.removed_ids.len(), 600);
    assert_eq!(sweep.remaining, 0);
    assert!(sweep.events.len() >= 3, "clear events must be chunked");
    let removed_from_events = sweep
        .events
        .iter()
        .flat_map(|event| event.payload["ids"].as_array().cloned().unwrap_or_default())
        .count();
    assert_eq!(removed_from_events, 600);
    assert_eq!(
        store
            .count_test_results(&project.id, None, None, None, true, &now)
            .unwrap(),
        0
    );
}

#[test]
fn test_cache_hit_event_and_expired_prune_are_projected() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("cache hit"))
        .unwrap();
    let session = store.create_session(&project.id, "hit").unwrap();
    let now = now_rfc3339();
    let entry = vp_protocol::test_cache::TestCacheEntry {
        id: "tr-hit".into(),
        project_id: project.id.clone(),
        session_id: session.id.clone(),
        kind: "test".into(),
        command: "cargo test".into(),
        command_source: "Cargo.toml".into(),
        key_version: vp_protocol::test_cache::TEST_CACHE_KEY_VERSION,
        cache_key: format!("v1:{}", "b".repeat(64)),
        inputs_hash: "inputs".into(),
        impact_generation: "impact".into(),
        status: "passed".into(),
        exit_code: 0,
        duration_ms: 2,
        output_ref: None,
        output_bytes: 0,
        covered_paths: vec!["tests/a.rs".into()],
        warnings: Vec::new(),
        warning_count: 0,
        failure_count: 0,
        created_at: "2000-01-01T00:00:00Z".into(),
        expires_at: "2000-01-02T00:00:00Z".into(),
        hit_count: 0,
        last_hit_at: None,
    };
    store
        .record_test_result_with_events(&entry, "{}", 10)
        .unwrap();
    let event = store
        .record_test_result_hit("tr-hit", &session.id, &now)
        .unwrap()
        .expect("hit event");
    assert_eq!(event.kind, "test_cache.hit");
    assert_eq!(event.payload["result_id"], "tr-hit");
    let stored = store
        .find_test_result_by_key(&project.id, &entry.cache_key)
        .unwrap()
        .unwrap();
    assert_eq!(stored.entry.hit_count, 1);
    assert!(stored.entry.last_hit_at.is_some());
    let sweep = store
        .prune_test_results(&project.id, None, true, None, false, &now)
        .unwrap();
    assert_eq!(sweep.removed_ids, vec!["tr-hit".to_string()]);
    assert_eq!(sweep.remaining, 0);
    assert!(
        sweep
            .events
            .iter()
            .any(|event| event.kind == "test_cache.pruned")
    );
}

fn approval_store_fixture() -> (TempDir, TempDir, Store, Project, Session) {
    let (data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("demo"))
        .unwrap();
    let session = store
        .create_session(&project.id, "approval optimizer")
        .unwrap();
    (data, repo, store, project, session)
}

#[allow(clippy::too_many_arguments)]
fn create_test_approval(
    store: &Store,
    session: &Session,
    key: &str,
    tool: &str,
    risk: &str,
    priority: i32,
    coalesce: u64,
    notify: u64,
    max_pending: u32,
    boost: &str,
) -> ApprovalCreateOutcome {
    store
        .create_approval_deduplicated(
            &session.id,
            &format!("call-{key}"),
            tool,
            &serde_json::json!({"path": format!("src/{key}.rs")}),
            Some(&serde_json::json!({
                "agent_id": "worker-1",
                "agent_name": "Worker",
                "agent_role": "worker",
                "task_id": "task-1"
            })),
            Some(risk),
            Some("test approval"),
            Some(key),
            Some("scope"),
            priority,
            None,
            coalesce,
            notify,
            max_pending,
            boost,
        )
        .unwrap()
}

#[test]
fn approval_dedup_coalesces_pending_requests_and_resolves_all_waiters() {
    let (_data, _repo, store, _project, session) = approval_store_fixture();
    let first = create_test_approval(
        &store,
        &session,
        "one",
        "write_file",
        "workspace_write",
        0,
        120,
        60,
        64,
        "host_state",
    );
    assert!(!first.coalesced);
    assert_eq!(first.approval.waiter_count, 1);
    assert!(
        first
            .events
            .iter()
            .any(|event| event.kind == "approval.created")
    );
    assert!(
        first
            .events
            .iter()
            .any(|event| event.kind == "approval.requested")
    );

    let second = create_test_approval(
        &store,
        &session,
        "one",
        "write_file",
        "workspace_write",
        0,
        120,
        60,
        64,
        "host_state",
    );
    assert!(second.coalesced);
    assert_eq!(second.approval.id, first.approval.id);
    assert_eq!(second.approval.waiter_count, 2);
    assert!(
        second
            .events
            .iter()
            .any(|event| event.kind == "approval.coalesced")
    );

    let (resolved, events) = store
        .resolve_approval_with_events(&first.approval.id, true)
        .unwrap()
        .unwrap();
    assert_eq!(resolved.status, "approved");
    assert_eq!(resolved.waiter_count, 2);
    assert!(events.iter().any(|event| event.kind == "approval.resolved"));

    // A later identical request is a new row; coalescing never turns into a
    // grant or a silent auto-approval.
    let third = create_test_approval(
        &store,
        &session,
        "one",
        "write_file",
        "workspace_write",
        0,
        120,
        60,
        64,
        "host_state",
    );
    assert!(!third.coalesced);
    assert_ne!(third.approval.id, first.approval.id);
    assert_eq!(third.approval.status, "pending");
    assert_eq!(store.list_approvals(Some(&session.id)).unwrap().len(), 2);
}

#[test]
fn approval_dedup_does_not_cross_agents_or_risks() {
    let (_data, _repo, store, _project, session) = approval_store_fixture();
    let first = create_test_approval(
        &store,
        &session,
        "agent-a",
        "write_file",
        "workspace_write",
        0,
        120,
        0,
        64,
        "host_state",
    );
    let second = store
        .create_approval_deduplicated(
            &session.id,
            "call-agent-b",
            "write_file",
            &serde_json::json!({"path": "src/agent-a.rs"}),
            Some(&serde_json::json!({"agent_id": "worker-2"})),
            Some("workspace_write"),
            Some("test"),
            Some("agent-b"),
            Some("scope"),
            0,
            None,
            120,
            0,
            64,
            "host_state",
        )
        .unwrap();
    assert!(!second.coalesced);
    assert_ne!(first.approval.id, second.approval.id);

    let other_risk = store
        .create_approval_deduplicated(
            &session.id,
            "call-risk",
            "write_file",
            &serde_json::json!({"path": "src/agent-a.rs"}),
            Some(&serde_json::json!({"agent_id": "worker-1"})),
            Some("external"),
            Some("test"),
            Some("risk-variant"),
            Some("scope"),
            0,
            None,
            120,
            0,
            64,
            "host_state",
        )
        .unwrap();
    assert!(!other_risk.coalesced);
    assert_ne!(first.approval.id, other_risk.approval.id);
}

#[test]
fn approval_template_use_consumption_is_atomic_across_threads() {
    let (_data, _repo, store, project, _session) = approval_store_fixture();
    let template = ApprovalTemplate {
        id: "t1".to_string(),
        project_id: project.id.clone(),
        name: "bounded".to_string(),
        enabled: true,
        tool_pattern: "write_file".to_string(),
        risk_levels: vec!["workspace_write".to_string()],
        path_globs: vec!["src/**".to_string()],
        action: "allow".to_string(),
        expires_at: None,
        max_uses: Some(10),
        used_count: 0,
        created_by: "test".to_string(),
        created_at: now_rfc3339(),
        updated_at: None,
    };
    store.create_approval_template(&template).unwrap();
    let mut handles = Vec::new();
    for _ in 0..24 {
        let store = store.clone();
        handles.push(std::thread::spawn(move || {
            store
                .consume_approval_template_use("t1", &now_rfc3339())
                .unwrap()
        }));
    }
    let successes = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .filter(Option::is_some)
        .count();
    assert_eq!(successes, 10);
    assert_eq!(
        store
            .get_approval_template("t1")
            .unwrap()
            .unwrap()
            .used_count,
        10
    );
    assert!(
        store
            .consume_approval_template_use("t1", &now_rfc3339())
            .unwrap()
            .is_none()
    );
}

#[test]
fn test_cache_historical_lookup_matches_covered_paths_and_ignores_failures() {
    let (_data, store, repo) = test_store();
    let project = store
        .add_project(repo.path().to_str().unwrap(), Some("cache history"))
        .unwrap();
    let session = store.create_session(&project.id, "history").unwrap();
    let now = now_rfc3339();
    for (id, status, covered) in [
        ("tr-pass", "passed", vec!["src/lib.rs".to_string()]),
        ("tr-fail", "failed", vec!["src/lib.rs".to_string()]),
        ("tr-other", "passed", vec!["src/other.rs".to_string()]),
    ] {
        let entry = vp_protocol::test_cache::TestCacheEntry {
            id: id.into(),
            project_id: project.id.clone(),
            session_id: session.id.clone(),
            kind: "test".into(),
            command: format!("cargo test {id}"),
            command_source: "Cargo.toml".into(),
            key_version: vp_protocol::test_cache::TEST_CACHE_KEY_VERSION,
            cache_key: format!("v1:{id:0>64}"),
            inputs_hash: id.into(),
            impact_generation: "impact".into(),
            status: status.into(),
            exit_code: if status == "passed" { 0 } else { 1 },
            duration_ms: 1,
            output_ref: None,
            output_bytes: 0,
            covered_paths: covered,
            warnings: Vec::new(),
            warning_count: 0,
            failure_count: u64::from(status != "passed"),
            created_at: now.clone(),
            expires_at: "2999-01-01T00:00:00Z".into(),
            hit_count: 0,
            last_hit_at: None,
        };
        store
            .record_test_result_with_events(&entry, "{}", 100)
            .unwrap();
    }
    let rows = store
        .test_results_for_paths(&project.id, &["src/lib.rs".into()], &now, 100)
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, "tr-pass");
}

#[test]
fn approval_template_expiry_is_inert_and_persisted_in_the_manifest() {
    let (_data, _repo, store, project, _session) = approval_store_fixture();
    let template = ApprovalTemplate {
        id: "t-expired".to_string(),
        project_id: project.id.clone(),
        name: "expired".to_string(),
        enabled: true,
        tool_pattern: "write_file".to_string(),
        risk_levels: vec!["workspace_write".to_string()],
        path_globs: Vec::new(),
        action: "allow".to_string(),
        expires_at: Some("2020-01-01T00:00:00Z".to_string()),
        max_uses: Some(5),
        used_count: 0,
        created_by: "test".to_string(),
        created_at: now_rfc3339(),
        updated_at: None,
    };
    store.create_approval_template(&template).unwrap();
    assert!(
        store
            .consume_approval_template_use("t-expired", &now_rfc3339())
            .unwrap()
            .is_none()
    );
    store.record_project_manifest().unwrap();
    let snapshots = read_project_manifest(store.data_dir()).unwrap();
    let snapshot = snapshots
        .iter()
        .find(|s| s.project.id == project.id)
        .unwrap();
    assert_eq!(snapshot.approval_templates.len(), 1);
    assert!(snapshot.approval_templates[0].expires_at.is_some());
}

#[test]
fn attention_policy_round_trips_and_is_normalized() {
    let (_data, _repo, store, project, _session) = approval_store_fixture();
    let policy = AttentionPolicy {
        enabled: true,
        max_pending_per_session: 0,
        coalesce_window_secs: 999_999,
        stale_after_secs: 1,
        priority_boost_risk: "bogus".to_string(),
        notify_coalesce_secs: 999_999,
    };
    store.set_attention_policy(&project.id, &policy).unwrap();
    let loaded = store.get_attention_policy(&project.id).unwrap();
    assert_eq!(loaded.max_pending_per_session, 1);
    assert_eq!(loaded.coalesce_window_secs, 3_600);
    assert_eq!(loaded.stale_after_secs, 60);
    assert_eq!(loaded.priority_boost_risk, "host_state");
    assert_eq!(loaded.notify_coalesce_secs, 3_600);
    assert_eq!(store.list_enabled_attention_policies().unwrap().len(), 1);

    store.record_project_manifest().unwrap();
    let snapshots = read_project_manifest(store.data_dir()).unwrap();
    let snapshot = snapshots
        .iter()
        .find(|s| s.project.id == project.id)
        .unwrap();
    assert_eq!(snapshot.attention_policy, loaded);
}

#[test]
fn stale_approvals_expire_and_never_become_approved() {
    let (_data, _repo, store, project, session) = approval_store_fixture();
    store
        .set_attention_policy(
            &project.id,
            &AttentionPolicy {
                enabled: true,
                stale_after_secs: 60,
                ..AttentionPolicy::default()
            },
        )
        .unwrap();
    let created = create_test_approval(
        &store,
        &session,
        "stale",
        "write_file",
        "workspace_write",
        0,
        0,
        0,
        64,
        "host_state",
    );
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE approvals SET created_at = '2020-01-01T00:00:00Z' WHERE id = ?1",
            params![created.approval.id],
        )
        .unwrap();
    let expired = store
        .expire_stale_approvals(time::OffsetDateTime::now_utc())
        .unwrap();
    assert_eq!(expired.len(), 1);
    assert_eq!(expired[0].0.status, "expired");
    assert!(
        expired[0]
            .1
            .iter()
            .any(|event| event.kind == "approval.expired")
    );
    let approval = store.get_approval(&created.approval.id).unwrap().unwrap();
    assert_eq!(approval.status, "expired");
    assert!(approval.resolved_at.is_some());
}

#[test]
fn batch_resolution_reports_each_failure_without_a_blanket_decision() {
    let (_data, _repo, store, _project, session) = approval_store_fixture();
    let first = create_test_approval(
        &store,
        &session,
        "first",
        "write_file",
        "workspace_write",
        0,
        0,
        0,
        64,
        "host_state",
    );
    let second = create_test_approval(
        &store,
        &session,
        "second",
        "write_file",
        "workspace_write",
        0,
        0,
        0,
        64,
        "host_state",
    );
    store.resolve_approval(&first.approval.id, true).unwrap();
    let outcomes = store
        .resolve_approvals_with_events(&[
            (first.approval.id.clone(), true),
            (second.approval.id.clone(), false),
            ("missing".to_string(), true),
        ])
        .unwrap();
    assert_eq!(outcomes.len(), 3);
    assert_eq!(outcomes[0].code.as_deref(), Some("APPROVAL_NOT_PENDING"));
    assert_eq!(outcomes[1].approval.as_ref().unwrap().status, "denied");
    assert_eq!(outcomes[2].code.as_deref(), Some("APPROVAL_NOT_FOUND"));
    assert_eq!(
        store
            .get_approval(&first.approval.id)
            .unwrap()
            .unwrap()
            .status,
        "approved"
    );
}

#[test]
fn attention_cap_expires_the_oldest_low_risk_row() {
    let (_data, _repo, store, _project, session) = approval_store_fixture();
    let first = create_test_approval(
        &store,
        &session,
        "first",
        "write_file",
        "workspace_write",
        0,
        0,
        0,
        2,
        "host_state",
    );
    let _second = create_test_approval(
        &store,
        &session,
        "second",
        "write_file",
        "workspace_write",
        0,
        0,
        0,
        2,
        "host_state",
    );
    let third = create_test_approval(
        &store,
        &session,
        "third",
        "write_file",
        "workspace_write",
        0,
        0,
        0,
        2,
        "host_state",
    );
    assert_eq!(third.expired.len(), 1);
    assert_eq!(third.expired[0].id, first.approval.id);
    assert_eq!(
        store
            .get_approval(&first.approval.id)
            .unwrap()
            .unwrap()
            .status,
        "expired"
    );
    let pending = store
        .list_pending_approvals(Some(&session.id), None)
        .unwrap();
    assert_eq!(pending.len(), 2);
}

#[test]
fn notification_coalescing_is_per_session_tool_risk_and_boost_bypasses_it() {
    let (_data, _repo, store, _project, session) = approval_store_fixture();
    let first = create_test_approval(
        &store,
        &session,
        "n1",
        "write_file",
        "workspace_write",
        0,
        0,
        60,
        64,
        "host_state",
    );
    let second = create_test_approval(
        &store,
        &session,
        "n2",
        "write_file",
        "workspace_write",
        0,
        0,
        60,
        64,
        "host_state",
    );
    assert!(first.approval.notify);
    assert!(!second.approval.notify);
    assert_eq!(
        second.approval.notify_key.as_deref(),
        first.approval.notify_key.as_deref()
    );

    let high_first = create_test_approval(
        &store,
        &session,
        "h1",
        "shell",
        "host_state",
        0,
        0,
        60,
        64,
        "host_state",
    );
    let high_second = create_test_approval(
        &store,
        &session,
        "h2",
        "shell",
        "host_state",
        0,
        0,
        60,
        64,
        "host_state",
    );
    assert!(high_first.approval.notify);
    assert!(high_second.approval.notify);
}

#[test]
fn approval_queue_orders_by_risk_then_blocked_task_criticality() {
    let (_data, _repo, store, _project, session) = approval_store_fixture();
    for (task_id, priority) in [("task-low", 0i64), ("task-high", 40i64)] {
        store
            .lock()
            .unwrap()
            .execute(
                "INSERT INTO plan_tasks
                 (session_id, plan_seq, task_id, project_id, department, title, priority,
                  status, deps, write_scope, attempts, enqueued_at, updated_at)
                 VALUES (?1, 1, ?2, ?3, 'api', ?2, ?4, 'running', '[]', '[]', 0, ?5, ?5)",
                params![
                    session.id,
                    task_id,
                    session.project_id,
                    priority,
                    now_rfc3339()
                ],
            )
            .unwrap();
    }
    let low = create_test_approval(
        &store,
        &session,
        "low",
        "write_file",
        "workspace_write",
        0,
        0,
        0,
        64,
        "host_state",
    );
    let high = create_test_approval(
        &store,
        &session,
        "high",
        "write_file",
        "workspace_write",
        40,
        0,
        0,
        64,
        "host_state",
    );
    // The store computes live task criticality; approval.priority is only the
    // creation snapshot.
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE approvals SET task_id = 'task-low' WHERE id = ?1",
            params![low.approval.id],
        )
        .unwrap();
    store
        .lock()
        .unwrap()
        .execute(
            "UPDATE approvals SET task_id = 'task-high' WHERE id = ?1",
            params![high.approval.id],
        )
        .unwrap();
    let pending = store
        .list_pending_approvals(Some(&session.id), None)
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert_eq!(pending[0].id, high.approval.id);
    let destructive = create_test_approval(
        &store,
        &session,
        "destructive",
        "computer",
        "destructive",
        0,
        0,
        0,
        64,
        "host_state",
    );
    let pending = store
        .list_pending_approvals(Some(&session.id), None)
        .unwrap();
    assert_eq!(pending[0].id, destructive.approval.id);
}

#[test]
fn approval_audit_rows_are_persisted_and_restored() {
    let (_data, _repo, store, project, _session) = approval_store_fixture();
    let event = ApprovalAuditEvent {
        id: "audit-1".to_string(),
        project_id: project.id.clone(),
        kind: "approval.template.created".to_string(),
        payload: serde_json::json!({"template_id": "t1"}),
        created_at: now_rfc3339(),
    };
    store.insert_approval_audit(&event).unwrap();
    assert_eq!(
        store.list_approval_audit(&project.id, None).unwrap().len(),
        1
    );
    store
        .restore_approval_audit(&project.id, std::slice::from_ref(&event))
        .unwrap();
    let audit = store.list_approval_audit(&project.id, None).unwrap();
    assert_eq!(audit, vec![event]);
}
