//! Read-only R4 trade-off and quality projection.
//!
//! The view is deliberately conservative: profile rows are documented posture
//! text, projected turns/cost are the existing budget forecast repeated without
//! per-profile guarantees, and confidence is an explicit heuristic over the
//! evidence the session already produced. The Verifier remains authoritative.

use super::*;

use std::collections::HashSet;
use vp_protocol::types::{
    CalibratedConfidence, ProfileTradeoff, QualityReview, SessionTradeoffsParams, TradeoffView,
};

const MAX_TRADEOFF_TEXT: usize = 240;
const MAX_TRADEOFF_PATHS: usize = 50;
const MAX_TRADEOFF_BLOCKERS: usize = 20;
const MAX_TRADEOFF_RECOMMENDATIONS: usize = 12;
const MAX_TRADEOFF_WARNINGS: usize = 12;

struct ProfileText {
    primary_goal: &'static str,
    reserve: f64,
    attention_posture: &'static str,
    quality_posture: &'static str,
}

const PROFILE_TEXT: [(&str, ProfileText); 5] = [
    (
        "quality",
        ProfileText {
            primary_goal: "trustworthy completion",
            reserve: 0.25,
            attention_posture: "high tolerance; prefer review before risky work",
            quality_posture: "tests + Verifier + reviewability",
        },
    ),
    (
        "balanced",
        ProfileText {
            primary_goal: "default",
            reserve: 0.20,
            attention_posture: "moderate; batch routine approvals",
            quality_posture: "tests + Verifier",
        },
    ),
    (
        "fast",
        ProfileText {
            primary_goal: "deadline",
            reserve: 0.15,
            attention_posture: "low tolerance; defer optional exploration",
            quality_posture: "do not skip Verifier; skip optional checks",
        },
    ),
    (
        "cheap",
        ProfileText {
            primary_goal: "cost",
            reserve: 0.15,
            attention_posture: "low; avoid extra interrupts",
            quality_posture: "tests + Verifier; fewer retries",
        },
    ),
    (
        "unattended",
        ProfileText {
            primary_goal: "no interruptions",
            reserve: 0.20,
            attention_posture: "zero; deny risky actions instead of asking",
            quality_posture: "tests + Verifier; stop on uncertainty",
        },
    ),
];

