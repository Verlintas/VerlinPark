//! Public-API integration tests for cross-process ownership.
//!
//! These tests intentionally open independent `Store` connections to one temp
//! data directory, mirroring two daemon processes on the same host. They do
//! not use private test hooks: every operation below is available to the
//! daemon, CLI, or a future harness.

use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use tempfile::TempDir;
use vp_core::Engine;
use vp_store::Store;
use vp_store::store::{
    ContinuationAckLostReason, RuntimeLeaseError, RuntimeLeaseState, SessionContinuationAckError,
};

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

struct Fixture {
    _data: TempDir,
    _repo: TempDir,
    engine: Engine,
    session_id: String,
}

async fn fixture(label: &str) -> Fixture {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    init_repo(repo.path());
    let engine = Engine::new(Store::open(data.path()).unwrap());
    let project = engine
        .add_project(
            repo.path().to_string_lossy().to_string(),
            Some(label.into()),
        )
        .await
        .unwrap();
    let session = engine
        .create_session(project.id.clone(), label.into(), None)
        .await
        .unwrap();
    Fixture {
        _data: data,
        _repo: repo,
        engine,
        session_id: session.id,
    }
}

async fn ready_continuation(fixture: &Fixture, label: &str) -> String {
    let continuation = fixture
        .engine
        .defer_session(
            &fixture.session_id,
            label,
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
    fixture
        .engine
        .mark_session_continuation_ready(&continuation.id, "deadline", &[])
        .await
        .unwrap()
        .unwrap();
    continuation.id
}

#[test]
fn live_lease_rejects_a_competitor_with_holder_metadata() {
    let data = TempDir::new().unwrap();
    let first = Store::open(data.path()).unwrap();
    let second = Store::open(data.path()).unwrap();
    let lease = first
        .acquire_runtime_lease(
            "session:public-1",
            "owner-a",
            "instance-a:1",
            "session",
            Some("public-1"),
            60,
        )
        .unwrap();
    let error = second
        .acquire_runtime_lease(
            "session:public-1",
            "owner-b",
            "instance-b:2",
            "session",
            Some("public-1"),
            60,
        )
        .unwrap_err();
    let holder = match error {
        RuntimeLeaseError::Busy { holder } => holder.expect("holder metadata"),
        other => panic!("expected typed busy error, got {other}"),
    };
    assert_eq!(holder.owner_id, "owner-a");
    assert_eq!(holder.owner_instance, "instance-a:1");
    assert_eq!(holder.fencing_token, 1);
    assert_eq!(holder.session_id.as_deref(), Some("public-1"));
    assert_eq!(holder.state(), RuntimeLeaseState::Live);
    assert_eq!(lease.fencing_token(), 1);
    assert_eq!(
        second
            .runtime_lease_owner("session:public-1")
            .unwrap()
            .unwrap()
            .owner_id,
        "owner-a"
    );
}

#[test]
fn stale_generation_cannot_assert_renew_or_release() {
    let data = TempDir::new().unwrap();
    let first = Store::open(data.path()).unwrap();
    let second = Store::open(data.path()).unwrap();
    let stale = first
        .acquire_runtime_lease(
            "integration:public-2",
            "owner-a",
            "instance-a:1",
            "integration",
            Some("public-2"),
            0,
        )
        .unwrap();
    let current = second
        .acquire_runtime_lease(
            "integration:public-2",
            "owner-b",
            "instance-b:2",
            "integration",
            Some("public-2"),
            60,
        )
        .unwrap();
    assert_eq!(current.fencing_token(), 2);
    for error in [
        first
            .assert_runtime_lease("integration:public-2", "owner-a", stale.token())
            .unwrap_err(),
        first
            .renew_runtime_lease("integration:public-2", "owner-a", stale.token(), 60)
            .unwrap_err(),
        first
            .release_runtime_lease("integration:public-2", "owner-a", stale.token())
            .unwrap_err(),
    ] {
        assert!(matches!(error, RuntimeLeaseError::Lost { .. }));
    }
    let asserted = second
        .assert_runtime_lease("integration:public-2", "owner-b", current.token())
        .unwrap();
    assert_eq!(asserted.owner_id, "owner-b");
    assert_eq!(
        first
            .runtime_lease_owner("integration:public-2")
            .unwrap()
            .unwrap()
            .owner_id,
        "owner-b"
    );
}

#[test]
fn release_tombstone_keeps_the_fencing_generation_monotonic() {
    let data = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let first = store
        .acquire_runtime_lease(
            "session:public-3",
            "owner-a",
            "instance-a:1",
            "session",
            Some("public-3"),
            60,
        )
        .unwrap();
    store
        .release_runtime_lease("session:public-3", "owner-a", first.token())
        .unwrap();
    let audit = store.list_runtime_leases_including_released().unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(audit[0].state(), RuntimeLeaseState::Released);
    assert_eq!(audit[0].fencing_token, 1);
    let second = store
        .acquire_runtime_lease(
            "session:public-3",
            "owner-b",
            "instance-b:2",
            "session",
            Some("public-3"),
            60,
        )
        .unwrap();
    assert_eq!(second.fencing_token(), 2);
    store
        .release_runtime_lease("session:public-3", "owner-b", second.token())
        .unwrap();
    assert_eq!(store.clear_expired_runtime_leases().unwrap(), 1);
    assert!(store.list_runtime_leases().unwrap().is_empty());
    let third = store
        .acquire_runtime_lease(
            "session:public-3",
            "owner-c",
            "instance-c:3",
            "session",
            Some("public-3"),
            60,
        )
        .unwrap();
    assert_eq!(
        third.fencing_token(),
        3,
        "cleanup must not reuse a generation"
    );
}

#[test]
fn concurrent_public_acquisition_has_exactly_one_winner() {
    let data = TempDir::new().unwrap();
    Store::open(data.path()).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(16));
    let mut handles = Vec::new();
    for index in 0..16 {
        let path = data.path().to_path_buf();
        let barrier = barrier.clone();
        handles.push(std::thread::spawn(move || {
            let store = Store::open(&path).unwrap();
            barrier.wait();
            store.acquire_runtime_lease(
                "session:public-race",
                &format!("owner-{index}"),
                &format!("instance-{index}:{index}"),
                "session",
                Some("public-race"),
                120,
            )
        }));
    }
    let mut winners = 0;
    let mut busy = 0;
    for handle in handles {
        match handle.join().unwrap() {
            Ok(acquisition) => {
                winners += 1;
                assert_eq!(acquisition.fencing_token(), 1);
            }
            Err(RuntimeLeaseError::Busy { .. }) => busy += 1,
            Err(other) => panic!("unexpected lease error: {other}"),
        }
    }
    assert_eq!(winners, 1);
    assert_eq!(busy, 15);
    let store = Store::open(data.path()).unwrap();
    assert_eq!(store.list_runtime_leases().unwrap().len(), 1);
}

