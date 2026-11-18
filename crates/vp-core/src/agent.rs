use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_recursion::async_recursion;
use async_trait::async_trait;
use base64::Engine as _;
use sha2::{Digest, Sha256};

use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::io::AsyncWriteExt;
use tokio::sync::{oneshot, watch};
use tokio::time::timeout;
use uuid::Uuid;
use vp_protocol::remote::{
    DEFAULT_BUNDLE_CHUNK_BYTES, RemoteBundleSummary, RemoteIntegrationParams,
    RemoteIntegrationResult, RemoteIntegrationVerification,
};
use vp_protocol::types::{
    AgentInfo, AgentProbeResult, AgentRecord, ApprovalMode, CapacityInfo, ConfigureAgentParams,
    ContextCheckpoint, ContextCheckpointDetailResult, ContextCompactionSummary, ContextMeasurement,
    ContextRestorePreflight, ContextRestoreResult, ContextStructuredState, EffectReceipt,
    EffectState, IntakeDispatchResult, IntakeTask, MERGE_SOURCE_REMOTE, MergeQueueEntry,
    MergeQueueSource, OrchestrationRecovery, PlanDepartment, PlanRevisionDetailResult, PlanTask,
    Program, ProgramMilestone, ProgramTickResult, Project, ReviseTeamPlanResult, RoleModels,
    RunBudgetSnapshot, ScheduledTask, SchedulerProjectStatus, SchedulerStatusResult, Session,
    SessionCachePlanResult, SessionCachePlanRole, SessionCacheWarmResult, SessionCheckpoint,
    SessionCheckpointDiffResult, SessionCheckpointRestoreResult, SessionCompactResult,
    SessionForkResult, SessionMergeForkResult, SessionPreemptResult, SessionPreemptionResult,
    SessionRunCancelResult, SessionRunResumeResult, Skill, SourceCitation, SourceSearchHit,
    StallRecoveryMode, StoredMessage, TeamPlan, TodoItem, ToolBundleCatalogResult,
    ToolBundleCatalogRow, ToolBundleCategoryCount, ToolBundleDiffResult, ToolBundleHistoryEntry,
    ToolBundleHistoryResult, ToolBundleLastRun, ToolBundleMatrixResult, ToolBundleMatrixRow,
    ToolBundleResult, ToolBundleRoleResult, ToolBundleToolInfo, ToolBundleWarningInfo,
};
use vp_providers::{
    AnthropicProvider, ChatMessage, ChatRequest, OpenAiCompatibleProvider, Provider, ProviderError,
    StreamEvent, ToolCall, ToolSchema,
};
use vp_store::Store;

use crate::Engine;
use crate::RuntimeLeaseGuard;
use crate::acp;
use crate::budget::{BudgetReason, GovernorGate, Rejection, ScopeRequest, Spend};
use crate::budget_governor::{
    DECISION_EVENT_KIND, GovernorBudgetView, GovernorDenial, GovernorReason, GovernorRunContext,
    denial_payload,
};
use crate::permissions::{
    ApprovalDecision, RiskLevel, assess_acp_permission, assess_remote_acp_delegation, decide,
};
use crate::remote_integration::{self, RemoteIntegrationFailure};
use crate::scaling::ScalingSlots;
use crate::tooling::errors::ToolErrorCode;
use crate::tools::{ToolContext, ToolOutput, ToolRegistry};
use crate::worktree;

// The agent runtime is split into cohesive submodules. Shared state/control
// types stay in this parent module; the private re-exports below keep sibling
// access flat across the split without widening the public API.
mod context;
mod delegation;
mod dispatch;
mod merge;
mod plan;
mod provider;
mod run_loop;
mod schemas;
mod scout;
mod script;
mod service;
mod service_run;
mod support;
mod tools;

// Keep the original `crate::agent::...` paths for items that moved into
// child modules but are still referenced from other `vp-core` modules.
pub(crate) use context::conversation_from_events;
pub(crate) use plan::validate_team_plan;

pub(crate) fn global_session_cap() -> usize {
    env::var("VERLINPARK_GLOBAL_SESSION_CAP")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4)
        .clamp(1, 32)
}

pub(crate) fn global_worker_cap() -> usize {
    env::var("VERLINPARK_GLOBAL_WORKERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12)
        .clamp(1, 100)
}

pub(crate) fn global_labour_cap() -> usize {
    env::var("VERLINPARK_GLOBAL_LABOUR")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24)
        .clamp(1, 100)
}

fn compact_after_tokens() -> u64 {
    env::var("VERLINPARK_COMPACT_AFTER_TOKENS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(0)
        .min(20_000_000)
}

fn context_window() -> u64 {
    env::var("VERLINPARK_CONTEXT_WINDOW")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(128_000)
        .min(10_000_000)
}

fn compact_keep_messages() -> usize {
    env::var("VERLINPARK_COMPACT_KEEP_MESSAGES")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(8)
        .clamp(2, 100)
}

/// Token budget for the retained tail. `0` disables the token constraint and
/// leaves message-count selection in charge.
fn compact_keep_tokens() -> u64 {
    env::var("VERLINPARK_COMPACT_KEEP_TOKENS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(15_000)
        .min(2_000_000)
}

/// Fraction of the compaction threshold at which a summary snapshot may be
/// prepared in the run loop. It is never a second agent run.
fn compact_prepare_ratio() -> f64 {
    env::var("VERLINPARK_COMPACT_PREPARE_RATIO")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|value| value.is_finite())
        .unwrap_or(0.6)
        .clamp(0.3, 0.95)
}

fn auto_expand_workers() -> bool {
    env::var("VERLINPARK_AUTO_EXPAND_WORKERS")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        })
}

fn priority_aging() -> Duration {
    let seconds = env::var("VERLINPARK_PRIORITY_AGING_SECS")
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

fn task_capacity_priority(session_priority: i32, task_priority: i32) -> i32 {
    session_priority
        .saturating_add(task_priority)
        .clamp(-100, 100)
}

fn effective_priority(priority: i32, enqueued_at: Instant, aging: Duration) -> i64 {
    if aging.is_zero() {
        return i64::from(priority);
    }
    let elapsed = enqueued_at.elapsed().as_millis() as i64;
    let interval = aging.as_millis().max(1) as i64;
    i64::from(priority) + (elapsed / interval).min(100)
}

#[derive(Clone)]
pub struct AgentConfig {
    pub provider: Arc<dyn Provider>,
    pub provider_name: String,
    pub model: String,
    pub role_models: RoleModels,
    pub base_url: Option<String>,
    pub max_turns: u32,
    pub max_tokens: Option<u32>,
    pub max_managers: usize,
    pub max_workers: usize,
    pub max_labour: usize,
    pub auto_expand_workers: bool,
    pub integration_command: Option<String>,
    pub tools: ToolRegistry,
    /// Maximum number of session runs active across the whole daemon.
    pub max_active_sessions: usize,
    /// Cross-session write-scope locks for Worker/Labour runs.
    pub scope_locks: Arc<ScopeLocks>,
    /// Daemon-wide Worker/Labour admission.
    pub capacity: Arc<CapacityController>,
    /// Maximum time to wait between provider stream events.
    pub provider_idle_timeout_secs: u64,
    /// Maximum time a blocking user question may wait for an answer.
    pub question_timeout_secs: u64,
    /// Risk-based approval policy for tool calls.
    pub approval_mode: ApprovalMode,
    /// Maximum time a tool/ACP approval may wait for a human.
    pub approval_timeout_secs: u64,
    /// Compact the active conversation after this many estimated tokens.
    /// Zero disables compaction.
    pub compact_after_tokens: u64,
    /// Number of recent messages to retain outside the compacted summary.
    pub compact_keep_messages: usize,
    /// Approximate token budget for the retained tail. The deterministic cut
    /// never splits a message and always keeps at least one message.
    pub compact_keep_tokens: u64,
    /// Fraction of the compaction threshold at which a prepared summary may
    /// be built. The swap still happens synchronously at the threshold.
    pub compact_prepare_ratio: f64,
    /// Base model context window in tokens. Zero disables automatic context
    /// management. Per-role role_models can override this.
    pub context_window: u64,
}

/// Daemon-wide admission control for Worker/Labour runs. Per-session caps still
/// apply; this prevents many sessions from each running their own full cap.
///
/// Permits are granted round-robin across sessions so one session cannot
/// monopolize the queue with a large batch.
pub struct CapacityController {
    workers: Arc<FairCapacity>,
    labour: Arc<FairCapacity>,
    worker_limit: usize,
    labour_limit: usize,
}

struct FairCapacity {
    limit: usize,
    aging: Duration,
    state: Mutex<FairState>,
}

struct FairState {
    active: usize,
    waiters: VecDeque<CapacityWaiter>,
    next_id: u64,
    last_session: Option<String>,
}

struct CapacityWaiter {
    id: u64,
    session_id: String,
    priority: i32,
    enqueued_at: Instant,
    sender: oneshot::Sender<()>,
}

pub struct CapacityPermit {
    queue: Arc<FairCapacity>,
}

impl Drop for CapacityPermit {
    fn drop(&mut self) {
        self.queue.release();
    }
}

impl FairCapacity {
    fn new_with_aging(limit: usize, aging: Duration) -> Self {
        Self {
            limit: limit.max(1),
            aging,
            state: Mutex::new(FairState {
                active: 0,
                waiters: VecDeque::new(),
                next_id: 0,
                last_session: None,
            }),
        }
    }

    fn available(&self) -> usize {
        self.state
            .lock()
            .map(|state| self.limit.saturating_sub(state.active))
            .unwrap_or(0)
    }

    fn try_acquire(self: &Arc<Self>, session_id: &str) -> Option<CapacityPermit> {
        let mut state = self.state.lock().ok()?;
        if state.active >= self.limit || !state.waiters.is_empty() {
            return None;
        }
        state.active += 1;
        state.last_session = Some(session_id.to_string());
        drop(state);
        Some(CapacityPermit {
            queue: self.clone(),
        })
    }

    async fn acquire(
        self: &Arc<Self>,
        session_id: &str,
        priority: i32,
        control: &RunControl,
    ) -> Option<CapacityPermit> {
        let (waiter_id, receiver) = {
            let mut state = self.state.lock().ok()?;
            if state.active < self.limit && state.waiters.is_empty() {
                state.active += 1;
                state.last_session = Some(session_id.to_string());
                return Some(CapacityPermit {
                    queue: self.clone(),
                });
            }
            let id = state.next_id;
            state.next_id = state.next_id.saturating_add(1);
            let (sender, receiver) = oneshot::channel();
            state.waiters.push_back(CapacityWaiter {
                id,
                session_id: session_id.to_string(),
                priority,
                enqueued_at: Instant::now(),
                sender,
            });
            (id, receiver)
        };
        tokio::select! {
            result = receiver => {
                if result.is_ok() {
                    Some(CapacityPermit { queue: self.clone() })
                } else {
                    None
                }
            }
            _ = wait_for_cancel(control) => {
                let removed = {
                    let mut state = self.state.lock().ok()?;
                    state
                        .waiters
                        .iter()
                        .position(|waiter| waiter.id == waiter_id)
                        .map(|index| state.waiters.remove(index))
                };
                if removed.is_none() {
                    // The permit was granted concurrently with cancellation.
                    self.release();
                }
                None
            }
        }
    }

    fn release(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.active = state.active.saturating_sub(1);
            self.grant_waiters(&mut state);
        }
    }

    fn grant_waiters(&self, state: &mut FairState) {
        while state.active < self.limit && !state.waiters.is_empty() {
            let aging = self.aging;
            let highest = state
                .waiters
                .iter()
                .map(|waiter| effective_priority(waiter.priority, waiter.enqueued_at, aging))
                .max()
                .expect("waiters are not empty");
            let candidates = state
                .waiters
                .iter()
                .enumerate()
                .filter(|(_, waiter)| {
                    effective_priority(waiter.priority, waiter.enqueued_at, aging) == highest
                })
                .map(|(index, _)| index)
                .collect::<Vec<_>>();
            let index = candidates
                .iter()
                .copied()
                .find(|index| {
                    state.last_session.as_deref() != Some(state.waiters[*index].session_id.as_str())
                })
                .unwrap_or(candidates[0]);
            let waiter = state.waiters.remove(index).expect("waiter index exists");
            state.active += 1;
            let session_id = waiter.session_id.clone();
            if waiter.sender.send(()).is_ok() {
                state.last_session = Some(session_id);
            } else {
                state.active = state.active.saturating_sub(1);
            }
        }
    }
}