impl Engine {
    async fn tradeoffs_session(&self, session_id: &str) -> Result<Session> {
        let store = self.store.clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.get_session(&id))
            .await??
            .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))
    }

    /// Bounded, read-only R4 trade-off view. It appends no event, changes no
    /// profile, and never routes a model or changes an approval/merge gate.
    pub async fn session_tradeoffs(&self, params: SessionTradeoffsParams) -> Result<TradeoffView> {
        let session = self.tradeoffs_session(&params.session_id).await?;
        let budget = self.session_budget(&session.id).await?;
        let contract = self.run_contract_view(&session.id).await?;
        let readiness = self.session_readiness(&session.id).await?;
        let queue = self.task_queue(None, Some(session.id.clone())).await?.tasks;
        let merge_queue = self.list_merge_queue(&session.id).await?;
        let approvals = self.list_approvals(Some(session.id.clone())).await?;
        let effects = self.session_effects(&session.id, None, false, 200).await?;
        let usage = self.usage_totals(&session.id).await?;
        let plan = self.get_team_plan(&session.id).await?;

        let quality = quality_review(&readiness, &queue, &merge_queue, &approvals, &effects);
        let confidence = calibrated_confidence(&readiness, &quality, usage.calls);
        let current_profile = contract.profile.as_str();
        let reserve_pressure = has_reserve_pressure(&budget, &contract);
        let recommended = recommended_profile(&quality, &effects, reserve_pressure);
        let mut recommendations = tradeoff_recommendations(
            &contract,
            &readiness,
            &approvals,
            &effects,
            reserve_pressure,
        );
        if let Some(recommended) = recommended
            && recommended != current_profile
            && !recommendations
                .iter()
                .any(|item| item == &format!("consider_profile:{recommended}"))
        {
            if recommendations.len() >= MAX_TRADEOFF_RECOMMENDATIONS {
                recommendations.pop();
            }
            recommendations.push(format!("consider_profile:{recommended}"));
        }
        let mut warnings: Vec<String> = Vec::new();
        if plan.is_none() {
            warnings.push(
                "No TeamPlan is stored; task counts and reviewability are partial.".to_string(),
            );
        }
        if budget.effective_cost_budget_usd.is_none() && budget.hard_cost_cap_usd.is_none() {
            warnings.push(
                "No USD cap/prices are configured; projected cost is not a guarantee.".to_string(),
            );
        }
        if readiness.latest_test_passed == Some(false) {
            warnings.push("The latest test evidence is failing.".to_string());
        } else if readiness.latest_test.is_none() {
            warnings.push("No post-commit test evidence is recorded.".to_string());
        }
        if readiness.latest_commit.is_some() && readiness.verifier.is_none() {
            warnings.push("The latest commit has no Verifier verdict.".to_string());
        }
        if approvals
            .iter()
            .any(|approval| approval.status == "pending")
        {
            warnings.push("Pending approvals still require operator attention.".to_string());
        }
        if effects.blocked {
            warnings.push("Unresolved effects block merge until reconciled.".to_string());
        }
        warnings.truncate(MAX_TRADEOFF_WARNINGS);
        for warning in &mut warnings {
            *warning = bounded_tradeoff_text(warning, MAX_TRADEOFF_TEXT);
        }

        let projected_turns = budget.forecast.runway_turns;
        let projected_cost_usd = budget.forecast.runway_cost_usd;
        let profiles = PROFILE_TEXT
            .iter()
            .map(|(profile, text)| ProfileTradeoff {
                profile: (*profile).to_string(),
                primary_goal: text.primary_goal.to_string(),
                completion_reserve_ratio: text.reserve,
                attention_posture: text.attention_posture.to_string(),
                quality_posture: text.quality_posture.to_string(),
                projected_turns,
                projected_cost_usd,
                recommended: profile_badge(profile, recommended, current_profile),
            })
            .collect::<Vec<_>>();

        Ok(TradeoffView {
            session_id: session.id,
            generated_at: now_rfc3339(),
            current_profile: current_profile.to_string(),
            effective_completion_reserve_ratio: contract.completion_reserve_ratio,
            reserve_source: contract.source.clone(),
            profiles,
            quality,
            confidence,
            recommendations,
            warnings,
        })
    }
}

