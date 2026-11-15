//! Canonical, immutable-in-practice bounds for the A2A facade.
//!
//! Every inbound HTTP body, JSON-RPC message, task and Agent Card is checked
//! against these limits. A host may lower a limit for a particular facade, but
//! it may not raise it above the canonical ceiling. That keeps memory use and
//! provider-visible payloads bounded even when a project configures the
//! facade itself.

use std::time::Duration;

use crate::error::{A2aError, A2aResult};

/// Maximum JSON-RPC / Agent Card request body accepted over loopback.
pub const MAX_BODY_BYTES: usize = 1024 * 1024;
/// Maximum retained conversation history on a task.
pub const MAX_TASK_HISTORY: usize = 128;
/// Maximum parts in one message or artifact.
pub const MAX_MESSAGE_PARTS: usize = 32;
/// Maximum UTF-8 bytes in a text part.
pub const MAX_PART_TEXT_BYTES: usize = 64 * 1024;
/// Maximum serialized JSON bytes in a data part.
pub const MAX_DATA_BYTES: usize = 64 * 1024;
/// Maximum serialized JSON bytes in one artifact.
pub const MAX_ARTIFACT_BYTES: usize = 256 * 1024;
/// Maximum advertised skills on an Agent Card.
pub const MAX_SKILLS: usize = 32;
/// Maximum retained lifetime of an in-memory task.
pub const MAX_TASK_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Maximum number of tasks kept in the in-memory projection.
pub const MAX_STORED_TASKS: usize = 1024;

/// Implementation ceilings that are stricter than the public A2A bounds but
/// still necessary to keep one facade from using unbounded memory.
pub const MAX_ARTIFACTS_PER_TASK: usize = 64;
pub const MAX_RETAINED_EVENTS: usize = 256;
pub const MAX_STREAM_QUEUE: usize = 128;
pub const MAX_SSE_EVENT_BYTES: usize = MAX_BODY_BYTES + 16 * 1024;
pub const MAX_FILE_PART_BYTES: usize = 256 * 1024;
pub const MAX_ID_BYTES: usize = 128;
pub const MAX_METADATA_BYTES: usize = 16 * 1024;
pub const MAX_URL_BYTES: usize = 2048;
pub const MAX_OUTPUT_MODES: usize = 8;
pub const MAX_TASK_HISTORY_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_TASK_ARTIFACT_BYTES: usize = 8 * 1024 * 1024;

/// Bounds used by every server, client, store and wire validation path.
///
/// Fields are public so a project can make the facade tighter. Call
/// [`A2aLimits::validate`] (or simply start a server, which validates the
/// policy) before relying on a hand-built value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct A2aLimits {
    pub max_body_bytes: usize,
    pub max_task_history: usize,
    pub max_message_parts: usize,
    pub max_part_text_bytes: usize,
    pub max_data_bytes: usize,
    pub max_artifact_bytes: usize,
    pub max_skills: usize,
    pub max_task_ttl: Duration,
    pub max_stored_tasks: usize,
    /// Extra defence-in-depth bounds.
    pub max_artifacts_per_task: usize,
    pub max_retained_events: usize,
    pub max_stream_queue: usize,
    pub max_sse_event_bytes: usize,
    pub max_file_part_bytes: usize,
    /// Aggregate in-memory bounds for one task. Individual messages and
    /// artifacts are bounded above; these protect the task projection from
    /// accumulating 128 maximum-sized messages.
    pub max_task_history_bytes: usize,
    pub max_task_artifact_bytes: usize,
}

impl Default for A2aLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: MAX_BODY_BYTES,
            max_task_history: MAX_TASK_HISTORY,
            max_message_parts: MAX_MESSAGE_PARTS,
            max_part_text_bytes: MAX_PART_TEXT_BYTES,
            max_data_bytes: MAX_DATA_BYTES,
            max_artifact_bytes: MAX_ARTIFACT_BYTES,
            max_skills: MAX_SKILLS,
            max_task_ttl: MAX_TASK_TTL,
            max_stored_tasks: MAX_STORED_TASKS,
            max_artifacts_per_task: MAX_ARTIFACTS_PER_TASK,
            max_retained_events: MAX_RETAINED_EVENTS,
            max_stream_queue: MAX_STREAM_QUEUE,
            max_sse_event_bytes: MAX_SSE_EVENT_BYTES,
            max_file_part_bytes: MAX_FILE_PART_BYTES,
            max_task_history_bytes: MAX_TASK_HISTORY_BYTES,
            max_task_artifact_bytes: MAX_TASK_ARTIFACT_BYTES,
        }
    }
}

