use tempfile::TempDir;
use vp_core::Engine;
use vp_protocol::progress::ProgressRound;
use vp_store::Store;

#[tokio::test]
async fn progress_projection_replays_and_repairs_from_events() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store);
    let project = engine
        .store()
        .add_project(repo.path().to_str().unwrap(), Some("progress replay"))
        .unwrap();
    let session = engine
        .store()
        .create_session(&project.id, "progress replay")
        .unwrap();
    for round_id in ["r1", "r2"] {
        engine
            .record_progress_round(
                &session.id,
                ProgressRound {
                    round_id: round_id.into(),
                    agent_id: Some("manager".into()),
                    agent_name: Some("Manager".into()),
                    satisfied: vec!["done".into()],
                    progressing: vec!["next".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    let report = engine.replay_session(&session.id).await.unwrap();
    assert!(
        !report
            .mismatches
            .iter()
            .any(|mismatch| mismatch.starts_with("progress:")),
        "{:?}",
        report.mismatches
    );

    engine
        .store()
        .replace_progress_rounds(&session.id, &[])
        .unwrap();
    let broken = engine.replay_session(&session.id).await.unwrap();
    assert!(
        broken
            .mismatches
            .iter()
            .any(|mismatch| mismatch.starts_with("progress:")),
        "{:?}",
        broken.mismatches
    );
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(
        !repaired
            .mismatches
            .iter()
            .any(|mismatch| mismatch.starts_with("progress:")),
        "{:?}",
        repaired.mismatches
    );
    let ledger = engine
        .session_progress(&session.id, None, Some(10))
        .await
        .unwrap();
    assert_eq!(ledger.rounds.len(), 2);
    assert!(ledger.advisory_only);
}

#[tokio::test]
async fn progress_repair_preserves_numeric_round_order_past_ten() {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store);
    let project = engine
        .store()
        .add_project(repo.path().to_str().unwrap(), Some("progress order"))
        .unwrap();
    let session = engine
        .store()
        .create_session(&project.id, "progress order")
        .unwrap();
    for index in 1..=11 {
        engine
            .record_progress_round(
                &session.id,
                ProgressRound {
                    round_id: format!("r{index}"),
                    agent_id: Some("manager".into()),
                    agent_name: Some("Manager".into()),
                    satisfied: vec![format!("item-{index}")],
                    progressing: vec!["next".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
    }
    engine
        .store()
        .replace_progress_rounds(&session.id, &[])
        .unwrap();
    let repaired = engine.repair_session(&session.id).await.unwrap();
    assert!(
        !repaired
            .mismatches
            .iter()
            .any(|mismatch| mismatch.starts_with("progress:")),
        "{:?}",
        repaired.mismatches
    );
    let ledger = engine
        .session_progress(&session.id, None, Some(32))
        .await
        .unwrap();
    let ids = ledger
        .rounds
        .iter()
        .map(|round| round.round_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        ids,
        vec![
            "r1", "r2", "r3", "r4", "r5", "r6", "r7", "r8", "r9", "r10", "r11"
        ]
    );
    assert_eq!(ledger.rounds.last().unwrap().round_id, "r11");
}