impl CapacityController {
    /// Construct a daemon-wide Worker/Labour admission controller. Embedders
    /// (including the deterministic benchmark harness) must provide the same
    /// explicit limits as the daemon; the controller never widens a session's
    /// configured caps.
    pub fn new(workers: usize, labour: usize) -> Self {
        Self::new_with_aging(workers, labour, priority_aging())
    }

    fn new_with_aging(workers: usize, labour: usize, aging: Duration) -> Self {
        let worker_limit = workers.max(1);
        let labour_limit = labour.max(1);
        Self {
            workers: Arc::new(FairCapacity::new_with_aging(worker_limit, aging)),
            labour: Arc::new(FairCapacity::new_with_aging(labour_limit, aging)),
            worker_limit,
            labour_limit,
        }
    }

    fn snapshot(&self) -> CapacityInfo {
        CapacityInfo {
            worker_limit: self.worker_limit,
            worker_available: self.workers.available(),
            labour_limit: self.labour_limit,
            labour_available: self.labour.available(),
        }
    }

    fn try_acquire(&self, role: AgentRole, session_id: &str) -> Option<CapacityPermit> {
        match role {
            // Scouts are read-only but still make provider calls. They account
            // against the Worker lane so a team cannot bypass daemon-wide
            // capacity by spawning assistants.
            AgentRole::Worker | AgentRole::Scout => self.workers.try_acquire(session_id),
            AgentRole::Labour => self.labour.try_acquire(session_id),
            _ => None,
        }
    }

    async fn acquire(
        &self,
        role: AgentRole,
        session_id: &str,
        priority: i32,
        control: &RunControl,
    ) -> Option<CapacityPermit> {
        match role {
            AgentRole::Worker | AgentRole::Scout => {
                self.workers.acquire(session_id, priority, control).await
            }
            AgentRole::Labour => self.labour.acquire(session_id, priority, control).await,
            _ => None,
        }
    }
}

/// A daemon-wide registry of active task write scopes. Tasks in different
/// sessions that overlap wait for each other; tasks in the same session are
/// allowed to share (their own scheduler already serializes them).
///
/// Production instances use the daemon store as the authoritative registry, so
/// leases survive a client reconnect and are visible through `task.queue`.
/// `Default` keeps an in-memory registry for unit tests and embedders.
#[derive(Default)]
pub struct ScopeLocks {
    entries: Mutex<Vec<ScopeLockEntry>>,
    changed: tokio::sync::Notify,
    store: Option<Store>,
}

struct ScopeLockEntry {
    project_id: String,
    session_id: String,
    agent_id: String,
    scopes: Vec<String>,
}

pub struct ScopeLockGuard {
    locks: Arc<ScopeLocks>,
    project_id: String,
    agent_id: String,
}

impl ScopeLocks {
    pub fn with_store(store: Store) -> Self {
        Self {
            entries: Mutex::new(Vec::new()),
            changed: tokio::sync::Notify::new(),
            store: Some(store),
        }
    }

    async fn acquire(
        self: &Arc<Self>,
        project_id: &str,
        session_id: &str,
        agent_id: &str,
        scopes: &[String],
        control: &RunControl,
    ) -> Result<Option<ScopeLockGuard>> {
        loop {
            if control.is_cancelled() {
                return Ok(None);
            }
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let acquired = if let Some(store) = &self.store {
                store.try_acquire_write_lease(project_id, session_id, agent_id, scopes)?
            } else {
                let mut entries = self
                    .entries
                    .lock()
                    .map_err(|_| anyhow!("scope lock mutex poisoned"))?;
                let conflict = entries.iter().find(|entry| {
                    entry.project_id == project_id
                        && entry.session_id != session_id
                        && scopes_overlap(&entry.scopes, scopes)
                });
                if conflict.is_none() {
                    entries.push(ScopeLockEntry {
                        project_id: project_id.to_string(),
                        session_id: session_id.to_string(),
                        agent_id: agent_id.to_string(),
                        scopes: scopes.to_vec(),
                    });
                    true
                } else {
                    false
                }
            };
            if acquired {
                return Ok(Some(ScopeLockGuard {
                    locks: self.clone(),
                    project_id: project_id.to_string(),
                    agent_id: agent_id.to_string(),
                }));
            }
            tokio::select! {
                _ = &mut changed => {}
                _ = wait_for_cancel(control) => return Ok(None),
            }
        }
    }
}

impl Drop for ScopeLockGuard {
    fn drop(&mut self) {
        if let Some(store) = &self.locks.store {
            if let Err(error) = store.release_write_lease(&self.agent_id) {
                tracing::warn!(%error, agent_id = %self.agent_id, "failed to release write lease");
            }
        } else if let Ok(mut entries) = self.locks.entries.lock() {
            entries.retain(|entry| {
                !(entry.project_id == self.project_id && entry.agent_id == self.agent_id)
            });
        }
        self.locks.changed.notify_waiters();
    }
}

#[derive(Clone)]
pub struct AgentService {
    engine: Engine,
    config: Arc<RwLock<Option<AgentConfig>>>,
    runs: Arc<Mutex<HashMap<String, Arc<RunControl>>>>,
    auto_resolver_started: Arc<AtomicBool>,
    task_scheduler_started: Arc<AtomicBool>,
    scaling_started: Arc<AtomicBool>,
    global_slots: Arc<AdmissionGate>,
    capacity: Arc<CapacityController>,
}

