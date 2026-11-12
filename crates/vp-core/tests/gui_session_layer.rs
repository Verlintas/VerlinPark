//! Cross-engine integration coverage for the unified GUI session layer.
//!
//! Unit tests in `vp-core` cover the coordinator and helper manager directly.
//! These tests use only the public engine/store surface: two engines sharing one
//! SQLite database, manifest persistence, durable timeline events, and the
//! status cache. No real Accessibility or TCC permission is required.

use std::path::Path;
use std::process::Command;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use vp_core::Engine;
use vp_core::gui::{GuiAcquire, GuiActionOutcome, GuiBusy, GuiSurface};
use vp_protocol::types::{GuiPolicy, GuiSensitiveMode};
use vp_store::Store;

fn init_repo(path: &Path) {
    let status = Command::new("git")
        .args(["init", "-b", "main"])
        .current_dir(path)
        .status()
        .unwrap();
    assert!(status.success());
    std::fs::write(path.join("README.md"), "gui integration\n").unwrap();
    let status = Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .status()
        .unwrap();
    assert!(status.success());
    let status = Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-m",
            "init",
        ])
        .current_dir(path)
        .status()
        .unwrap();
    assert!(status.success());
}

async fn project_and_sessions(
    engine: &Engine,
    repo: &Path,
    label: &str,
    count: usize,
) -> (String, Vec<String>) {
    let project = engine
        .add_project(repo.to_string_lossy().to_string(), Some(label.into()))
        .await
        .unwrap();
    let mut sessions = Vec::new();
    for index in 0..count {
        let session = engine
            .create_session(project.id.clone(), format!("{label} {index}"), None)
            .await
            .unwrap();
        sessions.push(session.id);
    }
    (project.id, sessions)
}

fn policy(grace_secs: u64, wait_ms: u64) -> GuiPolicy {
    GuiPolicy {
        grace_secs,
        wait_ms,
        ..GuiPolicy::default()
    }
}

fn acquired(value: GuiAcquire) -> vp_core::gui::GuiActionTicket {
    match value {
        GuiAcquire::Acquired(ticket) => ticket,
        GuiAcquire::Busy(busy) => panic!("expected an acquisition, got {busy}"),
    }
}

fn busy(value: GuiAcquire) -> GuiBusy {
    match value {
        GuiAcquire::Busy(busy) => busy,
        GuiAcquire::Acquired(ticket) => {
            panic!("expected a busy result, got lease {}", ticket.lease_id)
        }
    }
}

#[tokio::test]
async fn cross_engine_contention_returns_the_sqlite_holder_and_retry_hint() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let first = Engine::new(store.clone());
    let second = Engine::new(store);
    let (project, sessions) = project_and_sessions(&first, repo.path(), "cross engine", 2).await;
    let second = {
        // The second engine sees the same project/session rows because it
        // shares the database. Create its handle after the rows exist.
        second
    };
    assert_eq!(second.list_computer_leases().await.unwrap().len(), 0);
    let policy = policy(30, 5_000);
    let held = acquired(
        first
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Computer,
                &sessions[0],
                "agent-a",
                "Agent A",
            )
            .await
            .unwrap(),
    );
    let start = Instant::now();
    let blocked = busy(
        second
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Browser,
                &sessions[1],
                "agent-b",
                "Agent B",
            )
            .await
            .unwrap(),
    );
    assert!(
        start.elapsed() < Duration::from_millis(500),
        "cross-engine wait must not queue"
    );
    assert_eq!(blocked.code, "GUI_BUSY");
    assert_eq!(
        blocked.holder_session_id.as_deref(),
        Some(sessions[0].as_str())
    );
    assert_eq!(blocked.holder_agent_name.as_deref(), Some("Agent A"));
    // Another Engine sees only the authoritative SQLite row, which stores the
    // unified surface name; the holder agent still identifies the driver.
    assert_eq!(blocked.holder_surface.as_deref(), Some("gui"));
    assert!(blocked.retry_after_ms >= 100);
    assert_eq!(blocked.wait_ms, 5_000);
    assert!(blocked.queue_position.is_none());
    assert!(first.gui_end_session(&sessions[0], "handoff").await);
    let second_ticket = acquired(
        second
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Browser,
                &sessions[1],
                "agent-b",
                "Agent B",
            )
            .await
            .unwrap(),
    );
    assert!(second_ticket.lease_generation >= 1);
    assert_eq!(
        second.list_computer_leases().await.unwrap()[0].session_id,
        sessions[1]
    );
    let _ = (project, held);
}

