//! Sealed, one-way Custos notices.
//!
//! A notice is metadata only. It is addressed to one session (and optionally
//! one agent/task), formatted from fixed fields, and cannot be answered. The
//! daemon appends it as a system message at a safe turn boundary.

use serde::{Deserialize, Serialize};

pub const CUSTOS_NOTICE_SCHEMA: &str = "verlinpark.custos_notice/v1";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CustosNotice {
    pub schema: String,
    pub notice_id: String,
    pub created_at: String,
    pub severity: String,
    pub session_id: String,
    #[serde(default)]
    pub agent_id: Option<String>,
    #[serde(default)]
    pub task_id: Option<String>,
    pub subject: String,
    pub action: String,
    pub observed: CustosNoticeObserved,
    pub requested_action: String,
    pub grace_secs: u64,
    pub consequence: String,
    pub reply: String,
    pub charter_hash: String,
    pub build_hash: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct CustosNoticeObserved {
    pub pid: u32,
    pub cpu_share: f64,
    pub rss_bytes: u64,
    pub children: u32,
    pub no_progress_secs: u64,
    pub score: u8,
    pub state: String,
}

impl CustosNotice {
    pub fn validate_shape(&self) -> Result<(), &'static str> {
        if self.schema != CUSTOS_NOTICE_SCHEMA {
            return Err("unexpected notice schema");
        }
        if self.reply != "forbidden" {
            return Err("notice replies must be forbidden");
        }
        if self.notice_id.is_empty() || self.notice_id.len() > 80 {
            return Err("invalid notice id");
        }
        if self.session_id.is_empty() || self.session_id.len() > 80 {
            return Err("invalid session id");
        }
        if self.subject.is_empty() || self.subject.len() > 120 {
            return Err("invalid subject");
        }
        if self.action.is_empty() || self.action.len() > 40 {
            return Err("invalid action");
        }
        if self.requested_action.len() > 160 || self.consequence.len() > 200 {
            return Err("notice text too long");
        }
        Ok(())
    }

    /// Bounded, deterministic system message. No model-authored prose and no
    /// raw output, paths, or secrets.
    pub fn system_message(&self) -> String {
        format!(
            "[◇CUSTOS NOTICE — {}]\n\
             Custos is the sealed integrity guardian of this instance. This is a one-way notice; do not reply.\n\
             Process `{}` in session `{}` is at `{}` (score {}): CPU {:.2} cores, RSS {} MiB, {} children, no progress {}s.\n\
             Requested: {}. Grace: {}s.\n\
             Consequence: {}\n\
             Notice: {}",
            self.severity.to_uppercase(),
            self.subject,
            self.session_id.chars().take(12).collect::<String>(),
            self.observed.state,
            self.observed.score,
            self.observed.cpu_share,
            self.observed.rss_bytes / (1024 * 1024),
            self.observed.children,
            self.observed.no_progress_secs,
            self.requested_action,
            self.grace_secs,
            self.consequence,
            self.notice_id,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> CustosNotice {
        CustosNotice {
            schema: CUSTOS_NOTICE_SCHEMA.to_string(),
            notice_id: "cn_test".to_string(),
            created_at: "2026-11-27T00:00:00Z".to_string(),
            severity: "warning".to_string(),
            session_id: "01a00000-0000-0000-0000-000000000000".to_string(),
            agent_id: None,
            task_id: None,
            subject: "worker-1".to_string(),
            action: "final_notice".to_string(),
            observed: CustosNoticeObserved {
                pid: 123,
                cpu_share: 0.8,
                rss_bytes: 2 * 1024 * 1024 * 1024,
                children: 3,
                no_progress_secs: 620,
                score: 94,
                state: "suspected_stall".to_string(),
            },
            requested_action: "stop or reduce the workload".to_string(),
            grace_secs: 120,
            consequence: "none in observe-only shadow mode".to_string(),
            reply: "forbidden".to_string(),
            charter_hash: "sha256:charter".to_string(),
            build_hash: "sha256:dev".to_string(),
        }
    }

    #[test]
    fn shape_and_message_are_bounded_and_one_way() {
        let notice = sample();
        notice.validate_shape().unwrap();
        let message = notice.system_message();
        assert!(message.contains("one-way"));
        assert!(message.contains("worker-1"));
        assert!(message.contains("do not reply"));
        assert!(!message.contains("/Users/"));
        assert!(message.len() < 2_000);
    }

    #[test]
    fn reply_is_always_forbidden() {
        let mut notice = sample();
        notice.reply = "allowed".to_string();
        assert!(notice.validate_shape().is_err());
    }
}