fn continuation_claim_secs() -> u64 {
    env::var("VERLINPARK_CONTINUATION_CLAIM_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(120)
        .clamp(10, 3_600)
}

fn continuation_retry_backoff_secs(attempts: u32) -> i64 {
    let base = env::var("VERLINPARK_CONTINUATION_RETRY_SECS")
        .ok()
        .and_then(|value| value.parse::<i64>().ok())
        .unwrap_or(15)
        .clamp(1, 300);
    base.saturating_mul(i64::from(attempts.max(1))).min(900)
}

struct RunControl {
    cancelled: watch::Sender<bool>,
    paused: watch::Sender<bool>,
    finalized: tokio::sync::Mutex<Option<String>>,
    manual: bool,
    cancellable: bool,
    preempt: crate::preemption::Token,
    preemption_policy: Mutex<vp_protocol::types::PreemptionPolicy>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ResumeMode {
    Manual,
    Auto,
}

impl RunControl {
    fn new() -> Self {
        Self::with_manual(false, true)
    }

    fn new_manual() -> Self {
        Self::with_manual(true, true)
    }

    fn new_operation() -> Self {
        Self::with_manual(true, false)
    }

    fn with_manual(manual: bool, cancellable: bool) -> Self {
        let (cancelled, _) = watch::channel(false);
        let (paused, _) = watch::channel(false);
        Self {
            cancelled,
            paused,
            finalized: tokio::sync::Mutex::new(None),
            manual,
            cancellable,
            preempt: crate::preemption::Token::disabled(),
            preemption_policy: Mutex::new(vp_protocol::types::PreemptionPolicy::default()),
        }
    }

    /// Arm automatic wall-clock preemption for this run. The token is created
    /// with the control so an explicit `session.preempt` can fire during
    /// admission, before the run loop has its exact budget start.
    fn arm_preemption(
        &self,
        time_budget_secs: Option<u64>,
        policy: vp_protocol::types::PreemptionPolicy,
    ) {
        let deadline =
            time_budget_secs.map(|secs| Instant::now() + Duration::from_secs(secs.max(1)));
        self.arm_preemption_deadline(deadline, policy);
    }

    fn arm_preemption_deadline(
        &self,
        deadline: Option<Instant>,
        policy: vp_protocol::types::PreemptionPolicy,
    ) {
        crate::preemption::apply_policy(&self.preempt, &policy);
        self.preempt
            .set_deadline(deadline.filter(|_| policy.enabled));
        if let Ok(mut stored) = self.preemption_policy.lock() {
            *stored = policy;
        }
    }

    fn preemption_policy(&self) -> vp_protocol::types::PreemptionPolicy {
        self.preemption_policy
            .lock()
            .map(|policy| policy.clone())
            .unwrap_or_default()
    }

    fn preempt_report(&self) -> Option<crate::preemption::Snapshot> {
        self.preempt.snapshot()
    }

    fn is_preempted(&self) -> bool {
        self.preempt.is_preempted()
    }

    /// Operator-triggered hard preemption. This is distinct from `cancel`:
    /// it aborts provider/tool work in flight instead of waiting for the next
    /// turn boundary.
    fn request_preempt(&self) -> crate::preemption::Snapshot {
        let phase = self.preempt.phase();
        let side_effects_unknown = phase == crate::preemption::Phase::Tool;
        self.preempt.trigger(
            crate::preemption::Reason::Explicit,
            phase,
            side_effects_unknown,
        )
    }

    async fn finalize_session(
        &self,
        engine: &Engine,
        session_id: &str,
        status: &str,
    ) -> Result<()> {
        let mut finalized = self.finalized.lock().await;
        *finalized = Some(status.to_string());
        engine.budget_registry().clear_session(session_id);
        engine.clear_unlimited_flag(session_id);
        if let Err(error) = engine.interrupt_running_plan_tasks(session_id).await {
            tracing::warn!(%error, session_id, "failed to clear running plan tasks");
        }
        engine.update_session_status(session_id, status).await
    }

    fn cancel(&self) {
        self.cancelled.send_replace(true);
    }

    fn pause(&self) {
        self.paused.send_replace(true);
    }

    fn resume(&self) {
        self.paused.send_replace(false);
    }

    fn is_cancelled(&self) -> bool {
        *self.cancelled.borrow()
    }

    fn is_paused(&self) -> bool {
        *self.paused.borrow()
    }

    fn subscribe(&self) -> watch::Receiver<bool> {
        self.cancelled.subscribe()
    }

    fn subscribe_paused(&self) -> watch::Receiver<bool> {
        self.paused.subscribe()
    }
}

/// Daemon-wide session admission with explicit priority. Higher priority runs
/// are admitted first; ties prefer a different session/project from the last
/// admitted one so one project cannot monopolize all slots.
struct AdmissionGate {
    limit: usize,
    aging: Duration,
    state: Mutex<AdmissionState>,
}

struct AdmissionState {
    active: usize,
    waiters: Vec<AdmissionWaiter>,
    next_id: u64,
    last_session: Option<String>,
    last_project: Option<String>,
}

struct AdmissionWaiter {
    id: u64,
    session_id: String,
    project_id: String,
    priority: i32,
    enqueued_at: Instant,
    sender: oneshot::Sender<()>,
}

struct AdmissionPermit {
    gate: Arc<AdmissionGate>,
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        self.gate.release();
    }
}

impl AdmissionGate {
    fn new(limit: usize) -> Self {
        Self::new_with_aging(limit, priority_aging())
    }

    fn new_with_aging(limit: usize, aging: Duration) -> Self {
        Self {
            limit: limit.max(1),
            aging,
            state: Mutex::new(AdmissionState {
                active: 0,
                waiters: Vec::new(),
                next_id: 0,
                last_session: None,
                last_project: None,
            }),
        }
    }

    async fn acquire(
        self: &Arc<Self>,
        session_id: &str,
        project_id: &str,
        priority: i32,
        control: &RunControl,
    ) -> Option<AdmissionPermit> {
        let (waiter_id, receiver) = {
            let mut state = self.state.lock().ok()?;
            if state.active < self.limit && state.waiters.is_empty() {
                state.active += 1;
                state.last_session = Some(session_id.to_string());
                state.last_project = Some(project_id.to_string());
                return Some(AdmissionPermit { gate: self.clone() });
            }
            let id = state.next_id;
            state.next_id = state.next_id.saturating_add(1);
            let (sender, receiver) = oneshot::channel();
            state.waiters.push(AdmissionWaiter {
                id,
                session_id: session_id.to_string(),
                project_id: project_id.to_string(),
                priority,
                enqueued_at: Instant::now(),
                sender,
            });
            (id, receiver)
        };
        tokio::select! {
            result = receiver => {
                if result.is_ok() {
                    Some(AdmissionPermit { gate: self.clone() })
                } else {
                    None
                }
            }
            _ = wait_for_cancel(control) => {
                let removed = {
                    let mut state = self.state.lock().ok()?;
                    state
                        .waiters
                        .iter()
                        .position(|waiter| waiter.id == waiter_id)
                        .map(|index| state.waiters.remove(index))
                };
                if removed.is_none() {
                    self.release();
                }
                None
            }
        }
    }

    fn release(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.active = state.active.saturating_sub(1);
            self.grant_waiters(&mut state);
        }
    }

    fn grant_waiters(&self, state: &mut AdmissionState) {
        while state.active < self.limit {
            let waiter = {
                let last_session = state.last_session.as_deref();
                let last_project = state.last_project.as_deref();
                take_admission_waiter(&mut state.waiters, last_session, last_project, self.aging)
            };
            let Some(waiter) = waiter else {
                break;
            };
            state.active += 1;
            let session_id = waiter.session_id.clone();
            let project_id = waiter.project_id.clone();
            if waiter.sender.send(()).is_ok() {
                state.last_session = Some(session_id);
                state.last_project = Some(project_id);
            } else {
                state.active = state.active.saturating_sub(1);
            }
        }
    }

    #[cfg(test)]
    fn snapshot(&self) -> (usize, usize, usize) {
        self.state
            .lock()
            .map(|state| (self.limit, state.active, state.waiters.len()))
            .unwrap_or((self.limit, 0, 0))
    }
}

