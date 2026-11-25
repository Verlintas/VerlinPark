//! Bounded repeated-tool-failure detection.
//!
//! A model can get stuck correcting the same deterministic validation error
//! for many turns. The run loop stops that loop after a small, per-tool number
//! of consecutive failures instead of spending the remaining turn and token
//! budget on near-identical retries.

use std::collections::HashMap;

use crate::tools::ToolOutput;

/// Plan and verdict tools fail deterministically against their schema. Three
/// attempts is enough to fix a field; a fourth unchanged retry is a loop.
const DETERMINISTIC_TOOL_LIMIT: u32 = 3;
/// General tools may fail transiently (a test, a network read, a build). Stop
/// only after a longer uninterrupted failure streak.
const GENERAL_TOOL_LIMIT: u32 = 6;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ToolFailureLoop {
    pub(super) tool: String,
    pub(super) attempts: u32,
    pub(super) last_error: String,
}

#[derive(Default)]
pub(crate) struct ToolFailureGuard {
    failures: HashMap<String, u32>,
}

impl ToolFailureGuard {
    /// Observe one completed call. A successful call resets that tool's
    /// streak. Returns `Some` exactly when the caller must stop the run.
    pub(super) fn observe(&mut self, tool: &str, output: &ToolOutput) -> Option<ToolFailureLoop> {
        if !output.is_error {
            self.failures.remove(tool);
            return None;
        }
        let attempts = self.failures.entry(tool.to_string()).or_insert(0);
        *attempts = attempts.saturating_add(1);
        if *attempts < limit_for(tool) {
            return None;
        }
        let last_error = output
            .failure
            .as_ref()
            .map(|failure| format!("[{}] {}", failure.code.as_str(), failure.message))
            .unwrap_or_else(|| output.content.clone());
        Some(ToolFailureLoop {
            tool: tool.to_string(),
            attempts: *attempts,
            last_error: bounded(last_error),
        })
    }
}

fn limit_for(tool: &str) -> u32 {
    match tool {
        "submit_team_plan" | "submit_verdict" | "plan_fanout" => DETERMINISTIC_TOOL_LIMIT,
        _ => GENERAL_TOOL_LIMIT,
    }
}

fn bounded(value: String) -> String {
    const MAX: usize = 500;
    if value.chars().count() <= MAX {
        return value;
    }
    let mut bounded = value.chars().take(MAX).collect::<String>();
    bounded.push('…');
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tooling::errors::ToolErrorCode;

    fn failure() -> ToolOutput {
        ToolOutput::error_coded(
            "submit_team_plan",
            ToolErrorCode::INVALID_ARGUMENT,
            "bad plan",
        )
    }

    #[test]
    fn deterministic_tools_stop_after_three_consecutive_failures() {
        let mut guard = ToolFailureGuard::default();
        let output = failure();
        assert!(guard.observe("submit_team_plan", &output).is_none());
        assert!(guard.observe("submit_team_plan", &output).is_none());
        let stop = guard
            .observe("submit_team_plan", &output)
            .expect("third failure must stop the loop");
        assert_eq!(stop.tool, "submit_team_plan");
        assert_eq!(stop.attempts, 3);
        assert!(stop.last_error.contains("bad plan"));
    }

    #[test]
    fn success_resets_the_streak() {
        let mut guard = ToolFailureGuard::default();
        let output = failure();
        assert!(guard.observe("submit_team_plan", &output).is_none());
        assert!(guard.observe("submit_team_plan", &output).is_none());
        assert!(
            guard
                .observe("submit_team_plan", &ToolOutput::ok("accepted"))
                .is_none()
        );
        assert!(guard.observe("submit_team_plan", &output).is_none());
        assert!(guard.observe("submit_team_plan", &output).is_none());
    }

    #[test]
    fn general_tools_get_a_longer_streak() {
        let mut guard = ToolFailureGuard::default();
        let output = ToolOutput::error_coded("shell", ToolErrorCode::COMMAND_FAILED, "exit 1");
        for _ in 0..(GENERAL_TOOL_LIMIT - 1) {
            assert!(guard.observe("shell", &output).is_none());
        }
        assert!(guard.observe("shell", &output).is_some());
    }
}
