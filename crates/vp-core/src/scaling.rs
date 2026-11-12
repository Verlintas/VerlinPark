//! Deterministic organization scaling (ADR-0172).
//!
//! The controller is a pure decision function over durable plan/task state,
//! observed slot pressure, global capacity, and budget headroom. It never
//! creates Managers, rewrites a TeamPlan, spawns a Worker directly, or bypasses
//! admission: it only moves the Worker/Labour slot ceilings that the existing
//! delegate/plan paths already use. A daemon tick evaluates enabled projects and
//! applies a bounded target change; the decision itself is event-backed.
//!
//! Safety properties:
//! - disabled by default, project-scoped, and clamped to the runtime hard caps;
//! - expansion requires ready work, saturated slots, cooldown, demand, and
//!   global capacity, plus optional budget headroom;
//! - scale-down is Labour first, never below active agents, plan demand, or the
//!   policy minimums, and requires a continuous idle window;
//! - Managers stay plan-owned; evidence is recorded but never acted on.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use vp_protocol::scaling::{
    HARD_MAX_LABOUR, HARD_MAX_WORKERS, ScalingAction, ScalingDecision, ScalingPolicy,
    ScalingPolicyResult, ScalingPreviewResult, ScalingProjectStatus, ScalingSessionStatus,
    ScalingStatusResult, ScalingTaskObservation, ScalingTickResult,
};
use vp_protocol::types::{CapacityInfo, Event, Session};
use vp_store::Store;

use crate::Engine;
use crate::tooling::errors::ToolErrorCode;

/// Number of ready-task ids included in deterministic evidence.
const MAX_READY_ORDER_EVIDENCE: usize = 8;
/// Bound for reason strings persisted per decision.
const MAX_REASONS: usize = 32;
/// How many historical decisions are inspected for cooldown/idle carry-over.
const DECISION_LOOKBACK: u32 = 200;

pub(crate) const HARD_WORKERS: usize = HARD_MAX_WORKERS as usize;
pub(crate) const HARD_LABOUR: usize = HARD_MAX_LABOUR as usize;

/// Mutable Worker/Labour slot state shared by one session run and the scaling
/// tick. It owns both the target ceilings and the live reservation counters so
/// an apply and a reserve are serialized by the same mutex.
pub(crate) struct ScalingSlots {
    state: Mutex<SlotState>,
}

#[derive(Debug, Clone, Copy)]
struct SlotState {
    target_workers: usize,
    target_labour: usize,
    active_workers: usize,
    active_labour: usize,
    /// Once a scaling decision manages this run, the legacy Worker auto-expand
    /// guard no longer restores the original cap over the controller's target.
    managed: bool,
    generation: u64,
}

/// Effective targets read back after a compare-and-apply. The target is always
/// clamped to the live reservation counters, so a scale-down never removes an
/// agent that is already running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct AppliedScalingTargets {
    pub workers: usize,
    pub labour: usize,
    pub active_workers: usize,
    pub active_labour: usize,
    pub generation: u64,
}