fn quality_review(
    readiness: &vp_protocol::types::ReadinessSnapshot,
    queue: &[vp_protocol::types::ScheduledTask],
    merge_queue: &[vp_protocol::types::MergeQueueEntry],
    approvals: &[vp_protocol::types::Approval],
    effects: &vp_protocol::types::SessionEffectsResult,
) -> QualityReview {
    let completed_tasks = queue
        .iter()
        .filter(|task| task.status == "completed")
        .count() as u32;
    let total_tasks = queue.len() as u32;
    let merged_entries = merge_queue
        .iter()
        .filter(|entry| entry.status == "merged")
        .count() as u32;
    let pending_approvals = approvals
        .iter()
        .filter(|approval| approval.status == "pending")
        .count();
    let mut reasons = Vec::new();
    let mut reviewability = "ok";
    if readiness.level == "blocked" {
        reviewability = "risk";
        reasons.push("blocked".to_string());
    }
    if effects.blocked {
        reviewability = "risk";
        reasons.push("effect_unknown".to_string());
    }
    if pending_approvals > 0 {
        reviewability = "risk";
        reasons.push("unresolved_approvals".to_string());
    }
    if readiness.level == "dirty" {
        if reviewability == "ok" {
            reviewability = "watch";
        }
        reasons.push("dirty".to_string());
    }
    if readiness.latest_commit.is_none() {
        if reviewability == "ok" {
            reviewability = "watch";
        }
        reasons.push("no_commit".to_string());
    }
    match (readiness.latest_test.as_ref(), readiness.latest_test_passed) {
        (Some(_), Some(true)) => reasons.push("tests_passed".to_string()),
        (Some(_), Some(false)) => {
            reviewability = "risk";
            reasons.push("tests_failed".to_string());
        }
        (Some(_), None) => {
            if reviewability == "ok" {
                reviewability = "watch";
            }
            reasons.push("test_evidence_unverified".to_string());
        }
        (None, _) => {
            if reviewability == "ok" {
                reviewability = "watch";
            }
            reasons.push("no_tests".to_string());
        }
    }
    if readiness.verifier.is_none() {
        if reviewability == "ok" {
            reviewability = "watch";
        }
        reasons.push("no_verifier".to_string());
    }
    if readiness.merge.as_deref() == Some("merged") {
        reasons.push("merged".to_string());
    } else if let Some(status) = readiness.merge.as_deref() {
        if matches!(status, "conflict" | "failed" | "interrupted") {
            reviewability = "risk";
        } else if reviewability == "ok" {
            reviewability = "watch";
        }
        reasons.push(format!(
            "merge_{}",
            status
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                .take(32)
                .collect::<String>()
        ));
    } else if readiness.verifier.is_some() {
        reasons.push("verified".to_string());
    }
    if total_tasks > 0 && completed_tasks == total_tasks {
        reasons.push("all_tasks_completed".to_string());
    }
    let mut changed_paths = readiness.changed_paths.clone();
    changed_paths.truncate(MAX_TRADEOFF_PATHS);
    for path in &mut changed_paths {
        *path = bounded_tradeoff_text(path, 200);
    }
    let mut blockers = readiness.blockers.clone();
    blockers.truncate(MAX_TRADEOFF_BLOCKERS);
    for blocker in &mut blockers {
        *blocker = bounded_tradeoff_text(blocker, MAX_TRADEOFF_TEXT);
    }
    reasons.truncate(MAX_TRADEOFF_BLOCKERS);

    QualityReview {
        readiness_level: readiness.level.clone(),
        latest_commit: readiness
            .latest_commit
            .as_deref()
            .map(|commit| bounded_tradeoff_text(commit, 120)),
        latest_test: readiness
            .latest_test
            .as_deref()
            .map(|test| bounded_tradeoff_text(test, MAX_TRADEOFF_TEXT)),
        verifier: readiness
            .verifier
            .as_deref()
            .map(|verifier| bounded_tradeoff_text(verifier, MAX_TRADEOFF_TEXT)),
        merge: readiness
            .merge
            .as_deref()
            .map(|merge| bounded_tradeoff_text(merge, MAX_TRADEOFF_TEXT)),
        completed_tasks,
        total_tasks,
        merged_entries,
        changed_paths,
        blockers,
        reviewability: reviewability.to_string(),
        reasons,
    }
}

