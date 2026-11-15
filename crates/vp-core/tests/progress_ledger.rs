//! C12 advisory progress-ledger integration tests.
//!
//! The module is pure and advisory. These tests exercise bounds, normalization,
//! monotonic append, every deterministic status path, brief/payload rendering,
//! serde compatibility, and property-style synthetic ledgers. They never rely on
//! wall-clock time, a provider, a store, or a filesystem.

use serde_json::json;
use vp_core::progress_ledger::{
    ASSESSMENT_SCHEMA, MAX_BRIEF_CHARS, MAX_ITEM_CHARS, MAX_ROUND_ID_CHARS, ProgressEvidence,
    ProgressEvidenceKind, ProgressLedger, ProgressLedgerError, ProgressPolicy, ProgressRound,
    ProgressRoundExt, ProgressStatus, assessment_payload, assessment_value, codes,
    has_credential_like_content, reasons, round_sequence, validate_round,
};

fn policy() -> ProgressPolicy {
    ProgressPolicy::default()
}

fn ledger() -> ProgressLedger {
    ProgressLedger::new("ses-progress", 3).expect("ledger")
}

fn evidence(kind: ProgressEvidenceKind, reference: &str) -> ProgressEvidence {
    ProgressEvidence {
        kind,
        reference: reference.to_string(),
        note: None,
    }
}

fn test_evidence(reference: &str) -> ProgressEvidence {
    evidence(ProgressEvidenceKind::Test, reference)
}

fn round(id: &str, satisfied: &[&str], in_loop: &[&str], progressing: &[&str]) -> ProgressRound {
    ProgressRound {
        round_id: id.to_string(),
        satisfied: satisfied.iter().map(|value| value.to_string()).collect(),
        in_loop: in_loop.iter().map(|value| value.to_string()).collect(),
        progressing: progressing.iter().map(|value| value.to_string()).collect(),
        ..ProgressRound::default()
    }
}

fn push(ledger: &mut ProgressLedger, policy: &ProgressPolicy, round: ProgressRound) {
    ledger
        .push_round(policy, round)
        .expect("round should be accepted");
}

fn error_code(error: &ProgressLedgerError) -> &'static str {
    error.code
}

fn brief_line<'a>(brief: &'a str, prefix: &str) -> Option<&'a str> {
    brief
        .lines()
        .find(|line| line.starts_with(prefix))
        .map(|line| line.trim_end())
}

// ---------------------------------------------------------------------------
// Policy bounds
// ---------------------------------------------------------------------------

#[test]
fn default_policy_matches_documented_bounds() {
    let policy = ProgressPolicy::default();
    assert_eq!(policy.max_rounds, 32);
    assert_eq!(policy.max_items_per_list, 8);
    assert_eq!(policy.max_evidence, 8);
    assert_eq!(policy.max_text_chars, 1_000);
    assert_eq!(policy.max_reference_chars, 200);
    assert_eq!(policy.max_in_loop_streak, 3);
    assert_eq!(policy.stall_after_rounds, 4);
    assert_eq!(policy.progress_window, 3);
    assert_eq!(policy.normalized().expect("default policy"), policy);
}

#[test]
fn normalized_policy_preserves_valid_custom_values() {
    let custom = ProgressPolicy {
        max_rounds: 7,
        max_items_per_list: 4,
        max_evidence: 2,
        max_text_chars: 321,
        max_reference_chars: 99,
        max_in_loop_streak: 5,
        stall_after_rounds: 6,
        progress_window: 2,
    };
    assert_eq!(custom.normalized().expect("valid custom policy"), custom);
    assert_eq!(custom.effective(), custom);
}

#[test]
fn policy_rejects_zero_bounds() {
    let cases: Vec<(&str, ProgressPolicy)> = vec![
        (
            "max_rounds",
            ProgressPolicy {
                max_rounds: 0,
                ..ProgressPolicy::default()
            },
        ),
        (
            "max_items_per_list",
            ProgressPolicy {
                max_items_per_list: 0,
                ..ProgressPolicy::default()
            },
        ),
        (
            "max_evidence",
            ProgressPolicy {
                max_evidence: 0,
                ..ProgressPolicy::default()
            },
        ),
        (
            "max_text_chars",
            ProgressPolicy {
                max_text_chars: 0,
                ..ProgressPolicy::default()
            },
        ),
        (
            "max_reference_chars",
            ProgressPolicy {
                max_reference_chars: 0,
                ..ProgressPolicy::default()
            },
        ),
        (
            "max_in_loop_streak",
            ProgressPolicy {
                max_in_loop_streak: 0,
                ..ProgressPolicy::default()
            },
        ),
        (
            "stall_after_rounds",
            ProgressPolicy {
                stall_after_rounds: 0,
                ..ProgressPolicy::default()
            },
        ),
        (
            "progress_window",
            ProgressPolicy {
                progress_window: 0,
                ..ProgressPolicy::default()
            },
        ),
    ];
    for (name, policy) in cases {
        let error = policy.normalized().expect_err(name);
        assert_eq!(error_code(&error), codes::PROGRESS_POLICY_INVALID);
        assert!(error.message.contains(name), "{}", error.message);
    }
}

#[test]
fn policy_rejects_values_above_hard_ceilings() {
    let cases = [
        ProgressPolicy {
            max_rounds: 257,
            ..ProgressPolicy::default()
        },
        ProgressPolicy {
            max_items_per_list: 65,
            ..ProgressPolicy::default()
        },
        ProgressPolicy {
            max_evidence: 65,
            ..ProgressPolicy::default()
        },
        ProgressPolicy {
            max_text_chars: 8_001,
            ..ProgressPolicy::default()
        },
        ProgressPolicy {
            max_reference_chars: 1_001,
            ..ProgressPolicy::default()
        },
        ProgressPolicy {
            max_in_loop_streak: 65,
            ..ProgressPolicy::default()
        },
        ProgressPolicy {
            stall_after_rounds: 257,
            ..ProgressPolicy::default()
        },
        ProgressPolicy {
            progress_window: 65,
            ..ProgressPolicy::default()
        },
    ];
    for policy in cases {
        assert_eq!(
            policy.normalized().expect_err("over ceiling").code,
            codes::PROGRESS_POLICY_INVALID
        );
    }
}

#[test]
fn invalid_policy_falls_back_to_default_for_read_only_helpers() {
    let invalid = ProgressPolicy {
        max_rounds: 0,
        ..ProgressPolicy::default()
    };
    assert_eq!(invalid.effective(), ProgressPolicy::default());
    let ledger = ledger();
    let assessment = ledger.assess(&invalid);
    assert_eq!(assessment.status, ProgressStatus::InsufficientEvidence);
    assert!(ledger.render_brief(&invalid).chars().count() <= MAX_BRIEF_CHARS);
}

#[test]
fn policy_serde_defaults_missing_fields_and_round_trips() {
    let decoded: ProgressPolicy = serde_json::from_str("{}").expect("default fields");
    assert_eq!(decoded, ProgressPolicy::default());
    let custom = ProgressPolicy {
        max_rounds: 9,
        stall_after_rounds: 2,
        progress_window: 1,
        ..ProgressPolicy::default()
    };
    let encoded = serde_json::to_string(&custom).expect("encode");
    let decoded: ProgressPolicy = serde_json::from_str(&encoded).expect("decode");
    assert_eq!(decoded, custom);
}

// ---------------------------------------------------------------------------
// Round validation and normalization
// ---------------------------------------------------------------------------

#[test]
fn round_validation_trims_and_preserves_first_spelling() {
    let mut round = round(
        " r1 ",
        &[" Login  Page ", "login page", "LOGIN PAGE", "Other"],
        &[],
        &[],
    );
    round.agent_id = Some(" agent-1 ".to_string());
    round.agent_name = Some(" Manager One ".to_string());
    round.task_id = Some(" task-1 ".to_string());
    round.next_task = Some(" inspect  logs ".to_string());
    round.instruction = Some(" line one \r\n line two ".to_string());
    round.created_at = Some(" 2026-11-16T00:00:00Z ".to_string());
    let normalized = round.validate(&policy()).expect("valid round");
    assert_eq!(normalized.round_id, "r1");
    assert_eq!(normalized.agent_id.as_deref(), Some("agent-1"));
    assert_eq!(normalized.agent_name.as_deref(), Some("Manager One"));
    assert_eq!(normalized.task_id.as_deref(), Some("task-1"));
    assert_eq!(
        normalized.satisfied,
        vec!["Login Page".to_string(), "Other".to_string()]
    );
    assert_eq!(normalized.next_task.as_deref(), Some("inspect logs"));
    assert_eq!(
        normalized.instruction.as_deref(),
        Some("line one \n line two")
    );
    assert_eq!(
        normalized.created_at.as_deref(),
        Some("2026-11-16T00:00:00Z")
    );
}

