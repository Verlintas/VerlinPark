use std::collections::{HashMap, HashSet, VecDeque};
use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use async_recursion::async_recursion;
use async_trait::async_trait;

use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::sync::{oneshot, watch};
use tokio::time::timeout;
use vp_protocol::types::{
    AgentInfo, AgentProbeResult, ApprovalMode, CapacityInfo, ConfigureAgentParams,
    ContextCompactionSummary, ContextMeasurement, IntakeDispatchResult, IntakeTask,
    MergeQueueEntry, PlanDepartment, PlanRevisionDetailResult, PlanTask, Project,
    ReviseTeamPlanResult, RoleModels, ScheduledTask, SchedulerProjectStatus, SchedulerStatusResult,
    Session, SessionCheckpoint, SessionCheckpointDiffResult, SessionCheckpointRestoreResult,
    SessionCompactResult, SessionForkResult, SessionMergeForkResult, Skill, SourceCitation,
    SourceSearchHit, StoredMessage, TeamPlan, TodoItem,
};
use vp_providers::{
    AnthropicProvider, ChatMessage, ChatRequest, OpenAiCompatibleProvider, Provider, ProviderError,
    StreamEvent, ToolCall, ToolSchema,
};
use vp_store::Store;

use crate::Engine;
use crate::acp;
use crate::budget::{BudgetReason, Rejection, ScopeRequest, Spend};
use crate::permissions::{
    ApprovalDecision, RiskLevel, assess_acp_permission, assess_tool_call, decide,
};
use crate::tools::{ToolContext, ToolOutput, ToolRegistry};
use crate::worktree;

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
    fn new(workers: usize, labour: usize) -> Self {
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
            AgentRole::Worker => self.workers.try_acquire(session_id),
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
            AgentRole::Worker => self.workers.acquire(session_id, priority, control).await,
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
    global_slots: Arc<AdmissionGate>,
    capacity: Arc<CapacityController>,
}

struct RunControl {
    cancelled: watch::Sender<bool>,
    paused: watch::Sender<bool>,
    finalized: tokio::sync::Mutex<Option<String>>,
    manual: bool,
    cancellable: bool,
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
        }
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
        if control.is_cancelled() {
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
        if control.is_cancelled() {
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
    if control.is_cancelled() {
        return;
    }
    let mut cancelled = control.subscribe();
    loop {
        if cancelled.changed().await.is_err() || control.is_cancelled() {
            return;
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum AgentRole {
    Leader,
    Manager,
    Worker,
    Verifier,
    Labour,
}

impl AgentRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::Leader => "leader",
            Self::Manager => "manager",
            Self::Worker => "worker",
            Self::Verifier => "verifier",
            Self::Labour => "labour",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "leader" => Some(Self::Leader),
            "manager" => Some(Self::Manager),
            "worker" => Some(Self::Worker),
            "verifier" => Some(Self::Verifier),
            "labour" => Some(Self::Labour),
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
        }
    }

    fn child_role(self) -> Option<Self> {
        match self {
            Self::Leader => Some(Self::Manager),
            Self::Manager => Some(Self::Worker),
            Self::Worker => Some(Self::Labour),
            Self::Verifier | Self::Labour => None,
        }
    }

    fn allows_tool(self, profile: AgentProfile, name: &str) -> bool {
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
                        | "data_query"
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
                        | "data_query"
                        | "shell"
                        | "run_project_task"
                        | "code_diagnostics"
                        | "process"
                        | "wait_for"
                        | "local_http"
                ) || (profile == AgentProfile::Research
                    && matches!(name, "fetch_url" | "web_search"))
            }
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
                        | "data_query"
                        | "code_diagnostics"
                        | "process"
                        | "wait_for"
                        | "local_http"
                        | "browser"
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
                        | "data_query"
                        | "web_search"
                        | "local_http"
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
                        | "data_query"
                        | "code_diagnostics"
                        | "process"
                        | "wait_for"
                        | "local_http"
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
                        | "data_query"
                ),
            },
        }
    }
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
        AgentRole::Worker | AgentRole::Labour | AgentRole::Verifier => ContextStrategy::Trim,
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
    profile: AgentProfile,
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
}

#[derive(Clone)]
struct McpToolBinding {
    exposed_name: String,
    server_id: String,
    tool_name: String,
    schema: ToolSchema,
}

const MAX_DYNAMIC_MCP_TOOLS: usize = 32;

const HARD_WORKER_CAP: usize = 10;

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
            state: Mutex::new(BudgetState {
                spent,
                reserved_tokens: 0,
                cost_spent: 0.0,
                reserved_cost: 0.0,
                managers: 0,
                workers: 0,
                verifiers: 0,
                labour: 0,
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

    fn worker_cap(&self) -> usize {
        self.worker_cap.load(Ordering::SeqCst)
    }

    fn set_worker_cap(&self, cap: usize) {
        self.worker_cap
            .store(cap.clamp(1, HARD_WORKER_CAP), Ordering::SeqCst);
    }

    fn expand_workers(&self, target: usize) {
        let target = target.clamp(1, HARD_WORKER_CAP);
        self.worker_cap.fetch_max(target, Ordering::SeqCst);
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
        input_price: f64,
        output_price: f64,
    ) -> std::result::Result<Option<CostReservation<'_>>, ()> {
        if self.is_unlimited() {
            return Ok(None);
        }
        let Some(limit) = self.cost_limit else {
            return Ok(None);
        };
        let estimate = usage_cost(input_tokens, output_tokens, input_price, output_price);
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
        let mut state = self.state.lock().ok()?;
        let (counter, cap) = match role {
            AgentRole::Manager => (&mut state.managers, self.caps.managers),
            AgentRole::Worker => (&mut state.workers, self.worker_cap.load(Ordering::SeqCst)),
            AgentRole::Verifier => (&mut state.verifiers, 100),
            AgentRole::Labour => (&mut state.labour, self.caps.labour),
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
        let Ok(mut state) = self.state.lock() else {
            return;
        };
        let counter = match role {
            AgentRole::Manager => &mut state.managers,
            AgentRole::Worker => &mut state.workers,
            AgentRole::Verifier => &mut state.verifiers,
            AgentRole::Labour => &mut state.labour,
            AgentRole::Leader => return,
        };
        *counter = counter.saturating_sub(1);
    }

    fn available(&self, role: AgentRole) -> usize {
        let Ok(state) = self.state.lock() else {
            return 0;
        };
        let (active, cap) = match role {
            AgentRole::Manager => (state.managers, self.caps.managers),
            AgentRole::Worker => (state.workers, self.worker_cap.load(Ordering::SeqCst)),
            AgentRole::Verifier => (state.verifiers, 100),
            AgentRole::Labour => (state.labour, self.caps.labour),
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
struct VerifierVerdict {
    approved: bool,
    summary: String,
}

enum LoopOutcome {
    Final(String),
    Verdict(VerifierVerdict),
    Cancelled,
    BudgetExceeded(BudgetStop),
}

#[derive(Debug, Clone)]
struct BudgetStop {
    scope: String,
    reason: String,
    id: Option<String>,
    limit_tokens: Option<u64>,
    spent_tokens: u64,
    limit_cost: Option<f64>,
    spent_cost: f64,
    reserved_tokens: u64,
    reserved_cost: f64,
    elapsed_secs: Option<u64>,
}

impl BudgetStop {
    fn session(budget: &SessionBudget) -> Self {
        Self {
            scope: "session".to_string(),
            reason: budget.exceeded_reason().unwrap_or("tokens").to_string(),
            id: None,
            limit_tokens: budget.limit,
            spent_tokens: budget.spent(),
            limit_cost: budget.cost_limit,
            spent_cost: budget.cost_spent(),
            reserved_tokens: 0,
            reserved_cost: budget.reserved_cost(),
            elapsed_secs: Some(budget.elapsed_secs()),
        }
    }

    fn from_rejection(rejection: Rejection) -> Self {
        Self {
            scope: rejection.scope.to_string(),
            reason: match rejection.reason {
                BudgetReason::Tokens => "tokens",
                BudgetReason::Cost => "cost",
            }
            .to_string(),
            id: Some(rejection.id),
            limit_tokens: rejection.limit_tokens,
            spent_tokens: rejection.spent_tokens,
            limit_cost: rejection.limit_cost,
            spent_cost: rejection.spent_cost,
            reserved_tokens: rejection.reserved_tokens,
            reserved_cost: rejection.reserved_cost,
            elapsed_secs: None,
        }
    }

    fn session_tokens(budget: &SessionBudget) -> Self {
        Self {
            reason: "tokens".to_string(),
            ..Self::session(budget)
        }
    }

    fn session_cost(budget: &SessionBudget) -> Self {
        Self {
            reason: "cost".to_string(),
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
            "id": self.id,
            "limit": self.limit_tokens,
            "spent": self.spent_tokens,
            "cost_limit_usd": self.limit_cost,
            "cost_spent_usd": self.spent_cost,
            "reserved_tokens": self.reserved_tokens,
            "reserved_cost_usd": self.reserved_cost,
            "elapsed_secs": self.elapsed_secs,
        })
    }

    fn summary(&self) -> String {
        let scope = match (&self.scope[..], &self.id) {
            ("session", _) => "session".to_string(),
            (kind, Some(id)) => format!("{kind} {id}"),
            (kind, None) => kind.to_string(),
        };
        format!("{scope} {} budget exhausted", self.reason)
    }
}

fn agent_fields(payload: &mut serde_json::Value, spec: &AgentSpec) {
    if let Some(object) = payload.as_object_mut() {
        object.insert("agent_id".into(), serde_json::json!(spec.id));
        object.insert("agent_role".into(), serde_json::json!(spec.role.as_str()));
        object.insert("agent_name".into(), serde_json::json!(spec.name));
        object.insert("agent_depth".into(), serde_json::json!(spec.depth));
        object.insert(
            "agent_profile".into(),
            serde_json::json!(spec.profile.as_str()),
        );
        if let Some(title) = &spec.title {
            object.insert("agent_title".into(), serde_json::json!(title));
        }
    }
}

async fn emit(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    kind: &str,
    mut payload: serde_json::Value,
) -> Result<()> {
    agent_fields(&mut payload, spec);
    engine.append_event(session_id, kind, payload).await?;
    Ok(())
}

impl AgentService {
    pub fn from_env(engine: Engine) -> Self {
        let base_url = env::var("VERLINPARK_BASE_URL")
            .ok()
            .or_else(|| env::var("OPENAI_BASE_URL").ok());
        let api_key = env::var("VERLINPARK_API_KEY")
            .ok()
            .or_else(|| env::var("OPENAI_API_KEY").ok());
        let model = env::var("VERLINPARK_MODEL")
            .ok()
            .or_else(|| env::var("OPENAI_MODEL").ok());

        if base_url.is_none() && api_key.is_none() {
            return Self::unconfigured(engine);
        }

        let base_url = base_url.unwrap_or_else(|| "https://api.openai.com/v1".to_string());
        let model = model.unwrap_or_else(|| "gpt-4o-mini".to_string());
        let provider = Arc::new(OpenAiCompatibleProvider::new(
            "openai-compatible",
            base_url.clone(),
            api_key,
        ));
        let scope_store = engine.store().clone();
        Self::configured(
            engine,
            AgentConfig {
                provider,
                provider_name: "openai-compatible".to_string(),
                model,
                role_models: RoleModels::default(),
                base_url: Some(base_url),
                max_turns: env::var("VERLINPARK_MAX_TURNS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(20),
                max_tokens: env::var("VERLINPARK_MAX_TOKENS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .or(Some(4096)),
                max_managers: env::var("VERLINPARK_MAX_MANAGERS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(5),
                max_workers: env::var("VERLINPARK_MAX_WORKERS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(10),
                max_labour: env::var("VERLINPARK_MAX_LABOUR")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(40),
                auto_expand_workers: auto_expand_workers(),
                integration_command: env::var("VERLINPARK_INTEGRATION_COMMAND")
                    .ok()
                    .filter(|command| !command.trim().is_empty()),
                tools: ToolRegistry::default_tools(),
                max_active_sessions: global_session_cap(),
                scope_locks: Arc::new(ScopeLocks::with_store(scope_store)),
                capacity: Arc::new(CapacityController::new(
                    global_worker_cap(),
                    global_labour_cap(),
                )),
                provider_idle_timeout_secs: env::var("VERLINPARK_PROVIDER_IDLE_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(300)
                    .clamp(10, 3600),
                question_timeout_secs: env::var("VERLINPARK_QUESTION_TIMEOUT_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(1800)
                    .clamp(60, 86400),
                approval_mode: env::var("VERLINPARK_APPROVAL_MODE")
                    .ok()
                    .and_then(|value| ApprovalMode::parse(&value))
                    .unwrap_or_default(),
                approval_timeout_secs: env::var("VERLINPARK_APPROVAL_TIMEOUT_SECS")
                    .ok()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(600)
                    .clamp(10, 86400),
                compact_after_tokens: compact_after_tokens(),
                compact_keep_messages: compact_keep_messages(),
                context_window: context_window(),
            },
        )
    }

    pub fn unconfigured(engine: Engine) -> Self {
        Self {
            engine,
            config: Arc::new(RwLock::new(None)),
            runs: Arc::new(Mutex::new(HashMap::new())),
            auto_resolver_started: Arc::new(AtomicBool::new(false)),
            task_scheduler_started: Arc::new(AtomicBool::new(false)),
            global_slots: Arc::new(AdmissionGate::new(global_session_cap())),
            capacity: Arc::new(CapacityController::new(
                global_worker_cap(),
                global_labour_cap(),
            )),
        }
    }

    pub fn configured(engine: Engine, config: AgentConfig) -> Self {
        let global_cap = config.max_active_sessions.max(1);
        let capacity = config.capacity.clone();
        Self {
            engine,
            config: Arc::new(RwLock::new(Some(config))),
            runs: Arc::new(Mutex::new(HashMap::new())),
            auto_resolver_started: Arc::new(AtomicBool::new(false)),
            task_scheduler_started: Arc::new(AtomicBool::new(false)),
            global_slots: Arc::new(AdmissionGate::new(global_cap)),
            capacity,
        }
    }

    pub fn configure(&self, params: ConfigureAgentParams) -> Result<AgentInfo> {
        let model = params.model.trim().to_string();
        if model.is_empty() {
            return Err(anyhow!("model is required"));
        }
        let base_url = params
            .base_url
            .map(|url| url.trim().trim_end_matches('/').to_string())
            .filter(|url| !url.is_empty())
            .unwrap_or_else(|| match params.provider.as_str() {
                "anthropic" => "https://api.anthropic.com/v1".to_string(),
                _ => "https://api.openai.com/v1".to_string(),
            });
        let provider: Arc<dyn Provider> = match params.provider.as_str() {
            "openai-compatible" => Arc::new(OpenAiCompatibleProvider::new(
                "openai-compatible",
                base_url.clone(),
                params.api_key,
            )),
            "anthropic" => Arc::new(AnthropicProvider::with_base_url(
                "anthropic",
                base_url.clone(),
                params.api_key,
            )),
            other => return Err(anyhow!("unsupported provider: {other}")),
        };
        let scope_locks = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().map(|config| config.scope_locks.clone()))
            .unwrap_or_else(|| Arc::new(ScopeLocks::with_store(self.engine.store().clone())));
        let config = AgentConfig {
            provider,
            provider_name: params.provider,
            model,
            role_models: params.role_models.unwrap_or_default(),
            base_url: Some(base_url),
            max_turns: params.max_turns.unwrap_or(20).clamp(1, 200),
            max_tokens: params.max_tokens.or(Some(4096)),
            max_managers: params.max_managers.unwrap_or(5).clamp(1, 5) as usize,
            max_workers: params.max_workers.unwrap_or(10).clamp(1, 10) as usize,
            max_labour: params.max_labour.unwrap_or(40).clamp(1, 40) as usize,
            auto_expand_workers: params.auto_expand_workers.unwrap_or(false),
            integration_command: env::var("VERLINPARK_INTEGRATION_COMMAND")
                .ok()
                .filter(|command| !command.trim().is_empty()),
            tools: ToolRegistry::default_tools(),
            max_active_sessions: global_session_cap(),
            scope_locks,
            capacity: self.capacity.clone(),
            provider_idle_timeout_secs: env::var("VERLINPARK_PROVIDER_IDLE_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(300)
                .clamp(10, 3600),
            question_timeout_secs: env::var("VERLINPARK_QUESTION_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(1800)
                .clamp(60, 86400),
            approval_mode: params
                .approval_mode
                .or_else(|| {
                    env::var("VERLINPARK_APPROVAL_MODE")
                        .ok()
                        .and_then(|value| ApprovalMode::parse(&value))
                })
                .unwrap_or_default(),
            approval_timeout_secs: env::var("VERLINPARK_APPROVAL_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(600)
                .clamp(10, 86400),
            compact_after_tokens: compact_after_tokens(),
            compact_keep_messages: compact_keep_messages(),
            context_window: params
                .context_window
                .map(|value| value.min(10_000_000))
                .unwrap_or_else(context_window),
        };
        *self
            .config
            .write()
            .map_err(|_| anyhow!("agent config lock poisoned"))? = Some(config);
        Ok(self.info())
    }

    pub fn info(&self) -> AgentInfo {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        match config {
            Some(config) => AgentInfo {
                configured: true,
                provider: Some(config.provider_name.clone()),
                model: Some(config.model.clone()),
                base_url: config.base_url.clone(),
                role_models: config.role_models.clone(),
                capacity: self.capacity.snapshot(),
                context_window: Some(config.context_window),
                approval_mode: config.approval_mode,
            },
            None => AgentInfo {
                configured: false,
                provider: None,
                model: None,
                base_url: None,
                role_models: RoleModels::default(),
                capacity: self.capacity.snapshot(),
                context_window: None,
                approval_mode: ApprovalMode::default(),
            },
        }
    }

    pub async fn probe(&self) -> Result<AgentProbeResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let started = std::time::Instant::now();
        // Reasoning models can spend hundreds of tokens before emitting their
        // first visible content token. Keep the probe small but large enough to
        // distinguish a healthy reasoning endpoint from a dead one.
        let probe_tokens = config.max_tokens.unwrap_or(1024).clamp(1024, 2048);
        let request = ChatRequest {
            model: config.model.clone(),
            messages: vec![ChatMessage::user("Reply with the single word: OK")],
            system: Some("You are a connection probe. Reply concisely.".to_string()),
            max_tokens: Some(probe_tokens),
            temperature: Some(0.0),
            tools: Vec::new(),
        };
        let mut stream = timeout(
            Duration::from_secs(60),
            config.provider.stream_chat(request),
        )
        .await
        .map_err(|_| anyhow!("provider probe timed out"))?
        .context("starting provider probe")?;
        let mut text = String::new();
        let mut usage = (0u64, 0u64);
        let mut chunks = 0u32;
        let mut first_token_ms = 0u64;
        let mut done = false;
        while let Some(item) = timeout(Duration::from_secs(60), stream.next())
            .await
            .map_err(|_| anyhow!("provider probe stream timed out"))?
        {
            match item? {
                StreamEvent::TextDelta(delta) => {
                    if !delta.is_empty() {
                        if chunks == 0 {
                            first_token_ms =
                                started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                        }
                        chunks = chunks.saturating_add(1);
                        text.push_str(&delta);
                    }
                }
                StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    usage.0 = usage.0.saturating_add(input_tokens as u64);
                    usage.1 = usage.1.saturating_add(output_tokens as u64);
                }
                StreamEvent::Done => done = true,
                StreamEvent::ToolCall(_) => {}
            }
        }
        if text.trim().is_empty() {
            anyhow::bail!("provider probe returned no text");
        }
        Ok(AgentProbeResult {
            provider: config.provider_name,
            model: config.model,
            latency_ms: started.elapsed().as_millis().min(u64::MAX as u128) as u64,
            first_token_ms,
            chunks,
            done,
            input_tokens: usage.0,
            output_tokens: usage.1,
            text: truncate(&text),
        })
    }

    pub async fn retry_merge(&self, entry_id: &str) -> Result<MergeQueueEntry> {
        let entry = self
            .engine
            .get_merge_queue_entry(entry_id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry not found: {entry_id}"))?;
        let (session, _) =
            load_session_project(self.engine.store().clone(), &entry.session_id).await?;
        crate::ensure_session_quiescent(&session, "retry a merge")?;
        if session.status == "landed" {
            anyhow::bail!("session is already landed");
        }
        self.begin_operation(&entry.session_id)?;
        let result = self.retry_merge_inner(entry_id).await;
        self.end_operation(&entry.session_id);
        result
    }

    async fn retry_merge_inner(&self, entry_id: &str) -> Result<MergeQueueEntry> {
        let entry = self
            .engine
            .get_merge_queue_entry(entry_id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry not found: {entry_id}"))?;
        if !matches!(entry.status.as_str(), "conflict" | "failed" | "interrupted") {
            return Err(anyhow!(
                "merge queue entry is {}, only conflict/failed/interrupted can be retried",
                entry.status
            ));
        }
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), &entry.session_id).await?;
        let agents = self.engine.list_agents(&entry.session_id).await?;
        let agent = agents
            .iter()
            .find(|agent| agent.id == entry.agent_id)
            .ok_or_else(|| anyhow!("worker agent not found: {}", entry.agent_id))?;
        let agent_worktree = agent
            .worktree
            .clone()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("worker has no worktree: {}", entry.agent_id))?;
        let session_worktree = session
            .worktree
            .clone()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("session has no worktree"))?;
        let session_branch = session
            .branch
            .clone()
            .ok_or_else(|| anyhow!("session has no branch"))?;

        let rebase_worktree = agent_worktree.clone();
        let rebase_onto = session_branch.clone();
        let rebase = tokio::task::spawn_blocking(move || {
            worktree::rebase_onto(&rebase_worktree, &rebase_onto)
        })
        .await?;
        if let Err(error) = rebase {
            let detail = truncate(&format!("rebase failed: {error:#}"));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let commit_worktree = agent_worktree.clone();
        let commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&commit_worktree)).await??;
        let base_worktree = session_worktree.clone();
        let base_commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&base_worktree)).await??;

        let profile = agent
            .profile
            .as_deref()
            .and_then(AgentProfile::parse)
            .unwrap_or(AgentProfile::Coding);
        let plan = self.engine.get_team_plan(&session.id).await.ok().flatten();
        let task = entry.task_id.as_deref().and_then(|task_id| {
            plan.as_ref()
                .and_then(|plan| plan.tasks.iter().find(|task| task.id == task_id))
        });
        let objective = task
            .map(|task| task.title.clone())
            .unwrap_or_else(|| format!("Retry integration for queue entry {}", entry.id));
        let acceptance = task.map(|task| task.acceptance.join("; "));
        let control = Arc::new(RunControl::new_operation());
        let budget = SessionBudget::new(
            session.token_budget,
            session.tokens_in + session.tokens_out,
            AgentCaps {
                managers: config.max_managers,
                workers: config.max_workers,
                labour: config.max_labour,
            },
        )
        .with_cost(session.cost_budget_usd, session.cost_spent_usd)
        .with_time(session.time_budget_secs)
        .with_unlimited_flag(self.engine.unlimited_flag(&session.id, session.unlimited));
        let manager_spec = AgentSpec {
            id: agent
                .parent_id
                .clone()
                .unwrap_or_else(|| "integrator".to_string()),
            role: AgentRole::Manager,
            name: "Integrator".to_string(),
            depth: 1,
            parent_name: None,
            title: Some("Integration".to_string()),
            task_id: None,
            department: None,
            profile: AgentProfile::Generic,
            allow_delegation: false,
            auto_approve: true,
            worktree: session_worktree.clone(),
            branch: session_branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),
        };
        let worker_spec = AgentSpec {
            id: agent.id.clone(),
            role: AgentRole::Worker,
            name: agent.name.clone(),
            depth: 2,
            parent_name: Some("Integrator".to_string()),
            title: agent.title.clone(),
            task_id: entry.task_id.clone(),
            department: None,
            profile,
            allow_delegation: false,
            auto_approve: true,
            worktree: agent_worktree.clone(),
            branch: entry.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),
        };
        let verification = verify_worker(
            &self.engine,
            &config,
            &session,
            &project,
            &control,
            &budget,
            &manager_spec,
            &worker_spec,
            &commit,
            &objective,
            "Rebased onto the current session branch after a previous integration attempt.",
            acceptance.as_deref(),
            true,
        )
        .await?;
        let verifier_summary = match verification {
            VerificationOutcome::Approved(verdict) => verdict.summary,
            VerificationOutcome::Rejected(verdict) => {
                let detail = truncate(&format!("retry rejected by Verifier: {}", verdict.summary));
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::Cancelled => {
                let detail = "retry verification cancelled".to_string();
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::BudgetExceeded(stop) => {
                let detail = truncate(&format!("retry verification stopped: {}", stop.summary()));
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::Error(error) => {
                let detail = truncate(&format!("retry verification failed: {error}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
        };
        let Some(entry) = self
            .engine
            .retry_merge_queue_entry(
                &entry.id,
                &commit,
                &base_commit,
                Some(format!(
                    "rebased onto the current session branch; verified: {verifier_summary}"
                )),
            )
            .await?
        else {
            return Err(anyhow!("merge queue entry can no longer be retried"));
        };

        let spec = AgentSpec {
            id: agent.id.clone(),
            role: AgentRole::Worker,
            name: agent.name.clone(),
            depth: 2,
            parent_name: None,
            title: agent.title.clone(),
            task_id: entry.task_id.clone(),
            department: None,
            profile,
            allow_delegation: true,
            auto_approve: false,
            worktree: agent_worktree,
            branch: entry.branch.clone(),
            session_worktree,
            session_branch,
            write_scope: Vec::new(),
            base_commit: entry.base_commit.clone(),
        };
        process_merge_entry(
            &self.engine,
            &session,
            &spec,
            &entry,
            config.integration_command.as_deref(),
        )
        .await?;
        self.engine
            .get_merge_queue_entry(&entry.id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry disappeared after retry"))
    }

    /// Resolve a merge conflict with an internal coding agent, re-verify the
    /// rebased branch, then integrate it. This is an explicit user action: it
    /// skips routine prompts for the resolver and verifier because the user
    /// already chose auto-resolution, but host-level operations are still
    /// denied and the shell sandbox still applies.
    pub async fn resolve_merge(&self, entry_id: &str) -> Result<MergeQueueEntry> {
        self.resolve_merge_with_limit(entry_id, None).await
    }

    async fn resolve_merge_with_limit(
        &self,
        entry_id: &str,
        max_resolve_tokens: Option<u64>,
    ) -> Result<MergeQueueEntry> {
        let entry = self
            .engine
            .get_merge_queue_entry(entry_id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry not found: {entry_id}"))?;
        if entry.status != "conflict" {
            return Err(anyhow!(
                "merge queue entry is {}, only conflict entries can be auto-resolved",
                entry.status
            ));
        }
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), &entry.session_id).await?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        let control = Arc::new(RunControl::new());
        {
            let mut runs = self
                .runs
                .lock()
                .map_err(|_| anyhow!("agent run mutex poisoned"))?;
            if runs.contains_key(&session.id) {
                return Err(anyhow!(
                    "session is running; stop or wait before auto-resolving a merge"
                ));
            }
            runs.insert(session.id.clone(), control.clone());
        }
        self.engine
            .update_session_status(&session.id, "queued")
            .await?;
        let permit = tokio::select! {
            permit = self
                .global_slots
                .acquire(&session.id, &session.project_id, session.priority, &control) => permit,
            _ = wait_for_cancel(&control) => None,
        };
        let Some(_permit) = permit else {
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(&session.id);
            }
            let _ = self.engine.update_session_status(&session.id, "idle").await;
            return Err(anyhow!(
                "auto-resolution cancelled while waiting for a global slot"
            ));
        };
        if control.is_cancelled() {
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(&session.id);
            }
            let _ = self.engine.update_session_status(&session.id, "idle").await;
            return Err(anyhow!("auto-resolution cancelled"));
        }
        self.engine
            .update_session_status(&session.id, "running")
            .await?;
        let result = self
            .resolve_merge_inner(
                &config,
                &session,
                &project,
                &entry,
                &control,
                max_resolve_tokens,
            )
            .await;
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(&session.id);
        }
        let _ = self.engine.update_session_status(&session.id, "idle").await;
        result
    }

    async fn resolve_merge_inner(
        &self,
        config: &AgentConfig,
        session: &Session,
        project: &Project,
        entry: &MergeQueueEntry,
        control: &RunControl,
        max_resolve_tokens: Option<u64>,
    ) -> Result<MergeQueueEntry> {
        let agents = self.engine.list_agents(&session.id).await?;
        let worker = agents
            .iter()
            .find(|agent| agent.id == entry.agent_id)
            .ok_or_else(|| anyhow!("worker agent not found: {}", entry.agent_id))?;
        let worker_worktree = worker
            .worktree
            .clone()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("worker has no worktree: {}", entry.agent_id))?;
        let session_worktree = session
            .worktree
            .clone()
            .map(PathBuf::from)
            .ok_or_else(|| anyhow!("session has no worktree"))?;
        let session_branch = session
            .branch
            .clone()
            .ok_or_else(|| anyhow!("session has no branch"))?;

        let clean_worktree = session_worktree.clone();
        let clean = tokio::task::spawn_blocking(move || {
            worktree::git_output(&clean_worktree, &["status", "--porcelain"])
        })
        .await??;
        if !clean.trim().is_empty() {
            anyhow::bail!("session worktree has uncommitted changes; commit or discard them first");
        }
        let clean_worker = worker_worktree.clone();
        let clean = tokio::task::spawn_blocking(move || {
            worktree::git_output(&clean_worker, &["status", "--porcelain"])
        })
        .await??;
        if !clean.trim().is_empty() {
            anyhow::bail!("worker worktree has uncommitted changes; resolve them first");
        }
        let base_worktree = session_worktree.clone();
        let base_commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&base_worktree)).await??;

        let rebase_worktree = worker_worktree.clone();
        let rebase_onto = session_branch.clone();
        let rebase = tokio::task::spawn_blocking(move || {
            worktree::rebase_onto_keep_conflicts(&rebase_worktree, &rebase_onto)
        })
        .await?;
        if let Err(error) = rebase {
            let paths_worktree = worker_worktree.clone();
            let paths =
                tokio::task::spawn_blocking(move || worktree::conflicted_paths(&paths_worktree))
                    .await??;
            let in_progress_worktree = worker_worktree.clone();
            let in_progress = tokio::task::spawn_blocking(move || {
                worktree::rebase_in_progress(&in_progress_worktree)
            })
            .await?;
            if paths.is_empty() || !in_progress {
                let detail = truncate(&format!("rebase failed: {error:#}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            let detail = truncate(&format!(
                "rebase conflict in {} path(s): {}",
                paths.len(),
                paths.join(", ")
            ));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
        }

        let paths_worktree = worker_worktree.clone();
        let conflict_paths =
            tokio::task::spawn_blocking(move || worktree::conflicted_paths(&paths_worktree))
                .await??;
        let resolver = self
            .engine
            .create_agent(
                &session.id,
                AgentRole::Worker.as_str(),
                "Conflict Resolver",
                Some("Integration".to_string()),
                worker.parent_id.clone(),
                &model_for(config, AgentRole::Worker),
            )
            .await?;
        self.engine
            .set_agent_task(
                &resolver.id,
                worker.task_id.clone(),
                worker.department.clone(),
            )
            .await?;
        self.engine
            .set_agent_worktree(
                &resolver.id,
                &worker_worktree.to_string_lossy(),
                &entry.branch,
            )
            .await?;
        self.engine
            .set_agent_profile(&resolver.id, AgentProfile::Coding.as_str())
            .await?;
        let resolver_spec = AgentSpec {
            id: resolver.id.clone(),
            role: AgentRole::Worker,
            name: resolver.name.clone(),
            depth: 2,
            parent_name: worker.parent_id.clone(),
            title: Some("Integration".to_string()),
            task_id: entry.task_id.clone(),
            department: None,
            profile: AgentProfile::Coding,
            allow_delegation: false,
            auto_approve: true,
            worktree: worker_worktree.clone(),
            branch: entry.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),
        };
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "agent.spawned",
            serde_json::json!({
                "parent_id": resolver.parent_id,
                "parent_name": worker.parent_id,
                "objective": "Resolve a merge conflict",
                "conflict_paths": conflict_paths,
            }),
        )
        .await?;
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "integrator.conflict_resolution_started",
            serde_json::json!({
                "entry_id": entry.id,
                "branch": entry.branch,
                "base_commit": base_commit,
                "conflict_paths": conflict_paths,
            }),
        )
        .await?;

        let spent = session.tokens_in + session.tokens_out;
        let limit = match (session.token_budget, max_resolve_tokens) {
            (Some(session_limit), Some(cap)) => Some(session_limit.min(spent.saturating_add(cap))),
            (None, Some(cap)) => Some(spent.saturating_add(cap)),
            (Some(session_limit), None) => Some(session_limit),
            (None, None) => None,
        };
        let budget = SessionBudget::new(
            limit,
            spent,
            AgentCaps {
                managers: config.max_managers,
                workers: config.max_workers,
                labour: config.max_labour,
            },
        )
        .with_cost(session.cost_budget_usd, session.cost_spent_usd)
        .with_time(session.time_budget_secs)
        .with_unlimited_flag(self.engine.unlimited_flag(&session.id, session.unlimited));
        let prompt = format!(
            "You are resolving a Git rebase conflict for integration queue entry {}.              The worktree is {} on branch {}. The rebase target is session branch {} at commit {}.              Conflicted paths: {}.              Resolve the conflicts in place, preserving the intent of both sides. Do not abort the rebase.              Do not commit manually; leave the resolved files in the worktree. Run relevant tests if possible.              The runtime will stage resolved files, continue the rebase, verify the result, and merge it.              Reply with a concise summary of the resolution.",
            entry.id,
            worker_worktree.to_string_lossy(),
            entry.branch,
            session_branch,
            base_commit,
            conflict_paths.join(", ")
        );
        let mut conversation = vec![ChatMessage::user(prompt.clone())];
        let result = run_loop(
            &self.engine,
            config,
            session,
            project,
            control,
            &budget,
            &resolver_spec,
            &prompt,
            &mut conversation,
            None,
            false,
        )
        .await;
        let outcome = match result {
            Ok(LoopOutcome::Final(summary)) => summary,
            Ok(LoopOutcome::Cancelled) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "cancelled")
                    .await?;
                self.engine
                    .update_merge_queue_entry(
                        &entry.id,
                        "conflict",
                        Some("auto-resolution cancelled".into()),
                    )
                    .await?;
                return Err(anyhow!("auto-resolution cancelled"));
            }
            Ok(LoopOutcome::BudgetExceeded(stop)) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "failed")
                    .await?;
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(stop.summary()))
                    .await?;
                return Err(anyhow!("{} during auto-resolution", stop.summary()));
            }
            Ok(LoopOutcome::Verdict(_)) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "failed")
                    .await?;
                return Err(anyhow!("resolver returned a verdict unexpectedly"));
            }
            Err(error) => {
                let wt = worker_worktree.clone();
                tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
                self.engine
                    .update_agent_status(&resolver.id, "failed")
                    .await?;
                let detail = truncate(&format!("auto-resolution failed: {error:#}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
        };

        let paths_worktree = worker_worktree.clone();
        let remaining = tokio::task::spawn_blocking(move || {
            if worktree::rebase_in_progress(&paths_worktree) {
                worktree::stage_all(&paths_worktree)?;
            }
            worktree::conflicted_paths(&paths_worktree)
        })
        .await??;
        if !remaining.is_empty() {
            let wt = worker_worktree.clone();
            tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
            self.engine
                .update_agent_status(&resolver.id, "failed")
                .await?;
            let detail = truncate(&format!(
                "auto-resolution left {} conflicted path(s): {}",
                remaining.len(),
                remaining.join(", ")
            ));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let marker_worktree = worker_worktree.clone();
        let markers =
            tokio::task::spawn_blocking(move || worktree::conflict_markers(&marker_worktree))
                .await??;
        if !markers.is_empty() {
            let wt = worker_worktree.clone();
            tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
            self.engine
                .update_agent_status(&resolver.id, "failed")
                .await?;
            let detail = truncate(&format!(
                "auto-resolution left conflict markers in: {}",
                markers.join(", ")
            ));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let continue_worktree = worker_worktree.clone();
        let continued = tokio::task::spawn_blocking(move || -> Result<()> {
            if worktree::rebase_in_progress(&continue_worktree) {
                worktree::continue_rebase(&continue_worktree)?;
            }
            Ok(())
        })
        .await?;
        if let Err(error) = continued {
            let wt = worker_worktree.clone();
            tokio::task::spawn_blocking(move || worktree::abort_rebase(&wt)).await?;
            self.engine
                .update_agent_status(&resolver.id, "failed")
                .await?;
            let detail = truncate(&format!("could not complete rebase: {error:#}"));
            self.engine
                .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                .await?;
            return Err(anyhow!(detail));
        }

        let commit_worktree = worker_worktree.clone();
        let commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&commit_worktree)).await??;
        self.engine
            .update_agent_status(&resolver.id, "idle")
            .await?;
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "agent.finished",
            serde_json::json!({
                "status": "idle",
                "summary": truncate(&outcome),
                "resolved": true,
                "commit": commit,
                "worktree": worker_worktree,
                "branch": entry.branch,
            }),
        )
        .await?;

        let manager_spec = AgentSpec {
            id: worker
                .parent_id
                .clone()
                .unwrap_or_else(|| "integrator".to_string()),
            role: AgentRole::Manager,
            name: "Integrator".to_string(),
            depth: 1,
            parent_name: None,
            title: Some("Integration".to_string()),
            task_id: None,
            department: None,
            profile: AgentProfile::Generic,
            allow_delegation: false,
            auto_approve: true,
            worktree: session_worktree.clone(),
            branch: session_branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),
        };
        let worker_spec = AgentSpec {
            id: worker.id.clone(),
            role: AgentRole::Worker,
            name: worker.name.clone(),
            depth: 2,
            parent_name: Some("Integrator".to_string()),
            title: worker.title.clone(),
            task_id: entry.task_id.clone(),
            department: None,
            profile: AgentProfile::Coding,
            allow_delegation: false,
            auto_approve: true,
            worktree: worker_worktree.clone(),
            branch: entry.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: session_branch.clone(),
            write_scope: Vec::new(),
            base_commit: base_commit.clone(),
        };
        let verification = verify_worker(
            &self.engine,
            config,
            session,
            project,
            control,
            &budget,
            &manager_spec,
            &worker_spec,
            &commit,
            &format!("Resolve integration conflict for queue entry {}", entry.id),
            &outcome,
            Some("No conflict markers; both sides' intent is preserved; relevant tests pass"),
            true,
        )
        .await?;
        let verdict = match verification {
            VerificationOutcome::Approved(verdict) => verdict,
            VerificationOutcome::Rejected(verdict) => {
                let detail = truncate(&format!(
                    "auto-resolution rejected by Verifier: {}",
                    verdict.summary
                ));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
            VerificationOutcome::Cancelled => {
                self.engine
                    .update_merge_queue_entry(
                        &entry.id,
                        "conflict",
                        Some("auto-resolution verification cancelled".into()),
                    )
                    .await?;
                return Err(anyhow!("auto-resolution verification cancelled"));
            }
            VerificationOutcome::BudgetExceeded(stop) => {
                self.engine
                    .update_merge_queue_entry(
                        &entry.id,
                        "conflict",
                        Some(format!("{} during verification", stop.summary())),
                    )
                    .await?;
                return Err(anyhow!("{} during verification", stop.summary()));
            }
            VerificationOutcome::Error(error) => {
                let detail = truncate(&format!("auto-resolution verification failed: {error}"));
                self.engine
                    .update_merge_queue_entry(&entry.id, "conflict", Some(detail.clone()))
                    .await?;
                return Err(anyhow!(detail));
            }
        };

        let Some(entry) = self
            .engine
            .retry_merge_queue_entry(
                &entry.id,
                &commit,
                &base_commit,
                Some(format!(
                    "auto-resolved rebase conflict; verifier: {}",
                    truncate(&verdict.summary)
                )),
            )
            .await?
        else {
            return Err(anyhow!("merge queue entry can no longer be resolved"));
        };
        emit(
            &self.engine,
            &session.id,
            &resolver_spec,
            "integrator.conflict_resolution_finished",
            serde_json::json!({
                "entry_id": entry.id,
                "commit": commit,
                "verdict": truncate(&verdict.summary),
            }),
        )
        .await?;
        let integration_spec = AgentSpec {
            auto_approve: true,
            ..resolver_spec
        };
        let _ = process_merge_entry(
            &self.engine,
            session,
            &integration_spec,
            &entry,
            config.integration_command.as_deref(),
        )
        .await?;
        self.engine
            .get_merge_queue_entry(&entry.id)
            .await?
            .ok_or_else(|| anyhow!("merge queue entry disappeared after auto-resolution"))
    }

    /// Start the daemon-side watcher that auto-resolves conflicts when a
    /// project opts in. Safe to call more than once.
    pub fn start_auto_resolver(&self) {
        if self.auto_resolver_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let service = self.clone();
        tokio::spawn(async move {
            let mut events = service.engine.subscribe();
            service.scan_auto_resolve().await;
            loop {
                match events.recv().await {
                    Ok(event)
                        if event.kind == "session.updated" && event.payload["status"] == "idle" =>
                    {
                        service.auto_resolve_session(&event.session_id).await;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }

    /// Opt-in global task-DAG resume scheduler. Every tick it picks one
    /// idle/failed/cancelled session with retryable tasks, ordered by effective
    /// priority and session round-robin, then resumes its stored plan. This is
    /// intentionally not default-on: resuming a plan spends the session budget.
    /// Dispatch a standalone intake item immediately. Manual dispatch is a
    /// deliberate user action, so it bypasses the project's schedule toggle
    /// and parallel cap but still atomically claims the row and uses the same
    /// one-task plan, Verifier, and Integrator path as every other Worker.
    pub async fn dispatch_intake_task(&self, id: &str) -> Result<IntakeDispatchResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let Some(mut task) = self.engine.get_intake_task(id.to_string()).await? else {
            anyhow::bail!("intake task not found: {id}");
        };
        if !task.deps.is_empty() {
            let peers = self
                .engine
                .list_intake_tasks(Some(task.project_id.clone()), None, None)
                .await?
                .tasks;
            for dep in &task.deps {
                match peers.iter().find(|row| &row.id == dep) {
                    Some(row) if matches!(row.status.as_str(), "completed" | "skipped") => {}
                    Some(row) => anyhow::bail!(
                        "intake task {id} is waiting for dependency {} ({})",
                        row.title,
                        row.status
                    ),
                    None => anyhow::bail!("intake task {id} has unknown dependency {dep}"),
                }
            }
        }
        if !matches!(
            task.status.as_str(),
            "queued" | "failed" | "interrupted" | "cancelled"
        ) {
            anyhow::bail!("intake task {id} is not runnable (status: {})", task.status);
        }
        if task.attempts >= task.max_attempts
            && matches!(task.status.as_str(), "failed" | "interrupted" | "cancelled")
        {
            task = self
                .engine
                .retry_intake_task(task.id.clone())
                .await?
                .ok_or_else(|| anyhow!("intake task {id} could not be reset for a manual run"))?;
        }
        let Some(claimed) = self.engine.claim_intake_task(task.id.clone()).await? else {
            anyhow::bail!("intake task {id} was claimed by another dispatcher");
        };
        let plan = self.engine.intake_plan(&claimed);
        if let Err(error) = validate_team_plan(&plan, config.max_managers) {
            let next = self.intake_retry_at(&claimed).await;
            let _ = self
                .engine
                .settle_intake_task(
                    claimed.id.clone(),
                    "failed".to_string(),
                    None,
                    Some(format!("plan validation failed: {error:#}")),
                    next,
                )
                .await;
            return Err(error);
        }
        let goal = format!(
            "{} · {}",
            claimed.title,
            claimed
                .instructions
                .lines()
                .find(|line| !line.trim().is_empty())
                .unwrap_or("")
                .trim()
                .chars()
                .take(180)
                .collect::<String>()
        );
        let session = match self
            .engine
            .create_session_with_options(
                claimed.project_id.clone(),
                goal,
                claimed.token_budget,
                claimed.cost_budget_usd,
                claimed.time_budget_secs,
                claimed.priority,
            )
            .await
        {
            Ok(session) => session,
            Err(error) => {
                let next = self.intake_retry_at(&claimed).await;
                self.engine
                    .settle_intake_task(
                        claimed.id.clone(),
                        "failed".to_string(),
                        None,
                        Some(format!("could not create session: {error:#}")),
                        next,
                    )
                    .await?;
                return Err(error);
            }
        };
        let setup = async {
            validate_team_plan(&plan, config.max_managers)?;
            self.engine.set_team_plan(&session.id, &plan).await?;
            self.engine
                .start_intake_task(claimed.id.clone(), session.id.clone(), "T1".to_string())
                .await?;
            self.engine
                .append_event(
                    &session.id,
                    "intake.dispatched",
                    serde_json::json!({
                        "intake_task_id": claimed.id,
                        "title": claimed.title,
                        "profile": claimed.profile,
                        "priority": claimed.priority,
                        "attempt": claimed.attempts,
                    }),
                )
                .await?;
            Ok::<(), anyhow::Error>(())
        }
        .await;
        if let Err(error) = setup {
            let next = self.intake_retry_at(&claimed).await;
            let _ = self
                .engine
                .update_session_status(&session.id, "failed")
                .await;
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    "intake.dispatch_failed",
                    serde_json::json!({
                        "intake_task_id": claimed.id,
                        "error": format!("{error:#}"),
                    }),
                )
                .await;
            let _ = self
                .engine
                .settle_intake_task(
                    claimed.id.clone(),
                    "failed".to_string(),
                    None,
                    Some(format!("could not prepare the task plan: {error:#}")),
                    next,
                )
                .await;
            return Err(error);
        }
        if let Err(error) = self.resume_plan(&session.id).await {
            let next = self.intake_retry_at(&claimed).await;
            let _ = self
                .engine
                .update_session_status(&session.id, "failed")
                .await;
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    "intake.dispatch_failed",
                    serde_json::json!({
                        "intake_task_id": claimed.id,
                        "error": format!("{error:#}"),
                    }),
                )
                .await;
            let updated = self
                .engine
                .settle_intake_task(
                    claimed.id.clone(),
                    "failed".to_string(),
                    None,
                    Some(format!("could not start the task run: {error:#}")),
                    next,
                )
                .await?;
            return Ok(IntakeDispatchResult {
                started: false,
                task: updated.unwrap_or(claimed),
                session_id: Some(session.id),
            });
        }
        let updated = self.engine.get_intake_task(claimed.id.clone()).await?;
        Ok(IntakeDispatchResult {
            started: true,
            task: updated.unwrap_or(claimed),
            session_id: Some(session.id),
        })
    }

    /// Settle intake rows whose session reached a terminal plan status. This
    /// runs on every scheduler tick, so a daemon crash or a provider failure
    /// never leaves an intake item stuck in `running`.
    pub async fn reconcile_intake_tasks(&self) -> Result<u32> {
        let dependency_blocked = self.reconcile_intake_dependencies().await?;
        let tasks = self
            .engine
            .list_intake_tasks(None, Some("running".to_string()), Some(1_000))
            .await?
            .tasks;
        let mut settled = dependency_blocked;
        for task in tasks {
            let Some(session_id) = task.session_id.clone() else {
                let next = self.intake_retry_at(&task).await;
                let _ = self
                    .engine
                    .settle_intake_task(
                        task.id.clone(),
                        "failed".to_string(),
                        None,
                        Some("session id was not recorded".to_string()),
                        next,
                    )
                    .await;
                settled += 1;
                continue;
            };
            let (session, _) =
                match load_session_project(self.engine.store().clone(), &session_id).await {
                    Ok(value) => value,
                    Err(_) => {
                        let next = self.intake_retry_at(&task).await;
                        let _ = self
                            .engine
                            .settle_intake_task(
                                task.id.clone(),
                                "failed".to_string(),
                                None,
                                Some("the linked session no longer exists".to_string()),
                                next,
                            )
                            .await;
                        settled += 1;
                        continue;
                    }
                };
            let queue = self
                .engine
                .task_queue(None, Some(session.id.clone()))
                .await?;
            let row = queue
                .tasks
                .iter()
                .find(|row| row.task_id == task.task_id.clone().unwrap_or_default());
            let summary = latest_plan_results(&self.engine, &session.id)
                .await
                .ok()
                .and_then(|results| {
                    task.task_id
                        .as_ref()
                        .and_then(|task_id| results.get(task_id).cloned())
                });
            let terminal: Option<(&str, Option<String>, Option<String>)> =
                match row.map(|row| row.status.as_str()) {
                    Some("completed") | Some("skipped") => Some((
                        "completed",
                        Some(summary.unwrap_or_else(|| "completed".into())),
                        None,
                    )),
                    Some("failed") | Some("blocked") | Some("interrupted") => {
                        let detail = row.and_then(|row| row.last_error.clone());
                        Some((
                            "failed",
                            detail
                                .clone()
                                .or_else(|| summary.clone())
                                .or(Some("the worker did not complete".into())),
                            Some(detail.unwrap_or_else(|| "the worker did not complete".into())),
                        ))
                    }
                    Some("cancelled") => Some(("cancelled", summary, None)),
                    Some("pending") | Some("running") => match session.status.as_str() {
                        "idle" | "draft" | "landed" | "imported" => Some((
                            "failed",
                            Some("the session ended before the task settled".into()),
                            Some("the session ended before the task settled".into()),
                        )),
                        _ => None,
                    },
                    _ => match session.status.as_str() {
                        "failed" => Some((
                            "failed",
                            Some("the session failed before the task settled".into()),
                            Some("the session failed before the task settled".into()),
                        )),
                        "cancelled" => Some(("cancelled", summary, None)),
                        "idle" | "draft" | "landed" | "imported" => Some((
                            "failed",
                            Some("the plan task was not found".into()),
                            Some("the plan task was not found".into()),
                        )),
                        _ => None,
                    },
                };
            let Some((status, result_summary, last_error)) = terminal else {
                continue;
            };
            let next = if status == "failed" {
                self.intake_retry_at(&task).await
            } else {
                None
            };
            let blocked_next = next.clone();
            let _ = self
                .engine
                .settle_intake_task(
                    task.id.clone(),
                    status.to_string(),
                    result_summary,
                    last_error,
                    next,
                )
                .await?;
            if status == "cancelled" || (status == "failed" && blocked_next.is_none()) {
                self.block_intake_dependents(
                    &task.project_id,
                    &task.id,
                    &format!("dependency \"{}\" is {status}", task.title),
                )
                .await;
            }
            let kind = match status {
                "completed" => "intake.completed",
                "cancelled" => "intake.cancelled",
                _ => "intake.failed",
            };
            let _ = self
                .engine
                .append_event(
                    &session.id,
                    kind,
                    serde_json::json!({
                        "intake_task_id": task.id,
                        "task_id": task.task_id,
                        "status": status,
                        "attempt": task.attempts,
                    }),
                )
                .await;
            settled += 1;
        }
        Ok(settled)
    }

    /// Read-only scheduler observability for the dashboard and CLI. Counts
    /// are derived from the persisted projections; this never changes state.
    pub async fn scheduler_status(
        &self,
        project_id: Option<String>,
    ) -> Result<SchedulerStatusResult> {
        let sessions = self.engine.list_sessions(project_id.clone()).await?;
        let tasks = self
            .engine
            .task_queue(project_id.clone(), None)
            .await?
            .tasks;
        let intake = self
            .engine
            .list_intake_tasks(project_id.clone(), None, Some(2_000))
            .await?
            .tasks;
        let all_projects = self.engine.list_projects().await?;
        let projects = all_projects
            .into_iter()
            .filter(|project| project_id.as_deref().is_none_or(|id| id == project.id))
            .collect::<Vec<_>>();

        let session_is_runnable =
            |session: &Session| matches!(session.status.as_str(), "idle" | "failed" | "cancelled");
        let task_is_retryable =
            |status: &str| matches!(status, "pending" | "interrupted" | "failed" | "cancelled");
        let task_is_running =
            |status: &str| matches!(status, "running" | "claimed" | "pausing" | "paused");
        let mut pending_sessions = std::collections::HashSet::new();
        for task in &tasks {
            if !task_is_retryable(&task.status) {
                continue;
            }
            if sessions
                .iter()
                .find(|session| session.id == task.session_id)
                .is_some_and(session_is_runnable)
            {
                pending_sessions.insert(task.session_id.clone());
            }
        }
        let active_sessions = sessions
            .iter()
            .filter(|session| {
                matches!(
                    session.status.as_str(),
                    "running" | "paused" | "waiting_input"
                )
            })
            .count();
        let queued_sessions = sessions
            .iter()
            .filter(|session| session.status == "queued")
            .count();
        let paused_sessions = sessions
            .iter()
            .filter(|session| session.status == "paused")
            .count();
        let pending_plan_tasks = tasks
            .iter()
            .filter(|task| task_is_retryable(&task.status))
            .count();
        let running_plan_tasks = tasks
            .iter()
            .filter(|task| task_is_running(&task.status))
            .count();
        let queued_intake = intake.iter().filter(|task| task.status == "queued").count();
        let active_intake = intake
            .iter()
            .filter(|task| task.status == "running" || task.status == "dispatching")
            .count();
        let failed_intake = intake
            .iter()
            .filter(|task| task.status == "failed" || task.status == "interrupted")
            .count();

        let mut project_statuses = Vec::new();
        for project in projects {
            let policy = self
                .engine
                .get_task_scheduler_policy(&project.id)
                .await?
                .policy;
            let project_tasks = tasks
                .iter()
                .filter(|task| task.project_id == project.id)
                .collect::<Vec<_>>();
            let project_intake = intake
                .iter()
                .filter(|task| task.project_id == project.id)
                .collect::<Vec<_>>();
            let mut pending = std::collections::HashSet::new();
            for task in &project_tasks {
                if task_is_retryable(&task.status)
                    && sessions
                        .iter()
                        .find(|session| session.id == task.session_id)
                        .is_some_and(session_is_runnable)
                {
                    pending.insert(task.session_id.clone());
                }
            }
            project_statuses.push(SchedulerProjectStatus {
                project_id: project.id.clone(),
                project_name: project.name.clone(),
                enabled: policy.enabled,
                cooldown_secs: policy.cooldown_secs,
                dispatch_tasks: policy.dispatch_tasks,
                include_intake: policy.include_intake,
                max_intake_parallel: policy.max_intake_parallel,
                pending_sessions: pending.len(),
                pending_plan_tasks: project_tasks
                    .iter()
                    .filter(|task| task_is_retryable(&task.status))
                    .count(),
                running_plan_tasks: project_tasks
                    .iter()
                    .filter(|task| task_is_running(&task.status))
                    .count(),
                queued_intake: project_intake
                    .iter()
                    .filter(|task| task.status == "queued")
                    .count(),
                active_intake: project_intake
                    .iter()
                    .filter(|task| task.status == "running" || task.status == "dispatching")
                    .count(),
                failed_intake: project_intake
                    .iter()
                    .filter(|task| task.status == "failed" || task.status == "interrupted")
                    .count(),
            });
        }

        Ok(SchedulerStatusResult {
            active_sessions,
            queued_sessions,
            paused_sessions,
            pending_plan_tasks,
            running_plan_tasks,
            queued_intake,
            active_intake,
            failed_intake,
            global_session_cap: global_session_cap(),
            global_worker_cap: global_worker_cap(),
            global_labour_cap: global_labour_cap(),
            projects: project_statuses,
        })
    }

    /// Block queued dependents whose dependency can no longer make progress.
    /// This also repairs rows after a daemon restart, where recovery marked a
    /// running intake task failed without touching its dependents.
    async fn reconcile_intake_dependencies(&self) -> Result<u32> {
        let tasks = self.engine.list_intake_tasks(None, None, None).await?.tasks;
        let by_id = tasks
            .iter()
            .map(|task| (task.id.clone(), task))
            .collect::<HashMap<_, _>>();
        let mut policies = HashMap::new();
        let mut blocked = 0;
        for task in tasks.iter().filter(|task| task.status == "queued") {
            if task.deps.is_empty() {
                continue;
            }
            if !policies.contains_key(&task.project_id) {
                let policy = self
                    .engine
                    .orchestration_policy(&task.project_id)
                    .await
                    .unwrap_or_default();
                policies.insert(task.project_id.clone(), policy);
            }
            let policy = policies.get(&task.project_id).cloned().unwrap_or_default();
            let mut waiting = false;
            let mut failure = None;
            for dep in &task.deps {
                let Some(dependency) = by_id.get(dep) else {
                    failure = Some(format!("dependency {dep} no longer exists"));
                    break;
                };
                match dependency.status.as_str() {
                    "completed" | "skipped" => {}
                    "failed" | "interrupted"
                        if dependency.next_attempt_at.is_some()
                            || (policy.auto_retry_failed
                                && dependency.attempts < dependency.max_attempts) =>
                    {
                        // A retry is scheduled or still available; keep
                        // waiting, but continue scanning for a permanent
                        // failure in a sibling dependency.
                        waiting = true;
                    }
                    "failed" | "interrupted" | "cancelled" | "blocked" => {
                        failure = Some(format!(
                            "dependency \"{}\" is {}",
                            dependency.title, dependency.status
                        ));
                        break;
                    }
                    _ => {
                        // Dispatching/running/queued dependency: wait.
                        waiting = true;
                    }
                }
            }
            if waiting && failure.is_none() {
                continue;
            }
            let Some(reason) = failure else {
                continue;
            };
            self.engine
                .settle_intake_task(
                    task.id.clone(),
                    "blocked".to_string(),
                    None,
                    Some(format!("blocked: {reason}")),
                    None,
                )
                .await?;
            blocked += 1;
        }
        Ok(blocked)
    }

    pub async fn cancel_intake_task(&self, id: &str) -> Result<IntakeTask> {
        let Some(task) = self.engine.get_intake_task(id.to_string()).await? else {
            anyhow::bail!("intake task not found: {id}");
        };
        match task.status.as_str() {
            "running" | "dispatching" => {
                if let Some(session_id) = task.session_id.as_deref() {
                    let _ = self.cancel(session_id);
                    let _ = self
                        .engine
                        .append_event(
                            session_id,
                            "intake.cancelled",
                            serde_json::json!({
                                "intake_task_id": task.id,
                                "reason": "cancelled by user",
                            }),
                        )
                        .await;
                }
                let cancelled = self
                    .engine
                    .settle_intake_task(
                        task.id.clone(),
                        "cancelled".to_string(),
                        None,
                        Some("cancelled by user".to_string()),
                        None,
                    )
                    .await?
                    .ok_or_else(|| anyhow!("intake task disappeared while cancelling"))?;
                self.block_intake_dependents(
                    &cancelled.project_id,
                    &cancelled.id,
                    &format!("dependency \"{}\" was cancelled", cancelled.title),
                )
                .await;
                Ok(cancelled)
            }
            "queued" | "failed" | "interrupted" | "blocked" => {
                let cancelled = self
                    .engine
                    .cancel_intake_task(id.to_string())
                    .await?
                    .ok_or_else(|| {
                        anyhow!("intake task {id} cannot be cancelled from {}", task.status)
                    })?;
                self.block_intake_dependents(
                    &cancelled.project_id,
                    &cancelled.id,
                    &format!("dependency \"{}\" was cancelled", cancelled.title),
                )
                .await;
                Ok(cancelled)
            }
            "completed" | "cancelled" => Ok(task),
            other => anyhow::bail!("intake task {id} cannot be cancelled from {other}"),
        }
    }

    pub async fn retry_intake_task(&self, id: &str) -> Result<IntakeTask> {
        self.engine
            .retry_intake_task(id.to_string())
            .await?
            .ok_or_else(|| {
                anyhow!("intake task {id} is not failed, interrupted, cancelled, or blocked")
            })
    }

    pub async fn delete_intake_task(&self, id: &str) -> Result<()> {
        let dependents = self
            .engine
            .list_intake_tasks(None, None, None)
            .await?
            .tasks
            .into_iter()
            .filter(|task| task.deps.iter().any(|dep| dep == id))
            .collect::<Vec<_>>();
        if let Some(dependent) = dependents
            .iter()
            .find(|task| !matches!(task.status.as_str(), "completed" | "cancelled"))
        {
            anyhow::bail!(
                "intake task {id} is still a dependency of \"{}\"",
                dependent.title
            );
        }
        if !self.engine.delete_intake_task(id.to_string()).await? {
            anyhow::bail!("intake task {id} is active or does not exist");
        }
        Ok(())
    }

    async fn block_intake_dependents(&self, project_id: &str, dependency_id: &str, reason: &str) {
        let tasks = match self
            .engine
            .list_intake_tasks(Some(project_id.to_string()), None, None)
            .await
        {
            Ok(tasks) => tasks.tasks,
            Err(error) => {
                tracing::warn!(%error, project_id, "could not block intake dependents");
                return;
            }
        };
        for task in tasks {
            if task.status != "queued" || !task.deps.iter().any(|dep| dep == dependency_id) {
                continue;
            }
            let error = format!("blocked: {reason}");
            if let Err(error) = self
                .engine
                .settle_intake_task(
                    task.id.clone(),
                    "blocked".to_string(),
                    None,
                    Some(error.clone()),
                    None,
                )
                .await
            {
                tracing::warn!(%error, task_id = %task.id, "could not block dependent intake task");
            }
        }
    }

    async fn intake_retry_at(&self, task: &IntakeTask) -> Option<String> {
        if task.attempts >= task.max_attempts {
            return None;
        }
        let policy = self
            .engine
            .orchestration_policy(&task.project_id)
            .await
            .ok()?;
        if !policy.auto_retry_failed {
            return None;
        }
        let delay = crate::orchestration::retry_delay(&policy, task.attempts.max(1));
        (time::OffsetDateTime::now_utc() + delay)
            .format(&time::format_description::well_known::Rfc3339)
            .ok()
    }

    pub fn start_task_scheduler(&self) {
        if self.task_scheduler_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let interval_secs = env::var("VERLINPARK_TASK_SCHEDULER_SECS")
            .ok()
            .and_then(|value| value.parse::<u64>().ok())
            .unwrap_or(5)
            .clamp(1, 3600);
        let service = self.clone();
        tokio::spawn(async move {
            if let Err(error) = service.reconcile_intake_tasks().await {
                tracing::warn!(%error, "intake reconciler failed at startup");
            }
            let mut ticker = tokio::time::interval(Duration::from_secs(interval_secs));
            ticker.tick().await;
            let mut last_session: Option<String> = None;
            let mut last_project: Option<String> = None;
            let mut cooldowns: HashMap<String, Instant> = HashMap::new();
            let mut intake_cooldowns: HashMap<String, Instant> = HashMap::new();
            loop {
                ticker.tick().await;
                if let Err(error) = service.reconcile_intake_tasks().await {
                    tracing::warn!(%error, "global task scheduler could not reconcile intake tasks");
                }
                let now = Instant::now();
                cooldowns.retain(|_, until| *until > now);
                intake_cooldowns.retain(|_, until| *until > now);

                let excluded = cooldowns.keys().cloned().collect::<Vec<_>>();
                let candidate = match service
                    .engine
                    .scheduler()
                    .next_pending_session(last_session.as_deref(), &excluded)
                    .await
                {
                    Ok(candidate) => candidate,
                    Err(error) => {
                        tracing::warn!(%error, "global task scheduler could not read the queue");
                        None
                    }
                };
                if let Some(candidate) = candidate {
                    cooldowns.insert(
                        candidate.session_id.clone(),
                        now + Duration::from_secs(candidate.cooldown_secs),
                    );
                    let dispatch_tasks = service
                        .engine
                        .get_task_scheduler_policy(&candidate.project_id)
                        .await
                        .map(|policy| policy.policy.dispatch_tasks)
                        .unwrap_or(false);
                    tracing::info!(
                        session_id = %candidate.session_id,
                        project_id = %candidate.project_id,
                        pending = candidate.pending,
                        session_priority = candidate.priority,
                        task_priority = candidate.task_priority,
                        effective_priority = candidate.effective_priority,
                        cooldown_secs = candidate.cooldown_secs,
                        dispatch_tasks,
                        "global task scheduler dispatching work"
                    );
                    let result = if dispatch_tasks {
                        service.dispatch_next_task(&candidate.session_id).await
                    } else {
                        service.resume_plan(&candidate.session_id).await
                    };
                    match result {
                        Ok(true) => {
                            last_session = Some(candidate.session_id);
                        }
                        Ok(false) => {}
                        Err(error) => {
                            tracing::warn!(
                                session_id = %candidate.session_id,
                                dispatch_tasks,
                                %error,
                                "global task scheduler could not dispatch work"
                            );
                        }
                    }
                }

                if !service.info().configured {
                    continue;
                }
                let excluded_intake = intake_cooldowns.keys().cloned().collect::<Vec<_>>();
                let intake = match service
                    .engine
                    .scheduler()
                    .next_pending_intake(last_project.as_deref(), &excluded_intake)
                    .await
                {
                    Ok(intake) => intake,
                    Err(error) => {
                        tracing::warn!(%error, "global task scheduler could not read intake tasks");
                        None
                    }
                };
                let Some(intake) = intake else {
                    continue;
                };
                let policy = service
                    .engine
                    .get_task_scheduler_policy(&intake.task.project_id)
                    .await
                    .ok()
                    .map(|result| result.policy)
                    .unwrap_or_default();
                intake_cooldowns.insert(
                    intake.task.id.clone(),
                    now + Duration::from_secs(policy.cooldown_secs),
                );
                tracing::info!(
                    intake_task_id = %intake.task.id,
                    project_id = %intake.task.project_id,
                    effective_priority = intake.effective_priority,
                    attempt = intake.task.attempts,
                    "global task scheduler dispatching standalone intake work"
                );
                match service.dispatch_intake_task(&intake.task.id).await {
                    Ok(result) if result.started => {
                        last_project = result.task.project_id.clone().into();
                    }
                    Ok(_) => {}
                    Err(error) => {
                        tracing::warn!(
                            intake_task_id = %intake.task.id,
                            %error,
                            "global task scheduler could not dispatch intake work"
                        );
                    }
                }
            }
        });
    }

    pub async fn auto_resolve_project(&self, project_id: &str) {
        let sessions = match self
            .engine
            .list_sessions(Some(project_id.to_string()))
            .await
        {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(%error, "auto-resolve could not list project sessions");
                return;
            }
        };
        for session in sessions {
            self.auto_resolve_session(&session.id).await;
        }
    }

    pub async fn auto_resolve_all(&self) {
        self.scan_auto_resolve().await;
    }

    async fn scan_auto_resolve(&self) {
        let sessions = match self.engine.list_sessions(None).await {
            Ok(sessions) => sessions,
            Err(error) => {
                tracing::warn!(%error, "auto-resolve could not list sessions");
                return;
            }
        };
        for session in sessions {
            self.auto_resolve_session(&session.id).await;
        }
    }

    async fn auto_resolve_session(&self, session_id: &str) {
        let Ok((session, _)) = load_session_project(self.engine.store().clone(), session_id).await
        else {
            return;
        };
        if session.status != "idle" {
            return;
        }
        let policy = match self
            .engine
            .get_integration_policy(&session.project_id)
            .await
        {
            Ok(policy) => policy,
            Err(error) => {
                tracing::warn!(%error, session_id, "auto-resolve could not load policy");
                return;
            }
        };
        if !policy.auto_resolve || policy.max_auto_attempts == 0 {
            return;
        }
        if !self.info().configured {
            return;
        }
        if self
            .runs
            .lock()
            .map(|runs| runs.contains_key(session_id))
            .unwrap_or(true)
        {
            return;
        }
        let queue = match self.engine.list_merge_queue(session_id).await {
            Ok(queue) => queue,
            Err(error) => {
                tracing::warn!(%error, session_id, "auto-resolve could not list queue");
                return;
            }
        };
        for entry in queue.into_iter().filter(|entry| entry.status == "conflict") {
            let attempted = self
                .engine
                .auto_resolve_attempts(&entry.id)
                .await
                .unwrap_or(policy.max_auto_attempts);
            if attempted >= policy.max_auto_attempts {
                continue;
            }
            let attempted = match self.engine.bump_auto_resolve_attempts(&entry.id).await {
                Ok(attempted) => attempted,
                Err(error) => {
                    tracing::warn!(%error, entry_id = %entry.id, "auto-resolve could not record attempt");
                    continue;
                }
            };
            if attempted > policy.max_auto_attempts {
                continue;
            }
            let _ = self
                .engine
                .append_event(
                    session_id,
                    "merge_queue.auto_resolve_started",
                    serde_json::json!({
                        "entry_id": entry.id,
                        "attempt": attempted,
                        "max_auto_attempts": policy.max_auto_attempts,
                        "max_resolve_tokens": policy.max_resolve_tokens,
                    }),
                )
                .await;
            match self
                .resolve_merge_with_limit(&entry.id, policy.max_resolve_tokens)
                .await
            {
                Ok(resolved) => {
                    let _ = self
                        .engine
                        .append_event(
                            session_id,
                            "merge_queue.auto_resolve_finished",
                            serde_json::json!({
                                "entry_id": entry.id,
                                "status": resolved.status,
                                "detail": resolved.detail,
                            }),
                        )
                        .await;
                }
                Err(error) => {
                    let _ = self
                        .engine
                        .append_event(
                            session_id,
                            "merge_queue.auto_resolve_finished",
                            serde_json::json!({
                                "entry_id": entry.id,
                                "status": "failed",
                                "detail": error.to_string(),
                            }),
                        )
                        .await;
                }
            }
        }
    }

    pub async fn start(&self, session_id: &str, prompt: &str) -> Result<()> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        let Some(config) = config else {
            self.engine
                .append_event(
                    session_id,
                    "agent.unavailable",
                    serde_json::json!({
                        "message": "No model provider configured. Set VERLINPARK_BASE_URL and VERLINPARK_MODEL (and an API key if needed)."
                    }),
                )
                .await?;
            return Err(anyhow!(
                "agent provider is not configured; set VERLINPARK_BASE_URL and VERLINPARK_MODEL"
            ));
        };
        let (session, _) = load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!(
                "imported sessions are read-only archives; create a new session to continue"
            );
        }
        if session.status == "landed" {
            anyhow::bail!("landed sessions are final; create a new session or fork to continue");
        }

        let control = Arc::new(RunControl::new());
        {
            let mut runs = self
                .runs
                .lock()
                .map_err(|_| anyhow!("agent run mutex poisoned"))?;
            if runs.contains_key(session_id) {
                return Err(anyhow!("session is already running"));
            }
            runs.insert(session_id.to_string(), control.clone());
        }
        let lease = match self
            .engine
            .acquire_session_lease(session_id, std::time::Duration::from_secs(180))
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                if let Ok(mut runs) = self.runs.lock() {
                    runs.remove(session_id);
                }
                return Err(error);
            }
        };

        let prepared: Result<()> = async {
            self.engine
                .update_session_status(session_id, "queued")
                .await?;
            self.engine
                .append_message(session_id, "user", prompt, serde_json::json!([]), None)
                .await?;
            self.engine
                .append_event(
                    session_id,
                    "agent.queued",
                    serde_json::json!({"prompt": prompt}),
                )
                .await?;
            Ok(())
        }
        .await;
        if let Err(error) = prepared {
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(session_id);
            }
            let _ = self
                .engine
                .update_session_status(session_id, "failed")
                .await;
            lease.release().await;
            return Err(error);
        }

        self.spawn_run(
            config,
            session.id.clone(),
            session.project_id.clone(),
            session.priority,
            prompt.to_string(),
            control,
            lease,
        );
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_run(
        &self,
        config: AgentConfig,
        session_id: String,
        project_id: String,
        priority: i32,
        prompt: String,
        control: Arc<RunControl>,
        lease: crate::SessionLease,
    ) {
        let engine = self.engine.clone();
        let runs = self.runs.clone();
        let global_slots = self.global_slots.clone();
        tokio::spawn(async move {
            let permit = tokio::select! {
                permit = global_slots.acquire(&session_id, &project_id, priority, &control) => permit,
                _ = wait_for_cancel(&control) => None,
            };
            let Some(_permit) = permit else {
                let _ = engine
                    .append_event(&session_id, "agent.cancelled", serde_json::json!({}))
                    .await;
                let _ = control
                    .finalize_session(&engine, &session_id, "cancelled")
                    .await;
                if let Ok(mut runs) = runs.lock() {
                    runs.remove(&session_id);
                }
                lease.release().await;
                return;
            };
            if control.is_cancelled() {
                let _ = engine
                    .append_event(&session_id, "agent.cancelled", serde_json::json!({}))
                    .await;
                let _ = control
                    .finalize_session(&engine, &session_id, "cancelled")
                    .await;
                if let Ok(mut runs) = runs.lock() {
                    runs.remove(&session_id);
                }
                lease.release().await;
                return;
            }
            let status = if control.is_paused() {
                "paused"
            } else {
                "running"
            };
            let _ = engine.update_session_status(&session_id, status).await;
            let result = run_agent(
                engine.clone(),
                config,
                &session_id,
                &prompt,
                control.clone(),
            )
            .await;
            if let Err(error) = &result {
                let _ = engine
                    .append_event(
                        &session_id,
                        "agent.failed",
                        serde_json::json!({"error": format!("{error:#}")}),
                    )
                    .await;
                let _ = control
                    .finalize_session(&engine, &session_id, "failed")
                    .await;
            }
            if let Ok(mut runs) = runs.lock() {
                runs.remove(&session_id);
            }
            lease.release().await;
        });
    }

    /// Re-enqueue sessions that were admitted but had not started when the
    /// daemon restarted. Their original prompt is read from `agent.queued`.
    pub async fn resume_queued_sessions(&self) -> Result<usize> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        let Some(config) = config else {
            return Ok(0);
        };
        let store = self.engine.store().clone();
        let queued = tokio::task::spawn_blocking(move || store.queued_session_prompts()).await??;
        let mut started = 0;
        for (session_id, project_id, prompt, priority) in queued {
            let control = Arc::new(RunControl::new());
            {
                let mut runs = self
                    .runs
                    .lock()
                    .map_err(|_| anyhow!("agent run mutex poisoned"))?;
                if runs.contains_key(&session_id) {
                    continue;
                }
                runs.insert(session_id.clone(), control.clone());
            }
            let lease = match self
                .engine
                .acquire_session_lease(&session_id, std::time::Duration::from_secs(180))
                .await
            {
                Ok(lease) => lease,
                Err(error) => {
                    if let Ok(mut runs) = self.runs.lock() {
                        runs.remove(&session_id);
                    }
                    tracing::warn!(%error, %session_id, "could not acquire queued session lease");
                    continue;
                }
            };
            self.spawn_run(
                config.clone(),
                session_id,
                project_id,
                priority,
                prompt,
                control,
                lease,
            );
            started += 1;
        }
        Ok(started)
    }

    /// Serialize a checkpoint with prompt/plan/compaction operations. The
    /// checkpoint itself is provider-free but reads Git state and messages, so
    /// it must not race a run.
    pub async fn checkpoint_now(
        &self,
        session_id: &str,
        label: Option<String>,
    ) -> Result<SessionCheckpoint> {
        self.begin_operation(session_id)?;
        let result = self.engine.checkpoint_session(session_id, label).await;
        self.end_operation(session_id);
        result
    }

    /// Serialize a worktree checkpoint restore with prompts/plans/compaction.
    pub async fn restore_checkpoint_now(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointRestoreResult> {
        self.begin_operation(session_id)?;
        let result = self
            .engine
            .restore_checkpoint(session_id, checkpoint_id)
            .await;
        self.end_operation(session_id);
        result
    }

    /// Serialize a checkpoint diff preview with prompts/plans/compaction.
    pub async fn diff_checkpoint_now(
        &self,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<SessionCheckpointDiffResult> {
        self.begin_operation(session_id)?;
        let result = self.engine.diff_checkpoint(session_id, checkpoint_id).await;
        self.end_operation(session_id);
        result
    }

    /// Serialize a fork with prompt/plan/compaction operations on the source
    /// session. The new session is created only after the source is quiescent.
    pub async fn fork_now(
        &self,
        source_session_id: &str,
        checkpoint_id: Option<&str>,
        goal: Option<String>,
    ) -> Result<SessionForkResult> {
        self.begin_operation(source_session_id)?;
        let result = self
            .engine
            .fork_session(source_session_id, checkpoint_id, goal)
            .await;
        self.end_operation(source_session_id);
        result
    }

    /// Merge a direct fork's committed branch into its source session. The
    /// source worktree must be clean; conflicts are aborted and reported.
    pub async fn merge_fork_now(
        &self,
        source_session_id: &str,
        fork_session_id: &str,
    ) -> Result<SessionMergeForkResult> {
        self.begin_operations(&[source_session_id, fork_session_id])?;
        let result = async {
            let integration_command = self.config.read().ok().and_then(|config| {
                config
                    .as_ref()
                    .and_then(|config| config.integration_command.clone())
            });
            let (source, project) =
                load_session_project(self.engine.store().clone(), source_session_id).await?;
            let (fork, fork_project) =
                load_session_project(self.engine.store().clone(), fork_session_id).await?;
            if source.project_id != fork.project_id || project.id != fork_project.id {
                anyhow::bail!("sessions must belong to the same project");
            }
            crate::ensure_session_quiescent(&source, "merge a fork")?;
            crate::ensure_session_quiescent(&fork, "merge a fork")?;
            let lineage = self.engine.session_lineage(fork_session_id).await?;
            if lineage
                .parent
                .as_ref()
                .map(|parent| parent.session.id.as_str())
                != Some(source_session_id)
            {
                anyhow::bail!("fork is not a direct child of the source session");
            }
            self.merge_fork_inner(&source, &project, &fork, integration_command)
                .await
        }
        .await;
        self.end_operations(&[source_session_id, fork_session_id]);
        result
    }

    async fn merge_fork_inner(
        &self,
        source: &Session,
        project: &Project,
        fork: &Session,
        integration_command: Option<String>,
    ) -> Result<SessionMergeForkResult> {
        let source_worktree = PathBuf::from(
            source
                .worktree
                .clone()
                .ok_or_else(|| anyhow!("source session has no worktree"))?,
        );
        let source_branch = source
            .branch
            .clone()
            .ok_or_else(|| anyhow!("source session has no branch"))?;
        let fork_branch = fork
            .branch
            .clone()
            .ok_or_else(|| anyhow!("fork session has no branch"))?;
        if !source_worktree.is_dir() {
            anyhow::bail!("source session worktree is missing");
        }
        let git_lock = self.engine.git_lock.clone();
        let status_path = source_worktree.clone();
        let branch_path = source_worktree.clone();
        let head_path = source_worktree.clone();
        let expected_branch = source_branch.clone();
        let (status, branch, pre_merge_commit) = tokio::task::spawn_blocking(move || {
            let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
            Ok::<_, anyhow::Error>((
                worktree::status_porcelain(&status_path)?,
                worktree::current_branch(&branch_path)?,
                worktree::head_commit(&head_path)?,
            ))
        })
        .await??;
        if branch != expected_branch {
            anyhow::bail!("source worktree is on branch {branch}; expected {expected_branch}");
        }
        if !status.is_empty() {
            anyhow::bail!(
                "source worktree has uncommitted changes; commit or discard them before merging a fork"
            );
        }
        if let Some(fork_worktree) = &fork.worktree {
            let fork_path = PathBuf::from(fork_worktree);
            if fork_path.is_dir() {
                let git_lock = self.engine.git_lock.clone();
                let status = tokio::task::spawn_blocking(move || {
                    let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
                    worktree::status_porcelain(&fork_path)
                })
                .await??;
                if !status.is_empty() {
                    anyhow::bail!(
                        "fork worktree has uncommitted changes; commit them before merging"
                    );
                }
            }
        }
        let root = PathBuf::from(&project.root);
        let fork_branch_for_git = fork_branch.clone();
        let root_for_git = root.clone();
        let fork_commit = tokio::task::spawn_blocking(move || {
            worktree::git_output(&root_for_git, &["rev-parse", &fork_branch_for_git])
        })
        .await??;
        let message = format!("merge fork {}", fork.id);
        let merge_worktree = source_worktree.clone();
        let merge_lock = self.engine.git_lock.clone();
        let merge_outcome = tokio::task::spawn_blocking(move || {
            let _guard = merge_lock
                .lock()
                .map_err(|_| anyhow!("git mutex poisoned"))?;
            worktree::merge_fork_branch(&merge_worktree, &fork_branch, &message)
        })
        .await??;
        if !merge_outcome.merged {
            let detail = truncate(&format!("merge conflict: {}", merge_outcome.detail));
            self.record_fork_merge(
                source,
                fork,
                "conflict",
                &pre_merge_commit,
                &fork_commit,
                None,
                &merge_outcome.conflicts,
                &detail,
            )
            .await?;
            return Ok(SessionMergeForkResult {
                source_session_id: source.id.clone(),
                fork_session_id: fork.id.clone(),
                status: "conflict".to_string(),
                pre_merge_commit,
                fork_commit,
                merge_commit: None,
                conflicts: merge_outcome.conflicts,
                detail,
            });
        }

        let merge_worktree = source_worktree.clone();
        let check_base = pre_merge_commit.clone();
        let check_lock = self.engine.git_lock.clone();
        let check = tokio::task::spawn_blocking(move || {
            let _guard = check_lock
                .lock()
                .map_err(|_| anyhow!("git mutex poisoned"))?;
            worktree::check_merged_diff(&merge_worktree, &check_base)
        })
        .await?;
        let integration = match check {
            Ok(()) => {
                if let Some(command) = integration_command {
                    run_integration_command(
                        &self.engine,
                        project,
                        source,
                        source_worktree.clone(),
                        command.clone(),
                    )
                    .await
                } else {
                    Ok("diff check passed".to_string())
                }
            }
            Err(error) => Err(error),
        };
        let integration = match integration {
            Ok(detail) => detail,
            Err(error) => {
                let reset_worktree = source_worktree.clone();
                let reset_base = pre_merge_commit.clone();
                let reset_lock = self.engine.git_lock.clone();
                let reset = tokio::task::spawn_blocking(move || {
                    let _guard = reset_lock
                        .lock()
                        .map_err(|_| anyhow!("git mutex poisoned"))?;
                    worktree::reset_hard(&reset_worktree, &reset_base)
                })
                .await?;
                let detail = truncate(&match reset {
                    Ok(()) => format!("integration check failed: {error}"),
                    Err(reset_error) => {
                        format!("integration check failed: {error}; rollback failed: {reset_error}")
                    }
                });
                self.record_fork_merge(
                    source,
                    fork,
                    "failed",
                    &pre_merge_commit,
                    &fork_commit,
                    None,
                    &[],
                    &detail,
                )
                .await?;
                return Ok(SessionMergeForkResult {
                    source_session_id: source.id.clone(),
                    fork_session_id: fork.id.clone(),
                    status: "failed".to_string(),
                    pre_merge_commit,
                    fork_commit,
                    merge_commit: None,
                    conflicts: Vec::new(),
                    detail,
                });
            }
        };
        let head_path = source_worktree.clone();
        let merge_commit =
            tokio::task::spawn_blocking(move || worktree::head_commit(&head_path)).await??;
        let detail = truncate(&integration);
        self.record_fork_merge(
            source,
            fork,
            "merged",
            &pre_merge_commit,
            &fork_commit,
            Some(&merge_commit),
            &[],
            &detail,
        )
        .await?;
        Ok(SessionMergeForkResult {
            source_session_id: source.id.clone(),
            fork_session_id: fork.id.clone(),
            status: "merged".to_string(),
            pre_merge_commit,
            fork_commit,
            merge_commit: Some(merge_commit),
            conflicts: Vec::new(),
            detail,
        })
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_fork_merge(
        &self,
        source: &Session,
        fork: &Session,
        status: &str,
        pre_merge_commit: &str,
        fork_commit: &str,
        merge_commit: Option<&str>,
        conflicts: &[String],
        detail: &str,
    ) -> Result<()> {
        self.engine
            .append_event(
                &source.id,
                "session.fork.merge",
                serde_json::json!({
                    "fork_session_id": fork.id,
                    "status": status,
                    "pre_merge_commit": pre_merge_commit,
                    "fork_commit": fork_commit,
                    "merge_commit": merge_commit,
                    "conflicts": conflicts,
                    "detail": detail,
                }),
            )
            .await?;
        let _ = self
            .engine
            .append_event(
                &fork.id,
                "session.fork.merge",
                serde_json::json!({
                    "source_session_id": source.id,
                    "status": status,
                    "merge_commit": merge_commit,
                }),
            )
            .await;
        Ok(())
    }

    fn begin_manual_operation(&self, session_id: &str) -> Result<Arc<RunControl>> {
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?;
        if runs.contains_key(session_id) {
            anyhow::bail!("session is already running");
        }
        let control = Arc::new(RunControl::new_manual());
        runs.insert(session_id.to_string(), control.clone());
        Ok(control)
    }

    fn begin_operation(&self, session_id: &str) -> Result<()> {
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?;
        if runs.contains_key(session_id) {
            anyhow::bail!("session is already running");
        }
        runs.insert(
            session_id.to_string(),
            Arc::new(RunControl::new_operation()),
        );
        Ok(())
    }

    fn end_operation(&self, session_id: &str) {
        if let Ok(mut runs) = self.runs.lock() {
            runs.remove(session_id);
        }
    }

    fn begin_operations(&self, session_ids: &[&str]) -> Result<()> {
        let mut ids = session_ids.to_vec();
        ids.sort_unstable();
        ids.dedup();
        let mut runs = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?;
        for id in &ids {
            if runs.contains_key(*id) {
                anyhow::bail!("session is already running: {id}");
            }
        }
        for id in ids {
            runs.insert(id.to_string(), Arc::new(RunControl::new_operation()));
        }
        Ok(())
    }

    fn end_operations(&self, session_ids: &[&str]) {
        if let Ok(mut runs) = self.runs.lock() {
            for id in session_ids {
                runs.remove(*id);
            }
        }
    }

    /// Apply a user-requested plan revision with the configured Manager cap.
    pub async fn revise_team_plan(
        &self,
        session_id: &str,
        plan: TeamPlan,
        reason: &str,
        expected_revision: Option<u32>,
    ) -> Result<ReviseTeamPlanResult> {
        let max_managers = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().map(|config| config.max_managers))
            .unwrap_or(5);
        self.engine
            .revise_team_plan(
                session_id,
                plan,
                reason,
                "user",
                expected_revision,
                max_managers,
            )
            .await
    }

    pub async fn plan_revision_detail(
        &self,
        session_id: &str,
        revision: u32,
    ) -> Result<PlanRevisionDetailResult> {
        self.engine.plan_revision_detail(session_id, revision).await
    }

    pub async fn restore_team_plan_revision(
        &self,
        session_id: &str,
        revision: u32,
        reason: &str,
        expected_revision: Option<u32>,
    ) -> Result<ReviseTeamPlanResult> {
        let max_managers = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().map(|config| config.max_managers))
            .unwrap_or(5);
        self.engine
            .restore_team_plan_revision(
                session_id,
                revision,
                reason,
                expected_revision,
                max_managers,
            )
            .await
    }

    pub async fn edit_plan_task(
        &self,
        session_id: &str,
        task: vp_protocol::types::PlanTask,
        reason: &str,
        expected_revision: Option<u32>,
    ) -> Result<ReviseTeamPlanResult> {
        let max_managers = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().map(|config| config.max_managers))
            .unwrap_or(5);
        self.engine
            .edit_plan_task(session_id, task, reason, expected_revision, max_managers)
            .await
    }

    /// Resume a stored TeamPlan without another Leader model call. Completed
    /// tasks are skipped; failed/blocked/cancelled/pending tasks are retried in
    /// dependency batches.
    pub async fn resume_plan(&self, session_id: &str) -> Result<bool> {
        self.start_plan_run(session_id, None).await
    }

    /// Force a summary/trim of an idle session's active conversation. This is
    /// the explicit counterpart to automatic compaction: it never starts a
    /// prompt, and it refuses to run while the session is active so the
    /// in-memory conversation of a live run cannot diverge from the event log.
    pub async fn compact_now(&self, session_id: &str) -> Result<SessionCompactResult> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned())
            .ok_or_else(|| anyhow!("agent provider is not configured"))?;
        let (session, project) =
            load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!("imported sessions are read-only archives");
        }
        if matches!(
            session.status.as_str(),
            "queued" | "running" | "paused" | "waiting_input"
        ) {
            anyhow::bail!(
                "session must not be queued, running, paused, or waiting for input while compacting context"
            );
        }
        // Reuse the run registry so a prompt cannot start while the
        // compaction call is in flight. The manual control is cancellable but
        // not pausable.
        let control = self.begin_manual_operation(session_id)?;
        let result = async {
            let agents = self.engine.list_agents(session_id).await?;
            let leader = agents.into_iter().find(|agent| agent.role == "leader");
            let profile = leader
                .as_ref()
                .and_then(|agent| agent.profile.as_deref())
                .and_then(AgentProfile::parse)
                .unwrap_or(AgentProfile::Generic);
            let worktree = PathBuf::from(session.worktree.clone().unwrap_or_default());
            let branch = session.branch.clone().unwrap_or_default();
            let spec = AgentSpec {
                id: leader
                    .as_ref()
                    .map(|agent| agent.id.clone())
                    .unwrap_or_else(|| "leader".to_string()),
                role: AgentRole::Leader,
                name: leader
                    .as_ref()
                    .map(|agent| agent.name.clone())
                    .unwrap_or_else(|| "Leader".to_string()),
                depth: 0,
                parent_name: None,
                title: leader.as_ref().and_then(|agent| agent.title.clone()),
                task_id: None,
                department: None,
                profile,
                allow_delegation: true,
                auto_approve: false,
                worktree: worktree.clone(),
                branch: branch.clone(),
                session_worktree: worktree,
                session_branch: branch,
                write_scope: Vec::new(),
                base_commit: session.base_commit.clone().unwrap_or_default(),
            };
            let system = role_system_prompt(spec.role, &project, &session, &spec, &session.goal);
            let mut tools = tool_schemas(&config, spec.role, spec.profile, spec.allow_delegation);
            tools.extend(
                collect_dynamic_mcp_tools(&self.engine, spec.role)
                    .await
                    .iter()
                    .map(|binding| binding.schema.clone()),
            );
            let model_name = model_for(&config, spec.role);
            let calibration = load_context_calibration(&self.engine, session_id, &model_name).await;
            let system_tokens = estimate_context_tokens(&system);
            let tool_tokens = serde_json::to_string(&tools)
                .map(|schema| estimate_context_tokens(&schema))
                .unwrap_or(0);
            let (threshold, hard_limit) = context_limits(&config, spec.role);
            let loaded = load_conversation(&self.engine, session_id).await?;
            let mut conversation = loaded.messages;
            let mut conversation_ids = loaded.ids;
            let before = self
                .engine
                .latest_event_of_kind(session_id, "context.compacted")
                .await?;
            let failed_before = self
                .engine
                .latest_event_of_kind(session_id, "context.compact_failed")
                .await?;
            let budget = SessionBudget::new(
                session.token_budget,
                session.tokens_in + session.tokens_out,
                AgentCaps {
                    managers: config.max_managers,
                    workers: config.max_workers,
                    labour: config.max_labour,
                },
            )
            .with_cost(session.cost_budget_usd, session.cost_spent_usd)
            .with_time(session.time_budget_secs)
            .with_unlimited_flag(self.engine.unlimited_flag(&session.id, session.unlimited));
            if budget.exhausted() {
                anyhow::bail!("session budget is exhausted; compaction needs provider tokens");
            }
            let changed = compact_context_if_needed(
                &self.engine,
                &config,
                &session,
                &spec,
                &budget,
                &control,
                &mut conversation,
                &mut conversation_ids,
                threshold,
                hard_limit,
                system_tokens.saturating_add(tool_tokens),
                calibration.factor,
                true,
                context_strategy(spec.role),
            )
            .await?;
            if changed {
                let context_window = context_window_for(&config, spec.role);
                if context_window > 0 {
                    emit(
                        &self.engine,
                        session_id,
                        &spec,
                        "context.usage",
                        serde_json::json!({
                            "estimated_tokens": calibrate_context_tokens(
                                estimate_messages_tokens(&conversation)
                                    .saturating_add(system_tokens)
                                    .saturating_add(tool_tokens),
                                calibration.factor,
                            ),
                            "context_window": context_window,
                            "threshold_tokens": threshold,
                            "hard_limit_tokens": hard_limit,
                            "messages": conversation.len(),
                            "calibration": calibration.factor,
                        }),
                    )
                    .await?;
                }
            }
            if !changed
                && let Some(event) = self
                    .engine
                    .latest_event_of_kind(session_id, "context.compact_failed")
                    .await?
                && failed_before
                    .as_ref()
                    .is_none_or(|before| event.seq > before.seq)
            {
                let message = event
                    .payload
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("context compaction failed");
                anyhow::bail!("context compaction failed: {message}");
            }
            let compaction = if changed {
                self.engine
                    .latest_event_of_kind(session_id, "context.compacted")
                    .await?
                    .filter(|event| before.as_ref().is_none_or(|before| event.seq > before.seq))
                    .and_then(|event| {
                        serde_json::from_value::<ContextCompactionSummary>(event.payload.clone())
                            .ok()
                            .map(|mut summary| {
                                summary.created_at = event.created_at;
                                summary
                            })
                    })
            } else {
                None
            };
            Ok(SessionCompactResult {
                session_id: session.id.clone(),
                compacted: changed,
                compaction,
            })
        }
        .await;
        self.end_operation(session_id);
        result
    }

    /// Dispatch exactly one ready task from a stored TeamPlan. The session is
    /// released after the task settles so the global scheduler can make fair
    /// progress across sessions instead of running an entire plan at once.
    pub async fn dispatch_next_task(&self, session_id: &str) -> Result<bool> {
        self.start_plan_run(session_id, Some(1)).await
    }

    async fn start_plan_run(&self, session_id: &str, max_tasks: Option<usize>) -> Result<bool> {
        let config = self
            .config
            .read()
            .ok()
            .and_then(|config| config.as_ref().cloned());
        let Some(config) = config else {
            anyhow::bail!("agent provider is not configured");
        };
        let (session, _) = load_session_project(self.engine.store().clone(), session_id).await?;
        if session.status == "imported" {
            anyhow::bail!(
                "imported sessions are read-only archives; rehydrate before resuming a plan"
            );
        }
        if matches!(
            session.status.as_str(),
            "queued" | "running" | "paused" | "landed"
        ) {
            anyhow::bail!("session must be idle, failed, cancelled, or draft to resume a plan");
        }
        let Some(plan) = self.engine.get_team_plan(session_id).await? else {
            anyhow::bail!("session has no TeamPlan");
        };
        let (_, scheduled) = self.engine.scheduler().sync_plan(&session, &plan).await?;
        let mut terminal = scheduled
            .iter()
            .filter(|task| matches!(task.status.as_str(), "completed" | "skipped"))
            .map(|task| task.task_id.clone())
            .collect::<std::collections::HashSet<_>>();
        // Tasks absent from a just-synced queue are treated as pending.
        for task in &plan.tasks {
            if !scheduled.iter().any(|row| row.task_id == task.id) {
                terminal.remove(&task.id);
            }
        }
        if plan.tasks.iter().all(|task| terminal.contains(&task.id)) {
            anyhow::bail!("plan is already complete or skipped");
        }

        let control = Arc::new(RunControl::new());
        {
            let mut runs = self
                .runs
                .lock()
                .map_err(|_| anyhow!("agent run mutex poisoned"))?;
            if runs.contains_key(session_id) {
                return Err(anyhow!("session is already running"));
            }
            runs.insert(session_id.to_string(), control.clone());
        }
        let lease = match self
            .engine
            .acquire_session_lease(session_id, std::time::Duration::from_secs(180))
            .await
        {
            Ok(lease) => lease,
            Err(error) => {
                if let Ok(mut runs) = self.runs.lock() {
                    runs.remove(session_id);
                }
                return Err(error);
            }
        };

        let prepared: Result<()> = async {
            self.engine
                .update_session_status(session_id, "queued")
                .await?;
            let (event_kind, mut payload) = if max_tasks.is_some() {
                (
                    "agent.task_dispatch_queued",
                    serde_json::json!({
                        "goal": plan.goal,
                        "remaining_tasks": plan.tasks.len().saturating_sub(terminal.len()),
                        "task_limit": 1,
                    }),
                )
            } else {
                (
                    "agent.plan_resume_queued",
                    serde_json::json!({
                        "goal": plan.goal,
                        "remaining_tasks": plan.tasks.len().saturating_sub(terminal.len()),
                    }),
                )
            };
            if let Some(object) = payload.as_object_mut() {
                object.insert("task_limit".into(), serde_json::json!(max_tasks));
            }
            self.engine
                .append_event(session_id, event_kind, payload)
                .await?;
            Ok(())
        }
        .await;
        if let Err(error) = prepared {
            if let Ok(mut runs) = self.runs.lock() {
                runs.remove(session_id);
            }
            let _ = self
                .engine
                .update_session_status(session_id, "failed")
                .await;
            lease.release().await;
            return Err(error);
        }

        let engine = self.engine.clone();
        let session_id = session_id.to_string();
        let project_id = session.project_id.clone();
        let priority = session.priority;
        let runs = self.runs.clone();
        let global_slots = self.global_slots.clone();
        tokio::spawn(async move {
            let permit = tokio::select! {
                permit = global_slots.acquire(&session_id, &project_id, priority, &control) => permit,
                _ = wait_for_cancel(&control) => None,
            };
            let Some(_permit) = permit else {
                let _ = engine
                    .append_event(&session_id, "agent.cancelled", serde_json::json!({}))
                    .await;
                let _ = control
                    .finalize_session(&engine, &session_id, "cancelled")
                    .await;
                if let Ok(mut runs) = runs.lock() {
                    runs.remove(&session_id);
                }
                lease.release().await;
                return;
            };
            if control.is_cancelled() {
                let _ = engine
                    .append_event(&session_id, "agent.cancelled", serde_json::json!({}))
                    .await;
                let _ = control
                    .finalize_session(&engine, &session_id, "cancelled")
                    .await;
                if let Ok(mut runs) = runs.lock() {
                    runs.remove(&session_id);
                }
                lease.release().await;
                return;
            }
            let status = if control.is_paused() {
                "paused"
            } else {
                "running"
            };
            let _ = engine.update_session_status(&session_id, status).await;
            let result = run_plan_resume(
                engine.clone(),
                config,
                &session_id,
                control.clone(),
                max_tasks,
            )
            .await;
            if let Err(error) = &result {
                let _ = engine
                    .append_event(
                        &session_id,
                        "agent.failed",
                        serde_json::json!({"error": format!("{error:#}")}),
                    )
                    .await;
                let _ = control
                    .finalize_session(&engine, &session_id, "failed")
                    .await;
            }
            if let Ok(mut runs) = runs.lock() {
                runs.remove(&session_id);
            }
            lease.release().await;
        });
        Ok(true)
    }

    pub fn cancel(&self, session_id: &str) -> bool {
        if let Ok(runs) = self.runs.lock()
            && let Some(control) = runs.get(session_id)
        {
            if !control.cancellable {
                return false;
            }
            control.cancel();
            return true;
        }
        false
    }

    pub async fn pause(&self, session_id: &str) -> Result<bool> {
        let control = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?
            .get(session_id)
            .cloned();
        let Some(control) = control else {
            return Ok(false);
        };
        if control.manual {
            // Manual context compaction is a short foreground operation and
            // has no resumable run loop; pausing it would leave the session
            // stuck in `paused` after the compaction finishes.
            return Ok(false);
        }
        if control.is_paused() {
            return Ok(true);
        }
        let finalized = control.finalized.lock().await;
        if finalized.is_some() {
            return Ok(false);
        }
        control.pause();
        self.engine
            .update_session_status(session_id, "paused")
            .await?;
        self.engine
            .append_event(session_id, "agent.paused", serde_json::json!({}))
            .await?;
        drop(finalized);
        Ok(true)
    }

    pub async fn resume(&self, session_id: &str) -> Result<bool> {
        let control = self
            .runs
            .lock()
            .map_err(|_| anyhow!("agent run mutex poisoned"))?
            .get(session_id)
            .cloned();
        let Some(control) = control else {
            return Ok(false);
        };
        if !control.is_paused() {
            return Ok(true);
        }
        let finalized = control.finalized.lock().await;
        if finalized.is_some() {
            return Ok(false);
        }
        control.resume();
        self.engine
            .update_session_status(session_id, "running")
            .await?;
        self.engine
            .append_event(session_id, "agent.resumed", serde_json::json!({}))
            .await?;
        drop(finalized);
        Ok(true)
    }
}

async fn run_plan_resume(
    engine: Engine,
    config: AgentConfig,
    session_id: &str,
    control: Arc<RunControl>,
    max_tasks: Option<usize>,
) -> Result<()> {
    let store = engine.store().clone();
    let (_, project) = load_session_project(store, session_id).await?;
    let session = engine.ensure_session_worktree(session_id).await?;
    let plan = engine
        .get_team_plan(session_id)
        .await?
        .ok_or_else(|| anyhow!("session has no TeamPlan"))?;
    let worktree = PathBuf::from(
        session
            .worktree
            .clone()
            .ok_or_else(|| anyhow!("session has no worktree"))?,
    );
    let session_branch = session.branch.clone().unwrap_or_default();
    let leader = engine
        .list_agents(session_id)
        .await?
        .into_iter()
        .find(|agent| agent.role == "leader");
    let leader_id = leader.as_ref().map(|agent| agent.id.clone());
    let spec = AgentSpec {
        id: leader_id.clone().unwrap_or_else(|| "leader".to_string()),
        role: AgentRole::Leader,
        name: leader
            .map(|agent| agent.name)
            .unwrap_or_else(|| "Leader".to_string()),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        auto_approve: false,
        worktree: worktree.clone(),
        branch: session_branch.clone(),
        session_worktree: worktree.clone(),
        session_branch,
        write_scope: Vec::new(),
        base_commit: session.base_commit.clone().unwrap_or_default(),
    };
    let budget = SessionBudget::new(
        session.token_budget,
        session.tokens_in + session.tokens_out,
        AgentCaps {
            managers: config.max_managers,
            workers: config.max_workers,
            labour: config.max_labour,
        },
    )
    .with_cost(session.cost_budget_usd, session.cost_spent_usd)
    .with_time(session.time_budget_secs)
    .with_unlimited_flag(engine.unlimited_flag(&session.id, session.unlimited));
    if budget.exhausted() {
        emit(
            &engine,
            session_id,
            &spec,
            "agent.budget_exceeded",
            serde_json::json!({
                "reason": budget.exceeded_reason().unwrap_or("tokens"),
                "limit": session.token_budget,
                "spent": budget.spent(),
                "cost_limit_usd": session.cost_budget_usd,
                "cost_spent_usd": budget.cost_spent(),
                "time_budget_secs": session.time_budget_secs,
                "elapsed_secs": budget.elapsed_secs(),
            }),
        )
        .await?;
        control
            .finalize_session(&engine, session_id, "failed")
            .await?;
        return Ok(());
    }
    if let Some(agent_id) = &leader_id {
        engine.update_agent_status(agent_id, "running").await?;
    }
    let event_kind = if max_tasks.is_some() {
        "agent.task_dispatched"
    } else {
        "agent.plan_resumed"
    };
    emit(
        &engine,
        session_id,
        &spec,
        event_kind,
        serde_json::json!({
            "goal": plan.goal,
            "model": model_for(&config, spec.role),
            "provider": config.provider_name,
            "worktree": session.worktree,
            "branch": session.branch,
            "task_limit": max_tasks,
        }),
    )
    .await?;

    let result = execute_team_plan(
        &engine, &config, &session, &project, &control, &budget, &spec, &plan, max_tasks,
    )
    .await;
    match result? {
        PlanRunResult::Done(summary) => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "idle").await?;
            }
            emit(
                &engine,
                session_id,
                &spec,
                "agent.message",
                serde_json::json!({"text": summary}),
            )
            .await?;
            control
                .finalize_session(&engine, session_id, "idle")
                .await?;
        }
        PlanRunResult::Cancelled => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "cancelled").await?;
            }
            emit(
                &engine,
                session_id,
                &spec,
                "agent.cancelled",
                serde_json::json!({"resumed_plan": true}),
            )
            .await?;
            control
                .finalize_session(&engine, session_id, "cancelled")
                .await?;
        }
        PlanRunResult::BudgetExceeded(stop) => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "failed").await?;
            }
            let mut payload = stop.payload();
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "time_budget_secs".into(),
                    serde_json::json!(session.time_budget_secs),
                );
            }
            emit(&engine, session_id, &spec, "agent.budget_exceeded", payload).await?;
            control
                .finalize_session(&engine, session_id, "failed")
                .await?;
        }
    }
    Ok(())
}

async fn run_agent(
    engine: Engine,
    config: AgentConfig,
    session_id: &str,
    prompt: &str,
    control: Arc<RunControl>,
) -> Result<()> {
    let store = engine.store().clone();
    let (_, project) = load_session_project(store.clone(), session_id).await?;
    let session = engine.ensure_session_worktree(session_id).await?;
    let worktree = PathBuf::from(
        session
            .worktree
            .clone()
            .ok_or_else(|| anyhow!("session has no worktree"))?,
    );
    let session_branch = session.branch.clone().unwrap_or_default();
    let leader = engine
        .list_agents(session_id)
        .await?
        .into_iter()
        .find(|agent| agent.role == "leader");
    let leader_id = leader.as_ref().map(|agent| agent.id.clone());
    let spec = AgentSpec {
        id: leader_id.clone().unwrap_or_else(|| "leader".to_string()),
        role: AgentRole::Leader,
        name: leader
            .map(|agent| agent.name)
            .unwrap_or_else(|| "Leader".to_string()),
        depth: 0,
        parent_name: None,
        title: None,
        task_id: None,
        department: None,
        profile: AgentProfile::Generic,
        allow_delegation: true,
        auto_approve: false,
        worktree: worktree.clone(),
        branch: session_branch.clone(),
        session_worktree: worktree.clone(),
        session_branch,
        write_scope: Vec::new(),
        base_commit: session.base_commit.clone().unwrap_or_default(),
    };
    let budget = SessionBudget::new(
        session.token_budget,
        session.tokens_in + session.tokens_out,
        AgentCaps {
            managers: config.max_managers,
            workers: config.max_workers,
            labour: config.max_labour,
        },
    )
    .with_cost(session.cost_budget_usd, session.cost_spent_usd)
    .with_time(session.time_budget_secs)
    .with_unlimited_flag(engine.unlimited_flag(&session.id, session.unlimited));
    if budget.exhausted() {
        emit(
            &engine,
            session_id,
            &spec,
            "agent.budget_exceeded",
            serde_json::json!({
                "reason": budget.exceeded_reason().unwrap_or("tokens"),
                "limit": session.token_budget,
                "spent": budget.spent(),
                "cost_limit_usd": session.cost_budget_usd,
                "cost_spent_usd": budget.cost_spent(),
                "time_budget_secs": session.time_budget_secs,
                "elapsed_secs": budget.elapsed_secs(),
            }),
        )
        .await?;
        engine.update_session_status(session_id, "failed").await?;
        return Ok(());
    }
    if let Some(agent_id) = &leader_id {
        engine.update_agent_status(agent_id, "running").await?;
    }

    emit(
        &engine,
        session_id,
        &spec,
        "agent.started",
        serde_json::json!({
            "goal": session.goal,
            "prompt": prompt,
            "model": model_for(&config, spec.role),
            "provider": config.provider_name,
            "worktree": session.worktree,
            "branch": session.branch,
        }),
    )
    .await?;

    let loaded = load_conversation(&engine, session_id).await?;
    let mut conversation = loaded.messages;
    let mut conversation_ids = loaded.ids;
    let result = run_loop(
        &engine,
        &config,
        &session,
        &project,
        &control,
        &budget,
        &spec,
        prompt,
        &mut conversation,
        Some(&mut conversation_ids),
        true,
    )
    .await;
    if result.is_err()
        && let Some(agent_id) = &leader_id
    {
        let _ = engine.update_agent_status(agent_id, "failed").await;
    }
    match result? {
        LoopOutcome::Final(_) => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "idle").await?;
            }
            control
                .finalize_session(&engine, session_id, "idle")
                .await?;
        }
        LoopOutcome::Cancelled => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "cancelled").await?;
            }
            engine
                .append_event(session_id, "agent.cancelled", serde_json::json!({}))
                .await?;
            control
                .finalize_session(&engine, session_id, "cancelled")
                .await?;
        }
        LoopOutcome::BudgetExceeded(stop) => {
            if let Some(agent_id) = &leader_id {
                engine.update_agent_status(agent_id, "failed").await?;
            }
            let mut payload = stop.payload();
            if let Some(object) = payload.as_object_mut() {
                object.insert(
                    "time_budget_secs".into(),
                    serde_json::json!(session.time_budget_secs),
                );
            }
            emit(&engine, session_id, &spec, "agent.budget_exceeded", payload).await?;
            control
                .finalize_session(&engine, session_id, "failed")
                .await?;
        }
        LoopOutcome::Verdict(_) => {
            emit(
                &engine,
                session_id,
                &spec,
                "agent.failed",
                serde_json::json!({"error": "a verdict was submitted outside a verification run"}),
            )
            .await?;
            control
                .finalize_session(&engine, session_id, "failed")
                .await?;
        }
    }
    Ok(())
}

// Token counts are only exact after a provider reports usage. To keep parallel
// calls from all passing the same pre-flight check, reserve a conservative
// estimate up front: roughly two UTF-8 bytes per token, plus a small per-request
// overhead. The provider's reported usage replaces the estimate on settlement.
fn estimate_text_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(2).max(1)
}

fn estimate_output_tokens(text: &str, tool_calls: &[ToolCall]) -> u64 {
    estimate_text_tokens(text).saturating_add(
        serde_json::to_string(tool_calls)
            .map(|calls| estimate_text_tokens(&calls))
            .unwrap_or(0),
    )
}

/// Context-window estimates should be closer to the tokenizer than the
/// conservative budget reservation above. English/code averages ~4 ASCII
/// characters per token; CJK and other non-ASCII text is roughly 1.5 tokens
/// per character. This avoids compacting a 1M-token model at half its usable
/// window while still leaving headroom for tokenizer differences.
fn estimate_context_tokens(text: &str) -> u64 {
    let mut ascii = 0u64;
    let mut non_ascii = 0u64;
    for character in text.chars() {
        if character.is_ascii() {
            ascii = ascii.saturating_add(1);
        } else {
            non_ascii = non_ascii.saturating_add(1);
        }
    }
    ascii
        .div_ceil(4)
        .saturating_add(non_ascii.saturating_mul(3).div_ceil(2))
        .max(1)
}

const MAX_CONTEXT_CALIBRATION: f64 = 2.0;

/// Provider usage is the only exact token count we get. The heuristic estimate
/// is usually close, but CJK text, tool schemas, and provider-side formatting
/// can make it low. Calibration therefore only scales estimates upward: a
/// provider that reports fewer input tokens than the heuristic never makes
/// compaction later than it already was.
#[derive(Clone, Copy, Debug)]
struct ContextCalibration {
    factor: f64,
    ratio: f64,
    samples: u64,
}

impl Default for ContextCalibration {
    fn default() -> Self {
        Self {
            factor: 1.0,
            ratio: 1.0,
            samples: 0,
        }
    }
}

impl ContextCalibration {
    fn observe(&mut self, estimated: u64, actual: u64) {
        if estimated == 0 || actual == 0 {
            return;
        }
        let sample = (actual as f64 / estimated as f64).clamp(1.0, 4.0);
        self.ratio = if self.samples == 0 {
            sample
        } else {
            (self.ratio * 0.7 + sample * 0.3).min(MAX_CONTEXT_CALIBRATION * 2.0)
        };
        self.factor = self.ratio.clamp(1.0, MAX_CONTEXT_CALIBRATION);
        self.samples = self.samples.saturating_add(1).min(1000);
    }
}

fn calibrate_context_tokens(tokens: u64, factor: f64) -> u64 {
    if !factor.is_finite() || factor <= 1.0 {
        return tokens;
    }
    let scaled = (tokens as f64 * factor.clamp(1.0, MAX_CONTEXT_CALIBRATION)).round();
    if scaled.is_finite() && scaled > tokens as f64 {
        scaled.min(u64::MAX as f64) as u64
    } else {
        tokens
    }
}

async fn load_context_calibration(
    engine: &Engine,
    session_id: &str,
    model: &str,
) -> ContextCalibration {
    let Ok(Some(event)) = engine
        .latest_event_of_kind(session_id, "context.measured")
        .await
    else {
        return ContextCalibration::default();
    };
    let Ok(measurement) = serde_json::from_value::<ContextMeasurement>(event.payload) else {
        return ContextCalibration::default();
    };
    if measurement.model != model || !measurement.calibration.is_finite() {
        return ContextCalibration::default();
    }
    let factor = measurement.calibration.clamp(1.0, MAX_CONTEXT_CALIBRATION);
    ContextCalibration {
        factor,
        ratio: factor,
        samples: measurement.samples,
    }
}

fn estimate_request_tokens(request: &ChatRequest) -> u64 {
    let mut bytes = request.system.as_deref().map_or(0, str::len);
    for message in &request.messages {
        bytes = bytes.saturating_add(message.content.len());
        if let Some(tool_calls) = &message.tool_calls {
            for call in tool_calls {
                bytes = bytes
                    .saturating_add(call.name.len())
                    .saturating_add(call.arguments.len());
            }
        }
    }
    for tool in &request.tools {
        bytes = bytes
            .saturating_add(tool.name.len())
            .saturating_add(tool.description.len());
        if let Ok(parameters) = serde_json::to_string(&tool.parameters) {
            bytes = bytes.saturating_add(parameters.len());
        }
    }
    (bytes as u64).div_ceil(2).saturating_add(64)
}

fn normalize_scope(scope: &str) -> String {
    scope
        .trim()
        .trim_start_matches("./")
        .trim_end_matches("/**")
        .trim_end_matches('/')
        .to_string()
}

fn path_in_scope(path: &str, scopes: &[String]) -> bool {
    if scopes.is_empty() {
        return true;
    }
    scopes.iter().any(|scope| {
        let scope = normalize_scope(scope);
        scope == "*" || scope == "**" || path == scope || path.starts_with(&format!("{scope}/"))
    })
}

fn scopes_overlap(left: &[String], right: &[String]) -> bool {
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

async fn commit_worker(spec: &AgentSpec, name: &str) -> Result<Option<String>> {
    let task_worktree = spec.worktree.clone();
    let message = format!("task: {name}");
    let write_scope = spec.write_scope.clone();
    let base_commit = spec.base_commit.clone();
    tokio::task::spawn_blocking(move || -> Result<Option<String>> {
        if !write_scope.is_empty() {
            let paths = worktree::changed_paths(&task_worktree, &base_commit)?;
            let outside = paths
                .iter()
                .filter(|path| !path_in_scope(path, &write_scope))
                .cloned()
                .collect::<Vec<_>>();
            if !outside.is_empty() {
                return Err(anyhow!("write scope violation: {}", outside.join(", ")));
            }
        }
        let committed = worktree::commit_all(&task_worktree, &message)?;
        if committed.is_some() {
            return Ok(committed);
        }
        // A resumed task may already have a committed branch even though its
        // worktree is clean. Re-enter verification/integration with that commit
        // instead of treating the task as a no-op.
        let head = worktree::head_commit(&task_worktree)?;
        if head == base_commit {
            Ok(None)
        } else {
            Ok(Some(head))
        }
    })
    .await?
}

async fn run_integration_command(
    engine: &Engine,
    project: &Project,
    session: &Session,
    worktree: PathBuf,
    command: String,
) -> Result<String> {
    let policy = engine.get_shell_policy(&project.id).await?;
    let mut secret_roots = shell_secret_roots(engine.store().data_dir());
    secret_roots.extend(policy.read_deny.iter().map(PathBuf::from));
    let context = crate::tools::ToolContext {
        root: worktree,
        project_root: PathBuf::from(&project.root),
        sandbox_root: engine
            .store()
            .data_dir()
            .join("sandbox")
            .join(&session.id)
            .join("integration"),
        secret_roots,
        network_allowed: policy.network != "deny",
        home: env::var_os("HOME").map(PathBuf::from).unwrap_or_default(),
        restrict_home: policy.restrict_home,
        read_jail: policy.read_jail,
        git_branch: session.branch.clone(),
    };
    let (exit, text) =
        crate::tools::run_sandboxed_login_shell(&context, &command, Duration::from_secs(300))
            .await?;
    let text = truncate(&text);
    if exit == 0 {
        Ok(text)
    } else {
        Err(anyhow!("integration command exited with {exit}:\n{text}"))
    }
}

async fn integrate_worker(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    commit: &str,
    verifier_summary: &str,
    integration_command: Option<&str>,
) -> Result<String> {
    let worktree = spec.session_worktree.clone();
    let base_commit =
        tokio::task::spawn_blocking(move || worktree::head_commit(&worktree)).await??;
    let entry = engine
        .enqueue_merge(
            &session.id,
            &spec.id,
            &spec.name,
            spec.task_id.clone(),
            &spec.branch,
            commit,
            &base_commit,
            Some(verifier_summary.to_string()),
        )
        .await?;
    process_merge_entry(engine, session, spec, &entry, integration_command).await
}

/// Serialize integration across processes with a runtime lease, then run the
/// deterministic merge/check path.
async fn process_merge_entry(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    entry: &MergeQueueEntry,
    integration_command: Option<&str>,
) -> Result<String> {
    let lease = engine
        .acquire_runtime_lease(
            "integration",
            &session.id,
            std::time::Duration::from_secs(900),
        )
        .await?;
    let result = process_merge_entry_inner(engine, session, spec, entry, integration_command).await;
    lease.release().await;
    result
}

/// Merge and integration-check an existing queue entry. The entry must already
/// have a valid `base_commit` (the session HEAD before this merge).
async fn process_merge_entry_inner(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    entry: &MergeQueueEntry,
    integration_command: Option<&str>,
) -> Result<String> {
    let (_, project) = load_session_project(engine.store().clone(), &session.id).await?;
    let preflight_worktree = spec.session_worktree.clone();
    let git_lock = engine.git_mutex();
    let preflight = {
        let git_lock = git_lock.clone();
        let preflight_worktree = preflight_worktree.clone();
        tokio::task::spawn_blocking(move || -> Result<String> {
            let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
            if !worktree::is_clean(&preflight_worktree)? {
                return Err(anyhow!(
                    "session worktree has uncommitted tracked changes; refusing to integrate"
                ));
            }
            worktree::head_commit(&preflight_worktree)
        })
        .await?
    };
    let preflight = match preflight {
        Ok(commit) => commit,
        Err(error) => {
            let detail = truncate(&format!("merge preflight failed: {error}"));
            engine
                .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                .await?;
            emit(
                engine,
                &session.id,
                spec,
                "integrator.finished",
                serde_json::json!({
                    "entry_id": entry.id,
                    "status": "failed",
                    "detail": detail,
                }),
            )
            .await?;
            return Err(anyhow!(detail));
        }
    };
    engine
        .begin_merge_queue_entry(&entry.id, &preflight)
        .await?;
    emit(
        engine,
        &session.id,
        spec,
        "integrator.started",
        serde_json::json!({
            "entry_id": entry.id,
            "branch": entry.branch,
            "base_commit": entry.base_commit,
            "pre_merge_commit": preflight,
            "attempt": entry.attempt,
            "has_command": integration_command.is_some(),
        }),
    )
    .await?;

    let merge_worktree = spec.session_worktree.clone();
    let merge_commit = entry.commit.clone();
    let expected_base = preflight.clone();
    let message = format!("merge {}", spec.name);
    let merge_result = tokio::task::spawn_blocking(move || -> Result<String> {
        let _guard = git_lock.lock().map_err(|_| anyhow!("git mutex poisoned"))?;
        let actual_base = worktree::head_commit(&merge_worktree)?;
        if actual_base != expected_base {
            tracing::warn!(
                expected = %expected_base,
                actual = %actual_base,
                "merge queue base moved; integrating against current session HEAD"
            );
        }
        worktree::merge_commit(&merge_worktree, &merge_commit, &message)?;
        Ok(actual_base)
    })
    .await?;
    let actual_base = match merge_result {
        Ok(base) => base,
        Err(error) => {
            let message = format!("{error:#}");
            let conflict = message.to_lowercase().contains("conflict")
                || message.contains("Automatic merge failed");
            let status = if conflict { "conflict" } else { "failed" };
            let detail = truncate(&if conflict {
                format!("merge conflict: {message}")
            } else {
                format!("merge failed: {message}")
            });
            engine
                .update_merge_queue_entry(&entry.id, status, Some(detail.clone()))
                .await?;
            emit(
                engine,
                &session.id,
                spec,
                "integrator.finished",
                serde_json::json!({
                    "entry_id": entry.id,
                    "status": status,
                    "detail": detail,
                }),
            )
            .await?;
            return Err(anyhow!(detail));
        }
    };

    engine
        .update_merge_queue_entry(&entry.id, "checking", None)
        .await?;
    let check_worktree = spec.session_worktree.clone();
    let check_base = actual_base.clone();
    let check_result = tokio::task::spawn_blocking(move || {
        worktree::check_merged_diff(&check_worktree, &check_base)
    })
    .await?;
    let check_result = match check_result {
        Ok(()) => {
            if let Some(command) = integration_command {
                run_integration_command(
                    engine,
                    &project,
                    session,
                    spec.session_worktree.clone(),
                    command.to_string(),
                )
                .await
            } else {
                Ok("diff check passed".to_string())
            }
        }
        Err(error) => Err(error),
    };

    let integration_detail = match check_result {
        Ok(detail) => detail,
        Err(error) => {
            let detail = format!("integration check failed: {error}");
            let reset_worktree = spec.session_worktree.clone();
            let reset_base = actual_base.clone();
            let reset = tokio::task::spawn_blocking(move || {
                worktree::reset_hard(&reset_worktree, &reset_base)
            })
            .await?;
            let detail = truncate(&match reset {
                Ok(()) => detail,
                Err(reset_error) => format!("{detail}; rollback failed: {reset_error}"),
            });
            engine
                .update_merge_queue_entry(&entry.id, "failed", Some(detail.clone()))
                .await?;
            emit(
                engine,
                &session.id,
                spec,
                "integrator.finished",
                serde_json::json!({
                    "entry_id": entry.id,
                    "status": "failed",
                    "detail": detail,
                }),
            )
            .await?;
            return Err(anyhow!(detail));
        }
    };
    let integration_detail = truncate(&integration_detail);
    engine
        .update_merge_queue_entry(&entry.id, "merged", Some(integration_detail.clone()))
        .await?;
    emit(
        engine,
        &session.id,
        spec,
        "integrator.finished",
        serde_json::json!({
            "entry_id": entry.id,
            "status": "merged",
            "detail": integration_detail,
            "attempt": entry.attempt,
        }),
    )
    .await?;
    Ok(format!("merged; {integration_detail}"))
}

enum VerificationOutcome {
    Approved(VerifierVerdict),
    Rejected(VerifierVerdict),
    Cancelled,
    BudgetExceeded(BudgetStop),
    Error(String),
}

struct VerificationCheckout {
    path: PathBuf,
}

impl Drop for VerificationCheckout {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            tracing::warn!(%error, path = %self.path.display(), "failed to remove verification checkout");
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn verify_worker(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    manager_spec: &AgentSpec,
    worker_spec: &AgentSpec,
    commit: &str,
    objective: &str,
    worker_summary: &str,
    acceptance: Option<&str>,
    auto_approve: bool,
) -> Result<VerificationOutcome> {
    let Some((ordinal, _verifier_slot)) = budget.reserve_slot(AgentRole::Verifier) else {
        return Ok(VerificationOutcome::Error(
            "the Verifier cap for this session has been reached".to_string(),
        ));
    };
    let verifier_name = format!("Verifier {ordinal}");
    let checkout_path = match worktree::create_verification_checkout(
        Path::new(&project.root),
        engine.store().data_dir(),
        &session.id,
        commit,
    ) {
        Ok(path) => path,
        Err(error) => {
            return Ok(VerificationOutcome::Error(format!(
                "could not create isolated verification checkout: {error}"
            )));
        }
    };
    let _checkout = VerificationCheckout {
        path: checkout_path.clone(),
    };
    let verifier_branch = format!("verify/{}", truncate_to(commit, 12));
    let verifier = engine
        .create_agent(
            &session.id,
            AgentRole::Verifier.as_str(),
            &verifier_name,
            Some("Verification".to_string()),
            Some(manager_spec.id.clone()),
            &model_for(config, AgentRole::Verifier),
        )
        .await?;
    engine
        .set_agent_worktree(
            &verifier.id,
            &checkout_path.to_string_lossy(),
            &verifier_branch,
        )
        .await?;
    engine
        .set_agent_profile(&verifier.id, AgentProfile::Verification.as_str())
        .await?;
    engine
        .set_agent_task(
            &verifier.id,
            worker_spec.task_id.clone(),
            worker_spec.department.clone(),
        )
        .await?;
    let verifier_spec = AgentSpec {
        id: verifier.id.clone(),
        role: AgentRole::Verifier,
        name: verifier_name.clone(),
        depth: manager_spec.depth.saturating_add(1),
        parent_name: Some(manager_spec.name.clone()),
        title: Some("Verification".to_string()),
        task_id: worker_spec.task_id.clone(),
        department: worker_spec.department.clone(),
        profile: AgentProfile::Verification,
        allow_delegation: false,
        auto_approve,
        worktree: checkout_path.clone(),
        branch: verifier_branch,
        session_worktree: manager_spec.session_worktree.clone(),
        session_branch: manager_spec.session_branch.clone(),
        write_scope: Vec::new(),
        base_commit: worker_spec.base_commit.clone(),
    };
    let focus = match worker_spec.profile {
        AgentProfile::Research => {
            "Check the report, sources, and citations; use fetch_url if a source needs verification."
        }
        AgentProfile::Coding => {
            "Inspect the diff, run relevant tests with shell, and check the acceptance criteria."
        }
        AgentProfile::Verification => "Inspect the evidence and run checks; do not edit files.",
        AgentProfile::Generic => "Inspect the output and evidence; do not edit files.",
    };
    let acceptance_note = acceptance
        .filter(|value| !value.trim().is_empty())
        .map(|value| format!(" Acceptance criteria: {value}."))
        .unwrap_or_default();
    let prompt = format!(
        "Verify Worker {} for this task. Objective: {objective}.{acceptance_note} \
         Worker summary: {worker_summary}. The isolated verification worktree is {} on branch {}; base commit {}. \
         {focus} Do not edit files. Then call submit_verdict with approved and summary.",
        worker_spec.name,
        verifier_spec.worktree.to_string_lossy(),
        verifier_spec.branch,
        verifier_spec.base_commit
    );
    emit(
        engine,
        &session.id,
        &verifier_spec,
        "agent.spawned",
        serde_json::json!({
            "parent_id": manager_spec.id,
            "parent_name": manager_spec.name,
            "objective": truncate(&prompt),
            "verified_worker": worker_spec.name,
        }),
    )
    .await?;
    let mut conversation = vec![ChatMessage::user(prompt.clone())];
    let result = run_loop(
        engine,
        config,
        session,
        project,
        control,
        budget,
        &verifier_spec,
        &prompt,
        &mut conversation,
        None,
        false,
    )
    .await;
    match result {
        Ok(LoopOutcome::Verdict(verdict)) => {
            let verify_root = verifier_spec.worktree.clone();
            let expected_commit = commit.to_string();
            let head_root = verify_root.clone();
            let head =
                tokio::task::spawn_blocking(move || worktree::head_commit(&head_root)).await??;
            if head != expected_commit {
                let reset_root = verify_root.clone();
                let reset_commit = expected_commit.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    worktree::reset_hard(&reset_root, &reset_commit)
                })
                .await?;
                let summary = format!(
                    "Verifier changed HEAD from {} to {}",
                    truncate_to(&expected_commit, 12),
                    truncate_to(&head, 12)
                );
                engine.update_agent_status(&verifier.id, "failed").await?;
                emit(
                    engine,
                    &session.id,
                    &verifier_spec,
                    "agent.finished",
                    serde_json::json!({
                        "status": "failed",
                        "approved": false,
                        "summary": summary,
                        "verified_worker": worker_spec.name,
                        "worktree": verifier_spec.worktree,
                        "branch": verifier_spec.branch,
                    }),
                )
                .await?;
                return Ok(VerificationOutcome::Rejected(VerifierVerdict {
                    approved: false,
                    summary,
                }));
            }
            let changed =
                tokio::task::spawn_blocking(move || worktree::tracked_changes(&verify_root))
                    .await??;
            if !changed.is_empty() {
                let reset_root = verifier_spec.worktree.clone();
                if let Err(error) = tokio::task::spawn_blocking(move || {
                    worktree::reset_tracked_changes(&reset_root)
                })
                .await?
                {
                    tracing::warn!(%error, "failed to reset verifier worktree changes");
                }
                let summary = format!(
                    "Verifier modified tracked files: {}",
                    truncate_to(&changed.join(", "), 300)
                );
                engine.update_agent_status(&verifier.id, "failed").await?;
                emit(
                    engine,
                    &session.id,
                    &verifier_spec,
                    "agent.finished",
                    serde_json::json!({
                        "status": "failed",
                        "approved": false,
                        "summary": summary,
                        "modified_paths": changed,
                        "verified_worker": worker_spec.name,
                        "worktree": verifier_spec.worktree,
                        "branch": verifier_spec.branch,
                    }),
                )
                .await?;
                return Ok(VerificationOutcome::Rejected(VerifierVerdict {
                    approved: false,
                    summary,
                }));
            }
            engine.update_agent_status(&verifier.id, "idle").await?;
            emit(
                engine,
                &session.id,
                &verifier_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "idle",
                    "approved": verdict.approved,
                    "summary": truncate(&verdict.summary),
                    "verified_worker": worker_spec.name,
                    "worktree": verifier_spec.worktree,
                    "branch": verifier_spec.branch,
                }),
            )
            .await?;
            Ok(if verdict.approved {
                VerificationOutcome::Approved(verdict)
            } else {
                VerificationOutcome::Rejected(verdict)
            })
        }
        Ok(LoopOutcome::Final(text)) => {
            let summary = if text.trim().is_empty() {
                "Verifier finished without submitting a verdict.".to_string()
            } else {
                format!("Verifier finished without submitting a verdict: {text}")
            };
            engine.update_agent_status(&verifier.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &verifier_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "approved": false,
                    "summary": truncate(&summary),
                    "verified_worker": worker_spec.name,
                }),
            )
            .await?;
            Ok(VerificationOutcome::Error(summary))
        }
        Ok(LoopOutcome::Cancelled) => {
            engine
                .update_agent_status(&verifier.id, "cancelled")
                .await?;
            emit(
                engine,
                &session.id,
                &verifier_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "cancelled",
                    "verified_worker": worker_spec.name,
                }),
            )
            .await?;
            Ok(VerificationOutcome::Cancelled)
        }
        Ok(LoopOutcome::BudgetExceeded(stop)) => {
            engine.update_agent_status(&verifier.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &verifier_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": stop.summary(),
                    "verified_worker": worker_spec.name,
                }),
            )
            .await?;
            Ok(VerificationOutcome::BudgetExceeded(stop))
        }
        Err(error) => {
            engine.update_agent_status(&verifier.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &verifier_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "approved": false,
                    "summary": truncate(error.to_string().as_str()),
                    "verified_worker": worker_spec.name,
                }),
            )
            .await?;
            Ok(VerificationOutcome::Error(error.to_string()))
        }
    }
}

enum AcpDelegation {
    Output(ToolOutput),
    Cancelled,
    BudgetExceeded(BudgetStop),
}

struct AcpWorkerHost {
    engine: Engine,
    session: Session,
    spec: AgentSpec,
    acp_name: String,
    auto_approve: bool,
    approval_mode: ApprovalMode,
    approval_timeout_secs: u64,
    cancel: watch::Receiver<bool>,
    text: Mutex<String>,
    started_tools: Mutex<HashSet<String>>,
    finished_tools: Mutex<HashSet<String>>,
}

impl AcpWorkerHost {
    fn final_summary(&self) -> String {
        let text = self
            .text
            .lock()
            .map(|text| text.trim().to_string())
            .unwrap_or_default();
        if text.is_empty() {
            "external agent completed without a summary".to_string()
        } else {
            truncate(&text)
        }
    }

    async fn finish_tool(
        &self,
        id: &str,
        status: &str,
        content: Option<serde_json::Value>,
        raw_output: Option<serde_json::Value>,
        title: Option<&str>,
    ) {
        let first = self
            .started_tools
            .lock()
            .map(|mut started| started.insert(id.to_string()))
            .unwrap_or(false);
        let tool_call_id = format!("acp:{}:{id}", self.spec.id);
        if first {
            let _ = emit(
                &self.engine,
                &self.session.id,
                &self.spec,
                "tool.started",
                serde_json::json!({
                    "tool_call_id": tool_call_id,
                    "name": "acp.tool",
                    "input": {
                        "title": title.unwrap_or("External agent tool"),
                    },
                }),
            )
            .await;
        }
        if status != "completed" && status != "failed" {
            return;
        }
        let first_finish = self
            .finished_tools
            .lock()
            .map(|mut finished| finished.insert(id.to_string()))
            .unwrap_or(false);
        if !first_finish {
            return;
        }
        let output = raw_output
            .or(content)
            .map(|value| value.to_string())
            .unwrap_or_default();
        let _ = emit(
            &self.engine,
            &self.session.id,
            &self.spec,
            "tool.finished",
            serde_json::json!({
                "tool_call_id": tool_call_id,
                "name": "acp.tool",
                "output": truncate(&output),
                "is_error": status == "failed",
            }),
        )
        .await;
    }
}

#[async_trait]
impl acp::AcpHost for AcpWorkerHost {
    async fn update(&self, update: acp::AcpUpdate) {
        match update {
            acp::AcpUpdate::Message(text) => {
                if let Ok(mut full) = self.text.lock()
                    && full.len() < 64 * 1024
                {
                    full.push_str(&text);
                }
                let _ = emit(
                    &self.engine,
                    &self.session.id,
                    &self.spec,
                    "agent.message_delta",
                    serde_json::json!({"text": text, "acp_agent": self.acp_name}),
                )
                .await;
            }
            acp::AcpUpdate::Thought(text) => {
                let _ = emit(
                    &self.engine,
                    &self.session.id,
                    &self.spec,
                    "agent.thought_delta",
                    serde_json::json!({"text": text, "acp_agent": self.acp_name}),
                )
                .await;
            }
            acp::AcpUpdate::ToolCall {
                id,
                title,
                kind,
                status,
                raw_input,
                content,
                raw_output,
            } => {
                let first = self
                    .started_tools
                    .lock()
                    .map(|mut started| started.insert(id.clone()))
                    .unwrap_or(false);
                if first {
                    let _ = emit(
                        &self.engine,
                        &self.session.id,
                        &self.spec,
                        "tool.started",
                        serde_json::json!({
                            "tool_call_id": format!("acp:{}:{id}", self.spec.id),
                            "name": format!("acp.{kind}"),
                            "input": {
                                "title": title,
                                "input": raw_input,
                            },
                        }),
                    )
                    .await;
                }
                if status == "completed" || status == "failed" {
                    self.finish_tool(&id, &status, content, raw_output, Some(&title))
                        .await;
                }
            }
            acp::AcpUpdate::ToolCallUpdate {
                id,
                title,
                status,
                content,
                raw_output,
            } => {
                let status = status.unwrap_or_else(|| "in_progress".to_string());
                self.finish_tool(&id, &status, content, raw_output, title.as_deref())
                    .await;
            }
        }
    }

    async fn permission(&self, request: acp::AcpPermissionRequest) -> acp::AcpPermissionDecision {
        let assessment = assess_acp_permission(&request, &self.spec.worktree);
        let granted = !self.auto_approve
            && self
                .engine
                .approval_grant_matches(
                    &self.session.project_id,
                    &format!("acp.{}", self.acp_name),
                    request
                        .raw_input
                        .as_ref()
                        .unwrap_or(&serde_json::Value::Null),
                )
                .await
                .unwrap_or(false);
        let decision = if granted {
            crate::permissions::ApprovalDecision::Allow {
                risk: assessment.risk,
                reason: "matched a project approval grant".to_string(),
            }
        } else {
            decide(self.approval_mode, &assessment, self.auto_approve)
        };
        let tool_call_id = format!("acp:{}:{}", self.spec.id, request.tool_call_id);
        let (risk, reason) = match decision {
            ApprovalDecision::Allow { risk, reason } => {
                if risk >= RiskLevel::External {
                    let _ = emit(
                        &self.engine,
                        &self.session.id,
                        &self.spec,
                        "permission.allowed",
                        serde_json::json!({
                            "tool": format!("acp.{}", self.acp_name),
                            "tool_call_id": tool_call_id,
                            "risk": risk.as_str(),
                            "reason": reason,
                        }),
                    )
                    .await;
                }
                return acp::AcpPermissionDecision::Approve;
            }
            ApprovalDecision::Deny { risk, reason } => {
                let _ = emit(
                    &self.engine,
                    &self.session.id,
                    &self.spec,
                    "permission.denied",
                    serde_json::json!({
                        "tool": format!("acp.{}", self.acp_name),
                        "tool_call_id": tool_call_id,
                        "risk": risk.as_str(),
                        "reason": reason,
                    }),
                )
                .await;
                return acp::AcpPermissionDecision::Deny;
            }
            ApprovalDecision::Ask { risk, reason } => (risk, reason),
        };
        let options = request
            .options
            .iter()
            .map(|option| {
                serde_json::json!({
                    "id": option.id,
                    "name": option.name,
                    "kind": option.kind,
                })
            })
            .collect::<Vec<_>>();
        let input = serde_json::json!({
            "title": request.title,
            "kind": request.kind,
            "input": request.raw_input,
            "options": options,
            "agent": self.acp_name,
        });
        let mut context = serde_json::json!({});
        agent_fields(&mut context, &self.spec);
        context["acp_agent"] = serde_json::json!(self.acp_name);
        let approval = self
            .engine
            .request_approval_with_risk(
                &self.session.id,
                &tool_call_id,
                &format!("acp.{}", self.acp_name),
                input,
                Some(context),
                Some(risk.as_str()),
                Some(&reason),
            )
            .await;
        let Ok((approval, receiver)) = approval else {
            return acp::AcpPermissionDecision::Deny;
        };
        match wait_for_approval_with(
            self.cancel.clone(),
            receiver,
            Duration::from_secs(self.approval_timeout_secs),
        )
        .await
        {
            ApprovalOutcome::Approved => acp::AcpPermissionDecision::Approve,
            ApprovalOutcome::Denied => acp::AcpPermissionDecision::Deny,
            ApprovalOutcome::TimedOut => {
                let _ = self.engine.resolve_approval(&approval.id, false).await;
                acp::AcpPermissionDecision::Deny
            }
            ApprovalOutcome::Cancelled => {
                let _ = self.engine.resolve_approval(&approval.id, false).await;
                acp::AcpPermissionDecision::Cancelled
            }
        }
    }
}

struct AcpDelegateRequest {
    agent: String,
    name: Option<String>,
    objective: String,
    instructions: String,
    timeout: Option<u64>,
}

fn parse_acp_delegate(input: &serde_json::Value) -> Result<AcpDelegateRequest> {
    let agent = input
        .get("agent")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("agent is required"))?
        .to_string();
    let objective = input
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("objective is required"))?
        .to_string();
    let name = input
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    let instructions = input
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_string();
    let timeout = input
        .get("timeout_secs")
        .and_then(serde_json::Value::as_u64)
        .filter(|seconds| *seconds >= 30);
    Ok(AcpDelegateRequest {
        agent,
        name,
        objective,
        instructions,
        timeout,
    })
}

#[allow(clippy::too_many_arguments)]
async fn delegate_acp(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    parent_spec: &AgentSpec,
    input: &serde_json::Value,
) -> Result<AcpDelegation> {
    let request = match parse_acp_delegate(input) {
        Ok(parsed) => parsed,
        Err(error) => return Ok(AcpDelegation::Output(ToolOutput::error(error.to_string()))),
    };
    let AcpDelegateRequest {
        agent: agent_ref,
        name: requested_name,
        objective,
        instructions,
        timeout,
    } = request;
    let Some(acp_agent) = engine.resolve_acp_agent(&agent_ref).await? else {
        return Ok(AcpDelegation::Output(ToolOutput::error(format!(
            "ACP agent not found: {agent_ref}"
        ))));
    };
    if !acp_agent.enabled {
        return Ok(AcpDelegation::Output(ToolOutput::error(format!(
            "ACP agent {} is disabled",
            acp_agent.name
        ))));
    }
    let Some((ordinal, _slot)) = budget.reserve_slot(AgentRole::Worker) else {
        return Ok(AcpDelegation::Output(ToolOutput::error(
            "the Worker cap for this session has been reached",
        )));
    };
    let write_scope = match parse_write_scope(input) {
        Ok(scope) => scope,
        Err(error) => return Ok(AcpDelegation::Output(ToolOutput::error(error.to_string()))),
    };
    let child_name = requested_name.unwrap_or_else(|| format!("{} {}", acp_agent.name, ordinal));
    let child = engine
        .create_agent(
            &session.id,
            "worker",
            &child_name,
            Some(format!("ACP · {}", acp_agent.name)),
            Some(parent_spec.id.clone()),
            &format!("acp:{}", acp_agent.name),
        )
        .await?;
    engine
        .set_agent_profile(&child.id, AgentProfile::Coding.as_str())
        .await?;
    let info = engine
        .create_task_worktree(
            &project.root,
            &project.id,
            &session.id,
            &child.id,
            &parent_spec.session_branch,
        )
        .await?;
    engine
        .set_agent_worktree(&child.id, &info.path.to_string_lossy(), &info.branch)
        .await?;
    let child_spec = AgentSpec {
        id: child.id.clone(),
        role: AgentRole::Worker,
        name: child_name.clone(),
        depth: parent_spec.depth.saturating_add(1),
        parent_name: Some(parent_spec.name.clone()),
        title: Some(format!("ACP · {}", acp_agent.name)),
        task_id: None,
        department: parent_spec.department.clone(),
        profile: AgentProfile::Coding,
        allow_delegation: false,
        auto_approve: acp_agent.auto_approve,
        worktree: info.path.clone(),
        branch: info.branch.clone(),
        session_worktree: parent_spec.session_worktree.clone(),
        session_branch: parent_spec.session_branch.clone(),
        write_scope: write_scope.clone(),
        base_commit: info.base_commit.clone(),
    };
    emit(
        engine,
        &session.id,
        &child_spec,
        "agent.spawned",
        serde_json::json!({
            "parent_id": parent_spec.id,
            "parent_name": parent_spec.name,
            "objective": truncate(&objective),
            "acp_agent": acp_agent.name,
            "acp_command": acp_agent.command,
            "backend": "acp",
        }),
    )
    .await?;

    let capacity_priority = task_capacity_priority(session.priority, 0);
    let _capacity_permit = match config.capacity.try_acquire(AgentRole::Worker, &session.id) {
        Some(permit) => Some(permit),
        None => {
            let snapshot = config.capacity.snapshot();
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.capacity_wait",
                serde_json::json!({
                    "role": "worker",
                    "limit": snapshot.worker_limit,
                    "available": snapshot.worker_available,
                    "backend": "acp",
                }),
            )
            .await?;
            engine.update_agent_status(&child.id, "queued").await?;
            let permit = match config
                .capacity
                .acquire(AgentRole::Worker, &session.id, capacity_priority, control)
                .await
            {
                Some(permit) => permit,
                None => {
                    engine.update_agent_status(&child.id, "cancelled").await?;
                    return Ok(AcpDelegation::Cancelled);
                }
            };
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.capacity_ready",
                serde_json::json!({"role": "worker", "waited": true, "backend": "acp"}),
            )
            .await?;
            Some(permit)
        }
    };
    let _scope_guard = match config
        .scope_locks
        .acquire(&project.id, &session.id, &child.id, &write_scope, control)
        .await?
    {
        Some(guard) => guard,
        None => {
            engine.update_agent_status(&child.id, "cancelled").await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.finished",
                serde_json::json!({"status": "cancelled", "backend": "acp"}),
            )
            .await?;
            return Ok(AcpDelegation::Cancelled);
        }
    };
    engine.update_agent_status(&child.id, "running").await?;

    let shell_policy = engine.get_shell_policy(&project.id).await?;
    let mut secret_roots = shell_secret_roots(engine.store().data_dir());
    secret_roots.extend(shell_policy.read_deny.iter().map(PathBuf::from));
    let mcp_servers = engine
        .list_mcp_servers()
        .await?
        .servers
        .into_iter()
        .filter(|server| server.enabled)
        .map(|server| acp::mcp_server(&server))
        .collect::<Result<Vec<_>>>()?;
    let scope_note = if write_scope.is_empty() {
        "No explicit write scope was declared.".to_string()
    } else {
        format!("Write scope: {}.", write_scope.join(", "))
    };
    let instructions = if instructions.is_empty() {
        String::new()
    } else {
        format!(
            "
Additional instructions:
{instructions}
"
        )
    };
    let prompt = format!(
        "You are an external ACP agent delegated by {} inside VerlinPark.
         Objective: {objective}
{instructions}         Work only in the current Git worktree at {} on branch {} (base commit {}).
         {scope_note}
         Make the requested changes directly, run relevant checks, and do not ask the user questions.          Finish with a concise summary of what changed and which checks ran.",
        parent_spec.name,
        child_spec.worktree.to_string_lossy(),
        child_spec.branch,
        child_spec.base_commit,
    );
    let launch = acp::AcpLaunch {
        name: acp_agent.name.clone(),
        command: acp_agent.command.clone(),
        args: acp_agent.args.clone(),
        env: acp_agent.env.clone(),
        cwd: child_spec.worktree.clone(),
        project_root: PathBuf::from(&project.root),
        sandbox_root: engine
            .store()
            .data_dir()
            .join("sandbox")
            .join(&session.id)
            .join(&child.id),
        secret_roots,
        network_allowed: shell_policy.network != "deny",
        home: std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default(),
        restrict_home: shell_policy.restrict_home,
        read_jail: shell_policy.read_jail,
        git_branch: Some(child_spec.branch.clone()),
        mcp_servers,
        timeout: Duration::from_secs(
            timeout
                .unwrap_or(acp_agent.timeout_secs)
                .min(acp_agent.timeout_secs),
        ),
    };
    let host = Arc::new(AcpWorkerHost {
        engine: engine.clone(),
        session: session.clone(),
        spec: child_spec.clone(),
        acp_name: acp_agent.name.clone(),
        auto_approve: acp_agent.auto_approve,
        approval_mode: config.approval_mode,
        approval_timeout_secs: config.approval_timeout_secs,
        cancel: control.subscribe(),
        text: Mutex::new(String::new()),
        started_tools: Mutex::new(HashSet::new()),
        finished_tools: Mutex::new(HashSet::new()),
    });
    let result = acp::run_prompt(launch, prompt, host.clone(), control.subscribe()).await;
    if control.is_cancelled() {
        engine.update_agent_status(&child.id, "cancelled").await?;
        emit(
            engine,
            &session.id,
            &child_spec,
            "agent.finished",
            serde_json::json!({
                "status": "cancelled",
                "backend": "acp",
                "acp_agent": acp_agent.name,
            }),
        )
        .await?;
        return Ok(AcpDelegation::Cancelled);
    }
    let outcome = match result {
        Ok(outcome) => outcome,
        Err(error) => {
            let summary = format!("external ACP agent failed: {error:#}");
            engine.update_agent_status(&child.id, "failed").await?;
            emit(
                engine,
                &session.id,
                &child_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&summary),
                    "backend": "acp",
                    "acp_agent": acp_agent.name,
                }),
            )
            .await?;
            return Ok(AcpDelegation::Output(ToolOutput::error(summary)));
        }
    };
    if outcome.cancelled {
        engine.update_agent_status(&child.id, "cancelled").await?;
        emit(
            engine,
            &session.id,
            &child_spec,
            "agent.finished",
            serde_json::json!({
                "status": "cancelled",
                "backend": "acp",
                "acp_agent": acp_agent.name,
            }),
        )
        .await?;
        return Ok(AcpDelegation::Cancelled);
    }
    if outcome.stop_reason != "end_turn" {
        let summary = format!(
            "external ACP agent stopped with {} without completing the task",
            outcome.stop_reason
        );
        engine.update_agent_status(&child.id, "failed").await?;
        emit(
            engine,
            &session.id,
            &child_spec,
            "agent.finished",
            serde_json::json!({
                "status": "failed",
                "summary": summary,
                "backend": "acp",
                "acp_agent": outcome.agent_name,
                "stop_reason": outcome.stop_reason,
            }),
        )
        .await?;
        return Ok(AcpDelegation::Output(ToolOutput::error(summary)));
    }
    let summary = host.final_summary();
    emit(
        engine,
        &session.id,
        &child_spec,
        "agent.message",
        serde_json::json!({
            "text": summary,
            "backend": "acp",
            "acp_agent": outcome.agent_name,
            "acp_version": outcome.agent_version,
            "protocol_version": outcome.protocol_version,
        }),
    )
    .await?;
    match finish_worker(
        engine,
        config,
        session,
        project,
        control,
        budget,
        parent_spec,
        &child_spec,
        &objective,
        &summary,
        None,
    )
    .await?
    {
        WorkerFinish::Done { report, success } => {
            let report = format!(
                "External worker {} finished:
{}",
                child_name, report
            );
            if success {
                Ok(AcpDelegation::Output(ToolOutput::ok(report)))
            } else {
                Ok(AcpDelegation::Output(ToolOutput::error(report)))
            }
        }
        WorkerFinish::Cancelled => Ok(AcpDelegation::Cancelled),
        WorkerFinish::BudgetExceeded(stop) => {
            if stop.is_abortive() {
                Ok(AcpDelegation::BudgetExceeded(stop))
            } else {
                Ok(AcpDelegation::Output(ToolOutput::error(stop.summary())))
            }
        }
    }
}

fn worker_report_reports_failure(summary: &str) -> bool {
    let lower = summary.to_ascii_lowercase();
    [
        "blocked",
        "permission denied",
        "no write tool",
        "cannot proceed",
        "could not complete",
        "unable to complete",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

enum WorkerFinish {
    Done { report: String, success: bool },
    Cancelled,
    BudgetExceeded(BudgetStop),
}

#[allow(clippy::too_many_arguments)]
async fn finish_worker(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    manager_spec: &AgentSpec,
    worker_spec: &AgentSpec,
    objective: &str,
    summary: &str,
    acceptance: Option<&str>,
) -> Result<WorkerFinish> {
    let commit = match commit_worker(worker_spec, &worker_spec.name).await {
        Ok(commit) => commit,
        Err(error) => {
            let summary = format!("integration failed: {error}");
            engine
                .update_agent_status(&worker_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&summary),
                    "merged": false,
                    "worktree": worker_spec.worktree,
                    "branch": worker_spec.branch,
                }),
            )
            .await?;
            return Ok(WorkerFinish::Done {
                report: summary,
                success: false,
            });
        }
    };
    let Some(commit) = commit else {
        engine.update_agent_status(&worker_spec.id, "idle").await?;
        emit(
            engine,
            &session.id,
            worker_spec,
            "agent.finished",
            serde_json::json!({
                "status": "idle",
                "summary": truncate(summary),
                "merged": false,
                "verified": false,
                "worktree": worker_spec.worktree,
                "branch": worker_spec.branch,
            }),
        )
        .await?;
        return Ok(WorkerFinish::Done {
            report: format!("no changes to merge\n{}", truncate(summary)),
            success: !worker_report_reports_failure(summary),
        });
    };

    let verification = verify_worker(
        engine,
        config,
        session,
        project,
        control,
        budget,
        manager_spec,
        worker_spec,
        &commit,
        objective,
        summary,
        acceptance,
        false,
    )
    .await?;
    match verification {
        VerificationOutcome::Approved(verdict) => {
            match integrate_worker(
                engine,
                session,
                worker_spec,
                &commit,
                &verdict.summary,
                config.integration_command.as_deref(),
            )
            .await
            {
                Ok(detail) => {
                    engine.update_agent_status(&worker_spec.id, "idle").await?;
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "agent.finished",
                        serde_json::json!({
                            "status": "idle",
                            "summary": truncate(summary),
                            "commit": commit,
                            "merged": true,
                            "verified": true,
                            "approved": true,
                            "integrator": true,
                            "integration": truncate(&detail),
                            "verdict": truncate(&verdict.summary),
                            "worktree": worker_spec.worktree,
                            "branch": worker_spec.branch,
                        }),
                    )
                    .await?;
                    Ok(WorkerFinish::Done {
                        report: format!(
                            "merged: true\nverified: true\nintegrator: {}\n{}\nverdict: {}",
                            truncate(&detail),
                            truncate(summary),
                            truncate(&verdict.summary)
                        ),
                        success: true,
                    })
                }
                Err(error) => {
                    engine
                        .update_agent_status(&worker_spec.id, "failed")
                        .await?;
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "agent.finished",
                        serde_json::json!({
                            "status": "failed",
                            "summary": truncate(error.to_string().as_str()),
                            "commit": commit,
                            "merged": false,
                            "verified": true,
                            "approved": true,
                            "integrator": true,
                            "verdict": truncate(&verdict.summary),
                            "worktree": worker_spec.worktree,
                            "branch": worker_spec.branch,
                        }),
                    )
                    .await?;
                    Ok(WorkerFinish::Done {
                        report: format!(
                            "merge/integration failed: {error}\nverdict: {}",
                            truncate(&verdict.summary)
                        ),
                        success: false,
                    })
                }
            }
        }
        VerificationOutcome::Rejected(verdict) => {
            engine
                .update_agent_status(&worker_spec.id, "changes_requested")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "changes_requested",
                    "summary": truncate(summary),
                    "commit": commit,
                    "merged": false,
                    "verified": true,
                    "approved": false,
                    "verdict": truncate(&verdict.summary),
                    "worktree": worker_spec.worktree,
                    "branch": worker_spec.branch,
                }),
            )
            .await?;
            Ok(WorkerFinish::Done {
                report: format!(
                    "merged: false\nverification rejected:\n{}",
                    truncate(&verdict.summary)
                ),
                success: false,
            })
        }
        VerificationOutcome::Error(error) => {
            engine
                .update_agent_status(&worker_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": truncate(&error),
                    "commit": commit,
                    "merged": false,
                    "verified": false,
                    "worktree": worker_spec.worktree,
                    "branch": worker_spec.branch,
                }),
            )
            .await?;
            Ok(WorkerFinish::Done {
                report: format!("verification error: {error}"),
                success: false,
            })
        }
        VerificationOutcome::Cancelled => {
            engine
                .update_agent_status(&worker_spec.id, "cancelled")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "cancelled",
                    "commit": commit,
                    "merged": false,
                }),
            )
            .await?;
            Ok(WorkerFinish::Cancelled)
        }
        VerificationOutcome::BudgetExceeded(stop) => {
            engine
                .update_agent_status(&worker_spec.id, "failed")
                .await?;
            emit(
                engine,
                &session.id,
                worker_spec,
                "agent.finished",
                serde_json::json!({
                    "status": "failed",
                    "summary": format!("{} during verification", stop.summary()),
                    "commit": commit,
                    "merged": false,
                }),
            )
            .await?;
            if stop.is_abortive() {
                Ok(WorkerFinish::BudgetExceeded(stop))
            } else {
                Ok(WorkerFinish::Done {
                    report: format!("verification stopped: {}", stop.summary()),
                    success: false,
                })
            }
        }
    }
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
            id: Some(id.to_string()),
            limit_tokens,
            spent_tokens: spent.0,
            limit_cost,
            spent_cost: spent.1,
            reserved_tokens: 0,
            reserved_cost: 0.0,
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

async fn emit_scope_budget_stop(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    stop: &BudgetStop,
) -> Result<()> {
    emit(
        engine,
        session_id,
        spec,
        "agent.budget_exceeded",
        stop.payload(),
    )
    .await
}

const STREAM_ATTEMPTS: u32 = 3;
const STREAM_RETRY_BASE: Duration = Duration::from_millis(250);

#[derive(Default)]
struct StreamTurn {
    text: String,
    tool_calls: Vec<ToolCall>,
    usage: (u64, u64),
    cancelled: bool,
    error: Option<anyhow::Error>,
}

fn stream_error_retryable(error: &ProviderError) -> bool {
    match error {
        ProviderError::Http(error) => {
            error.is_timeout()
                || error.is_connect()
                || error.is_request()
                || error.is_body()
                || error.is_decode()
        }
        ProviderError::Stream(_) => true,
        ProviderError::Api { status, .. } => {
            matches!(status, 408 | 409 | 425 | 429 | 500 | 502 | 503 | 504)
        }
        ProviderError::MissingApiKey | ProviderError::Parse(_) | ProviderError::Unsupported(_) => {
            false
        }
    }
}

async fn emit_stream_retry(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    attempt: u32,
    phase: &str,
    error: &str,
) -> Result<()> {
    emit(
        engine,
        session_id,
        spec,
        "agent.stream_retry",
        serde_json::json!({
            "attempt": attempt,
            "max_attempts": STREAM_ATTEMPTS,
            "phase": phase,
            "error": truncate(error),
        }),
    )
    .await
}

async fn stream_retry_delay(control: &RunControl, attempt: u32) -> bool {
    if control.is_cancelled() {
        return false;
    }
    let delay = STREAM_RETRY_BASE.saturating_mul(attempt.max(1));
    let mut cancel_rx = control.subscribe();
    tokio::select! {
        _ = tokio::time::sleep(delay) => !control.is_cancelled(),
        _ = cancel_rx.changed() => false,
    }
}

/// Consume one provider stream turn. Transient failures before any output are
/// retried; after partial output the turn returns its error together with the
/// text/usage received so far so the caller can persist a partial message and
/// settle the budget honestly.
#[allow(clippy::too_many_arguments)]
async fn consume_stream(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    control: &RunControl,
    provider: &dyn Provider,
    request: &ChatRequest,
    idle_timeout: Duration,
) -> Result<StreamTurn> {
    let mut turn = StreamTurn::default();
    let mut attempt = 0;
    'streams: loop {
        attempt += 1;
        let mut setup_cancel = control.subscribe();
        let setup = tokio::select! {
            changed = setup_cancel.changed() => {
                if changed.is_err() || *setup_cancel.borrow() {
                    turn.cancelled = true;
                    break 'streams;
                }
                continue;
            }
            result = tokio::time::timeout(idle_timeout, provider.stream_chat(request.clone())) => result,
        };
        let mut stream = match setup {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                // The provider's own send path already retries transient
                // startup failures; do not multiply those attempts here.
                turn.error = Some(anyhow!(error).context("starting provider stream"));
                break;
            }
            Err(_) => {
                turn.error = Some(anyhow!(
                    "provider did not start a stream within {}s",
                    idle_timeout.as_secs()
                ));
                break;
            }
        };
        let mut produced = false;
        let mut cancel_rx = control.subscribe();
        loop {
            if control.is_cancelled() {
                turn.cancelled = true;
                break 'streams;
            }
            let item = tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_err() || *cancel_rx.borrow() {
                        turn.cancelled = true;
                        break 'streams;
                    }
                    continue;
                }
                item = tokio::time::timeout(idle_timeout, stream.next()) => match item {
                    Ok(item) => item,
                    Err(_) => {
                        let error = format!(
                            "provider stream was idle for {}s",
                            idle_timeout.as_secs()
                        );
                        if !produced
                            && turn.usage == (0, 0)
                            && attempt < STREAM_ATTEMPTS
                            && !control.is_cancelled()
                        {
                            emit_stream_retry(
                                engine,
                                session_id,
                                spec,
                                attempt,
                                "idle",
                                &error,
                            )
                            .await?;
                            if !stream_retry_delay(control, attempt).await {
                                turn.cancelled = true;
                                break 'streams;
                            }
                            continue 'streams;
                        }
                        turn.error = Some(anyhow!(error));
                        break 'streams;
                    }
                },
            };
            let Some(item) = item else {
                break;
            };
            match item {
                Ok(StreamEvent::TextDelta(delta)) => {
                    produced = true;
                    turn.text.push_str(&delta);
                    emit(
                        engine,
                        session_id,
                        spec,
                        "agent.message_delta",
                        serde_json::json!({"text": delta}),
                    )
                    .await?;
                }
                Ok(StreamEvent::ToolCall(call)) => {
                    produced = true;
                    turn.tool_calls.push(call);
                }
                Ok(StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                }) => {
                    turn.usage = (
                        turn.usage.0 + input_tokens as u64,
                        turn.usage.1 + output_tokens as u64,
                    );
                }
                Ok(StreamEvent::Done) => {}
                Err(error) => {
                    if !produced
                        && turn.usage == (0, 0)
                        && stream_error_retryable(&error)
                        && attempt < STREAM_ATTEMPTS
                        && !control.is_cancelled()
                    {
                        emit_stream_retry(
                            engine,
                            session_id,
                            spec,
                            attempt,
                            "stream",
                            &error.to_string(),
                        )
                        .await?;
                        if !stream_retry_delay(control, attempt).await {
                            turn.cancelled = true;
                            break 'streams;
                        }
                        continue 'streams;
                    }
                    turn.error = Some(anyhow!(error).context("provider stream"));
                    break 'streams;
                }
            }
        }
        break;
    }
    Ok(turn)
}

const COMPACTION_SYSTEM_PROMPT: &str = "You are compacting an AI coding agent conversation. \
Summarize the transcript so another agent can continue without the full history. Preserve: \
the user's goal and constraints, decisions and their reasons, files and paths changed or inspected, \
important tool results and test outcomes, open tasks and blockers, and exact identifiers or commands \
that would be costly to rediscover. Be concise and factual. Do not invent work, results, or files. \
Return only the summary text.";

fn estimate_messages_tokens(messages: &[ChatMessage]) -> u64 {
    messages.iter().fold(0u64, |total, message| {
        let tool_tokens = message
            .tool_calls
            .as_ref()
            .and_then(|calls| serde_json::to_string(calls).ok())
            .map(|calls| estimate_context_tokens(&calls))
            .unwrap_or(0);
        total
            .saturating_add(estimate_context_tokens(&message.content))
            .saturating_add(tool_tokens)
    })
}

fn compaction_transcript(messages: &[ChatMessage], max_tokens: u64) -> String {
    const PER_MESSAGE: usize = 4_000;
    let mut rendered = Vec::new();
    let mut used = 0u64;
    let mut omitted = false;
    // Keep the most recent (most relevant) summarized messages when the
    // transcript would not fit the summarizer's own context window.
    for message in messages.iter().rev() {
        let mut part = String::new();
        part.push_str(&message.role);
        part.push_str(": ");
        part.push_str(&truncate_to(&message.content, PER_MESSAGE));
        part.push('\n');
        if let Some(tool_calls) = &message.tool_calls {
            part.push_str("tool_calls: ");
            part.push_str(&serde_json::to_string(tool_calls).unwrap_or_default());
            part.push('\n');
        }
        if let Some(tool_call_id) = &message.tool_call_id {
            part.push_str("tool_call_id: ");
            part.push_str(tool_call_id);
            part.push('\n');
        }
        let tokens = estimate_context_tokens(&part);
        if used.saturating_add(tokens) > max_tokens && !rendered.is_empty() {
            omitted = true;
            break;
        }
        used = used.saturating_add(tokens);
        rendered.push(part);
    }
    rendered.reverse();
    let mut transcript = String::new();
    if omitted {
        transcript.push_str("… older messages omitted at the compaction limit …\n");
    }
    transcript.extend(rendered);
    transcript
}

async fn collect_compaction_completion(
    provider: &dyn Provider,
    request: &ChatRequest,
    idle_timeout: Duration,
    control: &RunControl,
) -> Result<(String, (u64, u64))> {
    let mut stream = provider
        .stream_chat(request.clone())
        .await
        .context("starting context compaction stream")?;
    let mut cancel_rx = control.subscribe();
    let mut text = String::new();
    let mut usage = (0u64, 0u64);
    loop {
        if control.is_cancelled() {
            anyhow::bail!("context compaction cancelled");
        }
        let item = tokio::select! {
            changed = cancel_rx.changed() => {
                if changed.is_err() || *cancel_rx.borrow() {
                    anyhow::bail!("context compaction cancelled");
                }
                continue;
            }
            item = tokio::time::timeout(idle_timeout, stream.next()) => match item {
                Ok(item) => item,
                Err(_) => anyhow::bail!(
                    "context compaction stream was idle for {}s",
                    idle_timeout.as_secs()
                ),
            },
        };
        let Some(item) = item else {
            break;
        };
        match item? {
            StreamEvent::TextDelta(delta) => {
                text.push_str(&delta);
                if text.len() >= 32_768 {
                    break;
                }
            }
            StreamEvent::Usage {
                input_tokens,
                output_tokens,
            } => {
                usage = (
                    usage.0.saturating_add(input_tokens as u64),
                    usage.1.saturating_add(output_tokens as u64),
                );
            }
            StreamEvent::ToolCall(_) | StreamEvent::Done => {}
        }
    }
    Ok((text, usage))
}

fn safe_context_cut(conversation: &[ChatMessage], keep: usize, floor: usize) -> Option<usize> {
    let len = conversation.len();
    if len <= keep.saturating_add(floor).saturating_add(1) {
        return None;
    }
    let min_cut = len.saturating_sub(keep).max(floor.saturating_add(1));
    (min_cut..len.saturating_sub(1))
        .find(|index| matches!(conversation[*index].role.as_str(), "user" | "assistant"))
}

/// Keep the active context under the model window. The primary strategy is a
/// persistent summary plus a retained tail. If summarization is unavailable,
/// fails, or the summarized context is still above the hard limit, older
/// messages are dropped at safe user/assistant boundaries as a rolling-window
/// fallback. Original messages remain in SQLite for search and audit.
#[allow(clippy::too_many_arguments)]
async fn compact_context_if_needed(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    spec: &AgentSpec,
    budget: &SessionBudget,
    control: &RunControl,
    conversation: &mut Vec<ChatMessage>,
    conversation_ids: &mut Vec<Option<String>>,
    compact_threshold: u64,
    hard_limit: u64,
    base_tokens: u64,
    calibration: f64,
    force: bool,
    strategy: ContextStrategy,
) -> Result<bool> {
    if budget.exhausted() {
        return Ok(false);
    }
    if !force && compact_threshold == 0 && hard_limit == u64::MAX {
        return Ok(false);
    }
    let keep = config.compact_keep_messages.max(2);
    if conversation.len() <= keep {
        return Ok(false);
    }
    // Thresholds and the hard limit describe the whole active request, so the
    // comparison must include the system prompt and tool schemas too. Using
    // messages alone could defer compaction until the provider request is
    // already over the model window.
    let estimated_before = calibrate_context_tokens(
        estimate_messages_tokens(conversation).saturating_add(base_tokens),
        calibration,
    );
    let triggered = match strategy {
        ContextStrategy::Summarize => {
            estimated_before >= compact_threshold || estimated_before > hard_limit
        }
        ContextStrategy::Trim => estimated_before > hard_limit,
    };
    if !force && !triggered {
        return Ok(false);
    }
    let mut estimated = estimated_before;
    let mut summarized_messages = 0usize;
    let mut dropped_messages = 0usize;
    // Preserve a summary already loaded at the head of the conversation. A
    // trim-only pass must carry it into the new event, or resume would lose it.
    let mut summary_text = if conversation_ids.first().is_some_and(Option::is_none) {
        conversation.first().map(|message| message.content.clone())
    } else {
        None
    };
    let mut through_id: Option<String> = None;
    let mut usage_tokens = (0u64, 0u64);
    let mut usage_estimated = true;

    if strategy == ContextStrategy::Summarize
        && (force || (compact_threshold > 0 && estimated >= compact_threshold))
        && let Some(cut) = safe_context_cut(conversation, keep, 0)
        && let Some(through) = (if conversation_ids.is_empty() {
            Some(None)
        } else {
            conversation_ids
                .get(cut.saturating_sub(1))
                .cloned()
                .flatten()
                .map(Some)
        })
    {
        let summary_window = context_window_for(config, spec.role);
        let summary_budget = if summary_window > 0 {
            let budget = summary_window
                .saturating_sub(1024)
                .saturating_sub(estimate_context_tokens(COMPACTION_SYSTEM_PROMPT))
                .saturating_sub(512)
                .max(512);
            // If provider usage showed that the heuristic undercounts, leave
            // proportionally more room for the summary request too.
            if calibration > 1.0 {
                (budget as f64 / calibration).round().max(512.0) as u64
            } else {
                budget
            }
        } else {
            60_000
        };
        let transcript = compaction_transcript(&conversation[..cut], summary_budget);
        if !transcript.trim().is_empty() {
            let request = ChatRequest {
                model: model_for(config, spec.role),
                messages: vec![ChatMessage::user(transcript)],
                system: Some(COMPACTION_SYSTEM_PROMPT.to_string()),
                max_tokens: Some(1024),
                temperature: Some(0.2),
                tools: Vec::new(),
            };
            let reservation = budget
                .limit
                .is_some()
                .then(|| {
                    budget.reserve_tokens(estimate_request_tokens(&request).saturating_add(1024))
                })
                .flatten();
            // When a token limit exists but the reservation cannot be granted,
            // skip the summary call; the rolling trim below still protects the
            // provider context and keeps the run alive.
            if budget.is_unlimited() || budget.limit.is_none() || reservation.is_some() {
                let outcome = collect_compaction_completion(
                    config.provider.as_ref(),
                    &request,
                    Duration::from_secs(config.provider_idle_timeout_secs),
                    control,
                )
                .await;
                match outcome {
                    Ok((summary, usage)) => {
                        let summary = summary.trim();
                        if !summary.is_empty() {
                            let (input_tokens, output_tokens, estimated_usage) = if usage != (0, 0)
                            {
                                (usage.0, usage.1, false)
                            } else {
                                (
                                    estimate_request_tokens(&request),
                                    estimate_text_tokens(summary),
                                    true,
                                )
                            };
                            let (input_price, output_price) = role_price(config, spec.role);
                            let cost =
                                usage_cost(input_tokens, output_tokens, input_price, output_price);
                            engine
                                .record_agent_usage(
                                    &session.id,
                                    &spec.id,
                                    &spec.name,
                                    "compaction",
                                    spec.task_id.clone(),
                                    spec.department.clone(),
                                    &request.model,
                                    input_tokens,
                                    output_tokens,
                                    estimated_usage,
                                    cost,
                                )
                                .await?;
                            if let Some(reservation) = reservation {
                                let _ =
                                    reservation.settle(input_tokens.saturating_add(output_tokens));
                            }
                            budget.add_cost(cost);
                            let prefixed = format!(
                                "Earlier conversation summary (auto-compacted from {cut} messages):\n{summary}"
                            );
                            conversation.drain(..cut);
                            conversation_ids.drain(..cut);
                            conversation.insert(0, ChatMessage::user(prefixed.clone()));
                            conversation_ids.insert(0, None);
                            summarized_messages = cut;
                            summary_text = Some(prefixed);
                            through_id = through;
                            usage_tokens = (input_tokens, output_tokens);
                            usage_estimated = estimated_usage;
                            estimated = calibrate_context_tokens(
                                estimate_messages_tokens(conversation).saturating_add(base_tokens),
                                calibration,
                            );
                        }
                    }
                    Err(error) => {
                        drop(reservation);
                        emit(
                            engine,
                            &session.id,
                            spec,
                            "context.compact_failed",
                            serde_json::json!({
                                "error": truncate(&format!("{error:#}")),
                            }),
                        )
                        .await?;
                    }
                }
            }
        }
    }

    // Rolling-window fallback: keep the summary at index 0 when one exists,
    // otherwise drop from the beginning at safe message boundaries. A manual
    // compaction that could not produce a summary still creates headroom by
    // trimming to half of the hard limit instead of doing nothing.
    let floor = usize::from(summary_text.is_some());
    let trim_target = if force && summary_text.is_none() {
        // A manual compaction that could not produce a summary still makes
        // headroom. Cap the target at `estimated - 1` so even a small context
        // produces a deterministic trim instead of reporting failure without
        // changing anything.
        (hard_limit / 2).max(1_024).min(estimated.saturating_sub(1))
    } else {
        hard_limit
    };
    let mut retention = keep;
    loop {
        if estimated <= trim_target {
            break;
        }
        let cut = match safe_context_cut(conversation, retention, floor) {
            Some(cut) => cut,
            None if retention > 2 => {
                retention = 2;
                continue;
            }
            None => break,
        };
        if cut <= floor {
            break;
        }
        let through = conversation_ids.get(cut - 1).cloned().flatten();
        if !conversation_ids.is_empty() && through.is_none() {
            if retention > 2 {
                retention = 2;
                continue;
            }
            break;
        }
        let removed = cut - floor;
        if floor == 0 {
            conversation.drain(..cut);
            conversation_ids.drain(..cut);
        } else {
            conversation.drain(floor..cut);
            conversation_ids.drain(floor..cut);
        }
        dropped_messages = dropped_messages.saturating_add(removed);
        if through.is_some() {
            through_id = through;
        }
        estimated = calibrate_context_tokens(
            estimate_messages_tokens(conversation).saturating_add(base_tokens),
            calibration,
        );
    }

    if summarized_messages == 0 && dropped_messages == 0 {
        return Ok(false);
    }
    let mode = if summary_text.is_some() {
        "summary"
    } else {
        "trim"
    };
    let kept_messages = conversation.len();
    emit(
        engine,
        &session.id,
        spec,
        "context.compacted",
        serde_json::json!({
            "mode": mode,
            "through_message_id": through_id,
            "summary": summary_text,
            "summarized_messages": summarized_messages,
            "dropped_messages": dropped_messages,
            "kept_messages": kept_messages,
            "estimated_tokens_before": estimated_before,
            "estimated_tokens_after": estimated,
            "threshold_tokens": compact_threshold,
            "hard_limit_tokens": hard_limit,
            "calibration": calibration,
            "input_tokens": usage_tokens.0,
            "output_tokens": usage_tokens.1,
            "estimated": usage_estimated,
        }),
    )
    .await?;
    Ok(true)
}

#[async_recursion]
#[allow(clippy::too_many_arguments)]
async fn run_loop(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    spec: &AgentSpec,
    objective: &str,
    conversation: &mut Vec<ChatMessage>,
    conversation_ids: Option<&mut Vec<Option<String>>>,
    top_level: bool,
) -> Result<LoopOutcome> {
    let result = run_loop_body(
        engine,
        config,
        session,
        project,
        control,
        budget,
        spec,
        objective,
        conversation,
        conversation_ids,
        top_level,
    )
    .await;
    // A run boundary is final for steering: anything still pending was not
    // consumed at a turn boundary and must not leak into a later run that
    // reuses this agent row.
    if let Err(error) = engine
        .cancel_pending_steering_for_agent(
            &session.id,
            &spec.id,
            "agent run ended before the steering message was delivered",
        )
        .await
    {
        tracing::warn!(%error, agent_id = %spec.id, "failed to cancel pending steering at run end");
    }
    result
}

#[allow(clippy::too_many_arguments)]
async fn run_loop_body(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    spec: &AgentSpec,
    objective: &str,
    conversation: &mut Vec<ChatMessage>,
    conversation_ids: Option<&mut Vec<Option<String>>>,
    top_level: bool,
) -> Result<LoopOutcome> {
    let mut empty_ids = Vec::new();
    let conversation_ids: &mut Vec<Option<String>> = match conversation_ids {
        Some(ids) => ids,
        None => &mut empty_ids,
    };
    let mut citations = CitationRegistry::default();
    let mut active_skill: Option<Skill> = None;
    let task_priority = if let Some(task_id) = &spec.task_id {
        engine
            .get_team_plan(&session.id)
            .await
            .ok()
            .flatten()
            .and_then(|plan| {
                plan.tasks
                    .iter()
                    .find(|task| &task.id == task_id)
                    .map(|task| task.priority)
            })
            .unwrap_or(0)
    } else {
        0
    };
    let capacity_priority = task_capacity_priority(session.priority, task_priority);
    let _capacity_permit =
        if matches!(spec.role, AgentRole::Worker | AgentRole::Labour) && !spec.auto_approve {
            match config.capacity.try_acquire(spec.role, &session.id) {
                Some(permit) => Some(permit),
                None => {
                    let snapshot = config.capacity.snapshot();
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "agent.capacity_wait",
                        serde_json::json!({
                            "role": spec.role.as_str(),
                            "limit": if spec.role == AgentRole::Worker {
                                snapshot.worker_limit
                            } else {
                                snapshot.labour_limit
                            },
                            "available": if spec.role == AgentRole::Worker {
                                snapshot.worker_available
                            } else {
                                snapshot.labour_available
                            },
                        }),
                    )
                    .await?;
                    engine.update_agent_status(&spec.id, "queued").await?;
                    let permit = match config
                        .capacity
                        .acquire(spec.role, &session.id, capacity_priority, control)
                        .await
                    {
                        Some(permit) => permit,
                        None => return Ok(LoopOutcome::Cancelled),
                    };
                    engine.update_agent_status(&spec.id, "running").await?;
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "agent.capacity_ready",
                        serde_json::json!({
                            "role": spec.role.as_str(),
                            "waited": true,
                        }),
                    )
                    .await?;
                    Some(permit)
                }
            }
        } else {
            None
        };
    let _scope_guard = if matches!(spec.role, AgentRole::Worker | AgentRole::Labour) {
        match config
            .scope_locks
            .acquire(
                &project.id,
                &session.id,
                &spec.id,
                &spec.write_scope,
                control,
            )
            .await?
        {
            Some(guard) => Some(guard),
            None => return Ok(LoopOutcome::Cancelled),
        }
    } else {
        None
    };
    let shell_policy = engine.get_shell_policy(&project.id).await?;
    let mut secret_roots = shell_secret_roots(engine.store().data_dir());
    secret_roots.extend(shell_policy.read_deny.iter().map(PathBuf::from));
    let tool_context = ToolContext {
        root: spec.worktree.clone(),
        project_root: PathBuf::from(&project.root),
        sandbox_root: engine
            .store()
            .data_dir()
            .join("sandbox")
            .join(&session.id)
            .join(&spec.id),
        secret_roots,
        network_allowed: shell_policy.network != "deny",
        home: std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default(),
        restrict_home: shell_policy.restrict_home,
        read_jail: shell_policy.read_jail,
        git_branch: Some(spec.branch.clone()),
    };
    let system = role_system_prompt(spec.role, project, session, spec, objective);
    let memory_brief = project_memory_brief(engine, &project.id, &session.goal, 6)
        .await
        .unwrap_or_default();
    let mut system = if memory_brief.is_empty() {
        system
    } else {
        format!("{system}\n\nLong-term project memory (persisted across sessions):\n{memory_brief}")
    };
    let source_brief = engine.source_brief(&project.id).await.unwrap_or_default();
    if !source_brief.is_empty() {
        system.push_str("\n\nIndexed project sources (use search_sources for content):\n");
        system.push_str(&source_brief);
    }
    let skill_catalog = engine
        .skill_catalog(&project.id, spec.role.as_str(), spec.profile.as_str())
        .await
        .unwrap_or_default();
    if !skill_catalog.is_empty() {
        system.push_str("\n\nProject skills (call use_skill with the exact name):\n");
        system.push_str(&skill_catalog);
    }
    let dynamic_mcp_tools = collect_dynamic_mcp_tools(engine, spec.role).await;
    let mut tools = tool_schemas(config, spec.role, spec.profile, spec.allow_delegation);
    tools.extend(
        dynamic_mcp_tools
            .iter()
            .map(|binding| binding.schema.clone()),
    );
    let system_tokens = estimate_context_tokens(&system);
    let tool_tokens = serde_json::to_string(&tools)
        .map(|schema| estimate_context_tokens(&schema))
        .unwrap_or(0);
    let (context_threshold, context_hard_limit) = context_limits(config, spec.role);
    if context_hard_limit != u64::MAX
        && system_tokens.saturating_add(tool_tokens) > context_hard_limit
    {
        return Err(anyhow!(
            "system prompt and tool schemas (~{} tokens) exceed the model context hard limit ({}); increase VERLINPARK_CONTEXT_WINDOW or role context_window, or reduce enabled tools/MCP servers",
            system_tokens.saturating_add(tool_tokens),
            context_hard_limit
        ));
    }
    let model_name = model_for(config, spec.role);
    let mut calibration = if top_level {
        load_context_calibration(engine, &session.id, &model_name).await
    } else {
        ContextCalibration::default()
    };
    let mut team_plan = engine.get_team_plan(&session.id).await?;
    let project_budget = engine.get_project_budget(&project.id).await?;
    let plan_budget = PlanBudgetScope::from_plan(
        team_plan.as_ref(),
        spec,
        Some(&project_budget.budget),
        &project.id,
    );
    let mut warned_project = false;

    for turn in 0..config.max_turns {
        if !wait_for_task_resume(engine, &session.id, spec, control).await? {
            return Ok(LoopOutcome::Cancelled);
        }
        if control.is_cancelled() {
            return Ok(LoopOutcome::Cancelled);
        }
        if budget.exhausted() {
            return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
        }
        if let Some(reason) = budget.warning() {
            emit(
                engine,
                &session.id,
                spec,
                "agent.budget_warning",
                serde_json::json!({
                    "reason": reason,
                    "limit": budget.limit,
                    "spent": budget.spent(),
                    "cost_limit_usd": budget.cost_limit,
                    "cost_spent_usd": budget.cost_spent(),
                    "time_budget_secs": budget.time_limit.map(|limit| limit.as_secs()),
                    "elapsed_secs": budget.elapsed_secs(),
                }),
            )
            .await?;
        }
        {
            compact_context_if_needed(
                engine,
                config,
                session,
                spec,
                budget,
                control,
                conversation,
                &mut *conversation_ids,
                context_threshold,
                context_hard_limit,
                system_tokens.saturating_add(tool_tokens),
                calibration.factor,
                false,
                context_strategy(spec.role),
            )
            .await?;
            if control.is_cancelled() {
                return Ok(LoopOutcome::Cancelled);
            }
            if budget.exhausted() {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
            }
            let context_window = context_window_for(config, spec.role);
            if context_window > 0 {
                let estimated_context = calibrate_context_tokens(
                    estimate_messages_tokens(conversation)
                        .saturating_add(system_tokens)
                        .saturating_add(tool_tokens),
                    calibration.factor,
                );
                emit(
                    engine,
                    &session.id,
                    spec,
                    "context.usage",
                    serde_json::json!({
                        "estimated_tokens": estimated_context,
                        "context_window": context_window,
                        "threshold_tokens": context_threshold,
                        "hard_limit_tokens": context_hard_limit,
                        "messages": conversation.len(),
                        "calibration": calibration.factor,
                    }),
                )
                .await?;
            }
        }
        if let Err(error) = engine
            .touch_task_progress(&session.id, spec.task_id.as_deref(), turn + 1, None)
            .await
        {
            tracing::warn!(%error, "failed to record task turn progress");
        }
        let delivered = engine
            .deliver_pending_steering(&session.id, &spec.id, spec.task_id.as_deref(), top_level)
            .await?;
        for steering in delivered {
            conversation.push(ChatMessage::user(steering.content));
            if let Some(message) = steering.message {
                conversation_ids.push(Some(message.id));
            }
        }
        let mut request = ChatRequest {
            model: model_name.clone(),
            messages: conversation.clone(),
            system: Some(system.clone()),
            max_tokens: config.max_tokens,
            temperature: Some(0.2),
            tools: tools
                .iter()
                .filter(|tool| {
                    active_skill
                        .as_ref()
                        .is_none_or(|skill| skill_allows_tool(skill, &tool.name))
                })
                .cloned()
                .collect(),
        };
        const MIN_OUTPUT_TOKENS: u64 = 64;
        let configured_output = config.max_tokens.unwrap_or(4096).max(1) as u64;
        let input_estimate = estimate_request_tokens(&request);
        let (input_price, output_price) = role_price(config, spec.role);
        let scope_spent = if plan_budget.active() && !budget.is_unlimited() {
            Some(
                engine
                    .usage_spent(
                        &session.id,
                        spec.task_id.as_deref(),
                        spec.department.as_deref(),
                    )
                    .await?,
            )
        } else {
            None
        };
        let task_spent = scope_spent
            .map(|(tokens, cost, _, _)| (tokens, cost))
            .unwrap_or((0, 0.0));
        let department_spent = scope_spent
            .map(|(_, _, tokens, cost)| (tokens, cost))
            .unwrap_or((0, 0.0));
        let project_spent = if plan_budget.project_active() && !budget.is_unlimited() {
            engine.project_usage(&project.id).await?
        } else {
            (0, 0.0)
        };
        if !budget.is_unlimited()
            && !warned_project
            && let Some(warning) = plan_budget.project_warning(project_spent)
        {
            emit(
                engine,
                &session.id,
                spec,
                "agent.budget_warning",
                warning.payload(),
            )
            .await?;
            warned_project = true;
        }
        let desired_output = if !budget.is_unlimited() && budget.limit.is_some() {
            let remaining = budget.available_tokens().unwrap_or(0);
            if remaining <= input_estimate.saturating_add(MIN_OUTPUT_TOKENS) {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session_tokens(
                    budget,
                )));
            }
            configured_output.min(remaining - input_estimate)
        } else {
            configured_output
        };
        let scope_lease = if plan_budget.active() && !budget.is_unlimited() {
            let requests = plan_budget.requests(
                &session.id,
                task_spent,
                department_spent,
                project_spent,
                input_price,
                output_price,
            );
            match engine.budget_registry().grant(
                &requests,
                input_estimate,
                desired_output,
                MIN_OUTPUT_TOKENS,
            ) {
                Ok(lease) => Some(lease),
                Err(rejection) => {
                    let stop = BudgetStop::from_rejection(rejection);
                    emit_scope_budget_stop(engine, &session.id, spec, &stop).await?;
                    return Ok(LoopOutcome::BudgetExceeded(stop));
                }
            }
        } else {
            None
        };
        let output_budget = scope_lease
            .as_ref()
            .map_or(desired_output, |lease| lease.granted_output_tokens());
        let mut token_reservation = None;
        if !budget.is_unlimited() && budget.limit.is_some() {
            let Some(reservation) =
                budget.reserve_tokens(input_estimate.saturating_add(output_budget))
            else {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session_tokens(
                    budget,
                )));
            };
            token_reservation = Some(reservation);
        }
        let mut cost_reservation =
            match budget.reserve_cost(input_estimate, output_budget, input_price, output_price) {
                Ok(reservation) => reservation,
                Err(()) => {
                    return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session_cost(
                        budget,
                    )));
                }
            };
        if output_budget < configured_output {
            request.max_tokens = Some(output_budget.min(u32::MAX as u64) as u32);
        }
        let _scope_lease = scope_lease;
        let stream_turn = consume_stream(
            engine,
            &session.id,
            spec,
            control,
            config.provider.as_ref(),
            &request,
            Duration::from_secs(config.provider_idle_timeout_secs),
        )
        .await?;
        if top_level && stream_turn.usage.0 > 0 {
            let estimated_request = estimate_messages_tokens(&request.messages)
                .saturating_add(system_tokens)
                .saturating_add(tool_tokens);
            let ratio = stream_turn.usage.0 as f64 / estimated_request.max(1) as f64;
            calibration.observe(estimated_request, stream_turn.usage.0);
            // Measurement is diagnostic; a failed append must not discard the
            // provider call's accounting or the turn output.
            if let Err(error) = emit(
                engine,
                &session.id,
                spec,
                "context.measured",
                serde_json::json!({
                    "estimated_tokens": estimated_request,
                    "actual_input_tokens": stream_turn.usage.0,
                    "actual_output_tokens": stream_turn.usage.1,
                    "ratio": ratio,
                    "calibration": calibration.factor,
                    "samples": calibration.samples,
                    "model": request.model.clone(),
                    "messages": request.messages.len(),
                }),
            )
            .await
            {
                tracing::warn!(%error, "could not persist context measurement");
            }
        }
        let has_output = !stream_turn.text.is_empty() || !stream_turn.tool_calls.is_empty();
        let reported = stream_turn.usage;
        let input_tokens = if reported.0 > 0 {
            reported.0
        } else if has_output || reported.1 > 0 {
            estimate_request_tokens(&request)
        } else {
            0
        };
        let output_tokens = if reported.1 > 0 {
            reported.1
        } else if has_output {
            estimate_output_tokens(&stream_turn.text, &stream_turn.tool_calls)
        } else {
            0
        };
        let estimated = reported.0 == 0 || reported.1 == 0;
        let record_usage = reported != (0, 0) || has_output;
        let mut budget_exceeded = false;
        if record_usage {
            let cost = usage_cost(input_tokens, output_tokens, input_price, output_price);
            engine
                .record_agent_usage(
                    &session.id,
                    &spec.id,
                    &spec.name,
                    spec.role.as_str(),
                    spec.task_id.clone(),
                    spec.department.clone(),
                    &request.model,
                    input_tokens,
                    output_tokens,
                    estimated,
                    cost,
                )
                .await?;
            let actual_usage = input_tokens.saturating_add(output_tokens);
            let cost_exceeded = if let Some(reservation) = cost_reservation.take() {
                reservation.settle(cost)
            } else {
                budget.add_cost(cost)
            };
            budget_exceeded = token_reservation
                .take()
                .is_some_and(|reservation| reservation.settle(actual_usage))
                || cost_exceeded;
        }
        if stream_turn.cancelled {
            return Ok(LoopOutcome::Cancelled);
        }
        if let Some(error) = stream_turn.error {
            // Keep partial streamed output visible instead of letting the
            // failure clear the live transcript.
            if !stream_turn.text.is_empty() {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.message",
                    serde_json::json!({
                        "text": stream_turn.text,
                        "tool_calls": stream_turn.tool_calls,
                        "partial": true,
                    }),
                )
                .await?;
            }
            return Err(error);
        }
        if budget_exceeded {
            return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
        }
        if !budget.is_unlimited() && plan_budget.active() {
            let (task_tokens, task_cost, department_tokens, department_cost) = engine
                .usage_spent(
                    &session.id,
                    spec.task_id.as_deref(),
                    spec.department.as_deref(),
                )
                .await?;
            let project_usage = if plan_budget.project_active() {
                engine.project_usage(&project.id).await?
            } else {
                (0, 0.0)
            };
            if let Some(stop) = plan_budget.after(
                (task_tokens, task_cost),
                (department_tokens, department_cost),
                project_usage,
            ) {
                emit_scope_budget_stop(engine, &session.id, spec, &stop).await?;
                return Ok(LoopOutcome::BudgetExceeded(stop));
            }
        }
        let text = stream_turn.text;
        let tool_calls = stream_turn.tool_calls;

        if text.is_empty() && tool_calls.is_empty() {
            emit(
                engine,
                &session.id,
                spec,
                "agent.turn_finished",
                serde_json::json!({"turn": turn + 1, "empty": true}),
            )
            .await?;
            return Ok(LoopOutcome::Final(String::new()));
        }

        let assistant_tool_calls = if tool_calls.is_empty() {
            None
        } else {
            Some(tool_calls.clone())
        };
        let stored_id = if top_level {
            let stored = engine
                .append_message(
                    &session.id,
                    "assistant",
                    &text,
                    serde_json::to_value(assistant_tool_calls.as_deref().unwrap_or(&[]))?,
                    None,
                )
                .await?;
            conversation_ids.push(Some(stored.id.clone()));
            Some(stored.id)
        } else {
            None
        };
        let citation_records = citations.list();
        emit(
            engine,
            &session.id,
            spec,
            "agent.message",
            serde_json::json!({
                "text": text,
                "tool_calls": tool_calls,
                "message_id": stored_id,
                "citations": citation_records,
            }),
        )
        .await?;
        conversation.push(ChatMessage {
            role: "assistant".to_string(),
            content: text.clone(),
            tool_calls: assistant_tool_calls,
            tool_call_id: None,
        });

        if !wait_for_task_resume(engine, &session.id, spec, control).await? {
            return Ok(LoopOutcome::Cancelled);
        }
        if tool_calls.is_empty() {
            emit(
                engine,
                &session.id,
                spec,
                "agent.turn_finished",
                serde_json::json!({"turn": turn + 1}),
            )
            .await?;
            return Ok(LoopOutcome::Final(text));
        }

        let safe_parallel_batch = tool_calls.len() > 1
            && tool_calls.iter().all(|call| {
                is_parallel_safe_tool(&call.name)
                    && spec.role.allows_tool(spec.profile, &call.name)
                    && active_skill
                        .as_ref()
                        .is_none_or(|skill| skill_allows_tool(skill, &call.name))
                    && config.tools.get(&call.name).is_some()
            });
        if safe_parallel_batch {
            let mut prepared = Vec::new();
            for (index, call) in tool_calls.iter().enumerate() {
                let call_id = if call.id.is_empty() {
                    format!("call_{}_{}", turn, index)
                } else {
                    call.id.clone()
                };
                let input: serde_json::Value =
                    serde_json::from_str(&call.arguments).unwrap_or_else(|_| serde_json::json!({}));
                let assessment = assess_tool_call(&call.name, &input, &spec.worktree);
                if !matches!(
                    decide(config.approval_mode, &assessment, spec.auto_approve),
                    ApprovalDecision::Allow { .. }
                ) {
                    prepared.clear();
                    break;
                }
                let tool = config
                    .tools
                    .get(&call.name)
                    .expect("parallel tools are registry-backed");
                prepared.push((index, call_id, call.name.clone(), input, tool));
            }
            if !prepared.is_empty() {
                for (_, call_id, name, input, _) in &prepared {
                    if let Err(error) = engine
                        .touch_task_progress(
                            &session.id,
                            spec.task_id.as_deref(),
                            turn + 1,
                            Some(name),
                        )
                        .await
                    {
                        tracing::warn!(%error, "failed to record task tool progress");
                    }
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "tool.started",
                        serde_json::json!({
                            "tool_call_id": call_id,
                            "name": name,
                            "input": input,
                        }),
                    )
                    .await?;
                }
                let mut cancel_rx = control.subscribe();
                let mut pending = prepared.into_iter();
                let mut set = tokio::task::JoinSet::new();
                for _ in 0..4 {
                    let Some((index, call_id, name, input, tool)) = pending.next() else {
                        break;
                    };
                    let context = tool_context.clone();
                    set.spawn(async move {
                        let output = match tool.execute(&context, input).await {
                            Ok(output) => output,
                            Err(error) => ToolOutput::error(format!("tool error: {error}")),
                        };
                        (index, call_id, name, output)
                    });
                }
                let mut results = Vec::new();
                loop {
                    if set.is_empty() && pending.len() == 0 {
                        break;
                    }
                    tokio::select! {
                        _ = cancel_rx.changed() => {
                            set.abort_all();
                            return Ok(LoopOutcome::Cancelled);
                        }
                        joined = set.join_next() => {
                            match joined {
                                Some(Ok(item)) => results.push(item),
                                Some(Err(error)) => {
                                    return Err(anyhow!("parallel tool task failed: {error}"));
                                }
                                None => break,
                            }
                            if let Some((index, call_id, name, input, tool)) = pending.next() {
                                let context = tool_context.clone();
                                set.spawn(async move {
                                    let output = match tool.execute(&context, input).await {
                                        Ok(output) => output,
                                        Err(error) => ToolOutput::error(format!("tool error: {error}")),
                                    };
                                    (index, call_id, name, output)
                                });
                            }
                        }
                    }
                }
                results.sort_by_key(|(index, _, _, _)| *index);
                for (_, call_id, name, output) in results {
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "tool.finished",
                        serde_json::json!({
                            "tool_call_id": call_id,
                            "name": name,
                            "output": truncate(&output.content),
                            "is_error": output.is_error,
                            "structured": crate::tools::bounded_structured(output.structured.clone()),
                        }),
                    )
                    .await?;
                    if top_level {
                        let stored = engine
                            .append_message(
                                &session.id,
                                "tool",
                                &output.content,
                                serde_json::json!([]),
                                Some(call_id.clone()),
                            )
                            .await?;
                        conversation_ids.push(Some(stored.id));
                    }
                    conversation.push(ChatMessage::tool(call_id, output.content));
                }
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.turn_finished",
                    serde_json::json!({"turn": turn + 1, "parallel": true}),
                )
                .await?;
                continue;
            }
        }

        for (index, call) in tool_calls.into_iter().enumerate() {
            if !wait_for_task_resume(engine, &session.id, spec, control).await? {
                return Ok(LoopOutcome::Cancelled);
            }
            if control.is_cancelled() {
                return Ok(LoopOutcome::Cancelled);
            }
            if budget.exhausted() {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
            }
            if let Err(error) = engine
                .touch_task_progress(
                    &session.id,
                    spec.task_id.as_deref(),
                    turn + 1,
                    Some(&call.name),
                )
                .await
            {
                tracing::warn!(%error, "failed to record task tool progress");
            }
            let call_id = if call.id.is_empty() {
                format!("call_{}_{}", turn, index)
            } else {
                call.id.clone()
            };
            let input: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|_| serde_json::json!({}));
            let is_delegate = call.name == "delegate" && spec.allow_delegation;
            let is_acp_delegate = call.name == "delegate_acp"
                && spec.allow_delegation
                && matches!(spec.role, AgentRole::Leader | AgentRole::Manager);
            let is_delegate_many = call.name == "delegate_many"
                && spec.role == AgentRole::Manager
                && spec.allow_delegation;
            let is_verdict = call.name == "submit_verdict" && spec.role == AgentRole::Verifier;
            let is_team_plan = call.name == "submit_team_plan" && spec.role == AgentRole::Leader;
            let is_run_plan = call.name == "run_team_plan" && spec.role == AgentRole::Leader;
            let is_deliberate =
                call.name == "deliberate_team_plan" && spec.role == AgentRole::Leader;
            let is_todo = call.name == "todo_write" && spec.role == AgentRole::Leader;
            let is_mailbox_tool = matches!(
                call.name.as_str(),
                "send_message"
                    | "read_mailbox"
                    | "publish_artifact"
                    | "list_artifacts"
                    | "read_artifact"
            );
            let is_memory_tool = matches!(call.name.as_str(), "remember" | "recall");
            let is_source_tool = matches!(
                call.name.as_str(),
                "search_sources" | "read_source" | "list_sources"
            );
            let is_skill_tool = call.name == "use_skill";
            let is_question_tool = call.name == "ask_user" && spec.role != AgentRole::Verifier;
            let is_mcp_tool = call.name == "mcp";
            let is_coordination_tool = call.name == "coordinate";
            let is_team_status = call.name == "team_status";
            let is_budget_status = call.name == "budget_status";
            let is_task_detail = call.name == "task_detail";
            let is_task_control = call.name == "task_control"
                && matches!(spec.role, AgentRole::Leader | AgentRole::Manager);
            let is_tool_search = call.name == "tool_search";
            let is_artifact_file = call.name == "publish_artifact_file";
            let is_browser =
                call.name == "browser" && spec.role.allows_tool(spec.profile, "browser");
            let dynamic_mcp_tool = dynamic_mcp_tools
                .iter()
                .find(|binding| binding.exposed_name == call.name);
            let registry_tool = config
                .tools
                .get(&call.name)
                .filter(|_| spec.role.allows_tool(spec.profile, &call.name));
            let mut assessment = assess_tool_call(&call.name, &input, &spec.worktree);
            if dynamic_mcp_tool.is_some() {
                assessment = crate::permissions::PermissionAssessment::new(
                    RiskLevel::HostState,
                    "dynamic MCP tool execution can change host or remote state",
                );
            }
            let granted = !spec.auto_approve
                && engine
                    .approval_grant_matches(&session.project_id, &call.name, &input)
                    .await
                    .unwrap_or(false);
            let decision = if granted {
                crate::permissions::ApprovalDecision::Allow {
                    risk: assessment.risk,
                    reason: "matched a project approval grant".to_string(),
                }
            } else {
                decide(config.approval_mode, &assessment, spec.auto_approve)
            };
            let mut denied_reason: Option<String> = None;
            match decision {
                ApprovalDecision::Allow { risk, reason } => {
                    if risk >= RiskLevel::External {
                        emit(
                            engine,
                            &session.id,
                            spec,
                            "permission.allowed",
                            serde_json::json!({
                                "tool": call.name,
                                "tool_call_id": call_id,
                                "risk": risk.as_str(),
                                "reason": reason,
                            }),
                        )
                        .await?;
                    }
                }
                ApprovalDecision::Ask { risk, reason } => {
                    let mut context = serde_json::json!({});
                    agent_fields(&mut context, spec);
                    let (approval, receiver) = engine
                        .request_approval_with_risk(
                            &session.id,
                            &call_id,
                            &call.name,
                            input.clone(),
                            Some(context),
                            Some(risk.as_str()),
                            Some(&reason),
                        )
                        .await?;
                    match wait_for_approval(
                        receiver,
                        control,
                        Duration::from_secs(config.approval_timeout_secs),
                    )
                    .await
                    {
                        ApprovalOutcome::Approved => {}
                        ApprovalOutcome::Denied => {
                            denied_reason = Some("The user denied this tool call.".to_string());
                        }
                        ApprovalOutcome::TimedOut => {
                            denied_reason = Some("The approval request timed out.".to_string());
                            let _ = engine.resolve_approval(&approval.id, false).await;
                        }
                        ApprovalOutcome::Cancelled => {
                            let _ = engine.resolve_approval(&approval.id, false).await;
                            return Ok(LoopOutcome::Cancelled);
                        }
                    }
                }
                ApprovalDecision::Deny { risk, reason } => {
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "permission.denied",
                        serde_json::json!({
                            "tool": call.name,
                            "tool_call_id": call_id,
                            "risk": risk.as_str(),
                            "reason": reason,
                        }),
                    )
                    .await?;
                    denied_reason =
                        Some(format!("Blocked by the local permission policy: {reason}"));
                }
            }

            let skill_denied = active_skill
                .as_ref()
                .filter(|skill| !skill_allows_tool(skill, &call.name));
            let output = if let Some(reason) = denied_reason {
                ToolOutput::error(reason)
            } else if let Some(skill) = skill_denied {
                ToolOutput::error(format!(
                    "tool '{}' is not allowed by active skill '{}'",
                    call.name, skill.name
                ))
            } else if is_deliberate {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match engine.get_team_plan(&session.id).await? {
                    None => ToolOutput::error("no TeamPlan submitted; call submit_team_plan first"),
                    Some(plan) => {
                        let rounds = input
                            .get("rounds")
                            .and_then(serde_json::Value::as_u64)
                            .unwrap_or(2)
                            .clamp(1, 3) as u32;
                        match deliberate_team_plan(
                            engine, config, session, project, control, budget, spec, &plan, rounds,
                        )
                        .await?
                        {
                            RoundtableResult::Done(report) => ToolOutput::ok(report),
                            RoundtableResult::Cancelled => {
                                return Ok(LoopOutcome::Cancelled);
                            }
                            RoundtableResult::BudgetExceeded(stop) => {
                                if stop.is_abortive() {
                                    return Ok(LoopOutcome::BudgetExceeded(stop));
                                }
                                ToolOutput::error(stop.summary())
                            }
                        }
                    }
                }
            } else if is_run_plan {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match engine.get_team_plan(&session.id).await? {
                    None => ToolOutput::error("no TeamPlan submitted; call submit_team_plan first"),
                    Some(plan) => match execute_team_plan(
                        engine, config, session, project, control, budget, spec, &plan, None,
                    )
                    .await?
                    {
                        PlanRunResult::Done(report) => ToolOutput::ok(report),
                        PlanRunResult::Cancelled => return Ok(LoopOutcome::Cancelled),
                        PlanRunResult::BudgetExceeded(stop) => {
                            return Ok(LoopOutcome::BudgetExceeded(stop));
                        }
                    },
                }
            } else if is_team_plan {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let parsed = serde_json::from_value::<TeamPlan>(input.clone())
                    .map_err(|error| anyhow!("invalid team plan: {error}"))
                    .and_then(|plan| {
                        validate_team_plan(&plan, config.max_managers)?;
                        Ok(plan)
                    });
                match parsed {
                    Ok(plan) => {
                        let summary = format!(
                            "Team plan recorded: {} departments, {} tasks. Delegate to Managers using the planned titles.",
                            plan.departments.len(),
                            plan.tasks.len()
                        );
                        engine.set_team_plan(&session.id, &plan).await?;
                        team_plan = Some(plan);
                        ToolOutput::ok(summary)
                    }
                    Err(error) => ToolOutput::error(error.to_string()),
                }
            } else if is_coordination_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_coordination_tool(engine, session, spec, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("coordination error: {error}")),
                }
            } else if is_team_status {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_team_status(engine, session, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("team status error: {error}")),
                }
            } else if is_budget_status {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_budget_status(engine, session, project, spec, budget, &plan_budget).await
                {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("budget status error: {error}")),
                }
            } else if is_task_detail {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_task_detail(engine, session, spec, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("task detail error: {error}")),
                }
            } else if is_task_control {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_task_control(engine, session, spec, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("task control error: {error}")),
                }
            } else if is_tool_search {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_tool_search(config, engine, spec, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("tool search error: {error}")),
                }
            } else if is_artifact_file {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_publish_artifact_file(engine, session, spec, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("artifact publish error: {error}")),
                }
            } else if is_browser {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match engine
                    .browser_action(
                        &session.id,
                        &spec.worktree,
                        &spec.id,
                        &spec.name,
                        input.clone(),
                    )
                    .await
                {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("browser error: {error}")),
                }
            } else if is_mailbox_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_mailbox_tool(engine, session, spec, &call.name, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("mailbox error: {error}")),
                }
            } else if let Some(binding) = dynamic_mcp_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match engine
                    .mcp_call(&binding.server_id, &binding.tool_name, input.clone())
                    .await
                {
                    Ok(result) => ToolOutput::ok(format_mcp_tool_result(&result.result)),
                    Err(error) => ToolOutput::error(format!("MCP error: {error}")),
                }
            } else if is_mcp_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_mcp_tool(engine, spec.role, input.clone()).await {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("MCP error: {error}")),
                }
            } else if is_question_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let question = input
                    .get("question")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow!("question is required"))?;
                let options = input
                    .get("options")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect::<Vec<_>>();
                let (item, receiver) = engine
                    .request_question(&session.id, &spec.id, &spec.name, question, options)
                    .await?;
                engine
                    .update_session_status(&session.id, "waiting_input")
                    .await?;
                engine
                    .update_agent_status(&spec.id, "waiting_input")
                    .await?;
                emit(
                    engine,
                    &session.id,
                    spec,
                    "agent.waiting_input",
                    serde_json::json!({
                        "question_id": item.id,
                        "question": item.question,
                        "options": item.options,
                    }),
                )
                .await?;
                match wait_for_question(receiver, control, config.question_timeout_secs).await {
                    QuestionOutcome::Answered(answer) => {
                        engine.update_session_status(&session.id, "running").await?;
                        engine.update_agent_status(&spec.id, "running").await?;
                        ToolOutput::ok(format!("User answered: {answer}"))
                    }
                    QuestionOutcome::Cancelled => {
                        let _ = engine.close_question(&item.id, "cancelled").await;
                        engine.update_agent_status(&spec.id, "cancelled").await?;
                        return Ok(LoopOutcome::Cancelled);
                    }
                    QuestionOutcome::TimedOut => {
                        let _ = engine.close_question(&item.id, "expired").await;
                        engine.update_session_status(&session.id, "running").await?;
                        engine.update_agent_status(&spec.id, "running").await?;
                        ToolOutput::error("User did not answer before the question timed out.")
                    }
                }
            } else if is_skill_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let output =
                    match run_skill_tool(engine, project, spec, input.clone(), &mut active_skill)
                        .await
                    {
                        Ok(output) => output,
                        Err(error) => ToolOutput::error(format!("skill error: {error}")),
                    };
                if !output.is_error
                    && let Some(skill) = &active_skill
                {
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "skill.activated",
                        serde_json::json!({
                            "name": skill.name,
                            "slug": skill.slug,
                            "version": skill.version,
                            "role": spec.role.as_str(),
                            "profile": spec.profile.as_str(),
                            "tool_allow": skill.tool_allow,
                            "tool_deny": skill.tool_deny,
                            "agent_id": spec.id,
                            "agent_name": spec.name,
                            "task_id": spec.task_id,
                        }),
                    )
                    .await?;
                }
                output
            } else if is_source_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_source_tool(engine, project, &call.name, input.clone(), &mut citations)
                    .await
                {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("source error: {error}")),
                }
            } else if is_memory_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_memory_tool(engine, session, project, spec, &call.name, input.clone())
                    .await
                {
                    Ok(output) => output,
                    Err(error) => ToolOutput::error(format!("memory error: {error}")),
                }
            } else if is_verdict {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match parse_verdict(&input) {
                    Ok(verdict) => {
                        emit(
                            engine,
                            &session.id,
                            spec,
                            "tool.finished",
                            serde_json::json!({
                                "tool_call_id": call_id,
                                "name": call.name,
                                "output": "verdict recorded",
                                "is_error": false,
                            }),
                        )
                        .await?;
                        return Ok(LoopOutcome::Verdict(verdict));
                    }
                    Err(error) => ToolOutput::error(error.to_string()),
                }
            } else if is_delegate_many {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match run_parallel_workers(
                    engine, config, session, project, control, budget, spec, &input,
                )
                .await?
                {
                    ParallelResult::Output(output) => output,
                    ParallelResult::Cancelled => return Ok(LoopOutcome::Cancelled),
                    ParallelResult::BudgetExceeded(stop) => {
                        return Ok(LoopOutcome::BudgetExceeded(stop));
                    }
                }
            } else if is_delegate {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match parse_delegate(&input, spec.role, spec.profile, team_plan.as_ref()) {
                    Ok((role, name, child_objective, title, child_profile)) => {
                        let reservation = if role == AgentRole::Manager {
                            budget.reserve(role).map(|ordinal| (ordinal, None))
                        } else {
                            budget
                                .reserve_slot(role)
                                .map(|(ordinal, slot)| (ordinal, Some(slot)))
                        };
                        match reservation {
                            None => ToolOutput::error(format!(
                                "the {} cap for this session has been reached",
                                role.as_str()
                            )),
                            Some((ordinal, _slot)) => {
                                let child_name = name.clone().unwrap_or_else(|| {
                                    title.clone().unwrap_or_else(|| {
                                        format!("{} {}", role.default_name(), ordinal)
                                    })
                                });
                                let child = engine
                                    .create_agent(
                                        &session.id,
                                        role.as_str(),
                                        &child_name,
                                        title.clone(),
                                        Some(spec.id.clone()),
                                        &model_for(config, role),
                                    )
                                    .await?;
                                engine
                                    .set_agent_profile(&child.id, child_profile.as_str())
                                    .await?;
                                let write_scope = parse_write_scope(&input)?;
                                let (child_worktree, child_branch, child_base, child_scope) =
                                    match role {
                                        AgentRole::Worker => {
                                            let info = engine
                                                .create_task_worktree(
                                                    &project.root,
                                                    &project.id,
                                                    &session.id,
                                                    &child.id,
                                                    &spec.session_branch,
                                                )
                                                .await?;
                                            (info.path, info.branch, info.base_commit, write_scope)
                                        }
                                        AgentRole::Manager => (
                                            spec.session_worktree.clone(),
                                            spec.session_branch.clone(),
                                            spec.base_commit.clone(),
                                            Vec::new(),
                                        ),
                                        AgentRole::Labour => (
                                            spec.worktree.clone(),
                                            spec.branch.clone(),
                                            spec.base_commit.clone(),
                                            spec.write_scope.clone(),
                                        ),
                                        AgentRole::Leader => (
                                            spec.session_worktree.clone(),
                                            spec.session_branch.clone(),
                                            spec.base_commit.clone(),
                                            Vec::new(),
                                        ),
                                        AgentRole::Verifier => (
                                            spec.worktree.clone(),
                                            spec.branch.clone(),
                                            spec.base_commit.clone(),
                                            Vec::new(),
                                        ),
                                    };
                                engine
                                    .set_agent_worktree(
                                        &child.id,
                                        &child_worktree.to_string_lossy(),
                                        &child_branch,
                                    )
                                    .await?;
                                engine
                                    .set_agent_task(
                                        &child.id,
                                        (role == AgentRole::Labour)
                                            .then(|| spec.task_id.clone())
                                            .flatten(),
                                        spec.department.clone(),
                                    )
                                    .await?;
                                let child_spec = AgentSpec {
                                    id: child.id.clone(),
                                    role,
                                    name: child_name.clone(),
                                    depth: spec.depth.saturating_add(1),
                                    parent_name: Some(spec.name.clone()),
                                    title: title.clone(),
                                    task_id: if role == AgentRole::Labour {
                                        spec.task_id.clone()
                                    } else {
                                        None
                                    },
                                    department: spec.department.clone(),
                                    profile: child_profile,
                                    allow_delegation: role.child_role().is_some(),
                                    auto_approve: false,
                                    worktree: child_worktree,
                                    branch: child_branch,
                                    session_worktree: spec.session_worktree.clone(),
                                    session_branch: spec.session_branch.clone(),
                                    write_scope: child_scope,
                                    base_commit: child_base,
                                };
                                emit(
                                    engine,
                                    &session.id,
                                    &child_spec,
                                    "agent.spawned",
                                    serde_json::json!({
                                        "parent_id": spec.id,
                                        "parent_name": spec.name,
                                        "objective": truncate(child_objective.as_str()),
                                    }),
                                )
                                .await?;
                                let mut child_conversation =
                                    vec![ChatMessage::user(child_objective.clone())];
                                let child_result = run_loop(
                                    engine,
                                    config,
                                    session,
                                    project,
                                    control,
                                    budget,
                                    &child_spec,
                                    &child_objective,
                                    &mut child_conversation,
                                    None,
                                    false,
                                )
                                .await;
                                match child_result {
                                    Ok(LoopOutcome::Verdict(_)) => ToolOutput::error(
                                        "a child agent returned a verdict unexpectedly",
                                    ),
                                    Ok(LoopOutcome::Final(summary)) => {
                                        let summary = if summary.trim().is_empty() {
                                            "The agent completed without a summary.".to_string()
                                        } else {
                                            summary
                                        };
                                        if role == AgentRole::Worker {
                                            match finish_worker(
                                                engine,
                                                config,
                                                session,
                                                project,
                                                control,
                                                budget,
                                                spec,
                                                &child_spec,
                                                &child_objective,
                                                &summary,
                                                None,
                                            )
                                            .await?
                                            {
                                                WorkerFinish::Done { report, success } => {
                                                    let text = format!(
                                                        "Worker {} finished:\n{}",
                                                        child_name, report
                                                    );
                                                    if success {
                                                        ToolOutput::ok(text)
                                                    } else {
                                                        ToolOutput::error(text)
                                                    }
                                                }
                                                WorkerFinish::Cancelled => {
                                                    return Ok(LoopOutcome::Cancelled);
                                                }
                                                WorkerFinish::BudgetExceeded(stop) => {
                                                    if stop.is_abortive() {
                                                        return Ok(LoopOutcome::BudgetExceeded(
                                                            stop,
                                                        ));
                                                    }
                                                    ToolOutput::error(stop.summary())
                                                }
                                            }
                                        } else {
                                            engine.update_agent_status(&child.id, "idle").await?;
                                            emit(
                                                engine,
                                                &session.id,
                                                &child_spec,
                                                "agent.finished",
                                                serde_json::json!({
                                                    "status": "idle",
                                                    "summary": truncate(&summary),
                                                    "worktree": child_spec.worktree,
                                                    "branch": child_spec.branch,
                                                }),
                                            )
                                            .await?;
                                            ToolOutput::ok(format!(
                                                "{} {} finished:\n{}",
                                                role.default_name(),
                                                child_name,
                                                truncate(&summary)
                                            ))
                                        }
                                    }
                                    Ok(LoopOutcome::Cancelled) => {
                                        engine.update_agent_status(&child.id, "cancelled").await?;
                                        emit(
                                            engine,
                                            &session.id,
                                            &child_spec,
                                            "agent.finished",
                                            serde_json::json!({"status": "cancelled"}),
                                        )
                                        .await?;
                                        return Ok(LoopOutcome::Cancelled);
                                    }
                                    Ok(LoopOutcome::BudgetExceeded(stop)) => {
                                        engine.update_agent_status(&child.id, "failed").await?;
                                        emit(
                                            engine,
                                            &session.id,
                                            &child_spec,
                                            "agent.finished",
                                            serde_json::json!({
                                                "status": "failed",
                                                "summary": stop.summary(),
                                            }),
                                        )
                                        .await?;
                                        return Ok(LoopOutcome::BudgetExceeded(stop));
                                    }
                                    Err(error) => {
                                        engine.update_agent_status(&child.id, "failed").await?;
                                        emit(
                                            engine,
                                            &session.id,
                                            &child_spec,
                                            "agent.finished",
                                            serde_json::json!({
                                                "status": "failed",
                                                "summary": truncate(error.to_string().as_str()),
                                            }),
                                        )
                                        .await?;
                                        ToolOutput::error(format!(
                                            "{} {} failed: {error}",
                                            role.default_name(),
                                            child_name
                                        ))
                                    }
                                }
                            }
                        }
                    }
                    Err(error) => ToolOutput::error(error.to_string()),
                }
            } else if is_acp_delegate {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match delegate_acp(
                    engine, config, session, project, control, budget, spec, &input,
                )
                .await?
                {
                    AcpDelegation::Output(output) => output,
                    AcpDelegation::Cancelled => return Ok(LoopOutcome::Cancelled),
                    AcpDelegation::BudgetExceeded(stop) => {
                        return Ok(LoopOutcome::BudgetExceeded(stop));
                    }
                }
            } else if is_todo {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                match parse_todos(&input) {
                    Ok(items) => match engine.set_todos(&session.id, items).await {
                        Ok(()) => ToolOutput::ok("Updated the session plan.".to_string()),
                        Err(error) => ToolOutput::error(format!("todo_write failed: {error}")),
                    },
                    Err(error) => ToolOutput::error(format!("todo_write failed: {error}")),
                }
            } else if let Some(tool) = registry_tool {
                emit(
                    engine,
                    &session.id,
                    spec,
                    "tool.started",
                    serde_json::json!({
                        "tool_call_id": call_id,
                        "name": call.name,
                        "input": input,
                    }),
                )
                .await?;
                let mut cancel_rx = control.subscribe();
                if control.is_cancelled() {
                    return Ok(LoopOutcome::Cancelled);
                }
                tokio::select! {
                    _ = cancel_rx.changed() => {
                        return Ok(LoopOutcome::Cancelled);
                    }
                    result = tool.execute(&tool_context, input.clone()) => match result {
                        Ok(output) => output,
                        Err(error) => ToolOutput::error(format!("tool error: {error}")),
                    },
                }
            } else {
                ToolOutput::error(format!(
                    "tool '{}' is not available to {}",
                    call.name,
                    spec.role.as_str()
                ))
            };

            emit(
                engine,
                &session.id,
                spec,
                "tool.finished",
                serde_json::json!({
                    "tool_call_id": call_id,
                    "name": call.name,
                    "output": truncate(&output.content),
                    "is_error": output.is_error,
                    "structured": crate::tools::bounded_structured(output.structured.clone()),
                }),
            )
            .await?;
            if top_level {
                let stored = engine
                    .append_message(
                        &session.id,
                        "tool",
                        &output.content,
                        serde_json::json!([]),
                        Some(call_id.clone()),
                    )
                    .await?;
                conversation_ids.push(Some(stored.id));
            }
            conversation.push(ChatMessage::tool(call_id, output.content));
        }
        emit(
            engine,
            &session.id,
            spec,
            "agent.turn_finished",
            serde_json::json!({"turn": turn + 1}),
        )
        .await?;
    }

    Err(anyhow!("maximum turns reached"))
}

async fn collect_dynamic_mcp_tools(engine: &Engine, role: AgentRole) -> Vec<McpToolBinding> {
    if !matches!(
        role,
        AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
    ) {
        return Vec::new();
    }
    let servers = match engine.list_mcp_servers().await {
        Ok(result) => result.servers,
        Err(error) => {
            tracing::warn!(%error, "MCP dynamic tool discovery could not list servers");
            return Vec::new();
        }
    };
    let mut bindings = Vec::new();
    let mut used = HashSet::new();
    for server in servers.into_iter().filter(|server| server.enabled) {
        if bindings.len() >= MAX_DYNAMIC_MCP_TOOLS {
            break;
        }
        let tools = match engine.mcp_tools(&server.id).await {
            Ok(result) => result.tools,
            Err(error) => {
                tracing::warn!(%error, server = %server.name, "MCP dynamic tool discovery failed");
                continue;
            }
        };
        for tool in tools {
            if bindings.len() >= MAX_DYNAMIC_MCP_TOOLS {
                break;
            }
            let exposed_name = dynamic_mcp_tool_name(&server.name, &tool.name);
            if !used.insert(exposed_name.clone()) {
                continue;
            }
            let mut parameters = if tool.input_schema.is_object() {
                tool.input_schema.clone()
            } else {
                serde_json::json!({})
            };
            if parameters.get("type").is_none()
                && let Some(object) = parameters.as_object_mut()
            {
                object.insert("type".to_string(), serde_json::json!("object"));
            }
            bindings.push(McpToolBinding {
                exposed_name: exposed_name.clone(),
                server_id: server.id.clone(),
                tool_name: tool.name.clone(),
                schema: ToolSchema {
                    name: exposed_name,
                    description: truncate_to(
                        &format!("[MCP {}] {}", server.name, tool.description),
                        400,
                    ),
                    parameters,
                },
            });
        }
    }
    bindings
}

fn dynamic_mcp_tool_name(server: &str, tool: &str) -> String {
    fn sanitize(value: &str) -> String {
        let sanitized = value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_lowercase()
                } else {
                    '_'
                }
            })
            .collect::<String>();
        sanitized.trim_matches('_').to_string()
    }
    let mut name = format!("mcp_{}_{}", sanitize(server), sanitize(tool));
    if name.len() > 64 {
        name.truncate(64);
    }
    if name.is_empty() || name == "mcp__" {
        "mcp_tool".to_string()
    } else {
        name
    }
}

fn format_mcp_tool_result(result: &serde_json::Value) -> String {
    let mut text = String::new();
    if let Some(content) = result.get("content").and_then(serde_json::Value::as_array) {
        for item in content {
            if let Some(value) = item.get("text").and_then(serde_json::Value::as_str) {
                text.push_str(value);
                text.push('\n');
            } else {
                text.push_str(&serde_json::to_string(item).unwrap_or_else(|_| item.to_string()));
                text.push('\n');
            }
        }
    }
    if text.trim().is_empty() {
        text = serde_json::to_string_pretty(result).unwrap_or_else(|_| result.to_string());
    }
    truncate_to(&text, 16_000)
}

async fn run_coordination_tool(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: serde_json::Value,
) -> Result<ToolOutput> {
    if spec.role == AgentRole::Verifier {
        return Ok(ToolOutput::error(
            "the Verifier records evidence through submit_verdict, not coordination notes",
        ));
    }
    let action = input
        .get("action")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("action is required"))?;
    if !matches!(
        action,
        "note" | "blocker" | "unblock" | "handoff" | "escalate" | "decision" | "list"
    ) {
        return Ok(ToolOutput::error(format!(
            "unsupported coordination action: {action}"
        )));
    }
    if action == "list" {
        let limit = input
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 50) as usize;
        let notes = recent_coordination_notes(engine, &session.id, limit).await?;
        if notes.is_empty() {
            return Ok(ToolOutput::ok("No coordination notes yet."));
        }
        return Ok(ToolOutput::ok(
            notes
                .iter()
                .map(format_coordination_note)
                .collect::<Vec<_>>()
                .join("\n"),
        ));
    }
    if matches!(action, "unblock" | "decision")
        && !matches!(spec.role, AgentRole::Leader | AgentRole::Manager)
    {
        return Ok(ToolOutput::error(format!(
            "only a Leader or Manager may record a coordination {action}"
        )));
    }
    let message = input
        .get("message")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|message| !message.is_empty())
        .ok_or_else(|| anyhow!("message is required"))?;
    if message.chars().count() > 2_000 {
        return Ok(ToolOutput::error(
            "coordination message is limited to 2000 chars",
        ));
    }
    let task_id = input
        .get("task_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|task_id| !task_id.is_empty())
        .map(str::to_string);
    if let Some(task_id) = &task_id {
        worktree::validate_task_id(task_id)?;
    }
    let target_agent_id = input
        .get("target_agent_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|target| !target.is_empty())
        .map(str::to_string);
    if let Some(target) = &target_agent_id {
        let agents = engine.list_agents(&session.id).await?;
        if !agents.iter().any(|agent| &agent.id == target) {
            return Ok(ToolOutput::error(format!(
                "target agent is not in this session: {target}"
            )));
        }
    }
    let severity = input
        .get("severity")
        .and_then(serde_json::Value::as_str)
        .unwrap_or(if matches!(action, "blocker" | "escalate") {
            "warning"
        } else {
            "info"
        });
    if !matches!(severity, "info" | "warning" | "critical") {
        return Ok(ToolOutput::error(
            "severity must be info, warning, or critical",
        ));
    }
    engine
        .record_coordination_note(
            &session.id,
            action,
            severity,
            task_id.as_deref(),
            target_agent_id.as_deref(),
            None,
            &spec.id,
            &spec.name,
            Some(spec.role.as_str()),
            message,
        )
        .await?;
    let scope = task_id
        .as_deref()
        .map(|task_id| format!(" for {task_id}"))
        .unwrap_or_default();
    Ok(ToolOutput::ok(format!(
        "recorded {} coordination{scope}: {}",
        action,
        truncate_to(message, 240)
    )))
}

/// Recent persisted coordination notes in ascending order.
async fn recent_coordination_notes(
    engine: &Engine,
    session_id: &str,
    limit: usize,
) -> Result<Vec<vp_protocol::types::OrchestrationNote>> {
    engine
        .coordination_notes(session_id, limit.clamp(1, 2_000) as u32)
        .await
}

fn format_coordination_note(note: &vp_protocol::types::OrchestrationNote) -> String {
    let task = note
        .task_id
        .as_deref()
        .map(|task| format!(" · {task}"))
        .unwrap_or_default();
    let status = if note.status == "resolved" {
        " (resolved)"
    } else {
        ""
    };
    format!(
        "[{}] {}{} · {}{} · {}",
        note.severity,
        note.action,
        status,
        note.agent_name,
        task,
        truncate_to(&note.message, 300)
    )
}

async fn run_team_status(
    engine: &Engine,
    session: &Session,
    input: serde_json::Value,
) -> Result<ToolOutput> {
    let note_limit = input
        .get("limit")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(8)
        .clamp(1, 50) as usize;
    let plan = engine.get_team_plan(&session.id).await?;
    let agents = engine.list_agents(&session.id).await?;
    let queue = engine.task_queue(None, Some(session.id.clone())).await?;
    let merge = engine.list_merge_queue(&session.id).await?;
    let questions = engine
        .list_questions(Some(session.id.clone()))
        .await?
        .questions;
    let recent = engine.recent_events(&session.id, 1_000).await?;

    let plan_tasks = plan
        .as_ref()
        .map(|plan| plan.tasks.as_slice())
        .unwrap_or(&[]);
    let scheduled = queue
        .tasks
        .iter()
        .map(|task| (task.task_id.clone(), task))
        .collect::<HashMap<_, _>>();
    let mut statuses = plan_tasks
        .iter()
        .map(|task| (task.id.clone(), "pending".to_string()))
        .collect::<HashMap<_, _>>();
    for event in &recent {
        if let Some(task_id) = event
            .payload
            .get("task_id")
            .and_then(serde_json::Value::as_str)
            && event.kind.starts_with("plan_task.")
            && let Some(status) = event
                .payload
                .get("task_status")
                .and_then(serde_json::Value::as_str)
        {
            statuses.insert(task_id.to_string(), status.to_string());
        }
    }
    for task in &queue.tasks {
        statuses.insert(task.task_id.clone(), task.status.clone());
    }
    let completed = plan_tasks
        .iter()
        .filter(|task| {
            matches!(
                statuses.get(&task.id).map(String::as_str),
                Some("completed" | "skipped")
            )
        })
        .count();
    let active = plan_tasks
        .iter()
        .filter(|task| {
            matches!(
                statuses.get(&task.id).map(String::as_str),
                Some("running" | "queued" | "paused" | "pausing" | "cancelling")
            )
        })
        .count();
    let ready = plan_tasks
        .iter()
        .filter(|task| {
            matches!(
                statuses.get(&task.id).map(String::as_str),
                Some("pending" | "interrupted" | "failed" | "cancelled")
            ) && task.deps.iter().all(|dep| {
                matches!(
                    statuses.get(dep).map(String::as_str),
                    Some("completed" | "skipped")
                )
            })
        })
        .collect::<Vec<_>>();
    let failed_tasks = plan_tasks
        .iter()
        .filter(|task| {
            matches!(
                statuses.get(&task.id).map(String::as_str),
                Some("failed" | "blocked")
            )
        })
        .collect::<Vec<_>>();
    let role_count = |role: &str| agents.iter().filter(|agent| agent.role == role).count();
    let running = agents
        .iter()
        .filter(|agent| agent.status == "running")
        .count();
    let conflicts = merge
        .iter()
        .filter(|entry| entry.status == "conflict")
        .count();
    let pending_questions = questions
        .iter()
        .filter(|question| question.status == "pending")
        .count();
    let notes = engine
        .coordination_notes(&session.id, note_limit as u32)
        .await?;

    let mut lines = Vec::new();
    lines.push(format!(
        "Team status · session {} · {}",
        session.status, session.goal
    ));
    lines.push(format!(
        "Agents: {} leader · {} managers · {} workers ({} running) · {} verifiers · {} labour",
        role_count("leader"),
        role_count("manager"),
        role_count("worker"),
        running,
        role_count("verifier"),
        role_count("labour")
    ));
    if plan.is_some() {
        lines.push(format!(
            "Tasks: {completed}/{} complete · {active} active · {} failed/blocked · {} ready",
            plan_tasks.len(),
            failed_tasks.len(),
            ready.len()
        ));
        if !ready.is_empty() {
            lines.push(format!(
                "Ready: {}",
                ready
                    .iter()
                    .take(8)
                    .map(|task| format!("{} ({})", task.id, task.department))
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
        if !failed_tasks.is_empty() {
            lines.push("Blocked/failed tasks:".to_string());
            for task in failed_tasks.iter().take(8) {
                let detail = scheduled
                    .get(&task.id)
                    .and_then(|task| task.last_error.as_deref())
                    .unwrap_or("no detail");
                lines.push(format!(
                    "- {} [{}] {}",
                    task.id,
                    statuses
                        .get(&task.id)
                        .map(String::as_str)
                        .unwrap_or("unknown"),
                    truncate_to(detail, 180)
                ));
            }
        }
    } else {
        lines.push("No TeamPlan submitted.".to_string());
    }
    if !merge.is_empty() {
        lines.push(format!(
            "Merge queue: {} entries · {} conflicts",
            merge.len(),
            conflicts
        ));
    }
    if pending_questions > 0 {
        lines.push(format!("User questions waiting: {pending_questions}"));
    }
    if !notes.is_empty() {
        lines.push("Recent coordination:".to_string());
        for note in notes.iter().rev() {
            lines.push(format!("- {}", format_coordination_note(note)));
        }
    }
    Ok(ToolOutput::ok(lines.join("\n")))
}

async fn run_budget_status(
    engine: &Engine,
    session: &Session,
    project: &Project,
    spec: &AgentSpec,
    budget: &SessionBudget,
    plan_budget: &PlanBudgetScope,
) -> Result<ToolOutput> {
    let unlimited = budget.is_unlimited();
    let spent_tokens = budget.spent();
    let spent_cost = budget.cost_spent();
    let (task_tokens, task_cost, department_tokens, department_cost) = engine
        .usage_spent(
            &session.id,
            spec.task_id.as_deref(),
            spec.department.as_deref(),
        )
        .await?;
    let project_budget = engine.get_project_budget(&project.id).await?;
    let session_value = json!({
        "limit_tokens": budget.limit,
        "spent_tokens": spent_tokens,
        "remaining_tokens": budget.limit.map(|limit| limit.saturating_sub(spent_tokens)),
        "limit_cost_usd": budget.cost_limit,
        "spent_cost_usd": spent_cost,
        "reserved_cost_usd": budget.reserved_cost(),
        "remaining_cost_usd": budget
            .cost_limit
            .map(|limit| (limit - spent_cost - budget.reserved_cost()).max(0.0)),
        "time_limit_secs": budget.time_limit.map(|limit| limit.as_secs()),
        "elapsed_secs": budget.elapsed_secs(),
        "remaining_time_secs": budget
            .time_limit
            .map(|limit| limit.saturating_sub(Duration::from_secs(budget.elapsed_secs())).as_secs()),
        "unlimited": unlimited,
    });
    let task_value = spec.task_id.as_ref().map(|task_id| {
        json!({
            "task_id": task_id,
            "limit_tokens": plan_budget.task_tokens,
            "spent_tokens": task_tokens,
            "remaining_tokens": plan_budget.task_tokens.map(|limit| limit.saturating_sub(task_tokens)),
            "limit_cost_usd": plan_budget.task_cost,
            "spent_cost_usd": task_cost,
            "remaining_cost_usd": plan_budget.task_cost.map(|limit| (limit - task_cost).max(0.0)),
        })
    });
    let department_value = spec.department.as_ref().map(|department| {
        json!({
            "department": department,
            "limit_tokens": plan_budget.department_tokens,
            "spent_tokens": department_tokens,
            "remaining_tokens": plan_budget.department_tokens.map(|limit| limit.saturating_sub(department_tokens)),
            "limit_cost_usd": plan_budget.department_cost,
            "spent_cost_usd": department_cost,
            "remaining_cost_usd": plan_budget.department_cost.map(|limit| (limit - department_cost).max(0.0)),
        })
    });
    let project_value = json!({
        "project_id": project.id,
        "limit_tokens": project_budget.budget.token_budget,
        "spent_tokens": project_budget.spent_tokens,
        "remaining_tokens": project_budget.budget.token_budget.map(|limit| limit.saturating_sub(project_budget.spent_tokens)),
        "limit_cost_usd": project_budget.budget.cost_budget_usd,
        "spent_cost_usd": project_budget.spent_cost_usd,
        "remaining_cost_usd": project_budget.budget.cost_budget_usd.map(|limit| (limit - project_budget.spent_cost_usd).max(0.0)),
    });
    let text = format!(
        "session tokens {}/{} · USD ${:.4}/{}{} · elapsed {}s{}\ntask tokens {}{} · department tokens {}{} · project tokens {}/{}",
        spent_tokens,
        budget
            .limit
            .map(|limit| limit.to_string())
            .unwrap_or_else(|| "unlimited".to_string()),
        spent_cost,
        budget
            .cost_limit
            .map(|limit| format!("{limit:.4}"))
            .unwrap_or_else(|| "unlimited".to_string()),
        if unlimited { " · unlimited mode" } else { "" },
        budget.elapsed_secs(),
        budget
            .time_limit
            .map(|limit| format!("/{}", limit.as_secs()))
            .unwrap_or_default(),
        spec.task_id.as_deref().unwrap_or("none"),
        plan_budget
            .task_tokens
            .map(|limit| format!("/{limit}"))
            .unwrap_or_default(),
        spec.department.as_deref().unwrap_or("none"),
        plan_budget
            .department_tokens
            .map(|limit| format!("/{limit}"))
            .unwrap_or_default(),
        project_budget.spent_tokens,
        project_budget
            .budget
            .token_budget
            .map(|limit| limit.to_string())
            .unwrap_or_else(|| "unlimited".to_string()),
    );
    Ok(ToolOutput::ok_structured(
        truncate(&text),
        json!({
            "session": session_value,
            "task": task_value,
            "department": department_value,
            "project": project_value,
            "unlimited": unlimited,
        }),
    ))
}

async fn run_task_detail(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: Value,
) -> Result<ToolOutput> {
    let task_id = input
        .get("task_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| spec.task_id.clone())
        .ok_or_else(|| anyhow!("task_id is required outside a plan task"))?;
    let plan = engine
        .get_team_plan(&session.id)
        .await?
        .ok_or_else(|| anyhow!("session has no stored TeamPlan"))?;
    let plan_task = plan
        .tasks
        .iter()
        .find(|task| task.id == task_id)
        .ok_or_else(|| anyhow!("task {task_id} is not in the stored TeamPlan"))?;
    let queue = engine.task_queue(None, Some(session.id.clone())).await?;
    let scheduled = queue.tasks.iter().find(|task| task.task_id == task_id);
    let progress = queue.progress.iter().find(|task| task.task_id == task_id);
    let threshold_secs = crate::task_stall_threshold_secs();
    let observed_at = progress
        .and_then(|progress| progress.last_progress_at.as_deref())
        .or_else(|| scheduled.and_then(|task| task.started_at.as_deref()))
        .or_else(|| scheduled.map(|task| task.enqueued_at.as_str()));
    let stalled = threshold_secs > 0
        && observed_at
            .and_then(|value| {
                time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .is_some_and(|observed| {
                time::OffsetDateTime::now_utc() - observed
                    >= time::Duration::seconds(threshold_secs.min(i64::MAX as u64) as i64)
            });
    let (task_tokens, task_cost, _, _) = engine
        .usage_spent(&session.id, Some(&task_id), Some(&plan_task.department))
        .await?;
    let changed_paths = crate::tooling::project::git_changed_paths(&spec.worktree)
        .into_iter()
        .take(100)
        .collect::<Vec<_>>();
    let structured = json!({
        "task_id": task_id,
        "department": plan_task.department,
        "title": plan_task.title,
        "kind": plan_task.kind,
        "priority": plan_task.priority,
        "profile": plan_task.profile,
        "deps": plan_task.deps,
        "write_scope": plan_task.write_scope,
        "acceptance": plan_task.acceptance,
        "token_budget": plan_task.token_budget,
        "cost_budget_usd": plan_task.cost_budget_usd,
        "spent_tokens": task_tokens,
        "spent_cost_usd": task_cost,
        "status": scheduled.map(|task| task.status.clone()),
        "attempts": scheduled.map(|task| task.attempts),
        "last_error": scheduled.and_then(|task| task.last_error.clone()),
        "next_attempt_at": scheduled.and_then(|task| task.next_attempt_at.clone()),
        "started_at": scheduled.and_then(|task| task.started_at.clone()),
        "finished_at": scheduled.and_then(|task| task.finished_at.clone()),
        "last_progress_at": progress.and_then(|progress| progress.last_progress_at.clone()),
        "stall_count": progress.map(|progress| progress.stall_count).unwrap_or(0),
        "last_turn": progress.and_then(|progress| progress.last_turn),
        "last_tool": progress.and_then(|progress| progress.last_tool.clone()),
        "stall_threshold_secs": threshold_secs,
        "stalled": stalled,
        "changed_paths": changed_paths,
        "current_agent_task": spec.task_id.as_deref() == Some(task_id.as_str()),
    });
    let text = format!(
        "{} · {} · {} · {} attempt(s) · tokens {}{} · changed {} file(s)",
        plan_task.title,
        plan_task.department,
        scheduled
            .map(|task| task.status.as_str())
            .unwrap_or("not queued"),
        scheduled.map(|task| task.attempts).unwrap_or(0),
        task_tokens,
        plan_task
            .token_budget
            .map(|limit| format!("/{limit}"))
            .unwrap_or_default(),
        changed_paths.len()
    );
    Ok(ToolOutput::ok_structured(truncate(&text), structured))
}

async fn run_task_control(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: Value,
) -> Result<ToolOutput> {
    if !matches!(spec.role, AgentRole::Leader | AgentRole::Manager) {
        return Ok(ToolOutput::error_structured(
            "only Leader or Manager may control plan tasks",
            json!({"code": "PERMISSION_DENIED"}),
        ));
    }
    let action = input
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("action is required"))?;
    let task_id = input
        .get("task_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("task_id is required"))?;
    let plan = engine
        .get_team_plan(&session.id)
        .await?
        .ok_or_else(|| anyhow!("session has no stored TeamPlan"))?;
    if !plan.tasks.iter().any(|task| task.id == task_id) {
        return Ok(ToolOutput::error_structured(
            format!("task {task_id} is not in the stored TeamPlan"),
            json!({"code": "TASK_NOT_FOUND", "task_id": task_id}),
        ));
    }
    let result = match action {
        "retry" => engine.retry_plan_task(&session.id, task_id).await,
        "skip" => engine.skip_plan_task(&session.id, task_id).await,
        "cancel" => engine.request_plan_task_cancel(&session.id, task_id).await,
        "pause" => engine.pause_plan_task(&session.id, task_id).await,
        "resume" => engine.resume_plan_task(&session.id, task_id).await,
        other => {
            return Ok(ToolOutput::error_structured(
                format!("unsupported task action: {other}"),
                json!({"code": "INVALID_ARGUMENT", "action": other}),
            ));
        }
    };
    match result {
        Ok(task) => {
            let structured = serde_json::to_value(&task).unwrap_or_else(|_| json!({}));
            Ok(ToolOutput::ok_structured(
                format!("{action} applied to {task_id} (status {})", task.status),
                json!({
                    "action": action,
                    "task": structured,
                }),
            ))
        }
        Err(error) => Ok(ToolOutput::error_structured(
            format!("{action} failed for {task_id}: {error}"),
            json!({"code": "TASK_CONTROL_FAILED", "action": action, "task_id": task_id}),
        )),
    }
}

async fn run_tool_search(
    config: &AgentConfig,
    engine: &Engine,
    spec: &AgentSpec,
    input: Value,
) -> Result<ToolOutput> {
    let query = input
        .get("query")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let limit = input
        .get("limit")
        .and_then(Value::as_u64)
        .unwrap_or(20)
        .clamp(1, 50) as usize;
    let include_schema = input
        .get("include_schema")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let mut results = Vec::new();
    // Mirror the exact tool bundle advertised to this role, including
    // special-cased tools such as `browser` that are not in the registry.
    // `tool_search` itself is omitted so a generic query does not return the
    // discovery tool as its own result.
    for tool in tool_schemas(config, spec.role, spec.profile, spec.allow_delegation)
        .into_iter()
        .filter(|tool| tool.name != "tool_search")
    {
        let score = tool_match_score(&tool.name, &tool.description, &query);
        if score <= 0.0 {
            continue;
        }
        let mut value = json!({
            "name": tool.name,
            "kind": "builtin",
            "description": tool.description.chars().take(400).collect::<String>(),
            "callable": true,
            "score": score,
        });
        if include_schema {
            value["parameters_preview"] = json!(bounded_schema(&tool.parameters));
        }
        results.push(value);
    }
    let mcp_callable = matches!(
        spec.role,
        AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
    );
    let servers = engine.list_mcp_servers().await?.servers;
    for server in servers.into_iter().filter(|server| server.enabled).take(20) {
        let Ok(tools) = engine.mcp_tools(&server.id).await else {
            continue;
        };
        for tool in tools.tools.into_iter().take(100) {
            let exposed = dynamic_mcp_tool_name(&server.name, &tool.name);
            let score = tool_match_score(&exposed, &tool.description, &query);
            if score <= 0.0 {
                continue;
            }
            let mut value = json!({
                "name": exposed,
                "kind": "mcp",
                "server_id": server.id,
                "server_name": server.name,
                "tool_name": tool.name,
                "description": tool.description.chars().take(400).collect::<String>(),
                "callable": mcp_callable,
                "score": score,
            });
            if include_schema {
                value["parameters_preview"] = json!(bounded_schema(&tool.input_schema));
            }
            results.push(value);
        }
    }
    results.sort_by(|left, right| {
        right["score"]
            .as_f64()
            .partial_cmp(&left["score"].as_f64())
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left["kind"].as_str().cmp(&right["kind"].as_str()))
            .then_with(|| left["name"].as_str().cmp(&right["name"].as_str()))
    });
    results.truncate(limit);
    let rendered = results
        .iter()
        .map(|result| {
            format!(
                "{} · {}{} · {}",
                result["name"].as_str().unwrap_or("?"),
                result["kind"].as_str().unwrap_or("tool"),
                if result["kind"] == "mcp" {
                    result["server_id"]
                        .as_str()
                        .map(|server| format!(" ({server})"))
                        .unwrap_or_default()
                } else {
                    String::new()
                },
                result["description"].as_str().unwrap_or("")
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let mcp_hint = if results.iter().any(|result| result["kind"] == "mcp") {
        if mcp_callable {
            "\nCall an MCP result with mcp action=call server=<server_id> tool=<tool_name> arguments={...}."
        } else {
            "\nThis role can inspect MCP tools but cannot call them."
        }
    } else {
        ""
    };
    Ok(ToolOutput::ok_structured(
        truncate(&format!("{rendered}{mcp_hint}")),
        json!({
            "query": query,
            "tools": results,
            "count": results.len(),
            "mcp_callable": mcp_callable,
        }),
    ))
}

fn tool_match_score(name: &str, description: &str, query: &str) -> f64 {
    if query.trim().is_empty() {
        return 1.0;
    }
    let name = name.to_ascii_lowercase();
    let description = description.to_ascii_lowercase();
    let query = query.to_ascii_lowercase();
    let tokens = query
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| token.len() > 1)
        .collect::<Vec<_>>();
    if name == query {
        return 1.0;
    }
    if name.contains(&query) {
        return 0.9;
    }
    if !tokens.is_empty() && tokens.iter().all(|token| name.contains(token)) {
        return 0.8;
    }
    if !tokens.is_empty() && tokens.iter().all(|token| description.contains(token)) {
        return 0.5;
    }
    0.0
}

fn bounded_schema(value: &Value) -> String {
    serde_json::to_string(value)
        .unwrap_or_default()
        .chars()
        .take(800)
        .collect()
}

async fn run_publish_artifact_file(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    input: Value,
) -> Result<ToolOutput> {
    let relative = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("path is required"))?;
    let absolute = crate::tools::safe_existing_path(&spec.worktree, relative)?;
    if !absolute.is_file() {
        return Ok(ToolOutput::error_structured(
            "artifact source must be a file",
            json!({"code": "INVALID_ARGUMENT", "path": relative}),
        ));
    }
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            absolute
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
        })
        .unwrap_or_else(|| "artifact".to_string());
    let kind = input.get("kind").and_then(Value::as_str).unwrap_or("file");
    let mime_type = input
        .get("mime_type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| crate::tooling::files::mime_for_path(&absolute).to_string());
    let artifact = engine
        .publish_artifact_file(
            &session.id,
            &spec.id,
            &spec.name,
            spec.task_id.clone(),
            &name,
            kind,
            &mime_type,
            &absolute,
            &spec.worktree,
        )
        .await?;
    let structured = serde_json::to_value(&artifact).unwrap_or_else(|_| json!({}));
    Ok(ToolOutput::ok_structured(
        format!(
            "published {} ({}) as {} · {} bytes",
            name, mime_type, artifact.id, artifact.size_bytes
        ),
        structured,
    ))
}

async fn run_mcp_tool(
    engine: &Engine,
    role: AgentRole,
    input: serde_json::Value,
) -> Result<ToolOutput> {
    let action = input
        .get("action")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("action is required"))?;
    if action == "call"
        && !matches!(
            role,
            AgentRole::Leader | AgentRole::Worker | AgentRole::Labour
        )
    {
        return Ok(ToolOutput::error(
            "this role may inspect MCP servers but cannot execute MCP tools",
        ));
    }
    let servers = engine.list_mcp_servers().await?.servers;
    let server = input
        .get("server")
        .and_then(serde_json::Value::as_str)
        .map(|selector| {
            servers
                .iter()
                .find(|server| server.id == selector || server.name == selector)
                .cloned()
                .ok_or_else(|| anyhow!("MCP server not found: {selector}"))
        })
        .transpose()?;
    match action {
        "list" => {
            if let Some(server) = server {
                let tools = engine.mcp_tools(&server.id).await?.tools;
                let mut text = format!(
                    "MCP server {} ({}):
",
                    server.name, server.id
                );
                for tool in tools {
                    text.push_str(&format!(
                        "- {}: {}
",
                        tool.name,
                        truncate_to(&tool.description, 300)
                    ));
                }
                if text.ends_with(":\n") {
                    text.push_str("(no tools)");
                }
                Ok(ToolOutput::ok(text))
            } else {
                let mut text = String::from(
                    "MCP servers:
",
                );
                for server in servers {
                    text.push_str(&format!(
                        "- {} ({}) [{}] {}
",
                        server.name,
                        server.id,
                        if server.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        },
                        server.command
                    ));
                }
                Ok(ToolOutput::ok(text))
            }
        }
        "call" => {
            let server = server.ok_or_else(|| anyhow!("server is required for action=call"))?;
            let tool = input
                .get("tool")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("tool is required for action=call"))?;
            let arguments = input
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let result = engine.mcp_call(&server.id, tool, arguments).await?;
            Ok(ToolOutput::ok(truncate_to(
                &serde_json::to_string_pretty(&result.result)?,
                16_000,
            )))
        }
        "resources" => {
            let server =
                server.ok_or_else(|| anyhow!("server is required for action=resources"))?;
            let result = engine.mcp_resources(&server.id).await?;
            let mut text = format!("MCP resources on {}:\n", server.name);
            for resource in &result.resources {
                text.push_str(&format!("- {} <{}>", resource.name, resource.uri));
                if let Some(mime_type) = &resource.mime_type {
                    text.push_str(&format!(" [{mime_type}]"));
                }
                if !resource.description.is_empty() {
                    text.push_str(&format!(": {}", truncate_to(&resource.description, 200)));
                }
                text.push('\n');
            }
            for template in &result.templates {
                text.push_str(&format!(
                    "- template {} <{}>: {}\n",
                    template.name,
                    template.uri_template,
                    truncate_to(&template.description, 200)
                ));
            }
            if result.resources.is_empty() && result.templates.is_empty() {
                text.push_str("(no resources)");
            }
            Ok(ToolOutput::ok(text))
        }
        "read_resource" => {
            let server =
                server.ok_or_else(|| anyhow!("server is required for action=read_resource"))?;
            let uri = input
                .get("uri")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("uri is required for action=read_resource"))?;
            let result = engine.mcp_read_resource(&server.id, uri).await?;
            let mut text = String::new();
            for content in result.contents {
                if let Some(body) = content.text {
                    text.push_str(&body);
                    text.push('\n');
                } else if let Some(blob) = content.blob {
                    text.push_str(&format!(
                        "[binary resource {}: {} base64 chars{}]\n",
                        content.uri,
                        blob.len(),
                        content
                            .mime_type
                            .map(|mime| format!(", {mime}"))
                            .unwrap_or_default()
                    ));
                }
            }
            if text.is_empty() {
                text.push_str("(empty resource)");
            }
            Ok(ToolOutput::ok(truncate_to(&text, 16_000)))
        }
        "prompts" => {
            let server = server.ok_or_else(|| anyhow!("server is required for action=prompts"))?;
            let result = engine.mcp_prompts(&server.id).await?;
            let mut text = format!("MCP prompts on {}:\n", server.name);
            for prompt in result.prompts {
                text.push_str(&format!(
                    "- {}: {}",
                    prompt.name,
                    truncate_to(&prompt.description, 200)
                ));
                if !prompt.arguments.is_empty() {
                    let arguments = prompt
                        .arguments
                        .iter()
                        .map(|argument| {
                            if argument.required {
                                format!("{}=?", argument.name)
                            } else {
                                format!("{}?", argument.name)
                            }
                        })
                        .collect::<Vec<_>>()
                        .join(", ");
                    text.push_str(&format!(" ({arguments})"));
                }
                text.push('\n');
            }
            if text.ends_with(":\n") {
                text.push_str("(no prompts)");
            }
            Ok(ToolOutput::ok(text))
        }
        "get_prompt" => {
            let server =
                server.ok_or_else(|| anyhow!("server is required for action=get_prompt"))?;
            let prompt = input
                .get("prompt")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("prompt is required for action=get_prompt"))?;
            let arguments = input
                .get("prompt_arguments")
                .and_then(serde_json::Value::as_object)
                .map(|arguments| {
                    arguments
                        .iter()
                        .filter_map(|(key, value)| Some((key.clone(), value.as_str()?.to_string())))
                        .collect()
                })
                .unwrap_or_default();
            let result = engine.mcp_prompt(&server.id, prompt, arguments).await?;
            let mut text = String::new();
            if !result.description.is_empty() {
                text.push_str(&result.description);
                text.push_str("\n\n");
            }
            for message in result.messages {
                text.push_str(&format!("[{}] ", message.role));
                text.push_str(&truncate_to(
                    &serde_json::to_string(&message.content)?,
                    4_000,
                ));
                text.push('\n');
            }
            Ok(ToolOutput::ok(truncate_to(&text, 16_000)))
        }
        other => Ok(ToolOutput::error(format!("unknown MCP action: {other}"))),
    }
}

fn skill_allows_tool(skill: &Skill, tool: &str) -> bool {
    if tool == "use_skill" {
        return true;
    }
    if skill
        .tool_deny
        .iter()
        .any(|denied| denied == "*" || denied == tool)
    {
        return false;
    }
    skill.tool_allow.is_empty()
        || skill
            .tool_allow
            .iter()
            .any(|allowed| allowed == "*" || allowed == tool)
}

async fn run_skill_tool(
    engine: &Engine,
    project: &Project,
    spec: &AgentSpec,
    input: Value,
    active: &mut Option<Skill>,
) -> Result<ToolOutput> {
    let name = input
        .get("name")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| anyhow!("name is required"))?;
    let skills = engine
        .skills_for(&project.id, spec.role.as_str(), spec.profile.as_str())
        .await?;
    let Some(skill) = skills.iter().find(|skill| {
        skill.name.eq_ignore_ascii_case(name) || skill.slug.eq_ignore_ascii_case(name)
    }) else {
        let available = skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Ok(ToolOutput::error(if available.is_empty() {
            "No enabled skills apply to this role/profile.".to_string()
        } else {
            format!("Unknown skill '{name}'. Available skills: {available}")
        }));
    };
    *active = Some(skill.clone());
    let allow = if skill.tool_allow.is_empty() {
        "unrestricted by this skill".to_string()
    } else {
        skill.tool_allow.join(", ")
    };
    let deny = if skill.tool_deny.is_empty() {
        "none".to_string()
    } else {
        skill.tool_deny.join(", ")
    };
    Ok(ToolOutput::ok(truncate_to(
        &format!(
            "Skill '{}' v{} activated.\nAllowed tools: {allow}\nDenied tools: {deny}\n\n{}\n\nFollow these instructions for the rest of this run. Use_skill always remains available to switch skills.",
            skill.name, skill.version, skill.instructions
        ),
        22_000,
    )))
}

async fn run_source_tool(
    engine: &Engine,
    project: &Project,
    name: &str,
    input: Value,
    citations: &mut CitationRegistry,
) -> Result<ToolOutput> {
    match name {
        "search_sources" => {
            let query = input
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("query is required"))?;
            let source_ids = input
                .get("source_ids")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .take(20)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let collections = input
                .get("collections")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .take(20)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let labels = input
                .get("labels")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .take(20)
                        .map(ToOwned::to_owned)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(6)
                .clamp(1, 20) as u32;
            let result = engine
                .search_sources_scoped(
                    project.id.clone(),
                    query.to_string(),
                    source_ids,
                    collections,
                    labels,
                    Some(limit),
                )
                .await?;
            if result.hits.is_empty() {
                return Ok(ToolOutput::ok(
                    "No matching source chunks. Try a shorter query or list_sources.",
                ));
            }
            let mut output = String::new();
            for hit in &result.hits {
                let key = citations.add_hit(hit, "search");
                let heading = hit.heading.as_deref().unwrap_or("untitled section");
                output.push_str(&format!(
                    "[{key}] {} — {} (chunk {}, score {:.2})\n{}\n\n",
                    hit.source_title,
                    heading,
                    hit.chunk_no,
                    hit.score,
                    truncate_to(&hit.content, 1_400),
                ));
            }
            if result.degraded {
                output.push_str(
                    "Search used the lexical fallback because the FTS index had no direct match.\n",
                );
            }
            Ok(ToolOutput::ok(truncate_to(&output, 24_000)))
        }
        "read_source" => {
            let source_id = input
                .get("source_id")
                .and_then(Value::as_str)
                .ok_or_else(|| anyhow!("source_id is required"))?;
            let chunk_no = input
                .get("chunk_no")
                .and_then(Value::as_u64)
                .unwrap_or(1)
                .clamp(1, 500) as u32;
            let max_chars = input
                .get("max_chars")
                .and_then(Value::as_u64)
                .unwrap_or(6_000)
                .clamp(300, 20_000) as usize;
            let result = engine
                .get_source(source_id.to_string(), Some(chunk_no - 1), Some(1))
                .await?;
            let Some(chunk) = result.chunks.first() else {
                return Ok(ToolOutput::error(format!(
                    "source {source_id} has no chunk {chunk_no}"
                )));
            };
            let hit = SourceSearchHit {
                source_id: result.source.id.clone(),
                source_title: result.source.title.clone(),
                collection: result.source.collection.clone(),
                labels: result.source.labels.clone(),
                chunk_id: chunk.id.clone(),
                chunk_no: chunk.chunk_no,
                heading: chunk.heading.clone(),
                content: chunk.content.clone(),
                score: 1.0,
            };
            let key = citations.add_hit(&hit, "read");
            let heading = chunk.heading.as_deref().unwrap_or("untitled section");
            Ok(ToolOutput::ok(format!(
                "[{key}] {} — {} (chunk {} of {})\n{}",
                result.source.title,
                heading,
                chunk.chunk_no,
                result.total_chunks,
                truncate_to(&chunk.content, max_chars),
            )))
        }
        "list_sources" => {
            let limit = input
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(20)
                .clamp(1, 50) as u32;
            let result = engine
                .list_sources(project.id.clone(), None, None, Some(limit))
                .await?;
            if result.sources.is_empty() {
                return Ok(ToolOutput::ok("No project sources are indexed."));
            }
            let mut output = String::new();
            for source in result.sources {
                output.push_str(&format!(
                    "- {} · {} · {} chunks · status={}\n  id={}\n",
                    source.title, source.kind, source.chunk_count, source.status, source.id
                ));
            }
            Ok(ToolOutput::ok(truncate_to(&output, 12_000)))
        }
        other => Ok(ToolOutput::error(format!("unknown source tool: {other}"))),
    }
}

async fn run_memory_tool(
    engine: &Engine,
    session: &Session,
    project: &Project,
    spec: &AgentSpec,
    name: &str,
    input: serde_json::Value,
) -> Result<ToolOutput> {
    match name {
        "remember" => {
            if spec.role == AgentRole::Verifier {
                return Ok(ToolOutput::error("Verifier cannot write project memory"));
            }
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("kind is required"))?;
            let title = input
                .get("title")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("title is required"))?;
            let body = input
                .get("body")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("body is required"))?;
            let tags = input
                .get("tags")
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(serde_json::Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>();
            let memory = engine
                .create_memory_as(&session.id, &spec.id, &spec.name, kind, title, body, tags)
                .await?;
            Ok(ToolOutput::ok(format!(
                "remembered [{}] {} ({})",
                memory.kind, memory.title, memory.id
            )))
        }
        "recall" => {
            let query = input
                .get("query")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let limit = input
                .get("limit")
                .and_then(serde_json::Value::as_u64)
                .unwrap_or(10)
                .clamp(1, 20) as u32;
            let memories = engine
                .list_memories(&project.id, query, kind, limit)
                .await?
                .memories;
            if memories.is_empty() {
                return Ok(ToolOutput::ok("No matching project memories."));
            }
            let mut text = String::new();
            for memory in memories {
                text.push_str(&format!(
                    "[{}] {} · {} · {}
{}
",
                    memory.kind,
                    memory.title,
                    memory.agent_name,
                    truncate_to(&memory.body, 1200),
                    memory.id
                ));
            }
            Ok(ToolOutput::ok(text))
        }
        other => Ok(ToolOutput::error(format!("unknown memory tool: {other}"))),
    }
}

async fn run_mailbox_tool(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    name: &str,
    input: serde_json::Value,
) -> Result<ToolOutput> {
    match name {
        "send_message" => {
            let to = input
                .get("to")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("to is required"))?;
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("kind is required"))?;
            let body = input
                .get("body")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("body is required"))?;
            let payload = input
                .get("payload")
                .cloned()
                .unwrap_or_else(|| serde_json::json!({}));
            let agents = engine.list_agents(&session.id).await?;
            let target = agents
                .iter()
                .find(|agent| {
                    agent.id == to || agent.name == to || agent.name.eq_ignore_ascii_case(to)
                })
                .ok_or_else(|| {
                    anyhow!(
                        "agent not found in this session: {to}. Address another agent by id/name; answer the human directly instead of sending to 'user'."
                    )
                })?;
            let message = engine
                .send_agent_message(
                    &session.id,
                    &spec.id,
                    &spec.name,
                    &target.id,
                    &target.name,
                    kind,
                    body,
                    payload,
                )
                .await?;
            Ok(ToolOutput::ok(format!(
                "message {} sent to {} ({})",
                message.id, message.to_name, message.kind
            )))
        }
        "read_mailbox" => {
            let unread_only = input
                .get("unread_only")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(true);
            let messages = engine
                .list_agent_messages(&session.id, Some(spec.id.clone()), unread_only)
                .await?;
            let mut text = String::new();
            for message in &messages {
                if message.status == "unread" {
                    let _ = engine.mark_agent_message_read(&message.id).await;
                }
                text.push_str(&format!(
                    "[{}] {} · {} · {}
{}
",
                    message.id, message.from_name, message.kind, message.created_at, message.body
                ));
            }
            if text.is_empty() {
                text.push_str("No messages.");
            }
            Ok(ToolOutput::ok(text))
        }
        "publish_artifact" => {
            let name = input
                .get("name")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("name is required"))?;
            let content = input
                .get("content")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("content is required"))?;
            let kind = input
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("note");
            let mime_type = input
                .get("mime_type")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("text/plain");
            let requested_key = input
                .get("request_key")
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if requested_key.is_some()
                && input
                    .get("artifact_id")
                    .and_then(serde_json::Value::as_str)
                    .is_some()
            {
                return Ok(ToolOutput::error(
                    "publish_artifact cannot update an artifact and use a request_key at the same time",
                ));
            }
            let artifact = if let Some(artifact_id) =
                input.get("artifact_id").and_then(serde_json::Value::as_str)
            {
                let expected = input
                    .get("expected_version")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| u32::try_from(value).ok());
                engine
                    .publish_artifact_version(artifact_id, expected, name, kind, mime_type, content)
                    .await?
            } else if let Some(request_key) = requested_key {
                engine
                    .publish_artifact_keyed(
                        &session.id,
                        &spec.id,
                        &spec.name,
                        spec.task_id.clone(),
                        name,
                        kind,
                        mime_type,
                        content,
                        request_key,
                    )
                    .await?
            } else {
                engine
                    .publish_artifact(
                        &session.id,
                        &spec.id,
                        &spec.name,
                        spec.task_id.clone(),
                        name,
                        kind,
                        mime_type,
                        content,
                    )
                    .await?
            };
            Ok(ToolOutput::ok(format!(
                "artifact {} version {} published: {}",
                artifact.id, artifact.version, artifact.name
            )))
        }
        "list_artifacts" => {
            let artifacts = engine.list_artifacts(&session.id).await?;
            if artifacts.is_empty() {
                return Ok(ToolOutput::ok("No artifacts."));
            }
            let text = artifacts
                .iter()
                .map(|artifact| {
                    format!(
                        "{}  {}  {}  {}  {}  {} bytes  {}",
                        artifact.id,
                        artifact.name,
                        artifact.kind,
                        artifact.storage,
                        artifact.compression,
                        artifact.size_bytes,
                        artifact.agent_name
                    )
                })
                .collect::<Vec<_>>()
                .join(
                    "
",
                );
            Ok(ToolOutput::ok(text))
        }
        "read_artifact" => {
            let id = input
                .get("artifact_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| anyhow!("artifact_id is required"))?;
            let artifact = engine
                .get_artifact(id)
                .await?
                .ok_or_else(|| anyhow!("artifact not found: {id}"))?;
            if artifact.storage == "file" {
                let materialized = engine
                    .materialize_artifact(id)
                    .await?
                    .ok_or_else(|| anyhow!("artifact {id} has no materialized file"))?;
                Ok(ToolOutput::ok(format!(
                    "{} [{}] · {} · {} · {} bytes · {}",
                    artifact.name,
                    artifact.mime_type,
                    artifact.compression,
                    artifact.storage,
                    artifact.size_bytes,
                    materialized.path
                )))
            } else {
                Ok(ToolOutput::ok(format!(
                    "{} [{}]
{}",
                    artifact.name, artifact.mime_type, artifact.content
                )))
            }
        }
        other => Err(anyhow!("unknown mailbox tool: {other}")),
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
/// The fixed 24k default is gone: a 1M-token model gets a ~75% window
/// threshold while a local 8k model gets a much lower one.
fn context_limits(config: &AgentConfig, role: AgentRole) -> (u64, u64) {
    let window = context_window_for(config, role);
    if window == 0 {
        return (config.compact_after_tokens, u64::MAX);
    }
    let output = u64::from(config.max_tokens.unwrap_or(4096));
    // `estimated` in the compaction path includes the system prompt and tool
    // schemas. The hard limit is therefore the budget for the whole request,
    // not a second deduction of those same tokens. Keep a small safety reserve
    // for provider framing and a final assistant turn.
    let safety = 1_024u64;
    let hard_limit = window
        .saturating_sub(output.saturating_add(safety))
        .max(1024);
    let threshold = if config.compact_after_tokens > 0 {
        config.compact_after_tokens.min(hard_limit)
    } else {
        hard_limit.saturating_mul(3) / 4
    }
    .max(512);
    (threshold, hard_limit)
}

fn role_price(config: &AgentConfig, role: AgentRole) -> (f64, f64) {
    let price = config.role_models.for_role(role.as_str());
    (
        price.input_usd_per_million.unwrap_or(0.0),
        price.output_usd_per_million.unwrap_or(0.0),
    )
}

fn usage_cost(input: u64, output: u64, input_price: f64, output_price: f64) -> f64 {
    (input as f64 / 1_000_000.0) * input_price + (output as f64 / 1_000_000.0) * output_price
}

/// Registry tools that only read repository state and can safely run in a
/// bounded parallel batch. `shell` and `fetch_url` are deliberately excluded:
/// shell commands can mutate state and network calls should not fan out.
fn is_parallel_safe_tool(name: &str) -> bool {
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
    )
}

fn tool_schemas(
    config: &AgentConfig,
    role: AgentRole,
    profile: AgentProfile,
    allow_delegation: bool,
) -> Vec<ToolSchema> {
    let mut tools = config
        .tools
        .schemas()
        .into_iter()
        .filter(|tool| role.allows_tool(profile, &tool.name))
        .collect::<Vec<_>>();
    tools.push(ToolSchema {
        name: "send_message".to_string(),
        description: "Send a typed message to another agent in this session. Do not address the human user; put the final answer in the normal assistant response.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "to": {"type": "string", "description": "Target agent id or unique name"},
                "kind": {"type": "string", "description": "e.g. question, handoff, result, review"},
                "body": {"type": "string"},
                "payload": {"type": "object"}
            },
            "required": ["to", "kind", "body"]
        }),
    });
    tools.push(ToolSchema {
        name: "read_mailbox".to_string(),
        description: "Read messages addressed to this agent. Marks returned unread messages read."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "unread_only": {"type": "boolean"}
            }
        }),
    });
    tools.push(ToolSchema {
        name: "publish_artifact".to_string(),
        description: "Publish a text artifact so other agents and the user can reference it."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string"},
                "kind": {"type": "string"},
                "mime_type": {"type": "string"},
                "content": {"type": "string", "maxLength": 100000},
                "artifact_id": {"type": "string", "description": "Update this inline artifact instead of creating a new one"},
                "expected_version": {"type": "integer", "minimum": 1, "description": "Optimistic concurrency guard for updates"},
                "request_key": {"type": "string", "maxLength": 160, "description": "Idempotency key scoped to this session; a repeated key returns the first artifact"}
            },
            "required": ["name", "content"]
        }),
    });
    tools.push(ToolSchema {
        name: "list_artifacts".to_string(),
        description: "List artifacts published in this session.".to_string(),
        parameters: serde_json::json!({"type": "object", "properties": {}}),
    });
    tools.push(ToolSchema {
        name: "read_artifact".to_string(),
        description: "Read one artifact by id.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {"artifact_id": {"type": "string"}},
            "required": ["artifact_id"]
        }),
    });
    tools.push(ToolSchema {
        name: "search_sources".to_string(),
        description: "Search the project's indexed knowledge sources. Returns ranked chunks with citation keys such as [c1]. Cite every used chunk with its exact key.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "maxLength": 400},
                "source_ids": {
                    "type": "array",
                    "maxItems": 20,
                    "items": {"type": "string"}
                },
                "collections": {
                    "type": "array",
                    "maxItems": 20,
                    "items": {"type": "string"},
                    "description": "Restrict to these collections; an empty string selects uncollected sources"
                },
                "labels": {
                    "type": "array",
                    "maxItems": 20,
                    "items": {"type": "string"},
                    "description": "Restrict to sources with any of these labels"
                },
                "limit": {"type": "integer", "minimum": 1, "maximum": 20}
            },
            "required": ["query"]
        }),
    });
    tools.push(ToolSchema {
        name: "read_source".to_string(),
        description: "Read one project source chunk in full. The returned citation key must be used when quoting it.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "source_id": {"type": "string"},
                "chunk_no": {"type": "integer", "minimum": 1},
                "max_chars": {"type": "integer", "minimum": 300, "maximum": 20000}
            },
            "required": ["source_id"]
        }),
    });
    tools.push(ToolSchema {
        name: "list_sources".to_string(),
        description:
            "List indexed project sources and their ids. Use search_sources to read content."
                .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 50}
            }
        }),
    });
    tools.push(ToolSchema {
        name: "use_skill".to_string(),
        description: "Activate a project skill by its exact name. Returns the full instructions and any tool constraints; calling another skill replaces the active one.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "maxLength": 120}
            },
            "required": ["name"]
        }),
    });
    tools.push(ToolSchema {
        name: "recall".to_string(),
        description: "Search project memories recorded by any session or agent.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "kind": {"type": "string"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 20}
            }
        }),
    });
    if role != AgentRole::Verifier {
        tools.push(ToolSchema {
            name: "remember".to_string(),
            description: "Record a durable project memory (decision, fact, convention, or source) for later agents.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "kind": {"type": "string"},
                    "title": {"type": "string"},
                    "body": {"type": "string"},
                    "tags": {
                        "type": "array",
                        "maxItems": 10,
                        "items": {"type": "string"}
                    }
                },
                "required": ["kind", "title", "body"]
            }),
        });
    }
    if role != AgentRole::Verifier {
        tools.push(ToolSchema {
            name: "ask_user".to_string(),
            description: "Ask the user a blocking question and wait for an answer. Use only when the task cannot proceed safely without a decision.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string", "maxLength": 2000},
                    "options": {
                        "type": "array",
                        "maxItems": 8,
                        "items": {"type": "string", "maxLength": 200}
                    }
                },
                "required": ["question"]
            }),
        });
    }
    if role != AgentRole::Verifier {
        tools.push(ToolSchema {
            name: "coordinate".to_string(),
            description: "Collaborate with the team: record a note, raise or clear a blocker, announce a handoff, escalate to the Leader, record a Manager decision, or list recent coordination notes.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["note", "blocker", "unblock", "handoff", "escalate", "decision", "list"]
                    },
                    "task_id": {"type": "string", "maxLength": 40},
                    "target_agent_id": {"type": "string"},
                    "message": {"type": "string", "maxLength": 2000},
                    "severity": {
                        "type": "string",
                        "enum": ["info", "warning", "critical"]
                    },
                    "limit": {"type": "integer", "minimum": 1, "maximum": 50}
                },
                "required": ["action"]
            }),
        });
    }
    tools.push(ToolSchema {
        name: "team_status".to_string(),
        description: "Read the current team board: agents by role, task progress, ready/blocked tasks, merge queue, and recent coordination notes. Read-only.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "limit": {"type": "integer", "minimum": 1, "maximum": 100}
            }
        }),
    });
    let mcp_actions = if matches!(role, AgentRole::Manager | AgentRole::Verifier) {
        serde_json::json!([
            "list",
            "resources",
            "read_resource",
            "prompts",
            "get_prompt"
        ])
    } else {
        serde_json::json!([
            "list",
            "call",
            "resources",
            "read_resource",
            "prompts",
            "get_prompt"
        ])
    };
    tools.push(ToolSchema {
        name: "mcp".to_string(),
        description: "Inspect configured MCP servers and use their tools, resources, or prompts. Tool executions require approval; Managers and Verifiers can only inspect.".to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": mcp_actions
                },
                "server": {"type": "string", "description": "MCP server id or name"},
                "tool": {"type": "string"},
                "arguments": {"type": "object", "description": "Tool arguments for action=call"},
                "uri": {"type": "string", "description": "Resource URI for action=read_resource"},
                "prompt": {"type": "string", "description": "Prompt name for action=get_prompt"},
                "prompt_arguments": {
                    "type": "object",
                    "additionalProperties": {"type": "string"},
                    "description": "String arguments for action=get_prompt"
                }
            },
            "required": ["action"]
        }),
    });
    if role == AgentRole::Leader {
        tools.push(ToolSchema {
            name: "todo_write".to_string(),
            description: "Replace the session plan/todo list. Use as work progresses.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "maxItems": 50,
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": {"type": "string"},
                                "status": {
                                    "type": "string",
                                    "enum": ["pending", "in_progress", "completed"]
                                }
                            },
                            "required": ["title", "status"]
                        }
                    }
                },
                "required": ["items"]
            }),
        });
    }
    if role.child_role().is_some() && allow_delegation {
        tools.push(ToolSchema {
            name: "delegate".to_string(),
            description: format!(
                "Delegate a focused objective to a {}. The child agent runs synchronously and returns a summary.",
                role.child_role()
                    .map(AgentRole::as_str)
                    .unwrap_or("worker")
            ),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "role": {
                        "type": "string",
                        "enum": ["manager", "worker", "labour"]
                    },
                    "name": {"type": "string"},
                    "title": {
                        "type": "string",
                        "description": "Required for Managers: a distinct department role such as Discovery, Implementation, or Quality"
                    },
                    "objective": {"type": "string"},
                    "instructions": {"type": "string"},
                    "profile": {
                        "type": "string",
                        "enum": ["coding", "research", "verification", "generic"]
                    },
                    "write_scope": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {"type": "string"}
                    }
                },
                "required": ["role", "objective"]
            }),
        });
    }
    if matches!(role, AgentRole::Leader | AgentRole::Manager) && allow_delegation {
        tools.push(ToolSchema {
            name: "delegate_acp".to_string(),
            description: "Delegate a focused task to a configured external Agent Client Protocol (ACP) agent. It runs in an isolated task worktree, its permission requests use the risk-based approval policy, and its committed changes are verified and merged like a Worker.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "agent": {
                        "type": "string",
                        "description": "Configured ACP agent id or name (see Settings > ACP)"
                    },
                    "name": {
                        "type": "string",
                        "description": "Optional Worker display name"
                    },
                    "objective": {"type": "string"},
                    "instructions": {"type": "string"},
                    "write_scope": {
                        "type": "array",
                        "maxItems": 20,
                        "items": {"type": "string"}
                    },
                    "timeout_secs": {
                        "type": "integer",
                        "minimum": 30,
                        "description": "Optional timeout, capped by the configured agent timeout"
                    }
                },
                "required": ["agent", "objective"]
            }),
        });
    }
    if role == AgentRole::Leader {
        tools.push(ToolSchema {
            name: "submit_team_plan".to_string(),
            description: "Define the team hierarchy and task plan before delegating Managers. Departments select a Manager title and profile; tasks form a DAG.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "goal": {"type": "string"},
                    "departments": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 32,
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "manager_title": {"type": "string"},
                                "profile": {"type": "string", "enum": ["coding", "research", "verification", "generic"]},
                                "objective": {"type": "string"},
                                "token_budget": {"type": "integer", "minimum": 1, "description": "Optional department token cap across all tasks"},
                                "cost_budget_usd": {"type": "number", "exclusiveMinimum": 0, "description": "Optional department USD cap across all tasks"}
                            },
                            "required": ["name", "manager_title", "profile", "objective"]
                        }
                    },
                    "tasks": {
                        "type": "array",
                        "maxItems": 100,
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string"},
                                "department": {"type": "string"},
                                "title": {"type": "string"},
                                "kind": {"type": "string"},
                                "priority": {"type": "integer", "minimum": -100, "maximum": 100, "description": "Higher priority runs first within a ready batch"},
                                "profile": {
                                    "type": "string",
                                    "enum": ["coding", "research", "verification", "generic"]
                                },
                                "deps": {"type": "array", "items": {"type": "string"}},
                                "write_scope": {"type": "array", "items": {"type": "string"}},
                                "acceptance": {"type": "array", "items": {"type": "string"}},
                                "token_budget": {"type": "integer", "minimum": 1, "description": "Optional task token cap"},
                                "cost_budget_usd": {"type": "number", "exclusiveMinimum": 0, "description": "Optional task USD cap"}
                            },
                            "required": ["id", "department", "title", "kind", "acceptance"]
                        }
                    }
                },
                "reason": {
                    "type": "string",
                    "maxLength": 1000,
                    "description": "Required when revising an existing plan; record why the team is changing course."
                },
                "required": ["goal", "departments", "tasks"]
            }),
        });
    }
    if role == AgentRole::Leader {
        tools.push(ToolSchema {
            name: "run_team_plan".to_string(),
            description: "Execute the submitted TeamPlan automatically. Runs ready tasks in dependency batches, creates task workers and verifies their output.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false
            }),
        });
    }
    if role == AgentRole::Leader {
        tools.push(ToolSchema {
            name: "deliberate_team_plan".to_string(),
            description: "Convene the planned Managers for a bounded roundtable. Each round shares peer positions; Managers cannot delegate or edit. Use before run_team_plan for multi-department plans.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "rounds": {"type": "integer", "minimum": 1, "maximum": 3}
                },
                "additionalProperties": false
            }),
        });
    }
    if role == AgentRole::Verifier {
        tools.push(ToolSchema {
            name: "submit_verdict".to_string(),
            description: "Submit the verification verdict. Approve only with evidence; do not edit files.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "approved": {"type": "boolean"},
                    "summary": {"type": "string", "description": "Evidence and decision, max 2000 chars"}
                },
                "required": ["approved", "summary"]
            }),
        });
    }
    if role.allows_tool(profile, "browser") {
        tools.push(ToolSchema {
            name: "browser".to_string(),
            description: "Drive a sandboxed local WKWebView: open localhost or worktree pages, inspect the accessibility/DOM tree, click/type/press/select, wait, read console logs, and save screenshots. External navigation is refused. The browser surface is serialized by the daemon ComputerLease. Use evaluate only when the user approves JavaScript execution."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["open", "snapshot", "click", "type", "press", "select", "wait_for", "screenshot", "console", "evaluate", "close"]
                    },
                    "url": {"type": "string", "description": "open: localhost HTTP(S) or a worktree file URL"},
                    "ref": {"type": "string", "description": "Element reference from snapshot, e.g. e12"},
                    "selector": {"type": "string", "description": "CSS selector alternative to ref"},
                    "text": {"type": "string", "description": "type: text to enter"},
                    "clear": {"type": "boolean", "description": "type: replace the current value; console: clear the bounded log buffer; default true for type"},
                    "key": {"type": "string", "description": "press: key such as Enter, Tab, Escape, ArrowDown"},
                    "value": {"type": "string", "description": "select: option value"},
                    "condition": {"type": "string", "enum": ["selector", "text", "url"]},
                    "timeout_ms": {"type": "integer", "minimum": 50, "maximum": 60000},
                    "max_chars": {"type": "integer", "minimum": 200, "maximum": 24000, "description": "snapshot: cap visible text"},
                    "max_nodes": {"type": "integer", "minimum": 1, "maximum": 1200},
                    "path": {"type": "string", "description": "screenshot: worktree-relative output path; defaults under .verlinpark/browser"},
                    "full_page": {"type": "boolean", "description": "screenshot: accepted for compatibility; v1 captures the viewport"},
                    "script": {"type": "string", "description": "evaluate: explicit approval-gated JavaScript"}
                },
                "required": ["action"],
                "additionalProperties": false
            }),
        });
    }
    tools.push(ToolSchema {
        name: "budget_status".to_string(),
        description: "Report remaining session, task, department, and project token/USD/time budgets, including whether unlimited mode is active."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {},
            "additionalProperties": false
        }),
    });
    tools.push(ToolSchema {
        name: "task_detail".to_string(),
        description: "Return the current or requested plan task: acceptance, dependencies, write scope, attempts, status, budgets, usage, and changed paths."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "task_id": {"type": "string", "description": "Defaults to the agent's current task"}
            },
            "additionalProperties": false
        }),
    });
    if matches!(role, AgentRole::Leader | AgentRole::Manager) {
        tools.push(ToolSchema {
            name: "task_control".to_string(),
            description: "Retry, skip, cancel, pause, or resume a task in this session's stored TeamPlan. Event-backed and audited."
                .to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "action": {"type": "string", "enum": ["retry", "skip", "cancel", "pause", "resume"]},
                    "task_id": {"type": "string"}
                },
                "required": ["action", "task_id"],
                "additionalProperties": false
            }),
        });
    }
    tools.push(ToolSchema {
        name: "tool_search".to_string(),
        description: "Search built-in tools and configured MCP tools by name or description. MCP results include the server/tool pair for the generic mcp call action."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Optional search text; empty lists available tools"},
                "limit": {"type": "integer", "minimum": 1, "maximum": 50, "description": "Default 20"},
                "include_schema": {"type": "boolean", "description": "Include a bounded parameter schema preview; default false"}
            },
            "additionalProperties": false
        }),
    });
    tools.push(ToolSchema {
        name: "publish_artifact_file".to_string(),
        description: "Publish an existing worktree file (screenshot, log, report, generated asset) as a session artifact. Max 5 MiB; content-addressed and retention-managed."
            .to_string(),
        parameters: serde_json::json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to the worktree root"},
                "name": {"type": "string", "description": "Optional display name; defaults to the file name"},
                "kind": {"type": "string", "description": "Optional category such as image, log, report, diff"},
                "mime_type": {"type": "string", "description": "Optional MIME type; inferred from the extension when omitted"}
            },
            "required": ["path"],
            "additionalProperties": false
        }),
    });
    if role == AgentRole::Manager && allow_delegation {
        tools.push(ToolSchema {
            name: "delegate_many".to_string(),
            description: "Run 1-5 Workers in parallel. Each task must declare a non-overlapping write_scope when more than one task is provided.".to_string(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": {
                    "tasks": {
                        "type": "array",
                        "minItems": 1,
                        "maxItems": 10,
                        "items": {
                            "type": "object",
                            "properties": {
                                "name": {"type": "string"},
                                "objective": {"type": "string"},
                                "instructions": {"type": "string"},
                                "profile": {
                                    "type": "string",
                                    "enum": ["coding", "research", "verification", "generic"]
                                },
                                "write_scope": {
                                    "type": "array",
                                    "maxItems": 20,
                                    "items": {"type": "string"}
                                }
                            },
                            "required": ["objective", "write_scope"]
                        }
                    }
                },
                "required": ["tasks"]
            }),
        });
    }
    tools
}

#[allow(clippy::type_complexity)]
fn parse_delegate(
    input: &serde_json::Value,
    parent_role: AgentRole,
    parent_profile: AgentProfile,
    team_plan: Option<&TeamPlan>,
) -> Result<(
    AgentRole,
    Option<String>,
    String,
    Option<String>,
    AgentProfile,
)> {
    let role_name = input
        .get("role")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("role is required"))?;
    let role = AgentRole::parse(role_name).ok_or_else(|| anyhow!("unknown role: {role_name}"))?;
    let expected = parent_role
        .child_role()
        .ok_or_else(|| anyhow!("{} cannot delegate", parent_role.as_str()))?;
    if role != expected {
        return Err(anyhow!(
            "{} can only delegate to {}",
            parent_role.as_str(),
            expected.as_str()
        ));
    }
    let objective = input
        .get("objective")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty() && objective.len() <= 4000)
        .ok_or_else(|| anyhow!("objective is required (max 4000 chars)"))?
        .to_string();
    let name = input
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty() && name.len() <= 80)
        .map(ToOwned::to_owned);
    let instructions = input
        .get("instructions")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|instructions| !instructions.is_empty() && instructions.len() <= 4000);
    let objective = match instructions {
        Some(instructions) => format!("{objective}\n\nInstructions:\n{instructions}"),
        None => objective,
    };
    let title = if role == AgentRole::Manager {
        let title = input
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty() && title.len() <= 60)
            .map(ToOwned::to_owned);
        if title.is_none() {
            return Err(anyhow!(
                "Managers need a title such as Discovery, Implementation, or Quality"
            ));
        }
        title
    } else {
        None
    };
    let requested_profile = match input.get("profile") {
        None => None,
        Some(value) => {
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("profile must be a string"))?;
            Some(AgentProfile::parse(value).ok_or_else(|| anyhow!("unknown profile: {value}"))?)
        }
    };
    let profile = match role {
        AgentRole::Manager => {
            if let Some(plan) = team_plan {
                let title = title.as_deref().unwrap_or_default();
                let department = plan
                    .departments
                    .iter()
                    .find(|department| department.manager_title.eq_ignore_ascii_case(title))
                    .ok_or_else(|| {
                        anyhow!("Manager title '{title}' is not in the submitted team plan")
                    })?;
                AgentProfile::parse(&department.profile)
                    .ok_or_else(|| anyhow!("unknown plan profile: {}", department.profile))?
            } else {
                requested_profile.unwrap_or(AgentProfile::Coding)
            }
        }
        AgentRole::Worker => requested_profile.unwrap_or(parent_profile),
        AgentRole::Labour | AgentRole::Verifier | AgentRole::Leader => parent_profile,
    };
    Ok((role, name, objective, title, profile))
}

fn parse_write_scope(input: &serde_json::Value) -> Result<Vec<String>> {
    let Some(items) = input.get("write_scope") else {
        return Ok(Vec::new());
    };
    let items = items
        .as_array()
        .ok_or_else(|| anyhow!("write_scope must be an array"))?;
    if items.len() > 20 {
        return Err(anyhow!("at most 20 write scopes are allowed"));
    }
    let mut scopes = Vec::with_capacity(items.len());
    for item in items {
        let scope = item
            .as_str()
            .ok_or_else(|| anyhow!("write_scope entries must be strings"))?;
        let scope = normalize_scope(scope);
        if scope.is_empty()
            || scope.contains("..")
            || scope.starts_with('/')
            || scope.starts_with('~')
        {
            return Err(anyhow!("invalid write scope: {scope}"));
        }
        scopes.push(scope);
    }
    Ok(scopes)
}

struct ParallelTask {
    name: Option<String>,
    objective: String,
    scope: Vec<String>,
    profile: AgentProfile,
}

enum ParallelResult {
    Output(ToolOutput),
    Cancelled,
    BudgetExceeded(BudgetStop),
}

fn parse_parallel_tasks(
    input: &serde_json::Value,
    parent_profile: AgentProfile,
) -> Result<Vec<ParallelTask>> {
    let tasks = input
        .get("tasks")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("tasks is required"))?;
    if tasks.is_empty() || tasks.len() > 10 {
        return Err(anyhow!("delegate_many accepts 1 to 10 tasks"));
    }
    let mut parsed = Vec::with_capacity(tasks.len());
    for task in tasks {
        let objective = task
            .get("objective")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|objective| !objective.is_empty() && objective.len() <= 4000)
            .ok_or_else(|| anyhow!("each task needs an objective (max 4000 chars)"))?;
        let instructions = task
            .get("instructions")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|instructions| !instructions.is_empty() && instructions.len() <= 4000);
        let objective = match instructions {
            Some(instructions) => format!("{objective}\n\nInstructions:\n{instructions}"),
            None => objective.to_string(),
        };
        let name = task
            .get("name")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|name| !name.is_empty() && name.len() <= 80)
            .map(ToOwned::to_owned);
        let scope = parse_write_scope(task)?;
        let profile = match task.get("profile") {
            None => parent_profile,
            Some(value) => {
                let value = value
                    .as_str()
                    .ok_or_else(|| anyhow!("profile must be a string"))?;
                AgentProfile::parse(value).ok_or_else(|| anyhow!("unknown profile: {value}"))?
            }
        };
        if tasks.len() > 1 && scope.is_empty() {
            return Err(anyhow!(
                "parallel Workers require a non-empty write_scope for every task"
            ));
        }
        parsed.push(ParallelTask {
            name,
            objective,
            scope,
            profile,
        });
    }
    for (index, left) in parsed.iter().enumerate() {
        for right in parsed.iter().skip(index + 1) {
            if scopes_overlap(&left.scope, &right.scope) {
                return Err(anyhow!(
                    "write scopes overlap: {} and {}",
                    left.scope.join(","),
                    right.scope.join(",")
                ));
            }
        }
    }
    Ok(parsed)
}

#[allow(clippy::too_many_arguments)]
async fn run_parallel_workers(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    spec: &AgentSpec,
    input: &serde_json::Value,
) -> Result<ParallelResult> {
    let tasks = parse_parallel_tasks(input, spec.profile)?;
    let mut prepared = Vec::new();
    let mut worker_slots = Vec::new();
    for task in tasks {
        if control.is_cancelled() {
            return Ok(ParallelResult::Cancelled);
        }
        let Some((ordinal, slot)) = budget.reserve_slot(AgentRole::Worker) else {
            return Ok(ParallelResult::Output(ToolOutput::error(
                "the Worker cap for this session has been reached",
            )));
        };
        worker_slots.push(slot);
        let child_name = task
            .name
            .clone()
            .unwrap_or_else(|| format!("Worker {}", ordinal));
        let child = engine
            .create_agent(
                &session.id,
                AgentRole::Worker.as_str(),
                &child_name,
                None,
                Some(spec.id.clone()),
                &model_for(config, AgentRole::Worker),
            )
            .await?;
        let info = engine
            .create_task_worktree(
                &project.root,
                &project.id,
                &session.id,
                &child.id,
                &spec.session_branch,
            )
            .await?;
        engine
            .set_agent_worktree(&child.id, &info.path.to_string_lossy(), &info.branch)
            .await?;
        let child_spec = AgentSpec {
            id: child.id.clone(),
            role: AgentRole::Worker,
            name: child_name.clone(),
            depth: spec.depth.saturating_add(1),
            parent_name: Some(spec.name.clone()),
            title: None,
            task_id: None,
            department: spec.department.clone(),
            profile: task.profile,
            allow_delegation: true,
            auto_approve: false,
            worktree: info.path,
            branch: info.branch,
            session_worktree: spec.session_worktree.clone(),
            session_branch: spec.session_branch.clone(),
            write_scope: task.scope.clone(),
            base_commit: info.base_commit,
        };
        emit(
            engine,
            &session.id,
            &child_spec,
            "agent.spawned",
            serde_json::json!({
                "parent_id": spec.id,
                "parent_name": spec.name,
                "objective": truncate(&task.objective),
                "write_scope": child_spec.write_scope,
            }),
        )
        .await?;
        let conversation = vec![ChatMessage::user(task.objective.clone())];
        prepared.push((child_spec, task.objective, conversation));
    }

    let futures = prepared
        .iter_mut()
        .map(|(child_spec, objective, conversation)| {
            run_loop(
                engine,
                config,
                session,
                project,
                control,
                budget,
                child_spec,
                objective,
                conversation,
                None,
                false,
            )
        });
    let results = futures_util::future::join_all(futures).await;

    let mut cancelled = false;
    let mut budget_exceeded = false;
    let mut handled = vec![false; prepared.len()];
    for (index, result) in results.iter().enumerate() {
        handled[index] = true;
        match result {
            Ok(LoopOutcome::Cancelled) => {
                cancelled = true;
                let (child_spec, _, _) = &prepared[index];
                engine
                    .update_agent_status(&child_spec.id, "cancelled")
                    .await?;
                emit(
                    engine,
                    &session.id,
                    child_spec,
                    "agent.finished",
                    serde_json::json!({"status": "cancelled"}),
                )
                .await?;
            }
            Ok(LoopOutcome::Verdict(_)) => {}
            Ok(LoopOutcome::BudgetExceeded(stop)) => {
                if stop.is_abortive() {
                    budget_exceeded = true;
                }
                let (child_spec, _, _) = &prepared[index];
                engine.update_agent_status(&child_spec.id, "failed").await?;
                emit(
                    engine,
                    &session.id,
                    child_spec,
                    "agent.finished",
                    serde_json::json!({
                        "status": "failed",
                        "summary": stop.summary(),
                    }),
                )
                .await?;
            }
            _ => {}
        }
    }
    if cancelled || budget_exceeded {
        let status = if cancelled { "cancelled" } else { "failed" };
        for (index, (child_spec, _, _)) in prepared.iter().enumerate() {
            if handled[index] {
                continue;
            }
            engine.update_agent_status(&child_spec.id, status).await?;
            emit(
                engine,
                &session.id,
                child_spec,
                "agent.finished",
                serde_json::json!({
                    "status": status,
                    "summary": "batch aborted before integration",
                }),
            )
            .await?;
        }
        return if cancelled {
            Ok(ParallelResult::Cancelled)
        } else {
            Ok(ParallelResult::BudgetExceeded(BudgetStop::session(budget)))
        };
    }

    let mut reports = Vec::new();
    let mut all_success = true;
    for (index, result) in results.into_iter().enumerate() {
        let (child_spec, objective, _) = &prepared[index];
        match result {
            Ok(LoopOutcome::Final(summary)) => {
                let summary = if summary.trim().is_empty() {
                    "The agent completed without a summary.".to_string()
                } else {
                    summary
                };
                match finish_worker(
                    engine, config, session, project, control, budget, spec, child_spec, objective,
                    &summary, None,
                )
                .await?
                {
                    WorkerFinish::Done { report, success } => {
                        all_success &= success;
                        reports.push(format!("{}: {}", child_spec.name, report));
                    }
                    WorkerFinish::Cancelled => {
                        for (_, (remaining, _, _)) in prepared.iter().enumerate().skip(index + 1) {
                            engine
                                .update_agent_status(&remaining.id, "cancelled")
                                .await?;
                            emit(
                                engine,
                                &session.id,
                                remaining,
                                "agent.finished",
                                serde_json::json!({
                                    "status": "cancelled",
                                    "summary": "batch aborted during verification",
                                }),
                            )
                            .await?;
                        }
                        return Ok(ParallelResult::Cancelled);
                    }
                    WorkerFinish::BudgetExceeded(stop) => {
                        for (_, (remaining, _, _)) in prepared.iter().enumerate().skip(index + 1) {
                            engine.update_agent_status(&remaining.id, "failed").await?;
                            emit(
                                engine,
                                &session.id,
                                remaining,
                                "agent.finished",
                                serde_json::json!({
                                    "status": "failed",
                                    "summary": "batch aborted during verification",
                                }),
                            )
                            .await?;
                        }
                        return Ok(ParallelResult::BudgetExceeded(stop));
                    }
                }
            }
            Ok(LoopOutcome::Verdict(_)) | Ok(LoopOutcome::Cancelled) => {
                all_success = false;
            }
            Ok(LoopOutcome::BudgetExceeded(stop)) => {
                if !stop.is_abortive() {
                    all_success = false;
                    reports.push(format!("{}: {}", child_spec.name, stop.summary()));
                }
            }
            Err(error) => {
                all_success = false;
                engine.update_agent_status(&child_spec.id, "failed").await?;
                emit(
                    engine,
                    &session.id,
                    child_spec,
                    "agent.finished",
                    serde_json::json!({
                        "status": "failed",
                        "summary": truncate(error.to_string().as_str()),
                    }),
                )
                .await?;
                reports.push(format!("{}: failed: {error}", child_spec.name));
            }
        }
    }
    let text = reports.join("\n\n");
    Ok(ParallelResult::Output(if all_success {
        ToolOutput::ok(text)
    } else {
        ToolOutput::error(text)
    }))
}

pub(crate) fn validate_team_plan(plan: &TeamPlan, max_managers: usize) -> Result<()> {
    if plan.goal.trim().is_empty() {
        return Err(anyhow!("team plan goal must not be empty"));
    }
    if plan.departments.is_empty() || plan.departments.len() > max_managers {
        return Err(anyhow!("team plan needs 1 to {max_managers} departments"));
    }
    let mut names = std::collections::HashSet::new();
    let mut titles = std::collections::HashSet::new();
    for department in &plan.departments {
        let name = department.name.trim();
        let title = department.manager_title.trim();
        if name.is_empty() || name.len() > 40 {
            return Err(anyhow!("department name must be 1-40 chars"));
        }
        if title.is_empty() || title.len() > 60 {
            return Err(anyhow!("manager title must be 1-60 chars"));
        }
        if !names.insert(name.to_lowercase()) {
            return Err(anyhow!("duplicate department name: {name}"));
        }
        if !titles.insert(title.to_lowercase()) {
            return Err(anyhow!("duplicate manager title: {title}"));
        }
        if AgentProfile::parse(&department.profile).is_none() {
            return Err(anyhow!(
                "unknown profile '{}' for department '{}'",
                department.profile,
                department.name
            ));
        }
        if department.objective.trim().is_empty() {
            return Err(anyhow!(
                "department '{}' needs an objective",
                department.name
            ));
        }
        if department.token_budget == Some(0) {
            return Err(anyhow!(
                "department '{}' token budget must be greater than zero",
                department.name
            ));
        }
        if department
            .cost_budget_usd
            .is_some_and(|budget| !budget.is_finite() || budget <= 0.0)
        {
            return Err(anyhow!(
                "department '{}' cost budget must be a positive finite number",
                department.name
            ));
        }
    }
    if plan.tasks.len() > 100 {
        return Err(anyhow!("team plan supports at most 100 tasks"));
    }
    let mut ids = std::collections::HashSet::new();
    for task in &plan.tasks {
        let id = task.id.trim();
        worktree::validate_task_id(id)?;
        if !ids.insert(id.to_string()) {
            return Err(anyhow!("duplicate task id: {id}"));
        }
        let Some(department) = plan
            .departments
            .iter()
            .find(|department| department.name == task.department)
        else {
            return Err(anyhow!(
                "task '{}' references unknown department '{}'",
                id,
                task.department
            ));
        };
        let needs_write = !task.write_scope.is_empty()
            || matches!(
                task.kind.to_ascii_lowercase().as_str(),
                "coding" | "code" | "implement" | "implementation" | "edit" | "fix"
            );
        let resolved_profile = task
            .profile
            .as_deref()
            .unwrap_or(department.profile.as_str());
        if needs_write && matches!(resolved_profile, "generic" | "verification") {
            return Err(anyhow!(
                "task '{}' needs a writable profile: set task.profile=\"coding\" or use a coding department",
                id
            ));
        }
        if task.title.trim().is_empty() {
            return Err(anyhow!("task '{id}' needs a title"));
        }
        if task.kind.trim().is_empty() {
            return Err(anyhow!("task '{id}' needs a kind"));
        }
        if !(-100..=100).contains(&task.priority) {
            return Err(anyhow!("task '{id}' priority must be between -100 and 100"));
        }
        if let Some(profile) = &task.profile
            && AgentProfile::parse(profile).is_none()
        {
            return Err(anyhow!("task '{id}' has unknown profile: {profile}"));
        }
        if task.acceptance.is_empty() {
            return Err(anyhow!("task '{id}' needs at least one acceptance item"));
        }
        for dep in &task.deps {
            if dep == id {
                return Err(anyhow!("task '{id}' depends on itself"));
            }
            if !ids.contains(dep) && !plan.tasks.iter().any(|candidate| candidate.id == *dep) {
                return Err(anyhow!("task '{id}' has unknown dependency '{dep}'"));
            }
        }
        let mut scopes = Vec::new();
        for scope in &task.write_scope {
            let normalized = normalize_scope(scope);
            if normalized.is_empty()
                || normalized.contains("..")
                || normalized.starts_with('/')
                || normalized.starts_with('~')
            {
                return Err(anyhow!("task '{id}' has invalid write scope: {scope}"));
            }
            scopes.push(normalized);
        }
        if task.token_budget == Some(0) {
            return Err(anyhow!(
                "task '{id}' token budget must be greater than zero"
            ));
        }
        if task
            .cost_budget_usd
            .is_some_and(|budget| !budget.is_finite() || budget <= 0.0)
        {
            return Err(anyhow!(
                "task '{id}' cost budget must be a positive finite number"
            ));
        }
        if let Some(department) = plan
            .departments
            .iter()
            .find(|department| department.name == task.department)
        {
            if let (Some(task_limit), Some(department_limit)) =
                (task.token_budget, department.token_budget)
                && task_limit > department_limit
            {
                return Err(anyhow!(
                    "task '{id}' token budget exceeds its department budget"
                ));
            }
            if let (Some(task_limit), Some(department_limit)) =
                (task.cost_budget_usd, department.cost_budget_usd)
                && task_limit > department_limit + f64::EPSILON
            {
                return Err(anyhow!(
                    "task '{id}' cost budget exceeds its department budget"
                ));
            }
        }
    }
    for department in &plan.departments {
        let task_tokens: u64 = plan
            .tasks
            .iter()
            .filter(|task| task.department == department.name)
            .filter_map(|task| task.token_budget)
            .fold(0, u64::saturating_add);
        if let Some(limit) = department.token_budget
            && task_tokens > limit
        {
            return Err(anyhow!(
                "department '{}' task token budgets exceed the department cap",
                department.name
            ));
        }
        let task_cost: f64 = plan
            .tasks
            .iter()
            .filter(|task| task.department == department.name)
            .filter_map(|task| task.cost_budget_usd)
            .sum();
        if let Some(limit) = department.cost_budget_usd
            && task_cost > limit + f64::EPSILON
        {
            return Err(anyhow!(
                "department '{}' task cost budgets exceed the department cap",
                department.name
            ));
        }
    }
    // Cycle detection over the task DAG.
    fn visit(
        id: &str,
        tasks: &[vp_protocol::types::PlanTask],
        state: &mut std::collections::HashMap<String, u8>,
    ) -> Result<()> {
        match state.get(id).copied().unwrap_or(0) {
            1 => return Err(anyhow!("team plan task graph contains a cycle at '{id}'")),
            2 => return Ok(()),
            _ => {}
        }
        state.insert(id.to_string(), 1);
        if let Some(task) = tasks.iter().find(|task| task.id == id) {
            for dep in &task.deps {
                visit(dep, tasks, state)?;
            }
        }
        state.insert(id.to_string(), 2);
        Ok(())
    }
    let mut state = std::collections::HashMap::new();
    for task in &plan.tasks {
        visit(&task.id, &plan.tasks, &mut state)?;
    }
    Ok(())
}

enum RoundtableResult {
    Done(String),
    Cancelled,
    BudgetExceeded(BudgetStop),
}

fn plan_budget_text(tokens: Option<u64>, cost: Option<f64>) -> String {
    match (tokens, cost) {
        (None, None) => String::new(),
        (Some(tokens), None) => format!(" budget={tokens} tokens"),
        (None, Some(cost)) => format!(" budget=${cost:.4}"),
        (Some(tokens), Some(cost)) => format!(" budget={tokens} tokens/${cost:.4}"),
    }
}

fn plan_prompt(plan: &TeamPlan) -> String {
    let departments = plan
        .departments
        .iter()
        .map(|department| {
            let budget = plan_budget_text(department.token_budget, department.cost_budget_usd);
            format!(
                "- {}: manager '{}' ({}) — {}{}",
                department.name,
                department.manager_title,
                department.profile,
                department.objective,
                budget
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let tasks = plan
        .tasks
        .iter()
        .map(|task| {
            format!(
                "- {} [{}] {} priority={} deps={} scope={}{}",
                task.id,
                task.department,
                task.title,
                task.priority,
                task.deps.join(","),
                task.write_scope.join(","),
                plan_budget_text(task.token_budget, task.cost_budget_usd)
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!(
        "Goal: {}\nDepartments:\n{departments}\nTasks:\n{tasks}",
        plan.goal
    )
}

#[allow(clippy::too_many_arguments)]
async fn deliberate_team_plan(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    leader_spec: &AgentSpec,
    plan: &TeamPlan,
    rounds: u32,
) -> Result<RoundtableResult> {
    let mut managers = Vec::new();
    for department in &plan.departments {
        managers.push(
            ensure_plan_manager(engine, config, session, budget, leader_spec, department).await?,
        );
    }
    let overview = plan_prompt(plan);
    let mut positions: Vec<(String, String)> = Vec::new();
    let mut report = Vec::new();

    for round in 1..=rounds {
        if control.is_cancelled() {
            return Ok(RoundtableResult::Cancelled);
        }
        if budget.exhausted() {
            return Ok(RoundtableResult::BudgetExceeded(BudgetStop::session(
                budget,
            )));
        }
        let peer_context = positions
            .iter()
            .map(|(name, position)| format!("{name}: {position}"))
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut prepared = Vec::new();
        for manager in &managers {
            let title = manager.title.as_deref().unwrap_or(manager.name.as_str());
            let prompt = if round == 1 {
                format!(
                    "Manager roundtable, round 1.\n{overview}\n\
                     You are the {title} Manager ({profile}). Analyze your department scope, \
                     dependencies, risks, and required handoffs with the other Managers. \
                     Do not delegate, edit, or run shell. Return a concise position.",
                    profile = manager.profile.as_str()
                )
            } else {
                format!(
                    "Manager roundtable, round {round}.\n{overview}\n\
                     Positions from the previous round:\n{peer_context}\n\
                     You are the {title} Manager ({profile}). Respond to peers, resolve \
                     conflicts, agree interfaces and handoffs, and update your position. \
                     Do not delegate, edit, or run shell. Return a concise decision.",
                    profile = manager.profile.as_str()
                )
            };
            let conversation = vec![ChatMessage::user(prompt.clone())];
            prepared.push((manager.clone(), prompt, conversation));
        }

        let futures = prepared.iter_mut().map(|(manager, prompt, conversation)| {
            run_loop(
                engine,
                config,
                session,
                project,
                control,
                budget,
                manager,
                prompt,
                conversation,
                None,
                false,
            )
        });
        let results = futures_util::future::join_all(futures).await;
        let mut round_positions = Vec::new();
        for (index, result) in results.into_iter().enumerate() {
            let (manager, _, _) = &prepared[index];
            match result {
                Ok(LoopOutcome::Final(position)) => {
                    let position = if position.trim().is_empty() {
                        "No position returned.".to_string()
                    } else {
                        position
                    };
                    emit(
                        engine,
                        &session.id,
                        manager,
                        "manager.roundtable",
                        serde_json::json!({
                            "round": round,
                            "position": truncate(&position),
                            "title": manager.title,
                            "profile": manager.profile.as_str(),
                        }),
                    )
                    .await?;
                    round_positions.push((
                        manager
                            .title
                            .clone()
                            .unwrap_or_else(|| manager.name.clone()),
                        position,
                    ));
                }
                Ok(LoopOutcome::Cancelled) => return Ok(RoundtableResult::Cancelled),
                Ok(LoopOutcome::BudgetExceeded(stop)) => {
                    return Ok(RoundtableResult::BudgetExceeded(stop));
                }
                Ok(LoopOutcome::Verdict(_)) => {}
                Err(error) => {
                    emit(
                        engine,
                        &session.id,
                        manager,
                        "manager.roundtable.failed",
                        serde_json::json!({
                            "round": round,
                            "error": truncate(error.to_string().as_str()),
                        }),
                    )
                    .await?;
                    return Err(error);
                }
            }
        }
        positions = round_positions;
        report.push(format!(
            "Round {round}\n{}",
            positions
                .iter()
                .map(|(title, position)| format!("{title}: {}", truncate(position)))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }

    Ok(RoundtableResult::Done(report.join("\n\n")))
}

enum PlanRunResult {
    Done(String),
    Cancelled,
    BudgetExceeded(BudgetStop),
}

enum PlanAbort {
    Cancelled,
    Budget(BudgetStop),
}

impl PlanAbort {
    fn task_status(&self) -> &'static str {
        match self {
            Self::Cancelled => "cancelled",
            Self::Budget(_) => "blocked",
        }
    }

    fn reason(&self) -> String {
        match self {
            Self::Cancelled => "session cancelled".to_string(),
            Self::Budget(stop) => stop.summary(),
        }
    }
}

async fn emit_plan_task_finished(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    task_id: &str,
    status: &str,
    reason: Option<&str>,
) -> Result<()> {
    let mut payload = serde_json::json!({
        "task_id": task_id,
        "task_status": status,
    });
    if let Some(reason) = reason {
        payload["reason"] = serde_json::json!(reason);
    }
    emit(engine, session_id, spec, "plan_task.finished", payload).await
}

async fn latest_plan_results(engine: &Engine, session_id: &str) -> Result<HashMap<String, String>> {
    let events = engine.recent_events(session_id, 1_000).await?;
    let mut results = HashMap::new();
    for event in events {
        if event.kind != "plan_task.finished" {
            continue;
        }
        let Some(task_id) = event
            .payload
            .get("task_id")
            .and_then(serde_json::Value::as_str)
        else {
            continue;
        };
        let status = event
            .payload
            .get("task_status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let detail = event
            .payload
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .or_else(|| {
                event
                    .payload
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
            })
            .unwrap_or("");
        results.insert(
            task_id.to_string(),
            if detail.is_empty() {
                status.to_string()
            } else {
                format!("{status} · {}", truncate_to(detail, 500))
            },
        );
    }
    Ok(results)
}

async fn project_memory_brief(
    engine: &Engine,
    project_id: &str,
    query: &str,
    limit: u32,
) -> Result<String> {
    let query = query.trim();
    let mut memories = if query.is_empty() {
        Vec::new()
    } else {
        engine
            .list_memories(project_id, Some(query.to_string()), None, limit)
            .await?
            .memories
    };
    if memories.is_empty() {
        memories = engine
            .list_memories(project_id, None, None, limit)
            .await?
            .memories;
    }
    Ok(memories
        .iter()
        .map(|memory| {
            format!(
                "[{}] {} · {} · {}",
                memory.kind,
                memory.title,
                memory.agent_name,
                truncate_to(&memory.body, 420)
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

async fn coordination_brief_for_agent(
    engine: &Engine,
    session_id: &str,
    agent_id: &str,
    task_id: Option<&str>,
    limit: usize,
) -> Result<String> {
    let notes = engine
        .coordination_notes_for_agent(session_id, agent_id, task_id, limit.clamp(1, 50) as u32)
        .await?;
    Ok(notes
        .iter()
        .map(format_coordination_note)
        .collect::<Vec<_>>()
        .join("\n"))
}

/// Inject unread mailbox messages into a Worker brief and mark them read.
async fn mailbox_brief(
    engine: &Engine,
    session_id: &str,
    agent_id: &str,
    limit: usize,
) -> Result<String> {
    let messages = engine
        .list_agent_messages(session_id, Some(agent_id.to_string()), true)
        .await?;
    if messages.is_empty() {
        return Ok(String::new());
    }
    let selected = messages
        .iter()
        .rev()
        .take(limit.clamp(1, 20))
        .cloned()
        .collect::<Vec<_>>();
    for message in selected.iter().rev() {
        let _ = engine.mark_agent_message_read(&message.id).await;
    }
    Ok(selected
        .iter()
        .rev()
        .map(|message| {
            format!(
                "[{}] {} → {} · {}",
                message.kind,
                message.from_name,
                message.to_name,
                truncate_to(&message.body, 500)
            )
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

fn task_dependency_brief(
    task: &PlanTask,
    statuses: &HashMap<String, String>,
    results: &HashMap<String, String>,
) -> String {
    if task.deps.is_empty() {
        return "No upstream dependencies.".to_string();
    }
    task.deps
        .iter()
        .take(8)
        .map(|dep| {
            let status = statuses.get(dep).map(String::as_str).unwrap_or("pending");
            let result = results
                .get(dep)
                .map(String::as_str)
                .unwrap_or("no completed result yet");
            format!("- {dep} [{status}] {result}")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

async fn record_scheduled_task_finish(
    engine: &Engine,
    session_id: &str,
    plan_seq: u64,
    task_id: &str,
    status: &str,
    detail: Option<&str>,
) -> Result<Option<ScheduledTask>> {
    let task = engine
        .scheduler()
        .finish(session_id, plan_seq, task_id, status, detail)
        .await?;
    if status == "completed"
        && let Some(summary) = detail.map(str::trim).filter(|summary| !summary.is_empty())
    {
        record_task_memory(engine, session_id, task_id, summary).await;
    }
    if task.is_some()
        && let Err(error) = engine.advance_orchestration(session_id).await
    {
        tracing::warn!(%error, task_id, "orchestration controller pass failed");
    }
    Ok(task)
}

async fn record_task_memory(engine: &Engine, session_id: &str, task_id: &str, summary: &str) {
    let Ok((_session, project)) = load_session_project(engine.store().clone(), session_id).await
    else {
        return;
    };
    let title = format!("Task {task_id} result");
    let existing = engine
        .list_memories(
            &project.id,
            Some(title.clone()),
            Some("task_result".into()),
            3,
        )
        .await
        .map(|result| result.memories)
        .unwrap_or_default();
    if existing.iter().any(|memory| memory.title == title) {
        return;
    }
    if let Err(error) = engine
        .create_memory_as(
            session_id,
            "orchestrator",
            "Orchestrator",
            "task_result",
            &title,
            &truncate_to(summary, 8_000),
            vec![task_id.to_string(), "task_result".to_string()],
        )
        .await
    {
        tracing::warn!(%error, task_id, "failed to persist task result memory");
    }
}

fn task_profile(task: &PlanTask, department_profile: AgentProfile) -> AgentProfile {
    if let Some(profile) = &task.profile
        && let Some(profile) = AgentProfile::parse(profile)
    {
        return profile;
    }
    match task.kind.as_str() {
        "research" => AgentProfile::Research,
        "review" | "verification" => AgentProfile::Verification,
        "coding" => AgentProfile::Coding,
        _ => department_profile,
    }
}

async fn ensure_plan_manager(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    budget: &SessionBudget,
    leader_spec: &AgentSpec,
    department: &PlanDepartment,
) -> Result<AgentSpec> {
    let profile = AgentProfile::parse(&department.profile)
        .ok_or_else(|| anyhow!("unknown profile: {}", department.profile))?;
    let existing = engine
        .list_agents(&session.id)
        .await?
        .into_iter()
        .find(|agent| {
            agent.role == "manager"
                && agent
                    .title
                    .as_deref()
                    .is_some_and(|title| title.eq_ignore_ascii_case(&department.manager_title))
        });
    let agent = match existing {
        Some(agent) => {
            engine
                .set_agent_profile(&agent.id, profile.as_str())
                .await?;
            engine
                .set_agent_task(&agent.id, None, Some(department.name.clone()))
                .await?;
            engine.update_agent_status(&agent.id, "idle").await?;
            agent
        }
        None => {
            if budget.reserve(AgentRole::Manager).is_none() {
                return Err(anyhow!("the Manager cap for this session has been reached"));
            }
            let agent = engine
                .create_agent(
                    &session.id,
                    AgentRole::Manager.as_str(),
                    &department.manager_title,
                    Some(department.manager_title.clone()),
                    Some(leader_spec.id.clone()),
                    &model_for(config, AgentRole::Manager),
                )
                .await?;
            engine
                .set_agent_profile(&agent.id, profile.as_str())
                .await?;
            engine
                .set_agent_task(&agent.id, None, Some(department.name.clone()))
                .await?;
            engine.update_agent_status(&agent.id, "idle").await?;
            agent
        }
    };
    let session_worktree = session
        .worktree
        .clone()
        .map(PathBuf::from)
        .unwrap_or_default();
    let session_branch = session.branch.clone().unwrap_or_default();
    let spec = AgentSpec {
        id: agent.id.clone(),
        role: AgentRole::Manager,
        name: agent.name.clone(),
        depth: 1,
        parent_name: Some(leader_spec.name.clone()),
        title: Some(department.manager_title.clone()),
        task_id: None,
        department: Some(department.name.clone()),
        profile,
        allow_delegation: false,
        auto_approve: false,
        worktree: session_worktree.clone(),
        branch: session_branch.clone(),
        session_worktree,
        session_branch,
        write_scope: Vec::new(),
        base_commit: session.base_commit.clone().unwrap_or_default(),
    };
    engine
        .set_agent_worktree(&agent.id, &spec.worktree.to_string_lossy(), &spec.branch)
        .await?;
    emit(
        engine,
        &session.id,
        &spec,
        "agent.spawned",
        serde_json::json!({
            "parent_id": leader_spec.id,
            "parent_name": leader_spec.name,
            "objective": department.objective,
            "department": department.name,
            "lazy": true,
        }),
    )
    .await?;
    Ok(spec)
}

fn normalized_task_scope(task: &PlanTask) -> Vec<String> {
    task.write_scope
        .iter()
        .map(|scope| normalize_scope(scope))
        .collect()
}

/// Select a ready batch that can safely run concurrently. The input order is
/// the scheduler's effective-priority order (session + task + aging); this
/// function only removes scope conflicts. Tasks with no scope are treated as
/// unrestricted and run alone. A task that overlaps an earlier task in the
/// batch stays pending for the next batch instead of being merged into a
/// conflicting write set.
fn select_plan_batch(ready: Vec<PlanTask>, limit: usize) -> Vec<PlanTask> {
    let mut batch: Vec<PlanTask> = Vec::new();
    for task in ready {
        if batch.len() >= limit {
            break;
        }
        let scope = normalized_task_scope(&task);
        if !batch.is_empty() {
            if scope.is_empty() {
                continue;
            }
            let overlaps = batch.iter().any(|existing| {
                let existing = normalized_task_scope(existing);
                existing.is_empty() || scopes_overlap(&scope, &existing)
            });
            if overlaps {
                continue;
            }
        }
        batch.push(task);
    }
    batch
}

#[allow(clippy::too_many_arguments)]
async fn execute_team_plan(
    engine: &Engine,
    config: &AgentConfig,
    session: &Session,
    project: &Project,
    control: &RunControl,
    budget: &SessionBudget,
    leader_spec: &AgentSpec,
    plan: &TeamPlan,
    max_tasks: Option<usize>,
) -> Result<PlanRunResult> {
    let limited = max_tasks.is_some();
    let limit = max_tasks.unwrap_or(usize::MAX).max(1);
    let mut executed = 0usize;
    let original_worker_cap = budget.worker_cap();
    let _worker_cap_guard = config.auto_expand_workers.then(|| WorkerCapGuard {
        budget,
        original: original_worker_cap,
    });
    let mut manager_specs: std::collections::HashMap<String, AgentSpec> =
        std::collections::HashMap::new();

    let (plan_seq, scheduled) = engine.scheduler().sync_plan(session, plan).await?;
    let mut statuses = scheduled
        .iter()
        .map(|task| (task.task_id.clone(), task.status.clone()))
        .collect::<std::collections::HashMap<_, _>>();
    for task in &plan.tasks {
        statuses
            .entry(task.id.clone())
            .or_insert_with(|| "pending".to_string());
    }
    let completed = statuses
        .iter()
        .filter(|(_, status)| status.as_str() == "completed")
        .map(|(task_id, _)| task_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let skipped = statuses
        .iter()
        .filter(|(_, status)| status.as_str() == "skipped")
        .map(|(task_id, _)| task_id.clone())
        .collect::<std::collections::HashSet<_>>();
    let mut remaining = plan
        .tasks
        .iter()
        .filter(|task| !completed.contains(&task.id) && !skipped.contains(&task.id))
        .cloned()
        .collect::<Vec<_>>();
    let mut reports = plan
        .tasks
        .iter()
        .filter_map(|task| {
            if completed.contains(&task.id) {
                Some(format!(
                    "{} [completed]: already completed; skipped",
                    task.id
                ))
            } else if skipped.contains(&task.id) {
                Some(format!("{} [skipped]: previously skipped", task.id))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();

    while !remaining.is_empty() {
        if executed >= limit {
            break;
        }
        if control.is_cancelled() {
            if limited {
                return Ok(PlanRunResult::Cancelled);
            }
            for task in &remaining {
                statuses.insert(task.id.clone(), "cancelled".to_string());
                emit(
                    engine,
                    &session.id,
                    leader_spec,
                    "plan_task.finished",
                    serde_json::json!({
                        "task_id": task.id,
                        "task_status": "cancelled",
                    }),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    "cancelled",
                    None,
                )
                .await?;
            }
            return Ok(PlanRunResult::Cancelled);
        }
        if budget.exhausted() {
            if limited {
                return Ok(PlanRunResult::BudgetExceeded(BudgetStop::session(budget)));
            }
            for task in &remaining {
                statuses.insert(task.id.clone(), "blocked".to_string());
                emit(
                    engine,
                    &session.id,
                    leader_spec,
                    "plan_task.finished",
                    serde_json::json!({
                        "task_id": task.id,
                        "task_status": "blocked",
                        "reason": "token budget exhausted",
                    }),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    "blocked",
                    Some("token budget exhausted"),
                )
                .await?;
            }
            return Ok(PlanRunResult::BudgetExceeded(BudgetStop::session(budget)));
        }
        let ready = engine
            .scheduler()
            .ready_tasks(session, plan_seq, plan, plan.tasks.len())
            .await?;
        if ready.is_empty() {
            for task in &remaining {
                statuses.insert(task.id.clone(), "blocked".to_string());
                let unmet = task
                    .deps
                    .iter()
                    .filter(|dep| {
                        !matches!(
                            statuses.get(*dep).map(String::as_str),
                            Some("completed" | "skipped")
                        )
                    })
                    .map(|dep| {
                        format!(
                            "{} ({})",
                            dep,
                            statuses.get(dep).map(String::as_str).unwrap_or("pending")
                        )
                    })
                    .collect::<Vec<_>>();
                let reason = if unmet.is_empty() {
                    "dependency not completed".to_string()
                } else {
                    format!("waiting on {}", unmet.join(", "))
                };
                emit(
                    engine,
                    &session.id,
                    leader_spec,
                    "plan_task.finished",
                    serde_json::json!({
                        "task_id": task.id,
                        "task_status": "blocked",
                        "reason": reason,
                    }),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    "blocked",
                    Some(&reason),
                )
                .await?;
            }
            break;
        }
        if !limited && config.auto_expand_workers && ready.len() > budget.worker_cap() {
            let target = ready.len().min(HARD_WORKER_CAP);
            if budget.can_expand_workers() {
                budget.expand_workers(target);
                emit(
                    engine,
                    &session.id,
                    leader_spec,
                    "plan.auto_expand",
                    serde_json::json!({
                        "ready_tasks": ready.len(),
                        "worker_cap": budget.worker_cap(),
                    }),
                )
                .await?;
            }
        }
        let capacity = budget
            .available(AgentRole::Worker)
            .min(limit.saturating_sub(executed));
        if capacity == 0 {
            if limited {
                break;
            }
            for task in &remaining {
                statuses.insert(task.id.clone(), "blocked".to_string());
                emit(
                    engine,
                    &session.id,
                    leader_spec,
                    "plan_task.finished",
                    serde_json::json!({
                        "task_id": task.id,
                        "task_status": "blocked",
                        "reason": "Worker cap is fully occupied",
                    }),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    "blocked",
                    Some("Worker cap is fully occupied"),
                )
                .await?;
            }
            break;
        }
        let batch = select_plan_batch(ready, capacity);
        let task_ids = batch.iter().map(|task| task.id.clone()).collect::<Vec<_>>();
        let (claimed, claim_events) = engine
            .scheduler()
            .claim(&session.id, plan_seq, &task_ids)
            .await?;
        for event in claim_events {
            engine.broadcast_event(event);
        }
        if claimed.is_empty() {
            break;
        }
        let batch_ids = claimed
            .iter()
            .map(|task| task.task_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let batch = batch
            .into_iter()
            .filter(|task| batch_ids.contains(&task.id))
            .collect::<Vec<_>>();

        let mut prepared = Vec::new();
        let mut worker_slots = Vec::new();
        let plan_results = latest_plan_results(engine, &session.id).await?;
        for task in &batch {
            let manager_spec = if let Some(spec) = manager_specs.get(&task.department) {
                spec.clone()
            } else {
                let department = plan
                    .departments
                    .iter()
                    .find(|department| department.name == task.department)
                    .ok_or_else(|| anyhow!("missing department for task {}", task.department))?;
                let spec =
                    ensure_plan_manager(engine, config, session, budget, leader_spec, department)
                        .await?;
                manager_specs.insert(task.department.clone(), spec.clone());
                spec
            };
            let profile = task_profile(task, manager_spec.profile);
            let acceptance = task.acceptance.join("; ");
            let write_scope = task
                .write_scope
                .iter()
                .map(|scope| normalize_scope(scope))
                .collect::<Vec<_>>();
            let worker_name = format!("{} · {}", task.department, truncate_to(&task.title, 48));
            let Some((_ordinal, slot)) = budget.reserve_slot(AgentRole::Worker) else {
                return Err(anyhow!("the Worker cap for this session has been reached"));
            };
            worker_slots.push(slot);
            let worker = engine
                .create_agent(
                    &session.id,
                    AgentRole::Worker.as_str(),
                    &worker_name,
                    None,
                    Some(manager_spec.id.clone()),
                    &model_for(config, AgentRole::Worker),
                )
                .await?;
            engine
                .set_agent_profile(&worker.id, profile.as_str())
                .await?;
            engine
                .set_agent_task(
                    &worker.id,
                    Some(task.id.clone()),
                    Some(task.department.clone()),
                )
                .await?;
            let info = engine
                .create_task_worktree(
                    &project.root,
                    &project.id,
                    &session.id,
                    &task.id,
                    &manager_spec.session_branch,
                )
                .await?;
            engine
                .set_agent_worktree(&worker.id, &info.path.to_string_lossy(), &info.branch)
                .await?;
            let worker_spec = AgentSpec {
                id: worker.id.clone(),
                role: AgentRole::Worker,
                name: worker_name,
                depth: manager_spec.depth.saturating_add(1),
                parent_name: Some(manager_spec.name.clone()),
                title: None,
                task_id: Some(task.id.clone()),
                department: Some(task.department.clone()),
                profile,
                allow_delegation: true,
                auto_approve: false,
                worktree: info.path,
                branch: info.branch,
                session_worktree: manager_spec.session_worktree.clone(),
                session_branch: manager_spec.session_branch.clone(),
                write_scope,
                base_commit: info.base_commit,
            };
            emit(
                engine,
                &session.id,
                &worker_spec,
                "agent.spawned",
                serde_json::json!({
                    "parent_id": manager_spec.id,
                    "parent_name": manager_spec.name,
                    "objective": task.title,
                    "plan_task_id": task.id,
                    "department": task.department,
                }),
            )
            .await?;
            statuses.insert(task.id.clone(), "running".to_string());
            let dependency_brief = task_dependency_brief(task, &statuses, &plan_results);
            let coordination = coordination_brief_for_agent(
                engine,
                &session.id,
                &worker_spec.id,
                Some(&task.id),
                6,
            )
            .await?;
            let mailbox = mailbox_brief(engine, &session.id, &worker_spec.id, 4).await?;
            let memory_brief = project_memory_brief(
                engine,
                &project.id,
                &format!("{} {}", task.title, task.department),
                4,
            )
            .await
            .unwrap_or_default();
            let memory_note = if memory_brief.is_empty() {
                String::new()
            } else {
                format!("\nRelevant long-term memory:\n{memory_brief}")
            };
            let coordination_note = if coordination.is_empty() {
                String::new()
            } else {
                format!("\nRecent coordination for this task:\n{coordination}")
            };
            let mailbox_note = if mailbox.is_empty() {
                String::new()
            } else {
                format!("\nUnread mailbox:\n{mailbox}")
            };
            let objective = format!(
                "Execute plan task {}: {}. Department: {} ({}). Acceptance: {}. \
                 Upstream handoffs:\n{}\n{}{}{}\
                 Work only in your task worktree and return a concise summary.",
                task.id,
                task.title,
                task.department,
                manager_spec.title.as_deref().unwrap_or(&manager_spec.name),
                acceptance,
                dependency_brief,
                memory_note,
                coordination_note,
                mailbox_note
            );
            prepared.push((
                task.clone(),
                worker_spec,
                manager_spec,
                objective.clone(),
                acceptance,
                vec![ChatMessage::user(objective)],
            ));
        }

        let futures = prepared
            .iter_mut()
            .map(|(_, worker_spec, _, objective, _, conversation)| {
                run_loop(
                    engine,
                    config,
                    session,
                    project,
                    control,
                    budget,
                    worker_spec,
                    objective,
                    conversation,
                    None,
                    false,
                )
            });
        let results = futures_util::future::join_all(futures).await;

        let mut abort: Option<PlanAbort> = None;
        for (index, result) in results.into_iter().enumerate() {
            let (task, worker_spec, manager_spec, objective, acceptance, _) = &prepared[index];
            if let Some(abort) = &abort {
                let status = abort.task_status();
                let reason = abort.reason();
                engine
                    .update_agent_status(
                        &worker_spec.id,
                        if status == "cancelled" {
                            "cancelled"
                        } else {
                            "failed"
                        },
                    )
                    .await?;
                statuses.insert(task.id.clone(), status.to_string());
                emit_plan_task_finished(
                    engine,
                    &session.id,
                    worker_spec,
                    &task.id,
                    status,
                    Some(&reason),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    status,
                    Some(&reason),
                )
                .await?;
                reports.push(format!("{} [{}]: {}", task.id, status, reason));
                continue;
            }
            match result {
                Ok(LoopOutcome::Final(summary)) => {
                    let summary = if summary.trim().is_empty() {
                        "The worker completed without a summary.".to_string()
                    } else {
                        summary
                    };
                    if !control.is_cancelled()
                        && engine
                            .plan_task_cancel_requested(&session.id, &task.id)
                            .await?
                    {
                        engine
                            .update_agent_status(&worker_spec.id, "cancelled")
                            .await?;
                        statuses.insert(task.id.clone(), "cancelled".to_string());
                        emit(
                            engine,
                            &session.id,
                            worker_spec,
                            "plan_task.finished",
                            serde_json::json!({
                                "task_id": task.id,
                                "task_status": "cancelled",
                                "reason": "cancel requested by user",
                            }),
                        )
                        .await?;
                        record_scheduled_task_finish(
                            engine,
                            &session.id,
                            plan_seq,
                            &task.id,
                            "cancelled",
                            Some("cancel requested by user"),
                        )
                        .await?;
                        reports.push(format!("{} [cancelled]: cancel requested", task.id));
                        continue;
                    }
                    match finish_worker(
                        engine,
                        config,
                        session,
                        project,
                        control,
                        budget,
                        manager_spec,
                        worker_spec,
                        objective,
                        &summary,
                        Some(acceptance),
                    )
                    .await?
                    {
                        WorkerFinish::Done { report, success } => {
                            let status = if success { "completed" } else { "failed" };
                            statuses.insert(task.id.clone(), status.to_string());
                            emit(
                                engine,
                                &session.id,
                                worker_spec,
                                "plan_task.finished",
                                serde_json::json!({
                                    "task_id": task.id,
                                    "task_status": status,
                                    "summary": truncate(&report),
                                }),
                            )
                            .await?;
                            record_scheduled_task_finish(
                                engine,
                                &session.id,
                                plan_seq,
                                &task.id,
                                status,
                                Some(report.as_str()),
                            )
                            .await?;
                            reports.push(format!(
                                "{} [{}]: {}",
                                task.id,
                                status,
                                truncate(&report)
                            ));
                        }
                        WorkerFinish::Cancelled => {
                            statuses.insert(task.id.clone(), "cancelled".to_string());
                            emit(
                                engine,
                                &session.id,
                                worker_spec,
                                "plan_task.finished",
                                serde_json::json!({
                                    "task_id": task.id,
                                    "task_status": "cancelled",
                                }),
                            )
                            .await?;
                            record_scheduled_task_finish(
                                engine,
                                &session.id,
                                plan_seq,
                                &task.id,
                                "cancelled",
                                None,
                            )
                            .await?;
                            abort = Some(PlanAbort::Cancelled);
                        }
                        WorkerFinish::BudgetExceeded(stop) => {
                            statuses.insert(task.id.clone(), "blocked".to_string());
                            emit(
                                engine,
                                &session.id,
                                worker_spec,
                                "plan_task.finished",
                                serde_json::json!({
                                    "task_id": task.id,
                                    "task_status": "blocked",
                                    "reason": stop.summary(),
                                }),
                            )
                            .await?;
                            record_scheduled_task_finish(
                                engine,
                                &session.id,
                                plan_seq,
                                &task.id,
                                "blocked",
                                Some(&stop.summary()),
                            )
                            .await?;
                            abort = Some(PlanAbort::Budget(stop));
                        }
                    }
                }
                Ok(LoopOutcome::Cancelled) => {
                    let task_cancel = !control.is_cancelled()
                        && engine
                            .plan_task_cancel_requested(&session.id, &task.id)
                            .await?;
                    engine
                        .update_agent_status(&worker_spec.id, "cancelled")
                        .await?;
                    statuses.insert(task.id.clone(), "cancelled".to_string());
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "plan_task.finished",
                        serde_json::json!({
                            "task_id": task.id,
                            "task_status": "cancelled",
                            "reason": if task_cancel { "cancel requested by user" } else { "session cancelled" },
                        }),
                    )
                    .await?;
                    record_scheduled_task_finish(
                        engine,
                        &session.id,
                        plan_seq,
                        &task.id,
                        "cancelled",
                        task_cancel.then_some("cancel requested by user"),
                    )
                    .await?;
                    if task_cancel {
                        reports.push(format!("{} [cancelled]: cancel requested", task.id));
                    } else {
                        abort = Some(PlanAbort::Cancelled);
                    }
                }
                Ok(LoopOutcome::BudgetExceeded(stop)) => {
                    engine
                        .update_agent_status(&worker_spec.id, "failed")
                        .await?;
                    statuses.insert(task.id.clone(), "blocked".to_string());
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "plan_task.finished",
                        serde_json::json!({
                            "task_id": task.id,
                            "task_status": "blocked",
                            "reason": stop.summary(),
                        }),
                    )
                    .await?;
                    record_scheduled_task_finish(
                        engine,
                        &session.id,
                        plan_seq,
                        &task.id,
                        "blocked",
                        Some(&stop.summary()),
                    )
                    .await?;
                    if stop.is_abortive() {
                        abort = Some(PlanAbort::Budget(stop));
                    }
                }
                Ok(LoopOutcome::Verdict(_)) => {}
                Err(error) => {
                    engine
                        .update_agent_status(&worker_spec.id, "failed")
                        .await?;
                    statuses.insert(task.id.clone(), "failed".to_string());
                    emit(
                        engine,
                        &session.id,
                        worker_spec,
                        "plan_task.finished",
                        serde_json::json!({
                            "task_id": task.id,
                            "task_status": "failed",
                            "summary": truncate(error.to_string().as_str()),
                        }),
                    )
                    .await?;
                    record_scheduled_task_finish(
                        engine,
                        &session.id,
                        plan_seq,
                        &task.id,
                        "failed",
                        Some(&error.to_string()),
                    )
                    .await?;
                    reports.push(format!("{} [failed]: {error}", task.id));
                }
            }
        }

        if let Some(abort) = abort {
            if limited {
                return Ok(match abort {
                    PlanAbort::Cancelled => PlanRunResult::Cancelled,
                    PlanAbort::Budget(stop) => PlanRunResult::BudgetExceeded(stop),
                });
            }
            let status = abort.task_status();
            let reason = abort.reason();
            for task in remaining
                .iter()
                .filter(|task| !batch_ids.contains(&task.id))
            {
                statuses.insert(task.id.clone(), status.to_string());
                emit_plan_task_finished(
                    engine,
                    &session.id,
                    leader_spec,
                    &task.id,
                    status,
                    Some(&reason),
                )
                .await?;
                record_scheduled_task_finish(
                    engine,
                    &session.id,
                    plan_seq,
                    &task.id,
                    status,
                    Some(&reason),
                )
                .await?;
            }
            return Ok(match abort {
                PlanAbort::Cancelled => PlanRunResult::Cancelled,
                PlanAbort::Budget(stop) => PlanRunResult::BudgetExceeded(stop),
            });
        }

        executed = executed.saturating_add(batch.len());
        remaining.retain(|task| !batch_ids.contains(&task.id));
    }

    if limited {
        if executed == 0 {
            return Ok(PlanRunResult::Done(
                "No ready task was dispatched.".to_string(),
            ));
        }
        let summary = reports
            .last()
            .cloned()
            .unwrap_or_else(|| "Task dispatch finished.".to_string());
        return Ok(PlanRunResult::Done(summary));
    }

    let completed = statuses
        .values()
        .filter(|status| status.as_str() == "completed")
        .count();
    let failed = statuses
        .values()
        .filter(|status| {
            matches!(
                status.as_str(),
                "failed" | "blocked" | "cancelled" | "skipped"
            )
        })
        .count();
    reports.push(format!(
        "Plan complete: {completed} completed, {failed} not completed."
    ));
    Ok(PlanRunResult::Done(reports.join("\n")))
}

fn truncate_to(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_string()
    } else {
        value.chars().take(max).collect::<String>() + "…"
    }
}

fn parse_verdict(input: &serde_json::Value) -> Result<VerifierVerdict> {
    let approved = input
        .get("approved")
        .and_then(serde_json::Value::as_bool)
        .ok_or_else(|| anyhow!("approved is required"))?;
    let summary = input
        .get("summary")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|summary| !summary.is_empty() && summary.len() <= 2000)
        .ok_or_else(|| anyhow!("summary is required (max 2000 chars)"))?
        .to_string();
    Ok(VerifierVerdict { approved, summary })
}

fn role_system_prompt(
    role: AgentRole,
    project: &Project,
    session: &Session,
    spec: &AgentSpec,
    objective: &str,
) -> String {
    let worktree = spec.worktree.to_string_lossy();
    let scope_note = if spec.write_scope.is_empty() {
        "Write scope: unrestricted within the worktree.".to_string()
    } else {
        format!(
            "Write scope: {}. Changes outside this scope are rejected before integration.",
            spec.write_scope.join(", ")
        )
    };
    let profile_note = match spec.profile {
        AgentProfile::Research => {
            "Profile: research. Use read/search/fetch_url and cite evidence; do not claim facts without sources."
        }
        AgentProfile::Coding => {
            "Profile: coding. Inspect the code, make focused changes, and run tests."
        }
        AgentProfile::Verification => {
            "Profile: verification. Review evidence and run checks; do not edit files."
        }
        AgentProfile::Generic => "Profile: generic. Read and search; do not assume write access.",
    };
    let base = format!(
        "Project: '{}' at {}. Session goal: '{}'. Worktree: {}. Branch: {}. {} {} \
         The project root is read-only; all writes stay inside the worktree. \
         Do not claim success without evidence. Project knowledge sources may be indexed: \
         call search_sources before source-grounded claims, and cite each used chunk with \
         the exact [cN] key returned by the tool. Never invent a citation key. \
         If a project skill is listed, call use_skill with its exact name before following \
         it; the active skill may narrow the tools you can use.",
        project.name, project.root, session.goal, worktree, spec.branch, profile_note, scope_note
    );
    match role {
        AgentRole::Leader => format!(
            "{base}\n\
             You are VerlinPark's Leader agent, {}. Keep a short plan with todo_write. \
             Prefer a real team for non-trivial work: if the objective has multiple steps, \
             files, or verification needs, submit a TeamPlan and use run_team_plan. Do the \
             work directly only when it is genuinely small (for example a one-file edit or a \
             quick inspection) or the user explicitly asks for no team. Do not use a team as \
             ceremony for a tiny task. For a goal that spans multiple areas, delegate \
             department-level objectives to Managers with delegate(role=\"manager\"). Give every Manager a distinct title \
             (for example Discovery, Implementation, Quality) via the title field; each title \
             defines that Manager's department role. Managers will split work and delegate to \
             Workers; Workers may use Labour. Do not delegate directly to Workers or Labour. \
             For a multi-department plan, call deliberate_team_plan first so Managers can \
             review peer positions and agree on handoffs; then call run_team_plan to execute. \
             Do not manually delegate the planned Managers unless you intentionally override \
             the plan. Call team_status to inspect ready/blocked tasks and recent coordination. \
             Use coordinate action=decision for cross-department choices, action=escalate when \
             the user must decide, and action=unblock after resolving a reported blocker. \
             You can still inspect and edit files yourself for small tasks. \
             Review summaries, verify the final diff, and report the result.",
            spec.name
        ),
        AgentRole::Manager => format!(
            "{base}\n\
             You are the {} Manager, reporting to {}. Objective: {objective}. \
             You may read and search, but you must not edit files or run shell commands. \
             Call team_status before splitting work and after a Worker returns. Split the \
             objective into focused Worker tasks. For independent tasks, use \
             delegate_many(tasks=[...]) with non-overlapping write_scope values so Workers \
             run in parallel; otherwise use delegate(role=\"worker\"). Tell Workers to report \
             blockers with coordinate action=blocker. Record handoffs and decisions with \
             coordinate, and call coordinate action=unblock after resolving a blocker. \
             Review the returned summaries, resolve gaps, then return a concise consolidated \
             result. Do not delegate to Managers or Labour.",
            spec.title.as_deref().unwrap_or(spec.name.as_str()),
            spec.parent_name.as_deref().unwrap_or("the Leader")
        ),
        AgentRole::Worker => format!(
            "{base}\n\
             You are Worker {}, reporting to a Manager. Task: {objective}. \
             You own implementation in the worktree. Inspect first, make focused edits, \
             and run tests. Use team_status to see upstream/blocked tasks before you start. \
             Delegate small research or verification subtasks to Labour with \
             delegate(role=\"labour\") when useful. If you cannot proceed, call coordinate \
             action=blocker with the task id and a concrete reason; never silently stall. \
             Record a final summary with coordinate action=note before returning. Include \
             evidence in your result. Do not delegate to Managers or Workers.",
            spec.name
        ),
        AgentRole::Verifier => format!(
            "{base}\n\
             You are Verifier {}, reporting to a Manager. Task under review: {objective}. \
             You are read-only: do not edit files. Use team_status for upstream context. \
             Inspect the task branch/diff and run relevant tests or checks with shell. \
             Approve only when the changes meet the objective and evidence is sufficient. Then call submit_verdict with \
             approved and summary. You cannot delegate.",
            spec.name
        ),
        AgentRole::Labour => format!(
            "{base}\n\
             You are Labour {}, reporting to a Worker. Subtask: {objective}. \
             Execute it directly in the worktree and return a concise report. If you are \
             blocked, use coordinate action=blocker with the task id and a concrete reason. \
             You cannot delegate to other agents.",
            spec.name
        ),
    }
}

async fn load_session_project(store: Store, session_id: &str) -> Result<(Session, Project)> {
    let session_id = session_id.to_string();
    tokio::task::spawn_blocking(move || -> Result<(Session, Project)> {
        let session = store
            .get_session(&session_id)?
            .ok_or_else(|| anyhow!("session not found"))?;
        let project = store
            .get_project(&session.project_id)?
            .ok_or_else(|| anyhow!("project not found"))?;
        Ok((session, project))
    })
    .await?
}

struct LoadedConversation {
    messages: Vec<ChatMessage>,
    ids: Vec<Option<String>>,
}

async fn load_conversation(engine: &Engine, session_id: &str) -> Result<LoadedConversation> {
    let messages: Vec<StoredMessage> = engine.list_messages(session_id).await?;
    let mut start = 0usize;
    let mut summary = None;
    // A context.compacted event points at the last summarized message. The
    // summary text lives in the event; the active conversation becomes
    // [summary] + messages after that point. Older messages stay in the
    // database for search, replay, and transcript.
    if let Some(event) = engine
        .latest_event_of_kind(session_id, "context.compacted")
        .await?
    {
        let mode = event
            .payload
            .get("mode")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("summary");
        let through = event
            .payload
            .get("through_message_id")
            .and_then(serde_json::Value::as_str);
        let text = event
            .payload
            .get("summary")
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.trim().is_empty());
        if let Some(through) = through
            && let Some(index) = messages.iter().position(|message| message.id == through)
        {
            start = index.saturating_add(1);
            if mode == "summary" {
                summary = text.map(str::to_string);
            }
        }
    }
    let mut ids = messages
        .iter()
        .skip(start)
        .map(|message| Some(message.id.clone()))
        .collect::<Vec<_>>();
    let mut messages = messages
        .into_iter()
        .skip(start)
        .map(|message| {
            let tool_calls: Vec<ToolCall> =
                serde_json::from_value(message.tool_calls).unwrap_or_default();
            ChatMessage {
                role: message.role,
                content: message.content,
                tool_calls: if tool_calls.is_empty() {
                    None
                } else {
                    Some(tool_calls)
                },
                tool_call_id: message.tool_call_id,
            }
        })
        .collect::<Vec<_>>();
    if let Some(summary) = summary {
        // The event stores the already-prefixed summary text.
        messages.insert(0, ChatMessage::user(summary));
        ids.insert(0, None);
    }
    Ok(LoadedConversation { messages, ids })
}

fn parse_todos(input: &serde_json::Value) -> Result<Vec<TodoItem>> {
    let items = input
        .get("items")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| anyhow!("items is required"))?;
    if items.len() > 50 {
        return Err(anyhow!("at most 50 todo items are allowed"));
    }
    let mut todos = Vec::with_capacity(items.len());
    for item in items {
        let title = item
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|title| !title.is_empty() && title.len() <= 200)
            .ok_or_else(|| anyhow!("each item needs a non-empty title (max 200 chars)"))?;
        let status = item
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("pending");
        if !matches!(status, "pending" | "in_progress" | "completed") {
            return Err(anyhow!("status must be pending, in_progress, or completed"));
        }
        todos.push(TodoItem {
            title: title.to_string(),
            status: status.to_string(),
        });
    }
    Ok(todos)
}

enum ApprovalOutcome {
    Approved,
    Denied,
    TimedOut,
    Cancelled,
}

enum QuestionOutcome {
    Answered(String),
    Cancelled,
    TimedOut,
}

async fn wait_for_question(
    receiver: oneshot::Receiver<String>,
    control: &RunControl,
    timeout_secs: u64,
) -> QuestionOutcome {
    if control.is_cancelled() {
        return QuestionOutcome::Cancelled;
    }
    let mut cancel_rx = control.subscribe();
    tokio::select! {
        _ = cancel_rx.changed() => QuestionOutcome::Cancelled,
        result = timeout(Duration::from_secs(timeout_secs), receiver) => match result {
            Ok(Ok(answer)) => QuestionOutcome::Answered(answer),
            Ok(Err(_)) => QuestionOutcome::TimedOut,
            Err(_) => QuestionOutcome::TimedOut,
        },
    }
}

async fn wait_for_approval(
    receiver: oneshot::Receiver<bool>,
    control: &RunControl,
    timeout_duration: Duration,
) -> ApprovalOutcome {
    wait_for_approval_with(control.subscribe(), receiver, timeout_duration).await
}

async fn wait_for_approval_with(
    mut cancel_rx: watch::Receiver<bool>,
    receiver: oneshot::Receiver<bool>,
    timeout_duration: Duration,
) -> ApprovalOutcome {
    if *cancel_rx.borrow() {
        return ApprovalOutcome::Cancelled;
    }
    tokio::select! {
        _ = cancel_rx.changed() => ApprovalOutcome::Cancelled,
        result = timeout(timeout_duration, receiver) => match result {
            Ok(Ok(true)) => ApprovalOutcome::Approved,
            Ok(Ok(false)) | Ok(Err(_)) => ApprovalOutcome::Denied,
            Err(_) => ApprovalOutcome::TimedOut,
        },
    }
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

#[cfg(test)]
#[path = "agent/tests.rs"]
mod tests;
