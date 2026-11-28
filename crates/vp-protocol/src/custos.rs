//! Shared CUSTOS product-mode and gate types.
//!
//! Observer is the default read-only surface. Advanced is a local root-owned
//! guardian (ADR-0247). These types are pure data: they do not perform IO and
//! do not grant authority.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CustosTier {
    #[default]
    Observer,
    Advanced,
}

impl CustosTier {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observer => "observer",
            Self::Advanced => "advanced",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustosGateReason {
    Unattended,
    SchedulerAutoDispatch,
    LargePlan,
    HighValueProject,
}

impl CustosGateReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unattended => "unattended",
            Self::SchedulerAutoDispatch => "scheduler_auto_dispatch",
            Self::LargePlan => "large_plan",
            Self::HighValueProject => "high_value_project",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CustosGateDecision {
    Allow,
    RequireObserverAcknowledgement,
    RequireAdvanced,
}

impl CustosGateDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::RequireObserverAcknowledgement => "require_observer_acknowledgement",
            Self::RequireAdvanced => "require_advanced",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustosGate {
    pub decision: CustosGateDecision,
    pub tier: CustosTier,
    pub reason: CustosGateReason,
    pub message: String,
}

impl CustosGate {
    pub fn allowed(&self) -> bool {
        self.decision == CustosGateDecision::Allow
    }
}

fn default_large_plan_threshold() -> u32 {
    25
}

/// Per-project CUSTOS policy. Observer can be acknowledged for unattended and
/// large work; high-value projects always require Advanced. Values are
/// operator-authored and bounded; this type never widens approval, sandbox,
/// Verifier, Integrator, or budget gates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectCustosPolicy {
    #[serde(default)]
    pub high_value: bool,
    #[serde(default = "default_large_plan_threshold")]
    pub large_plan_threshold: u32,
    #[serde(default)]
    pub observer_unattended_ack: bool,
    #[serde(default)]
    pub observer_ack_at: Option<String>,
}

impl Default for ProjectCustosPolicy {
    fn default() -> Self {
        Self {
            high_value: false,
            large_plan_threshold: default_large_plan_threshold(),
            observer_unattended_ack: false,
            observer_ack_at: None,
        }
    }
}

impl ProjectCustosPolicy {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=1_000).contains(&self.large_plan_threshold) {
            return Err("large_plan_threshold must be between 1 and 1000".to_string());
        }
        if self
            .observer_ack_at
            .as_deref()
            .is_some_and(|value| value.trim().is_empty())
        {
            return Err("observer_ack_at cannot be empty when present".to_string());
        }
        Ok(())
    }

    pub fn is_large_plan(&self, task_count: usize) -> bool {
        task_count > self.large_plan_threshold as usize
    }
}

/// Read-only description of the root-owned Advanced marker. Signature
/// verification is the root crate's responsibility; this type only carries the
/// bounded public fields used by the Observer/status surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustosAdvancedMarker {
    pub schema: String,
    pub install_id: String,
    pub build_hash: String,
    pub charter_hash: String,
    pub root_key_id: String,
    pub created_at: String,
    #[serde(default)]
    pub tier: Option<String>,
    #[serde(default)]
    pub epoch: Option<u64>,
    #[serde(default)]
    pub installed_at_unix: Option<i64>,
    #[serde(default)]
    pub signature: Option<String>,
}

pub const ADVANCED_MARKER_SCHEMA: &str = "verlinpark.custos_advanced/v1";

impl CustosAdvancedMarker {
    pub fn is_compatible(&self) -> bool {
        self.schema == ADVANCED_MARKER_SCHEMA
            && self
                .tier
                .as_deref()
                .is_none_or(|tier| tier.eq_ignore_ascii_case("advanced"))
            && !self.install_id.trim().is_empty()
            && !self.build_hash.trim().is_empty()
            && !self.charter_hash.trim().is_empty()
            && !self.root_key_id.trim().is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_policy_is_observer_safe_and_bounded() {
        let policy = ProjectCustosPolicy::default();
        assert!(!policy.high_value);
        assert!(!policy.observer_unattended_ack);
        assert_eq!(policy.large_plan_threshold, 25);
        assert!(policy.validate().is_ok());
        assert!(!policy.is_large_plan(25));
        assert!(policy.is_large_plan(26));
        assert!(
            ProjectCustosPolicy {
                large_plan_threshold: 0,
                ..ProjectCustosPolicy::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn advanced_marker_requires_all_public_fields() {
        let marker = CustosAdvancedMarker {
            schema: ADVANCED_MARKER_SCHEMA.into(),
            install_id: "install".into(),
            build_hash: "build".into(),
            charter_hash: "charter".into(),
            root_key_id: "key".into(),
            created_at: "2026-11-29T00:00:00Z".into(),
            tier: Some("advanced".into()),
            epoch: Some(1),
            installed_at_unix: Some(1_764_460_800),
            signature: None,
        };
        assert!(marker.is_compatible());
        let mut broken = marker;
        broken.charter_hash.clear();
        assert!(!broken.is_compatible());
    }
}