#[tokio::test]
async fn five_sessions_are_served_in_fifo_order() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let (_project, sessions) = project_and_sessions(&engine, repo.path(), "fifo", 5).await;
    let policy = policy(30, 8_000);
    let held = acquired(
        engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &sessions[0], "a", "A")
            .await
            .unwrap(),
    );
    let mut waiters = Vec::new();
    for (index, session) in sessions.iter().enumerate().skip(1) {
        let waiter_engine = engine.clone();
        let waiter_policy = policy.clone();
        let session = session.clone();
        let label = format!("{index}");
        waiters.push(tokio::spawn(async move {
            let ticket = waiter_engine
                .gui_begin_action_with_policy(
                    &waiter_policy,
                    GuiSurface::Computer,
                    &session,
                    &label,
                    &label,
                )
                .await
                .unwrap();
            (index, ticket)
        }));
        tokio::time::sleep(Duration::from_millis(80)).await;
    }
    assert!(engine.gui_end_session(&sessions[0], "handoff").await);
    for (expected, session) in sessions.iter().enumerate().skip(1) {
        let waiter = waiters.remove(0);
        let (index, ticket) = tokio::time::timeout(Duration::from_secs(3), waiter)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(index, expected);
        let ticket = acquired(ticket);
        assert_eq!(&ticket.session_id, session);
        assert!(engine.gui_end_session(session, "handoff").await);
    }
    assert_eq!(engine.list_computer_leases().await.unwrap().len(), 0);
    let _ = held;
}

#[tokio::test]
async fn grace_sweep_frees_the_surface_for_another_engine() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let first = Engine::new(store.clone());
    let second = Engine::new(store);
    let (_project, sessions) = project_and_sessions(&first, repo.path(), "grace", 2).await;
    let policy = policy(0, 200);
    let ticket = acquired(
        first
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &sessions[0], "a", "A")
            .await
            .unwrap(),
    );
    let mut outcome = GuiActionOutcome::new("apps");
    outcome.latency_ms = 3;
    first.gui_finish_action(&ticket, outcome).await.unwrap();
    assert_eq!(first.list_computer_leases().await.unwrap().len(), 1);
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(first.sweep_gui_grace().await, 1);
    assert!(first.list_computer_leases().await.unwrap().is_empty());
    let second_ticket = acquired(
        second
            .gui_begin_action_with_policy(&policy, GuiSurface::Browser, &sessions[1], "b", "B")
            .await
            .unwrap(),
    );
    assert_eq!(second_ticket.session_id, sessions[1]);
    assert!(second.gui_end_session(&sessions[1], "cleanup").await);
}

#[tokio::test]
async fn gui_policy_survives_manifest_and_database_rebuild() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store);
    let (project, _sessions) = project_and_sessions(&engine, repo.path(), "rebuild", 1).await;
    let policy = GuiPolicy {
        grace_secs: 7,
        wait_ms: 1_234,
        status_ttl_secs: 12,
        sensitive_mode: GuiSensitiveMode::RequireApproval,
        sensitive_bundles: vec!["com.example.Vault".into()],
        sensitive_titles: vec!["Vault".into()],
        timeline_retention_days: 14,
    };
    let saved = engine
        .set_gui_policy(&project, policy.clone())
        .await
        .unwrap();
    assert_eq!(saved, policy.clone().normalized());
    engine.record_project_manifest().await.unwrap();
    // Simulate a metadata rebuild: the manifest is the source of truth for
    // project policy. Keep the directory but replace the SQLite file.
    let rebuilt_dir = TempDir::new().unwrap();
    std::fs::copy(
        data.path().join("projects.jsonl"),
        rebuilt_dir.path().join("projects.jsonl"),
    )
    .unwrap();
    let rebuilt_store = Store::open(rebuilt_dir.path()).unwrap();
    for snapshot in vp_store::read_project_manifest(rebuilt_dir.path()).unwrap() {
        rebuilt_store
            .insert_project_snapshot(&snapshot.project)
            .unwrap();
        rebuilt_store
            .set_gui_policy(&snapshot.project.id, &snapshot.gui_policy)
            .unwrap();
    }
    let rebuilt = Engine::new(rebuilt_store);
    let loaded = rebuilt.get_gui_policy(&project).await.unwrap();
    assert_eq!(loaded, saved);
}

