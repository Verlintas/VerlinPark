#![allow(clippy::assertions_on_constants, clippy::field_reassign_with_default)]

use std::time::Duration;

use vp_a2a::{
    A2aLimits, MAX_ARTIFACT_BYTES, MAX_BODY_BYTES, MAX_MESSAGE_PARTS, MAX_STORED_TASKS,
    MAX_TASK_HISTORY, MAX_TASK_TTL,
};

#[test]
fn default_limits_match_canonical_ceilings() {
    let limits = A2aLimits::default();
    assert_eq!(limits.max_body_bytes, MAX_BODY_BYTES);
    assert_eq!(limits.max_task_history, MAX_TASK_HISTORY);
    assert_eq!(limits.max_message_parts, MAX_MESSAGE_PARTS);
    assert_eq!(limits.max_artifact_bytes, MAX_ARTIFACT_BYTES);
    assert_eq!(limits.max_stored_tasks, MAX_STORED_TASKS);
    assert_eq!(limits.max_task_ttl, MAX_TASK_TTL);
    assert!(limits.validate().is_ok());
}

#[test]
fn zero_and_over_ceiling_limits_fail_closed() {
    let mut limits = A2aLimits::default();
    limits.max_body_bytes = 0;
    assert!(limits.validate().is_err());
    limits = A2aLimits::default();
    limits.max_body_bytes = MAX_BODY_BYTES + 1;
    assert!(limits.validate().is_err());
    limits = A2aLimits::default();
    limits.max_message_parts = 0;
    assert!(limits.validate().is_err());
    limits = A2aLimits::default();
    limits.max_task_history = MAX_TASK_HISTORY + 1;
    assert!(limits.validate().is_err());
    limits = A2aLimits::default();
    limits.max_task_ttl = Duration::ZERO;
    assert!(limits.validate().is_err());
    limits = A2aLimits::default();
    limits.max_task_ttl = MAX_TASK_TTL + Duration::from_secs(1);
    assert!(limits.validate().is_err());
}

#[test]
fn tighter_limits_are_accepted_and_enforced_by_validation() {
    let mut limits = A2aLimits::default();
    limits.max_message_parts = 1;
    limits.max_body_bytes = 1024;
    limits.max_stored_tasks = 2;
    assert!(limits.validate().is_ok());
    let message = vp_a2a::Message::new(
        vp_a2a::MessageRole::User,
        "m1",
        vec![vp_a2a::Part::text("a"), vp_a2a::Part::text("b")],
    );
    assert!(limits.validate_message(&message).is_err());
}

#[test]
fn canonical_ceilings_are_positive_and_bounded() {
    assert!(MAX_BODY_BYTES >= 1024);
    assert!(MAX_TASK_HISTORY >= 1);
    assert!(MAX_MESSAGE_PARTS >= 1);
    assert!(MAX_STORED_TASKS >= 1);
    assert!(MAX_TASK_TTL >= Duration::from_secs(60));
}