#[test]
fn duplicate_items_are_case_insensitive_with_first_spelling() {
    let round = round(
        "r1",
        &["Alpha", " alpha ", "ALPHA", "Beta", "beta"],
        &["Gamma", "gamma"],
        &[],
    );
    let normalized = round.validate(&policy()).expect("valid");
    assert_eq!(normalized.satisfied, vec!["Alpha", "Beta"]);
    assert_eq!(normalized.in_loop, vec!["Gamma"]);
}

#[test]
fn empty_round_is_rejected_but_blocker_only_round_is_valid() {
    let error = round("r1", &[], &[], &[])
        .validate(&policy())
        .expect_err("empty round");
    assert_eq!(error_code(&error), codes::PROGRESS_ROUND_INVALID);

    let mut blocker_round = round("r1", &[], &[], &[]);
    blocker_round.blockers = vec!["waiting for CI".to_string()];
    let normalized = blocker_round.validate(&policy()).expect("blocker round");
    assert_eq!(normalized.blockers, vec!["waiting for CI"]);
}

#[test]
fn lists_truncate_to_policy_counts_after_dedupe() {
    let policy = ProgressPolicy {
        max_items_per_list: 3,
        max_evidence: 2,
        ..ProgressPolicy::default()
    };
    let mut round = round("r1", &[], &[], &[]);
    round.satisfied = (0..20).map(|index| format!("item {index}")).collect();
    round.satisfied.push("ITEM 0".to_string());
    round.evidence = (0..10)
        .map(|index| test_evidence(&format!("test-{index}")))
        .collect();
    let normalized = round.validate(&policy).expect("valid");
    assert_eq!(
        normalized.satisfied,
        vec![
            "item 0".to_string(),
            "item 1".to_string(),
            "item 2".to_string()
        ]
    );
    assert_eq!(normalized.evidence.len(), 2);
}

#[test]
fn text_and_reference_bounds_are_applied_without_splitting_utf8() {
    let policy = ProgressPolicy {
        max_text_chars: 50,
        max_reference_chars: 12,
        ..ProgressPolicy::default()
    };
    let mut round = round("r1", &[], &[], &[]);
    round.progressing = vec!["é".repeat(400)];
    round.next_task = Some("é".repeat(4000));
    round.instruction = Some("é".repeat(4000));
    round.evidence = vec![ProgressEvidence {
        kind: ProgressEvidenceKind::Artifact,
        reference: "é".repeat(400),
        note: Some("é".repeat(4000)),
    }];
    let normalized = round.validate(&policy).expect("valid");
    assert_eq!(
        normalized.progressing[0].chars().count(),
        MAX_ITEM_CHARS.min(50)
    );
    assert_eq!(normalized.next_task.as_deref().unwrap().chars().count(), 50);
    assert_eq!(
        normalized.instruction.as_deref().unwrap().chars().count(),
        50
    );
    assert_eq!(normalized.evidence[0].reference.chars().count(), 12);
    assert!(
        normalized.evidence[0]
            .note
            .as_deref()
            .unwrap()
            .chars()
            .count()
            <= MAX_ITEM_CHARS
    );
    // Truncation is char-safe: every string remains valid UTF-8 and ends with
    // the deterministic marker.
    assert!(normalized.instruction.as_deref().unwrap().ends_with('…'));
}

#[test]
fn control_characters_are_rejected_in_identifiers_and_items() {
    let mut bad = round("r1", &["ok", "bad\u{7}"], &[], &[]);
    let error = bad.validate(&policy()).expect_err("control item");
    assert_eq!(error_code(&error), codes::PROGRESS_ROUND_INVALID);

    bad = round("r1", &[], &[], &[]);
    bad.progressing = vec!["line\nbreak".to_string()];
    assert!(bad.validate(&policy()).is_err());

    bad = round("r1", &[], &[], &[]);
    bad.blockers = vec!["tab\there".to_string()];
    assert!(bad.validate(&policy()).is_err());

    bad = round("r\u{7}1", &["ok"], &[], &[]);
    assert!(bad.validate(&policy()).is_err());

    bad = round("r1", &["ok"], &[], &[]);
    bad.evidence = vec![ProgressEvidence {
        kind: ProgressEvidenceKind::Test,
        reference: "t\u{0}1".to_string(),
        note: None,
    }];
    assert!(bad.validate(&policy()).is_err());
}

#[test]
fn free_text_allows_newlines_and_tabs_but_rejects_other_controls() {
    let mut round = round("r1", &["ok"], &[], &[]);
    round.instruction = Some("line 1\r\nline 2\tend".to_string());
    let normalized = round.validate(&policy()).expect("text controls");
    assert_eq!(
        normalized.instruction.as_deref(),
        Some("line 1\nline 2\tend")
    );

    round.instruction = Some("bell\u{7}".to_string());
    assert!(round.validate(&policy()).is_err());
}

#[test]
fn credential_like_assignments_are_rejected() {
    assert!(has_credential_like_content("password=hunter2"));
    assert!(has_credential_like_content("\"api_key\": \"value\""));
    assert!(has_credential_like_content("Authorization: Bearer abc"));
    assert!(has_credential_like_content(
        "-----BEGIN PRIVATE KEY-----\nabc\n-----END PRIVATE KEY-----"
    ));
    assert!(!has_credential_like_content("token budget increased"));
    assert!(!has_credential_like_content("refresh the token cache"));

    let mut credential_round = round("r1", &["api_key=abc"], &[], &[]);
    assert!(credential_round.validate(&policy()).is_err());

    credential_round = round("r1", &["ok"], &[], &[]);
    credential_round.instruction = Some("\"password\": \"secret\"".to_string());
    assert!(credential_round.validate(&policy()).is_err());

    credential_round = round("r1", &["ok"], &[], &[]);
    credential_round.evidence = vec![ProgressEvidence {
        kind: ProgressEvidenceKind::Note,
        reference: "note-1".to_string(),
        note: Some("token: abc".to_string()),
    }];
    assert!(credential_round.validate(&policy()).is_err());
}

#[test]
fn absolute_paths_and_urls_are_rejected_where_they_are_not_identifiers() {
    for item in [
        "/Users/apple/secret",
        "~/secret",
        "C:\\secret",
        "\\\\server\\share",
    ] {
        let round = round("r1", &[item], &[], &[]);
        assert!(round.validate(&policy()).is_err(), "{item}");
    }
    let relative = round("r1", &["src/lib.rs"], &[], &[]);
    assert!(relative.validate(&policy()).is_ok());

    let mut round = round("r1", &["ok"], &[], &[]);
    round.evidence = vec![evidence(
        ProgressEvidenceKind::Artifact,
        "https://example.test/artifact",
    )];
    assert!(round.validate(&policy()).is_err());

    round.evidence = vec![evidence(ProgressEvidenceKind::Artifact, "artifact-1")];
    assert!(round.validate(&policy()).is_ok());
}

#[test]
fn evidence_references_must_be_non_empty_and_dedupe_by_kind_and_reference() {
    let mut round = round("r1", &["ok"], &[], &[]);
    round.evidence = vec![
        test_evidence("test-a"),
        test_evidence("TEST-A"),
        evidence(ProgressEvidenceKind::Commit, "test-a"),
        ProgressEvidence {
            kind: ProgressEvidenceKind::Note,
            reference: "   ".to_string(),
            note: None,
        },
    ];
    let error = round.validate(&policy()).expect_err("empty reference");
    assert_eq!(error_code(&error), codes::PROGRESS_ROUND_INVALID);

    round.evidence = vec![
        test_evidence("test-a"),
        test_evidence("TEST-A"),
        evidence(ProgressEvidenceKind::Commit, "test-a"),
    ];
    let normalized = round.validate(&policy()).expect("dedupe");
    assert_eq!(normalized.evidence.len(), 2);
    assert_eq!(normalized.evidence[0].kind, ProgressEvidenceKind::Test);
    assert_eq!(normalized.evidence[1].kind, ProgressEvidenceKind::Commit);
}