#[tokio::test]
async fn gui_action_timeline_is_durable_and_scoped() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let (_project, sessions) = project_and_sessions(&engine, repo.path(), "timeline", 2).await;
    let policy = policy(30, 100);
    for (session, action, latency) in [
        (&sessions[0], "snapshot", 11u64),
        (&sessions[0], "click", 22),
        (&sessions[1], "open", 33),
    ] {
        let ticket = acquired(
            engine
                .gui_begin_action_with_policy(
                    &policy,
                    if action == "open" {
                        GuiSurface::Browser
                    } else {
                        GuiSurface::Computer
                    },
                    session,
                    "agent",
                    "Agent",
                )
                .await
                .unwrap(),
        );
        let mut outcome = GuiActionOutcome::new(action);
        outcome.latency_ms = latency;
        outcome.snapshot_generation = latency;
        engine.gui_finish_action(&ticket, outcome).await.unwrap();
        engine.gui_end_session(session, "timeline test").await;
    }
    let scoped = engine.gui_timeline(Some(&sessions[0]), 10).await.unwrap();
    assert_eq!(scoped.len(), 2);
    assert_eq!(scoped[0].action, "snapshot");
    assert_eq!(scoped[1].action, "click");
    assert!(scoped.iter().all(|action| action.session_id == sessions[0]));

    // Re-open the store and prove events, not process memory, are the source.
    drop(engine);
    drop(store);
    let reopened = Engine::new(Store::open(data.path()).unwrap());
    let all = reopened.gui_timeline(None, 500).await.unwrap();
    assert_eq!(all.len(), 3);
    assert!(all.iter().any(|action| action.surface == "browser"));
    let limited = reopened.gui_timeline(None, 1).await.unwrap();
    assert_eq!(limited.len(), 1);
    assert_eq!(limited[0].action, "open");
}

#[tokio::test]
async fn computer_status_is_cached_and_refresh_bypasses_the_cache() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let (project, sessions) = project_and_sessions(&engine, repo.path(), "status", 1).await;
    let policy = GuiPolicy {
        status_ttl_secs: 60,
        ..GuiPolicy::default()
    };
    engine.set_gui_policy(&project, policy).await.unwrap();
    let first = engine.computer_status_for(Some(&sessions[0]), false).await;
    assert!(!first.cached);
    assert_eq!(first.grace_secs, 45);
    assert_eq!(first.wait_ms, 15_000);
    assert!(first.snapshot_generations);
    let second = engine.computer_status_for(Some(&sessions[0]), false).await;
    assert!(second.cached);
    assert!(second.cache_age_ms.is_some());
    let refreshed = engine.computer_status_for(Some(&sessions[0]), true).await;
    assert!(!refreshed.cached);
    assert_eq!(refreshed.lease_surface, "gui");
}

#[tokio::test]
async fn gui_status_reports_policy_queue_and_helper_warmth() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let (project, sessions) = project_and_sessions(&engine, repo.path(), "status surface", 2).await;
    let policy = GuiPolicy {
        grace_secs: 30,
        wait_ms: 1_000,
        status_ttl_secs: 60,
        ..GuiPolicy::default()
    };
    engine
        .set_gui_policy(&project, policy.clone())
        .await
        .unwrap();
    let held = acquired(
        engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Computer,
                &sessions[0],
                "a",
                "Agent A",
            )
            .await
            .unwrap(),
    );
    let _ = held;
    let status = engine.gui_status(Some(&project)).await.unwrap();
    assert_eq!(status.lease_surface, "gui");
    assert_eq!(status.holder.as_ref().unwrap().session_id, sessions[0]);
    assert_eq!(status.policy, policy);
    assert!(status.stale_ref_supported);
    assert!(status.computer.snapshot_generations);
    assert_eq!(status.warm_computer_helpers, 0);
    assert_eq!(status.warm_browser_helpers, 0);
    assert!(status.queue.is_empty());
    engine.gui_end_session(&sessions[0], "cleanup").await;
}

#[tokio::test]
async fn legacy_lease_retirement_is_visible_through_the_engine() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let (_project, sessions) = project_and_sessions(&engine, repo.path(), "legacy engine", 1).await;
    let attempt = store
        .acquire_computer_lease(
            "browser",
            "legacy-owner",
            &sessions[0],
            "legacy-agent",
            "Legacy Agent",
            60,
        )
        .unwrap();
    assert!(matches!(
        attempt,
        vp_store::store::ComputerLeaseAttempt::Acquired { .. }
    ));
    let holder = engine.gui_lease_holder().await.unwrap().unwrap();
    assert_eq!(holder.surface, "browser");
    assert_eq!(holder.agent_name, "Legacy Agent");
    assert_eq!(engine.retire_legacy_gui_leases().await.unwrap(), 1);
    assert!(engine.gui_lease_holder().await.unwrap().is_none());
    let policy = policy(30, 100);
    let ticket = acquired(
        engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Computer,
                &sessions[0],
                "agent",
                "Agent",
            )
            .await
            .unwrap(),
    );
    assert_eq!(ticket.session_id, sessions[0]);
}

