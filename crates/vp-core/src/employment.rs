//! S6 hiring, promotion, transfer, retirement, and performance engine surface.
//!
//! The store owns the transactional invariants; this module selects bounded
//! policy inputs and exposes explicit, read-only or approval-classified
//! operations. No method here is called by a scheduler, a model, or an
//! automatic retry path. No method here changes role/profile execution policy,
//! sandboxing, Verifier, merge, capacity, or budget gates.

use anyhow::Result;
use time::OffsetDateTime;
use vp_protocol::types::{
    AgentEmploymentReplayParams, AgentEmploymentReplayReport, AgentHireApproveParams,
    AgentHireListParams, AgentHireListResult, AgentHireParams, AgentHireRejectParams,
    AgentHireResult, AgentIdentityError, AgentPerformance, AgentPerformanceParams,
    AgentPerformanceReplayParams, AgentPerformanceReplayReport, AgentPerformanceSummaryParams,
    AgentPerformanceSummaryResult, AgentPromoteParams, AgentPromoteResult, AgentRetireParams,
    AgentRetireResult, AgentRetirementCandidate, AgentRetirementCandidatesParams,
    AgentRetirementCandidatesResult, AgentRetirementDecisionResult, AgentRetirementPolicy,
    AgentRetirementPolicyResult, AgentRetirementProposalListParams,
    AgentRetirementProposalListResult, AgentRetirementProposalParams, AgentRetirementProposeParams,
    AgentRetirementProposeResult, AgentRetirementRejectParams, AgentTransferParams,
    AgentTransferResult, SetAgentRetirementPolicyParams, agent_identity_codes,
};

use crate::Engine;

fn performance_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    AgentIdentityError::new(code, message).into()
}

impl Engine {
    // -----------------------------------------------------------------
    // Policy
    // -----------------------------------------------------------------

