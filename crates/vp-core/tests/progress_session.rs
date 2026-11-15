use tempfile::TempDir;
use vp_core::Engine;
use vp_protocol::progress::{
    ProgressEvidence, ProgressEvidenceKind, ProgressRound, ProgressStatus,
};
use vp_store::Store;

fn service() -> (TempDir, TempDir, Engine, vp_protocol::types::Session) {
    let data = TempDir::new().unwrap();
    let repo = TempDir::new().unwrap();
    std::fs::write(repo.path().join("README.md"), "progress\n").unwrap();
    let store = Store::open(data.path()).unwrap();
    let engine = Engine::new(store);
    let project = engine
        .store()
        .add_project(repo.path().to_str().unwrap(), Some("progress"))
        .unwrap();
    let session = engine
        .store()
        .create_session(&project.id, "progress")
        .unwrap();
    (data, repo, engine, session)
}

fn round(id: &str, satisfied: &[&str], in_loop: &[&str]) -> ProgressRound {
    ProgressRound {
        round_id: id.to_string(),
        agent_id: Some("manager-1".into()),
        agent_name: Some("Manager".into()),
        task_id: Some("T1".into()),
        satisfied: satisfied.iter().map(|value| value.to_string()).collect(),
        in_loop: in_loop.iter().map(|value| value.to_string()).collect(),
        progressing: vec!["next step".into()],
        blockers: Vec::new(),
        next_task: Some("T2".into()),
        instruction: Some("continue".into()),
        evidence: vec![ProgressEvidence {
            kind: ProgressEvidenceKind::Test,
            reference: "cargo test".into(),
            note: None,
        }],
        created_at: None,
    }
}

#[tokio::test]
async fn progress_rounds_are_validated_event_backed_and_advisory() {
    let (_data, _repo, engine, session) = service();
    let first = engine
        .record_progress_round(&session.id, round("r1", &["T1"], &[]))
        .await
        .unwrap();
    assert_eq!(first.rounds.len(), 1);
    assert!(first.advisory_only);
    assert_eq!(first.assessment.status, ProgressStatus::Progressing);
    let second = engine
        .record_progress_round(&session.id, round("r2", &[], &["same"]))
        .await
        .unwrap();
    assert_eq!(second.rounds.len(), 2);
    assert_eq!(second.assessment.status, ProgressStatus::Progressing);
    for id in ["r3", "r4"] {
        let next = engine
            .record_progress_round(&session.id, round(id, &[], &["same"]))
            .await
            .unwrap();
        assert_eq!(next.rounds.len(), if id == "r3" { 3 } else { 4 });
    }
    let read = engine
        .session_progress(&session.id, None, Some(10))
        .await
        .unwrap();
    assert_eq!(read.rounds.len(), 4);
    assert_eq!(read.rounds[3].round_id, "r4");
    assert_eq!(read.assessment.status, ProgressStatus::InLoop);
    let events = engine.all_events(&session.id).await.unwrap();
    assert_eq!(
        events
            .iter()
            .filter(|event| event.kind == "progress.round")
            .count(),
        4
    );
    let duplicate = engine
        .record_progress_round(&session.id, round("r1", &["again"], &[]))
        .await;
    assert!(duplicate.is_err(), "out-of-order round must fail closed");
}

#[tokio::test]
async fn progress_round_rejects_imported_sessions_and_bad_shapes() {
    let (_data, _repo, engine, session) = service();
    let mut bad = round("r1", &["ok"], &[]);
    bad.evidence[0].reference = "https://example.com".into();
    let result = engine.record_progress_round(&session.id, bad).await;
    assert!(result.is_err());
    assert!(
        engine
            .session_progress(&session.id, None, None)
            .await
            .unwrap()
            .rounds
            .is_empty()
    );
}

#[tokio::test]
async fn progress_blocks_and_sequence_gaps_fail_closed() {
    let (_data, _repo, engine, session) = service();
    let empty = engine
        .session_progress(&session.id, None, None)
        .await
        .unwrap();
    assert_eq!(
        empty.assessment.status,
        ProgressStatus::InsufficientEvidence
    );
    assert!(empty.advisory_only);

    let mut blocked = round("r1", &["T1"], &[]);
    blocked.blockers = vec!["missing credential".into()];
    let first = engine
        .record_progress_round(&session.id, blocked)
        .await
        .unwrap();
    assert_eq!(first.assessment.status, ProgressStatus::Blocked);

    let gap = engine
        .record_progress_round(&session.id, round("r5", &["T1"], &[]))
        .await;
    assert!(gap.is_err(), "round sequence gaps must fail closed");

    let mut control = round("r2", &["ok"], &[]);
    control.instruction = Some("bad\u{0007}text".into());
    assert!(
        engine
            .record_progress_round(&session.id, control)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn progress_evidence_and_lists_are_bounded_in_the_engine_path() {
    let (_data, _repo, engine, session) = service();
    let mut huge = round("r1", &[], &[]);
    huge.satisfied = (0..20).map(|index| format!("item-{index}")).collect();
    huge.evidence = (0..20)
        .map(|index| ProgressEvidence {
            kind: ProgressEvidenceKind::Note,
            reference: format!("note-{index}"),
            note: Some("x".repeat(500)),
        })
        .collect();
    huge.next_task = Some("n".repeat(5_000));
    let result = engine
        .record_progress_round(&session.id, huge)
        .await
        .unwrap();
    assert_eq!(result.rounds.len(), 1);
    assert!(result.rounds[0].satisfied.len() <= 8);
    assert!(result.rounds[0].evidence.len() <= 8);
    assert!(
        result.rounds[0]
            .next_task
            .as_deref()
            .unwrap_or_default()
            .chars()
            .count()
            <= 1_000
    );
    assert!(result.assessment.advisory_only);
}