impl A2aLimits {
    /// Return an error if these limits exceed any canonical ceiling or are
    /// zero in a place where zero is not meaningful.
    pub fn validate(&self) -> A2aResult<()> {
        self.check("max_body_bytes", self.max_body_bytes, MAX_BODY_BYTES)?;
        self.check("max_task_history", self.max_task_history, MAX_TASK_HISTORY)?;
        self.check(
            "max_message_parts",
            self.max_message_parts,
            MAX_MESSAGE_PARTS,
        )?;
        self.check(
            "max_part_text_bytes",
            self.max_part_text_bytes,
            MAX_PART_TEXT_BYTES,
        )?;
        self.check("max_data_bytes", self.max_data_bytes, MAX_DATA_BYTES)?;
        self.check(
            "max_artifact_bytes",
            self.max_artifact_bytes,
            MAX_ARTIFACT_BYTES,
        )?;
        self.check("max_skills", self.max_skills, MAX_SKILLS)?;
        self.check("max_stored_tasks", self.max_stored_tasks, MAX_STORED_TASKS)?;
        self.check(
            "max_artifacts_per_task",
            self.max_artifacts_per_task,
            MAX_ARTIFACTS_PER_TASK,
        )?;
        self.check(
            "max_retained_events",
            self.max_retained_events,
            MAX_RETAINED_EVENTS,
        )?;
        self.check("max_stream_queue", self.max_stream_queue, MAX_STREAM_QUEUE)?;
        self.check(
            "max_sse_event_bytes",
            self.max_sse_event_bytes,
            MAX_SSE_EVENT_BYTES,
        )?;
        self.check(
            "max_file_part_bytes",
            self.max_file_part_bytes,
            MAX_FILE_PART_BYTES,
        )?;
        self.check(
            "max_task_history_bytes",
            self.max_task_history_bytes,
            MAX_TASK_HISTORY_BYTES,
        )?;
        self.check(
            "max_task_artifact_bytes",
            self.max_task_artifact_bytes,
            MAX_TASK_ARTIFACT_BYTES,
        )?;
        if self.max_task_ttl.is_zero() || self.max_task_ttl > MAX_TASK_TTL {
            return Err(A2aError::ConfigInvalid {
                reason: format!(
                    "max_task_ttl must be between 1ms and {} seconds",
                    MAX_TASK_TTL.as_secs()
                ),
            });
        }
        Ok(())
    }

    fn check(&self, field: &'static str, actual: usize, max: usize) -> A2aResult<()> {
        if actual == 0 || actual > max {
            return Err(A2aError::BoundsExceeded { field, max, actual });
        }
        Ok(())
    }

    /// Validate a message, including every part and the aggregate size.
    pub fn validate_message(&self, message: &crate::wire::Message) -> A2aResult<()> {
        message.validate(self)
    }

    /// Validate an artifact, including each part and the aggregate size.
    pub fn validate_artifact(&self, artifact: &crate::wire::Artifact) -> A2aResult<()> {
        artifact.validate(self)
    }

    /// Validate a complete task projection.
    pub fn validate_task(&self, task: &crate::wire::A2aTask) -> A2aResult<()> {
        task.validate(self)
    }

    /// Validate a host callback's proposed update.
    pub fn validate_update(&self, update: &crate::wire::A2aTaskUpdate) -> A2aResult<()> {
        update.validate(self)
    }

    /// Upper bound for a task response body. A task projection may legally be
    /// larger than one inbound request because its bounded history and
    /// artifact stores have their own aggregate caps. This is still a hard
    /// bound, not an unbounded read.
    pub fn max_task_response_bytes(&self) -> usize {
        self.max_body_bytes
            .saturating_mul(2)
            .saturating_add(self.max_task_history_bytes)
            .saturating_add(self.max_task_artifact_bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_bounds_are_the_ceiling() {
        let limits = A2aLimits {
            max_body_bytes: MAX_BODY_BYTES + 1,
            ..A2aLimits::default()
        };
        assert_eq!(
            limits.validate().expect_err("raised body bound").code(),
            crate::error::A2aErrorCode::BoundsExceeded
        );

        let limits = A2aLimits {
            max_task_history: 0,
            ..A2aLimits::default()
        };
        assert!(limits.validate().is_err());

        let limits = A2aLimits {
            max_task_ttl: MAX_TASK_TTL + std::time::Duration::from_secs(1),
            ..A2aLimits::default()
        };
        assert_eq!(
            limits.validate().expect_err("raised TTL").code(),
            crate::error::A2aErrorCode::ConfigInvalid
        );
    }

    #[test]
    fn smaller_bounds_are_allowed() {
        let limits = A2aLimits {
            max_body_bytes: 1024,
            max_task_history: 4,
            max_message_parts: 2,
            max_part_text_bytes: 128,
            max_data_bytes: 128,
            max_artifact_bytes: 1024,
            max_skills: 1,
            max_stored_tasks: 2,
            max_task_ttl: Duration::from_secs(1),
            ..A2aLimits::default()
        };
        limits.validate().expect("lower bounds are valid");
    }
}