#[tokio::test]
async fn two_engines_claim_the_same_continuation_once() {
    let fixture = fixture("public claim").await;
    let continuation_id = ready_continuation(&fixture, "claim once").await;
    let second = Engine::new(Store::open(fixture._data.path()).unwrap());
    let (left, right) = tokio::join!(
        fixture
            .engine
            .claim_session_continuation_for_delivery(&continuation_id, 60),
        second.claim_session_continuation_for_delivery(&continuation_id, 60)
    );
    let left = left.unwrap();
    let right = right.unwrap();
    assert!(left.is_some() ^ right.is_some());
    let winner = left.or(right).unwrap();
    assert_eq!(winner.continuation.status, "delivering");
    assert_eq!(winner.continuation.delivery_attempt, 1);
    let delivered = fixture
        .engine
        .ack_session_continuation_delivered(&winner)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.status, "delivered");
    assert_eq!(delivered.run_count, 1);
    let events = fixture
        .engine
        .list_events(fixture.session_id.clone(), 0, 200)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivering")
            .count(),
        1
    );
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.delivered")
            .count(),
        1
    );
    let replay = fixture
        .engine
        .replay_session(&fixture.session_id)
        .await
        .unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn stale_continuation_owner_cannot_ack_after_reclaim() {
    let fixture = fixture("public stale claim").await;
    let continuation_id = ready_continuation(&fixture, "stale claim").await;
    let stale = fixture
        .engine
        .claim_session_continuation_for_delivery(&continuation_id, 0)
        .await
        .unwrap()
        .expect("first claim");
    let second = Engine::new(Store::open(fixture._data.path()).unwrap());
    let current = second
        .claim_session_continuation_for_delivery(&continuation_id, 60)
        .await
        .unwrap()
        .expect("expired claim reclaimed");
    assert!(current.reclaimed);
    assert!(matches!(
        fixture
            .engine
            .ack_session_continuation_delivered(&stale)
            .await
            .unwrap_err(),
        SessionContinuationAckError::Lost { .. }
    ));
    let rows = fixture
        .engine
        .session_continuations(&fixture.session_id, None, Some(10))
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
async fn restart_recovery_preserves_live_claims_and_reclaims_expired_ones() {
    let fixture = fixture("public recovery").await;
    let live_id = ready_continuation(&fixture, "live").await;
    let expired_id = ready_continuation(&fixture, "expired").await;
    let live = fixture
        .engine
        .claim_session_continuation_for_delivery(&live_id, 300)
        .await
        .unwrap()
        .unwrap();
    let expired = fixture
        .engine
        .claim_session_continuation_for_delivery(&expired_id, 0)
        .await
        .unwrap()
        .unwrap();
    let restarted = Engine::new(Store::open(fixture._data.path()).unwrap());
    restarted.recover_interrupted_sessions().await.unwrap();
    let rows = restarted
        .session_continuations(&fixture.session_id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    let live_row = rows.iter().find(|row| row.id == live_id).unwrap();
    assert_eq!(live_row.status, "delivering");
    assert_eq!(
        live_row.lease_owner.as_deref(),
        Some(live.owner_id.as_str())
    );
    let expired_row = rows.iter().find(|row| row.id == expired_id).unwrap();
    assert_eq!(expired_row.status, "ready");
    assert!(expired_row.lease_owner.is_none());
    assert!(
        restarted
            .ack_session_continuation_delivered(&expired)
            .await
            .is_err(),
        "expired owner cannot acknowledge after recovery"
    );
    let events = restarted
        .list_events(fixture.session_id.clone(), 0, 300)
        .await
        .unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "session.continuation.reclaimed")
            .count(),
        1
    );
    assert!(
        !events
            .iter()
            .any(|event| event.kind == "session.continuation.failed")
    );
    let replay = restarted.replay_session(&fixture.session_id).await.unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
    assert!(replay.consistent, "{:?}", replay.mismatches);
}

