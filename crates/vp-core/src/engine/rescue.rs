//! Deterministic R2b rescue/allocation preview.
//!
//! A rescue grant is a short-lived, event-only operator release of the
//! completion reserve for work that is already near completion (a clean
//! commit/Verifier/merge queue entry). It never extends a budget, raises a hard
//! cap, changes capacity, approves anything, or bypasses the Verifier and
//! deterministic Integrator. A looping task with no progress is never eligible.

use super::*;

use time::format_description::well_known::Rfc3339;
use vp_protocol::types::{
    BudgetMode, RESCUE_SCHEMA, RESCUE_USED_SCHEMA, RescueCandidate, RescueError, RescueGrant,
    RescueParams, RescueReserve, RescueResult, RescueStatus, SESSION_RESCUE_EVENT,
    SESSION_RESCUE_USED_EVENT, rescue_codes,
};

/// Grants are deliberately short-lived. They release the reserve for one safe
/// turn boundary, not for a new work program.
const RESCUE_TTL_SECS: i64 = 1_800;
const MAX_RESCUE_REASON_CHARS: usize = 500;
const MAX_RESCUE_RECORDED_BY_CHARS: usize = 120;
const MAX_RESCUE_DETAIL_CHARS: usize = 200;
const MAX_RESCUE_CANDIDATES: usize = 20;
const MAX_RESCUE_RECOMMENDATIONS: usize = 8;
const DEFAULT_RESCUE_REASON: &str = "operator reserve release for near-completion work";

#[derive(Debug, Clone, serde::Deserialize)]
struct RescueUsedPayload {
    #[serde(default)]
    schema: String,
    #[serde(default)]
    grant_id: String,
    #[serde(default)]
    used_at: String,
}

struct RescueUsed {
    grant_id: String,
    used_at: String,
    seq: u64,
}