#[tokio::test]
async fn session_cleanup_releases_the_gui_lease_immediately() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let (_project, sessions) = project_and_sessions(&engine, repo.path(), "cleanup", 1).await;
    let policy = policy(300, 100);
    let _ticket = acquired(
        engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Browser,
                &sessions[0],
                "agent",
                "Agent",
            )
            .await
            .unwrap(),
    );
    assert_eq!(engine.list_computer_leases().await.unwrap().len(), 1);
    assert_eq!(engine.stop_session_browsers(&sessions[0]).await, 0);
    assert!(engine.list_computer_leases().await.unwrap().is_empty());
}

#[test]
fn project_sensitive_policy_reaches_the_permission_assessment() {
    use vp_core::permissions::{RiskLevel, assess_tool_call_with_gui};
    let worktree = Path::new("/tmp/gui-policy");
    let mut policy = GuiPolicy {
        sensitive_mode: GuiSensitiveMode::RequireApproval,
        ..GuiPolicy::default()
    };
    policy.sensitive_bundles = vec!["com.example.CustomVault".into()];
    let custom = assess_tool_call_with_gui(
        "computer",
        &serde_json::json!({"action": "snapshot", "bundle_id": "com.example.customvault"}),
        worktree,
        &policy,
    );
    assert_eq!(custom.risk, RiskLevel::Destructive);
    assert!(!custom.blocked);
    let denied = assess_tool_call_with_gui(
        "computer",
        &serde_json::json!({"action": "click", "window_title": "Keychain Access"}),
        worktree,
        &GuiPolicy::default(),
    );
    assert!(denied.blocked);
    let ordinary = assess_tool_call_with_gui(
        "computer",
        &serde_json::json!({"action": "snapshot", "bundle_id": "com.example.Editor"}),
        worktree,
        &policy,
    );
    assert_eq!(ordinary.risk, RiskLevel::External);
}

#[tokio::test]
async fn lease_generation_survives_engine_and_store_reopen() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let (_project, sessions) = project_and_sessions(&engine, repo.path(), "generation", 1).await;
    let policy = policy(30, 100);
    let first = acquired(
        engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Computer,
                &sessions[0],
                "agent",
                "Agent",
            )
            .await
            .unwrap(),
    );
    assert_eq!(first.lease_generation, 1);
    engine.gui_end_session(&sessions[0], "reopen").await;
    drop(engine);
    let reopened = Engine::new(Store::open(data.path()).unwrap());
    let second = acquired(
        reopened
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Browser,
                &sessions[0],
                "agent",
                "Agent",
            )
            .await
            .unwrap(),
    );
    assert_eq!(second.lease_generation, 2);
    assert_eq!(
        reopened.list_computer_leases().await.unwrap()[0].generation,
        2
    );
}

#[tokio::test]
async fn same_session_reentry_does_not_grant_a_queued_waiter() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let (_project, sessions) = project_and_sessions(&engine, repo.path(), "reentry queue", 2).await;
    let policy = policy(30, 2_000);
    let held = acquired(
        engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Computer,
                &sessions[0],
                "agent-a",
                "Agent A",
            )
            .await
            .unwrap(),
    );
    let waiter_engine = engine.clone();
    let waiter_policy = policy.clone();
    let waiter_session = sessions[1].clone();
    let waiting = tokio::spawn(async move {
        waiter_engine
            .gui_begin_action_with_policy(
                &waiter_policy,
                GuiSurface::Computer,
                &waiter_session,
                "agent-b",
                "Agent B",
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(150)).await;
    let reentry = acquired(
        engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Browser,
                &sessions[0],
                "agent-c",
                "Agent C",
            )
            .await
            .unwrap(),
    );
    assert_eq!(reentry.lease_id, held.lease_id);
    assert_eq!(reentry.lease_generation, held.lease_generation);
    assert_eq!(reentry.surface, GuiSurface::Browser);
    let status = engine.gui_status(None).await.unwrap();
    assert_eq!(status.queue.len(), 1);
    assert_eq!(status.queue[0].session_id, sessions[1]);
    assert!(engine.gui_end_session(&sessions[0], "handoff").await);
    let waiter_ticket = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let waiter_ticket = acquired(waiter_ticket);
    assert_eq!(waiter_ticket.session_id, sessions[1]);
    engine.gui_end_session(&sessions[1], "cleanup").await;
}