#[tokio::test]
async fn continuation_retry_reclaims_a_fresh_generation() {
    let fixture = fixture("public retry").await;
    let continuation_id = ready_continuation(&fixture, "retry").await;
    let first = fixture
        .engine
        .claim_session_continuation_for_delivery(&continuation_id, 60)
        .await
        .unwrap()
        .unwrap();
    fixture
        .engine
        .ack_session_continuation_retry(&first, "provider down", "2999-01-01T00:00:00Z")
        .await
        .unwrap()
        .unwrap();
    assert!(
        fixture
            .engine
            .ack_session_continuation_delivered(&first)
            .await
            .is_err()
    );
    let second = fixture
        .engine
        .claim_session_continuation_for_delivery(&continuation_id, 60)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.delivery_attempt, 2);
    let delivered = fixture
        .engine
        .ack_session_continuation_delivered(&second)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(delivered.run_count, 1);
    assert_eq!(delivered.status, "delivered");
    let replay = fixture
        .engine
        .replay_session(&fixture.session_id)
        .await
        .unwrap();
    assert!(replay.continuations_match, "{:?}", replay.mismatches);
}

#[test]
fn daemon_identity_is_stable_and_each_engine_has_a_unique_owner() {
    let data = TempDir::new().unwrap();
    let first = Store::open(data.path()).unwrap();
    let second = Store::open(data.path()).unwrap();
    assert_eq!(first.daemon_identity(), second.daemon_identity());
    assert_eq!(first.daemon_identity().pid, std::process::id());
    assert_eq!(
        first.process_owner_instance(),
        second.process_owner_instance()
    );
    let engine_a = Engine::new(first.clone());
    let engine_b = Engine::new(second.clone());
    assert_ne!(engine_a.runtime_id(), engine_b.runtime_id());
    assert!(
        engine_a
            .runtime_id()
            .starts_with(&format!("{}:", first.daemon_identity().instance_id))
    );
    assert_eq!(
        engine_a.daemon_identity().instance_id,
        first.daemon_identity().instance_id
    );
}

