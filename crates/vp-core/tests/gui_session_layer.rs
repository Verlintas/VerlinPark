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