#[test]
fn empty_optional_fields_normalize_to_none() {
    let mut round = round("r1", &["ok"], &[], &[]);
    round.next_task = Some("   ".to_string());
    round.instruction = Some("\r\n".to_string());
    round.agent_id = Some(" ".to_string());
    round.created_at = Some(" ".to_string());
    round.evidence = vec![ProgressEvidence {
        kind: ProgressEvidenceKind::Note,
        reference: "note-1".to_string(),
        note: Some(" ".to_string()),
    }];
    let normalized = round.validate(&policy()).expect("optional empties");
    assert!(normalized.next_task.is_none());
    assert!(normalized.instruction.is_none());
    assert!(normalized.agent_id.is_none());
    assert!(normalized.created_at.is_none());
    assert!(normalized.evidence[0].note.is_none());
}

#[test]
fn round_id_sequence_syntax_is_strict() {
    for good in ["1", "r2", "round-3", "manager.round-4", "build:5"] {
        assert!(round_sequence(good).is_some(), "{good}");
        let round = round(good, &["ok"], &[], &[]);
        assert!(round.validate(&policy()).is_ok(), "{good}");
    }
    for bad in ["", "0", "r0", "01", "r01", "round", "-1"] {
        assert!(round_sequence(bad).is_none(), "{bad}");
        let round = round(bad, &["ok"], &[], &[]);
        assert!(round.validate(&policy()).is_err(), "{bad}");
    }
    let trimmed = round("r1 ", &["ok"], &[], &[]);
    assert!(trimmed.validate(&policy()).is_ok());
    let too_long = format!("r{}", "1".repeat(MAX_ROUND_ID_CHARS));
    assert!(round_sequence(&too_long).is_none());
}

#[test]
fn evidence_note_is_bounded_to_item_chars() {
    let mut round = round("r1", &["ok"], &[], &[]);
    round.evidence = vec![ProgressEvidence {
        kind: ProgressEvidenceKind::Note,
        reference: "note-1".to_string(),
        note: Some("n".repeat(5_000)),
    }];
    let normalized = round.validate(&policy()).expect("valid");
    assert_eq!(
        normalized.evidence[0]
            .note
            .as_deref()
            .unwrap()
            .chars()
            .count(),
        MAX_ITEM_CHARS
    );
}

// ---------------------------------------------------------------------------
// Ledger construction, append, and sequence enforcement
// ---------------------------------------------------------------------------

#[test]
fn ledger_requires_a_bounded_session_identifier() {
    for bad in [
        "",
        "   ",
        "bad\u{7}id",
        "https://example.test",
        "password=abc",
    ] {
        let error = ProgressLedger::try_new(bad, 1).expect_err(bad);
        assert_eq!(error_code(&error), codes::PROGRESS_SESSION_INVALID);
    }
    let long = ProgressLedger::try_new("s".repeat(500), 9).expect("bounded");
    assert_eq!(long.session_id.chars().count(), 200);
    assert_eq!(long.plan_revision, 9);
    let long_via_new = ProgressLedger::new("s".repeat(500), 9).expect("bounded");
    assert_eq!(long_via_new.session_id, long.session_id);

    let ledger = ledger();
    assert!(ledger.rounds.is_empty());
    assert_eq!(
        ledger.assessment.status,
        ProgressStatus::InsufficientEvidence
    );
    assert!(ledger.assessment.advisory_only);
}

#[test]
fn push_assigns_an_empty_round_id_and_keeps_monotonic_sequence() {
    let mut ledger = ledger();
    let mut first = round("", &["a"], &[], &[]);
    first.evidence = vec![test_evidence("test-a")];
    ledger.push_round(&policy(), first).expect("first");
    assert_eq!(ledger.rounds[0].round_id, "r1");
    assert_eq!(ledger.next_round_id().expect("next"), "r2");

    let mut second = round("", &["b"], &[], &[]);
    second.evidence = vec![test_evidence("test-b")];
    ledger.push_round(&policy(), second).expect("second");
    assert_eq!(ledger.rounds[1].round_id, "r2");
    assert_eq!(ledger.plan_revision, 3);
}

#[test]
fn custom_prefixes_are_accepted_when_the_sequence_is_monotonic() {
    let mut ledger = ledger();
    let mut first = round("manager-round-1", &["a"], &[], &[]);
    first.evidence = vec![test_evidence("test-a")];
    let mut second = round("manager-round-2", &["b"], &[], &[]);
    second.evidence = vec![test_evidence("test-b")];
    push(&mut ledger, &policy(), first);
    push(&mut ledger, &policy(), second);
    assert_eq!(ledger.rounds[0].round_id, "manager-round-1");
    assert_eq!(ledger.rounds[1].round_id, "manager-round-2");
}

#[test]
fn out_of_order_and_duplicate_rounds_fail_closed() {
    let mut ledger = ledger();
    let mut first = round("r1", &["a"], &[], &[]);
    first.evidence = vec![test_evidence("test-a")];
    push(&mut ledger, &policy(), first);

    let mut skipped = round("r3", &["b"], &[], &[]);
    skipped.evidence = vec![test_evidence("test-b")];
    let error = ledger.push_round(&policy(), skipped).expect_err("skip");
    assert_eq!(error_code(&error), codes::PROGRESS_ROUND_SEQUENCE);
    assert_eq!(ledger.rounds.len(), 1);

    let mut duplicate = round("r1", &["b"], &[], &[]);
    duplicate.evidence = vec![test_evidence("test-b")];
    let error = ledger
        .push_round(&policy(), duplicate)
        .expect_err("duplicate");
    assert_eq!(error_code(&error), codes::PROGRESS_ROUND_SEQUENCE);
    assert_eq!(ledger.rounds.len(), 1);
}

#[test]
fn first_round_must_start_at_one() {
    let mut ledger = ledger();
    let mut second = round("r2", &["a"], &[], &[]);
    second.evidence = vec![test_evidence("test-a")];
    let error = ledger
        .push_round(&policy(), second)
        .expect_err("starts at 2");
    assert_eq!(error_code(&error), codes::PROGRESS_ROUND_SEQUENCE);
    assert!(ledger.rounds.is_empty());
}

#[test]
fn max_rounds_retains_the_newest_window_deterministically() {
    let policy = ProgressPolicy {
        max_rounds: 2,
        ..ProgressPolicy::default()
    };
    let mut ledger = ledger();
    for (index, id) in ["r1", "r2", "r3", "r4", "r5"].iter().enumerate() {
        let item = format!("item-{index}");
        let mut round = round(id, &[item.as_str()], &[], &[]);
        round.evidence = vec![test_evidence(&format!("test-{index}"))];
        push(&mut ledger, &policy, round);
        assert!(ledger.rounds.len() <= 2);
    }
    assert_eq!(
        ledger
            .rounds
            .iter()
            .map(|round| round.round_id.as_str())
            .collect::<Vec<_>>(),
        vec!["r4", "r5"]
    );
    assert_eq!(ledger.rounds[0].satisfied, vec!["item-3".to_string()]);
    assert_eq!(ledger.rounds[1].satisfied, vec!["item-4".to_string()]);
    assert_eq!(ledger.assess(&policy).evidence_count, 2);

    // A failed append at the window edge must not drop the oldest retained
    // round: validation happens before pruning.
    let before = ledger.clone();
    let error = ledger
        .push_round(&policy, round("r6", &[], &[], &[]))
        .expect_err("empty round");
    assert_eq!(error_code(&error), codes::PROGRESS_ROUND_INVALID);
    assert_eq!(ledger, before);
}

#[test]
fn failed_validation_leaves_rounds_and_assessment_unchanged() {
    let mut ledger = ledger();
    let mut first = round("r1", &["a"], &[], &[]);
    first.evidence = vec![test_evidence("test-a")];
    push(&mut ledger, &policy(), first);
    let before = ledger.clone();
    let error = ledger
        .push_round(&policy(), round("r2", &[], &[], &[]))
        .expect_err("empty lists");
    assert_eq!(error_code(&error), codes::PROGRESS_ROUND_INVALID);
    assert_eq!(ledger, before);
}