impl Engine {
    async fn rescue_session(&self, session_id: &str) -> Result<Session> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))
    }

    /// Newest valid rescue consumption receipt. A malformed payload is treated
    /// as absent; the raw event remains in the audit stream.
    async fn latest_rescue_use(&self, session_id: &str) -> Result<Option<RescueUsed>> {
        let Some(event) = self
            .latest_event_of_kind(session_id, SESSION_RESCUE_USED_EVENT)
            .await?
        else {
            return Ok(None);
        };
        let Ok(payload) = serde_json::from_value::<RescueUsedPayload>(event.payload) else {
            return Ok(None);
        };
        if payload.schema != RESCUE_USED_SCHEMA || payload.grant_id.trim().is_empty() {
            return Ok(None);
        }
        let used_at = if payload.used_at.trim().is_empty() {
            event.created_at
        } else {
            payload.used_at
        };
        Ok(Some(RescueUsed {
            grant_id: payload.grant_id,
            used_at,
            seq: event.seq,
        }))
    }

    /// The newest rescue grant is active while its schema/id match the session,
    /// it has not expired, and no newer consumption receipt names it. Expiry
    /// and consumption are derived from event data, so tests never sleep.
    pub(crate) async fn active_rescue_grant(
        &self,
        session_id: &str,
    ) -> Result<Option<RescueGrant>> {
        let Some(event) = self
            .latest_event_of_kind(session_id, SESSION_RESCUE_EVENT)
            .await?
        else {
            return Ok(None);
        };
        let Ok(grant) = serde_json::from_value::<RescueGrant>(event.payload) else {
            return Ok(None);
        };
        if grant.schema != RESCUE_SCHEMA
            || grant.id.trim().is_empty()
            || grant.session_id != session_id
        {
            return Ok(None);
        }
        if rescue_expired(&grant.expires_at) {
            return Ok(None);
        }
        if let Some(used) = self.latest_rescue_use(session_id).await?
            && used.grant_id == grant.id
            && used.seq > event.seq
        {
            return Ok(None);
        }
        Ok(Some(grant))
    }

    /// Consume the active grant at a safe turn boundary. This is a one-shot
    /// receipt; a second call (or a different grant id) is a no-op.
    pub(crate) async fn consume_rescue_grant(
        &self,
        session_id: &str,
        grant_id: &str,
    ) -> Result<bool> {
        // One receipt per grant: concurrent Leader/Manager/Worker/Labour
        // boundaries must not both observe the same unused grant.
        let _rescue_guard = self.rescue_lock.lock().await;
        let Some(grant) = self.active_rescue_grant(session_id).await? else {
            return Ok(false);
        };
        if grant.id != grant_id {
            return Ok(false);
        }
        self.append_event(
            session_id,
            SESSION_RESCUE_USED_EVENT,
            serde_json::json!({
                "schema": RESCUE_USED_SCHEMA,
                "grant_id": grant.id,
                "used_at": now_rfc3339(),
            }),
        )
        .await?;
        Ok(true)
    }

    /// Bounded read-only rescue/allocation preview. It performs no mutation and
    /// appends no event; only [`Engine::grant_rescue`] writes the grant.
    pub async fn rescue_status(&self, session_id: &str) -> Result<RescueStatus> {
        let session = self.rescue_session(session_id).await?;
        let budget = self.session_budget(session_id).await?;
        let readiness = self.session_readiness(session_id).await?;
        let contract = self.run_contract_view(session_id).await?;
        let active = self.active_rescue_grant(session_id).await?;
        let used = self.latest_rescue_use(session_id).await?;
        let no_progress_turns = budget
            .state
            .as_ref()
            .map(|state| state.turns_without_progress)
            .unwrap_or(0);

        let (base_eligible, base_reason) =
            rescue_eligibility(&session, &budget, &readiness, no_progress_turns);
        let (eligible, reason) = if active.is_some() {
            (false, "active_grant".to_string())
        } else {
            (base_eligible, base_reason)
        };

        let reserve = RescueReserve {
            completion_reserve_ratio: contract.completion_reserve_ratio,
            remaining_tokens: budget.remaining_tokens,
            remaining_cost_usd: budget.remaining_cost_usd,
            remaining_secs: budget.remaining_secs,
            hard_token_cap: budget.hard_token_cap,
            hard_cost_cap_usd: budget.hard_cost_cap_usd,
            hard_wall_clock_secs: budget.hard_wall_clock_secs,
        };

        let queue = self.list_merge_queue(session_id).await?;
        let near_completion = rescue_near_completion(&session, &readiness, &queue);
        let looping = if no_progress_turns > 0 {
            vec![RescueCandidate {
                kind: "session".to_string(),
                id: session.id.clone(),
                status: "no_progress".to_string(),
                detail: bounded_rescue_text(
                    &format!(
                        "{no_progress_turns} turn(s) without progress evidence; rescue must not fund a loop"
                    ),
                    MAX_RESCUE_DETAIL_CHARS,
                ),
                commit: readiness.latest_commit.clone(),
            }]
        } else {
            Vec::new()
        };
        let recommendations = rescue_recommendations(
            active.is_some(),
            eligible,
            no_progress_turns,
            readiness.level.as_str(),
            budget.mode,
            reason.as_str(),
        );

        Ok(RescueStatus {
            session_id: session_id.to_string(),
            active: active.is_some(),
            grant: active,
            used_at: used.as_ref().map(|used| used.used_at.clone()),
            used_grant_id: used.map(|used| used.grant_id),
            eligible,
            reason,
            readiness,
            no_progress_turns,
            near_completion,
            looping,
            recommendations,
            reserve,
            generated_at: now_rfc3339(),
        })
    }

    /// Append one explicit rescue grant for an inactive, fixed-budget session
    /// whose work is already near completion. The grant releases only the
    /// completion reserve for its short TTL; it is not a top-up or a merge.
    pub async fn grant_rescue(&self, params: RescueParams) -> Result<RescueResult> {
        if !params.confirm {
            return Err(rescue_error(
                rescue_codes::RESCUE_CONFIRM_REQUIRED,
                "granting a rescue requires explicit confirmation (confirm=true / --yes); it releases the completion reserve but does not change budgets, caps, approvals, or verification",
            ));
        }

        // Serialize manual grants with the automatic path and with each other.
        // A concurrent manual grant must not create a second active grant or
        // race the automatic one-shot count.
        let _rescue_guard = self.rescue_lock.lock().await;
        let status = self.rescue_status(&params.session_id).await?;
        if status.active {
            return Err(rescue_error(
                rescue_codes::RESCUE_ACTIVE,
                format!(
                    "session {} already has an active rescue grant",
                    status.session_id
                ),
            ));
        }
        if !status.eligible {
            let code = match status.reason.as_str() {
                "imported" => rescue_codes::RESCUE_IMPORTED,
                "session_active" => rescue_codes::RESCUE_SESSION_ACTIVE,
                _ => rescue_codes::RESCUE_NOT_ELIGIBLE,
            };
            return Err(rescue_error(
                code,
                format!(
                    "session {} is not eligible for rescue: {}",
                    status.session_id, status.reason
                ),
            ));
        }

        let session = self.rescue_session(&params.session_id).await?;
        let reason = params
            .reason
            .as_deref()
            .map(|value| bounded_rescue_text(value, MAX_RESCUE_REASON_CHARS))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| DEFAULT_RESCUE_REASON.to_string());
        let recorded_by = params
            .recorded_by
            .as_deref()
            .map(|value| bounded_rescue_text(value, MAX_RESCUE_RECORDED_BY_CHARS))
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "operator".to_string());
        let grant = build_rescue_grant(
            &session,
            &status.readiness,
            status.no_progress_turns,
            recorded_by,
            reason,
        );

        self.append_event(
            &session.id,
            SESSION_RESCUE_EVENT,
            serde_json::to_value(&grant)?,
        )
        .await?;

        let status = self.rescue_status(&session.id).await?;
        Ok(RescueResult {
            session_id: session.id,
            grant,
            status,
        })
    }

    /// Turn-boundary automatic rescue owned by an explicit project policy.
    ///
    /// The auto path is deliberately one-shot per session: any prior
    /// `session.resource.rescue` grant (manual or automatic) consumes the
    /// automatic allowance. The grant still only releases the completion
    /// reserve for the normal 30-minute TTL; it never raises a hard cap,
    /// changes attention/approval policy, approves, verifies, merges, or routes
    /// a model. Ineligible work, including dirty/blocked/looping/auto/unlimited
    /// sessions, imported archives, no-headroom sessions, and sessions without
    /// a latest commit, is never auto-granted.
    pub(crate) async fn maybe_auto_rescue(&self, session_id: &str, reason: &str) -> Result<bool> {
        if !reason.trim().starts_with("completion_reserve_") {
            return Ok(false);
        }
        let session = self.rescue_session(session_id).await?;
        let policy = self
            .get_project_run_policy(&session.project_id)
            .await?
            .policy;
        if !policy.auto_rescue {
            return Ok(false);
        }
        // Readiness and budget reads can be slow, so do them before taking the
        // one-shot critical section. The turn loop is concurrent for
        // Leader/Manager/Worker/Labour; only the count and append must be
        // serialized or two children can both observe zero grants. Manual
        // `session.rescue` deliberately stays outside this process-wide lock.
        let budget = self.session_budget(&session.id).await?;
        let readiness = self.session_readiness(&session.id).await?;
        let no_progress_turns = budget
            .state
            .as_ref()
            .map(|state| state.turns_without_progress)
            .unwrap_or(0);
        let (eligible, _) =
            rescue_eligibility_internal(&session, &budget, &readiness, no_progress_turns, true);
        if !eligible {
            return Ok(false);
        }
        // Releasing a token/cost completion reserve cannot help when a
        // configured economic dimension has no headroom left. Time-only
        // headroom must not consume the session's one automatic grant.
        let token_headroom = budget
            .remaining_tokens
            .is_none_or(|remaining| remaining > 0);
        let cost_headroom = budget
            .remaining_cost_usd
            .is_none_or(|remaining| remaining > 0.0);
        if !token_headroom || !cost_headroom {
            return Ok(false);
        }
        let _rescue_guard = self.rescue_lock.lock().await;
        if self
            .count_events_of_kind(&session.id, SESSION_RESCUE_EVENT)
            .await?
            > 0
        {
            return Ok(false);
        }
        let grant = build_rescue_grant(
            &session,
            &readiness,
            no_progress_turns,
            "policy:auto_rescue".to_string(),
            "automatic completion-reserve reallocation (project policy)".to_string(),
        );
        self.append_event(
            &session.id,
            SESSION_RESCUE_EVENT,
            serde_json::to_value(&grant)?,
        )
        .await?;
        Ok(true)
    }
}