#[tokio::test]
async fn close_releases_the_lease_before_grace_and_wakes_the_next_session() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let (_project, sessions) = project_and_sessions(&engine, repo.path(), "close wake", 2).await;
    let policy = policy(300, 2_000);
    let _held = acquired(
        engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &sessions[0], "a", "A")
            .await
            .unwrap(),
    );
    let waiter_engine = engine.clone();
    let waiter_policy = policy.clone();
    let waiter_session = sessions[1].clone();
    let waiting = tokio::spawn(async move {
        waiter_engine
            .gui_begin_action_with_policy(
                &waiter_policy,
                GuiSurface::Computer,
                &waiter_session,
                "b",
                "B",
            )
            .await
    });
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(engine.gui_end_session(&sessions[0], "close").await);
    assert!(engine.list_computer_leases().await.unwrap().is_empty());
    let waiter_ticket = tokio::time::timeout(Duration::from_secs(2), waiting)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let waiter_ticket = acquired(waiter_ticket);
    assert_eq!(waiter_ticket.session_id, sessions[1]);
    assert_eq!(engine.list_computer_leases().await.unwrap().len(), 1);
    engine.gui_end_session(&sessions[1], "cleanup").await;
}

#[tokio::test]
async fn action_timeline_limit_is_clamped_to_five_hundred() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let (_project, sessions) = project_and_sessions(&engine, repo.path(), "limit", 1).await;
    let policy = policy(30, 50);
    let ticket = acquired(
        engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Computer,
                &sessions[0],
                "agent",
                "Agent",
            )
            .await
            .unwrap(),
    );
    for index in 0..505 {
        let mut outcome = GuiActionOutcome::new(format!("click-{index}"));
        outcome.latency_ms = index;
        engine.gui_finish_action(&ticket, outcome).await.unwrap();
    }
    let actions = engine
        .gui_timeline(Some(&sessions[0]), 10_000)
        .await
        .unwrap();
    assert_eq!(actions.len(), 500);
    assert_eq!(actions.last().unwrap().action, "click-504");
}

#[tokio::test]
async fn operator_release_invalidates_same_session_reentry() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let (_project, sessions) = project_and_sessions(&engine, repo.path(), "operator", 2).await;
    let policy = policy(300, 100);
    let held = acquired(
        engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &sessions[0], "a", "A")
            .await
            .unwrap(),
    );
    assert!(engine.release_computer_lease(&held.lease_id).await.unwrap());
    assert!(engine.list_computer_leases().await.unwrap().is_empty());
    let next = acquired(
        engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Browser, &sessions[1], "b", "B")
            .await
            .unwrap(),
    );
    assert_eq!(next.session_id, sessions[1]);
    assert!(next.lease_generation > held.lease_generation);
}

#[tokio::test]
async fn gui_status_uses_the_computer_status_cache_across_repeated_polls() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let (project, _sessions) = project_and_sessions(&engine, repo.path(), "status cache", 1).await;
    let policy = GuiPolicy {
        status_ttl_secs: 60,
        ..GuiPolicy::default()
    };
    engine.set_gui_policy(&project, policy).await.unwrap();
    let first = engine.gui_status(Some(&project)).await.unwrap();
    assert!(!first.computer.cached);
    let second = engine.gui_status(Some(&project)).await.unwrap();
    assert!(second.computer.cached);
    assert!(second.computer.cache_age_ms.is_some());
    assert_eq!(second.lease_surface, "gui");
    assert_eq!(second.policy.status_ttl_secs, 60);
}

fn historical_event(
    session_id: &str,
    seq: u64,
    kind: &str,
    payload: serde_json::Value,
    days_ago: i64,
) -> vp_protocol::types::Event {
    let created = time::OffsetDateTime::now_utc() - time::Duration::days(days_ago);
    vp_protocol::types::Event {
        session_id: session_id.to_string(),
        seq,
        kind: kind.to_string(),
        payload,
        created_at: created
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap(),
    }
}

fn gui_action_event(
    session_id: &str,
    seq: u64,
    action: &str,
    days_ago: i64,
) -> vp_protocol::types::Event {
    historical_event(
        session_id,
        seq,
        "gui.action",
        serde_json::json!({
            "id": format!("gui_{seq}"),
            "session_id": session_id,
            "agent_id": "agent-1",
            "agent_name": "Agent",
            "surface": "computer",
            "action": action,
            "sensitive_target": false,
            "snapshot_generation": 1,
            "lease_generation": 1,
            "queued_ms": 0,
            "latency_ms": 4,
            "result": "ok",
            "occurred_at": format!("2026-01-01T00:00:0{seq}Z"),
        }),
        days_ago,
    )
}