#[test]
fn unparseable_stored_round_id_fails_the_next_append() {
    let mut ledger = ledger();
    ledger.rounds.push(round("not-sequenced", &["a"], &[], &[]));
    ledger.assessment = ledger.assess(&policy());
    let error = ledger
        .push_round(&policy(), round("r2", &["b"], &[], &[]))
        .expect_err("previous id");
    assert_eq!(error_code(&error), codes::PROGRESS_ROUND_SEQUENCE);
    assert_eq!(ledger.rounds.len(), 1);
    assert_eq!(
        ledger.next_round_id().expect_err("next id").code,
        codes::PROGRESS_ROUND_SEQUENCE
    );
}

#[test]
fn push_and_assess_never_touch_plan_revision_or_task_identity() {
    let mut ledger = ledger();
    let mut round = round("r1", &["done"], &[], &[]);
    round.task_id = Some("T1".to_string());
    round.next_task = Some("T2".to_string());
    round.evidence = vec![test_evidence("test-1")];
    push(&mut ledger, &policy(), round);
    assert_eq!(ledger.plan_revision, 3);
    assert_eq!(ledger.rounds[0].task_id.as_deref(), Some("T1"));
    assert_eq!(ledger.rounds[0].next_task.as_deref(), Some("T2"));

    let before = ledger.clone();
    let _ = ledger.assess(&policy());
    let _ = ledger.render_brief(&policy());
    let _ = ledger.assessment_payload();
    let _ = ledger.next_round_id().expect("next");
    let _ = ledger.to_result(&policy(), false);
    assert_eq!(ledger, before);
}

// ---------------------------------------------------------------------------
// Status paths
// ---------------------------------------------------------------------------

#[test]
fn empty_ledger_is_insufficient_evidence_with_no_rounds() {
    let assessment = ledger().assess(&policy());
    assert_eq!(assessment.status, ProgressStatus::InsufficientEvidence);
    assert_eq!(assessment.reason_code, reasons::NO_ROUNDS);
    assert_eq!(assessment.in_loop_streak, 0);
    assert_eq!(assessment.stalled_rounds, 0);
    assert_eq!(assessment.satisfied_count, 0);
    assert_eq!(assessment.evidence_count, 0);
    assert!(assessment.advisory_only);
}

#[test]
fn rounds_without_evidence_are_insufficient_evidence() {
    let mut ledger = ledger();
    push(
        &mut ledger,
        &policy(),
        round("r1", &["claimed done"], &[], &[]),
    );
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.status, ProgressStatus::InsufficientEvidence);
    assert_eq!(assessment.reason_code, reasons::NO_EVIDENCE);
    assert_eq!(assessment.satisfied_count, 1);
    assert_eq!(assessment.evidence_count, 0);
    assert!(assessment.advisory_only);
}

#[test]
fn latest_blocker_is_blocked_before_other_statuses() {
    let mut ledger = ledger();
    let mut latest = round("r1", &["a"], &[], &[]);
    latest.blockers = vec!["waiting for CI".to_string()];
    latest.evidence = vec![test_evidence("test-a")];
    push(&mut ledger, &policy(), latest);
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.status, ProgressStatus::Blocked);
    assert_eq!(assessment.reason_code, reasons::BLOCKED);
    assert!(assessment.reason.contains("blocker"));
    assert!(assessment.advisory_only);
}

#[test]
fn strict_completion_requires_new_satisfied_work_and_strong_evidence() {
    let mut ledger = ledger();
    let mut round = round("r1", &["build passes"], &[], &[]);
    round.evidence = vec![test_evidence("cargo test")];
    push(&mut ledger, &policy(), round);
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.status, ProgressStatus::Completed);
    assert_eq!(assessment.reason_code, reasons::COMPLETED);
    assert!(assessment.reason.contains("advisory"));
    assert!(assessment.advisory_only);
}

#[test]
fn completion_is_not_claimed_from_a_note_or_repeated_satisfied_items() {
    let mut note_ledger = ledger();
    let mut note_round = round("r1", &["looks done"], &[], &[]);
    note_round.evidence = vec![evidence(ProgressEvidenceKind::Note, "manager says done")];
    push(&mut note_ledger, &policy(), note_round);
    let note_assessment = note_ledger.assess(&policy());
    assert_ne!(note_assessment.status, ProgressStatus::Completed);
    assert_eq!(note_assessment.status, ProgressStatus::Progressing);

    let mut repeated = ledger();
    let mut first = round("r1", &["build passes"], &[], &[]);
    first.evidence = vec![test_evidence("cargo test 1")];
    push(&mut repeated, &policy(), first);
    let mut second = round("r2", &["build passes"], &[], &[]);
    second.evidence = vec![test_evidence("cargo test 2")];
    push(&mut repeated, &policy(), second);
    let repeated_assessment = repeated.assess(&policy());
    assert_ne!(repeated_assessment.status, ProgressStatus::Completed);
}

#[test]
fn completion_requires_evidence_on_every_round_and_no_loops() {
    let mut missing_evidence = ledger();
    let mut first = round("r1", &["a"], &[], &[]);
    first.evidence = vec![test_evidence("test-a")];
    push(&mut missing_evidence, &policy(), first);
    let mut second = round("r2", &["b"], &[], &[]);
    second.evidence = vec![test_evidence("test-b")];
    push(&mut missing_evidence, &policy(), second);
    // Remove evidence from the older round after validation to simulate a
    // malformed stored ledger. The latest still has strong evidence.
    missing_evidence.rounds[0].evidence.clear();
    assert_ne!(
        missing_evidence.assess(&policy()).status,
        ProgressStatus::Completed
    );

    let mut looped = ledger();
    let mut first = round("r1", &["a"], &[], &[]);
    first.evidence = vec![test_evidence("test-a")];
    push(&mut looped, &policy(), first);
    let mut second = round("r2", &[], &["retry"], &[]);
    second.evidence = vec![test_evidence("test-b")];
    push(&mut looped, &policy(), second);
    assert_eq!(looped.assess(&policy()).status, ProgressStatus::InLoop);
}

#[test]
fn repeated_in_loop_item_triggers_streak_status() {
    let mut ledger = ledger();
    for (index, satisfied) in ["a", "a", "b"].iter().enumerate() {
        let id = format!("r{}", index + 1);
        let mut round = round(&id, &[satisfied], &["retry flaky test"], &[]);
        round.evidence = vec![test_evidence(&format!("test-{index}"))];
        push(&mut ledger, &policy(), round);
    }
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.status, ProgressStatus::InLoop);
    assert_eq!(assessment.reason_code, reasons::IN_LOOP_STREAK);
    assert_eq!(assessment.in_loop_streak, 3);
    assert!(assessment.advisory_only);
}

#[test]
fn latest_in_loop_without_progress_is_in_loop_immediately() {
    let mut ledger = ledger();
    let mut round = round("r1", &[], &["inspect same file"], &[]);
    round.evidence = vec![test_evidence("test-1")];
    push(&mut ledger, &policy(), round);
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.status, ProgressStatus::InLoop);
    assert_eq!(assessment.reason_code, reasons::IN_LOOP_NO_ADVANCE);
    assert_eq!(assessment.in_loop_streak, 1);
    assert!(assessment.advisory_only);
}

#[test]
fn satisfaction_breaks_the_no_advance_rule_but_not_a_real_streak() {
    let mut ledger = ledger();
    for (index, satisfied) in ["a", "a", "b"].iter().enumerate() {
        let id = format!("r{}", index + 1);
        let mut round = round(&id, &[satisfied], &["retry"], &[]);
        round.evidence = vec![test_evidence(&format!("test-{index}"))];
        push(&mut ledger, &policy(), round);
    }
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.in_loop_streak, 3);
    assert_ne!(assessment.reason_code, reasons::IN_LOOP_NO_ADVANCE);
    assert_eq!(assessment.status, ProgressStatus::InLoop);
}

#[test]
fn stale_progress_is_stalled_after_the_threshold() {
    let mut ledger = ledger();
    for index in 0..5 {
        let id = format!("r{}", index + 1);
        let mut round = round(&id, &[], &[], &["same work item"]);
        round.evidence = vec![test_evidence(&format!("test-{index}"))];
        push(&mut ledger, &policy(), round);
    }
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.status, ProgressStatus::Stalled);
    assert_eq!(assessment.reason_code, reasons::STALLED);
    assert_eq!(assessment.stalled_rounds, 4);
    assert!(assessment.advisory_only);
}