    pub async fn get_agent_retirement_policy(
        &self,
        project_id: &str,
    ) -> Result<AgentRetirementPolicyResult> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::get_retirement_policy(&store, &project_id)
        })
        .await?
    }

    pub async fn set_agent_retirement_policy(
        &self,
        params: SetAgentRetirementPolicyParams,
    ) -> Result<AgentRetirementPolicyResult> {
        let store = self.store.clone();
        let project_id = params.project_id.trim().to_string();
        let policy = params.policy;
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::set_retirement_policy(&store, &project_id, &policy)
        })
        .await?
    }

    // -----------------------------------------------------------------
    // Hiring
    // -----------------------------------------------------------------

    pub async fn hire_agent_identity(&self, params: AgentHireParams) -> Result<AgentHireResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::hire_agent(&store, &params)
        })
        .await?
    }

    pub async fn approve_agent_hire(
        &self,
        params: AgentHireApproveParams,
    ) -> Result<AgentHireResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::approve_agent_hire(&store, &params)
        })
        .await?
    }

    pub async fn reject_agent_hire(
        &self,
        params: AgentHireRejectParams,
    ) -> Result<AgentHireResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::reject_agent_hire(&store, &params)
        })
        .await?
    }

    pub async fn list_agent_hire_requests(
        &self,
        params: AgentHireListParams,
    ) -> Result<AgentHireListResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::list_agent_hire_requests(&store, &params)
        })
        .await?
    }

    // -----------------------------------------------------------------
    // Promotion and transfer
    // -----------------------------------------------------------------

    pub async fn promote_agent_identity(
        &self,
        params: AgentPromoteParams,
    ) -> Result<AgentPromoteResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::promote_agent(&store, &params)
        })
        .await?
    }

    pub async fn transfer_agent_identity(
        &self,
        params: AgentTransferParams,
    ) -> Result<AgentTransferResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::transfer_agent(&store, &params)
        })
        .await?
    }

    // -----------------------------------------------------------------
    // Retirement
    // -----------------------------------------------------------------

    pub async fn retire_agent(&self, params: AgentRetireParams) -> Result<AgentRetireResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::retire_agent(&store, &params)
        })
        .await?
    }

    pub async fn agent_retirement_proposals(
        &self,
        params: AgentRetirementProposalListParams,
    ) -> Result<AgentRetirementProposalListResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::list_retirement_proposals(&store, &params)
        })
        .await?
    }

    pub async fn approve_agent_retirement_proposal(
        &self,
        params: AgentRetirementProposalParams,
    ) -> Result<AgentRetirementDecisionResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::approve_retirement_proposal(&store, &params)
        })
        .await?
    }

    pub async fn reject_agent_retirement_proposal(
        &self,
        params: AgentRetirementRejectParams,
    ) -> Result<AgentRetirementDecisionResult> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::reject_retirement_proposal(&store, &params)
        })
        .await?
    }

    // -----------------------------------------------------------------
    // Performance ledger and candidate evaluation
    // -----------------------------------------------------------------

    pub async fn agent_performance(
        &self,
        params: AgentPerformanceParams,
    ) -> Result<AgentPerformance> {
        let store = self.store.clone();
        let project_id = params.project_id.trim().to_string();
        let identity_id = params.identity_id.trim().to_string();
        let now = OffsetDateTime::now_utc();
        tokio::task::spawn_blocking(move || {
            let policy =
                vp_store::store::employment::get_retirement_policy(&store, &project_id)?.policy;
            let lookback = params.lookback_days.unwrap_or(policy.lookback_days);
            let identity = store.get_agent_identity(&identity_id)?.ok_or_else(|| {
                performance_error(
                    agent_identity_codes::IDENTITY_NOT_FOUND,
                    format!("agent identity not found: {identity_id}"),
                )
            })?;
            if identity.project_id != project_id {
                return Err(performance_error(
                    agent_identity_codes::IDENTITY_PROJECT_MISMATCH,
                    "identity belongs to another project",
                ));
            }
            let rows = vp_store::store::employment::compute_agent_performance(
                &store,
                &project_id,
                lookback,
                now,
            )?;
            rows.into_iter()
                .find(|row| row.identity_id == identity_id)
                .ok_or_else(|| {
                    performance_error(
                        agent_identity_codes::PERFORMANCE_INVALID,
                        "performance row missing for an identity in this project",
                    )
                })
        })
        .await?
    }

    pub async fn agent_performance_summary(
        &self,
        params: AgentPerformanceSummaryParams,
    ) -> Result<AgentPerformanceSummaryResult> {
        let store = self.store.clone();
        let now = OffsetDateTime::now_utc();
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::performance_summary(&store, &params, now)
        })
        .await?
    }

    pub async fn agent_performance_replay(
        &self,
        params: AgentPerformanceReplayParams,
    ) -> Result<AgentPerformanceReplayReport> {
        let store = self.store.clone();
        let now = OffsetDateTime::now_utc();
        let project_id = params.project_id;
        let repair = params.repair;
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::performance_replay(&store, &project_id, repair, now)
        })
        .await?
    }

    pub async fn agent_retirement_candidates(
        &self,
        params: AgentRetirementCandidatesParams,
    ) -> Result<AgentRetirementCandidatesResult> {
        let store = self.store.clone();
        let now = OffsetDateTime::now_utc();
        tokio::task::spawn_blocking(move || {
            let project_id = params.project_id.trim().to_string();
            let policy =
                vp_store::store::employment::get_retirement_policy(&store, &project_id)?.policy;
            let lookback = params.lookback_days.unwrap_or(policy.lookback_days);
            let identities = store.list_agent_identities(&project_id, None, None)?;
            let evaluated = identities
                .iter()
                .filter(|identity| identity.status != "retired" && identity.origin == "local")
                .count() as u64;
            let performances = vp_store::store::employment::compute_agent_performance(
                &store,
                &project_id,
                lookback,
                now,
            )?;
            let last =
                vp_store::store::employment::last_retirement_proposal_times(&store, &project_id)?;
            let local_identities = identities
                .iter()
                .filter(|identity| identity.origin == "local")
                .cloned()
                .collect::<Vec<_>>();
            let candidates = vp_store::store::employment::retirement_candidates(
                &project_id,
                &local_identities,
                &performances,
                &policy,
                &last,
                now,
                params.limit,
            );
            let limited = params
                .limit
                .map(|limit| candidates.len() >= limit as usize)
                .unwrap_or(false);
            Ok(AgentRetirementCandidatesResult {
                project_id,
                generated_at: vp_protocol::now_rfc3339(),
                policy_enabled: policy.enabled,
                policy,
                evaluated,
                candidates,
                limited,
            })
        })
        .await?
    }

    pub async fn propose_agent_retirement(
        &self,
        params: AgentRetirementProposeParams,
    ) -> Result<AgentRetirementProposeResult> {
        let store = self.store.clone();
        let now = OffsetDateTime::now_utc();
        tokio::task::spawn_blocking(move || {
            let identity = store
                .get_agent_identity(params.identity_id.trim())?
                .ok_or_else(|| {
                    performance_error(
                        agent_identity_codes::IDENTITY_NOT_FOUND,
                        format!("agent identity not found: {}", params.identity_id.trim()),
                    )
                })?;
            let policy =
                vp_store::store::employment::get_retirement_policy(&store, &identity.project_id)?
                    .policy;
            if !policy.enabled {
                return Err(performance_error(
                    agent_identity_codes::RETIREMENT_POLICY_DISABLED,
                    "agent retirement policy is disabled",
                ));
            }
            let performances = vp_store::store::employment::compute_agent_performance(
                &store,
                &identity.project_id,
                policy.lookback_days,
                now,
            )?;
            let Some(performance) = performances
                .iter()
                .find(|row| row.identity_id == identity.id)
            else {
                return Err(performance_error(
                    agent_identity_codes::PERFORMANCE_INVALID,
                    "performance row missing for this identity",
                ));
            };
            let candidate =
                vp_store::store::employment::retirement_candidate(&identity, performance, &policy)
                    .ok_or_else(|| {
                        performance_error(
                            agent_identity_codes::RETIREMENT_NOT_ELIGIBLE,
                            "identity has not crossed a configured retirement threshold",
                        )
                    })?;
            let (proposal, event, already_pending) =
                vp_store::store::employment::create_retirement_proposal(
                    &store,
                    &candidate,
                    params.operator.as_deref(),
                )?;
            Ok(AgentRetirementProposeResult {
                proposal,
                candidate,
                already_pending,
                changed: event.is_some(),
            })
        })
        .await?
    }

    pub async fn replay_agent_employment(
        &self,
        params: AgentEmploymentReplayParams,
    ) -> Result<AgentEmploymentReplayReport> {
        let store = self.store.clone();
        let project_id = params.project_id;
        let repair = params.repair;
        tokio::task::spawn_blocking(move || {
            vp_store::store::employment::employment_replay(&store, &project_id, repair)
        })
        .await?
    }

    /// Used by `db-rebuild` after identities and session evidence are restored.
    pub async fn rebuild_agent_performance_projection(&self, project_id: &str) -> Result<()> {
        let store = self.store.clone();
        let project_id = project_id.to_string();
        let now = OffsetDateTime::now_utc();
        tokio::task::spawn_blocking(move || {
            let policy =
                vp_store::store::employment::get_retirement_policy(&store, &project_id)?.policy;
            vp_store::store::employment::rebuild_agent_performance(
                &store,
                &project_id,
                policy.lookback_days,
                now,
            )?;
            Ok(())
        })
        .await?
    }

    /// Project-scoped candidate list helper for tests and future UIs. The
    /// public RPC path is `agent.retirement.candidates`.
    pub async fn retirement_candidates_for_project(
        &self,
        project_id: &str,
    ) -> Result<Vec<AgentRetirementCandidate>> {
        Ok(self
            .agent_retirement_candidates(AgentRetirementCandidatesParams {
                project_id: project_id.to_string(),
                ..AgentRetirementCandidatesParams::default()
            })
            .await?
            .candidates)
    }

    pub async fn retirement_policy_for_project(
        &self,
        project_id: &str,
    ) -> Result<AgentRetirementPolicy> {
        Ok(self.get_agent_retirement_policy(project_id).await?.policy)
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use time::{Duration, OffsetDateTime};
    use vp_protocol::types::{
        AgentIdentity, AgentPerformance, AgentPerformanceEvidence, AgentRetirementPolicy,
        AgentRetirementReason, agent_performance_evidence_reasons as perf_reasons,
        agent_retirement_reasons,
    };

    fn identity(id: &str, role: &str) -> AgentIdentity {
        AgentIdentity {
            id: id.into(),
            project_id: "prj-1".into(),
            role: role.into(),
            department: Some("platform".into()),
            component: Some("auth".into()),
            title: Some(format!("{role} title")),
            profile: Some("coding".into()),
            status: "active".into(),
            generation: 2,
            created_at: "2026-11-01T00:00:00Z".into(),
            updated_at: "2026-11-01T00:00:00Z".into(),
            origin: "local".into(),
            ..AgentIdentity::default()
        }
    }

    fn performance(id: &str, completed: u64, failures: u64) -> AgentPerformance {
        AgentPerformance {
            project_id: "prj-1".into(),
            identity_id: id.into(),
            role: "worker".into(),
            status: "active".into(),
            lookback_days: 30,
            completed_tasks: completed,
            failed_tasks: failures,
            consecutive_failures: failures,
            verifier_rejections: failures,
            unsubstantiated_handoffs: failures,
            false_consensus: failures,
            evidence: vec![AgentPerformanceEvidence {
                kind: "task".into(),
                reference: format!("task-{id}"),
                reason: Some(perf_reasons::TASK_FAILED.into()),
                at: Some("2026-11-12T00:00:00Z".into()),
                ..AgentPerformanceEvidence::default()
            }],
            ..AgentPerformance::default()
        }
    }

    fn policy() -> AgentRetirementPolicy {
        AgentRetirementPolicy {
            enabled: true,
            lookback_days: 30,
            min_completed_tasks: 1,
            max_consecutive_failures: 2,
            max_verifier_rejections: 2,
            max_unsubstantiated_handoffs: 2,
            max_false_consensus: 2,
            cooldown_secs: 60,
            require_approval: true,
        }
    }

    #[test]
    fn candidate_requires_completed_work_and_any_enabled_threshold() {
        let policy = policy();
        let identity = identity("id-1", "worker");
        assert!(
            vp_store::store::employment::retirement_candidate(
                &identity,
                &performance("id-1", 0, 4),
                &policy
            )
            .is_none()
        );
        let candidate = vp_store::store::employment::retirement_candidate(
            &identity,
            &performance("id-1", 1, 2),
            &policy,
        )
        .unwrap();
        assert!(
            candidate
                .reasons
                .iter()
                .any(|reason| { reason.code == agent_retirement_reasons::CONSECUTIVE_FAILURES })
        );
        assert_eq!(candidate.identity_id, "id-1");
        assert!(candidate.generation >= 1);
        assert!(!candidate.evidence.is_empty());
    }

    #[test]
    fn disabled_policy_and_retired_identity_never_produce_candidates() {
        let identity = identity("id-1", "worker");
        let performance = performance("id-1", 5, 5);
        let mut disabled = policy();
        disabled.enabled = false;
        assert!(
            vp_store::store::employment::retirement_candidate(&identity, &performance, &disabled)
                .is_none()
        );
        let mut retired = identity.clone();
        retired.status = "retired".into();
        assert!(
            vp_store::store::employment::retirement_candidate(&retired, &performance, &policy())
                .is_none()
        );
        let mut zero = policy();
        zero.max_consecutive_failures = 0;
        zero.max_verifier_rejections = 0;
        zero.max_unsubstantiated_handoffs = 0;
        zero.max_false_consensus = 0;
        assert!(
            vp_store::store::employment::retirement_candidate(&identity, &performance, &zero)
                .is_none()
        );
    }

    #[test]
    fn candidate_ranking_is_severity_then_identity_and_cooldown_aware() {
        let policy = policy();
        let identities = vec![
            identity("id-a", "worker"),
            identity("id-b", "worker"),
            identity("id-c", "manager"),
        ];
        let performances = vec![
            performance("id-a", 1, 2),
            performance("id-b", 1, 5),
            performance("id-c", 1, 2),
        ];
        let now = OffsetDateTime::now_utc();
        let candidates = vp_store::store::employment::retirement_candidates(
            "prj-1",
            &identities,
            &performances,
            &policy,
            &HashMap::new(),
            now,
            Some(10),
        );
        assert_eq!(candidates.len(), 3);
        assert_eq!(candidates[0].identity_id, "id-b");
        assert_eq!(candidates[1].identity_id, "id-a");
        assert_eq!(candidates[2].identity_id, "id-c");

        let mut last = HashMap::new();
        last.insert("id-b".to_string(), now - Duration::seconds(30));
        let cooled = vp_store::store::employment::retirement_candidates(
            "prj-1",
            &identities,
            &performances,
            &policy,
            &last,
            now,
            Some(10),
        );
        assert!(
            !cooled
                .iter()
                .any(|candidate| candidate.identity_id == "id-b")
        );
        let expired = vp_store::store::employment::retirement_candidates(
            "prj-1",
            &identities,
            &performances,
            &policy,
            &last,
            now + Duration::seconds(31),
            Some(10),
        );
        assert!(
            expired
                .iter()
                .any(|candidate| candidate.identity_id == "id-b")
        );
    }

    #[test]
    fn disabled_policy_returns_no_candidates_and_limit_is_bounded() {
        let mut disabled = policy();
        disabled.enabled = false;
        let identities = vec![identity("id-a", "worker")];
        let performances = vec![performance("id-a", 1, 5)];
        assert!(
            vp_store::store::employment::retirement_candidates(
                "prj-1",
                &identities,
                &performances,
                &disabled,
                &HashMap::new(),
                OffsetDateTime::now_utc(),
                None,
            )
            .is_empty()
        );
        let many = (0..40)
            .map(|index| identity(&format!("id-{index:02}"), "worker"))
            .collect::<Vec<_>>();
        let many_performance = many
            .iter()
            .map(|identity| performance(&identity.id, 1, 5))
            .collect::<Vec<_>>();
        let candidates = vp_store::store::employment::retirement_candidates(
            "prj-1",
            &many,
            &many_performance,
            &policy(),
            &HashMap::new(),
            OffsetDateTime::now_utc(),
            None,
        );
        assert_eq!(
            candidates.len(),
            vp_protocol::types::AGENT_RETIREMENT_MAX_CANDIDATES as usize
        );
    }

    #[test]
    fn hash_is_stable_for_the_same_bounded_evidence() {
        let identity = identity("id-1", "worker");
        let candidate = vp_store::store::employment::retirement_candidate(
            &identity,
            &performance("id-1", 1, 2),
            &policy(),
        )
        .unwrap();
        let first = vp_store::store::employment::retirement_candidate_hash(&candidate);
        let second = vp_store::store::employment::retirement_candidate_hash(&candidate);
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
        let mut changed = candidate.clone();
        changed.reasons[0].count += 1;
        assert_ne!(
            first,
            vp_store::store::employment::retirement_candidate_hash(&changed)
        );
    }

    #[test]
    fn threshold_reason_preserves_count_and_threshold() {
        let identity = identity("id-1", "worker");
        let candidate = vp_store::store::employment::retirement_candidate(
            &identity,
            &performance("id-1", 1, 3),
            &policy(),
        )
        .unwrap();
        let reason: &AgentRetirementReason = candidate
            .reasons
            .iter()
            .find(|reason| reason.code == agent_retirement_reasons::CONSECUTIVE_FAILURES)
            .unwrap();
        assert_eq!(reason.count, 3);
        assert_eq!(reason.threshold, 2);
        assert!(candidate.severity >= 1_000);
    }
}
