use std::time::Duration;

use anyhow::{Result, anyhow};
use vp_protocol::types::OrchestrationPolicy;

/// Bounds are policy validation, not hidden tuning. They keep a malformed RPC
/// payload from creating an unbounded retry loop or a huge wait.
pub const MAX_ATTEMPTS_LIMIT: u32 = 10;
pub const MAX_RETRY_BACKOFF_SECS: u64 = 86_400;
pub const MAX_RETRY_PRIORITY_BOOST: i32 = 100;

#[derive(Debug, thiserror::Error)]
pub enum OrchestrationPolicyError {
    #[error("max_attempts must be between 1 and {MAX_ATTEMPTS_LIMIT}")]
    MaxAttempts,
    #[error("retry_backoff_secs must be between 0 and {MAX_RETRY_BACKOFF_SECS}")]
    Backoff,
    #[error("retry_priority_boost must be between 0 and {MAX_RETRY_PRIORITY_BOOST}")]
    PriorityBoost,
    #[error("auto_retry_failed requires a non-zero max_attempts")]
    AutoRetryAttempts,
}

pub fn validate_policy(policy: &OrchestrationPolicy) -> Result<()> {
    if !(1..=MAX_ATTEMPTS_LIMIT).contains(&policy.max_attempts) {
        return Err(anyhow!(OrchestrationPolicyError::MaxAttempts));
    }
    if policy.retry_backoff_secs > MAX_RETRY_BACKOFF_SECS {
        return Err(anyhow!(OrchestrationPolicyError::Backoff));
    }
    if !(0..=MAX_RETRY_PRIORITY_BOOST).contains(&policy.retry_priority_boost) {
        return Err(anyhow!(OrchestrationPolicyError::PriorityBoost));
    }
    if policy.auto_retry_failed && policy.max_attempts == 0 {
        return Err(anyhow!(OrchestrationPolicyError::AutoRetryAttempts));
    }
    Ok(())
}

pub fn should_auto_retry(policy: &OrchestrationPolicy, status: &str, attempts: u32) -> bool {
    policy.auto_retry_failed && status == "failed" && attempts < policy.max_attempts
}

/// Exponential backoff with a cap. Attempt 1 uses the configured base delay;
/// attempt 2 doubles it; the policy cap is never exceeded.
pub fn retry_delay(policy: &OrchestrationPolicy, attempts: u32) -> Duration {
    if policy.retry_backoff_secs == 0 {
        return Duration::ZERO;
    }
    let exponent = attempts.saturating_sub(1).min(16);
    let multiplier = 1u64.checked_shl(exponent).unwrap_or(u64::MAX);
    let seconds = policy
        .retry_backoff_secs
        .saturating_mul(multiplier)
        .min(MAX_RETRY_BACKOFF_SECS);
    Duration::from_secs(seconds)
}