#[tokio::test]
async fn session_lease_fences_two_engines_and_releases_cleanly() {
    let fixture = fixture("public session lease").await;
    let engine_a = fixture.engine.clone();
    let engine_b = Engine::new(Store::open(fixture._data.path()).unwrap());
    let lease = engine_a
        .acquire_session_lease(&fixture.session_id, std::time::Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(lease.fencing_token, 1);
    let error = match engine_b
        .acquire_session_lease(&fixture.session_id, std::time::Duration::from_secs(60))
        .await
    {
        Ok(_) => panic!("second engine acquired a held session lease"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("already running"), "{error}");
    lease.release().await;
    assert!(engine_a.list_runtime_leases().await.unwrap().is_empty());
    let second = engine_b
        .acquire_session_lease(&fixture.session_id, std::time::Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(second.fencing_token, 2);
    second.release().await;
    let events = engine_a
        .list_events(fixture.session_id.clone(), 0, 100)
        .await
        .unwrap();
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
async fn integration_guard_release_on_drop_and_generation_visibility() {
    let fixture = fixture("public integration guard").await;
    let guard = fixture
        .engine
        .acquire_runtime_lease(
            "integration",
            &fixture.session_id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(guard.generation(), 1);
    assert_eq!(guard.owner_instance, fixture.engine.owner_instance());
    let listed = fixture.engine.list_runtime_leases().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].fencing_token, 1);
    assert_eq!(listed[0].owner_instance, guard.owner_instance);
    drop(guard);
    assert!(
        fixture
            .engine
            .list_runtime_leases()
            .await
            .unwrap()
            .is_empty()
    );
    let next = Engine::new(Store::open(fixture._data.path()).unwrap())
        .acquire_runtime_lease(
            "integration",
            &fixture.session_id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(next.generation(), 2);
    next.release().await;
}

#[tokio::test]
async fn public_lease_projection_never_contains_a_token() {
    let fixture = fixture("public redaction").await;
    let guard = fixture
        .engine
        .acquire_runtime_lease(
            "integration",
            &fixture.session_id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    let value = serde_json::to_value(fixture.engine.list_runtime_leases().await.unwrap()).unwrap();
    let text = value.to_string();
    assert!(!text.contains("lease_token"));
    assert!(!text.contains("\"token\""));
    assert_eq!(value[0]["fencing_token"], 1);
    guard.release().await;
}

#[tokio::test]
async fn continuation_claim_status_is_public_but_token_free() {
    let fixture = fixture("public claim status").await;
    let continuation_id = ready_continuation(&fixture, "claim status").await;
    let claim = fixture
        .engine
        .claim_session_continuation_for_delivery(&continuation_id, 60)
        .await
        .unwrap()
        .unwrap();
    let status = fixture
        .engine
        .store()
        .session_continuation_claim_status(&continuation_id)
        .unwrap()
        .unwrap();
    assert_eq!(status.owner_id.as_deref(), Some(claim.owner_id.as_str()));
    assert_eq!(status.delivery_attempt, 1);
    assert!(status.live);
    let text = serde_json::to_string(&status).unwrap();
    assert!(!text.contains("token"));
    fixture
        .engine
        .ack_session_continuation_delivered(&claim)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn duplicate_public_ack_is_idempotent_and_does_not_add_a_child() {
    let fixture = fixture("public duplicate ack").await;
    let continuation_id = ready_continuation(&fixture, "duplicate ack").await;
    let claim = fixture
        .engine
        .claim_session_continuation_for_delivery(&continuation_id, 60)
        .await
        .unwrap()
        .unwrap();
    fixture
        .engine
        .ack_session_continuation_delivered(&claim)
        .await
        .unwrap()
        .unwrap();
    let repeated = fixture
        .engine
        .ack_session_continuation_delivered(&claim)
        .await
        .unwrap_err();
    assert!(matches!(
        repeated,
        SessionContinuationAckError::Lost {
            reason: ContinuationAckLostReason::AlreadyTerminal,
            ..
        }
    ));
    let rows = fixture
        .engine
        .session_continuations(&fixture.session_id, None, Some(10))
        .await
        .unwrap()
        .continuations;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].run_count, 1);
}

#[tokio::test]
async fn continuation_status_and_reclaim_expose_only_claim_metadata() {
    let fixture = fixture("public status rpc").await;
    let continuation_id = ready_continuation(&fixture, "status rpc").await;
    let claim = fixture
        .engine
        .claim_session_continuation_for_delivery(&continuation_id, 0)
        .await
        .unwrap()
        .unwrap();
    let status = fixture
        .engine
        .session_continuation_status(&continuation_id)
        .await
        .unwrap();
    assert_eq!(status.continuation.id, continuation_id);
    assert_eq!(status.continuation.status, "delivering");
    let claim_info = status.claim.as_ref().expect("claim metadata");
    assert_eq!(claim_info.status, "delivering");
    assert_eq!(
        claim_info.owner_id.as_deref(),
        Some(claim.owner_id.as_str())
    );
    assert_eq!(claim_info.delivery_attempt, 1);
    assert!(!claim_info.live, "a zero-TTL claim is already stale");
    let encoded = serde_json::to_value(&status).unwrap();
    assert!(encoded.get("lease_token").is_none());
    assert!(!encoded.to_string().contains("lease_token"));

    let reclaimed = fixture
        .engine
        .reclaim_session_continuation(&continuation_id, Some("operator drill"))
        .await
        .unwrap();
    assert!(reclaimed.reclaimed);
    assert_eq!(reclaimed.continuation.as_ref().unwrap().status, "ready");
    let second = fixture
        .engine
        .reclaim_session_continuation(&continuation_id, Some("again"))
        .await
        .unwrap();
    assert!(!second.reclaimed);
    assert!(second.continuation.is_none());
}

#[tokio::test]
async fn continuation_status_rejects_unknown_ids() {
    let fixture = fixture("public unknown continuation").await;
    let error = fixture
        .engine
        .session_continuation_status("missing-continuation")
        .await
        .unwrap_err();
    assert!(error.to_string().contains("continuation not found"));
}

#[tokio::test]
async fn session_lease_drop_releases_the_row_without_an_explicit_call() {
    let fixture = fixture("drop session lease").await;
    let lease = fixture
        .engine
        .acquire_session_lease(&fixture.session_id, std::time::Duration::from_secs(60))
        .await
        .unwrap();
    assert_eq!(fixture.engine.list_runtime_leases().await.unwrap().len(), 1);
    drop(lease);
    assert!(
        fixture
            .engine
            .list_runtime_leases()
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn two_engines_reacquire_after_release_with_a_new_generation() {
    let fixture = fixture("generation handoff").await;
    let second = Engine::new(Store::open(fixture._data.path()).unwrap());
    let first_lease = fixture
        .engine
        .acquire_runtime_lease(
            "integration",
            &fixture.session_id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(first_lease.generation(), 1);
    first_lease.release().await;
    let second_lease = second
        .acquire_runtime_lease(
            "integration",
            &fixture.session_id,
            std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
    assert_eq!(second_lease.generation(), 2);
    let audit = second
        .runtime_lease_audit(
            Some(&format!("integration:{}", fixture.session_id)),
            Some(10),
        )
        .await
        .unwrap();
    assert_eq!(
        audit
            .iter()
            .map(|entry| entry.action.as_str())
            .collect::<Vec<_>>(),
        vec!["reacquired", "released", "acquired"]
    );
    second_lease.release().await;
}

#[tokio::test]
async fn continuation_status_is_empty_before_a_claim_and_delivered_after_ack() {
    let fixture = fixture("status lifecycle").await;
    let continuation_id = ready_continuation(&fixture, "status lifecycle").await;
    let before = fixture
        .engine
        .session_continuation_status(&continuation_id)
        .await
        .unwrap();
    assert_eq!(before.continuation.status, "ready");
    assert!(before.claim.is_none());
    let claim = fixture
        .engine
        .claim_session_continuation_for_delivery(&continuation_id, 60)
        .await
        .unwrap()
        .unwrap();
    fixture
        .engine
        .ack_session_continuation_delivered(&claim)
        .await
        .unwrap()
        .unwrap();
    let after = fixture
        .engine
        .session_continuation_status(&continuation_id)
        .await
        .unwrap();
    assert_eq!(after.continuation.status, "delivered");
    assert_eq!(after.continuation.run_count, 1);
    assert!(
        after.claim.is_none(),
        "terminal rows have no delivery claim metadata"
    );
    assert_eq!(after.continuation.delivery_attempt, 1);
}

#[tokio::test]
async fn reclaim_rejects_live_claims_and_terminal_rows() {
    let fixture = fixture("reclaim guards").await;
    let continuation_id = ready_continuation(&fixture, "reclaim guards").await;
    let claim = fixture
        .engine
        .claim_session_continuation_for_delivery(&continuation_id, 60)
        .await
        .unwrap()
        .unwrap();
    let live = fixture
        .engine
        .reclaim_session_continuation(&continuation_id, Some("too early"))
        .await
        .unwrap();
    assert!(!live.reclaimed);
    assert!(live.continuation.is_none());
    fixture
        .engine
        .ack_session_continuation_delivered(&claim)
        .await
        .unwrap()
        .unwrap();
    let delivered = fixture
        .engine
        .reclaim_session_continuation(&continuation_id, Some("too late"))
        .await
        .unwrap();
    assert!(!delivered.reclaimed);
    assert!(delivered.continuation.is_none());
}

#[test]
fn lease_audit_survives_store_reopen_and_row_cleanup() {
    let data = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let acquisition = store
        .acquire_runtime_lease(
            "session:reopen-audit",
            "owner-a",
            "instance-a:1",
            "session",
            Some("reopen-audit"),
            0,
        )
        .unwrap();
    store.clear_expired_runtime_leases().unwrap();
    drop(store);
    let reopened = Store::open(data.path()).unwrap();
    let audit = reopened
        .list_runtime_lease_audit(Some("session:reopen-audit"), Some(10))
        .unwrap();
    assert_eq!(audit.len(), 2);
    assert_eq!(audit[0].action, "expired");
    assert_eq!(audit[1].action, "acquired");
    assert_eq!(audit[0].fencing_token, acquisition.fencing_token());
    assert!(
        reopened
            .runtime_lease_owner("session:reopen-audit")
            .unwrap()
            .is_none()
    );
    assert_eq!(
        reopened
            .runtime_lease_fencing_token("session:reopen-audit")
            .unwrap(),
        Some(1)
    );
}
