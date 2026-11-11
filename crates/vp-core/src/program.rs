//! Project programs and milestones (ADR-0157).
//!
//! A program is a project-scoped ledger above sessions and TeamPlans. It does
//! not execute work itself: every milestone start creates an ordinary session
//! with a deterministic one-task TeamPlan, so admission, budgets, capacity,
//! approvals, sandboxing, write scopes, the Verifier, the Integrator, and the
//! merge queue are unchanged.
//!
//! The controller in this module is deterministic Rust. It applies readiness,
//! dependency, retry, pause, and budget rules; it never asks a model which
//! milestone to run next.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_protocol::types::{
    DependencyFailurePolicy, MergeQueueEntry, PlanDepartment, PlanTask, Program, ProgramListEntry,
    ProgramMilestone, ProgramMilestoneCounts, ProgramMilestoneStatus, ProgramPolicy,
    ProgramReplayReport, ProgramStatus, ProgramStatusResult, ProgramUsage, ScheduledTask, Session,
    TeamPlan, rebuild_program_from_events,
};
use vp_store::Store;
use vp_store::store::{NewProgram, NewProgramMilestone, ProgramMilestoneUpdate, ProgramUpdate};

/// Maximum milestone priority boost from waiting. Mirrors the task scheduler's
/// aging cap so a low-priority milestone cannot starve forever.
const MAX_AGING_BOOST: i64 = 100;
/// Bounded retry delay cap. Program backoff is exponential but never longer
/// than one day, even if a caller configures a large base.
const MAX_RETRY_BACKOFF_SECS: u64 = 86_400;
/// A dispatch row with no session id is a crash window. Give the launcher this
/// much time before treating it as an abandoned start, so a tick that observes
/// a concurrently running RPC does not fail a healthy dispatch.
const DEFAULT_DISPATCH_GRACE_SECS: u64 = 300;

/// Deterministic program controller. It owns no task state beyond the store;
/// the session launcher is the agent service's normal session path.
#[derive(Clone)]
pub struct ProgramController {
    store: Store,
    aging: Duration,
    dispatch_grace: Duration,
}

#[derive(Debug, Clone, Default)]
pub struct ProgramReconcileResult {
    pub programs: usize,
    pub milestones_advanced: usize,
    pub completed: usize,
    pub failed: usize,
}

#[derive(Debug, Clone)]
pub struct ProgramCandidate {
    pub program: Program,
    pub milestone: ProgramMilestone,
    pub effective_priority: i64,
}

#[derive(Debug, Clone)]
pub struct ProgramLaunchBudget {
    pub token_budget: Option<u64>,
    pub cost_budget_usd: Option<f64>,
}

impl ProgramController {
    pub fn new(store: Store) -> Self {
        Self::with_aging(store, program_aging())
    }

    pub fn with_aging(store: Store, aging: Duration) -> Self {
        Self {
            store,
            aging,
            dispatch_grace: Duration::from_secs(program_dispatch_grace_secs()),
        }
    }