fn write_mirror(data: &Path, session_id: &str, events: &[vp_protocol::types::Event]) {
    let dir = data.join("sessions").join(session_id);
    std::fs::create_dir_all(&dir).unwrap();
    let mut content = String::new();
    for event in events {
        content.push_str(&serde_json::to_string(event).unwrap());
        content.push('\n');
    }
    std::fs::write(dir.join("events.jsonl"), content).unwrap();
}

#[tokio::test]
async fn gui_timeline_retention_prunes_only_old_gui_actions_and_audits() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let (project, sessions) = project_and_sessions(&engine, repo.path(), "retention", 1).await;
    let session = &sessions[0];
    let events = vec![
        gui_action_event(session, 1, "old-gui", 40),
        historical_event(
            session,
            2,
            "message",
            serde_json::json!({"content": "old message"}),
            60,
        ),
        gui_action_event(session, 3, "new-gui", 1),
    ];
    store.replace_events(session, &events).unwrap();
    write_mirror(data.path(), session, &events);
    let result = engine
        .prune_gui_timeline(&project, Some(30), false, Some(100))
        .await
        .unwrap();
    assert_eq!(result.pruned, 1, "result={result:?}");
    assert_eq!(result.other_kinds_touched, 0);
    assert_eq!(result.sessions, 1);
    assert!(!result.dry_run);
    let remaining = store.list_all_events(session).unwrap();
    assert!(!remaining.iter().any(|event| event.kind == "gui.action"
        && event.payload.get("action").and_then(|value| value.as_str()) == Some("old-gui")));
    assert!(remaining.iter().any(|event| event.kind == "gui.action"
        && event.payload.get("action").and_then(|value| value.as_str()) == Some("new-gui")));
    assert!(remaining.iter().any(|event| {
        event.kind == "message"
            && event
                .payload
                .get("content")
                .and_then(|value| value.as_str())
                == Some("old message")
    }));
    let audit = remaining
        .iter()
        .find(|event| event.kind == "gui.timeline_pruned")
        .expect("audit event");
    assert_eq!(audit.payload["kind"], serde_json::json!("gui.action"));
    assert_eq!(audit.payload["pruned"], serde_json::json!(1));
    let status = engine.session_log_status(session).await.unwrap();
    assert!(status.matches, "status={status:?}");
    let timeline = engine.gui_timeline(Some(session), 10).await.unwrap();
    assert_eq!(timeline.len(), 1);
    assert_eq!(timeline[0].action, "new-gui");
    let audit = engine.gui_timeline_audit(&project, Some(10)).await.unwrap();
    assert_eq!(audit.total, 1);
    assert_eq!(audit.entries.len(), 1);
    assert_eq!(audit.entries[0].pruned, 1);
    assert_eq!(audit.entries[0].retention_days, 30);
    assert!(!audit.entries[0].cutoff.is_empty());
    assert_eq!(audit.entries[0].session_id, *session);
    assert!(!serde_json::to_string(&audit).unwrap().contains("old-gui"));
}

#[tokio::test]
async fn gui_timeline_retention_is_bounded_dry_run_safe_and_policy_driven() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let (project, sessions) =
        project_and_sessions(&engine, repo.path(), "retention bound", 1).await;
    let session = &sessions[0];
    let events = vec![
        historical_event(
            session,
            1,
            "gui.action",
            serde_json::json!({"action": "a"}),
            50,
        ),
        historical_event(
            session,
            2,
            "gui.action",
            serde_json::json!({"action": "b"}),
            45,
        ),
        historical_event(
            session,
            3,
            "gui.action",
            serde_json::json!({"action": "c"}),
            40,
        ),
    ];
    store.replace_events(session, &events).unwrap();
    write_mirror(data.path(), session, &events);
    let dry = engine
        .prune_gui_timeline(&project, Some(30), true, Some(10))
        .await
        .unwrap();
    assert_eq!(dry.pruned, 3);
    assert!(dry.dry_run);
    assert_eq!(store.list_all_events(session).unwrap().len(), 3);
    let first = engine
        .prune_gui_timeline(&project, Some(30), false, Some(1))
        .await
        .unwrap();
    assert_eq!(first.pruned, 1);
    assert!(first.truncated, "result={first:?}");
    assert_eq!(first.limit, 1);
    let second = engine
        .prune_gui_timeline(&project, Some(30), false, Some(10))
        .await
        .unwrap();
    assert_eq!(second.pruned, 2);
    assert!(!second.truncated);
    assert!(
        engine
            .gui_timeline(Some(session), 10)
            .await
            .unwrap()
            .is_empty()
    );

    let mut policy = engine.get_gui_policy(&project).await.unwrap();
    assert_eq!(policy.timeline_retention_days, 30);
    policy.timeline_retention_days = 0;
    engine.set_gui_policy(&project, policy).await.unwrap();
    let old = historical_event(
        session,
        4,
        "gui.action",
        serde_json::json!({"action": "forever"}),
        500,
    );
    store
        .replace_events(session, std::slice::from_ref(&old))
        .unwrap();
    write_mirror(data.path(), session, std::slice::from_ref(&old));
    let sweep = engine.sweep_gui_timeline_retention(100).await.unwrap();
    assert!(
        sweep.is_empty(),
        "keep-forever projects are skipped: {sweep:?}"
    );
}