#[test]
fn quiet_but_below_threshold_reports_not_stalled() {
    let mut ledger = ledger();
    for index in 0..4 {
        let id = format!("r{}", index + 1);
        let mut round = round(&id, &[], &[], &["same work item"]);
        round.evidence = vec![test_evidence(&format!("test-{index}"))];
        push(&mut ledger, &policy(), round);
    }
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.status, ProgressStatus::Progressing);
    assert_eq!(assessment.reason_code, reasons::NOT_STALLED);
    assert_eq!(assessment.stalled_rounds, 3);
    assert!(assessment.reason.contains("threshold"));
}

#[test]
fn new_progress_within_the_window_reports_progressing() {
    let mut ledger = ledger();
    for index in 0..3 {
        let id = format!("r{}", index + 1);
        let mut round = round(&id, &[], &[], &["first"]);
        round.evidence = vec![test_evidence(&format!("test-{index}"))];
        push(&mut ledger, &policy(), round);
    }
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.status, ProgressStatus::Progressing);
    assert_eq!(assessment.reason_code, reasons::PROGRESSING);
    assert_eq!(assessment.stalled_rounds, 2);
    assert!(assessment.advisory_only);
}

#[test]
fn counts_are_distinct_and_evidence_is_counted_across_rounds() {
    let mut ledger = ledger();
    let mut first = round("r1", &["alpha", "beta"], &[], &[]);
    first.evidence = vec![test_evidence("test-1"), test_evidence("test-2")];
    push(&mut ledger, &policy(), first);
    let mut second = round("r2", &["alpha", "gamma"], &[], &[]);
    second.evidence = vec![test_evidence("test-3")];
    push(&mut ledger, &policy(), second);
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.satisfied_count, 3);
    assert_eq!(assessment.evidence_count, 3);
}

#[test]
fn next_task_and_instruction_are_copied_from_the_latest_round_only() {
    let mut ledger = ledger();
    let mut first = round("r1", &["a"], &[], &[]);
    first.next_task = Some("old task".to_string());
    first.instruction = Some("old instruction".to_string());
    first.evidence = vec![test_evidence("test-1")];
    push(&mut ledger, &policy(), first);
    let mut second = round("r2", &["b"], &[], &[]);
    second.next_task = Some("new task".to_string());
    second.instruction = Some("new instruction".to_string());
    second.evidence = vec![test_evidence("test-2")];
    push(&mut ledger, &policy(), second);
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.next_task.as_deref(), Some("new task"));
    assert_eq!(assessment.instruction.as_deref(), Some("new instruction"));
}

#[test]
fn assessment_only_uses_the_newest_policy_window() {
    let window_policy = ProgressPolicy {
        max_rounds: 2,
        ..ProgressPolicy::default()
    };
    let mut ledger = ledger();
    for index in 0..3 {
        let id = format!("r{}", index + 1);
        let mut round = round(&id, &[], &[], &["work"]);
        if index == 0 {
            round.blockers = vec!["old blocker".to_string()];
        }
        round.evidence = vec![test_evidence(&format!("test-{index}"))];
        push(&mut ledger, &policy(), round);
    }
    let assessment = ledger.assess(&window_policy);
    assert_ne!(assessment.status, ProgressStatus::Blocked);
    assert_eq!(assessment.satisfied_count, 0);
    assert_eq!(assessment.evidence_count, 2);

    let result = ledger.to_result(&window_policy, true);
    assert_eq!(result.rounds.len(), 2);
    assert_eq!(result.rounds[0].round_id, "r2");
    assert!(result.truncated);
    assert!(result.advisory_only);
}

#[test]
fn assessment_is_deterministic_across_list_order() {
    let items = ["a", "b", "c", "d", "e"];
    let mut assessments = Vec::new();
    for offset in 0..items.len() {
        let mut ledger = ledger();
        let mut round = round(
            "r1",
            &[items[offset], items[(offset + 1) % items.len()]],
            &[items[(offset + 2) % items.len()]],
            &[items[(offset + 3) % items.len()]],
        );
        round.evidence = vec![
            test_evidence(&format!("test-{}", offset % 2)),
            evidence(ProgressEvidenceKind::Commit, "abc123"),
        ];
        ledger.push_round(&policy(), round).expect("round");
        assessments.push(ledger.assess(&policy()));
    }
    let first = &assessments[0];
    for assessment in &assessments[1..] {
        assert_eq!(assessment.status, first.status);
        assert_eq!(assessment.reason_code, first.reason_code);
        assert_eq!(assessment.satisfied_count, first.satisfied_count);
        assert_eq!(assessment.evidence_count, first.evidence_count);
        assert_eq!(assessment.in_loop_streak, first.in_loop_streak);
        assert_eq!(assessment.stalled_rounds, first.stalled_rounds);
    }
}

#[test]
fn every_status_is_advisory_only() {
    let statuses = [
        ProgressStatus::Progressing,
        ProgressStatus::InLoop,
        ProgressStatus::Stalled,
        ProgressStatus::Blocked,
        ProgressStatus::Completed,
        ProgressStatus::InsufficientEvidence,
    ];
    for status in statuses {
        let json = serde_json::to_string(&status).expect("status json");
        assert!(json.contains(status.as_str()));
        let assessment = ProgressLedger {
            session_id: "ses".to_string(),
            plan_revision: 0,
            rounds: Vec::new(),
            assessment: Default::default(),
        };
        assert!(assessment.assess(&policy()).advisory_only);
        assert!(
            assessment.assessment_payload()["advisory_only"]
                .as_bool()
                .unwrap_or(false)
        );
    }
}

// ---------------------------------------------------------------------------
// Brief and payload rendering
// ---------------------------------------------------------------------------

#[test]
fn empty_ledger_brief_is_bounded_and_honest() {
    let brief = ledger().render_brief(&policy());
    assert!(brief.contains("status=insufficient_evidence"));
    assert!(brief.contains("reason=no_rounds"));
    assert!(brief.contains("advisory_only=true"));
    assert!(brief.contains("rounds=0"));
    assert!(brief.chars().count() <= MAX_BRIEF_CHARS);
    assert!(!brief.contains("satisfied:"));
    assert!(!brief.contains("evidence:"));
}

#[test]
fn brief_is_bounded_by_policy_items_and_evidence() {
    let policy = ProgressPolicy {
        max_items_per_list: 2,
        max_evidence: 1,
        max_text_chars: 80,
        ..ProgressPolicy::default()
    };
    let mut ledger = ledger();
    let mut round = round(
        "r1",
        &["a", "b", "c", "d", "e"],
        &["x", "y", "z"],
        &["p", "q", "r"],
    );
    round.blockers = vec!["block-1".to_string(), "block-2".to_string()];
    round.evidence = (0..6)
        .map(|index| test_evidence(&format!("test-{index}")))
        .collect();
    ledger.push_round(&policy, round).expect("round");
    let brief = ledger.render_brief(&policy);

    assert_eq!(
        brief_line(&brief, "satisfied:").expect("satisfied line"),
        "satisfied: a; b"
    );
    assert_eq!(
        brief_line(&brief, "in_loop:").expect("in_loop line"),
        "in_loop: x; y"
    );
    assert_eq!(
        brief_line(&brief, "progressing:").expect("progressing line"),
        "progressing: p; q"
    );
    assert_eq!(
        brief_line(&brief, "blockers:").expect("blockers line"),
        "blockers: block-1; block-2"
    );
    let evidence_line = brief_line(&brief, "evidence:").expect("evidence line");
    assert_eq!(evidence_line, "evidence: test:test-0");
    assert!(brief.chars().count() <= MAX_BRIEF_CHARS);
}

#[test]
fn brief_is_deterministic_and_excludes_timestamps() {
    let mut ledger = ledger();
    let mut round = round("r1", &["a"], &[], &[]);
    round.created_at = Some("2026-11-16T12:34:56Z".to_string());
    round.next_task = Some("T2".to_string());
    round.instruction = Some("check logs".to_string());
    round.evidence = vec![test_evidence("test-1")];
    ledger.push_round(&policy(), round).expect("round");
    let first = ledger.render_brief(&policy());
    let second = ledger.render_brief(&policy());
    assert_eq!(first, second);
    assert!(!first.contains("2026-11-16"));
    assert!(first.contains("next_task: T2"));
    assert!(first.contains("instruction: check logs"));
    assert!(first.chars().count() <= MAX_BRIEF_CHARS);
}