fn take_admission_waiter(
    waiters: &mut Vec<AdmissionWaiter>,
    last_session: Option<&str>,
    last_project: Option<&str>,
    aging: Duration,
) -> Option<AdmissionWaiter> {
    if waiters.is_empty() {
        return None;
    }
    let highest = waiters
        .iter()
        .map(|waiter| effective_priority(waiter.priority, waiter.enqueued_at, aging))
        .max()?;
    let candidates = waiters
        .iter()
        .enumerate()
        .filter(|(_, waiter)| {
            effective_priority(waiter.priority, waiter.enqueued_at, aging) == highest
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    let index = candidates
        .iter()
        .copied()
        .find(|index| Some(waiters[*index].session_id.as_str()) != last_session)
        .or_else(|| {
            candidates
                .iter()
                .copied()
                .find(|index| Some(waiters[*index].project_id.as_str()) != last_project)
        })
        .unwrap_or(candidates[0]);
    Some(waiters.remove(index))
}

async fn wait_for_resume(control: &RunControl) -> bool {
    let mut paused = control.subscribe_paused();
    let mut cancelled = control.subscribe();
    loop {
        if control.is_cancelled() || control.is_preempted() {
            return false;
        }
        if !*paused.borrow() {
            return true;
        }
        tokio::select! {
            changed = paused.changed() => {
                if changed.is_err() {
                    return false;
                }
            }
            changed = cancelled.changed() => {
                if changed.is_err() || *cancelled.borrow() {
                    return false;
                }
            }
            _ = control.preempt.cancelled() => return false,
        }
    }
}

async fn wait_for_task_resume(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    control: &RunControl,
) -> Result<bool> {
    loop {
        if !wait_for_resume(control).await {
            return Ok(false);
        }
        if control.is_cancelled() || control.is_preempted() {
            return Ok(false);
        }
        let Some(task_id) = &spec.task_id else {
            return Ok(true);
        };
        match engine
            .plan_task_status(session_id, task_id)
            .await?
            .as_deref()
        {
            Some("cancelling" | "cancelled") => return Ok(false),
            Some("pausing") => {
                engine.mark_plan_task_paused(session_id, task_id).await?;
                continue;
            }
            Some("paused") => {
                let mut state = engine.scheduler().subscribe_state();
                // Re-check after subscribing so a resume between the first
                // status read and the subscription cannot be lost.
                if engine
                    .plan_task_status(session_id, task_id)
                    .await?
                    .as_deref()
                    != Some("paused")
                {
                    continue;
                }
                tokio::select! {
                    changed = state.changed() => {
                        if changed.is_err() {
                            return Ok(false);
                        }
                    }
                    _ = wait_for_cancel(control) => return Ok(false),
                }
            }
            _ => return Ok(true),
        }
    }
}

async fn wait_for_cancel(control: &RunControl) {
    if control.is_cancelled() || control.is_preempted() {
        return;
    }
    let mut cancelled = control.subscribe();
    let preempt = control.preempt.clone();
    tokio::select! {
        _ = async {
            loop {
                if cancelled.changed().await.is_err() || control.is_cancelled() {
                    break;
                }
            }
        } => {}
        _ = preempt.cancelled() => {}
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum AgentRole {
    Leader,
    Manager,
    Worker,
    Verifier,
    Labour,
    /// Auxiliary read-only exploration agent. It is not part of the
    /// Leader → Manager → Worker → Labour delegation chain and never owns a
    /// task worktree, merge entry, or commit.
    Scout,
}

impl AgentRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Manager => "manager",
            Self::Worker => "worker",
            Self::Verifier => "verifier",
            Self::Labour => "labour",
            Self::Scout => "scout",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "leader" => Some(Self::Leader),
            "manager" => Some(Self::Manager),
            "worker" => Some(Self::Worker),
            "verifier" => Some(Self::Verifier),
            "labour" => Some(Self::Labour),
            "scout" => Some(Self::Scout),
            _ => None,
        }
    }

    fn default_name(self) -> &'static str {
        match self {
            Self::Leader => "Leader",
            Self::Manager => "Manager",
            Self::Worker => "Worker",
            Self::Verifier => "Verifier",
            Self::Labour => "Labour",
            Self::Scout => "Scout",
        }
    }

    fn child_role(self) -> Option<Self> {
        match self {
            Self::Leader => Some(Self::Manager),
            Self::Manager => Some(Self::Worker),
            Self::Worker => Some(Self::Labour),
            Self::Verifier | Self::Labour | Self::Scout => None,
        }
    }

    /// Task-scoped agents receive `run/task` in the v3 head prefix. A Leader
    /// follow-up (and a Manager's department objective) must not turn the
    /// current prompt into a head system section.
    fn task_scoped(self) -> bool {
        matches!(
            self,
            Self::Worker | Self::Labour | Self::Verifier | Self::Scout
        )
    }

    pub(crate) fn allows_tool(self, profile: AgentProfile, name: &str) -> bool {
        if name == "wait_for_gate" && self == Self::Labour {
            return false;
        }
        match self {
            Self::Leader => true,
            Self::Manager => {
                matches!(
                    name,
                    "read_file"
                        | "list_dir"
                        | "grep"
                        | "find_files"
                        | "file_info"
                        | "read_files"
                        | "project_profile"
                        | "git"
                        | "repo_map"
                        | "find_symbol"
                        | "find_references"
                        | "dependency_graph"
                        | "build_context"
                        | "affected_tests"
                        | "lsp_query"
                        | "read_pdf"
                        | "read_image"
                        | "read_document"
                        | "archive_list"
                        | "media_info"
                        | "data_query"
                        | "read_tool_output"
                        | "wait_for_gate"
                        | "remote_read"
                        | "remote_list"
                        | "remote_git"
                ) || (profile == AgentProfile::Research
                    && matches!(name, "fetch_url" | "web_search"))
            }
            Self::Verifier => {
                matches!(
                    name,
                    "read_file"
                        | "list_dir"
                        | "grep"
                        | "find_files"
                        | "file_info"
                        | "read_files"
                        | "project_profile"
                        | "git"
                        | "repo_map"
                        | "find_symbol"
                        | "find_references"
                        | "dependency_graph"
                        | "build_context"
                        | "affected_tests"
                        | "lsp_query"
                        | "read_pdf"
                        | "read_image"
                        | "read_document"
                        | "archive_list"
                        | "media_info"
                        | "data_query"
                        | "read_tool_output"
                        | "shell"
                        | "run_project_task"
                        | "code_diagnostics"
                        | "process"
                        | "wait_for"
                        | "local_http"
                        | "remote_read"
                        | "remote_list"
                        | "remote_git"
                        | "remote_exec"
                ) || (profile == AgentProfile::Research
                    && matches!(name, "fetch_url" | "web_search"))
            }
            Self::Scout => matches!(
                name,
                "read_file"
                    | "list_dir"
                    | "grep"
                    | "find_files"
                    | "file_info"
                    | "read_files"
                    | "git"
                    | "repo_map"
                    | "find_symbol"
                    | "find_references"
                    | "dependency_graph"
                    | "affected_tests"
                    | "code_diagnostics"
                    | "build_context"
                    | "archive_list"
                    | "media_info"
                    // Read-only context tools. There is deliberately no shell,
                    // write, process, delegation, merge, MCP, or network tool.
                    | "list_sources"
                    | "search_sources"
                    | "read_source"
                    | "read_tool_output"
                    | "recall"
                    | "team_status"
                    | "remote_read"
                    | "remote_list"
                    | "remote_git"
            ),
            Self::Worker | Self::Labour => match profile {
                AgentProfile::Coding => matches!(
                    name,
                    "read_file"
                        | "list_dir"
                        | "grep"
                        | "edit_file"
                        | "write_file"
                        | "shell"
                        | "fetch_url"
                        | "find_files"
                        | "file_info"
                        | "read_files"
                        | "apply_patch"
                        | "file_manage"
                        | "project_profile"
                        | "run_project_task"
                        | "git"
                        | "repo_map"
                        | "find_symbol"
                        | "find_references"
                        | "dependency_graph"
                        | "build_context"
                        | "affected_tests"
                        | "lsp_query"
                        | "read_pdf"
                        | "read_image"
                        | "read_document"
                        | "archive_list"
                        | "media_info"
                        | "archive_extract"
                        | "media_transcode"
                        | "data_query"
                        | "read_tool_output"
                        | "wait_for_gate"
                        | "code_diagnostics"
                        | "process"
                        | "wait_for"
                        | "local_http"
                        | "browser"
                        | "remote_read"
                        | "remote_write"
                        | "remote_list"
                        | "remote_exec"
                        | "remote_git"
                        | "remote_worktree"
                        | "computer"
                ),
                AgentProfile::Research => matches!(
                    name,
                    "read_file"
                        | "list_dir"
                        | "grep"
                        | "write_file"
                        | "fetch_url"
                        | "find_files"
                        | "file_info"
                        | "read_files"
                        | "project_profile"
                        | "git"
                        | "repo_map"
                        | "find_symbol"
                        | "find_references"
                        | "dependency_graph"
                        | "build_context"
                        | "affected_tests"
                        | "lsp_query"
                        | "read_pdf"
                        | "read_image"
                        | "read_document"
                        | "archive_list"
                        | "media_info"
                        | "archive_extract"
                        | "media_transcode"
                        | "data_query"
                        | "read_tool_output"
                        | "wait_for_gate"
                        | "web_search"
                        | "local_http"
                        | "remote_read"
                        | "remote_write"
                        | "remote_list"
                        | "remote_git"
                ),
                AgentProfile::Verification => matches!(
                    name,
                    "read_file"
                        | "list_dir"
                        | "grep"
                        | "shell"
                        | "fetch_url"
                        | "find_files"
                        | "file_info"
                        | "read_files"
                        | "project_profile"
                        | "run_project_task"
                        | "git"
                        | "repo_map"
                        | "find_symbol"
                        | "find_references"
                        | "dependency_graph"
                        | "build_context"
                        | "affected_tests"
                        | "lsp_query"
                        | "read_pdf"
                        | "read_image"
                        | "read_document"
                        | "archive_list"
                        | "media_info"
                        | "data_query"
                        | "read_tool_output"
                        | "wait_for_gate"
                        | "code_diagnostics"
                        | "process"
                        | "wait_for"
                        | "local_http"
                        | "remote_read"
                        | "remote_list"
                        | "remote_git"
                        | "remote_exec"
                ),
                AgentProfile::Generic => matches!(
                    name,
                    "read_file"
                        | "list_dir"
                        | "grep"
                        | "find_files"
                        | "file_info"
                        | "read_files"
                        | "project_profile"
                        | "git"
                        | "repo_map"
                        | "find_symbol"
                        | "find_references"
                        | "dependency_graph"
                        | "build_context"
                        | "affected_tests"
                        | "lsp_query"
                        | "read_pdf"
                        | "read_image"
                        | "read_document"
                        | "archive_list"
                        | "media_info"
                        | "data_query"
                        | "read_tool_output"
                        | "wait_for_gate"
                        | "remote_read"
                        | "remote_list"
                        | "remote_git"
                ),
            },
        }
    }
}