#[tokio::test]
async fn gui_timeline_retention_rejects_zero_explicit_days_and_unreadable_mirror() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let (project, sessions) =
        project_and_sessions(&engine, repo.path(), "retention mirror", 1).await;
    let session = &sessions[0];
    let events = vec![historical_event(
        session,
        1,
        "gui.action",
        serde_json::json!({"action": "x"}),
        90,
    )];
    store.replace_events(session, &events).unwrap();
    let dir = data.path().join("sessions").join(session);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), b"{not-json}\n").unwrap();
    let result = engine
        .prune_gui_timeline(&project, Some(30), false, Some(10))
        .await
        .unwrap();
    assert_eq!(result.pruned, 0);
    assert_eq!(result.skipped_sessions, 1);
    assert_eq!(result.skipped_events, 1);
    assert_eq!(store.list_all_events(session).unwrap().len(), 1);
    let error = engine
        .prune_gui_timeline(&project, Some(0), false, None)
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("keep forever"), "{error}");
}

#[tokio::test]
async fn gui_timeline_retention_status_counts_only_eligible_actions() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let (project, sessions) =
        project_and_sessions(&engine, repo.path(), "retention stats", 1).await;
    let session = &sessions[0];
    let events = vec![
        gui_action_event(session, 1, "old", 40),
        gui_action_event(session, 2, "new", 1),
    ];
    store.replace_events(session, &events).unwrap();
    let status = engine
        .gui_timeline_retention(&project, Some(30))
        .await
        .unwrap();
    assert_eq!(status.total, 2);
    assert_eq!(status.eligible, 1);
    assert!(!status.keep_forever);
    assert!(status.cutoff.is_some());
    assert!(status.oldest_at.is_some() && status.newest_at.is_some());
    let forever = engine
        .gui_timeline_retention(&project, Some(0))
        .await
        .unwrap();
    assert!(forever.keep_forever);
    assert!(forever.cutoff.is_none());
    assert_eq!(forever.eligible, 0);
    assert_eq!(forever.total, 2);
    let error = engine
        .gui_timeline_retention(&project, Some(u32::MAX))
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("older_than_days"), "{error}");
}