#[test]
fn brief_sanitizes_untrusted_deserialized_values() {
    let mut raw = round("r1", &[], &[], &[]);
    raw.satisfied = (0..100)
        .map(|index| format!("item {index} \u{7} {}", "x".repeat(2_000)))
        .collect();
    raw.evidence = (0..100)
        .map(|index| ProgressEvidence {
            kind: ProgressEvidenceKind::Note,
            reference: if index == 0 {
                "password=hunter2".to_string()
            } else {
                format!("evidence-{index} \u{1b} {}", "y".repeat(1_000))
            },
            note: Some("note".to_string()),
        })
        .collect();
    raw.next_task = Some("next\u{7} task".to_string());
    raw.instruction = Some("instruction \u{1b} text".to_string());
    let ledger = ProgressLedger {
        session_id: "ses-\u{7}".to_string(),
        plan_revision: 1,
        rounds: vec![raw],
        assessment: Default::default(),
    };
    let brief = ledger.render_brief(&policy());
    assert!(!brief.contains('\u{7}'));
    assert!(!brief.contains('\u{1b}'));
    assert!(!brief.contains("password=hunter2"));
    assert!(brief.chars().count() <= MAX_BRIEF_CHARS);
}

#[test]
fn assessment_payload_has_stable_schema_and_counts() {
    let mut ledger = ledger();
    let mut round = round("r1", &["a"], &[], &[]);
    round.next_task = Some("T2".to_string());
    round.instruction = Some("review".to_string());
    round.evidence = vec![test_evidence("test-1")];
    ledger.push_round(&policy(), round).expect("round");
    let payload = ledger.assessment_payload();
    assert_eq!(payload["schema"], ASSESSMENT_SCHEMA);
    assert_eq!(payload["advisory_only"], true);
    assert_eq!(payload["session_id"], "ses-progress");
    assert_eq!(payload["plan_revision"], 3);
    assert_eq!(payload["round_count"], 1);
    assert_eq!(payload["latest_round_id"], "r1");
    assert_eq!(payload["status"], "completed");
    assert_eq!(payload["satisfied_count"], 1);
    assert_eq!(payload["evidence_count"], 1);
    assert_eq!(payload["next_task"], "T2");
    assert_eq!(payload["instruction"], "review");
}

#[test]
fn assessment_value_forces_advisory_true_and_bounds_text() {
    let mut assessment = ProgressLedger::new("ses", 1)
        .expect("ledger")
        .assess(&policy());
    assessment.advisory_only = false;
    assessment.reason = "r".repeat(20_000);
    assessment.next_task = Some("n".repeat(20_000));
    assessment.instruction = Some("i".repeat(20_000));
    let value = assessment_value(&assessment);
    assert_eq!(value, assessment_payload(&assessment));
    assert_eq!(value["advisory_only"], true);
    assert!(value["reason"].as_str().unwrap().chars().count() <= 1_000);
    assert!(value["next_task"].as_str().unwrap().chars().count() <= 1_000);
    assert!(value["instruction"].as_str().unwrap().chars().count() <= 1_000);
}

// ---------------------------------------------------------------------------
// Serde compatibility
// ---------------------------------------------------------------------------

#[test]
fn full_ledger_serde_round_trip() {
    let mut ledger = ledger();
    let mut round = round("r1", &["a"], &["loop"], &["b"]);
    round.blockers = vec!["blocker".to_string()];
    round.agent_id = Some("agent-1".to_string());
    round.agent_name = Some("Manager".to_string());
    round.task_id = Some("T1".to_string());
    round.next_task = Some("T2".to_string());
    round.instruction = Some("continue".to_string());
    round.created_at = Some("2026-11-16T00:00:00Z".to_string());
    round.evidence = vec![test_evidence("test-1")];
    ledger.push_round(&policy(), round).expect("round");
    let encoded = serde_json::to_string(&ledger).expect("encode");
    let decoded: ProgressLedger = serde_json::from_str(&encoded).expect("decode");
    assert_eq!(decoded, ledger);
    assert_eq!(decoded.assessment, ledger.assessment);
    assert!(decoded.assessment.advisory_only);
}

#[test]
fn ledger_result_round_trip_and_from_result_preserves_advisory_flag() {
    let mut ledger = ledger();
    let mut round = round("r1", &["a"], &[], &[]);
    round.evidence = vec![test_evidence("test-1")];
    ledger.push_round(&policy(), round).expect("round");
    let result = ledger.to_result(&policy(), false);
    assert_eq!(result.rounds, ledger.rounds);
    let encoded = serde_json::to_string(&result).expect("encode");
    let decoded: vp_core::progress_ledger::ProgressLedgerResult =
        serde_json::from_str(&encoded).expect("decode");
    assert_eq!(decoded, result);

    let mut result = result;
    result.assessment.advisory_only = false;
    let adopted = ProgressLedger::from_result(result);
    assert!(adopted.assessment.advisory_only);
    assert_eq!(adopted.session_id, ledger.session_id);
    assert_eq!(adopted.rounds, ledger.rounds);
}

#[test]
fn unknown_status_and_evidence_kinds_are_rejected() {
    assert!(serde_json::from_str::<ProgressStatus>("\"unknown\"").is_err());
    assert!(serde_json::from_str::<ProgressEvidenceKind>("\"shell\"").is_err());
    assert_eq!(
        serde_json::from_str::<ProgressStatus>("\"in_loop\"").expect("known"),
        ProgressStatus::InLoop
    );
    assert_eq!(
        serde_json::from_str::<ProgressEvidenceKind>("\"commit\"").expect("known"),
        ProgressEvidenceKind::Commit
    );
}