/// The complete role/profile policy surface, not only the advertised bundle.
/// Contract derivation uses this list so a tool that is temporarily omitted
/// from a frozen bundle remains authorized by the role policy and is still
/// recorded in `allowed_tools`.
pub(crate) fn role_tool_surface(role: AgentRole, profile: AgentProfile) -> Vec<String> {
    let mut names = crate::tool_bundle::BUNDLE_TOOL_CATALOG
        .iter()
        .map(|entry| entry.name.to_string())
        .filter(|name| role.allows_tool(profile, name))
        .collect::<Vec<_>>();
    if matches!(
        role,
        AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
    ) && !names.iter().any(|name| name == "mcp")
    {
        names.push("mcp".to_string());
    }
    // Runtime-managed tools that are dispatched outside the registry role
    // filter. They are part of the effective role surface even when
    // `allows_tool` only covers registry tools. Scout is deliberately excluded
    // from every write/delegation/artifact surface.
    if role != AgentRole::Scout {
        for name in [
            "send_message",
            "read_mailbox",
            "publish_artifact",
            "publish_artifact_file",
            "list_artifacts",
            "read_artifact",
            "archive_list",
            "media_info",
            "ask_user",
            "coordinate",
            "team_status",
            "use_skill",
            "tool_help",
            "tool_search",
            "task_detail",
            "budget_status",
            "read_tool_output",
            "remember",
            "recall",
            "search_sources",
            "read_source",
            "list_sources",
        ] {
            names.push(name.to_string());
        }
    } else {
        for name in [
            "read_tool_output",
            "recall",
            "team_status",
            "search_sources",
            "read_source",
            "list_sources",
        ] {
            names.push(name.to_string());
        }
    }
    match role {
        AgentRole::Verifier => names.push("submit_verdict".to_string()),
        AgentRole::Leader => {
            names.push("todo_write".to_string());
            names.push("report_progress".to_string());
            names.push("submit_team_plan".to_string());
            names.push("plan_fanout".to_string());
            names.push("run_team_plan".to_string());
            names.push("deliberate_team_plan".to_string());
        }
        AgentRole::Manager => {
            // Managers are read-only for workspace tools but may own
            // orchestration: delegate to Workers, delegate ACP work, run the
            // parallel batch, and scout. These are authority operations, not
            // workspace writes.
            for name in [
                "task_control",
                "delegate",
                "delegate_many",
                "delegate_acp",
                "scout",
                "report_progress",
            ] {
                names.push(name.to_string());
            }
        }
        AgentRole::Worker => {
            names.push("delegate".to_string());
            names.push("delegate_acp".to_string());
        }
        _ => {}
    }
    if matches!(
        role,
        AgentRole::Leader | AgentRole::Manager | AgentRole::Worker
    ) {
        names.push("wait_for_gate".to_string());
    }
    if matches!(
        role,
        AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
    ) {
        names.push("run_script".to_string());
    }
    names.sort();
    names.dedup();
    names
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum AgentProfile {
    Coding,
    Research,
    Verification,
    Generic,
}

impl AgentProfile {
    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "coding" => Some(Self::Coding),
            "research" => Some(Self::Research),
            "verification" => Some(Self::Verification),
            "generic" => Some(Self::Generic),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Coding => "coding",
            Self::Research => "research",
            Self::Verification => "verification",
            Self::Generic => "generic",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ContextStrategy {
    /// Provider summary followed by a rolling trim. Used for the Leader and
    /// Managers, where preserving intent and cross-task context matters.
    Summarize,
    /// Provider-free rolling trim. Used for Workers and Labour, where the
    /// current objective and recent tool results are more valuable than a
    /// verbose summary and the extra model call is avoidable.
    Trim,
}

fn context_strategy(role: AgentRole) -> ContextStrategy {
    match role {
        AgentRole::Leader | AgentRole::Manager => ContextStrategy::Summarize,
        AgentRole::Worker | AgentRole::Labour | AgentRole::Verifier | AgentRole::Scout => {
            ContextStrategy::Trim
        }
    }
}

#[derive(Default)]
struct CitationRegistry {
    by_chunk: HashMap<String, SourceCitation>,
    order: Vec<String>,
}

impl CitationRegistry {
    fn add_hit(&mut self, hit: &SourceSearchHit, origin: &str) -> String {
        if let Some(existing) = self.by_chunk.get(&hit.chunk_id) {
            return existing.key.clone();
        }
        let key = format!("c{}", self.order.len() + 1);
        let citation = SourceCitation {
            key: key.clone(),
            source_id: hit.source_id.clone(),
            source_title: hit.source_title.clone(),
            chunk_id: hit.chunk_id.clone(),
            chunk_no: hit.chunk_no,
            heading: hit.heading.clone(),
            excerpt: truncate_to(&hit.content, 320),
            score: hit.score,
            origin: origin.to_string(),
        };
        self.by_chunk.insert(hit.chunk_id.clone(), citation);
        self.order.push(hit.chunk_id.clone());
        key
    }

    fn list(&self) -> Vec<SourceCitation> {
        self.order
            .iter()
            .filter_map(|chunk_id| self.by_chunk.get(chunk_id).cloned())
            .collect()
    }
}

#[derive(Clone, Debug)]
struct AgentSpec {
    id: String,
    role: AgentRole,
    name: String,
    depth: u8,
    parent_name: Option<String>,
    title: Option<String>,
    task_id: Option<String>,
    department: Option<String>,
    /// Project identity that owns this run. Resolved once by the run loop for
    /// Leader/Manager/Worker; a Labour child inherits the Worker identity that
    /// owns its workspace so S6 attribution lands on the same component cell.
    identity_id: Option<String>,
    /// Generation of the owning identity at run start. Emitted on agent/tool
    /// events so OTel and diagnosis can attribute evidence without a DB read.
    identity_generation: Option<u64>,
    profile: AgentProfile,
    /// Plan task kind that selects the frozen tool bundle for a Worker or
    /// Labour run. `None` means the legacy full bundle for task agents and the
    /// role bundle for Leader/Manager/Verifier/Scout.
    task_kind: Option<String>,
    allow_delegation: bool,
    /// Skip routine tool prompts for an explicitly authorized internal run.
    /// Host-level operations are still denied, and the shell sandbox applies.
    auto_approve: bool,
    worktree: PathBuf,
    branch: String,
    session_worktree: PathBuf,
    session_branch: String,
    write_scope: Vec<String>,
    base_commit: String,
    /// Project workspace id when this Worker run is bound to a durable
    /// component workspace. `None` keeps the legacy task-worktree flow.
    workspace_id: Option<String>,
    /// Session workspace binding id. A Worker run owns the workspace worktree;
    /// a Labour run branches from it and merges back into it.
    session_workspace_id: Option<String>,
    /// Parent Worker session workspace binding id for a Labour child. `None`
    /// means the Labour run shares its parent worktree (legacy behavior).
    parent_workspace_id: Option<String>,
}

#[derive(Clone)]
struct McpToolBinding {
    exposed_name: String,
    server_id: String,
    tool_name: String,
    schema: ToolSchema,
    /// Resolved pin trust at discovery time. Only `Trusted` bindings are
    /// advertised/executable; unverified/drift rows remain visible through
    /// `tool_search` with `callable=false`.
    pin_trust: crate::contract::McpPinTrust,
}

const MAX_DYNAMIC_MCP_TOOLS: usize = 32;

const HARD_WORKER_CAP: usize = 10;

/// Concurrent read-only Scouts allowed per session run. Scout runs are cheap
/// but not free: the cap keeps a team from fanning out unbounded provider calls
/// while the session token budget and global Worker capacity still apply.
const SCOUT_CAP: usize = 3;

/// Hard bound on the final Scout brief returned to the parent. The full
/// transcript stays in the event stream; the parent receives this slice.
const SCOUT_SUMMARY_MAX_CHARS: usize = 6_000;

struct SessionBudget {
    limit: Option<u64>,
    cost_limit: Option<f64>,
    time_limit: Option<Duration>,
    /// Shared with the engine so an explicit unlimited toggle affects a live
    /// run at the next turn/tool boundary.
    unlimited: Arc<AtomicBool>,
    started: std::time::Instant,
    caps: AgentCaps,
    worker_cap: AtomicUsize,
    /// Shared Worker/Labour targets and live reservation counters. The tick
    /// updates this object; reservations see a target change atomically.
    scaling_slots: Option<Arc<ScalingSlots>>,
    state: Mutex<BudgetState>,
}

#[derive(Clone, Copy)]
struct AgentCaps {
    managers: usize,
    workers: usize,
    labour: usize,
}

struct BudgetState {
    spent: u64,
    /// Tokens held by in-flight provider calls. Reservations count against the
    /// limit before the request starts, so concurrent agents cannot all pass
    /// the same pre-flight check and overshoot by a full batch.
    reserved_tokens: u64,
    cost_spent: f64,
    reserved_cost: f64,
    managers: usize,
    workers: usize,
    verifiers: usize,
    labour: usize,
    scouts: usize,
    warned_tokens: bool,
    warned_cost: bool,
    warned_time: bool,
}

struct AgentSlot<'a> {
    budget: &'a SessionBudget,
    role: AgentRole,
}

impl Drop for AgentSlot<'_> {
    fn drop(&mut self) {
        self.budget.release(self.role);
    }
}

struct WorkerCapGuard<'a> {
    budget: &'a SessionBudget,
    original: usize,
}

impl Drop for WorkerCapGuard<'_> {
    fn drop(&mut self) {
        self.budget.set_worker_cap(self.original);
    }
}

struct TokenReservation<'a> {
    budget: &'a SessionBudget,
    amount: u64,
}

impl SessionBudget {
    fn new(limit: Option<u64>, spent: u64, caps: AgentCaps) -> Self {
        Self {
            limit,
            cost_limit: None,
            time_limit: None,
            unlimited: Arc::new(AtomicBool::new(false)),
            started: std::time::Instant::now(),
            caps,
            worker_cap: AtomicUsize::new(caps.workers),
            scaling_slots: None,
            state: Mutex::new(BudgetState {
                spent,
                reserved_tokens: 0,
                cost_spent: 0.0,
                reserved_cost: 0.0,
                managers: 0,
                workers: 0,
                verifiers: 0,
                labour: 0,
                scouts: 0,
                warned_tokens: false,
                warned_cost: false,
                warned_time: false,
            }),
        }
    }

    fn with_cost(mut self, limit: Option<f64>, spent: f64) -> Self {
        self.cost_limit = limit;
        if let Ok(mut state) = self.state.lock() {
            state.cost_spent = spent;
        }
        self
    }

    fn with_scaling_slots(mut self, slots: Arc<ScalingSlots>) -> Self {
        self.scaling_slots = Some(slots);
        self
    }

    fn with_time(mut self, seconds: Option<u64>) -> Self {
        self.time_limit = seconds.map(Duration::from_secs);
        self.started = std::time::Instant::now();
        self
    }

    fn with_unlimited_flag(mut self, flag: Arc<AtomicBool>) -> Self {
        self.unlimited = flag;
        self
    }

    fn is_unlimited(&self) -> bool {
        self.unlimited.load(Ordering::SeqCst)
    }

    fn run_snapshot(&self) -> RunBudgetSnapshot {
        RunBudgetSnapshot {
            token_limit: self.limit,
            tokens_spent: self.spent(),
            cost_limit_usd: self.cost_limit,
            cost_spent_usd: self.cost_spent(),
            time_limit_secs: self.time_limit.map(|limit| limit.as_secs()),
            elapsed_secs: self.elapsed_secs(),
            unlimited: self.is_unlimited(),
        }
    }