#[tokio::test]
async fn gui_retention_sweep_skips_keep_forever_projects_and_is_bounded() {
    let data = TempDir::new().unwrap();
    let repo_a = TempDir::new().unwrap();
    let repo_b = TempDir::new().unwrap();
    init_repo(repo_a.path());
    init_repo(repo_b.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let (project_a, sessions_a) = project_and_sessions(&engine, repo_a.path(), "sweep a", 1).await;
    let (project_b, sessions_b) = project_and_sessions(&engine, repo_b.path(), "sweep b", 1).await;
    let events_a = vec![
        gui_action_event(&sessions_a[0], 1, "a-old", 90),
        gui_action_event(&sessions_a[0], 2, "a-new", 1),
    ];
    store.replace_events(&sessions_a[0], &events_a).unwrap();
    let events_b = vec![gui_action_event(&sessions_b[0], 1, "b-old", 90)];
    store.replace_events(&sessions_b[0], &events_b).unwrap();
    let mut policy = engine.get_gui_policy(&project_a).await.unwrap();
    policy.timeline_retention_days = 30;
    engine.set_gui_policy(&project_a, policy).await.unwrap();
    let mut keep = engine.get_gui_policy(&project_b).await.unwrap();
    keep.timeline_retention_days = 0;
    engine.set_gui_policy(&project_b, keep).await.unwrap();

    let sweep = engine.sweep_gui_timeline_retention(100).await.unwrap();
    assert_eq!(sweep.len(), 1, "keep-forever project is skipped: {sweep:?}");
    assert_eq!(sweep[0].project_id, project_a);
    assert_eq!(sweep[0].pruned, 1);
    let remaining_a = store.list_all_events(&sessions_a[0]).unwrap();
    assert!(
        !remaining_a.iter().any(|event| event.kind == "gui.action"
            && event.payload["action"] == serde_json::json!("a-old"))
    );
    assert!(
        remaining_a.iter().any(|event| event.kind == "gui.action"
            && event.payload["action"] == serde_json::json!("a-new"))
    );
    let remaining_b = store.list_all_events(&sessions_b[0]).unwrap();
    assert!(
        remaining_b.iter().any(|event| event.kind == "gui.action"
            && event.payload["action"] == serde_json::json!("b-old"))
    );

    let mut bounded = engine.get_gui_policy(&project_a).await.unwrap();
    bounded.timeline_retention_days = 30;
    engine.set_gui_policy(&project_a, bounded).await.unwrap();
    let more = vec![
        gui_action_event(&sessions_a[0], 3, "a-old-2", 80),
        gui_action_event(&sessions_a[0], 4, "a-old-3", 70),
    ];
    store.replace_events(&sessions_a[0], &more).unwrap();
    let limited = engine.sweep_gui_timeline_retention(1).await.unwrap();
    assert_eq!(limited[0].pruned, 1);
    assert!(limited[0].truncated, "sweep must report a bounded batch");
    let follow_up = engine.sweep_gui_timeline_retention(10).await.unwrap();
    assert_eq!(follow_up[0].pruned, 1);
}

#[tokio::test]
async fn gui_retention_rejects_an_oversized_window_and_clamps_audit_limits() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let (project, sessions) =
        project_and_sessions(&engine, repo.path(), "retention limits", 1).await;
    let session = &sessions[0];
    let events = vec![gui_action_event(session, 1, "limit", 90)];
    store.replace_events(session, &events).unwrap();
    let error = engine
        .prune_gui_timeline(
            &project,
            Some(vp_protocol::types::GuiPolicy::MAX_TIMELINE_RETENTION_DAYS + 1),
            true,
            Some(10),
        )
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("older_than_days"), "{error}");
    let pruned = engine
        .prune_gui_timeline(&project, Some(30), false, Some(10))
        .await
        .unwrap();
    assert_eq!(pruned.pruned, 1);
    let audit = engine.gui_timeline_audit(&project, Some(0)).await.unwrap();
    assert_eq!(audit.limit, 1);
    assert_eq!(audit.entries.len(), 1);
    let audit = engine
        .gui_timeline_audit(&project, Some(99_999))
        .await
        .unwrap();
    assert_eq!(audit.limit, 1000);
    let missing = engine
        .gui_timeline_audit("missing-project", None)
        .await
        .unwrap();
    assert_eq!(missing.total, 0);
    assert!(missing.entries.is_empty());
}

#[tokio::test]
async fn gui_retention_dry_run_writes_no_audit_or_delete() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let (project, sessions) =
        project_and_sessions(&engine, repo.path(), "retention dry audit", 1).await;
    let session = &sessions[0];
    let events = vec![gui_action_event(session, 1, "dry", 90)];
    store.replace_events(session, &events).unwrap();
    let result = engine
        .prune_gui_timeline(&project, Some(30), true, Some(10))
        .await
        .unwrap();
    assert!(result.dry_run);
    assert_eq!(result.pruned, 1);
    let after = store.list_all_events(session).unwrap();
    assert_eq!(after.len(), 1);
    assert!(
        !after
            .iter()
            .any(|event| event.kind == "gui.timeline_pruned")
    );
    let audit = engine.gui_timeline_audit(&project, None).await.unwrap();
    assert_eq!(audit.total, 0);
}

#[tokio::test]
async fn gui_retention_does_not_remove_other_kinds_from_the_mirror() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store.clone());
    let (project, sessions) =
        project_and_sessions(&engine, repo.path(), "retention no mirror", 1).await;
    let session = &sessions[0];
    let events = vec![gui_action_event(session, 1, "no-mirror", 90)];
    store.replace_events(session, &events).unwrap();
    let result = engine
        .prune_gui_timeline(&project, Some(30), false, Some(10))
        .await
        .unwrap();
    assert_eq!(result.pruned, 1);
    assert_eq!(result.skipped_sessions, 0);
    let mirror = data
        .path()
        .join("sessions")
        .join(session)
        .join("events.jsonl");
    let mirror_text = std::fs::read_to_string(&mirror).unwrap();
    assert!(
        !mirror_text.contains("no-mirror"),
        "the pruned gui.action line must be removed from the mirror"
    );
    assert!(
        mirror_text.contains("session.created"),
        "retention must never remove another event kind from the mirror"
    );
    let audit = engine.gui_timeline_audit(&project, None).await.unwrap();
    assert_eq!(audit.total, 1);
}