fn calibrated_confidence(
    readiness: &vp_protocol::types::ReadinessSnapshot,
    quality: &QualityReview,
    sample_calls: u64,
) -> CalibratedConfidence {
    let mut score = 0.0f64;
    let mut reasons = Vec::new();
    if readiness.latest_commit.is_some() {
        score += 0.20;
        reasons.push("commit evidence".to_string());
    } else {
        reasons.push("no commit evidence".to_string());
    }
    match (readiness.latest_test.as_ref(), readiness.latest_test_passed) {
        (Some(_), Some(true)) => {
            score += 0.15;
            reasons.push("passing test evidence".to_string());
        }
        (Some(_), Some(false)) => {
            score -= 0.10;
            reasons.push("failing test evidence".to_string());
        }
        (Some(_), None) => {
            reasons.push("unverified test evidence".to_string());
        }
        (None, _) => {
            reasons.push("no test evidence".to_string());
        }
    }
    if readiness.verifier.is_some() {
        score += 0.20;
        reasons.push("Verifier evidence".to_string());
    } else {
        reasons.push("no Verifier evidence".to_string());
    }
    if readiness.merge.as_deref() == Some("merged") {
        score += 0.15;
        reasons.push("merge evidence".to_string());
    } else if let Some(status) = readiness.merge.as_deref() {
        score -= 0.05;
        reasons.push(format!("merge pending: {status}"));
    }
    score += (sample_calls.min(10) as f64 / 10.0) * 0.10;
    if sample_calls == 0 {
        reasons.push("no usage samples".to_string());
    } else {
        reasons.push(format!("{sample_calls} usage sample(s)"));
    }
    if quality.blockers.is_empty() {
        score += 0.10;
    } else {
        score -= (quality.blockers.len().min(5) as f64) * 0.04;
        reasons.push(format!("{} blocker(s)", quality.blockers.len()));
    }
    if quality.reviewability == "risk" {
        score -= 0.10;
        reasons.push("reviewability risk".to_string());
    } else if quality.reviewability == "watch" {
        score -= 0.05;
        reasons.push("reviewability watch".to_string());
    }
    let score = score.clamp(0.0, 1.0);
    let level = if score >= 0.75 {
        "high"
    } else if score >= 0.45 {
        "medium"
    } else {
        "low"
    };
    reasons.truncate(MAX_TRADEOFF_BLOCKERS);
    for reason in &mut reasons {
        *reason = bounded_tradeoff_text(reason, MAX_TRADEOFF_TEXT);
    }
    CalibratedConfidence {
        level: level.to_string(),
        score: (score * 100.0).round() / 100.0,
        sample_calls: sample_calls.min(u32::MAX as u64) as u32,
        reasons,
    }
}

fn has_reserve_pressure(
    budget: &vp_protocol::types::SessionBudgetStatus,
    contract: &vp_protocol::types::RunContractView,
) -> bool {
    let token_pressure = budget.remaining_tokens.is_some_and(|remaining| {
        budget
            .hard_token_cap
            .or(budget.effective_token_budget)
            .is_some_and(|hard| {
                remaining < ((hard as f64) * contract.completion_reserve_ratio) as u64
            })
    });
    let cost_pressure = budget.remaining_cost_usd.is_some_and(|remaining| {
        budget
            .hard_cost_cap_usd
            .or(budget.effective_cost_budget_usd)
            .is_some_and(|hard| hard > 0.0 && remaining < hard * contract.completion_reserve_ratio)
    });
    let time_pressure = budget.remaining_secs.is_some_and(|remaining| {
        budget
            .hard_wall_clock_secs
            .or(budget.effective_time_budget_secs)
            .is_some_and(|hard| {
                hard > 0 && (remaining as f64) < hard as f64 * contract.completion_reserve_ratio
            })
    });
    token_pressure || cost_pressure || time_pressure
}

/// Advisory only: the runtime never switches the session profile. Quality is
/// recommended for risk/blocker evidence; cost is recommended only under
/// completion-reserve pressure. A profile equal to the current one is never
/// badged as a change recommendation.
fn recommended_profile(
    quality: &QualityReview,
    effects: &vp_protocol::types::SessionEffectsResult,
    reserve_pressure: bool,
) -> Option<&'static str> {
    if quality.reviewability == "risk" || !quality.blockers.is_empty() || effects.blocked {
        Some("quality")
    } else if reserve_pressure {
        Some("cost")
    } else {
        None
    }
}

fn profile_badge(profile: &str, recommended: Option<&str>, current: &str) -> bool {
    recommended.is_some_and(|recommended| recommended == profile && recommended != current)
}