    fn add_cost(&self, cost: f64) -> bool {
        let unlimited = self.is_unlimited();
        if let Ok(mut state) = self.state.lock() {
            state.cost_spent += cost.max(0.0);
            return !unlimited
                && self
                    .cost_limit
                    .is_some_and(|limit| state.cost_spent >= limit);
        }
        true
    }

    fn exceeded_reason(&self) -> Option<&'static str> {
        if self.is_unlimited() {
            return None;
        }
        if self.limit.is_some_and(|limit| self.spent() >= limit) {
            return Some("tokens");
        }
        if let Ok(state) = self.state.lock()
            && self
                .cost_limit
                .is_some_and(|limit| state.cost_spent + state.reserved_cost >= limit)
        {
            return Some("cost");
        }
        if self
            .time_limit
            .is_some_and(|limit| self.started.elapsed() >= limit)
        {
            return Some("time");
        }
        None
    }

    fn warning(&self) -> Option<&'static str> {
        if self.is_unlimited() {
            return None;
        }
        let mut state = self.state.lock().ok()?;
        if let Some(limit) = self.limit
            && state.spent >= limit.saturating_mul(8) / 10
            && !state.warned_tokens
        {
            state.warned_tokens = true;
            return Some("tokens");
        }
        if let Some(limit) = self.cost_limit
            && state.cost_spent + state.reserved_cost >= limit * 0.8
            && !state.warned_cost
        {
            state.warned_cost = true;
            return Some("cost");
        }
        if let Some(limit) = self.time_limit
            && self.started.elapsed() >= limit.mul_f64(0.8)
            && !state.warned_time
        {
            state.warned_time = true;
            return Some("time");
        }
        None
    }

    fn spent(&self) -> u64 {
        self.state.lock().map(|state| state.spent).unwrap_or(0)
    }

    fn reserved_tokens(&self) -> u64 {
        self.state
            .lock()
            .map(|state| state.reserved_tokens)
            .unwrap_or(0)
    }

    fn exhausted(&self) -> bool {
        self.exceeded_reason().is_some()
    }

    fn cost_spent(&self) -> f64 {
        self.state
            .lock()
            .map(|state| state.cost_spent)
            .unwrap_or(0.0)
    }

    fn reserved_cost(&self) -> f64 {
        self.state
            .lock()
            .map(|state| state.reserved_cost)
            .unwrap_or(0.0)
    }

    fn elapsed_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    /// Exact instant at which the wall-clock budget expires, using the same
    /// `started` clock as `exceeded_reason`.
    fn wall_clock_deadline(&self) -> Option<Instant> {
        self.time_limit.map(|limit| self.started + limit)
    }

    fn worker_cap(&self) -> usize {
        if let Some(slots) = &self.scaling_slots {
            return slots.targets().0;
        }
        self.worker_cap.load(Ordering::SeqCst)
    }

    fn set_worker_cap(&self, cap: usize) {
        if let Some(slots) = &self.scaling_slots {
            slots.restore_target_if_unmanaged(AgentRole::Worker, cap);
            if slots.managed() {
                return;
            }
        }
        self.worker_cap
            .store(cap.clamp(1, HARD_WORKER_CAP), Ordering::SeqCst);
    }

    fn expand_workers(&self, target: usize) {
        let target = target.clamp(1, HARD_WORKER_CAP);
        if let Some(slots) = &self.scaling_slots {
            if slots.managed() {
                return;
            }
            slots.raise_target(AgentRole::Worker, target);
        }
        self.worker_cap.fetch_max(target, Ordering::SeqCst);
    }

    fn scaling_managed(&self) -> bool {
        self.scaling_slots
            .as_ref()
            .is_some_and(|slots| slots.managed())
    }

    fn can_expand_workers(&self) -> bool {
        if self.is_unlimited() {
            return true;
        }
        let Some(limit) = self.limit else {
            return true;
        };
        self.available_tokens()
            .is_some_and(|available| available >= limit.saturating_mul(20) / 100)
    }

    fn available_tokens(&self) -> Option<u64> {
        if self.is_unlimited() {
            return None;
        }
        let limit = self.limit?;
        let state = self.state.lock().ok()?;
        Some(
            limit
                .saturating_sub(state.spent)
                .saturating_sub(state.reserved_tokens),
        )
    }

    fn reserve_tokens(&self, amount: u64) -> Option<TokenReservation<'_>> {
        if self.is_unlimited() {
            return None;
        }
        let limit = self.limit?;
        let mut state = self.state.lock().ok()?;
        let committed = state.spent.saturating_add(state.reserved_tokens);
        if committed.saturating_add(amount) > limit {
            return None;
        }
        state.reserved_tokens = state.reserved_tokens.saturating_add(amount);
        drop(state);
        Some(TokenReservation {
            budget: self,
            amount,
        })
    }

    fn reserve_cost(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        prices: RolePrices,
    ) -> std::result::Result<Option<CostReservation<'_>>, ()> {
        if self.is_unlimited() {
            return Ok(None);
        }
        let Some(limit) = self.cost_limit else {
            return Ok(None);
        };
        // Reserve conservatively: the cache mix is unknown before the call, so
        // all input is reserved at the highest configured input-side price.
        let worst_input_price = prices.input.max(prices.cache_read).max(prices.cache_write);
        let estimate = usage_cost(
            input_tokens,
            output_tokens,
            0,
            0,
            RolePrices {
                input: worst_input_price,
                output: prices.output,
                cache_read: worst_input_price,
                cache_write: worst_input_price,
            },
        );
        if !estimate.is_finite() || estimate <= 0.0 {
            return Ok(None);
        }
        let mut state = self.state.lock().map_err(|_| ())?;
        let committed = state.cost_spent + state.reserved_cost;
        if committed + estimate > limit {
            return Err(());
        }
        state.reserved_cost += estimate;
        drop(state);
        Ok(Some(CostReservation {
            budget: self,
            amount: estimate,
        }))
    }

    fn reserve(&self, role: AgentRole) -> Option<usize> {
        if let Some(slots) = &self.scaling_slots
            && matches!(role, AgentRole::Worker | AgentRole::Labour)
        {
            return slots.try_reserve(role);
        }
        let mut state = self.state.lock().ok()?;
        let (counter, cap) = match role {
            AgentRole::Manager => (&mut state.managers, self.caps.managers),
            AgentRole::Worker => (&mut state.workers, self.worker_cap.load(Ordering::SeqCst)),
            AgentRole::Verifier => (&mut state.verifiers, 100),
            AgentRole::Labour => (&mut state.labour, self.caps.labour),
            AgentRole::Scout => (&mut state.scouts, SCOUT_CAP),
            AgentRole::Leader => return None,
        };
        if *counter >= cap {
            return None;
        }
        *counter += 1;
        Some(*counter)
    }

    fn reserve_slot(&self, role: AgentRole) -> Option<(usize, AgentSlot<'_>)> {
        let ordinal = self.reserve(role)?;
        Some((ordinal, AgentSlot { budget: self, role }))
    }

    fn release(&self, role: AgentRole) {
        if let Some(slots) = &self.scaling_slots
            && matches!(role, AgentRole::Worker | AgentRole::Labour)
        {
            slots.release(role);
            return;
        }
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let counter = match role {
            AgentRole::Manager => &mut state.managers,
            AgentRole::Worker => &mut state.workers,
            AgentRole::Verifier => &mut state.verifiers,
            AgentRole::Labour => &mut state.labour,
            AgentRole::Scout => &mut state.scouts,
            AgentRole::Leader => return,
        };
        *counter = counter.saturating_sub(1);
    }

    fn available(&self, role: AgentRole) -> usize {
        if let Some(slots) = &self.scaling_slots
            && matches!(role, AgentRole::Worker | AgentRole::Labour)
        {
            return slots.available(role);
        }
        let Ok(state) = self.state.lock() else {
            return 0;
        };
        let (active, cap) = match role {
            AgentRole::Manager => (state.managers, self.caps.managers),
            AgentRole::Worker => (state.workers, self.worker_cap.load(Ordering::SeqCst)),
            AgentRole::Verifier => (state.verifiers, 100),
            AgentRole::Labour => (state.labour, self.caps.labour),
            AgentRole::Scout => (state.scouts, SCOUT_CAP),
            AgentRole::Leader => return 0,
        };
        cap.saturating_sub(active)
    }
}

impl TokenReservation<'_> {
    /// Replace the pre-flight reservation with the provider's reported usage.
    /// Returns true when the limit is now reached.
    fn settle(mut self, actual: u64) -> bool {
        let amount = self.amount;
        let unlimited = self.budget.is_unlimited();
        self.amount = 0;
        let Ok(mut state) = self.budget.state.lock() else {
            return true;
        };
        state.reserved_tokens = state.reserved_tokens.saturating_sub(amount);
        state.spent = state.spent.saturating_add(actual);
        !unlimited && self.budget.limit.is_some_and(|limit| state.spent >= limit)
    }
}

impl Drop for TokenReservation<'_> {
    fn drop(&mut self) {
        if self.amount == 0 {
            return;
        }
        if let Ok(mut state) = self.budget.state.lock() {
            state.reserved_tokens = state.reserved_tokens.saturating_sub(self.amount);
        }
    }
}

struct CostReservation<'a> {
    budget: &'a SessionBudget,
    amount: f64,
}