fn build_rescue_grant(
    session: &Session,
    readiness: &vp_protocol::types::ReadinessSnapshot,
    no_progress_turns: u32,
    recorded_by: String,
    reason: String,
) -> RescueGrant {
    RescueGrant {
        schema: RESCUE_SCHEMA.to_string(),
        id: format!("rs-{}", Uuid::now_v7()),
        session_id: session.id.clone(),
        project_id: session.project_id.clone(),
        recorded_at: now_rfc3339(),
        expires_at: rescue_timestamp_after(RESCUE_TTL_SECS),
        recorded_by,
        reason,
        readiness_level: readiness.level.clone(),
        latest_commit: readiness.latest_commit.clone(),
        no_progress_turns,
    }
}

fn rescue_eligibility(
    session: &Session,
    budget: &vp_protocol::types::SessionBudgetStatus,
    readiness: &vp_protocol::types::ReadinessSnapshot,
    no_progress_turns: u32,
) -> (bool, String) {
    rescue_eligibility_internal(session, budget, readiness, no_progress_turns, false)
}

/// Shared eligibility for the operator path and the turn-boundary auto-rescue
/// path. Only the auto path may treat the still-`running` session as eligible;
/// every other gate (imported/dirty/blocked/looping/auto/unlimited/headroom/
/// no-commit) is identical.
fn rescue_eligibility_internal(
    session: &Session,
    budget: &vp_protocol::types::SessionBudgetStatus,
    readiness: &vp_protocol::types::ReadinessSnapshot,
    no_progress_turns: u32,
    allow_running: bool,
) -> (bool, String) {
    if session.status == "imported" {
        return (false, "imported".to_string());
    }
    let allowed_status = matches!(
        session.status.as_str(),
        "paused" | "idle" | "failed" | "cancelled"
    ) || (allow_running && session.status == "running");
    if !allowed_status {
        let reason = if session.status == "draft" {
            "no_commit"
        } else {
            "session_active"
        };
        return (false, reason.to_string());
    }
    // A looping task is never rescued, regardless of budget mode: extra
    // reserve would only fund more of the same unproductive turn.
    if no_progress_turns > 0 {
        return (false, "looping".to_string());
    }
    // Auto owns its own reserve/extension and unlimited already has no
    // economic ceiling; neither needs a reserve release.
    if budget.unlimited || budget.mode == BudgetMode::Unlimited {
        return (false, "unlimited".to_string());
    }
    if budget.mode == BudgetMode::Auto {
        return (false, "auto_budget".to_string());
    }
    let usable_readiness = matches!(
        readiness.level.as_str(),
        "committed" | "tests_passed" | "verified" | "partial"
    );
    if !usable_readiness {
        return (false, readiness.level.clone());
    }
    if readiness
        .latest_commit
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty()
    {
        return (false, "no_commit".to_string());
    }
    let has_headroom = budget.remaining_tokens.unwrap_or(0) > 0
        || budget.remaining_cost_usd.unwrap_or(0.0) > 0.0
        || budget.remaining_secs.unwrap_or(0) > 0;
    if !has_headroom {
        return (false, "no_headroom".to_string());
    }
    (true, "eligible".to_string())
}

fn rescue_near_completion(
    session: &Session,
    readiness: &vp_protocol::types::ReadinessSnapshot,
    queue: &[vp_protocol::types::MergeQueueEntry],
) -> Vec<RescueCandidate> {
    let pending = queue
        .iter()
        .filter(|entry| matches!(entry.status.as_str(), "queued" | "merging"))
        .take(MAX_RESCUE_CANDIDATES)
        .map(|entry| {
            let task = entry.task_id.as_deref().unwrap_or("untasked");
            let detail = entry.detail.as_deref().unwrap_or("awaiting integration");
            RescueCandidate {
                kind: "merge_queue".to_string(),
                id: entry.id.clone(),
                status: entry.status.clone(),
                detail: bounded_rescue_text(
                    &format!("task {task}: {detail}"),
                    MAX_RESCUE_DETAIL_CHARS,
                ),
                commit: Some(entry.commit.clone()),
            }
        })
        .collect::<Vec<_>>();
    if !pending.is_empty() {
        return pending;
    }
    let Some(commit) = readiness
        .latest_commit
        .as_deref()
        .map(str::trim)
        .filter(|commit| !commit.is_empty())
    else {
        return Vec::new();
    };
    let detail = format!(
        "latest {} commit awaiting verification/merge; {} remaining work item(s)",
        readiness.level,
        readiness.remaining_work.len()
    );
    vec![RescueCandidate {
        kind: "session".to_string(),
        id: session.id.clone(),
        status: readiness.level.clone(),
        detail: bounded_rescue_text(&detail, MAX_RESCUE_DETAIL_CHARS),
        commit: Some(commit.to_string()),
    }]
}