#[allow(clippy::too_many_arguments)]
fn tradeoff_recommendations(
    contract: &vp_protocol::types::RunContractView,
    readiness: &vp_protocol::types::ReadinessSnapshot,
    approvals: &[vp_protocol::types::Approval],
    effects: &vp_protocol::types::SessionEffectsResult,
    reserve_pressure: bool,
) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut recommendations: Vec<String> = Vec::new();
    let mut push = |action: &str| {
        if recommendations.len() < MAX_TRADEOFF_RECOMMENDATIONS && seen.insert(action.to_string()) {
            recommendations.push(action.to_string());
        }
    };
    let pending_approvals = approvals
        .iter()
        .filter(|approval| approval.status == "pending")
        .count() as u32;
    if effects.blocked || readiness.level == "blocked" {
        push("commit_or_reconcile");
    }
    if readiness.level == "dirty" {
        push("commit_or_reconcile");
    }
    if readiness.latest_commit.is_some() && readiness.latest_test.is_none() {
        push("add_tests");
    }
    if readiness.verifier.is_none() && readiness.latest_commit.is_some() {
        push("wait_for_verifier");
    }
    if matches!(
        readiness.level.as_str(),
        "tests_passed" | "verified" | "merged"
    ) {
        push("verify_or_merge");
    }
    if reserve_pressure {
        push("rescue_reserve");
    }
    if pending_approvals > contract.attention_cap {
        push("partial_delivery");
    }
    drop(push);
    if recommendations.is_empty() {
        recommendations.push("verify_or_merge".to_string());
    }
    recommendations
}