#[test]
fn round_serde_defaults_missing_fields_and_ignores_unknown_fields() {
    let decoded: ProgressRound =
        serde_json::from_str(r#"{"round_id":"r1","satisfied":["a"],"unknown_future_field":true}"#)
            .expect("lenient round");
    assert_eq!(decoded.round_id, "r1");
    assert_eq!(decoded.satisfied, vec!["a"]);
    assert!(decoded.in_loop.is_empty());
    assert!(decoded.evidence.is_empty());
    assert!(decoded.agent_id.is_none());
    assert!(decoded.created_at.is_none());
}

#[test]
fn ledger_serde_defaults_empty_object_to_an_empty_conservative_ledger() {
    let decoded: ProgressLedger = serde_json::from_str("{}").expect("default ledger");
    assert!(decoded.session_id.is_empty());
    assert_eq!(decoded.plan_revision, 0);
    assert!(decoded.rounds.is_empty());
    assert_eq!(
        decoded.assessment.status,
        ProgressStatus::InsufficientEvidence
    );
    assert!(decoded.assessment.advisory_only);
}

// ---------------------------------------------------------------------------
// Boundedness / property-style loops
// ---------------------------------------------------------------------------

#[test]
fn synthetic_round_sequences_never_panic_and_stay_bounded() {
    for seed in 0u64..200 {
        let policy = ProgressPolicy {
            max_rounds: 1 + (seed as usize % 8),
            max_items_per_list: 1 + (seed as usize % 4),
            max_evidence: 1 + (seed as usize % 3),
            max_text_chars: 40 + (seed as usize % 60),
            max_reference_chars: 20 + (seed as usize % 20),
            stall_after_rounds: 1 + (seed as usize % 5),
            progress_window: 1 + (seed as usize % 4),
            max_in_loop_streak: 1 + (seed as usize % 3),
        };

        let mut ledger = ProgressLedger::new(format!("ses-{seed}"), seed).expect("ledger");
        let count = (seed as usize) % policy.max_rounds;
        for index in 0..count {
            let mut round = ProgressRound {
                round_id: String::new(),
                ..ProgressRound::default()
            };
            let bucket = (seed as usize + index) % 5;
            let item = format!("item-{}", (seed as usize + index) % 7);
            match bucket {
                0 => round.satisfied.push(item),
                1 => round.in_loop.push(item),
                2 => round.progressing.push(item),
                3 => round.blockers.push(item),
                _ => {
                    round.satisfied.push(item);
                    round.progressing.push(format!("progress-{index}"));
                    round.in_loop.push("loop".to_string());
                }
            }
            if index % 2 == 0 {
                round.evidence.push(ProgressEvidence {
                    kind: if index % 4 == 0 {
                        ProgressEvidenceKind::Test
                    } else {
                        ProgressEvidenceKind::Note
                    },
                    reference: format!("evidence-{seed}-{index}"),
                    note: (index % 3 == 0).then(|| "bounded note".to_string()),
                });
            }
            round.next_task = Some(format!("task {index}"));
            round.instruction = Some("check the next bounded step".to_string());
            ledger.push_round(&policy, round).expect("synthetic round");
        }

        let assessment = ledger.assess(&policy);
        assert!(assessment.advisory_only);
        assert!(matches!(
            assessment.status,
            ProgressStatus::Progressing
                | ProgressStatus::InLoop
                | ProgressStatus::Stalled
                | ProgressStatus::Blocked
                | ProgressStatus::Completed
                | ProgressStatus::InsufficientEvidence
        ));
        assert!((assessment.in_loop_streak as usize) <= policy.max_rounds);
        assert!((assessment.stalled_rounds as usize) <= policy.max_rounds);
        assert!(
            (assessment.satisfied_count as usize) <= policy.max_rounds * policy.max_items_per_list
        );
        assert!((assessment.evidence_count as usize) <= policy.max_rounds * policy.max_evidence);

        let brief = ledger.render_brief(&policy);
        assert!(brief.chars().count() <= MAX_BRIEF_CHARS);
        assert!(brief.contains("advisory_only=true"));

        let payload = ledger.assessment_payload();
        assert_eq!(payload["advisory_only"], true);

        let encoded = serde_json::to_string(&ledger).expect("encode synthetic ledger");
        let decoded: ProgressLedger = serde_json::from_str(&encoded).expect("decode synthetic");
        assert_eq!(decoded.rounds.len(), ledger.rounds.len());
    }
}

#[test]
fn oversized_untrusted_ledger_is_assessed_and_rendered_within_bounds() {
    let mut rounds = Vec::new();
    for index in 0..40 {
        let mut round = round(&format!("r{}", index + 1), &[], &[], &[]);
        round.satisfied = (0..50)
            .map(|item| format!("item-{index}-{item}-{}", "x".repeat(500)))
            .collect();
        round.in_loop = (0..50)
            .map(|item| format!("loop-{index}-{item}-{}", "y".repeat(500)))
            .collect();
        round.evidence = (0..50)
            .map(|item| ProgressEvidence {
                kind: ProgressEvidenceKind::Test,
                reference: format!("test-{index}-{item}-{}", "z".repeat(500)),
                note: None,
            })
            .collect();
        round.next_task = Some("n".repeat(50_000));
        round.instruction = Some("i".repeat(50_000));
        rounds.push(round);
    }
    let ledger = ProgressLedger {
        session_id: "s".repeat(50_000),
        plan_revision: 1,
        rounds,
        assessment: Default::default(),
    };
    let assessment = ledger.assess(&policy());
    assert!(assessment.advisory_only);
    assert!((assessment.satisfied_count as usize) <= 32 * 8);
    assert!((assessment.evidence_count as usize) <= 32 * 8);
    assert!(assessment.next_task.as_deref().unwrap().chars().count() <= 1_000);
    let brief = ledger.render_brief(&policy());
    assert!(brief.chars().count() <= MAX_BRIEF_CHARS);
    assert_eq!(ledger.assessment_payload()["advisory_only"], true);
}

#[test]
fn conflicting_status_signals_choose_a_conservative_precedence() {
    // Blockers win over the completion-looking satisfied list.
    let mut blocked = ledger();
    let mut completed_round = round("r1", &["done"], &[], &[]);
    completed_round.blockers = vec!["deploy is blocked".to_string()];
    completed_round.evidence = vec![test_evidence("test-1")];
    push(&mut blocked, &policy(), completed_round);
    assert_eq!(blocked.assess(&policy()).status, ProgressStatus::Blocked);

    // Missing evidence wins over blockers: the ledger cannot claim anything
    // without evidence pointers.
    let mut no_evidence = ledger();
    let mut unproven_round = round("r1", &["done"], &[], &[]);
    unproven_round.blockers = vec!["blocked".to_string()];
    push(&mut no_evidence, &policy(), unproven_round);
    let assessment = no_evidence.assess(&policy());
    assert_eq!(assessment.status, ProgressStatus::InsufficientEvidence);
    assert_eq!(assessment.reason_code, reasons::NO_EVIDENCE);

    // An in-loop item wins over a stale-looking quiet window.
    let mut looping = ledger();
    let mut loop_round = round("r1", &[], &["retry"], &[]);
    loop_round.evidence = vec![test_evidence("test-1")];
    push(&mut looping, &policy(), loop_round);
    assert_eq!(looping.assess(&policy()).status, ProgressStatus::InLoop);
}

#[test]
fn custom_policy_changes_status_thresholds_deterministically() {
    let rounds: Vec<ProgressRound> = (0..3)
        .map(|index| {
            let mut round = round(&format!("r{}", index + 1), &[], &[], &["same"]);
            round.evidence = vec![test_evidence(&format!("test-{index}"))];
            round
        })
        .collect();
    let mut ledger = ledger();
    for round in rounds {
        ledger.rounds.push(round);
    }

    let early = ProgressPolicy {
        stall_after_rounds: 2,
        progress_window: 1,
        ..ProgressPolicy::default()
    };
    assert_eq!(ledger.assess(&early).status, ProgressStatus::Stalled);
    assert_eq!(ledger.assess(&early).stalled_rounds, 2);

    let late = ProgressPolicy {
        stall_after_rounds: 8,
        progress_window: 1,
        ..ProgressPolicy::default()
    };
    let late_assessment = ledger.assess(&late);
    assert_eq!(late_assessment.status, ProgressStatus::Progressing);
    assert_eq!(late_assessment.reason_code, reasons::NOT_STALLED);
}

#[test]
fn credential_screening_never_becomes_a_positive_secret_classifier() {
    // Ordinary prose mentioning the words is allowed.
    for text in [
        "rotate the token after the deployment",
        "password reset flow is covered",
        "authorization checks were reviewed",
    ] {
        let mut round = round("r1", &[text], &[], &[]);
        round.evidence = vec![test_evidence("test-1")];
        assert!(
            round.validate(&policy()).is_ok(),
            "ordinary prose rejected: {text}"
        );
    }
}

#[test]
fn validate_function_and_extension_trait_agree() {
    let round = round("r7", &["a"], &[], &[]);
    let via_function = validate_round(&round, &policy()).expect("function");
    let via_trait = round.validate(&policy()).expect("trait");
    assert_eq!(via_function, via_trait);
}

#[test]
fn round_sequence_helper_handles_boundaries() {
    assert_eq!(round_sequence("1"), Some(1));
    assert_eq!(round_sequence("r18446744073709551615"), Some(u64::MAX));
    assert_eq!(round_sequence("r18446744073709551616"), None);
    assert_eq!(round_sequence("r1.2.3"), Some(3));
    assert_eq!(round_sequence("r1é"), None);
    assert_eq!(
        round_sequence(&format!("r{}", "9".repeat(MAX_ROUND_ID_CHARS))),
        None
    );
}

#[test]
fn to_result_bounds_untrusted_rows_and_preserves_metadata() {
    let mut rounds = Vec::new();
    for index in 0..5 {
        let mut round = round(&format!("r{}", index + 1), &[], &[], &[]);
        round.agent_name = Some(format!("manager\u{7}-{index}"));
        round.satisfied = (0..20)
            .map(|item| format!("s{index}-{item}-{}", "x".repeat(500)))
            .collect();
        round.in_loop = (0..20)
            .map(|item| format!("l{index}-{item}-{}", "y".repeat(500)))
            .collect();
        round.evidence = (0..20)
            .map(|item| ProgressEvidence {
                kind: ProgressEvidenceKind::Test,
                reference: format!("test-{index}-{item}-{}", "z".repeat(500)),
                note: Some(format!("note\u{7}-{item}-{}", "n".repeat(500))),
            })
            .collect();
        round.next_task = Some(format!("next\u{7}-{index}-{}", "t".repeat(5_000)));
        round.instruction = Some(format!("do\u{7}-{index}-{}", "i".repeat(5_000)));
        rounds.push(round);
    }
    let ledger = ProgressLedger {
        session_id: "s".repeat(1_000),
        plan_revision: 9,
        rounds,
        assessment: Default::default(),
    };
    let result = ledger.to_result(&policy(), false);
    assert_eq!(result.session_id.chars().count(), 200);
    assert_eq!(result.plan_revision, 9);
    assert_eq!(result.rounds.len(), 5);
    assert!(result.advisory_only);
    for round in &result.rounds {
        assert!(round.satisfied.len() <= 8);
        assert!(round.in_loop.len() <= 8);
        assert!(round.evidence.len() <= 8);
        assert!(round.next_task.as_deref().unwrap().chars().count() <= 1_000);
        assert!(round.instruction.as_deref().unwrap().chars().count() <= 1_000);
        assert!(!round.agent_name.as_deref().unwrap().contains('\u{7}'));
        assert!(!round.next_task.as_deref().unwrap().contains('\u{7}'));
        assert!(!round.instruction.as_deref().unwrap().contains('\u{7}'));
        for evidence in &round.evidence {
            assert!(evidence.reference.chars().count() <= 200);
            assert!(!evidence.reference.contains('\u{7}'));
            assert!(!evidence.note.as_deref().unwrap().contains('\u{7}'));
        }
    }
}

#[test]
fn new_and_try_new_share_strict_validation() {
    let via_new =
        ProgressLedger::new("https://example.test/session", 4).expect_err("url session id");
    let via_try =
        ProgressLedger::try_new("https://example.test/session", 4).expect_err("url session id");
    assert_eq!(error_code(&via_new), codes::PROGRESS_SESSION_INVALID);
    assert_eq!(error_code(&via_try), codes::PROGRESS_SESSION_INVALID);

    let ledger = ProgressLedger::new("ses-ok", 4).expect("valid");
    assert_eq!(ledger.plan_revision, 4);
    assert_eq!(
        ledger.assessment.status,
        ProgressStatus::InsufficientEvidence
    );
    assert!(ledger.assessment.advisory_only);
}

#[test]
fn in_loop_streak_resets_after_a_clean_round() {
    let mut ledger = ledger();
    let mut first = round("r1", &["a"], &["retry"], &[]);
    first.evidence = vec![test_evidence("test-1")];
    push(&mut ledger, &policy(), first);
    let mut second = round("r2", &["b"], &[], &[]);
    second.evidence = vec![test_evidence("test-2")];
    push(&mut ledger, &policy(), second);
    let mut third = round("r3", &["c"], &["retry"], &[]);
    third.evidence = vec![test_evidence("test-3")];
    push(&mut ledger, &policy(), third);
    let assessment = ledger.assess(&policy());
    assert_eq!(assessment.in_loop_streak, 1);
    assert_ne!(assessment.reason_code, reasons::IN_LOOP_STREAK);
    assert_eq!(assessment.status, ProgressStatus::Progressing);
}

#[test]
fn custom_in_loop_threshold_is_honored() {
    let policy = ProgressPolicy {
        max_in_loop_streak: 2,
        ..ProgressPolicy::default()
    };
    let mut ledger = ledger();
    for index in 0..2 {
        let id = format!("r{}", index + 1);
        let mut round = round(&id, &[if index == 0 { "a" } else { "b" }], &["retry"], &[]);
        round.evidence = vec![test_evidence(&format!("test-{index}"))];
        push(&mut ledger, &policy, round);
    }
    let assessment = ledger.assess(&policy);
    assert_eq!(assessment.status, ProgressStatus::InLoop);
    assert_eq!(assessment.reason_code, reasons::IN_LOOP_STREAK);
    assert_eq!(assessment.in_loop_streak, 2);
}

#[test]
fn identifiers_and_timestamps_use_reference_bounds() {
    let policy = ProgressPolicy {
        max_reference_chars: 8,
        ..ProgressPolicy::default()
    };
    let mut round = round("r1", &["ok"], &[], &[]);
    round.agent_id = Some("agent-identifier".to_string());
    round.agent_name = Some("manager-name".to_string());
    round.task_id = Some("task-identifier".to_string());
    round.created_at = Some("t".repeat(100));
    let normalized = round.validate(&policy).expect("valid");
    assert_eq!(normalized.agent_id.as_deref(), Some("agent-i…"));
    assert_eq!(normalized.agent_name.as_deref(), Some("manager…"));
    assert_eq!(normalized.task_id.as_deref(), Some("task-id…"));
    assert_eq!(
        normalized.created_at.as_deref().unwrap().chars().count(),
        64
    );
}

#[test]
fn refresh_assessment_and_latest_round_are_consistent() {
    let mut ledger = ledger();
    let mut round = round("r1", &["a"], &[], &[]);
    round.evidence = vec![test_evidence("test-1")];
    push(&mut ledger, &policy(), round);
    assert_eq!(
        ledger.latest_round().map(|round| round.round_id.as_str()),
        Some("r1")
    );
    let expected = ledger.assess(&policy());
    let refreshed = ledger.refresh_assessment(&policy());
    assert_eq!(refreshed, expected);
    assert_eq!(ledger.assessment, expected);
    assert!(refreshed.advisory_only);
}

#[test]
fn brief_never_exceeds_cap_for_extreme_generated_text() {
    let policy = ProgressPolicy {
        max_items_per_list: 8,
        max_evidence: 8,
        max_text_chars: 8_000,
        ..ProgressPolicy::default()
    };
    let mut ledger = ledger();
    let mut round = round("", &[], &[], &[]);
    round.satisfied = (0..8)
        .map(|index| format!("s{index}-{}", "s".repeat(8_000)))
        .collect();
    round.in_loop = (0..8)
        .map(|index| format!("i{index}-{}", "i".repeat(8_000)))
        .collect();
    round.progressing = (0..8)
        .map(|index| format!("p{index}-{}", "p".repeat(8_000)))
        .collect();
    round.blockers = (0..8)
        .map(|index| format!("b{index}-{}", "b".repeat(8_000)))
        .collect();
    round.evidence = (0..8)
        .map(|index| test_evidence(&format!("t{index}-{}", "t".repeat(900))))
        .collect();
    round.next_task = Some("n".repeat(8_000));
    round.instruction = Some("x".repeat(8_000));
    ledger.push_round(&policy, round).expect("max policy round");
    let brief = ledger.render_brief(&policy);
    assert!(brief.chars().count() <= MAX_BRIEF_CHARS);
    assert!(brief.contains("advisory_only=true"));
    assert_eq!(
        ledger.assess(&policy).satisfied_count,
        8,
        "each truncated item stays distinct because of its index prefix"
    );
}

#[test]
fn payload_is_json_object_with_only_bounded_metadata() {
    let mut ledger = ledger();
    let mut round = round("r1", &["a"], &[], &[]);
    round.evidence = vec![test_evidence("test-1")];
    ledger.push_round(&policy(), round).expect("round");
    let payload = ledger.assessment_payload();
    let object = payload.as_object().expect("object");
    assert!(object.contains_key("schema"));
    assert!(object.contains_key("status"));
    assert!(object.contains_key("reason_code"));
    assert!(object.contains_key("reason"));
    assert!(object.contains_key("in_loop_streak"));
    assert!(object.contains_key("stalled_rounds"));
    assert!(object.contains_key("satisfied_count"));
    assert!(object.contains_key("evidence_count"));
    assert!(object.contains_key("next_task"));
    assert!(object.contains_key("instruction"));
    assert!(object.contains_key("advisory_only"));
    assert_eq!(object.get("advisory_only"), Some(&json!(true)));
    assert!(!object.contains_key("rounds"));
    assert!(!object.contains_key("worktree"));
    assert!(!object.contains_key("approval"));
}

#[test]
fn no_public_type_mentions_plan_task_mutation() {
    // Compile-time/documentation guard: the public assessment, ledger result,
    // and payload expose no plan or task mutation field. `next_task` is a
    // suggestion string copied from the Manager round.
    let value = ledger().assessment_payload();
    let object = value.as_object().expect("object");
    for forbidden in [
        "tasks", "plan", "dag", "retry", "cancel", "merge", "approval", "budget", "capacity",
    ] {
        assert!(!object.contains_key(forbidden), "{forbidden}");
    }
    assert!(object.contains_key("next_task"));
}