fn rescue_recommendations(
    active: bool,
    eligible: bool,
    no_progress_turns: u32,
    readiness_level: &str,
    budget_mode: BudgetMode,
    reason: &str,
) -> Vec<String> {
    let mut recommendations = Vec::new();
    let mut push = |recommendation: &str| {
        if recommendations.len() < MAX_RESCUE_RECOMMENDATIONS
            && !recommendations
                .iter()
                .any(|existing| existing == recommendation)
        {
            recommendations.push(recommendation.to_string());
        }
    };
    if active {
        push("rescue_active");
    }
    if eligible {
        push("rescue_reserve");
        push("reallocate_attention_to_near_completion");
    }
    if no_progress_turns > 0 {
        push("stop_or_replan");
        push("do_not_rescue_looping_work");
    }
    if readiness_level == "dirty" {
        push("commit_or_reconcile_first");
    }
    match budget_mode {
        BudgetMode::Auto => push("top_up_or_adjust_budget"),
        BudgetMode::Unlimited => push("continue"),
        BudgetMode::Fixed => {}
    }
    if reason == "no_headroom" {
        push("top_up");
    }
    recommendations
}

fn rescue_error(code: &'static str, message: impl Into<String>) -> anyhow::Error {
    RescueError::new(code, message).into()
}

fn rescue_timestamp_after(seconds: i64) -> String {
    (time::OffsetDateTime::now_utc() + time::Duration::seconds(seconds))
        .format(&Rfc3339)
        .unwrap_or_else(|_| now_rfc3339())
}

fn rescue_expired(expires_at: &str) -> bool {
    let Ok(expires_at) = time::OffsetDateTime::parse(expires_at.trim(), &Rfc3339) else {
        return true;
    };
    expires_at <= time::OffsetDateTime::now_utc()
}