fn bounded_tradeoff_text(value: &str, max_chars: usize) -> String {
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
    use vp_protocol::types::{
        AutoBudgetCaps, BudgetForecast, BudgetMode, ReadinessSnapshot, ResourceSoftTargets,
        RunContractView, RunProfile, RunStopPolicy, SessionBudgetStatus,
    };

    fn budget(
        token_cap: Option<u64>,
        remaining_tokens: Option<u64>,
        cost_cap: Option<f64>,
        remaining_cost: Option<f64>,
    ) -> SessionBudgetStatus {
        SessionBudgetStatus {
            session_id: "ses-1".into(),
            mode: BudgetMode::Fixed,
            unlimited: false,
            effective_token_budget: token_cap,
            effective_cost_budget_usd: cost_cap,
            effective_time_budget_secs: None,
            hard_token_cap: token_cap,
            hard_cost_cap_usd: cost_cap,
            hard_wall_clock_secs: None,
            spent_tokens: 0,
            spent_cost_usd: 0.0,
            elapsed_secs: 0,
            remaining_tokens,
            remaining_cost_usd: remaining_cost,
            remaining_secs: None,
            policy: None,
            state: None,
            forecast: BudgetForecast::default(),
            updated_at: None,
        }
    }

    #[test]
    fn reserve_pressure_checks_token_and_cost_caps() {
        let contract = contract();
        assert!(!has_reserve_pressure(
            &budget(Some(10_000), Some(9_000), Some(10.0), Some(9.0)),
            &contract
        ));
        assert!(has_reserve_pressure(
            &budget(Some(10_000), Some(1_000), Some(10.0), Some(9.0)),
            &contract
        ));
        assert!(has_reserve_pressure(
            &budget(Some(10_000), Some(9_000), Some(10.0), Some(1.0)),
            &contract
        ));
        assert!(!has_reserve_pressure(
            &budget(None, None, None, None),
            &contract
        ));

        let mut time_budget = budget(Some(10_000), Some(9_000), None, None);
        time_budget.hard_wall_clock_secs = Some(3_600);
        time_budget.remaining_secs = Some(100);
        assert!(has_reserve_pressure(&time_budget, &contract));
    }

    fn readiness(level: &str) -> ReadinessSnapshot {
        ReadinessSnapshot {
            session_id: "ses-1".into(),
            level: level.into(),
            shippable: false,
            mergeable: false,
            latest_commit: Some("abc123".into()),
            latest_test: None,
            latest_test_passed: None,
            verifier: None,
            merge: None,
            changed_paths: vec!["src/lib.rs".into()],
            remaining_work: Vec::new(),
            blockers: Vec::new(),
            partial_delivery: None,
            generated_at: "2026-11-24T00:00:00Z".into(),
        }
    }

    fn quality() -> QualityReview {
        QualityReview {
            readiness_level: "committed".into(),
            latest_commit: Some("abc123".into()),
            latest_test: None,
            verifier: None,
            merge: None,
            completed_tasks: 1,
            total_tasks: 2,
            merged_entries: 0,
            changed_paths: vec!["src/lib.rs".into()],
            blockers: Vec::new(),
            reviewability: "watch".into(),
            reasons: vec!["no_tests".into()],
        }
    }

    fn effects() -> vp_protocol::types::SessionEffectsResult {
        vp_protocol::types::SessionEffectsResult {
            session_id: "ses-1".into(),
            policy: vp_protocol::types::EffectPolicy::default(),
            receipts: Vec::new(),
            counts: vp_protocol::types::EffectCounts::default(),
            blocked: false,
            block_reason: None,
            truncated: false,
        }
    }

    fn contract() -> RunContractView {
        RunContractView {
            session_id: "ses-1".into(),
            project_id: "prj-1".into(),
            profile: "balanced".into(),
            source: "derived".into(),
            goal: "goal".into(),
            acceptance: Vec::new(),
            risk_ceiling: "approval-managed".into(),
            autonomy: "operator-configured".into(),
            hard_caps: AutoBudgetCaps::default(),
            soft_targets: ResourceSoftTargets {
                cost_usd: None,
                time_secs: None,
                attention: None,
            },
            completion_reserve_ratio: 0.20,
            attention_cap: 20,
            stop_policy: RunStopPolicy::default(),
            warnings: Vec::new(),
            generated_at: "2026-11-24T00:00:00Z".into(),
        }
    }

    #[test]
    fn quality_review_is_conservative_and_marks_dirty_missing_evidence() {
        let review = quality_review(&readiness("committed"), &[], &[], &[], &effects());
        assert_eq!(review.reviewability, "watch");
        assert!(review.reasons.contains(&"no_tests".to_string()));
        assert!(review.reasons.contains(&"no_verifier".to_string()));

        let dirty = quality_review(&readiness("dirty"), &[], &[], &[], &effects());
        assert_eq!(dirty.reviewability, "watch");
        assert!(dirty.reasons.contains(&"dirty".to_string()));

        let mut queued = readiness("verified");
        queued.latest_test = Some("test passed".into());
        queued.latest_test_passed = Some(true);
        queued.verifier = Some("approved · queue entry".into());
        queued.merge = Some("queued".into());
        let review = quality_review(&queued, &[], &[], &[], &effects());
        assert!(review.reasons.contains(&"merge_queued".to_string()));
        assert!(!review.reasons.contains(&"merged".to_string()));
        assert_eq!(review.reviewability, "watch");

        let mut merged = queued.clone();
        merged.merge = Some("merged".into());
        let review = quality_review(&merged, &[], &[], &[], &effects());
        assert!(review.reasons.contains(&"merged".to_string()));
        assert!(!review.reasons.contains(&"merge_merged".to_string()));

        let mut failing = merged.clone();
        failing.latest_test_passed = Some(false);
        failing.merge = Some("conflict".into());
        let review = quality_review(&failing, &[], &[], &[], &effects());
        assert_eq!(review.reviewability, "risk");
        assert!(review.reasons.contains(&"tests_failed".to_string()));
        assert!(review.reasons.contains(&"merge_conflict".to_string()));
    }

    #[test]
    fn confidence_is_an_explicit_heuristic() {
        let low = calibrated_confidence(&readiness("none"), &quality(), 0);
        assert_eq!(low.level, "low");
        assert!(low.score < 0.45);
        let mut strong_readiness = readiness("merged");
        strong_readiness.latest_test = Some("test passed".into());
        strong_readiness.latest_test_passed = Some(true);
        strong_readiness.verifier = Some("approved".into());
        strong_readiness.merge = Some("merged".into());
        let strong = calibrated_confidence(&strong_readiness, &quality(), 12);
        assert_eq!(strong.level, "high");
        assert!(strong.score >= 0.75);

        let mut failing_readiness = strong_readiness.clone();
        failing_readiness.latest_test_passed = Some(false);
        failing_readiness.merge = Some("conflict".into());
        let failing = calibrated_confidence(&failing_readiness, &quality(), 12);
        assert!(failing.score < strong.score);
        assert!(
            failing
                .reasons
                .iter()
                .any(|reason| reason == "failing test evidence")
        );
    }

    #[test]
    fn recommendations_include_verification_and_reserve_pressure() {
        let recommendations =
            tradeoff_recommendations(&contract(), &readiness("committed"), &[], &effects(), true);
        assert!(recommendations.contains(&"add_tests".to_string()));
        assert!(recommendations.contains(&"wait_for_verifier".to_string()));
        assert!(recommendations.contains(&"rescue_reserve".to_string()));
        assert!(recommendations.len() <= MAX_TRADEOFF_RECOMMENDATIONS);
    }

    #[test]
    fn profile_table_covers_every_run_profile_with_documented_posture() {
        let seen = PROFILE_TEXT
            .iter()
            .map(|(profile, _)| *profile)
            .collect::<HashSet<_>>();
        for profile in [
            RunProfile::Quality,
            RunProfile::Balanced,
            RunProfile::Fast,
            RunProfile::Cheap,
            RunProfile::Unattended,
        ] {
            assert!(seen.contains(profile.as_str()), "{profile:?}");
        }
        assert_eq!(PROFILE_TEXT.len(), 5);
        assert!(PROFILE_TEXT.iter().all(|(_, text)| text.reserve <= 0.25));
        let long = "x".repeat(MAX_TRADEOFF_TEXT + 10);
        assert_eq!(
            bounded_tradeoff_text(&long, MAX_TRADEOFF_TEXT)
                .chars()
                .count(),
            MAX_TRADEOFF_TEXT
        );
    }

    #[test]
    fn recommended_profile_is_advisory_and_evidence_bounded() {
        let no_effects = effects();
        let clean = quality();
        assert_eq!(
            recommended_profile(&clean, &no_effects, false),
            None,
            "no risk and no reserve pressure recommends no profile change"
        );

        let mut risk = quality();
        risk.reviewability = "risk".into();
        assert_eq!(
            recommended_profile(&risk, &no_effects, false),
            Some("quality")
        );

        let mut blocked = quality();
        blocked.blockers = vec!["merge conflict".into()];
        assert_eq!(
            recommended_profile(&blocked, &no_effects, false),
            Some("quality")
        );

        let mut effect_blocked = effects();
        effect_blocked.blocked = true;
        assert_eq!(
            recommended_profile(&clean, &effect_blocked, false),
            Some("quality")
        );

        assert_eq!(
            recommended_profile(&clean, &no_effects, true),
            Some("cost"),
            "reserve pressure recommends the cost profile only after quality clears"
        );
    }

    #[test]
    fn profile_badges_only_mark_a_different_recommended_profile() {
        assert!(!profile_badge("quality", None, "balanced"));
        assert!(!profile_badge("balanced", Some("quality"), "balanced"));
        assert!(profile_badge("quality", Some("quality"), "balanced"));
        assert!(profile_badge("cost", Some("cost"), "balanced"));
        assert!(
            !profile_badge("quality", Some("quality"), "quality"),
            "the current profile is not badged as a change recommendation"
        );
        assert!(!profile_badge("cost", Some("cost"), "cost"));
    }

    #[test]
    fn consider_profile_recommendation_is_specific_and_bounded() {
        let recommended = recommended_profile(&quality(), &effects(), true);
        assert_eq!(recommended, Some("cost"));
        assert_eq!(
            recommended.map(|profile| format!("consider_profile:{profile}")),
            Some("consider_profile:cost".to_string())
        );
        const { assert!(MAX_TRADEOFF_RECOMMENDATIONS <= 12) };
    }
}