impl CostReservation<'_> {
    fn settle(mut self, actual: f64) -> bool {
        let amount = self.amount;
        let unlimited = self.budget.is_unlimited();
        self.amount = 0.0;
        let Ok(mut state) = self.budget.state.lock() else {
            return true;
        };
        state.reserved_cost = (state.reserved_cost - amount).max(0.0);
        state.cost_spent += actual.max(0.0);
        !unlimited
            && self
                .budget
                .cost_limit
                .is_some_and(|limit| state.cost_spent >= limit)
    }
}

impl Drop for CostReservation<'_> {
    fn drop(&mut self) {
        if self.amount <= 0.0 {
            return;
        }
        if let Ok(mut state) = self.budget.state.lock() {
            state.reserved_cost = (state.reserved_cost - self.amount).max(0.0);
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct VerifierVerdict {
    pub(crate) approved: bool,
    pub(crate) summary: String,
}

#[derive(Clone, Debug)]
pub(crate) struct DeferredWait {
    pub gate_id: String,
    pub continuation_id: String,
    pub summary: String,
}

enum LoopOutcome {
    Final(String),
    Verdict(VerifierVerdict),
    Cancelled,
    BudgetExceeded(BudgetStop),
    /// A clean terminal handoff to a durable external-gate wait. The run loop
    /// stops before another provider call; the continuation scheduler owns the
    /// next run once the gate settles.
    Deferred(DeferredWait),
}

#[derive(Debug, Clone)]
struct BudgetStop {
    scope: String,
    reason: String,
    code: String,
    id: Option<String>,
    limit_tokens: Option<u64>,
    spent_tokens: u64,
    limit_cost: Option<f64>,
    spent_cost: f64,
    reserved_tokens: u64,
    reserved_cost: f64,
    protected_tokens: u64,
    elapsed_secs: Option<u64>,
}

impl BudgetStop {
    fn session(budget: &SessionBudget) -> Self {
        Self {
            scope: "session".to_string(),
            reason: budget.exceeded_reason().unwrap_or("tokens").to_string(),
            code: "BUDGET_EXHAUSTED".to_string(),
            id: None,
            limit_tokens: budget.limit,
            spent_tokens: budget.spent(),
            limit_cost: budget.cost_limit,
            spent_cost: budget.cost_spent(),
            reserved_tokens: 0,
            reserved_cost: budget.reserved_cost(),
            protected_tokens: 0,
            elapsed_secs: Some(budget.elapsed_secs()),
        }
    }

    fn from_rejection(rejection: Rejection) -> Self {
        Self {
            scope: rejection.scope.to_string(),
            reason: rejection.reason.as_str().to_string(),
            code: rejection.reason.code().to_string(),
            id: Some(rejection.id),
            limit_tokens: rejection.limit_tokens,
            spent_tokens: rejection.spent_tokens,
            limit_cost: rejection.limit_cost,
            spent_cost: rejection.spent_cost,
            reserved_tokens: rejection.reserved_tokens,
            reserved_cost: rejection.reserved_cost,
            protected_tokens: rejection.protected_tokens,
            elapsed_secs: None,
        }
    }

    /// A governor deferral is a task-scoped stop even when the reservation
    /// request touched a department/project pool: it must not abort the whole
    /// session, and the task stays retryable.
    fn from_governor_rejection(rejection: Rejection, task_id: Option<&str>) -> Self {
        let mut stop = Self::from_rejection(rejection);
        if matches!(stop.reason.as_str(), "reserved" | "throttled")
            && let Some(task_id) = task_id
        {
            stop.scope = "task".to_string();
            stop.id = Some(task_id.to_string());
        }
        stop
    }

    fn governor(
        reason: BudgetReason,
        task_id: Option<&str>,
        budget: &SessionBudget,
        protected_tokens: u64,
    ) -> Self {
        let (scope, id) = match task_id {
            Some(task) => ("task", Some(task.to_string())),
            None => ("session", None),
        };
        Self {
            scope: scope.to_string(),
            reason: reason.as_str().to_string(),
            code: reason.code().to_string(),
            id,
            limit_tokens: budget.limit,
            spent_tokens: budget.spent(),
            limit_cost: budget.cost_limit,
            spent_cost: budget.cost_spent(),
            reserved_tokens: budget.reserved_tokens(),
            reserved_cost: budget.reserved_cost(),
            protected_tokens,
            elapsed_secs: Some(budget.elapsed_secs()),
        }
    }

    fn session_tokens(budget: &SessionBudget) -> Self {
        Self {
            reason: "tokens".to_string(),
            code: "BUDGET_EXHAUSTED".to_string(),
            ..Self::session(budget)
        }
    }

    fn session_cost(budget: &SessionBudget) -> Self {
        Self {
            reason: "cost".to_string(),
            code: "BUDGET_EXHAUSTED".to_string(),
            ..Self::session(budget)
        }
    }

    fn is_abortive(&self) -> bool {
        self.scope == "session" || self.scope == "project"
    }

    fn payload(&self) -> serde_json::Value {
        serde_json::json!({
            "scope": self.scope,
            "reason": self.reason,
            "code": self.code,
            "id": self.id,
            "limit": self.limit_tokens,
            "spent": self.spent_tokens,
            "cost_limit_usd": self.limit_cost,
            "cost_spent_usd": self.spent_cost,
            "reserved_tokens": self.reserved_tokens,
            "reserved_cost_usd": self.reserved_cost,
            "protected_tokens": self.protected_tokens,
            "elapsed_secs": self.elapsed_secs,
        })
    }

    fn summary(&self) -> String {
        let scope = match (&self.scope[..], &self.id) {
            ("session", _) => "session".to_string(),
            (kind, Some(id)) => format!("{kind} {id}"),
            (kind, None) => kind.to_string(),
        };
        match self.reason.as_str() {
            "reserved" => {
                format!("{scope} deferred by the critical-path budget reserve")
            }
            "throttled" => {
                format!("{scope} deferred by the budget governor throttle")
            }
            reason => format!("{scope} {reason} budget exhausted"),
        }
    }
}

pub(crate) fn normalize_scope(scope: &str) -> String {
    scope
        .trim()
        .trim_start_matches("./")
        .trim_end_matches("/**")
        .trim_end_matches('/')
        .to_string()
}

pub(crate) fn path_in_scope(path: &str, scopes: &[String]) -> bool {
    if scopes.is_empty() {
        return true;
    }
    scopes.iter().any(|scope| {
        let scope = normalize_scope(scope);
        scope == "*" || scope == "**" || path == scope || path.starts_with(&format!("{scope}/"))
    })
}

pub(crate) fn scopes_overlap(left: &[String], right: &[String]) -> bool {
    if left.is_empty() || right.is_empty() {
        return true;
    }
    let is_unrestricted = |scope: &str| matches!(normalize_scope(scope).as_str(), "*" | "**");
    if left.iter().any(|scope| is_unrestricted(scope))
        || right.iter().any(|scope| is_unrestricted(scope))
    {
        return true;
    }
    left.iter().any(|a| {
        let a = normalize_scope(a);
        right.iter().any(|b| {
            let b = normalize_scope(b);
            a == b || a.starts_with(&format!("{b}/")) || b.starts_with(&format!("{a}/"))
        })
    })
}

pub(crate) fn shell_secret_roots(data_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut roots = vec![
        data_dir.join("verlinpark.db"),
        data_dir.join("verlinpark.db-wal"),
        data_dir.join("verlinpark.db-shm"),
        data_dir.join("verlinpark.db-journal"),
        data_dir.join("run"),
        data_dir.join("sessions"),
        data_dir.join("logs"),
        data_dir.join("artifacts"),
        // Daemon-owned runtime state that backs session-scoped APIs. Shell
        // must not read another session's spilled tool outputs, process logs,
        // verification clones, scratch, or derived archive/media scratch.
        data_dir.join("tool-outputs"),
        data_dir.join("processes"),
        data_dir.join("verify"),
        data_dir.join("sandbox"),
        data_dir.join("archive-extracts"),
        data_dir.join("media-transcode"),
        // Sibling session/task worktrees and daemon temp/import scratch. The
        // current agent's own worktree, scratch, and Git metadata are
        // re-allowed after the denies in `tools::sandboxed_command`.
        data_dir.join("worktrees"),
        data_dir.join("tmp"),
        // Backup signing keyring; it is 0600 for the daemon, not for a shell.
        data_dir.join("backup-keys"),
        data_dir.join("projects.jsonl"),
        data_dir.join("projects.jsonl.tmp"),
    ];
    // Rebuild backups use a timestamped suffix; deny every sidecar/backup
    // rather than only the live database file.
    if let Ok(entries) = std::fs::read_dir(data_dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("verlinpark.db.pre-rebuild-") {
                roots.push(entry.path());
            }
        }
    }
    if let Some(home) = std::env::var_os("HOME") {
        let home = PathBuf::from(home);
        for relative in [
            ".ssh",
            ".gnupg",
            ".aws",
            ".azure",
            ".config/gh",
            ".config/gcloud",
            ".config/op",
            ".config/1Password",
            ".kube",
            ".docker",
            ".password-store",
            "Library/Keychains",
            ".local/share/keyrings",
            ".netrc",
            ".git-credentials",
            ".npmrc",
            ".pypirc",
            ".cargo/credentials",
            ".cargo/credentials.toml",
        ] {
            roots.push(home.join(relative));
        }
    }
    roots
}

#[derive(Debug, Default, Clone)]
struct PlanBudgetScope {
    task_id: Option<String>,
    department: Option<String>,
    project_id: Option<String>,
    task_tokens: Option<u64>,
    task_cost: Option<f64>,
    department_tokens: Option<u64>,
    department_cost: Option<f64>,
    project_tokens: Option<u64>,
    project_cost: Option<f64>,
}

impl PlanBudgetScope {
    fn from_plan(
        plan: Option<&TeamPlan>,
        spec: &AgentSpec,
        project_budget: Option<&vp_protocol::types::ProjectBudget>,
        project_id: &str,
    ) -> Self {
        let task = spec.task_id.as_deref().and_then(|task_id| {
            plan.and_then(|plan| plan.tasks.iter().find(|task| task.id == task_id))
        });
        let department = spec.department.as_deref().and_then(|name| {
            plan.and_then(|plan| {
                plan.departments
                    .iter()
                    .find(|department| department.name == name)
            })
        });
        let project_tokens = project_budget.and_then(|budget| budget.token_budget);
        let project_cost = project_budget.and_then(|budget| budget.cost_budget_usd);
        Self {
            task_id: spec.task_id.clone(),
            department: spec.department.clone(),
            project_id: (project_tokens.is_some() || project_cost.is_some())
                .then(|| project_id.to_string()),
            task_tokens: task.and_then(|task| task.token_budget),
            task_cost: task.and_then(|task| task.cost_budget_usd),
            department_tokens: department.and_then(|department| department.token_budget),
            department_cost: department.and_then(|department| department.cost_budget_usd),
            project_tokens,
            project_cost,
        }
    }

    fn active(&self) -> bool {
        self.task_tokens.is_some()
            || self.task_cost.is_some()
            || self.department_tokens.is_some()
            || self.department_cost.is_some()
            || self.project_tokens.is_some()
            || self.project_cost.is_some()
    }

    fn project_active(&self) -> bool {
        self.project_tokens.is_some() || self.project_cost.is_some()
    }

    fn stop(
        &self,
        scope: &str,
        id: &str,
        reason: &str,
        limit_tokens: Option<u64>,
        spent: (u64, f64),
        limit_cost: Option<f64>,
    ) -> BudgetStop {
        BudgetStop {
            scope: scope.to_string(),
            reason: reason.to_string(),
            code: "BUDGET_EXHAUSTED".to_string(),
            id: Some(id.to_string()),
            limit_tokens,
            spent_tokens: spent.0,
            limit_cost,
            spent_cost: spent.1,
            reserved_tokens: 0,
            reserved_cost: 0.0,
            protected_tokens: 0,
            elapsed_secs: None,
        }
    }

    fn requests(
        &self,
        session_id: &str,
        task: (u64, f64),
        department: (u64, f64),
        project: (u64, f64),
        input_price: f64,
        output_price: f64,
    ) -> Vec<ScopeRequest> {
        let mut requests = Vec::new();
        if (self.task_tokens.is_some() || self.task_cost.is_some())
            && let Some(id) = &self.task_id
        {
            requests.push(ScopeRequest {
                pool_key: format!("session:{session_id}:task:{id}"),
                scope: "task",
                id: id.clone(),
                limit_tokens: self.task_tokens,
                limit_cost: self.task_cost,
                observed: Spend {
                    tokens: task.0,
                    cost_usd: task.1,
                },
                input_price_per_m: input_price,
                output_price_per_m: output_price,
            });
        }
        if (self.department_tokens.is_some() || self.department_cost.is_some())
            && let Some(id) = &self.department
        {
            requests.push(ScopeRequest {
                pool_key: format!("session:{session_id}:department:{id}"),
                scope: "department",
                id: id.clone(),
                limit_tokens: self.department_tokens,
                limit_cost: self.department_cost,
                observed: Spend {
                    tokens: department.0,
                    cost_usd: department.1,
                },
                input_price_per_m: input_price,
                output_price_per_m: output_price,
            });
        }
        if (self.project_tokens.is_some() || self.project_cost.is_some())
            && let Some(id) = &self.project_id
        {
            requests.push(ScopeRequest {
                pool_key: format!("project:{id}"),
                scope: "project",
                id: id.clone(),
                limit_tokens: self.project_tokens,
                limit_cost: self.project_cost,
                observed: Spend {
                    tokens: project.0,
                    cost_usd: project.1,
                },
                input_price_per_m: input_price,
                output_price_per_m: output_price,
            });
        }
        requests
    }

    fn project_warning(&self, project: (u64, f64)) -> Option<BudgetStop> {
        if let Some(limit) = self.project_tokens
            && project.0 >= limit.saturating_mul(8) / 10
        {
            return Some(self.stop(
                "project",
                self.project_id.as_deref().unwrap_or("?"),
                "tokens",
                Some(limit),
                project,
                self.project_cost,
            ));
        }
        if let Some(limit) = self.project_cost
            && project.1 >= limit * 0.8
        {
            return Some(self.stop(
                "project",
                self.project_id.as_deref().unwrap_or("?"),
                "cost",
                self.project_tokens,
                project,
                Some(limit),
            ));
        }
        None
    }

    fn after(
        &self,
        task: (u64, f64),
        department: (u64, f64),
        project: (u64, f64),
    ) -> Option<BudgetStop> {
        if let Some(limit) = self.project_tokens
            && project.0 >= limit
        {
            return Some(self.stop(
                "project",
                self.project_id.as_deref().unwrap_or("?"),
                "tokens",
                Some(limit),
                project,
                self.project_cost,
            ));
        }
        if let Some(limit) = self.project_cost
            && project.1 >= limit
        {
            return Some(self.stop(
                "project",
                self.project_id.as_deref().unwrap_or("?"),
                "cost",
                self.project_tokens,
                project,
                Some(limit),
            ));
        }
        if let Some(limit) = self.task_tokens
            && task.0 >= limit
        {
            return Some(self.stop(
                "task",
                self.task_id.as_deref().unwrap_or("?"),
                "tokens",
                Some(limit),
                task,
                self.task_cost,
            ));
        }
        if let Some(limit) = self.task_cost
            && task.1 >= limit
        {
            return Some(self.stop(
                "task",
                self.task_id.as_deref().unwrap_or("?"),
                "cost",
                self.task_tokens,
                task,
                Some(limit),
            ));
        }
        if let Some(limit) = self.department_tokens
            && department.0 >= limit
        {
            return Some(self.stop(
                "department",
                self.department.as_deref().unwrap_or("?"),
                "tokens",
                Some(limit),
                department,
                self.department_cost,
            ));
        }
        if let Some(limit) = self.department_cost
            && department.1 >= limit
        {
            return Some(self.stop(
                "department",
                self.department.as_deref().unwrap_or("?"),
                "cost",
                self.department_tokens,
                department,
                Some(limit),
            ));
        }
        None
    }
}

fn model_for(config: &AgentConfig, role: AgentRole) -> String {
    config
        .role_models
        .for_role(role.as_str())
        .model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .unwrap_or(&config.model)
        .to_string()
}

fn context_window_for(config: &AgentConfig, role: AgentRole) -> u64 {
    config
        .role_models
        .for_role(role.as_str())
        .context_window
        .filter(|value| *value > 0)
        .unwrap_or(config.context_window)
        .min(10_000_000)
}

/// Derive the summarization threshold and the hard total-request limit from
/// the model window and output reserve. The caller's estimate includes the
/// system prompt and tool schemas, so they are not deducted a second time.
///
/// L4 replaces the old 75%-of-hard-limit default with the cache-lossless
/// window policy: 90% of the configured model window, bounded by the hard
/// limit. A 1M-token DeepSeek window therefore compacts around 943k tokens,
/// not around 786k, and not at a fixed 24k. `VERLINPARK_COMPACT_AFTER_TOKENS`
/// remains an explicit override and is only clamped to the hard limit.
fn context_limits(config: &AgentConfig, role: AgentRole) -> (u64, u64) {
    let policy = context_compaction_policy(config, role);
    (policy.threshold, policy.hard_limit)
}

fn context_compaction_policy(
    config: &AgentConfig,
    role: AgentRole,
) -> crate::context::CompactionPolicy {
    let window = context_window_for(config, role);
    let output = u64::from(config.max_tokens.unwrap_or(4096));
    crate::context::CompactionPolicy::derive(window, output, config.compact_after_tokens)
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct RolePrices {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
}

fn role_prices(config: &AgentConfig, role: AgentRole) -> RolePrices {
    let price = config.role_models.for_role(role.as_str());
    let input = price.input_usd_per_million.unwrap_or(0.0);
    RolePrices {
        input,
        output: price.output_usd_per_million.unwrap_or(0.0),
        // Unset cache prices fall back to the full input price. This keeps
        // existing configs conservative and preserves their previous cost.
        cache_read: price.cache_read_usd_per_million.unwrap_or(input),
        cache_write: price.cache_write_usd_per_million.unwrap_or(input),
    }
}

/// Price one provider call. `input` is the total input and `cache_read` /
/// `cache_write` are subsets of it. The cached and uncached portions are
/// disjoint, so no token is billed twice.
fn usage_cost(
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    prices: RolePrices,
) -> f64 {
    let cache_read = cache_read.min(input);
    let cache_write = cache_write.min(input.saturating_sub(cache_read));
    let uncached = input.saturating_sub(cache_read).saturating_sub(cache_write);
    let per_million = |tokens: u64, price: f64| (tokens as f64 / 1_000_000.0) * price;
    per_million(uncached, prices.input)
        + per_million(cache_read, prices.cache_read)
        + per_million(cache_write, prices.cache_write)
        + per_million(output, prices.output)
}

fn truncate(content: &str) -> String {
    const MAX: usize = 8_000;
    if content.len() > MAX {
        // Byte slicing at a fixed offset can split a UTF-8 character and
        // panic. Walk back to the nearest character boundary instead.
        let mut end = MAX;
        while end > 0 && !content.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}… (truncated)", &content[..end])
    } else {
        content.to_string()
    }
}

use context::*;
use delegation::*;
use dispatch::*;
use merge::*;
use plan::*;
use provider::*;
use run_loop::*;
use schemas::*;
use scout::*;
use script::*;
use support::*;
use tools::*;

#[cfg(test)]
#[path = "agent/tests.rs"]
mod tests;