/// Trim and bound a single operator/derived line by character count. The
/// ellipsis is included in the bound.
fn bounded_rescue_text(value: &str, max_chars: usize) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() || max_chars == 0 {
        return String::new();
    }
    let mut chars = trimmed.chars();
    let mut bounded = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        bounded.pop();
        bounded.push('…');
    }
    bounded
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_protocol::types::{
        AutoBudgetPolicy, ProjectRunPolicy, SessionAutoBudgetState, SetProjectRunPolicyParams,
        SetSessionBudgetParams,
    };

    fn init_repo(path: &Path) {
        std::fs::write(path.join("README.md"), "hello\n").unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "test@verlinpark.local"],
            vec!["config", "user.name", "VerlinPark Test"],
            vec!["add", "."],
            vec!["commit", "-q", "-m", "initial"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(path)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    fn commit_work(session: &Session) {
        let worktree = session.worktree.as_deref().expect("session worktree");
        std::fs::create_dir_all(Path::new(worktree).join("src")).unwrap();
        std::fs::write(
            Path::new(worktree).join("src/lib.rs"),
            "pub fn rescue() {}\n",
        )
        .unwrap();
        for args in [
            vec!["add", "-A"],
            vec!["commit", "-q", "-m", "near completion"],
        ] {
            let status = Command::new("git")
                .arg("-C")
                .arg(worktree)
                .args(args)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    async fn fixture(token_budget: Option<u64>) -> (TempDir, TempDir, Engine, Session) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("rescue"))
            .unwrap();
        let session = store.create_session(&project.id, "rescue status").unwrap();
        let engine = Engine::new(store);
        let session = engine.ensure_session_worktree(&session.id).await.unwrap();
        if let Some(tokens) = token_budget {
            engine
                .set_session_budget(SetSessionBudgetParams {
                    session_id: session.id.clone(),
                    mode: Some(BudgetMode::Fixed),
                    token_budget: Some(tokens),
                    ..SetSessionBudgetParams::default()
                })
                .await
                .unwrap();
        }
        commit_work(&session);
        engine
            .update_session_status(&session.id, "idle")
            .await
            .unwrap();
        (data, repo, engine, session)
    }

    async fn auto_fixture() -> (TempDir, TempDir, Engine, Session) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("rescue-auto"))
            .unwrap();
        let policy = AutoBudgetPolicy {
            enabled: true,
            initial_token_budget: Some(1_000),
            hard_token_cap: Some(5_000),
            ..AutoBudgetPolicy::default()
        };
        let engine = Engine::new(store);
        let session = engine
            .create_session_with_budget_options(
                project.id,
                "auto rescue".into(),
                None,
                None,
                None,
                Some(BudgetMode::Auto),
                Some(policy),
                0,
                false,
            )
            .await
            .unwrap();
        let session = engine.ensure_session_worktree(&session.id).await.unwrap();
        commit_work(&session);
        engine
            .update_session_status(&session.id, "idle")
            .await
            .unwrap();
        (data, repo, engine, session)
    }

    async fn record_usage(engine: &Engine, session_id: &str, input: u64, output: u64) {
        engine
            .record_agent_usage(
                session_id,
                "test-agent",
                "Test Agent",
                "leader",
                None,
                None,
                "test-model",
                input,
                output,
                false,
                0.0,
            )
            .await
            .unwrap();
    }

    fn params(session: &Session) -> RescueParams {
        RescueParams {
            session_id: session.id.clone(),
            confirm: true,
            ..RescueParams::default()
        }
    }

    fn code(error: &anyhow::Error) -> Option<&'static str> {
        error.downcast_ref::<RescueError>().map(|typed| typed.code)
    }

    async fn set_auto_rescue(engine: &Engine, project_id: &str, enabled: bool) {
        let current = engine
            .get_project_run_policy(project_id)
            .await
            .unwrap()
            .policy;
        engine
            .set_project_run_policy(SetProjectRunPolicyParams {
                project_id: project_id.to_string(),
                policy: ProjectRunPolicy {
                    auto_rescue: enabled,
                    ..current
                },
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn auto_rescue_policy_is_off_by_default_and_one_shot() {
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        record_usage(&engine, &session.id, 9_000, 0).await;
        let decision = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert!(decision.should_block(), "{decision:?}");
        assert_eq!(decision.reason, "completion_reserve_tokens");
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, &decision.reason)
                .await
                .unwrap(),
            "the project policy is off by default; the turn must pause"
        );
        assert_eq!(
            engine
                .count_events_of_kind(&session.id, SESSION_RESCUE_EVENT)
                .await
                .unwrap(),
            0
        );

        set_auto_rescue(&engine, &session.project_id, true).await;
        assert!(
            engine
                .maybe_auto_rescue(&session.id, &decision.reason)
                .await
                .unwrap(),
            "an eligible running session is auto-granted once"
        );
        let event = engine
            .latest_event_of_kind(&session.id, SESSION_RESCUE_EVENT)
            .await
            .unwrap()
            .expect("automatic rescue event");
        assert_eq!(event.payload["recorded_by"], "policy:auto_rescue");
        assert_eq!(
            event.payload["reason"],
            "automatic completion-reserve reallocation (project policy)"
        );
        let grant = engine
            .active_rescue_grant(&session.id)
            .await
            .unwrap()
            .expect("active auto grant");
        assert_eq!(grant.recorded_by, "policy:auto_rescue");
        assert!(grant.id.starts_with("rs-"));

        let decision = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert!(!decision.should_block(), "{decision:?}");
        assert!(!decision.reason.starts_with("completion_reserve_"));
        assert!(
            engine
                .consume_rescue_grant(&session.id, &grant.id)
                .await
                .unwrap()
        );
        assert!(
            !engine
                .consume_rescue_grant(&session.id, &grant.id)
                .await
                .unwrap(),
            "the grant is consumed exactly once"
        );
        assert_eq!(
            engine
                .count_events_of_kind(&session.id, SESSION_RESCUE_EVENT)
                .await
                .unwrap(),
            1
        );
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, "completion_reserve_tokens")
                .await
                .unwrap(),
            "the automatic/manual grant cap is one per session"
        );
    }

    #[tokio::test]
    async fn a_manual_grant_consumes_the_automatic_allowance() {
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        set_auto_rescue(&engine, &session.project_id, true).await;
        engine.grant_rescue(params(&session)).await.unwrap();
        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        record_usage(&engine, &session.id, 9_000, 0).await;
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, "completion_reserve_tokens")
                .await
                .unwrap(),
            "a prior manual grant blocks the automatic one-shot"
        );
        assert_eq!(
            engine
                .count_events_of_kind(&session.id, SESSION_RESCUE_EVENT)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn auto_rescue_is_serialized_across_concurrent_turn_boundaries() {
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        set_auto_rescue(&engine, &session.project_id, true).await;
        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        record_usage(&engine, &session.id, 9_000, 0).await;

        let (first, second) = tokio::join!(
            engine.maybe_auto_rescue(&session.id, "completion_reserve_tokens"),
            engine.maybe_auto_rescue(&session.id, "completion_reserve_tokens"),
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert!(
            first ^ second,
            "exactly one concurrent boundary may append the automatic grant"
        );
        assert_eq!(
            engine
                .count_events_of_kind(&session.id, SESSION_RESCUE_EVENT)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn auto_rescue_never_grants_for_ineligible_work() {
        // Dirty worktree.
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        set_auto_rescue(&engine, &session.project_id, true).await;
        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        record_usage(&engine, &session.id, 9_000, 0).await;
        let worktree = session.worktree.as_deref().expect("session worktree");
        std::fs::write(
            Path::new(worktree).join("src/pending.rs"),
            "pub fn wip() {}\n",
        )
        .unwrap();
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, "completion_reserve_tokens")
                .await
                .unwrap()
        );

        // Looping work.
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        set_auto_rescue(&engine, &session.project_id, true).await;
        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        record_usage(&engine, &session.id, 9_000, 0).await;
        engine
            .update_session_auto_budget_state(
                &session.id,
                SessionAutoBudgetState {
                    turns_without_progress: 2,
                    ..SessionAutoBudgetState::default()
                },
            )
            .await
            .unwrap();
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, "completion_reserve_tokens")
                .await
                .unwrap()
        );

        // Auto budget owns its own reserve.
        let (_data, _repo, engine, session) = auto_fixture().await;
        set_auto_rescue(&engine, &session.project_id, true).await;
        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, "completion_reserve_tokens")
                .await
                .unwrap()
        );

        // Unlimited sessions have no economic reserve to release.
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        set_auto_rescue(&engine, &session.project_id, true).await;
        engine
            .set_session_unlimited(&session.id, true)
            .await
            .unwrap();
        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        record_usage(&engine, &session.id, 9_000, 0).await;
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, "completion_reserve_tokens")
                .await
                .unwrap()
        );

        // No remaining headroom.
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        set_auto_rescue(&engine, &session.project_id, true).await;
        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        record_usage(&engine, &session.id, 10_000, 0).await;
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, "completion_reserve_tokens")
                .await
                .unwrap()
        );

        // Time-only headroom must not consume the one automatic grant when the
        // token dimension that paused the turn has no headroom left.
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        set_auto_rescue(&engine, &session.project_id, true).await;
        engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: session.id.clone(),
                mode: Some(BudgetMode::Fixed),
                token_budget: Some(10_000),
                time_budget_secs: Some(3_600),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap();
        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        record_usage(&engine, &session.id, 10_000, 0).await;
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, "completion_reserve_tokens")
                .await
                .unwrap()
        );

        // Imported archives are read-only even when the policy opts in.
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        set_auto_rescue(&engine, &session.project_id, true).await;
        engine
            .store()
            .update_session_status(&session.id, "imported")
            .unwrap();
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, "completion_reserve_tokens")
                .await
                .unwrap()
        );

        // No latest commit.
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("auto-no-commit"))
            .unwrap();
        let session = store.create_session(&project.id, "no commit").unwrap();
        let engine = Engine::new(store);
        set_auto_rescue(&engine, &project.id, true).await;
        engine
            .set_session_budget(SetSessionBudgetParams {
                session_id: session.id.clone(),
                mode: Some(BudgetMode::Fixed),
                token_budget: Some(10_000),
                ..SetSessionBudgetParams::default()
            })
            .await
            .unwrap();
        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        record_usage(&engine, &session.id, 9_000, 0).await;
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, "completion_reserve_tokens")
                .await
                .unwrap()
        );

        // A non-completion-reserve reason is never auto-rescued.
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        set_auto_rescue(&engine, &session.project_id, true).await;
        engine
            .store()
            .update_session_status(&session.id, "running")
            .unwrap();
        assert!(
            !engine
                .maybe_auto_rescue(&session.id, "resource_cap_tokens")
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn rescue_preview_is_read_only_and_lists_near_completion() {
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        let before = engine
            .count_events_of_kind(&session.id, SESSION_RESCUE_EVENT)
            .await
            .unwrap();
        let status = engine.rescue_status(&session.id).await.unwrap();
        assert!(status.eligible, "{status:?}");
        assert_eq!(status.reason, "eligible");
        assert!(!status.active);
        assert!(status.grant.is_none());
        assert_eq!(status.readiness.level, "committed");
        assert_eq!(status.near_completion.len(), 1);
        assert_eq!(status.near_completion[0].kind, "session");
        assert_eq!(
            status.near_completion[0].commit.as_deref(),
            status.readiness.latest_commit.as_deref()
        );
        assert!(
            status
                .recommendations
                .contains(&"rescue_reserve".to_string())
        );
        assert!(
            status
                .recommendations
                .contains(&"reallocate_attention_to_near_completion".to_string())
        );
        assert_eq!(status.reserve.completion_reserve_ratio, 0.2);
        assert_eq!(status.reserve.remaining_tokens, Some(10_000));
        assert_eq!(status.reserve.hard_token_cap, None);
        assert_eq!(
            engine
                .count_events_of_kind(&session.id, SESSION_RESCUE_EVENT)
                .await
                .unwrap(),
            before,
            "the preview must not append an event"
        );

        let entry = engine
            .enqueue_merge(
                &session.id,
                "agent-1",
                "Worker",
                Some("T1".into()),
                "feature",
                "0123456789abcdef0123456789abcdef01234567",
                session.base_commit.as_deref().unwrap_or(""),
                Some("awaiting integration".into()),
            )
            .await
            .unwrap();
        let status = engine.rescue_status(&session.id).await.unwrap();
        assert_eq!(status.readiness.level, "verified", "{status:?}");
        assert_eq!(status.near_completion.len(), 1);
        let candidate = &status.near_completion[0];
        assert_eq!(candidate.kind, "merge_queue");
        assert_eq!(candidate.id, entry.id);
        assert_eq!(candidate.status, "queued");
        assert!(candidate.detail.contains("T1"));
        assert_eq!(candidate.commit.as_deref(), Some(entry.commit.as_str()));
    }

    #[tokio::test]
    async fn rescue_refuses_dirty_blocked_empty_auto_unlimited_and_imported_sessions() {
        // Dirty uncommitted work.
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        let worktree = session.worktree.as_deref().expect("session worktree");
        std::fs::write(
            Path::new(worktree).join("src/pending.rs"),
            "pub fn wip() {}\n",
        )
        .unwrap();
        let dirty = engine.rescue_status(&session.id).await.unwrap();
        assert_eq!(dirty.readiness.level, "dirty");
        assert!(!dirty.eligible);
        assert_eq!(dirty.reason, "dirty");
        assert!(
            dirty
                .recommendations
                .contains(&"commit_or_reconcile_first".to_string())
        );
        let error = engine
            .grant_rescue(RescueParams {
                reason: Some("too early".into()),
                ..params(&session)
            })
            .await
            .expect_err("dirty work is not eligible");
        assert_eq!(code(&error), Some(rescue_codes::RESCUE_NOT_ELIGIBLE));

        // Unresolved effect receipt.
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        let plan = crate::effect::classify_tool_effect(
            "remote_write",
            &json!({"path": "out/app.txt", "content": "deploy"}),
            crate::permissions::RiskLevel::External,
            false,
            &vp_protocol::types::EffectPolicy::default(),
        )
        .unwrap()
        .expect("remote write plan");
        let prepare = engine
            .prepare_tool_effect(&session.id, Some("T1"), "agent-1", "", &plan)
            .await
            .unwrap();
        let intent = match prepare {
            crate::effect::EffectPrepare::Proceed(receipt) => receipt,
            other => panic!("expected proceed, got {other:?}"),
        };
        engine.begin_effect(intent).await.unwrap();
        let blocked = engine.rescue_status(&session.id).await.unwrap();
        assert_eq!(blocked.readiness.level, "blocked");
        assert_eq!(blocked.reason, "blocked");
        assert!(!blocked.eligible);

        // No budget headroom.
        let (_data, _repo, engine, session) = fixture(None).await;
        let empty = engine.rescue_status(&session.id).await.unwrap();
        assert_eq!(empty.reason, "no_headroom");
        assert!(!empty.eligible);
        assert!(empty.recommendations.contains(&"top_up".to_string()));

        // Auto budget owns its own reserve/extension.
        let (_data, _repo, engine, session) = auto_fixture().await;
        let auto = engine.rescue_status(&session.id).await.unwrap();
        assert_eq!(auto.reason, "auto_budget");
        assert!(!auto.eligible);
        assert!(
            auto.recommendations
                .contains(&"top_up_or_adjust_budget".to_string())
        );

        // Unlimited has no economic reserve to release.
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        engine
            .set_session_unlimited(&session.id, true)
            .await
            .unwrap();
        let unlimited = engine.rescue_status(&session.id).await.unwrap();
        assert_eq!(unlimited.reason, "unlimited");
        assert!(!unlimited.eligible);
        assert!(unlimited.recommendations.contains(&"continue".to_string()));

        // Imported archives are read-only.
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        engine
            .store()
            .update_session_status(&session.id, "imported")
            .unwrap();
        let imported = engine.rescue_status(&session.id).await.unwrap();
        assert_eq!(imported.reason, "imported");
        assert!(!imported.eligible);
        let error = engine
            .grant_rescue(params(&session))
            .await
            .expect_err("imported sessions are read-only");
        assert_eq!(code(&error), Some(rescue_codes::RESCUE_IMPORTED));
    }

    #[tokio::test]
    async fn rescue_refuses_active_session_and_looping_work() {
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        for status in ["running", "queued", "waiting_input", "landed"] {
            engine
                .store()
                .update_session_status(&session.id, status)
                .unwrap();
            let active = engine.rescue_status(&session.id).await.unwrap();
            assert_eq!(active.reason, "session_active", "status {status}");
            assert!(!active.eligible);
            let error = engine
                .grant_rescue(params(&session))
                .await
                .expect_err("an active session is not eligible");
            assert_eq!(
                code(&error),
                Some(rescue_codes::RESCUE_SESSION_ACTIVE),
                "status {status}"
            );
        }

        let (_data, _repo, engine, session) = fixture(None).await;
        engine
            .store()
            .update_session_status(&session.id, "draft")
            .unwrap();
        let draft = engine.rescue_status(&session.id).await.unwrap();
        assert_eq!(draft.reason, "no_commit");
        assert!(!draft.eligible);

        // A looping Auto session is refused before the auto-budget reason so
        // the operator sees the real risk: no progress, not a funding mode.
        let (_data, _repo, engine, session) = auto_fixture().await;
        engine
            .update_session_auto_budget_state(
                &session.id,
                SessionAutoBudgetState {
                    turns_without_progress: 3,
                    total_turns: 5,
                    ..SessionAutoBudgetState::default()
                },
            )
            .await
            .unwrap();
        let looping = engine.rescue_status(&session.id).await.unwrap();
        assert_eq!(looping.no_progress_turns, 3);
        assert_eq!(looping.reason, "looping");
        assert!(!looping.eligible);
        assert_eq!(looping.looping.len(), 1);
        assert_eq!(looping.looping[0].status, "no_progress");
        assert!(
            looping
                .recommendations
                .contains(&"do_not_rescue_looping_work".to_string())
        );
        assert!(
            looping
                .recommendations
                .contains(&"stop_or_replan".to_string())
        );
        let error = engine
            .grant_rescue(params(&session))
            .await
            .expect_err("looping work is never rescued");
        assert_eq!(code(&error), Some(rescue_codes::RESCUE_NOT_ELIGIBLE));
    }

    #[tokio::test]
    async fn rescue_grant_is_bounded_event_backed_and_one_shot() {
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;

        let unconfirmed = engine
            .grant_rescue(RescueParams {
                reason: Some("finish it".into()),
                confirm: false,
                ..params(&session)
            })
            .await
            .expect_err("confirmation is required");
        assert_eq!(
            code(&unconfirmed),
            Some(rescue_codes::RESCUE_CONFIRM_REQUIRED)
        );
        assert_eq!(
            engine
                .count_events_of_kind(&session.id, SESSION_RESCUE_EVENT)
                .await
                .unwrap(),
            0
        );

        let reason = "r".repeat(800);
        let recorded_by = "o".repeat(300);
        let granted = engine
            .grant_rescue(RescueParams {
                reason: Some(reason),
                recorded_by: Some(recorded_by),
                ..params(&session)
            })
            .await
            .unwrap();
        assert_eq!(granted.grant.schema, RESCUE_SCHEMA);
        assert!(granted.grant.id.starts_with("rs-"));
        assert_eq!(granted.grant.session_id, session.id);
        assert_eq!(granted.grant.project_id, session.project_id);
        assert_eq!(granted.grant.readiness_level, "committed");
        assert_eq!(granted.grant.no_progress_turns, 0);
        assert!(granted.grant.reason.chars().count() <= MAX_RESCUE_REASON_CHARS);
        assert!(granted.grant.recorded_by.chars().count() <= MAX_RESCUE_RECORDED_BY_CHARS);
        let expires = time::OffsetDateTime::parse(&granted.grant.expires_at, &Rfc3339).unwrap();
        let recorded = time::OffsetDateTime::parse(&granted.grant.recorded_at, &Rfc3339).unwrap();
        assert_eq!((expires - recorded).whole_seconds(), RESCUE_TTL_SECS);
        assert!(granted.status.active);
        assert_eq!(granted.status.reason, "active_grant");
        assert!(!granted.status.eligible);
        assert!(
            granted
                .status
                .recommendations
                .contains(&"rescue_active".to_string())
        );

        let event = engine
            .latest_event_of_kind(&session.id, SESSION_RESCUE_EVENT)
            .await
            .unwrap()
            .expect("rescue event");
        assert_eq!(event.kind, SESSION_RESCUE_EVENT);
        assert_eq!(event.payload["schema"], RESCUE_SCHEMA);
        assert_eq!(event.payload["id"], granted.grant.id);
        assert_eq!(
            engine.active_rescue_grant(&session.id).await.unwrap(),
            Some(granted.grant.clone())
        );

        let duplicate = engine
            .grant_rescue(params(&session))
            .await
            .expect_err("an active grant cannot be replaced");
        assert_eq!(code(&duplicate), Some(rescue_codes::RESCUE_ACTIVE));

        assert!(
            !engine
                .consume_rescue_grant(&session.id, "rs-other")
                .await
                .unwrap()
        );
        assert!(
            engine
                .consume_rescue_grant(&session.id, &granted.grant.id)
                .await
                .unwrap()
        );
        assert!(
            !engine
                .consume_rescue_grant(&session.id, &granted.grant.id)
                .await
                .unwrap(),
            "consumption is idempotent"
        );
        assert!(
            engine
                .active_rescue_grant(&session.id)
                .await
                .unwrap()
                .is_none()
        );
        let status = engine.rescue_status(&session.id).await.unwrap();
        assert!(!status.active);
        assert_eq!(
            status.used_grant_id.as_deref(),
            Some(granted.grant.id.as_str())
        );
        assert!(status.used_at.is_some());
        assert!(
            status.eligible,
            "the consumed grant no longer blocks a new one"
        );
    }

    #[tokio::test]
    async fn rescue_consumption_is_one_receipt_across_concurrent_boundaries() {
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        let granted = engine.grant_rescue(params(&session)).await.unwrap();
        let (first, second) = tokio::join!(
            engine.consume_rescue_grant(&session.id, &granted.grant.id),
            engine.consume_rescue_grant(&session.id, &granted.grant.id),
        );
        let first = first.unwrap();
        let second = second.unwrap();
        assert!(
            first ^ second,
            "exactly one concurrent consumer may persist the receipt"
        );
        assert_eq!(
            engine
                .count_events_of_kind(&session.id, SESSION_RESCUE_USED_EVENT)
                .await
                .unwrap(),
            1
        );
    }

    #[tokio::test]
    async fn rescue_grant_expiry_is_derived_from_event_data() {
        let (_data, _repo, engine, session) = fixture(Some(10_000)).await;
        let granted = engine.grant_rescue(params(&session)).await.unwrap();

        let mut expired = granted.grant.clone();
        expired.expires_at = "2000-01-01T00:00:00Z".into();
        engine
            .append_event(
                &session.id,
                SESSION_RESCUE_EVENT,
                serde_json::to_value(&expired).unwrap(),
            )
            .await
            .unwrap();
        assert!(
            engine
                .active_rescue_grant(&session.id)
                .await
                .unwrap()
                .is_none(),
            "an expired grant is inactive"
        );
        let status = engine.rescue_status(&session.id).await.unwrap();
        assert!(!status.active);
        assert_ne!(status.reason, "active_grant");

        // A malformed newest grant is inactive rather than a crash.
        engine
            .append_event(&session.id, SESSION_RESCUE_EVENT, json!({"bad": true}))
            .await
            .unwrap();
        assert!(
            engine
                .active_rescue_grant(&session.id)
                .await
                .unwrap()
                .is_none()
        );

        // A mismatched-session payload is inactive too.
        let mut mismatched = granted.grant;
        mismatched.session_id = "ses-other".into();
        mismatched.expires_at = rescue_timestamp_after(600);
        engine
            .append_event(
                &session.id,
                SESSION_RESCUE_EVENT,
                serde_json::to_value(&mismatched).unwrap(),
            )
            .await
            .unwrap();
        assert!(
            engine
                .active_rescue_grant(&session.id)
                .await
                .unwrap()
                .is_none()
        );
    }

    #[tokio::test]
    async fn turn_governor_releases_only_the_completion_reserve() {
        let (_data, _repo, engine, session) = fixture(Some(1_000)).await;
        record_usage(&engine, &session.id, 850, 0).await;

        let before = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert_eq!(before.action, "defer", "{before:?}");
        assert_eq!(before.reason, "completion_reserve_tokens");
        assert_eq!(before.details["completion_reserve_ratio"], 0.2);
        assert!(before.details.get("rescue").is_none());

        let granted = engine.grant_rescue(params(&session)).await.unwrap();
        let rescued = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert_eq!(rescued.action, "warn", "{rescued:?}");
        assert_eq!(rescued.reason, "soft_resource_limit");
        assert_eq!(rescued.details["completion_reserve_ratio"], 0.0);
        assert_eq!(rescued.details["rescue"]["grant_id"], granted.grant.id);
        assert_eq!(
            rescued.details["rescue"]["original_completion_reserve_ratio"],
            0.2
        );
        assert_eq!(
            rescued.details["rescue"]["expires_at"],
            granted.grant.expires_at
        );

        // A hard cap still pauses even while the rescue grant is active.
        record_usage(&engine, &session.id, 150, 0).await;
        let hard = engine
            .resource_governor_turn_decision(&session.id)
            .await
            .unwrap();
        assert_eq!(hard.action, "defer", "{hard:?}");
        assert_eq!(hard.reason, "resource_cap_tokens");
        assert!(
            hard.details.get("rescue").is_some(),
            "the active grant is still reported, but it cannot remove a hard cap"
        );
    }
}