impl ScalingSlots {
    pub fn new(workers: usize, labour: usize) -> Self {
        Self {
            state: Mutex::new(SlotState {
                target_workers: workers.clamp(1, HARD_WORKERS),
                target_labour: labour.clamp(1, HARD_LABOUR),
                active_workers: 0,
                active_labour: 0,
                managed: false,
                generation: 0,
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, SlotState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn targets(&self) -> (usize, usize) {
        let state = self.lock();
        (state.target_workers, state.target_labour)
    }

    pub(crate) fn active(&self) -> (usize, usize) {
        let state = self.lock();
        (state.active_workers, state.active_labour)
    }

    pub(crate) fn managed(&self) -> bool {
        self.lock().managed
    }

    pub(crate) fn enable_managed(&self) {
        self.lock().managed = true;
    }

    /// Reserve one Worker/Labour slot. Returns the 1-based ordinal within the
    /// role, matching the historical `SessionBudget::reserve` contract.
    pub(crate) fn try_reserve(&self, role: crate::agent::AgentRole) -> Option<usize> {
        let mut state = self.lock();
        match role {
            crate::agent::AgentRole::Worker => {
                if state.active_workers >= state.target_workers {
                    None
                } else {
                    state.active_workers = state.active_workers.saturating_add(1);
                    Some(state.active_workers)
                }
            }
            crate::agent::AgentRole::Labour => {
                if state.active_labour >= state.target_labour {
                    None
                } else {
                    state.active_labour = state.active_labour.saturating_add(1);
                    Some(state.active_labour)
                }
            }
            _ => None,
        }
    }

    pub(crate) fn release(&self, role: crate::agent::AgentRole) {
        let mut state = self.lock();
        match role {
            crate::agent::AgentRole::Worker => {
                state.active_workers = state.active_workers.saturating_sub(1);
            }
            crate::agent::AgentRole::Labour => {
                state.active_labour = state.active_labour.saturating_sub(1);
            }
            _ => {}
        }
    }

    pub(crate) fn available(&self, role: crate::agent::AgentRole) -> usize {
        let state = self.lock();
        match role {
            crate::agent::AgentRole::Worker => {
                state.target_workers.saturating_sub(state.active_workers)
            }
            crate::agent::AgentRole::Labour => {
                state.target_labour.saturating_sub(state.active_labour)
            }
            _ => 0,
        }
    }

    /// Apply a controller target. Targets are clamped to the hard caps and to
    /// the currently active reservations, so a concurrent reserve can never be
    /// removed by a scale-down.
    pub(crate) fn apply(&self, workers: usize, labour: usize) -> AppliedScalingTargets {
        let mut state = self.lock();
        state.target_workers = workers.clamp(1, HARD_WORKERS).max(state.active_workers);
        state.target_labour = labour.clamp(1, HARD_LABOUR).max(state.active_labour);
        state.managed = true;
        state.generation = state.generation.saturating_add(1);
        AppliedScalingTargets {
            workers: state.target_workers,
            labour: state.target_labour,
            active_workers: state.active_workers,
            active_labour: state.active_labour,
            generation: state.generation,
        }
    }

    /// Legacy `expand_workers` compatibility. It only raises a ceiling.
    pub(crate) fn raise_target(&self, role: crate::agent::AgentRole, target: usize) {
        let mut state = self.lock();
        match role {
            crate::agent::AgentRole::Worker => {
                state.target_workers = state.target_workers.max(target.clamp(1, HARD_WORKERS));
            }
            crate::agent::AgentRole::Labour => {
                state.target_labour = state.target_labour.max(target.clamp(1, HARD_LABOUR));
            }
            _ => {}
        }
    }

    /// Restore a legacy cap only while the controller does not own this run.
    pub(crate) fn restore_target_if_unmanaged(&self, role: crate::agent::AgentRole, target: usize) {
        let mut state = self.lock();
        if state.managed {
            return;
        }
        match role {
            crate::agent::AgentRole::Worker => {
                state.target_workers = target.clamp(1, HARD_WORKERS);
            }
            crate::agent::AgentRole::Labour => {
                state.target_labour = target.clamp(1, HARD_LABOUR);
            }
            _ => {}
        }
    }

    /// Reconcile a leaked reservation counter against the durable agent rows
    /// after a run ended. It only lowers counters, never raises them.
    pub(crate) fn reconcile_idle(&self, workers: usize, labour: usize) {
        let mut state = self.lock();
        state.active_workers = state.active_workers.min(workers);
        state.active_labour = state.active_labour.min(labour);
    }
}

/// Daemon-owned session slot registry. Decisions can therefore apply to a
/// session before, during, or after a run without creating a second execution
/// path. The registry is in-memory by design; the durable decision trail is the
/// source for the next tick after a restart.
#[derive(Default)]
pub(crate) struct ScalingCoordinator {
    slots: Mutex<HashMap<String, Arc<ScalingSlots>>>,
}

impl ScalingCoordinator {
    pub(crate) fn slots_for(
        &self,
        session_id: &str,
        base_workers: usize,
        base_labour: usize,
    ) -> Arc<ScalingSlots> {
        let mut map = self
            .slots
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        map.entry(session_id.to_string())
            .or_insert_with(|| Arc::new(ScalingSlots::new(base_workers, base_labour)))
            .clone()
    }

    pub(crate) fn existing(&self, session_id: &str) -> Option<Arc<ScalingSlots>> {
        self.slots
            .lock()
            .ok()
            .and_then(|map| map.get(session_id).cloned())
    }

    pub(crate) fn remove(&self, session_id: &str) {
        if let Ok(mut map) = self.slots.lock() {
            map.remove(session_id);
        }
    }

    pub(crate) fn retain(&self, known: &HashSet<String>) {
        if let Ok(mut map) = self.slots.lock() {
            map.retain(|session_id, _| known.contains(session_id));
        }
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.slots.lock().map(|map| map.len()).unwrap_or(0)
    }
}

/// The complete input to the pure controller. Times are explicit so tests can
/// evaluate cooldown/idle behavior deterministically.
#[derive(Debug, Clone)]
pub struct ScalingObservation {
    pub session_id: String,
    pub project_id: String,
    pub session_status: String,
    pub plan_revision: u64,
    pub tasks: Vec<ScalingTaskObservation>,
    pub current_workers: usize,
    pub current_labour: usize,
    pub active_workers: usize,
    pub active_labour: usize,
    pub global_worker_available: usize,
    pub global_labour_available: usize,
    pub budget_headroom_ratio: Option<f64>,
    pub last_action_at: Option<OffsetDateTime>,
    pub last_plan_revision: Option<u64>,
    pub idle_since_workers: Option<OffsetDateTime>,
    pub idle_since_labour: Option<OffsetDateTime>,
    pub now: OffsetDateTime,
    pub hard_worker_cap: usize,
    pub hard_labour_cap: usize,
}

impl ScalingObservation {
    pub fn new(session_id: impl Into<String>, project_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            project_id: project_id.into(),
            session_status: "idle".to_string(),
            plan_revision: 0,
            tasks: Vec::new(),
            current_workers: 1,
            current_labour: 1,
            active_workers: 0,
            active_labour: 0,
            global_worker_available: HARD_WORKERS,
            global_labour_available: HARD_LABOUR,
            budget_headroom_ratio: None,
            last_action_at: None,
            last_plan_revision: None,
            idle_since_workers: None,
            idle_since_labour: None,
            now: OffsetDateTime::now_utc(),
            hard_worker_cap: HARD_WORKERS,
            hard_labour_cap: HARD_LABOUR,
        }
    }
}

/// Capture the exact deterministic inputs for `orchestration.replay`.
/// Times are RFC3339 strings; task observations and policy are already public
/// wire types. Nothing here reads prompt text or provider output.
pub(crate) fn replay_inputs(
    policy: &ScalingPolicy,
    observation: &ScalingObservation,
) -> Option<serde_json::Value> {
    let inputs = vp_protocol::scaling::ScalingReplayInputs {
        policy: policy.clone(),
        session_status: observation.session_status.clone(),
        plan_revision: observation.plan_revision,
        tasks: observation.tasks.clone(),
        current_workers: observation.current_workers as u32,
        current_labour: observation.current_labour as u32,
        active_workers: observation.active_workers as u32,
        active_labour: observation.active_labour as u32,
        global_worker_available: observation.global_worker_available as u32,
        global_labour_available: observation.global_labour_available as u32,
        budget_headroom_ratio: observation.budget_headroom_ratio,
        last_action_at: observation.last_action_at.map(format_time),
        last_plan_revision: observation.last_plan_revision,
        idle_since_workers: observation.idle_since_workers.map(format_time),
        idle_since_labour: observation.idle_since_labour.map(format_time),
        now: format_time(observation.now),
        hard_worker_cap: observation.hard_worker_cap as u32,
        hard_labour_cap: observation.hard_labour_cap as u32,
    };
    serde_json::to_value(inputs).ok()
}

pub(crate) fn observation_from_replay_inputs(
    inputs: &vp_protocol::scaling::ScalingReplayInputs,
) -> Option<ScalingObservation> {
    fn parse(value: &str) -> Option<OffsetDateTime> {
        OffsetDateTime::parse(value, &Rfc3339).ok()
    }
    let now = parse(&inputs.now)?;
    Some(ScalingObservation {
        session_id: String::new(),
        project_id: String::new(),
        session_status: inputs.session_status.clone(),
        plan_revision: inputs.plan_revision,
        tasks: inputs.tasks.clone(),
        current_workers: inputs.current_workers as usize,
        current_labour: inputs.current_labour as usize,
        active_workers: inputs.active_workers as usize,
        active_labour: inputs.active_labour as usize,
        global_worker_available: inputs.global_worker_available as usize,
        global_labour_available: inputs.global_labour_available as usize,
        budget_headroom_ratio: inputs.budget_headroom_ratio,
        last_action_at: inputs.last_action_at.as_deref().and_then(parse),
        last_plan_revision: inputs.last_plan_revision,
        idle_since_workers: inputs.idle_since_workers.as_deref().and_then(parse),
        idle_since_labour: inputs.idle_since_labour.as_deref().and_then(parse),
        now,
        hard_worker_cap: inputs.hard_worker_cap as usize,
        hard_labour_cap: inputs.hard_labour_cap as usize,
    })
}

#[derive(Debug, Clone, Default)]
struct PlanDemand {
    ready_tasks: usize,
    running_tasks: usize,
    completed_tasks: usize,
    demand_workers: usize,
    oldest_ready_age_secs: u64,
    ready_order: Vec<String>,
    retry_ready_tasks: usize,
}

/// Pure deterministic controller. It never performs I/O and its output depends
/// only on the observation and policy.
pub fn evaluate(policy: &ScalingPolicy, observation: &ScalingObservation) -> ScalingDecision {
    let policy = policy.clone().normalized();
    let hard_workers = observation.hard_worker_cap.clamp(1, HARD_WORKERS);
    let hard_labour = observation.hard_labour_cap.clamp(1, HARD_LABOUR);
    let current_workers = observation.current_workers.clamp(1, hard_workers);
    let current_labour = observation.current_labour.clamp(1, hard_labour);
    let active_workers = observation.active_workers.min(HARD_WORKERS);
    let active_labour = observation.active_labour.min(HARD_LABOUR);
    let demand = analyze_plan(&observation.tasks, observation.now, hard_workers);

    let mut decision = ScalingDecision {
        decision_id: String::new(),
        decision_key: String::new(),
        session_id: observation.session_id.clone(),
        project_id: observation.project_id.clone(),
        plan_revision: observation.plan_revision,
        action: ScalingAction::Hold,
        current_workers: current_workers as u32,
        current_labour: current_labour as u32,
        target_workers: current_workers as u32,
        target_labour: current_labour as u32,
        active_workers: active_workers as u32,
        active_labour: active_labour as u32,
        ready_tasks: demand.ready_tasks as u32,
        running_tasks: demand.running_tasks as u32,
        inputs_hash: String::new(),
        reasons: Vec::new(),
        manager_recommended: false,
        manager_evidence: None,
        applied: false,
        idle_since_workers: None,
        idle_since_labour: None,
        replay_inputs: None,
        created_at: format_time(observation.now),
    };
    let mut reasons = Vec::new();

    // A new plan revision invalidates both the expansion cooldown and the
    // idle window carried by the previous revision.
    let revision_changed = observation
        .last_plan_revision
        .is_some_and(|revision| revision != observation.plan_revision);
    let carried_idle_workers = (!revision_changed)
        .then_some(observation.idle_since_workers)
        .flatten();
    let carried_idle_labour = (!revision_changed)
        .then_some(observation.idle_since_labour)
        .flatten();
    let idle_since_workers = if current_workers > active_workers {
        carried_idle_workers.or(Some(observation.now))
    } else {
        None
    };
    let idle_since_labour = if current_labour > active_labour {
        carried_idle_labour.or(Some(observation.now))
    } else {
        None
    };
    decision.idle_since_workers = idle_since_workers.map(format_time);
    decision.idle_since_labour = idle_since_labour.map(format_time);

    if !policy.enabled {
        reasons.push("policy_disabled".to_string());
        return finish(decision, &policy, observation, &demand, reasons);
    }
    if observation.plan_revision == 0 || observation.tasks.is_empty() {
        reasons.push("no_plan".to_string());
        return finish(decision, &policy, observation, &demand, reasons);
    }

    reasons.push(format!("session_status:{}", observation.session_status));
    reasons.push(format!("ready_tasks:{}", demand.ready_tasks));
    reasons.push(format!("running_tasks:{}", demand.running_tasks));
    if demand.completed_tasks > 0 {
        reasons.push(format!("completed_tasks:{}", demand.completed_tasks));
    }
    if demand.retry_ready_tasks > 0 {
        reasons.push(format!("retry_ready_tasks:{}", demand.retry_ready_tasks));
    }
    if !demand.ready_order.is_empty() {
        reasons.push(format!(
            "ready_order:{}",
            demand
                .ready_order
                .iter()
                .take(MAX_READY_ORDER_EVIDENCE)
                .cloned()
                .collect::<Vec<_>>()
                .join(">")
        ));
    }
    if demand.oldest_ready_age_secs > 0 {
        reasons.push(format!(
            "oldest_ready_age_secs:{}",
            demand.oldest_ready_age_secs
        ));
    }

    let ceiling_workers = (policy.max_workers as usize).min(hard_workers);
    let ceiling_labour = (policy.max_labour as usize).min(hard_labour);
    let demand_workers = demand.demand_workers.min(ceiling_workers);
    let demand_labour = demand_workers.min(ceiling_labour);

    let threshold_ok = demand.ready_tasks >= policy.expand_ready_threshold as usize;
    if !threshold_ok && demand.ready_tasks > 0 {
        reasons.push(format!(
            "below_expand_threshold:{}/{}",
            demand.ready_tasks, policy.expand_ready_threshold
        ));
    }

    let running_session = observation.session_status == "running";
    let saturated = if running_session {
        if active_workers < current_workers {
            reasons.push("idle_slots_available".to_string());
            false
        } else {
            true
        }
    } else {
        true
    };

    let critical_delay_ok =
        if running_session || demand.oldest_ready_age_secs >= policy.expand_critical_delay_secs {
            true
        } else {
            reasons.push(format!(
                "critical_delay_wait:{}/{}s",
                demand.oldest_ready_age_secs, policy.expand_critical_delay_secs
            ));
            false
        };

    let cooldown_ok = match observation.last_action_at {
        Some(last) if observation.last_plan_revision == Some(observation.plan_revision) => {
            let elapsed = elapsed_secs(observation.now, last);
            if elapsed < policy.cooldown_secs {
                reasons.push(format!(
                    "cooldown_active:{}/{}s",
                    elapsed, policy.cooldown_secs
                ));
                false
            } else {
                true
            }
        }
        _ => {
            if observation
                .last_plan_revision
                .is_some_and(|revision| revision != observation.plan_revision)
            {
                reasons.push("plan_revision_changed_cooldown_reset".to_string());
            }
            true
        }
    };

    let budget_ok = budget_gate(&policy, observation, &mut reasons);
    let global_workers_ok = observation.global_worker_available > 0;
    let global_labour_ok = observation.global_labour_available > 0;

    let expansion_needed_workers = demand_workers > current_workers;
    let can_expand_workers = expansion_needed_workers
        && threshold_ok
        && saturated
        && critical_delay_ok
        && cooldown_ok
        && budget_ok
        && global_workers_ok;
    let mut target_workers = current_workers;
    let mut expanded_workers = false;
    if expansion_needed_workers && !can_expand_workers && !global_workers_ok {
        reasons.push("global_worker_capacity_blocked".to_string());
    }
    if can_expand_workers {
        let step = (demand_workers - current_workers)
            .min(policy.max_step as usize)
            .min(observation.global_worker_available.max(1));
        let step = step.max(1);
        target_workers = current_workers.saturating_add(step).min(ceiling_workers);
        expanded_workers = target_workers > current_workers;
        if expanded_workers {
            reasons.push(format!(
                "expand_workers:{}->{}",
                current_workers, target_workers
            ));
            if target_workers < demand_workers {
                reasons.push("max_step_applied".to_string());
            }
        }
    } else if expansion_needed_workers && !threshold_ok {
        reasons.push("plan_demand_waiting_for_threshold".to_string());
    } else if expansion_needed_workers && !saturated {
        reasons.push("plan_demand_waiting_for_slots".to_string());
    }

    let desired_labour = (policy.min_labour as usize)
        .max(demand_labour)
        .min(ceiling_labour);
    let expansion_needed_labour = desired_labour > current_labour;
    let labour_saturated = !running_session || active_labour >= current_labour;
    if expansion_needed_labour && !labour_saturated {
        reasons.push("labour_idle_slots_available".to_string());
    }
    let worker_pressure_resolved = !expansion_needed_workers || expanded_workers;
    let can_expand_labour = expansion_needed_labour
        && worker_pressure_resolved
        && labour_saturated
        && demand.ready_tasks > 0
        && threshold_ok
        && critical_delay_ok
        && cooldown_ok
        && budget_ok
        && global_labour_ok
        && (saturated || expanded_workers);
    let mut target_labour = current_labour;
    let mut expanded_labour = false;
    if expansion_needed_labour && !can_expand_labour && !global_labour_ok {
        reasons.push("global_labour_capacity_blocked".to_string());
    }
    if can_expand_labour {
        let step = (desired_labour - current_labour)
            .min(policy.max_step as usize)
            .min(observation.global_labour_available.max(1))
            .max(1);
        target_labour = current_labour.saturating_add(step).min(ceiling_labour);
        expanded_labour = target_labour > current_labour;
        if expanded_labour {
            reasons.push(format!(
                "expand_labour:{}->{}",
                current_labour, target_labour
            ));
            if target_labour < desired_labour {
                reasons.push("max_step_applied".to_string());
            }
        }
    } else if expansion_needed_labour && !worker_pressure_resolved {
        reasons.push("labour_waiting_for_worker_pressure".to_string());
    }

    if expanded_workers || expanded_labour {
        let action = match (expanded_workers, expanded_labour) {
            (true, true) => ScalingAction::ExpandWorkersAndLabour,
            (true, false) => ScalingAction::ExpandWorkers,
            (false, true) => ScalingAction::ExpandLabour,
            (false, false) => ScalingAction::Hold,
        };
        decision.action = action;
        decision.target_workers = target_workers as u32;
        decision.target_labour = target_labour as u32;
        return finish(decision, &policy, observation, &demand, reasons);
    }

    // Scale-down is evaluated only when no expansion happened. Labour is always
    // reduced before Workers so a Worker never loses its possible helper first.
    let min_workers = (policy.min_workers as usize)
        .max(active_workers)
        .max(demand_workers)
        .min(ceiling_workers);
    let min_labour = (policy.min_labour as usize)
        .max(active_labour)
        .max(demand_labour)
        .min(ceiling_labour);
    let idle_workers_ok = current_workers > min_workers
        && idle_since_workers.is_some_and(|since| {
            elapsed_secs(observation.now, since) >= policy.scale_down_idle_secs
        });
    let idle_labour_ok = current_labour > min_labour
        && idle_since_labour.is_some_and(|since| {
            elapsed_secs(observation.now, since) >= policy.scale_down_idle_secs
        });

    if idle_labour_ok {
        let step = (current_labour - min_labour)
            .min(policy.max_step as usize)
            .max(1);
        decision.action = ScalingAction::ScaleDownLabour;
        decision.target_labour = current_labour.saturating_sub(step) as u32;
        reasons.push(format!(
            "scale_down_labour:{}->{}",
            current_labour, decision.target_labour
        ));
        if idle_workers_ok {
            reasons.push("scale_down_workers_pending_next_tick".to_string());
        }
        return finish(decision, &policy, observation, &demand, reasons);
    }
    if idle_workers_ok {
        let step = (current_workers - min_workers)
            .min(policy.max_step as usize)
            .max(1);
        decision.action = ScalingAction::ScaleDownWorkers;
        decision.target_workers = current_workers.saturating_sub(step) as u32;
        reasons.push(format!(
            "scale_down_workers:{}->{}",
            current_workers, decision.target_workers
        ));
        return finish(decision, &policy, observation, &demand, reasons);
    }

    if current_labour > min_labour {
        reasons.push(format!(
            "scale_down_labour_waiting:{}s",
            idle_since_labour
                .map(|since| elapsed_secs(observation.now, since))
                .unwrap_or(0)
        ));
    }
    if current_workers > min_workers {
        reasons.push(format!(
            "scale_down_workers_waiting:{}s",
            idle_since_workers
                .map(|since| elapsed_secs(observation.now, since))
                .unwrap_or(0)
        ));
    }
    finish(decision, &policy, observation, &demand, reasons)
}

fn finish(
    mut decision: ScalingDecision,
    policy: &ScalingPolicy,
    observation: &ScalingObservation,
    demand: &PlanDemand,
    mut reasons: Vec<String>,
) -> ScalingDecision {
    let ceiling_workers = (policy.max_workers as usize).min(observation.hard_worker_cap.max(1));
    let requested_workers = demand.ready_tasks.saturating_add(demand.running_tasks);
    let unserved = requested_workers.saturating_sub(ceiling_workers);
    if unserved > 0 {
        decision.manager_recommended = true;
        decision.manager_evidence = Some(format!(
            "{unserved} queued/dependency-ready task(s) exceed the scalable Worker ceiling ({ceiling_workers}); \
             a plan revision with an additional department/Manager is required. Managers are never \
             created automatically."
        ));
        reasons.push("manager_recommended".to_string());
    }
    decision.reasons = reasons.into_iter().take(MAX_REASONS).collect();
    decision.inputs_hash = observation_hash(policy, observation, demand);
    decision.decision_key = decision.inputs_hash.clone();
    decision
}

fn budget_gate(
    policy: &ScalingPolicy,
    observation: &ScalingObservation,
    reasons: &mut Vec<String>,
) -> bool {
    let Some(threshold) = policy.budget_headroom_ratio else {
        reasons.push("budget_check_disabled".to_string());
        return true;
    };
    match observation.budget_headroom_ratio {
        Some(ratio) if ratio >= threshold => {
            reasons.push(format!("budget_headroom_ok:{ratio:.3}"));
            true
        }
        Some(ratio) => {
            reasons.push(format!(
                "budget_headroom_below_threshold:{ratio:.3}<{threshold:.3}"
            ));
            false
        }
        None => {
            reasons.push("budget_headroom_unknown".to_string());
            false
        }
    }
}

fn analyze_plan(
    tasks: &[ScalingTaskObservation],
    now: OffsetDateTime,
    hard_worker_cap: usize,
) -> PlanDemand {
    let statuses = tasks
        .iter()
        .map(|task| (task.task_id.clone(), task.status.as_str()))
        .collect::<HashMap<_, _>>();
    let completed = statuses
        .iter()
        .filter(|(_, status)| matches!(**status, "completed" | "skipped"))
        .map(|(task_id, _)| task_id.clone())
        .collect::<HashSet<_>>();
    let running = tasks
        .iter()
        .filter(|task| task.status == "running")
        .cloned()
        .collect::<Vec<_>>();
    let mut ready = Vec::new();
    let mut retry_ready = 0usize;
    for task in tasks {
        if !matches!(
            task.status.as_str(),
            "pending" | "interrupted" | "failed" | "blocked" | "cancelled"
        ) {
            continue;
        }
        if !task.deps.iter().all(|dep| completed.contains(dep)) {
            continue;
        }
        if let Some(ready_since) = task.ready_since.as_deref()
            && let Some(at) = parse_time(ready_since)
            && at > now
        {
            continue;
        }
        if matches!(task.status.as_str(), "failed" | "blocked" | "cancelled") {
            retry_ready = retry_ready.saturating_add(1);
        }
        ready.push(task.clone());
    }
    ready.sort_by(|left, right| {
        right
            .priority
            .cmp(&left.priority)
            .then_with(|| left.department.cmp(&right.department))
            .then_with(|| left.task_id.cmp(&right.task_id))
    });
    let ready_order = ready
        .iter()
        .map(|task| task.task_id.clone())
        .collect::<Vec<_>>();
    let oldest_ready_age_secs = ready
        .iter()
        .filter_map(|task| task.ready_since.as_deref().and_then(parse_time))
        .map(|at| elapsed_secs(now, at))
        .max()
        .unwrap_or(0);

    let mut selected_scopes = running
        .iter()
        .map(|task| task.write_scope.clone())
        .collect::<Vec<_>>();
    let mut barrier = selected_scopes.iter().any(Vec::is_empty);
    let mut demand = running.len().min(hard_worker_cap);
    for task in ready {
        if demand >= hard_worker_cap {
            break;
        }
        if barrier {
            break;
        }
        let scope = task.write_scope.clone();
        if scope.is_empty() {
            if !selected_scopes.is_empty() {
                continue;
            }
            selected_scopes.push(scope);
            demand = demand.saturating_add(1);
            barrier = true;
            continue;
        }
        let overlaps = selected_scopes
            .iter()
            .any(|existing| existing.is_empty() || scopes_overlap(&scope, existing));
        if overlaps {
            continue;
        }
        selected_scopes.push(scope);
        demand = demand.saturating_add(1);
    }
    PlanDemand {
        ready_tasks: ready_order.len(),
        running_tasks: running.len(),
        completed_tasks: completed.len(),
        demand_workers: demand.min(hard_worker_cap),
        oldest_ready_age_secs,
        ready_order,
        retry_ready_tasks: retry_ready,
    }
}

fn scopes_overlap(left: &[String], right: &[String]) -> bool {
    crate::agent::scopes_overlap(left, right)
}

fn observation_hash(
    policy: &ScalingPolicy,
    observation: &ScalingObservation,
    demand: &PlanDemand,
) -> String {
    let mut tasks = observation.tasks.clone();
    tasks.sort_by(|left, right| left.task_id.cmp(&right.task_id));
    let mut inputs = BTreeMap::<&str, Value>::new();
    inputs.insert(
        "policy",
        serde_json::to_value(policy).unwrap_or(Value::Null),
    );
    inputs.insert("session_id", json!(observation.session_id));
    inputs.insert("project_id", json!(observation.project_id));
    inputs.insert("session_status", json!(observation.session_status));
    inputs.insert("plan_revision", json!(observation.plan_revision));
    inputs.insert("tasks", serde_json::to_value(tasks).unwrap_or(Value::Null));
    inputs.insert("current_workers", json!(observation.current_workers));
    inputs.insert("current_labour", json!(observation.current_labour));
    inputs.insert("active_workers", json!(observation.active_workers));
    inputs.insert("active_labour", json!(observation.active_labour));
    inputs.insert(
        "global_worker_available",
        json!(observation.global_worker_available),
    );
    inputs.insert(
        "global_labour_available",
        json!(observation.global_labour_available),
    );
    inputs.insert(
        "budget_headroom_ratio",
        observation
            .budget_headroom_ratio
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    inputs.insert(
        "last_action_at",
        observation
            .last_action_at
            .map(format_time)
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    inputs.insert(
        "last_plan_revision",
        observation
            .last_plan_revision
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    inputs.insert(
        "idle_since_workers",
        observation
            .idle_since_workers
            .map(format_time)
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    inputs.insert(
        "idle_since_labour",
        observation
            .idle_since_labour
            .map(format_time)
            .map(Value::from)
            .unwrap_or(Value::Null),
    );
    inputs.insert("now_unix", json!(observation.now.unix_timestamp()));
    inputs.insert("hard_worker_cap", json!(observation.hard_worker_cap));
    inputs.insert("hard_labour_cap", json!(observation.hard_labour_cap));
    inputs.insert("demand_workers", json!(demand.demand_workers));
    inputs.insert("demand_ready", json!(demand.ready_tasks));
    let bytes = serde_json::to_vec(&inputs).unwrap_or_default();
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

pub fn decisions_from_events(events: &[Event]) -> Vec<ScalingDecision> {
    events
        .iter()
        .filter(|event| event.kind == "scaling.decision")
        .filter_map(|event| {
            let mut decision =
                serde_json::from_value::<ScalingDecision>(event.payload.clone()).ok()?;
            decision.session_id = event.session_id.clone();
            if decision.created_at.is_empty() {
                decision.created_at = event.created_at.clone();
            }
            Some(decision)
        })
        .collect()
}

pub fn decisions_match(left: &[ScalingDecision], right: &[ScalingDecision]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let left = left
        .iter()
        .filter_map(|decision| {
            Some((
                decision.decision_id.clone(),
                serde_json::to_value(decision).ok()?,
            ))
        })
        .collect::<BTreeMap<_, _>>();
    let right = right
        .iter()
        .filter_map(|decision| {
            Some((
                decision.decision_id.clone(),
                serde_json::to_value(decision).ok()?,
            ))
        })
        .collect::<BTreeMap<_, _>>();
    left == right
}

fn format_time(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

fn parse_time(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value, &Rfc3339).ok()
}

fn elapsed_secs(now: OffsetDateTime, then: OffsetDateTime) -> u64 {
    let elapsed = (now - then).whole_seconds();
    elapsed.max(0) as u64
}

/// Strictly evaluate a policy update. The RPC and CLI surface this error so an
/// invalid policy is never silently normalized into different behavior.
pub fn validate_policy(policy: &ScalingPolicy) -> Result<ScalingPolicy> {
    policy.validate().map_err(|message| {
        anyhow::anyhow!(
            "{}: {message}",
            ToolErrorCode::SCALING_POLICY_INVALID.as_str()
        )
    })?;
    Ok(policy.clone().normalized())
}

fn store_error(context: &str, error: impl std::fmt::Display) -> anyhow::Error {
    anyhow::anyhow!(
        "{}: {context}: {error}",
        ToolErrorCode::SCALING_STATE_CONFLICT.as_str()
    )
}

/// Read the durable inputs for one session. This function performs only store
/// queries; the pure controller is called separately.
async fn observe_session(
    engine: &Engine,
    session: &Session,
    _policy: &ScalingPolicy,
    capacity: &CapacityInfo,
    base_workers: usize,
    base_labour: usize,
) -> Result<ScalingObservation> {
    let store = engine.store().clone();
    let session = session.clone();
    let session_for_query = session.clone();
    let data = tokio::task::spawn_blocking(move || {
        let plan_present = store.get_team_plan(&session_for_query.id)?.is_some();
        if !plan_present {
            return Ok::<_, anyhow::Error>(None);
        }
        let plan_revision = u64::from(store.plan_revision_count(&session_for_query.id)?);
        let mut rows = store.list_plan_tasks(None, Some(&session_for_query.id))?;
        let latest_seq = rows.iter().map(|row| row.plan_seq).max().unwrap_or(0);
        rows.retain(|row| row.plan_seq == latest_seq);
        let tasks = rows
            .into_iter()
            .map(|row| ScalingTaskObservation {
                task_id: row.task_id,
                department: row.department,
                priority: row.priority,
                status: row.status,
                deps: row.deps,
                write_scope: row.write_scope,
                ready_since: row.next_attempt_at.or(Some(row.enqueued_at)),
            })
            .collect::<Vec<_>>();
        let decisions = store.list_scaling_decisions(&session_for_query.id, DECISION_LOOKBACK)?;
        let last_action = decisions
            .iter()
            .find(|decision| {
                decision.action != ScalingAction::Hold && decision.plan_revision == plan_revision
            })
            .cloned();
        let latest = decisions.first().cloned();
        let budget_headroom_ratio = budget_headroom(&store, &session_for_query)?;
        Ok(Some(ObservedSession {
            plan_revision,
            tasks,
            latest,
            last_action,
            budget_headroom_ratio,
        }))
    })
    .await?
    .map_err(|error| store_error("reading scaling observation", error))?;
    let Some(data) = data else {
        let slots =
            engine.scaling_slots(&session.id, &session.project_id, base_workers, base_labour);
        let (current_workers, current_labour) = slots.targets();
        let (active_workers, active_labour) = slots.active();
        return Ok(ScalingObservation {
            session_id: session.id,
            project_id: session.project_id,
            session_status: session.status,
            plan_revision: 0,
            tasks: Vec::new(),
            current_workers,
            current_labour,
            active_workers,
            active_labour,
            global_worker_available: capacity.worker_available,
            global_labour_available: capacity.labour_available,
            budget_headroom_ratio: None,
            ..ScalingObservation::new(String::new(), String::new())
        });
    };

    let slots = engine.scaling_slots(&session.id, &session.project_id, base_workers, base_labour);
    let (current_workers, current_labour) = slots.targets();
    let (agent_active_workers, agent_active_labour) = tokio::task::spawn_blocking({
        let store = engine.store().clone();
        let id = session.id.clone();
        move || count_running_agents(&store, &id)
    })
    .await?
    .map_err(|error| store_error("counting running agents", error))?;
    if session.status != "running" {
        // A leaked reservation counter must not keep an idle session from
        // scaling down. This only lowers counters and never interrupts work.
        slots.reconcile_idle(agent_active_workers, agent_active_labour);
    }
    let (slot_active_workers, slot_active_labour) = slots.active();
    let active_workers = slot_active_workers.max(agent_active_workers);
    let active_labour = slot_active_labour.max(agent_active_labour);

    let latest_matches_plan = data
        .latest
        .as_ref()
        .is_some_and(|decision| decision.plan_revision == data.plan_revision);
    let idle_since_workers = latest_matches_plan
        .then(|| {
            data.latest
                .as_ref()
                .filter(|decision| decision.target_workers as usize == current_workers)
                .and_then(|decision| decision.idle_since_workers.as_deref())
                .and_then(parse_time)
        })
        .flatten();
    let idle_since_labour = latest_matches_plan
        .then(|| {
            data.latest
                .as_ref()
                .filter(|decision| decision.target_labour as usize == current_labour)
                .and_then(|decision| decision.idle_since_labour.as_deref())
                .and_then(parse_time)
        })
        .flatten();
    let last_action_at = data
        .last_action
        .as_ref()
        .and_then(|decision| parse_time(&decision.created_at));
    let last_plan_revision = data
        .last_action
        .as_ref()
        .map(|decision| decision.plan_revision);

    Ok(ScalingObservation {
        session_id: session.id,
        project_id: session.project_id,
        session_status: session.status,
        plan_revision: data.plan_revision,
        tasks: data.tasks,
        current_workers,
        current_labour,
        active_workers,
        active_labour,
        global_worker_available: capacity.worker_available,
        global_labour_available: capacity.labour_available,
        budget_headroom_ratio: data.budget_headroom_ratio,
        last_action_at,
        last_plan_revision,
        idle_since_workers,
        idle_since_labour,
        now: OffsetDateTime::now_utc(),
        hard_worker_cap: HARD_WORKERS,
        hard_labour_cap: HARD_LABOUR,
    })
}

struct ObservedSession {
    plan_revision: u64,
    tasks: Vec<ScalingTaskObservation>,
    latest: Option<ScalingDecision>,
    last_action: Option<ScalingDecision>,
    budget_headroom_ratio: Option<f64>,
}

fn budget_headroom(store: &Store, session: &Session) -> Result<Option<f64>> {
    if session.unlimited {
        return Ok(None);
    }
    let project = store.get_project_budget(&session.project_id)?;
    let (project_tokens, project_cost) = store.project_usage(&session.project_id)?;
    let mut ratios = Vec::new();
    if let Some(limit) = project.token_budget
        && limit > 0
    {
        ratios.push(limit.saturating_sub(project_tokens).min(limit) as f64 / limit as f64);
    }
    if let Some(limit) = project.cost_budget_usd
        && limit > 0.0
    {
        ratios.push(((limit - project_cost) / limit).clamp(0.0, 1.0));
    }
    if ratios.is_empty() {
        if let Some(limit) = session.token_budget
            && limit > 0
        {
            let spent = session.tokens_in.saturating_add(session.tokens_out);
            ratios.push(limit.saturating_sub(spent).min(limit) as f64 / limit as f64);
        }
        if let Some(limit) = session.cost_budget_usd
            && limit > 0.0
        {
            ratios.push(((limit - session.cost_spent_usd) / limit).clamp(0.0, 1.0));
        }
    }
    if ratios.is_empty() {
        return Ok(None);
    }
    Ok(ratios.into_iter().reduce(f64::min))
}

fn count_running_agents(store: &Store, session_id: &str) -> Result<(usize, usize)> {
    let agents = store.list_agents(session_id)?;
    let workers = agents
        .iter()
        .filter(|agent| agent.role == "worker" && agent.status == "running")
        .count();
    let labour = agents
        .iter()
        .filter(|agent| agent.role == "labour" && agent.status == "running")
        .count();
    Ok((workers, labour))
}

/// Preview one session without persisting or applying anything.
pub(crate) async fn preview(
    engine: &Engine,
    session_id: &str,
    capacity: &CapacityInfo,
    base_workers: usize,
    base_labour: usize,
) -> Result<ScalingPreviewResult> {
    let session = engine
        .store()
        .get_session(session_id)?
        .ok_or_else(|| anyhow::anyhow!("session not found: {session_id}"))?;
    let policy = engine.store().get_scaling_policy(&session.project_id)?;
    let observation = observe_session(
        engine,
        &session,
        &policy,
        capacity,
        base_workers,
        base_labour,
    )
    .await?;
    let mut decision = evaluate(&policy, &observation);
    if session.status == "imported" {
        decision.action = ScalingAction::Hold;
        decision.target_workers = decision.current_workers;
        decision.target_labour = decision.current_labour;
        decision.reasons.push("imported_read_only".to_string());
    }
    Ok(ScalingPreviewResult {
        session_id: session.id,
        project_id: session.project_id,
        policy,
        decision,
    })
}

/// Build status without mutating any state. Session target ceilings come from
/// the durable decision trail so status is available immediately after restart.
pub(crate) fn status(
    engine: &Engine,
    project_id: Option<&str>,
    session_id: Option<&str>,
    limit: Option<u32>,
    capacity: &CapacityInfo,
    base_workers: usize,
    base_labour: usize,
) -> Result<ScalingStatusResult> {
    let store = engine.store().clone();
    let project_id = project_id.map(ToOwned::to_owned);
    let session_id = session_id.map(ToOwned::to_owned);
    let limit = limit.unwrap_or(100).clamp(1, 1_000);
    let mut projects = store.list_projects()?;
    projects.retain(|project| project_id.as_deref().is_none_or(|id| id == project.id));
    let mut result = ScalingStatusResult {
        global_worker_cap: capacity.worker_limit,
        global_labour_cap: capacity.labour_limit,
        global_worker_available: capacity.worker_available,
        global_labour_available: capacity.labour_available,
        projects: Vec::new(),
    };
    for project in projects {
        let policy = store.get_scaling_policy(&project.id)?;
        let mut sessions = store.list_sessions(Some(&project.id))?;
        sessions.retain(|session| {
            session_id
                .as_deref()
                .is_none_or(|wanted| wanted == session.id)
        });
        sessions.truncate(limit as usize);
        let mut statuses = Vec::new();
        for session in sessions {
            let plan_present = store.get_team_plan(&session.id)?.is_some();
            let mut rows = store.list_plan_tasks(None, Some(&session.id))?;
            let latest_seq = rows.iter().map(|row| row.plan_seq).max().unwrap_or(0);
            rows.retain(|row| row.plan_seq == latest_seq);
            let plan_tasks = rows.len();
            let observations = rows
                .iter()
                .map(|row| ScalingTaskObservation {
                    task_id: row.task_id.clone(),
                    department: row.department.clone(),
                    priority: row.priority,
                    status: row.status.clone(),
                    deps: row.deps.clone(),
                    write_scope: row.write_scope.clone(),
                    ready_since: row
                        .next_attempt_at
                        .clone()
                        .or(Some(row.enqueued_at.clone())),
                })
                .collect::<Vec<_>>();
            let demand = analyze_plan(&observations, OffsetDateTime::now_utc(), HARD_WORKERS);
            let ready_tasks = demand.ready_tasks;
            let running_tasks = demand.running_tasks;
            let completed_tasks = demand.completed_tasks;
            let decisions = store.list_scaling_decisions(&session.id, 1)?;
            let latest = decisions.into_iter().next();
            let plan_revision = u64::from(store.plan_revision_count(&session.id)?);
            let (mut current_workers, mut current_labour) = (
                base_workers.clamp(1, HARD_WORKERS),
                base_labour.clamp(1, HARD_LABOUR),
            );
            if policy.enabled {
                current_workers =
                    current_workers.clamp(policy.min_workers as usize, policy.max_workers as usize);
                current_labour =
                    current_labour.clamp(policy.min_labour as usize, policy.max_labour as usize);
            }
            if policy.enabled
                && let Some(latest) = &latest
            {
                current_workers = latest.target_workers as usize;
                current_labour = latest.target_labour as usize;
            }
            let (mut active_workers, mut active_labour) =
                count_running_agents(&store, &session.id)?;
            if let Some(slots) = engine.scaling_slots_existing(&session.id) {
                let (live_workers, live_labour) = slots.targets();
                let (live_active_workers, live_active_labour) = slots.active();
                current_workers = live_workers;
                current_labour = live_labour;
                active_workers = active_workers.max(live_active_workers);
                active_labour = active_labour.max(live_active_labour);
            }
            let idle_since_workers = latest
                .as_ref()
                .and_then(|decision| decision.idle_since_workers.clone());
            let idle_since_labour = latest
                .as_ref()
                .and_then(|decision| decision.idle_since_labour.clone());
            statuses.push(ScalingSessionStatus {
                session_id: session.id.clone(),
                project_id: session.project_id.clone(),
                goal: session.goal.clone(),
                status: session.status.clone(),
                plan_revision,
                plan_tasks,
                ready_tasks,
                running_tasks,
                completed_tasks,
                current_workers: current_workers as u32,
                current_labour: current_labour as u32,
                active_workers: active_workers as u32,
                active_labour: active_labour as u32,
                latest,
                idle_since_workers,
                idle_since_labour,
            });
            if !plan_present {
                // A plan-less session is still listed so an operator can see
                // that scaling has no durable demand to act on.
                if let Some(status) = statuses.last_mut() {
                    status.ready_tasks = 0;
                }
            }
        }
        result.projects.push(ScalingProjectStatus {
            project_id: project.id,
            project_name: project.name,
            policy,
            sessions: statuses,
        });
    }
    Ok(result)
}

/// One daemon tick. `tick_key` makes the run idempotent for a plan revision and
/// tick bucket; a policy change produces a different key. `persist=false` is
/// used by tests/diagnostics to inspect actions without writing events.
pub(crate) async fn run_tick(
    engine: &Engine,
    capacity: &CapacityInfo,
    base_workers: usize,
    base_labour: usize,
    project_filter: Option<&str>,
    tick_key: Option<String>,
    persist: bool,
) -> Result<ScalingTickResult> {
    let store = engine.store().clone();
    let mut projects = store.list_projects()?;
    projects.retain(|project| project_filter.is_none_or(|wanted| wanted == project.id));
    let mut result = ScalingTickResult {
        projects: projects.len(),
        ..Default::default()
    };
    let now = OffsetDateTime::now_utc();
    let tick = tick_key.unwrap_or_else(|| format!("t{}", now.unix_timestamp() / 30));
    for project in projects {
        let policy = store.get_scaling_policy(&project.id)?;
        if !policy.enabled {
            continue;
        }
        let policy_hash = policy_input_hash(&policy);
        let sessions = store.list_sessions(Some(&project.id))?;
        for session in sessions {
            if session.status == "imported" || store.get_team_plan(&session.id)?.is_none() {
                continue;
            }
            result.sessions = result.sessions.saturating_add(1);
            let observation = match observe_session(
                engine,
                &session,
                &policy,
                capacity,
                base_workers,
                base_labour,
            )
            .await
            {
                Ok(observation) => observation,
                Err(error) => {
                    if result.errors.len() < 20 {
                        result.errors.push(format!("{}: {error:#}", session.id));
                    }
                    continue;
                }
            };
            result.evaluated = result.evaluated.saturating_add(1);
            // A policy edit that raced this tick wins: do not apply a stale
            // decision. The next tick evaluates against the new policy.
            let current_policy = store.get_scaling_policy(&project.id)?;
            if !current_policy.enabled || policy_input_hash(&current_policy) != policy_hash {
                continue;
            }
            let mut decision = evaluate(&policy, &observation);
            decision.replay_inputs = replay_inputs(&policy, &observation);
            decision.decision_key = format!(
                "rev{}:policy{}:{}",
                decision.plan_revision,
                &policy_hash[..12.min(policy_hash.len())],
                tick
            );
            if decision.action == ScalingAction::Hold {
                result.holds = result.holds.saturating_add(1);
            } else if decision.action.is_expansion() {
                result.expanded = result.expanded.saturating_add(1);
            } else {
                result.scaled_down = result.scaled_down.saturating_add(1);
            }
            if decision.manager_recommended {
                result.manager_recommendations = result.manager_recommendations.saturating_add(1);
            }
            if !persist {
                continue;
            }
            // A repeated tick must not apply the same plan-revision/tick twice,
            // even if the observation changed between the two calls.
            if store.has_scaling_decision(&session.id, &decision.decision_key)? {
                continue;
            }
            if decision.action != ScalingAction::Hold {
                let slots = engine.scaling_slots(
                    &session.id,
                    &session.project_id,
                    base_workers,
                    base_labour,
                );
                slots.enable_managed();
                let requested_workers = decision.target_workers;
                let requested_labour = decision.target_labour;
                let applied = slots.apply(requested_workers as usize, requested_labour as usize);
                decision.target_workers = applied.workers as u32;
                decision.target_labour = applied.labour as u32;
                if decision.target_workers != requested_workers
                    || decision.target_labour != requested_labour
                {
                    decision.reasons.push(format!(
                        "applied_target_clamped:{}W/{}L->{}W/{}L",
                        requested_workers,
                        requested_labour,
                        decision.target_workers,
                        decision.target_labour
                    ));
                }
                decision.applied = true;
                result.applied = result.applied.saturating_add(1);
            } else if let Some(slots) = engine.scaling_slots_existing(&session.id) {
                // A completed plan may still have a managed target. Reconcile
                // leaked counters against durable agent rows once the session
                // is no longer running.
                if session.status != "running" {
                    let (running_workers, running_labour) =
                        count_running_agents(&store, &session.id)?;
                    slots.reconcile_idle(running_workers, running_labour);
                }
            }
            // A repeated identical Hold is not a state transition. Persisting
            // one every tick would grow the event log without adding audit
            // value; we still persist a Hold when its targets, plan revision,
            // idle markers, or Manager evidence changed.
            let previous = store.latest_scaling_decision(&session.id)?;
            let should_persist = decision.action != ScalingAction::Hold
                || previous.as_ref().is_none_or(|previous| {
                    previous.action != ScalingAction::Hold
                        || previous.plan_revision != decision.plan_revision
                        || previous.target_workers != decision.target_workers
                        || previous.target_labour != decision.target_labour
                        || previous.manager_recommended != decision.manager_recommended
                        || previous.idle_since_workers != decision.idle_since_workers
                        || previous.idle_since_labour != decision.idle_since_labour
                });
            if !should_persist {
                continue;
            }
            match engine.record_scaling_decision(decision).await {
                Ok(Some(_)) => result.persisted = result.persisted.saturating_add(1),
                Ok(None) => {}
                Err(error) => {
                    if result.errors.len() < 20 {
                        result.errors.push(format!("{}: {error:#}", session.id));
                    }
                }
            }
        }
    }
    if project_filter.is_none() {
        let known = store
            .list_sessions(None)?
            .into_iter()
            .map(|session| session.id)
            .collect::<HashSet<_>>();
        engine.prune_scaling_slots(&known);
    }
    Ok(result)
}

fn policy_input_hash(policy: &ScalingPolicy) -> String {
    let bytes = serde_json::to_vec(policy).unwrap_or_default();
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

impl Engine {
    pub async fn get_scaling_policy(&self, project_id: &str) -> Result<ScalingPolicyResult> {
        let store = self.store().clone();
        let id = project_id.to_string();
        let project = tokio::task::spawn_blocking(move || store.get_project(&id)).await??;
        if project.is_none() {
            anyhow::bail!("project not found: {project_id}");
        }
        let policy = self.store().get_scaling_policy(project_id)?;
        Ok(ScalingPolicyResult {
            project_id: project_id.to_string(),
            policy,
        })
    }

    pub async fn set_scaling_policy(
        &self,
        project_id: &str,
        policy: ScalingPolicy,
    ) -> Result<ScalingPolicyResult> {
        let normalized = validate_policy(&policy)?;
        let store = self.store().clone();
        let id = project_id.to_string();
        let project = tokio::task::spawn_blocking(move || store.get_project(&id)).await??;
        if project.is_none() {
            anyhow::bail!("project not found: {project_id}");
        }
        let stored = normalized.clone();
        let id = project_id.to_string();
        let store = self.store().clone();
        tokio::task::spawn_blocking(move || store.set_scaling_policy(&id, &stored)).await??;
        let sessions = self.store().list_sessions(Some(project_id))?;
        for session in sessions {
            if !normalized.enabled {
                // Disabling stops new decisions and releases the managed targets
                // so the next run starts from the daemon's configured caps again.
                // A live run keeps its own Arc and is not interrupted.
                self.scaling.remove(&session.id);
                continue;
            }
            if let Some(slots) = self.scaling.existing(&session.id) {
                // An explicit policy change applies its new bounds immediately;
                // `apply` still clamps to active reservations, so this cannot
                // remove a running agent.
                let (workers, labour) = slots.targets();
                slots.enable_managed();
                slots.apply(
                    workers.clamp(
                        normalized.min_workers as usize,
                        normalized.max_workers as usize,
                    ),
                    labour.clamp(
                        normalized.min_labour as usize,
                        normalized.max_labour as usize,
                    ),
                );
            }
        }
        Ok(ScalingPolicyResult {
            project_id: project_id.to_string(),
            policy: normalized,
        })
    }

    /// Return the shared slot state for a session, creating it at the policy's
    /// bounded starting point when possible. This never starts a run.
    pub(crate) fn scaling_slots(
        &self,
        session_id: &str,
        project_id: &str,
        base_workers: usize,
        base_labour: usize,
    ) -> Arc<ScalingSlots> {
        if let Some(slots) = self.scaling.existing(session_id) {
            return slots;
        }
        let policy = self
            .store()
            .get_scaling_policy(project_id)
            .unwrap_or_default();
        let (workers, labour) = if policy.enabled {
            (
                base_workers.clamp(policy.min_workers as usize, policy.max_workers as usize),
                base_labour.clamp(policy.min_labour as usize, policy.max_labour as usize),
            )
        } else {
            (base_workers, base_labour)
        };
        let slots = self.scaling.slots_for(session_id, workers, labour);
        if policy.enabled {
            slots.enable_managed();
        }
        slots
    }

    pub(crate) fn prune_scaling_slots(&self, known: &HashSet<String>) {
        self.scaling.retain(known);
    }

    pub(crate) fn scaling_slots_existing(&self, session_id: &str) -> Option<Arc<ScalingSlots>> {
        self.scaling.existing(session_id)
    }

    pub async fn record_scaling_decision(
        &self,
        decision: ScalingDecision,
    ) -> Result<Option<ScalingDecision>> {
        let store = self.store().clone();
        let stored = tokio::task::spawn_blocking(move || {
            store.record_scaling_decision_with_event(&decision)
        })
        .await??;
        if let Some((stored, event)) = stored {
            self.broadcast_event(event);
            Ok(Some(stored))
        } else {
            Ok(None)
        }
    }

    pub async fn list_scaling_decisions(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<ScalingDecision>> {
        let store = self.store().clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.list_scaling_decisions(&id, limit)).await?
    }

    pub async fn latest_scaling_decision(
        &self,
        session_id: &str,
    ) -> Result<Option<ScalingDecision>> {
        let store = self.store().clone();
        let id = session_id.to_string();
        tokio::task::spawn_blocking(move || store.latest_scaling_decision(&id)).await?
    }
}

/// Exposed for tests and the daemon tick; the pure controller has no store.
pub fn analyze_plan_tasks(
    tasks: &[ScalingTaskObservation],
    now: OffsetDateTime,
) -> (usize, usize, u64) {
    let demand = analyze_plan(tasks, now, HARD_WORKERS);
    (
        demand.demand_workers,
        demand.ready_tasks,
        demand.oldest_ready_age_secs,
    )
}

#[cfg(test)]
#[path = "scaling/tests.rs"]
mod tests;