    pub fn with_dispatch_grace(mut self, grace: Duration) -> Self {
        self.dispatch_grace = grace;
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn list_programs(
        &self,
        project_id: Option<&str>,
        status: Option<ProgramStatus>,
        limit: Option<u32>,
    ) -> Result<Vec<ProgramListEntry>> {
        let programs = self.store.list_programs(project_id, status, limit)?;
        let mut entries = Vec::with_capacity(programs.len());
        for program in programs {
            let counts = milestone_counts(&self.store.list_program_milestones(&program.id)?);
            entries.push(ProgramListEntry { program, counts });
        }
        Ok(entries)
    }

    pub fn status(
        &self,
        program_id: &str,
        event_limit: Option<u32>,
    ) -> Result<Option<ProgramStatusResult>> {
        let Some(program) = self.store.get_program(program_id)? else {
            return Ok(None);
        };
        let milestones = self.store.list_program_milestones(program_id)?;
        let counts = milestone_counts(&milestones);
        let usage = self.store.program_usage(program_id)?;
        let events = self
            .store
            .list_program_events(program_id, Some(event_limit.unwrap_or(50)))?;
        Ok(Some(ProgramStatusResult {
            program,
            milestones,
            counts,
            usage,
            events,
        }))
    }

    /// Run the deterministic controller over every project (or one project).
    /// This function only advances persisted ledger rows; it never starts a
    /// session. The agent service calls `ready_candidates` after it returns.
    pub async fn reconcile(&self, project_id: Option<&str>) -> Result<ProgramReconcileResult> {
        let controller = self.clone();
        let project_id = project_id.map(ToOwned::to_owned);
        tokio::task::spawn_blocking(move || controller.reconcile_blocking(project_id.as_deref()))
            .await?
    }

    fn reconcile_blocking(&self, project_id: Option<&str>) -> Result<ProgramReconcileResult> {
        let programs = self.store.list_programs(project_id, None, Some(10_000))?;
        let mut result = ProgramReconcileResult {
            programs: programs.len(),
            ..Default::default()
        };
        for program in programs {
            if program.archived {
                continue;
            }
            if matches!(
                program.status,
                ProgramStatus::Draft | ProgramStatus::Completed | ProgramStatus::Failed
            ) {
                continue;
            }
            let usage = self.store.program_usage(&program.id)?;
            let budget_exhausted = program_budget_exhausted(&program, &usage);
            let mut milestones = self.store.list_program_milestones(&program.id)?;
            let statuses = milestones
                .iter()
                .map(|milestone| (milestone.id.clone(), milestone.status))
                .collect::<HashMap<_, _>>();
            let mut advanced_ids = HashSet::new();
            for milestone in milestones.clone() {
                let changed = match milestone.status {
                    ProgramMilestoneStatus::Running | ProgramMilestoneStatus::Verifying => {
                        self.reconcile_active_milestone(&program, &milestone, &usage)?
                    }
                    ProgramMilestoneStatus::Pending
                    | ProgramMilestoneStatus::Ready
                    | ProgramMilestoneStatus::Failed => {
                        self.reconcile_waiting_milestone(&program, &milestone, &statuses)?
                    }
                    ProgramMilestoneStatus::Completed
                    | ProgramMilestoneStatus::Blocked
                    | ProgramMilestoneStatus::Cancelled => false,
                };
                if changed {
                    advanced_ids.insert(milestone.id.clone());
                }
            }

            milestones = self.store.list_program_milestones(&program.id)?;
            // Apply dependency policy for newly terminal failures. This is
            // idempotent: a dependent already blocked/cancelled is untouched.
            for milestone in &milestones {
                if matches!(
                    milestone.status,
                    ProgramMilestoneStatus::Blocked | ProgramMilestoneStatus::Cancelled
                ) && self.apply_dependency_failure_policy(&program, milestone, &milestones)?
                {
                    advanced_ids.insert(milestone.id.clone());
                }
            }

            milestones = self.store.list_program_milestones(&program.id)?;
            let all_terminal = milestones
                .iter()
                .all(|milestone| milestone.status.is_terminal());
            if all_terminal {
                let completed = milestones
                    .iter()
                    .filter(|milestone| milestone.status == ProgramMilestoneStatus::Completed)
                    .count();
                let blocked = milestones
                    .iter()
                    .filter(|milestone| milestone.status == ProgramMilestoneStatus::Blocked)
                    .count();
                let cancelled = milestones
                    .iter()
                    .filter(|milestone| milestone.status == ProgramMilestoneStatus::Cancelled)
                    .count();
                let outcome = if blocked > 0 {
                    Some((
                        false,
                        format!("{blocked} milestone(s) are blocked and need operator action"),
                    ))
                } else if completed == 0 {
                    Some((
                        false,
                        "program has no completed milestones; refusing to report success"
                            .to_string(),
                    ))
                } else if milestones.is_empty() {
                    None
                } else {
                    Some((
                        true,
                        format!(
                            "all milestones are terminal: {completed} completed, {cancelled} cancelled"
                        ),
                    ))
                };
                if let Some((success, reason)) = outcome {
                    if success {
                        if self.store.complete_program(&program.id, &reason)?.is_some() {
                            result.completed += 1;
                        }
                    } else if self.store.fail_program(&program.id, &reason)?.is_some() {
                        result.failed += 1;
                    }
                }
                result.milestones_advanced += advanced_ids.len();
                continue;
            }
            if budget_exhausted {
                if self.exhaust_program_budget(&program, &milestones)? {
                    result.failed += 1;
                    advanced_ids.extend(milestones.iter().map(|milestone| milestone.id.clone()));
                }
                result.milestones_advanced += advanced_ids.len();
                continue;
            }
            result.milestones_advanced += advanced_ids.len();
        }
        Ok(result)
    }

    fn reconcile_active_milestone(
        &self,
        program: &Program,
        milestone: &ProgramMilestone,
        usage: &ProgramUsage,
    ) -> Result<bool> {
        let Some(session_id) = milestone.session_id.as_deref() else {
            let age = OffsetDateTime::parse(&milestone.updated_at, &Rfc3339)
                .map(|updated| (OffsetDateTime::now_utc() - updated).whole_seconds().max(0))
                .unwrap_or(0) as u64;
            if age < self.dispatch_grace.as_secs() {
                return Ok(false);
            }
            self.handle_milestone_failure(
                program,
                milestone,
                usage,
                "the milestone was claimed for dispatch but no session was attached",
            )?;
            return Ok(true);
        };
        let Some(session) = self.store.get_session(session_id)? else {
            self.handle_milestone_failure(
                program,
                milestone,
                usage,
                &format!("linked session {session_id} no longer exists"),
            )?;
            return Ok(true);
        };
        if matches!(
            session.status.as_str(),
            "queued" | "running" | "paused" | "waiting_input"
        ) {
            return Ok(false);
        }
        if session.status == "imported" {
            self.handle_milestone_failure(
                program,
                milestone,
                usage,
                "the linked session is a read-only imported archive",
            )?;
            return Ok(true);
        }

        let tasks = self.store.list_plan_tasks(None, Some(session_id))?;
        let task = tasks
            .iter()
            .filter(|task| task.task_id == "T1")
            .max_by_key(|task| task.plan_seq);
        let Some(task) = task else {
            self.handle_milestone_failure(
                program,
                milestone,
                usage,
                "the linked session has no task T1",
            )?;
            return Ok(true);
        };

        match task.status.as_str() {
            "completed" | "skipped" => {
                if session.status != "idle" {
                    if matches!(session.status.as_str(), "failed" | "cancelled") {
                        self.handle_milestone_failure(
                            program,
                            milestone,
                            usage,
                            &format!(
                                "task {} completed but session ended {}",
                                task.task_id, session.status
                            ),
                        )?;
                        return Ok(true);
                    }
                    return Ok(false);
                }
                if !self.integration_evidence_is_clean(session_id, &task.task_id)? {
                    self.handle_milestone_failure(
                        program,
                        milestone,
                        usage,
                        "the task completed without an integrated Verifier-approved merge",
                    )?;
                    return Ok(true);
                }
                if milestone.status == ProgramMilestoneStatus::Running {
                    if self
                        .store
                        .mark_program_milestone_verifying(&milestone.id)?
                        .is_some()
                    {
                        return Ok(true);
                    }
                    return Ok(false);
                }
                if milestone.acceptance.is_empty() {
                    self.handle_milestone_failure(
                        program,
                        milestone,
                        usage,
                        "milestone acceptance criteria are empty",
                    )?;
                    return Ok(true);
                }
                let result = self
                    .milestone_result(&session, task)
                    .unwrap_or_else(|| format!("task {} completed", task.task_id));
                if self
                    .store
                    .complete_program_milestone(&milestone.id, &result)?
                    .is_some()
                {
                    return Ok(true);
                }
                Ok(false)
            }
            "failed" | "blocked" | "cancelled" | "interrupted" => {
                self.handle_milestone_failure(
                    program,
                    milestone,
                    usage,
                    task.last_error
                        .as_deref()
                        .unwrap_or("the plan task did not complete"),
                )?;
                Ok(true)
            }
            "pending" | "running" | "claimed" => {
                if matches!(session.status.as_str(), "failed" | "cancelled") {
                    self.handle_milestone_failure(
                        program,
                        milestone,
                        usage,
                        &format!(
                            "session ended {} while task {} was {}",
                            session.status, task.task_id, task.status
                        ),
                    )?;
                    return Ok(true);
                }
                Ok(false)
            }
            _ => {
                if session.status == "idle" {
                    self.handle_milestone_failure(
                        program,
                        milestone,
                        usage,
                        &format!(
                            "plan task {} has unknown status {}",
                            task.task_id, task.status
                        ),
                    )?;
                    return Ok(true);
                }
                Ok(false)
            }
        }
    }

    fn reconcile_waiting_milestone(
        &self,
        program: &Program,
        milestone: &ProgramMilestone,
        statuses: &HashMap<String, ProgramMilestoneStatus>,
    ) -> Result<bool> {
        if program.status != ProgramStatus::Active {
            return Ok(false);
        }
        let dependencies_completed = milestone.deps.iter().all(|dependency| {
            statuses.get(dependency).copied() == Some(ProgramMilestoneStatus::Completed)
        });
        if dependencies_completed {
            if milestone.status == ProgramMilestoneStatus::Ready {
                return Ok(false);
            }
            if milestone.status == ProgramMilestoneStatus::Failed {
                if milestone.attempts >= milestone.max_attempts {
                    self.store.block_program_milestone(
                        &milestone.id,
                        "automatic attempt budget is exhausted",
                    )?;
                    return Ok(true);
                }
                if let Some(next) = milestone.next_attempt_at.as_deref()
                    && OffsetDateTime::parse(next, &Rfc3339)
                        .map(|next| next > OffsetDateTime::now_utc())
                        .unwrap_or(false)
                {
                    return Ok(false);
                }
            }
            return Ok(self
                .store
                .mark_program_milestone_ready(&milestone.id)?
                .is_some());
        }

        // A blocked/cancelled dependency is permanent for the default block
        // and cancel policies. `retry` intentionally leaves dependents pending
        // so a manual retry of the dependency can unblock them without spinning.
        if milestone.status == ProgramMilestoneStatus::Failed
            && milestone.attempts >= milestone.max_attempts
        {
            self.store
                .block_program_milestone(&milestone.id, "automatic attempt budget is exhausted")?;
            return Ok(true);
        }
        Ok(false)
    }

    fn apply_dependency_failure_policy(
        &self,
        program: &Program,
        dependency: &ProgramMilestone,
        milestones: &[ProgramMilestone],
    ) -> Result<bool> {
        let mut changed = false;
        for dependent in milestones {
            if dependent.id == dependency.id
                || !dependent.deps.iter().any(|dep| dep == &dependency.id)
                || dependent.status.is_terminal()
                || dependent.status == ProgramMilestoneStatus::Running
                || dependent.status == ProgramMilestoneStatus::Verifying
            {
                continue;
            }
            let reason = format!(
                "dependency \"{}\" ({}) is {}",
                dependency.title, dependency.key, dependency.status
            );
            match program.policy.dependency_failure {
                DependencyFailurePolicy::Block => {
                    if self
                        .store
                        .block_program_milestone(&dependent.id, &reason)?
                        .is_some()
                    {
                        changed = true;
                    }
                }
                DependencyFailurePolicy::Cancel => {
                    if self
                        .store
                        .cancel_program_milestone(&dependent.id, &reason, false)?
                        .is_some()
                    {
                        changed = true;
                    }
                }
                DependencyFailurePolicy::Retry => {}
            }
        }
        Ok(changed)
    }

    fn handle_milestone_failure(
        &self,
        program: &Program,
        milestone: &ProgramMilestone,
        usage: &ProgramUsage,
        reason: &str,
    ) -> Result<()> {
        let reason = bounded_reason(reason);
        let session_budget_exhausted = milestone
            .session_id
            .as_deref()
            .and_then(|session_id| self.store.get_session(session_id).ok().flatten())
            .is_some_and(|session| {
                crate::session_budget_exhausted(&session)
                    || milestone_budget_exhausted_for_program(program, milestone, &session)
            });
        let retryable = !program_budget_exhausted(program, usage)
            && !session_budget_exhausted
            && milestone.attempts < milestone.max_attempts;
        if retryable {
            let next = retry_deadline(program, milestone.attempts.max(1))?;
            if self
                .store
                .fail_program_milestone(&milestone.id, &reason, Some(&next))?
                .is_some()
            {
                return Ok(());
            }
        } else if session_budget_exhausted {
            self.store
                .fail_program_milestone(&milestone.id, &reason, None)?;
            self.store.block_program_milestone(
                &milestone.id,
                &format!("{reason}; milestone budget is exhausted and will not be retried"),
            )?;
            return Ok(());
        } else if milestone.attempts >= milestone.max_attempts {
            self.store
                .fail_program_milestone(&milestone.id, &reason, None)?;
            self.store.block_program_milestone(
                &milestone.id,
                &format!("{reason}; attempt budget is exhausted"),
            )?;
            return Ok(());
        } else {
            self.store
                .fail_program_milestone(&milestone.id, &reason, None)?;
            self.store.block_program_milestone(&milestone.id, &reason)?;
            return Ok(());
        }
        Ok(())
    }

    fn exhaust_program_budget(
        &self,
        program: &Program,
        milestones: &[ProgramMilestone],
    ) -> Result<bool> {
        let reason = "program budget is exhausted; no further milestone will be dispatched";
        let mut any_active = false;
        for milestone in milestones {
            if milestone.status.is_terminal() {
                continue;
            }
            if milestone.status.is_active() {
                any_active = true;
                continue;
            }
            if matches!(
                milestone.status,
                ProgramMilestoneStatus::Pending
                    | ProgramMilestoneStatus::Ready
                    | ProgramMilestoneStatus::Failed
            ) {
                self.store.block_program_milestone(&milestone.id, reason)?;
            }
        }
        if any_active {
            return Ok(false);
        }
        Ok(self.store.fail_program(&program.id, reason)?.is_some())
    }

    fn integration_evidence_is_clean(&self, session_id: &str, task_id: &str) -> Result<bool> {
        let entries = self.store.list_merge_queue(session_id)?;
        let task_entries = entries
            .iter()
            .filter(|entry| entry.task_id.as_deref() == Some(task_id))
            .collect::<Vec<&MergeQueueEntry>>();
        if task_entries.is_empty() {
            // A no-change Worker run has nothing to integrate. The plan task is
            // only marked completed after finish_worker accepted that outcome.
            return Ok(true);
        }
        Ok(task_entries
            .iter()
            .all(|entry| entry.status == "integrated"))
    }

    fn milestone_result(&self, session: &Session, task: &ScheduledTask) -> Option<String> {
        let events = self.store.recent_events(&session.id, 2_000).ok()?;
        let summary = events
            .iter()
            .rev()
            .filter(|event| event.kind == "plan_task.finished")
            .find(|event| {
                event
                    .payload
                    .get("task_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(task.task_id.as_str())
            })
            .and_then(|event| {
                event
                    .payload
                    .get("summary")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string)
            });
        Some(summary.unwrap_or_else(|| {
            format!(
                "task {} completed in session {} ({} attempt{})",
                task.task_id,
                session.id,
                task.attempts,
                if task.attempts == 1 { "" } else { "s" }
            )
        }))
    }

    /// Deterministic ready candidates for the launcher. The caller claims each
    /// one atomically before creating a session, so the returned list is only a
    /// scheduling hint and never a reservation.
    pub fn ready_candidates(
        &self,
        last_program: Option<&str>,
        limit: usize,
    ) -> Result<Vec<ProgramCandidate>> {
        let now = OffsetDateTime::now_utc();
        let programs = self
            .store
            .list_programs(None, Some(ProgramStatus::Active), Some(10_000))?;
        let mut candidates = Vec::new();
        for program in programs {
            if program.archived || !program.policy.auto_dispatch {
                continue;
            }
            let usage = self.store.program_usage(&program.id)?;
            if program_budget_exhausted(&program, &usage) {
                continue;
            }
            let milestones = self.store.list_program_milestones(&program.id)?;
            let active = milestones
                .iter()
                .filter(|milestone| milestone.status.is_active())
                .count() as u32;
            let slots = program
                .policy
                .max_parallel_milestones
                .saturating_sub(active)
                .min(10);
            if slots == 0 {
                continue;
            }
            let statuses = milestones
                .iter()
                .map(|milestone| (milestone.id.clone(), milestone.status))
                .collect::<HashMap<_, _>>();
            let mut program_candidates = Vec::new();
            for milestone in &milestones {
                if !matches!(
                    milestone.status,
                    ProgramMilestoneStatus::Pending
                        | ProgramMilestoneStatus::Ready
                        | ProgramMilestoneStatus::Failed
                ) || milestone.attempts >= milestone.max_attempts
                {
                    continue;
                }
                if milestone.status == ProgramMilestoneStatus::Failed {
                    let due = milestone
                        .next_attempt_at
                        .as_deref()
                        .and_then(|value| OffsetDateTime::parse(value, &Rfc3339).ok())
                        .map(|next| next <= now)
                        .unwrap_or(true);
                    if !due {
                        continue;
                    }
                }
                let deps_ready = milestone.deps.iter().all(|dependency| {
                    statuses.get(dependency).copied() == Some(ProgramMilestoneStatus::Completed)
                });
                if !deps_ready {
                    continue;
                }
                let effective_priority = milestone_effective_priority(milestone, now, self.aging);
                program_candidates.push(ProgramCandidate {
                    program: program.clone(),
                    milestone: milestone.clone(),
                    effective_priority,
                });
            }
            program_candidates.sort_by(|left, right| {
                right
                    .effective_priority
                    .cmp(&left.effective_priority)
                    .then_with(|| left.milestone.created_at.cmp(&right.milestone.created_at))
                    .then_with(|| left.milestone.id.cmp(&right.milestone.id))
            });
            program_candidates.truncate(slots as usize);
            candidates.extend(program_candidates);
        }
        candidates.sort_by(|left, right| {
            right
                .effective_priority
                .cmp(&left.effective_priority)
                .then_with(|| left.milestone.created_at.cmp(&right.milestone.created_at))
                .then_with(|| left.milestone.id.cmp(&right.milestone.id))
        });
        if let Some(last_program) = last_program
            && candidates.len() > 1
            && candidates[0].program.id == last_program
            && candidates[0].effective_priority == candidates[1].effective_priority
        {
            let next = candidates[1].clone();
            candidates[1] = candidates[0].clone();
            candidates[0] = next;
        }
        candidates.truncate(limit.max(1));
        Ok(candidates)
    }

    pub fn claim_start(&self, milestone_id: &str) -> Result<Option<ProgramMilestone>> {
        Ok(self
            .store
            .claim_program_milestone_start(milestone_id)?
            .map(|(milestone, _event)| milestone))
    }

    pub fn attach_session(
        &self,
        milestone_id: &str,
        session_id: &str,
        plan_revision: u32,
    ) -> Result<ProgramMilestone> {
        self.store
            .attach_program_milestone_session(milestone_id, session_id, plan_revision)?
            .map(|(milestone, _event)| milestone)
            .ok_or_else(|| anyhow!("milestone {milestone_id} is no longer running"))
    }

    pub fn fail_start(
        &self,
        program: &Program,
        milestone: &ProgramMilestone,
        reason: &str,
    ) -> Result<()> {
        let usage = self.store.program_usage(&program.id)?;
        self.handle_milestone_failure(program, milestone, &usage, reason)
    }

    pub fn budget_for_start(
        &self,
        program: &Program,
        milestone: &ProgramMilestone,
    ) -> Result<ProgramLaunchBudget> {
        let usage = self.store.program_usage(&program.id)?;
        let (token_budget, cost_budget_usd) = effective_budget(program, milestone, &usage)?;
        Ok(ProgramLaunchBudget {
            token_budget,
            cost_budget_usd,
        })
    }

    pub fn replay(&self, program_id: &str, repair: bool) -> Result<ProgramReplayReport> {
        let events = self.store.list_program_events(program_id, Some(10_000))?;
        let (expected_program, expected_milestones) = rebuild_program_from_events(&events);
        let current_program = self.store.get_program(program_id)?;
        let mut current_milestones = self.store.list_program_milestones(program_id)?;
        current_milestones.sort_by(|left, right| left.id.cmp(&right.id));
        let mut expected_sorted = expected_milestones.clone();
        expected_sorted.sort_by(|left, right| left.id.cmp(&right.id));
        let mut mismatches = Vec::new();
        let program_matches = current_program == expected_program;
        if !program_matches {
            mismatches.push("program row differs from the event projection".to_string());
        }
        let milestones_match = current_milestones == expected_sorted;
        if !milestones_match {
            mismatches.push("milestone rows differ from the event projection".to_string());
        }
        if repair && (!program_matches || !milestones_match) {
            self.store.replace_program_projection(
                program_id,
                expected_program.as_ref(),
                &expected_milestones,
            )?;
        }
        Ok(ProgramReplayReport {
            program_id: program_id.to_string(),
            event_count: events.len(),
            program_matches,
            milestones_match,
            consistent: program_matches && milestones_match,
            repaired: repair && (!program_matches || !milestones_match),
            mismatches,
        })
    }
}

/// Build the deterministic one-task plan used to execute a milestone. It is
/// public so tests and future import tooling can inspect exactly what the
/// launcher will submit.
pub fn milestone_plan(program: &Program, milestone: &ProgramMilestone) -> TeamPlan {
    let department = match milestone.profile.as_str() {
        "research" => "research",
        "verification" => "verification",
        _ => "delivery",
    };
    let manager_title = match department {
        "research" => "Research Manager",
        "verification" => "Verification Manager",
        _ => "Delivery Manager",
    };
    let token_budget = milestone.token_budget.or(program.policy.token_budget);
    let cost_budget_usd = milestone.cost_budget_usd.or(program.policy.cost_budget_usd);
    let acceptance = if milestone.acceptance.is_empty() {
        vec!["the milestone objective is demonstrably complete".to_string()]
    } else {
        milestone.acceptance.clone()
    };
    let objective = format!(
        "Program: {}\nMilestone {}: {}\n\n{}\n\nAcceptance:\n{}",
        program.goal,
        milestone.key,
        milestone.title,
        milestone.objective,
        acceptance
            .iter()
            .map(|criterion| format!("- {criterion}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    TeamPlan {
        goal: format!("{} · {}", program.goal, milestone.title),
        departments: vec![PlanDepartment {
            name: department.to_string(),
            manager_title: manager_title.to_string(),
            profile: milestone.profile.clone(),
            objective,
            token_budget,
            cost_budget_usd,
        }],
        tasks: vec![PlanTask {
            id: "T1".to_string(),
            department: department.to_string(),
            title: milestone.title.clone(),
            kind: "coding".to_string(),
            priority: milestone.priority,
            profile: Some(milestone.profile.clone()),
            deps: Vec::new(),
            write_scope: milestone.write_scope.clone(),
            acceptance,
            token_budget,
            cost_budget_usd,
        }],
    }
}

pub fn milestone_session_goal(program: &Program, milestone: &ProgramMilestone) -> String {
    format!(
        "{} · {} {}",
        program.goal.trim(),
        milestone.key.trim(),
        milestone.title.trim()
    )
}

pub fn validate_program_goal(goal: &str) -> Result<()> {
    let trimmed = goal.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 2_000 {
        anyhow::bail!("program goal must be 1-2000 characters");
    }
    Ok(())
}

pub fn validate_program_policy(policy: &ProgramPolicy) -> Result<()> {
    if !(1..=5).contains(&policy.max_attempts) {
        anyhow::bail!("program max_attempts must be between 1 and 5");
    }
    if policy.retry_backoff_secs > MAX_RETRY_BACKOFF_SECS {
        anyhow::bail!("program retry_backoff_secs must be between 0 and 86400");
    }
    if !(1..=10).contains(&policy.max_parallel_milestones) {
        anyhow::bail!("program max_parallel_milestones must be between 1 and 10");
    }
    if policy.token_budget == Some(0) {
        anyhow::bail!("program policy token budget must be greater than zero");
    }
    if policy
        .cost_budget_usd
        .is_some_and(|value| !value.is_finite() || value <= 0.0)
    {
        anyhow::bail!("program policy cost budget must be a positive finite number");
    }
    Ok(())
}

pub fn validate_program_budgets(
    token_budget: Option<u64>,
    cost_budget_usd: Option<f64>,
) -> Result<()> {
    if token_budget == Some(0) {
        anyhow::bail!("program token budget must be greater than zero");
    }
    if cost_budget_usd.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        anyhow::bail!("program cost budget must be a positive finite number");
    }
    Ok(())
}

pub fn validate_milestone_key(key: &str) -> Result<()> {
    let key = key.trim();
    if key.is_empty() || key.len() > 64 {
        anyhow::bail!("milestone key must be 1-64 characters");
    }
    if !key
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_'))
    {
        anyhow::bail!("milestone key may contain only ASCII letters, digits, '.', '-', and '_'");
    }
    if key.starts_with('.') || key.ends_with('.') {
        anyhow::bail!("milestone key cannot start or end with '.'");
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn validate_milestone_fields(
    title: &str,
    objective: &str,
    acceptance: &[String],
    write_scope: &[String],
    profile: &str,
    priority: i32,
    max_attempts: u32,
    token_budget: Option<u64>,
    cost_budget_usd: Option<f64>,
) -> Result<()> {
    if title.trim().is_empty() || title.trim().chars().count() > 300 {
        anyhow::bail!("milestone title must be 1-300 characters");
    }
    if objective.trim().is_empty() || objective.chars().count() > 8_000 {
        anyhow::bail!("milestone objective must be 1-8000 characters");
    }
    if acceptance.is_empty() || acceptance.len() > 32 {
        anyhow::bail!("milestone acceptance must contain 1-32 criteria");
    }
    for criterion in acceptance {
        if criterion.trim().is_empty() || criterion.chars().count() > 2_000 {
            anyhow::bail!("milestone acceptance criteria must be 1-2000 characters");
        }
    }
    if write_scope.len() > 64 {
        anyhow::bail!("milestone write scope cannot contain more than 64 entries");
    }
    for scope in write_scope {
        if scope.trim().is_empty() || scope.chars().count() > 512 {
            anyhow::bail!("milestone write scope entries must be 1-512 characters");
        }
    }
    if !matches!(profile, "coding" | "research" | "verification" | "generic") {
        anyhow::bail!("milestone profile must be coding, research, verification, or generic");
    }
    if !(-100..=100).contains(&priority) {
        anyhow::bail!("milestone priority must be between -100 and 100");
    }
    if !(1..=5).contains(&max_attempts) {
        anyhow::bail!("milestone max_attempts must be between 1 and 5");
    }
    if token_budget == Some(0) {
        anyhow::bail!("milestone token budget must be greater than zero");
    }
    if cost_budget_usd.is_some_and(|value| !value.is_finite() || value <= 0.0) {
        anyhow::bail!("milestone cost budget must be a positive finite number");
    }
    Ok(())
}

pub fn validate_milestone_dependencies(
    all: &[ProgramMilestone],
    milestone_id: Option<&str>,
    deps: &[String],
) -> Result<()> {
    let known = all
        .iter()
        .map(|milestone| milestone.id.as_str())
        .collect::<HashSet<_>>();
    let mut seen = HashSet::new();
    for dependency in deps {
        if !seen.insert(dependency.as_str()) {
            anyhow::bail!("milestone dependency {dependency} is duplicated");
        }
        if Some(dependency.as_str()) == milestone_id {
            anyhow::bail!("a milestone cannot depend on itself");
        }
        if !known.contains(dependency.as_str()) {
            anyhow::bail!("milestone dependency {dependency} does not exist in this program");
        }
    }
    if milestone_id.is_none() {
        // A new node has no dependents yet, and every dependency already forms
        // an acyclic graph because it was validated on creation/update.
        return Ok(());
    }
    let milestone_id = milestone_id.unwrap_or_default();
    let mut edges = all
        .iter()
        .map(|milestone| (milestone.id.clone(), milestone.deps.to_vec()))
        .collect::<HashMap<_, _>>();
    edges.insert(milestone_id.to_string(), deps.to_vec());
    let mut visiting = HashSet::new();
    let mut visited = HashSet::new();
    for node in edges.keys() {
        detect_cycle(node, &edges, &mut visiting, &mut visited)?;
    }
    Ok(())
}

fn detect_cycle(
    node: &str,
    edges: &HashMap<String, Vec<String>>,
    visiting: &mut HashSet<String>,
    visited: &mut HashSet<String>,
) -> Result<()> {
    if visited.contains(node) {
        return Ok(());
    }
    if !visiting.insert(node.to_string()) {
        anyhow::bail!("milestone dependencies contain a cycle at {node}");
    }
    if let Some(dependencies) = edges.get(node) {
        for dependency in dependencies {
            if edges.contains_key(dependency) {
                detect_cycle(dependency, edges, visiting, visited)?;
            }
        }
    }
    visiting.remove(node);
    visited.insert(node.to_string());
    Ok(())
}

pub fn validate_program_milestone_update(
    all: &[ProgramMilestone],
    milestone: &ProgramMilestone,
    update: &ProgramMilestoneUpdate,
) -> Result<()> {
    validate_milestone_key(&update.key)?;
    validate_milestone_fields(
        &update.title,
        &update.objective,
        &update.acceptance,
        &update.write_scope,
        &update.profile,
        update.priority,
        update.max_attempts,
        update.token_budget,
        update.cost_budget_usd,
    )?;
    validate_milestone_dependencies(all, Some(&milestone.id), &update.deps)
}

pub fn next_milestone_key(milestones: &[ProgramMilestone]) -> String {
    let highest = milestones
        .iter()
        .filter_map(|milestone| {
            milestone
                .key
                .split('.')
                .next()
                .and_then(|part| part.parse::<u32>().ok())
        })
        .max()
        .unwrap_or(0);
    (highest.saturating_add(1)).to_string()
}

pub fn milestone_counts(milestones: &[ProgramMilestone]) -> ProgramMilestoneCounts {
    let mut counts = ProgramMilestoneCounts::default();
    for milestone in milestones {
        counts.add(milestone.status);
    }
    counts
}

pub fn program_budget_exhausted(program: &Program, usage: &ProgramUsage) -> bool {
    program
        .token_budget
        .is_some_and(|budget| usage.token_spent >= budget)
        || program
            .cost_budget_usd
            .is_some_and(|budget| usage.cost_spent_usd >= budget)
}

pub fn milestone_budget_exhausted(milestone: &ProgramMilestone, session: &Session) -> bool {
    let tokens = session.tokens_in.saturating_add(session.tokens_out);
    milestone
        .token_budget
        .is_some_and(|budget| tokens >= budget)
        || milestone
            .cost_budget_usd
            .is_some_and(|budget| session.cost_spent_usd >= budget)
}

fn milestone_budget_exhausted_for_program(
    program: &Program,
    milestone: &ProgramMilestone,
    session: &Session,
) -> bool {
    let tokens = session.tokens_in.saturating_add(session.tokens_out);
    let token_budget = milestone.token_budget.or(program.policy.token_budget);
    let cost_budget = milestone.cost_budget_usd.or(program.policy.cost_budget_usd);
    token_budget.is_some_and(|budget| tokens >= budget)
        || cost_budget.is_some_and(|budget| session.cost_spent_usd >= budget)
}

fn effective_budget(
    program: &Program,
    milestone: &ProgramMilestone,
    usage: &ProgramUsage,
) -> Result<(Option<u64>, Option<f64>)> {
    let mut token_budget = milestone.token_budget.or(program.policy.token_budget);
    if let Some(program_budget) = program.token_budget {
        let remaining = program_budget.saturating_sub(usage.token_spent);
        if remaining == 0 {
            anyhow::bail!("program token budget is exhausted");
        }
        token_budget = Some(token_budget.map_or(remaining, |budget| budget.min(remaining)));
    }
    let mut cost_budget = milestone.cost_budget_usd.or(program.policy.cost_budget_usd);
    if let Some(program_budget) = program.cost_budget_usd {
        let remaining = (program_budget - usage.cost_spent_usd).max(0.0);
        if remaining <= f64::EPSILON {
            anyhow::bail!("program cost budget is exhausted");
        }
        cost_budget = Some(cost_budget.map_or(remaining, |budget| budget.min(remaining)));
    }
    Ok((token_budget, cost_budget))
}

fn milestone_effective_priority(
    milestone: &ProgramMilestone,
    now: OffsetDateTime,
    aging: Duration,
) -> i64 {
    let base = i64::from(milestone.priority.clamp(-100, 100));
    let retry_boost = if milestone.attempts > 0 { 10 } else { 0 };
    if aging.is_zero() {
        return base + retry_boost;
    }
    let age = OffsetDateTime::parse(&milestone.created_at, &Rfc3339)
        .map(|created| (now - created).whole_seconds().max(0))
        .unwrap_or(0);
    base + retry_boost + (age / aging.as_secs().max(1) as i64).min(MAX_AGING_BOOST)
}

fn retry_deadline(program: &Program, attempt: u32) -> Result<String> {
    let exponent = attempt.saturating_sub(1).min(20);
    let delay = program
        .policy
        .retry_backoff_secs
        .saturating_mul(1u64 << exponent)
        .min(MAX_RETRY_BACKOFF_SECS);
    (OffsetDateTime::now_utc() + time::Duration::seconds(delay as i64))
        .format(&Rfc3339)
        .context("formatting program retry deadline")
}

fn bounded_reason(reason: &str) -> String {
    reason.trim().chars().take(4_000).collect()
}

fn program_aging() -> Duration {
    let seconds = std::env::var("VERLINPARK_PROGRAM_AGING_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(60)
        .min(86_400);
    if seconds == 0 {
        Duration::ZERO
    } else {
        Duration::from_secs(seconds)
    }
}

fn program_dispatch_grace_secs() -> u64 {
    std::env::var("VERLINPARK_PROGRAM_DISPATCH_GRACE_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_DISPATCH_GRACE_SECS)
        .clamp(1, 3_600)
}

/// Build the input used by the store for a newly created program.
pub fn new_program_input(
    project_id: &str,
    goal: &str,
    policy: ProgramPolicy,
    token_budget: Option<u64>,
    cost_budget_usd: Option<f64>,
    activate: bool,
) -> NewProgram {
    NewProgram {
        project_id: project_id.to_string(),
        goal: goal.trim().to_string(),
        policy,
        token_budget,
        cost_budget_usd,
        status: if activate {
            ProgramStatus::Active
        } else {
            ProgramStatus::Draft
        },
        archived: false,
    }
}

pub fn program_update(
    goal: &str,
    policy: ProgramPolicy,
    token_budget: Option<u64>,
    cost_budget_usd: Option<f64>,
) -> ProgramUpdate {
    ProgramUpdate {
        goal: goal.trim().to_string(),
        policy,
        token_budget,
        cost_budget_usd,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn new_program_milestone_input(
    program_id: &str,
    key: String,
    title: &str,
    objective: &str,
    acceptance: Vec<String>,
    deps: Vec<String>,
    priority: i32,
    max_attempts: u32,
    profile: String,
    write_scope: Vec<String>,
    token_budget: Option<u64>,
    cost_budget_usd: Option<f64>,
) -> NewProgramMilestone {
    NewProgramMilestone {
        program_id: program_id.to_string(),
        key,
        title: title.trim().to_string(),
        objective: objective.trim().to_string(),
        acceptance,
        deps,
        priority,
        max_attempts,
        profile,
        write_scope,
        token_budget,
        cost_budget_usd,
    }
}

#[allow(clippy::too_many_arguments)]
pub fn program_milestone_update(
    key: String,
    title: &str,
    objective: &str,
    acceptance: Vec<String>,
    deps: Vec<String>,
    priority: i32,
    max_attempts: u32,
    profile: String,
    write_scope: Vec<String>,
    token_budget: Option<u64>,
    cost_budget_usd: Option<f64>,
) -> ProgramMilestoneUpdate {
    ProgramMilestoneUpdate {
        key,
        title: title.trim().to_string(),
        objective: objective.trim().to_string(),
        acceptance,
        deps,
        priority,
        max_attempts,
        profile,
        write_scope,
        token_budget,
        cost_budget_usd,
    }
}

/// RPC/CLI-friendly list of program event kinds. Replay accepts every event;
/// this list only documents the vocabulary for operators and tests.
pub const PROGRAM_EVENT_KINDS: &[&str] = &[
    "program.created",
    "program.updated",
    "program.paused",
    "program.resumed",
    "program.completed",
    "program.failed",
    "program.deleted",
    "program.milestone.created",
    "program.milestone.updated",
    "program.milestone.started",
    "program.milestone.verified",
    "program.milestone.completed",
    "program.milestone.failed",
    "program.milestone.blocked",
    "program.milestone.retried",
    "program.milestone.skipped",
    "program.milestone.cancelled",
];

/// Serialize the program row used by event payloads. Kept public for tests
/// that assert full-row payloads without duplicating the event vocabulary.
pub fn program_event_payload(program: &Program) -> serde_json::Value {
    serde_json::to_value(program).unwrap_or(serde_json::Value::Null)
}

pub fn milestone_event_payload(milestone: &ProgramMilestone) -> serde_json::Value {
    serde_json::to_value(milestone).unwrap_or_else(|_| {
        json!({
            "id": milestone.id,
            "program_id": milestone.program_id,
            "status": milestone.status.as_str(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine;
    use tempfile::TempDir;
    use vp_protocol::types::{
        ProgramCreateParams, ProgramMilestoneCreateParams, ProgramMilestoneUpdateParams,
        ProgramReplayParams, ProgramStatusParams, ProgramUpdateParams, Project,
    };
    use vp_store::Store;

    fn setup() -> (TempDir, Engine, Project) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let store = Store::open(data.path()).unwrap();
        let project = store
            .add_project(repo.path().to_str().unwrap(), Some("programs"))
            .unwrap();
        (data, Engine::new(store), project)
    }

    fn policy() -> ProgramPolicy {
        ProgramPolicy {
            auto_dispatch: true,
            max_attempts: 2,
            retry_backoff_secs: 60,
            dependency_failure: DependencyFailurePolicy::Block,
            token_budget: None,
            cost_budget_usd: None,
            max_parallel_milestones: 2,
        }
    }

    async fn create_program(engine: &Engine, project: &Project) -> Program {
        engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Ship a multi-session program".into(),
                policy: Some(policy()),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap()
    }

    async fn add_milestone(
        engine: &Engine,
        program: &Program,
        title: &str,
        deps: Vec<String>,
    ) -> ProgramMilestone {
        engine
            .create_program_milestone(ProgramMilestoneCreateParams {
                program_id: program.id.clone(),
                key: None,
                title: title.into(),
                objective: format!("Deliver {title}."),
                acceptance: vec!["the deliverable is verified".into()],
                deps,
                priority: Some(0),
                max_attempts: Some(2),
                profile: Some("coding".into()),
                write_scope: vec!["src".into()],
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap()
    }

    async fn attach_completed_plan(
        engine: &Engine,
        controller: &ProgramController,
        program: &Program,
        milestone: &ProgramMilestone,
    ) -> Session {
        let store = engine.store().clone();
        let session = store
            .create_session(&program.project_id, "milestone session")
            .unwrap();
        store.update_session_status(&session.id, "idle").unwrap();
        let claimed = controller.claim_start(&milestone.id).unwrap().unwrap();
        controller
            .attach_session(&milestone.id, &session.id, 1)
            .unwrap();
        let plan = milestone_plan(program, &claimed);
        engine.set_team_plan(&session.id, &plan).await.unwrap();
        let seq = store
            .latest_event_seq(&session.id, "team_plan.submitted")
            .unwrap();
        store
            .claim_plan_tasks(&session.id, seq, &["T1".to_string()])
            .unwrap();
        store
            .finish_plan_task(
                &session.id,
                seq,
                "T1",
                "completed",
                Some("verified and merged"),
            )
            .unwrap();
        session
    }

    async fn attach_failed_plan(
        engine: &Engine,
        controller: &ProgramController,
        program: &Program,
        milestone: &ProgramMilestone,
        detail: &str,
    ) -> Session {
        let store = engine.store().clone();
        let session = store
            .create_session(&program.project_id, "failing milestone")
            .unwrap();
        store.update_session_status(&session.id, "idle").unwrap();
        let claimed = controller.claim_start(&milestone.id).unwrap().unwrap();
        controller
            .attach_session(&milestone.id, &session.id, 1)
            .unwrap();
        let plan = milestone_plan(program, &claimed);
        engine.set_team_plan(&session.id, &plan).await.unwrap();
        let seq = store
            .latest_event_seq(&session.id, "team_plan.submitted")
            .unwrap();
        store
            .claim_plan_tasks(&session.id, seq, &["T1".to_string()])
            .unwrap();
        store
            .finish_plan_task(&session.id, seq, "T1", "failed", Some(detail))
            .unwrap();
        session
    }

    #[tokio::test]
    async fn lifecycle_ready_claim_verify_complete_and_program_completion() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        assert_eq!(program.status, ProgramStatus::Active);
        let first = add_milestone(&engine, &program, "Foundation", Vec::new()).await;
        let second = add_milestone(&engine, &program, "Integration", vec![first.id.clone()]).await;
        assert_eq!(first.status, ProgramMilestoneStatus::Pending);
        assert_eq!(second.key, "2");

        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let first = engine
            .store()
            .get_program_milestone(&first.id)
            .unwrap()
            .unwrap();
        let second = engine
            .store()
            .get_program_milestone(&second.id)
            .unwrap()
            .unwrap();
        assert_eq!(first.status, ProgramMilestoneStatus::Ready);
        assert_eq!(second.status, ProgramMilestoneStatus::Pending);

        let candidates = controller.ready_candidates(None, 10).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].milestone.id, first.id);

        let claimed = controller.claim_start(&first.id).unwrap().unwrap();
        assert_eq!(claimed.status, ProgramMilestoneStatus::Running);
        assert_eq!(claimed.attempts, 1);
        assert!(claimed.session_id.is_none());
        // The atomic claim prevents a second session launch.
        assert!(controller.claim_start(&first.id).unwrap().is_none());

        let session = engine.store().create_session(&project.id, "first").unwrap();
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        let attached = controller
            .attach_session(&first.id, &session.id, 1)
            .unwrap();
        assert_eq!(attached.session_id.as_deref(), Some(session.id.as_str()));
        let plan = milestone_plan(&program, &attached);
        engine.set_team_plan(&session.id, &plan).await.unwrap();
        let seq = engine
            .store()
            .latest_event_seq(&session.id, "team_plan.submitted")
            .unwrap();
        engine
            .store()
            .claim_plan_tasks(&session.id, seq, &["T1".to_string()])
            .unwrap();
        engine
            .store()
            .finish_plan_task(
                &session.id,
                seq,
                "T1",
                "completed",
                Some("verified and integrated"),
            )
            .unwrap();

        controller.reconcile(None).await.unwrap();
        let first = engine
            .store()
            .get_program_milestone(&first.id)
            .unwrap()
            .unwrap();
        assert_eq!(first.status, ProgramMilestoneStatus::Verifying);
        controller.reconcile(None).await.unwrap();
        let first = engine
            .store()
            .get_program_milestone(&first.id)
            .unwrap()
            .unwrap();
        assert_eq!(first.status, ProgramMilestoneStatus::Completed);
        assert!(first.result.is_some());
        let program = engine.store().get_program(&program.id).unwrap().unwrap();
        assert_eq!(program.status, ProgramStatus::Active);

        controller.reconcile(None).await.unwrap();
        let second = engine
            .store()
            .get_program_milestone(&second.id)
            .unwrap()
            .unwrap();
        assert_eq!(second.status, ProgramMilestoneStatus::Ready);
        let _ = attach_completed_plan(&engine, &controller, &program, &second).await;
        controller.reconcile(None).await.unwrap();
        controller.reconcile(None).await.unwrap();
        let second = engine
            .store()
            .get_program_milestone(&second.id)
            .unwrap()
            .unwrap();
        assert_eq!(second.status, ProgramMilestoneStatus::Completed);
        let program = engine.store().get_program(&program.id).unwrap().unwrap();
        assert_eq!(program.status, ProgramStatus::Completed);
        assert!(program.completed_at.is_some());
    }

    #[tokio::test]
    async fn pause_stops_new_dispatch_but_resume_restores_ready_work() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let milestone = add_milestone(&engine, &program, "Paused work", Vec::new()).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        assert_eq!(
            engine
                .store()
                .get_program_milestone(&milestone.id)
                .unwrap()
                .unwrap()
                .status,
            ProgramMilestoneStatus::Ready
        );
        engine
            .pause_program(vp_protocol::types::ProgramActionParams {
                program_id: program.id.clone(),
                reason: Some("operator pause".into()),
            })
            .await
            .unwrap();
        assert!(controller.ready_candidates(None, 10).unwrap().is_empty());
        assert!(
            controller.claim_start(&milestone.id).unwrap().is_none(),
            "a paused program must reject even a manual claim"
        );
        engine
            .resume_program(vp_protocol::types::ProgramActionParams {
                program_id: program.id.clone(),
                reason: None,
            })
            .await
            .unwrap();
        assert_eq!(controller.ready_candidates(None, 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn transient_failure_schedules_backoff_and_manual_retry_resets_attempts() {
        let (_dir, engine, project) = setup();
        let mut program_policy = policy();
        program_policy.retry_backoff_secs = 3_600;
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Retry a milestone".into(),
                policy: Some(program_policy),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let milestone = add_milestone(&engine, &program, "Retry me", Vec::new()).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let _ =
            attach_failed_plan(&engine, &controller, &program, &milestone, "tests failed").await;
        controller.reconcile(None).await.unwrap();
        let failed = engine
            .store()
            .get_program_milestone(&milestone.id)
            .unwrap()
            .unwrap();
        assert_eq!(failed.status, ProgramMilestoneStatus::Failed);
        assert!(failed.next_attempt_at.is_some());
        assert_eq!(failed.last_error.as_deref(), Some("tests failed"));
        assert!(controller.ready_candidates(None, 10).unwrap().is_empty());
        let events = engine
            .store()
            .list_program_events(&program.id, Some(100))
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == "program.milestone.failed")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == "program.milestone.retried")
        );

        engine
            .retry_program_milestone(vp_protocol::types::ProgramMilestoneActionParams {
                milestone_id: milestone.id.clone(),
                reason: Some("operator retry".into()),
            })
            .await
            .unwrap();
        let retried = engine
            .store()
            .get_program_milestone(&milestone.id)
            .unwrap()
            .unwrap();
        assert_eq!(retried.status, ProgramMilestoneStatus::Pending);
        assert_eq!(retried.attempts, 0);
        assert!(retried.next_attempt_at.is_none());
        controller.reconcile(None).await.unwrap();
        assert_eq!(controller.ready_candidates(None, 10).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn exhausted_attempts_block_and_dependency_policy_blocks_dependents() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let first = engine
            .create_program_milestone(ProgramMilestoneCreateParams {
                program_id: program.id.clone(),
                key: None,
                title: "Fails permanently".into(),
                objective: "fail".into(),
                acceptance: vec!["never".into()],
                deps: Vec::new(),
                priority: Some(0),
                max_attempts: Some(1),
                profile: Some("coding".into()),
                write_scope: vec!["src".into()],
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap();
        let second = add_milestone(
            &engine,
            &program,
            "Depends on failure",
            vec![first.id.clone()],
        )
        .await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let _ = attach_failed_plan(&engine, &controller, &program, &first, "boom").await;
        controller.reconcile(None).await.unwrap();
        controller.reconcile(None).await.unwrap();
        let first = engine
            .store()
            .get_program_milestone(&first.id)
            .unwrap()
            .unwrap();
        let second = engine
            .store()
            .get_program_milestone(&second.id)
            .unwrap()
            .unwrap();
        assert_eq!(first.status, ProgramMilestoneStatus::Blocked);
        assert_eq!(second.status, ProgramMilestoneStatus::Blocked);
        let program = engine.store().get_program(&program.id).unwrap().unwrap();
        assert_eq!(program.status, ProgramStatus::Failed);
        assert!(program.reason.unwrap().contains("blocked"));
    }

    #[tokio::test]
    async fn dependency_cancel_policy_cancels_dependents() {
        let (_dir, engine, project) = setup();
        let mut program_policy = policy();
        program_policy.dependency_failure = DependencyFailurePolicy::Cancel;
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Cancel dependents".into(),
                policy: Some(program_policy),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let first = engine
            .create_program_milestone(ProgramMilestoneCreateParams {
                program_id: program.id.clone(),
                key: None,
                title: "Root".into(),
                objective: "root".into(),
                acceptance: vec!["done".into()],
                deps: Vec::new(),
                priority: Some(0),
                max_attempts: Some(1),
                profile: Some("coding".into()),
                write_scope: vec!["src".into()],
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap();
        let second = add_milestone(&engine, &program, "Child", vec![first.id.clone()]).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let _ = attach_failed_plan(&engine, &controller, &program, &first, "boom").await;
        controller.reconcile(None).await.unwrap();
        controller.reconcile(None).await.unwrap();
        let first = engine
            .store()
            .get_program_milestone(&first.id)
            .unwrap()
            .unwrap();
        let second = engine
            .store()
            .get_program_milestone(&second.id)
            .unwrap()
            .unwrap();
        assert_eq!(first.status, ProgramMilestoneStatus::Blocked);
        assert_eq!(second.status, ProgramMilestoneStatus::Cancelled);
    }

    #[tokio::test]
    async fn dependency_retry_policy_leaves_dependents_pending_without_spinning() {
        let (_dir, engine, project) = setup();
        let mut program_policy = policy();
        program_policy.dependency_failure = DependencyFailurePolicy::Retry;
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Retry dependencies".into(),
                policy: Some(program_policy),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let first = engine
            .create_program_milestone(ProgramMilestoneCreateParams {
                program_id: program.id.clone(),
                key: None,
                title: "Root".into(),
                objective: "root".into(),
                acceptance: vec!["done".into()],
                deps: Vec::new(),
                priority: Some(0),
                max_attempts: Some(1),
                profile: Some("coding".into()),
                write_scope: vec!["src".into()],
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap();
        let second = add_milestone(&engine, &program, "Child", vec![first.id.clone()]).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let _ = attach_failed_plan(&engine, &controller, &program, &first, "boom").await;
        controller.reconcile(None).await.unwrap();
        controller.reconcile(None).await.unwrap();
        assert_eq!(
            engine
                .store()
                .get_program_milestone(&first.id)
                .unwrap()
                .unwrap()
                .status,
            ProgramMilestoneStatus::Blocked
        );
        assert_eq!(
            engine
                .store()
                .get_program_milestone(&second.id)
                .unwrap()
                .unwrap()
                .status,
            ProgramMilestoneStatus::Pending
        );
        assert!(controller.ready_candidates(None, 10).unwrap().is_empty());
        engine
            .retry_program_milestone(vp_protocol::types::ProgramMilestoneActionParams {
                milestone_id: first.id.clone(),
                reason: None,
            })
            .await
            .unwrap();
        controller.reconcile(None).await.unwrap();
        let candidates = controller.ready_candidates(None, 10).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].milestone.id, first.id);
    }

    #[tokio::test]
    async fn program_budget_exhaustion_blocks_work_and_fails_closed() {
        let (_dir, engine, project) = setup();
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Budgeted program".into(),
                policy: Some(policy()),
                token_budget: Some(100),
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let first = add_milestone(&engine, &program, "Spender", Vec::new()).await;
        let second = add_milestone(&engine, &program, "Should not run", Vec::new()).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let session = engine
            .store()
            .create_session_with_limits(&project.id, "spend", Some(100), None, None)
            .unwrap();
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        let claimed = controller.claim_start(&first.id).unwrap().unwrap();
        let _ = claimed;
        controller
            .attach_session(&first.id, &session.id, 1)
            .unwrap();
        engine
            .store()
            .add_session_usage(&session.id, 90, 30)
            .unwrap();
        engine
            .skip_program_milestone(vp_protocol::types::ProgramMilestoneActionParams {
                milestone_id: first.id.clone(),
                reason: Some("settled".into()),
            })
            .await
            .unwrap();
        controller.reconcile(None).await.unwrap();
        let second = engine
            .store()
            .get_program_milestone(&second.id)
            .unwrap()
            .unwrap();
        assert_eq!(second.status, ProgramMilestoneStatus::Blocked);
        assert!(second.last_error.unwrap().contains("budget"));
        let program = engine.store().get_program(&program.id).unwrap().unwrap();
        assert_eq!(program.status, ProgramStatus::Failed);
        assert!(controller.ready_candidates(None, 10).unwrap().is_empty());
    }

    #[tokio::test]
    async fn milestone_budget_exhaustion_blocks_instead_of_retrying() {
        let (_dir, engine, project) = setup();
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Milestone budget".into(),
                policy: Some(policy()),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let milestone = engine
            .create_program_milestone(ProgramMilestoneCreateParams {
                program_id: program.id.clone(),
                key: None,
                title: "Budgeted milestone".into(),
                objective: "spend".into(),
                acceptance: vec!["done".into()],
                deps: Vec::new(),
                priority: Some(0),
                max_attempts: Some(5),
                profile: Some("coding".into()),
                write_scope: vec!["src".into()],
                token_budget: Some(100),
                cost_budget_usd: None,
            })
            .await
            .unwrap();
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let session = engine.store().create_session(&project.id, "spend").unwrap();
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        let claimed = controller.claim_start(&milestone.id).unwrap().unwrap();
        controller
            .attach_session(&milestone.id, &session.id, 1)
            .unwrap();
        let plan = milestone_plan(&program, &claimed);
        engine.set_team_plan(&session.id, &plan).await.unwrap();
        let seq = engine
            .store()
            .latest_event_seq(&session.id, "team_plan.submitted")
            .unwrap();
        engine
            .store()
            .claim_plan_tasks(&session.id, seq, &["T1".to_string()])
            .unwrap();
        engine
            .store()
            .finish_plan_task(&session.id, seq, "T1", "failed", Some("budget stop"))
            .unwrap();
        engine
            .store()
            .add_session_usage(&session.id, 80, 40)
            .unwrap();
        controller.reconcile(None).await.unwrap();
        let milestone = engine
            .store()
            .get_program_milestone(&milestone.id)
            .unwrap()
            .unwrap();
        assert_eq!(milestone.status, ProgramMilestoneStatus::Blocked);
        assert!(milestone.last_error.unwrap().contains("budget"));
        assert!(milestone.next_attempt_at.is_none());
    }

    #[tokio::test]
    async fn readiness_respects_priority_aging_and_max_parallel() {
        let (_dir, engine, project) = setup();
        let mut program_policy = policy();
        program_policy.max_parallel_milestones = 1;
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Priority program".into(),
                policy: Some(program_policy.clone()),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let low = add_milestone(&engine, &program, "Low", Vec::new()).await;
        let high = engine
            .create_program_milestone(ProgramMilestoneCreateParams {
                program_id: program.id.clone(),
                key: None,
                title: "High".into(),
                objective: "high".into(),
                acceptance: vec!["done".into()],
                deps: Vec::new(),
                priority: Some(50),
                max_attempts: Some(2),
                profile: Some("coding".into()),
                write_scope: vec!["src".into()],
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap();
        let controller = ProgramController::with_aging(engine.store().clone(), Duration::ZERO);
        controller.reconcile(None).await.unwrap();
        let candidates = controller.ready_candidates(None, 10).unwrap();
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].milestone.id, high.id);
        controller.claim_start(&high.id).unwrap().unwrap();
        assert!(
            controller
                .ready_candidates(None, 10)
                .unwrap()
                .iter()
                .all(|candidate| candidate.milestone.id != low.id)
        );
        assert_eq!(program_policy.max_parallel_milestones, 1);
    }

    #[tokio::test]
    async fn validation_rejects_bad_bounds_duplicate_keys_and_cycles() {
        let (_dir, engine, project) = setup();
        let mut bad = policy();
        bad.max_attempts = 0;
        let error = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Bad".into(),
                policy: Some(bad),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("max_attempts"));

        let program = create_program(&engine, &project).await;
        let first = add_milestone(&engine, &program, "First", Vec::new()).await;
        let second = add_milestone(&engine, &program, "Second", vec![first.id.clone()]).await;
        let duplicate_key = engine
            .create_program_milestone(ProgramMilestoneCreateParams {
                program_id: program.id.clone(),
                key: Some(first.key.clone()),
                title: "Duplicate".into(),
                objective: "duplicate".into(),
                acceptance: vec!["done".into()],
                deps: Vec::new(),
                priority: Some(0),
                max_attempts: Some(2),
                profile: Some("coding".into()),
                write_scope: Vec::new(),
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap_err();
        assert!(duplicate_key.to_string().contains("already exists"));

        let cycle = engine
            .update_program_milestone(ProgramMilestoneUpdateParams {
                milestone_id: first.id.clone(),
                title: first.title.clone(),
                objective: first.objective.clone(),
                key: Some(first.key.clone()),
                acceptance: first.acceptance.clone(),
                deps: vec![second.id.clone()],
                priority: Some(0),
                max_attempts: Some(2),
                profile: Some("coding".into()),
                write_scope: vec!["src".into()],
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap_err();
        assert!(cycle.to_string().contains("cycle"));
    }

    #[tokio::test]
    async fn events_are_full_rows_and_replay_repair_restores_projection() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let milestone = add_milestone(&engine, &program, "Replay", Vec::new()).await;
        let events = engine
            .store()
            .list_program_events(&program.id, Some(100))
            .unwrap();
        let created = events
            .iter()
            .find(|event| event.kind == "program.created")
            .unwrap();
        let payload: Program = serde_json::from_value(created.payload.clone()).unwrap();
        assert_eq!(payload, program);
        let milestone_event = events
            .iter()
            .find(|event| event.kind == "program.milestone.created")
            .unwrap();
        let milestone_payload: ProgramMilestone =
            serde_json::from_value(milestone_event.payload.clone()).unwrap();
        assert_eq!(milestone_payload, milestone);

        let mut corrupt = program.clone();
        corrupt.goal = "corrupted outside the event log".into();
        engine.store().insert_program_snapshot(&corrupt).unwrap();
        let report = engine.replay_program(&program.id, false).await.unwrap();
        assert!(!report.consistent);
        assert_eq!(report.event_count, events.len());
        let repaired = engine.replay_program(&program.id, true).await.unwrap();
        assert!(repaired.repaired);
        let restored = engine.store().get_program(&program.id).unwrap().unwrap();
        assert_eq!(restored.goal, program.goal);
        let report = engine.replay_program(&program.id, false).await.unwrap();
        assert!(report.consistent);
    }

    #[tokio::test]
    async fn delete_is_tombstoned_and_replay_understands_it() {
        let (_dir, engine, project) = setup();
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Delete me".into(),
                policy: Some(policy()),
                token_budget: None,
                cost_budget_usd: None,
                activate: false,
            })
            .await
            .unwrap();
        let _milestone = add_milestone(&engine, &program, "Draft milestone", Vec::new()).await;
        engine
            .delete_program(vp_protocol::types::ProgramDeleteParams {
                program_id: program.id.clone(),
            })
            .await
            .unwrap();
        assert!(engine.store().get_program(&program.id).unwrap().is_none());
        assert!(
            engine
                .store()
                .list_program_milestones(&program.id)
                .unwrap()
                .is_empty()
        );
        let events = engine
            .store()
            .list_program_events(&program.id, Some(100))
            .unwrap();
        assert!(events.iter().any(|event| event.kind == "program.deleted"));
        let report = engine.replay_program(&program.id, false).await.unwrap();
        assert!(report.consistent);
    }

    #[tokio::test]
    async fn manifest_contains_program_rows_and_events() {
        let (dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let _milestone = add_milestone(&engine, &program, "Manifest milestone", Vec::new()).await;
        engine.record_project_manifest().await.unwrap();
        let snapshots = vp_store::read_project_manifest(dir.path()).unwrap();
        assert_eq!(snapshots.len(), 1);
        let snapshot = &snapshots[0];
        assert_eq!(snapshot.programs.len(), 1);
        assert_eq!(snapshot.programs[0].id, program.id);
        assert_eq!(snapshot.program_milestones.len(), 1);
        assert!(
            snapshot
                .program_events
                .iter()
                .any(|event| event.kind == "program.created")
        );
        assert_eq!(project.id, snapshot.project.id);
    }

    #[tokio::test]
    async fn status_reports_counts_usage_and_bounded_events() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let _first = add_milestone(&engine, &program, "One", Vec::new()).await;
        let _second = add_milestone(&engine, &program, "Two", Vec::new()).await;
        let status = engine
            .program_status(ProgramStatusParams {
                program_id: program.id.clone(),
                event_limit: Some(2),
            })
            .await
            .unwrap();
        assert_eq!(status.counts.total(), 2);
        assert_eq!(status.counts.pending, 2);
        assert_eq!(status.events.len(), 2);
        assert_eq!(status.usage, ProgramUsage::default());
        let list = engine
            .list_programs(
                Some(project.id.clone()),
                Some(ProgramStatus::Active),
                Some(10),
            )
            .await
            .unwrap();
        assert_eq!(list.programs.len(), 1);
        assert_eq!(list.programs[0].counts.total(), 2);
    }

    #[tokio::test]
    async fn update_program_and_milestone_keep_events_consistent() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let milestone = add_milestone(&engine, &program, "Editable", Vec::new()).await;
        let mut changed_policy = policy();
        changed_policy.max_parallel_milestones = 1;
        let updated = engine
            .update_program(ProgramUpdateParams {
                program_id: program.id.clone(),
                goal: "Updated goal".into(),
                policy: changed_policy,
                token_budget: Some(500),
                cost_budget_usd: Some(2.0),
            })
            .await
            .unwrap();
        assert_eq!(updated.goal, "Updated goal");
        assert_eq!(updated.token_budget, Some(500));
        let updated_milestone = engine
            .update_program_milestone(ProgramMilestoneUpdateParams {
                milestone_id: milestone.id.clone(),
                title: "Edited title".into(),
                objective: "Edited objective".into(),
                key: Some(milestone.key.clone()),
                acceptance: vec!["edited acceptance".into()],
                deps: Vec::new(),
                priority: Some(7),
                max_attempts: Some(3),
                profile: Some("research".into()),
                write_scope: vec!["docs".into()],
                token_budget: Some(1_000),
                cost_budget_usd: None,
            })
            .await
            .unwrap();
        assert_eq!(updated_milestone.profile, "research");
        assert_eq!(updated_milestone.priority, 7);
        let report = engine.replay_program(&program.id, false).await.unwrap();
        assert!(report.consistent);
        let events = engine
            .store()
            .list_program_events(&program.id, Some(100))
            .unwrap();
        assert!(events.iter().any(|event| event.kind == "program.updated"));
        assert!(
            events
                .iter()
                .any(|event| event.kind == "program.milestone.updated")
        );
    }

    #[tokio::test]
    async fn program_replay_params_decode_and_unknown_fields_are_ignored() {
        let params: ProgramReplayParams =
            serde_json::from_value(json!({"program_id": "p1", "repair": true})).unwrap();
        assert!(params.repair);
        let legacy: ProgramReplayParams =
            serde_json::from_value(json!({"program_id": "p1"})).unwrap();
        assert!(!legacy.repair);
    }

    #[tokio::test]
    async fn cost_budget_exhaustion_blocks_work_and_fails_closed() {
        let (_dir, engine, project) = setup();
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Cost budget".into(),
                policy: Some(policy()),
                token_budget: None,
                cost_budget_usd: Some(1.0),
                activate: true,
            })
            .await
            .unwrap();
        let first = add_milestone(&engine, &program, "Cost spender", Vec::new()).await;
        let second = add_milestone(&engine, &program, "Blocked by cost", Vec::new()).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let session = engine
            .store()
            .create_session(&project.id, "cost session")
            .unwrap();
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        controller.claim_start(&first.id).unwrap().unwrap();
        controller
            .attach_session(&first.id, &session.id, 1)
            .unwrap();
        engine
            .store()
            .add_session_cost_with_event(&session.id, 1.5)
            .unwrap();
        engine
            .skip_program_milestone(vp_protocol::types::ProgramMilestoneActionParams {
                milestone_id: first.id.clone(),
                reason: Some("settled".into()),
            })
            .await
            .unwrap();
        controller.reconcile(None).await.unwrap();
        let second = engine
            .store()
            .get_program_milestone(&second.id)
            .unwrap()
            .unwrap();
        assert_eq!(second.status, ProgramMilestoneStatus::Blocked);
        assert!(second.last_error.unwrap().contains("budget"));
        assert_eq!(
            engine
                .store()
                .get_program(&program.id)
                .unwrap()
                .unwrap()
                .status,
            ProgramStatus::Failed
        );
    }

    #[tokio::test]
    async fn pause_stops_dispatch_but_an_in_flight_milestone_can_complete() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let milestone = add_milestone(&engine, &program, "In flight", Vec::new()).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let claimed = controller.claim_start(&milestone.id).unwrap().unwrap();
        let session = engine
            .store()
            .create_session(&project.id, "in flight")
            .unwrap();
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        controller
            .attach_session(&milestone.id, &session.id, 1)
            .unwrap();
        let plan = milestone_plan(&program, &claimed);
        engine.set_team_plan(&session.id, &plan).await.unwrap();
        let seq = engine
            .store()
            .latest_event_seq(&session.id, "team_plan.submitted")
            .unwrap();
        engine
            .store()
            .claim_plan_tasks(&session.id, seq, &["T1".to_string()])
            .unwrap();
        engine
            .store()
            .finish_plan_task(&session.id, seq, "T1", "completed", Some("done"))
            .unwrap();
        engine
            .pause_program(vp_protocol::types::ProgramActionParams {
                program_id: program.id.clone(),
                reason: Some("pause while running".into()),
            })
            .await
            .unwrap();
        controller.reconcile(None).await.unwrap();
        controller.reconcile(None).await.unwrap();
        let milestone = engine
            .store()
            .get_program_milestone(&milestone.id)
            .unwrap()
            .unwrap();
        assert_eq!(milestone.status, ProgramMilestoneStatus::Completed);
        let program = engine.store().get_program(&program.id).unwrap().unwrap();
        assert_eq!(program.status, ProgramStatus::Completed);
    }

    #[tokio::test]
    async fn cancelled_sibling_still_allows_a_program_with_one_completion() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let first = add_milestone(&engine, &program, "Completed", Vec::new()).await;
        let second = add_milestone(&engine, &program, "Skipped", Vec::new()).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let session = engine
            .store()
            .create_session(&project.id, "completion")
            .unwrap();
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        controller.claim_start(&first.id).unwrap().unwrap();
        controller
            .attach_session(&first.id, &session.id, 1)
            .unwrap();
        let plan = milestone_plan(&program, &first);
        engine.set_team_plan(&session.id, &plan).await.unwrap();
        let seq = engine
            .store()
            .latest_event_seq(&session.id, "team_plan.submitted")
            .unwrap();
        engine
            .store()
            .claim_plan_tasks(&session.id, seq, &["T1".to_string()])
            .unwrap();
        engine
            .store()
            .finish_plan_task(&session.id, seq, "T1", "completed", Some("done"))
            .unwrap();
        controller.reconcile(None).await.unwrap();
        controller.reconcile(None).await.unwrap();
        engine
            .skip_program_milestone(vp_protocol::types::ProgramMilestoneActionParams {
                milestone_id: second.id.clone(),
                reason: Some("not needed".into()),
            })
            .await
            .unwrap();
        controller.reconcile(None).await.unwrap();
        let program = engine.store().get_program(&program.id).unwrap().unwrap();
        assert_eq!(program.status, ProgramStatus::Completed);
    }

    #[tokio::test]
    async fn program_with_only_cancelled_milestones_fails_instead_of_claiming_success() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let milestone = add_milestone(&engine, &program, "Only milestone", Vec::new()).await;
        engine
            .skip_program_milestone(vp_protocol::types::ProgramMilestoneActionParams {
                milestone_id: milestone.id.clone(),
                reason: Some("skipped".into()),
            })
            .await
            .unwrap();
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let program = engine.store().get_program(&program.id).unwrap().unwrap();
        assert_eq!(program.status, ProgramStatus::Failed);
        assert!(program.reason.unwrap().contains("no completed milestones"));
    }

    #[tokio::test]
    async fn delete_rejects_a_running_milestone() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let milestone = add_milestone(&engine, &program, "Running", Vec::new()).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        controller.claim_start(&milestone.id).unwrap().unwrap();
        let error = engine
            .delete_program(vp_protocol::types::ProgramDeleteParams {
                program_id: program.id.clone(),
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("running"));
        assert!(engine.store().get_program(&program.id).unwrap().is_some());
    }

    #[tokio::test]
    async fn archived_programs_cannot_resume_and_are_never_ready() {
        let (_dir, engine, project) = setup();
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Archive".into(),
                policy: Some(policy()),
                token_budget: None,
                cost_budget_usd: None,
                activate: false,
            })
            .await
            .unwrap();
        let milestone = add_milestone(&engine, &program, "Archived work", Vec::new()).await;
        let mut archived = program.clone();
        archived.archived = true;
        archived.reason = Some("imported archive".into());
        engine.store().insert_program_snapshot(&archived).unwrap();
        let error = engine
            .resume_program(vp_protocol::types::ProgramActionParams {
                program_id: program.id.clone(),
                reason: None,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("archive"));
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        assert!(controller.ready_candidates(None, 10).unwrap().is_empty());
        assert!(controller.claim_start(&milestone.id).unwrap().is_none());
    }

    #[tokio::test]
    async fn project_scoped_tick_does_not_advance_another_project() {
        let (_dir, engine, first_project) = setup();
        let other_repo = TempDir::new().unwrap();
        let second_project = engine
            .store()
            .add_project(other_repo.path().to_str().unwrap(), Some("other"))
            .unwrap();
        let first_program = create_program(&engine, &first_project).await;
        let first_milestone = add_milestone(&engine, &first_program, "First", Vec::new()).await;
        let second_program = engine
            .create_program(ProgramCreateParams {
                project_id: second_project.id.clone(),
                goal: "Second project".into(),
                policy: Some(policy()),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let second_milestone = add_milestone(&engine, &second_program, "Second", Vec::new()).await;
        let controller = engine.programs();
        controller.reconcile(Some(&first_project.id)).await.unwrap();
        assert_eq!(
            engine
                .store()
                .get_program_milestone(&first_milestone.id)
                .unwrap()
                .unwrap()
                .status,
            ProgramMilestoneStatus::Ready
        );
        assert_eq!(
            engine
                .store()
                .get_program_milestone(&second_milestone.id)
                .unwrap()
                .unwrap()
                .status,
            ProgramMilestoneStatus::Pending
        );
    }

    #[tokio::test]
    async fn ready_candidates_round_robin_on_equal_priority() {
        let (_dir, engine, first_project) = setup();
        let other_repo = TempDir::new().unwrap();
        let second_project = engine
            .store()
            .add_project(other_repo.path().to_str().unwrap(), Some("other"))
            .unwrap();
        let first_program = create_program(&engine, &first_project).await;
        let _first = add_milestone(&engine, &first_program, "First", Vec::new()).await;
        let second_program = engine
            .create_program(ProgramCreateParams {
                project_id: second_project.id.clone(),
                goal: "Second".into(),
                policy: Some(policy()),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let _second = add_milestone(&engine, &second_program, "Second", Vec::new()).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let candidates = controller
            .ready_candidates(Some(&first_program.id), 10)
            .unwrap();
        assert_eq!(candidates.len(), 2);
        assert_ne!(candidates[0].program.id, first_program.id);
        let candidates = controller
            .ready_candidates(Some(&second_program.id), 10)
            .unwrap();
        assert_ne!(candidates[0].program.id, second_program.id);
    }

    #[tokio::test]
    async fn skip_emits_skipped_event_and_terminal_cancelled_row() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let milestone = add_milestone(&engine, &program, "Skip", Vec::new()).await;
        let skipped = engine
            .skip_program_milestone(vp_protocol::types::ProgramMilestoneActionParams {
                milestone_id: milestone.id.clone(),
                reason: Some("operator skip".into()),
            })
            .await
            .unwrap();
        assert_eq!(skipped.status, ProgramMilestoneStatus::Cancelled);
        assert!(skipped.completed_at.is_some());
        let events = engine
            .store()
            .list_program_events(&program.id, Some(100))
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == "program.milestone.skipped")
        );
    }

    #[tokio::test]
    async fn dependency_retry_unblocks_after_the_dependency_completes() {
        let (_dir, engine, project) = setup();
        let mut program_policy = policy();
        program_policy.dependency_failure = DependencyFailurePolicy::Retry;
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Unblock".into(),
                policy: Some(program_policy),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let first = engine
            .create_program_milestone(ProgramMilestoneCreateParams {
                program_id: program.id.clone(),
                key: None,
                title: "Root".into(),
                objective: "root".into(),
                acceptance: vec!["done".into()],
                deps: Vec::new(),
                priority: Some(0),
                max_attempts: Some(1),
                profile: Some("coding".into()),
                write_scope: vec!["src".into()],
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap();
        let second = add_milestone(&engine, &program, "Child", vec![first.id.clone()]).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let _ = attach_failed_plan(&engine, &controller, &program, &first, "boom").await;
        controller.reconcile(None).await.unwrap();
        controller.reconcile(None).await.unwrap();
        assert_eq!(
            engine
                .store()
                .get_program_milestone(&second.id)
                .unwrap()
                .unwrap()
                .status,
            ProgramMilestoneStatus::Pending
        );
        engine
            .retry_program_milestone(vp_protocol::types::ProgramMilestoneActionParams {
                milestone_id: first.id.clone(),
                reason: None,
            })
            .await
            .unwrap();
        controller.reconcile(None).await.unwrap();
        let retried = engine
            .store()
            .get_program_milestone(&first.id)
            .unwrap()
            .unwrap();
        assert_eq!(retried.status, ProgramMilestoneStatus::Ready);
        let _ = attach_completed_plan(&engine, &controller, &program, &retried).await;
        controller.reconcile(None).await.unwrap();
        controller.reconcile(None).await.unwrap();
        controller.reconcile(None).await.unwrap();
        assert_eq!(
            engine
                .store()
                .get_program_milestone(&second.id)
                .unwrap()
                .unwrap()
                .status,
            ProgramMilestoneStatus::Ready
        );
    }

    #[tokio::test]
    async fn replay_repair_restores_a_corrupted_milestone_row() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let milestone = add_milestone(&engine, &program, "Replay repair", Vec::new()).await;
        let mut corrupt = milestone.clone();
        corrupt.title = "corrupted milestone".into();
        corrupt.status = ProgramMilestoneStatus::Blocked;
        engine
            .store()
            .insert_program_milestone_snapshot(&corrupt)
            .unwrap();
        let report = engine.replay_program(&program.id, false).await.unwrap();
        assert!(!report.consistent);
        assert!(!report.milestones_match);
        let report = engine.replay_program(&program.id, true).await.unwrap();
        assert!(report.repaired);
        let restored = engine
            .store()
            .get_program_milestone(&milestone.id)
            .unwrap()
            .unwrap();
        assert_eq!(restored.title, "Replay repair");
        assert_eq!(restored.status, ProgramMilestoneStatus::Pending);
        assert!(
            engine
                .replay_program(&program.id, false)
                .await
                .unwrap()
                .consistent
        );
    }

    #[tokio::test]
    async fn engine_tick_reports_ready_work_without_starting_sessions() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let milestone = add_milestone(&engine, &program, "Tick only", Vec::new()).await;
        let tick = engine
            .program_tick(vp_protocol::types::ProgramTickParams {
                project_id: Some(project.id.clone()),
            })
            .await
            .unwrap();
        assert_eq!(tick.programs, 1);
        assert_eq!(tick.sessions_started, 0);
        assert_eq!(tick.ready.len(), 1);
        assert_eq!(tick.ready[0].id, milestone.id);
        assert!(
            engine
                .list_sessions(Some(project.id.clone()))
                .await
                .unwrap()
                .is_empty(),
            "the diagnostic tick must not create a session"
        );
    }

    #[tokio::test]
    async fn terminal_programs_reject_updates_and_running_milestones_reject_edits() {
        let (_dir, engine, project) = setup();
        let program = create_program(&engine, &project).await;
        let milestone = add_milestone(&engine, &program, "Edit guard", Vec::new()).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        controller.claim_start(&milestone.id).unwrap().unwrap();
        let error = engine
            .update_program_milestone(ProgramMilestoneUpdateParams {
                milestone_id: milestone.id.clone(),
                title: "Changed".into(),
                objective: "Changed".into(),
                key: Some(milestone.key.clone()),
                acceptance: vec!["changed".into()],
                deps: Vec::new(),
                priority: Some(0),
                max_attempts: Some(2),
                profile: Some("coding".into()),
                write_scope: Vec::new(),
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("cannot be edited"));

        engine
            .skip_program_milestone(vp_protocol::types::ProgramMilestoneActionParams {
                milestone_id: milestone.id.clone(),
                reason: Some("done".into()),
            })
            .await
            .unwrap();
        controller.reconcile(None).await.unwrap();
        let terminal = engine.store().get_program(&program.id).unwrap().unwrap();
        assert!(terminal.status.is_terminal());
        let error = engine
            .update_program(ProgramUpdateParams {
                program_id: program.id.clone(),
                goal: "Should not change".into(),
                policy: policy(),
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap_err();
        assert!(error.to_string().contains("terminal"));
    }

    #[tokio::test]
    async fn program_list_filters_by_status_and_project() {
        let (_dir, engine, project) = setup();
        let other_repo = TempDir::new().unwrap();
        let other_project = engine
            .store()
            .add_project(other_repo.path().to_str().unwrap(), Some("other"))
            .unwrap();
        let active = create_program(&engine, &project).await;
        let draft = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Draft program".into(),
                policy: None,
                token_budget: None,
                cost_budget_usd: None,
                activate: false,
            })
            .await
            .unwrap();
        engine
            .create_program(ProgramCreateParams {
                project_id: other_project.id.clone(),
                goal: "Other project program".into(),
                policy: Some(policy()),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let active_list = engine
            .list_programs(Some(project.id.clone()), Some(ProgramStatus::Active), None)
            .await
            .unwrap();
        assert_eq!(active_list.programs.len(), 1);
        assert_eq!(active_list.programs[0].program.id, active.id);
        let draft_list = engine
            .list_programs(Some(project.id.clone()), Some(ProgramStatus::Draft), None)
            .await
            .unwrap();
        assert_eq!(draft_list.programs.len(), 1);
        assert_eq!(draft_list.programs[0].program.id, draft.id);
        let all = engine.list_programs(None, None, None).await.unwrap();
        assert_eq!(all.programs.len(), 3);
    }

    #[tokio::test]
    async fn parallel_candidates_come_from_multiple_programs() {
        let (_dir, engine, first_project) = setup();
        let other_repo = TempDir::new().unwrap();
        let second_project = engine
            .store()
            .add_project(other_repo.path().to_str().unwrap(), Some("other"))
            .unwrap();
        let first_program = create_program(&engine, &first_project).await;
        let first = add_milestone(&engine, &first_program, "First", Vec::new()).await;
        let second_program = engine
            .create_program(ProgramCreateParams {
                project_id: second_project.id.clone(),
                goal: "Second".into(),
                policy: Some(policy()),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let second = add_milestone(&engine, &second_program, "Second", Vec::new()).await;
        let controller = engine.programs();
        controller.reconcile(None).await.unwrap();
        let candidates = controller.ready_candidates(None, 10).unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.milestone.id == first.id)
        );
        assert!(
            candidates
                .iter()
                .any(|candidate| candidate.milestone.id == second.id)
        );
        controller.claim_start(&first.id).unwrap().unwrap();
        controller.claim_start(&second.id).unwrap().unwrap();
        assert!(controller.ready_candidates(None, 10).unwrap().is_empty());
    }

    #[test]
    fn milestone_plan_is_deterministic_and_inherits_policy_defaults() {
        let program = Program {
            id: "prog-plan".into(),
            project_id: "project-1".into(),
            goal: "Program goal".into(),
            status: ProgramStatus::Active,
            policy: ProgramPolicy {
                auto_dispatch: true,
                max_attempts: 2,
                retry_backoff_secs: 60,
                dependency_failure: DependencyFailurePolicy::Block,
                token_budget: Some(7_000),
                cost_budget_usd: Some(1.5),
                max_parallel_milestones: 2,
            },
            token_budget: None,
            cost_budget_usd: None,
            reason: None,
            archived: false,
            created_at: "2026-11-11T00:00:00Z".into(),
            updated_at: "2026-11-11T00:00:00Z".into(),
            completed_at: None,
        };
        let milestone = ProgramMilestone {
            id: "ms-plan".into(),
            program_id: program.id.clone(),
            key: "1.2".into(),
            title: "Research milestone".into(),
            objective: "Investigate the API".into(),
            acceptance: vec!["notes are cited".into()],
            deps: Vec::new(),
            priority: 5,
            status: ProgramMilestoneStatus::Ready,
            session_id: None,
            plan_revision: None,
            attempts: 0,
            max_attempts: 3,
            next_attempt_at: None,
            result: None,
            last_error: None,
            profile: "research".into(),
            write_scope: vec!["docs".into()],
            token_budget: None,
            cost_budget_usd: None,
            created_at: "2026-11-11T00:00:00Z".into(),
            updated_at: "2026-11-11T00:00:00Z".into(),
            completed_at: None,
        };
        let plan = milestone_plan(&program, &milestone);
        assert_eq!(plan.departments.len(), 1);
        assert_eq!(plan.departments[0].name, "research");
        assert_eq!(plan.departments[0].manager_title, "Research Manager");
        assert_eq!(plan.departments[0].token_budget, Some(7_000));
        assert_eq!(plan.departments[0].cost_budget_usd, Some(1.5));
        assert_eq!(plan.tasks.len(), 1);
        assert_eq!(plan.tasks[0].id, "T1");
        assert_eq!(plan.tasks[0].profile.as_deref(), Some("research"));
        assert_eq!(plan.tasks[0].priority, 5);
        assert_eq!(plan.tasks[0].write_scope, vec!["docs"]);
        assert_eq!(plan.tasks[0].acceptance, vec!["notes are cited"]);
        assert!(plan.departments[0].objective.contains("Program goal"));
        assert!(plan.departments[0].objective.contains("Milestone 1.2"));
        assert_eq!(
            milestone_session_goal(&program, &milestone),
            "Program goal · 1.2 Research milestone"
        );
    }
}

#[cfg(test)]
mod bundle_tests {
    use super::*;
    use crate::Engine;
    use tempfile::TempDir;
    use vp_protocol::types::{ProgramCreateParams, ProgramMilestoneCreateParams, Project};
    use vp_store::Store;

    fn policy() -> ProgramPolicy {
        ProgramPolicy {
            auto_dispatch: false,
            max_attempts: 2,
            retry_backoff_secs: 60,
            dependency_failure: DependencyFailurePolicy::Block,
            token_budget: None,
            cost_budget_usd: None,
            max_parallel_milestones: 2,
        }
    }

    #[tokio::test]
    async fn session_bundle_exports_and_imports_the_program_ledger_as_an_archive() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project: Project = engine
            .add_project(
                repo.path().to_str().unwrap().to_string(),
                Some("bundle".into()),
            )
            .await
            .unwrap();
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Bundled program".into(),
                policy: Some(policy()),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let milestone = engine
            .create_program_milestone(ProgramMilestoneCreateParams {
                program_id: program.id.clone(),
                key: Some("1".into()),
                title: "Bundle milestone".into(),
                objective: "ship".into(),
                acceptance: vec!["done".into()],
                deps: Vec::new(),
                priority: Some(0),
                max_attempts: Some(2),
                profile: Some("coding".into()),
                write_scope: vec!["src".into()],
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap();
        let session = engine
            .store()
            .create_session(&project.id, "bundle session")
            .unwrap();
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        let controller = engine.programs();
        let claimed = controller.claim_start(&milestone.id).unwrap().unwrap();
        controller
            .attach_session(&milestone.id, &session.id, 1)
            .unwrap();
        engine
            .set_team_plan(&session.id, &milestone_plan(&program, &claimed))
            .await
            .unwrap();

        let bundle = engine.export_session(&session.id).await.unwrap();
        assert_eq!(bundle.programs.len(), 1);
        assert_eq!(bundle.program_milestones.len(), 1);
        assert!(!bundle.program_events.is_empty());
        assert_eq!(bundle.programs[0].id, program.id);

        let imported_dir = TempDir::new().unwrap();
        let imported_store = Store::open(imported_dir.path()).unwrap();
        imported_store.import_session_bundle(&bundle).unwrap();
        let imported = imported_store.get_program(&program.id).unwrap().unwrap();
        assert!(imported.archived);
        assert_eq!(imported.status, ProgramStatus::Paused);
        assert_eq!(
            imported_store
                .list_program_milestones(&program.id)
                .unwrap()
                .len(),
            1
        );
        let replay = ProgramController::new(imported_store)
            .replay(&program.id, false)
            .unwrap();
        assert!(replay.consistent, "{:?}", replay.mismatches);
    }
}

#[cfg(test)]
mod bundle_overwrite_tests {
    use super::*;
    use crate::Engine;
    use tempfile::TempDir;
    use vp_protocol::types::{ProgramCreateParams, ProgramMilestoneCreateParams};
    use vp_store::Store;

    #[tokio::test]
    async fn importing_a_bundle_never_overwrites_a_live_program_ledger() {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some("overwrite".into()),
            )
            .await
            .unwrap();
        let program = engine
            .create_program(ProgramCreateParams {
                project_id: project.id.clone(),
                goal: "Original program".into(),
                policy: Some(ProgramPolicy {
                    auto_dispatch: false,
                    max_attempts: 2,
                    retry_backoff_secs: 60,
                    dependency_failure: DependencyFailurePolicy::Block,
                    token_budget: None,
                    cost_budget_usd: None,
                    max_parallel_milestones: 2,
                }),
                token_budget: None,
                cost_budget_usd: None,
                activate: true,
            })
            .await
            .unwrap();
        let first = engine
            .create_program_milestone(ProgramMilestoneCreateParams {
                program_id: program.id.clone(),
                key: Some("1".into()),
                title: "First".into(),
                objective: "first".into(),
                acceptance: vec!["done".into()],
                deps: Vec::new(),
                priority: Some(0),
                max_attempts: Some(2),
                profile: Some("coding".into()),
                write_scope: Vec::new(),
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap();
        let controller = engine.programs();
        engine
            .store()
            .update_session_status(
                &engine
                    .store()
                    .create_session(&project.id, "first session")
                    .unwrap()
                    .id,
                "idle",
            )
            .unwrap();
        let session = engine
            .store()
            .create_session(&project.id, "first session")
            .unwrap();
        engine
            .store()
            .update_session_status(&session.id, "idle")
            .unwrap();
        controller.claim_start(&first.id).unwrap().unwrap();
        controller
            .attach_session(&first.id, &session.id, 1)
            .unwrap();
        engine
            .set_team_plan(&session.id, &milestone_plan(&program, &first))
            .await
            .unwrap();
        let first_bundle = engine.export_session(&session.id).await.unwrap();

        let imported_dir = TempDir::new().unwrap();
        let imported_store = Store::open(imported_dir.path()).unwrap();
        imported_store.import_session_bundle(&first_bundle).unwrap();
        let mut live = imported_store.get_program(&program.id).unwrap().unwrap();
        live.goal = "live program".into();
        live.archived = false;
        live.status = ProgramStatus::Active;
        live.reason = None;
        imported_store.insert_program_snapshot(&live).unwrap();

        let second = engine
            .create_program_milestone(ProgramMilestoneCreateParams {
                program_id: program.id.clone(),
                key: Some("2".into()),
                title: "Second".into(),
                objective: "second".into(),
                acceptance: vec!["done".into()],
                deps: vec![first.id.clone()],
                priority: Some(0),
                max_attempts: Some(2),
                profile: Some("coding".into()),
                write_scope: Vec::new(),
                token_budget: None,
                cost_budget_usd: None,
            })
            .await
            .unwrap();
        let second_session = engine
            .store()
            .create_session(&project.id, "second session")
            .unwrap();
        engine
            .store()
            .update_session_status(&second_session.id, "idle")
            .unwrap();
        engine
            .store()
            .insert_program_milestone_snapshot(&ProgramMilestone {
                status: ProgramMilestoneStatus::Completed,
                session_id: Some(second_session.id.clone()),
                ..second
            })
            .unwrap();
        let second_bundle = engine.export_session(&second_session.id).await.unwrap();
        imported_store
            .import_session_bundle(&second_bundle)
            .unwrap();

        let restored = imported_store.get_program(&program.id).unwrap().unwrap();
        assert_eq!(restored.goal, "live program");
        assert!(!restored.archived);
        assert_eq!(
            imported_store
                .list_program_milestones(&program.id)
                .unwrap()
                .len(),
            1
        );
    }
}
