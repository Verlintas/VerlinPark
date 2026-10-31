use std::collections::{HashMap, VecDeque};
use std::env;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use async_recursion::async_recursion;

use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use tokio::sync::{Semaphore, oneshot, watch};
use tokio::time::timeout;
use vp_protocol::types::{
    AgentInfo, CapacityInfo, ConfigureAgentParams, MergeQueueEntry, PlanDepartment, PlanTask,
    Project, RoleModels, Session, StoredMessage, TeamPlan, TodoItem,
};
use vp_providers::{
    AnthropicProvider, ChatMessage, ChatRequest, OpenAiCompatibleProvider, Provider, StreamEvent,
    ToolCall, ToolSchema,
};
use vp_store::Store;

use crate::Engine;
use crate::budget::{BudgetReason, Rejection, ScopeRequest, Spend};
use crate::tools::{ToolContext, ToolOutput, ToolRegistry};
use crate::worktree;

fn global_session_cap() -> usize {
    env::var("VERLINPARK_GLOBAL_SESSION_CAP")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(4)
        .clamp(1, 32)
}

fn global_worker_cap() -> usize {
    env::var("VERLINPARK_GLOBAL_WORKERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(12)
        .clamp(1, 100)
}

fn global_labour_cap() -> usize {
    env::var("VERLINPARK_GLOBAL_LABOUR")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(24)
        .clamp(1, 100)
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
    pub integration_command: Option<String>,
    pub tools: ToolRegistry,
    /// Maximum number of session runs active across the whole daemon.
    pub max_active_sessions: usize,
    /// Cross-session write-scope locks for Worker/Labour runs.
    pub scope_locks: Arc<ScopeLocks>,
    /// Daemon-wide Worker/Labour admission.
    pub capacity: Arc<CapacityController>,
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
    fn new(limit: usize) -> Self {
        Self {
            limit: limit.max(1),
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
                        .and_then(|index| state.waiters.remove(index))
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
            let index = state
                .waiters
                .iter()
                .position(|waiter| {
                    state.last_session.as_deref() != Some(waiter.session_id.as_str())
                })
                .unwrap_or(0);
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
        let worker_limit = workers.max(1);
        let labour_limit = labour.max(1);
        Self {
            workers: Arc::new(FairCapacity::new(worker_limit)),
            labour: Arc::new(FairCapacity::new(labour_limit)),
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
        control: &RunControl,
    ) -> Option<CapacityPermit> {
        match role {
            AgentRole::Worker => self.workers.acquire(session_id, control).await,
            AgentRole::Labour => self.labour.acquire(session_id, control).await,
            _ => None,
        }
    }
}

/// A daemon-wide registry of active task write scopes. Tasks in different
/// sessions that overlap wait for each other; tasks in the same session are
/// allowed to share (their own scheduler already serializes them).
#[derive(Default)]
pub struct ScopeLocks {
    entries: Mutex<Vec<ScopeLockEntry>>,
    changed: tokio::sync::Notify,
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
            {
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
                    return Ok(Some(ScopeLockGuard {
                        locks: self.clone(),
                        project_id: project_id.to_string(),
                        agent_id: agent_id.to_string(),
                    }));
                }
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
        if let Ok(mut entries) = self.locks.entries.lock() {
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
    global_slots: Arc<Semaphore>,
    capacity: Arc<CapacityController>,
}

struct RunControl {
    cancelled: watch::Sender<bool>,
    paused: watch::Sender<bool>,
    finalized: tokio::sync::Mutex<Option<String>>,
}

impl RunControl {
    fn new() -> Self {
        let (cancelled, _) = watch::channel(false);
        let (paused, _) = watch::channel(false);
        Self {
            cancelled,
            paused,
            finalized: tokio::sync::Mutex::new(None),
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
                matches!(name, "read_file" | "list_dir" | "grep")
                    || (profile == AgentProfile::Research && name == "fetch_url")
            }
            Self::Verifier => {
                matches!(name, "read_file" | "list_dir" | "grep" | "shell")
                    || (profile == AgentProfile::Research && name == "fetch_url")
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
                ),
                AgentProfile::Research => matches!(
                    name,
                    "read_file" | "list_dir" | "grep" | "write_file" | "fetch_url"
                ),
                AgentProfile::Verification => matches!(
                    name,
                    "read_file" | "list_dir" | "grep" | "shell" | "fetch_url"
                ),
                AgentProfile::Generic => matches!(name, "read_file" | "list_dir" | "grep"),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AgentProfile {
    Coding,
    Research,
    Verification,
    Generic,
}

impl AgentProfile {
    fn parse(value: &str) -> Option<Self> {
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
    /// Skip per-tool approvals for an explicitly authorized internal run.
    /// The shell sandbox still applies.
    auto_approve: bool,
    worktree: PathBuf,
    branch: String,
    session_worktree: PathBuf,
    session_branch: String,
    write_scope: Vec<String>,
    base_commit: String,
}

struct SessionBudget {
    limit: Option<u64>,
    cost_limit: Option<f64>,
    time_limit: Option<Duration>,
    started: std::time::Instant,
    caps: AgentCaps,
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
            started: std::time::Instant::now(),
            caps,
            state: Mutex::new(BudgetState {
                spent,
                reserved_tokens: 0,
                cost_spent: 0.0,
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

    fn add_cost(&self, cost: f64) -> bool {
        if let Ok(mut state) = self.state.lock() {
            state.cost_spent += cost.max(0.0);
            return self
                .cost_limit
                .is_some_and(|limit| state.cost_spent >= limit);
        }
        true
    }

    fn exceeded_reason(&self) -> Option<&'static str> {
        if self.limit.is_some_and(|limit| self.spent() >= limit) {
            return Some("tokens");
        }
        if let Ok(state) = self.state.lock()
            && self
                .cost_limit
                .is_some_and(|limit| state.cost_spent >= limit)
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
        let mut state = self.state.lock().ok()?;
        if let Some(limit) = self.limit
            && state.spent >= limit.saturating_mul(8) / 10
            && !state.warned_tokens
        {
            state.warned_tokens = true;
            return Some("tokens");
        }
        if let Some(limit) = self.cost_limit
            && state.cost_spent >= limit * 0.8
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

    fn elapsed_secs(&self) -> u64 {
        self.started.elapsed().as_secs()
    }

    fn available_tokens(&self) -> Option<u64> {
        let limit = self.limit?;
        let state = self.state.lock().ok()?;
        Some(
            limit
                .saturating_sub(state.spent)
                .saturating_sub(state.reserved_tokens),
        )
    }

    fn reserve_tokens(&self, amount: u64) -> Option<TokenReservation<'_>> {
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

    fn reserve(&self, role: AgentRole) -> Option<usize> {
        let mut state = self.state.lock().ok()?;
        let (counter, cap) = match role {
            AgentRole::Manager => (&mut state.managers, self.caps.managers),
            AgentRole::Worker => (&mut state.workers, self.caps.workers),
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
            AgentRole::Worker => (state.workers, self.caps.workers),
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
        self.amount = 0;
        let Ok(mut state) = self.budget.state.lock() else {
            return true;
        };
        state.reserved_tokens = state.reserved_tokens.saturating_sub(amount);
        state.spent = state.spent.saturating_add(actual);
        self.budget.limit.is_some_and(|limit| state.spent >= limit)
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
            reserved_cost: 0.0,
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

    fn is_session(&self) -> bool {
        self.scope == "session"
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
                integration_command: env::var("VERLINPARK_INTEGRATION_COMMAND")
                    .ok()
                    .filter(|command| !command.trim().is_empty()),
                tools: ToolRegistry::default_tools(),
                max_active_sessions: global_session_cap(),
                scope_locks: Arc::new(ScopeLocks::default()),
                capacity: Arc::new(CapacityController::new(
                    global_worker_cap(),
                    global_labour_cap(),
                )),
            },
        )
    }

    pub fn unconfigured(engine: Engine) -> Self {
        Self {
            engine,
            config: Arc::new(RwLock::new(None)),
            runs: Arc::new(Mutex::new(HashMap::new())),
            auto_resolver_started: Arc::new(AtomicBool::new(false)),
            global_slots: Arc::new(Semaphore::new(global_session_cap())),
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
            global_slots: Arc::new(Semaphore::new(global_cap)),
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
            integration_command: env::var("VERLINPARK_INTEGRATION_COMMAND")
                .ok()
                .filter(|command| !command.trim().is_empty()),
            tools: ToolRegistry::default_tools(),
            max_active_sessions: global_session_cap(),
            scope_locks: Arc::new(ScopeLocks::default()),
            capacity: self.capacity.clone(),
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
            },
            None => AgentInfo {
                configured: false,
                provider: None,
                model: None,
                base_url: None,
                role_models: RoleModels::default(),
                capacity: self.capacity.snapshot(),
            },
        }
    }

    pub async fn retry_merge(&self, entry_id: &str) -> Result<MergeQueueEntry> {
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
        let (session, _) =
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
        let Some(entry) = self
            .engine
            .retry_merge_queue_entry(
                &entry.id,
                &commit,
                &base_commit,
                Some("rebased onto the current session branch".to_string()),
            )
            .await?
        else {
            return Err(anyhow!("merge queue entry can no longer be retried"));
        };

        let profile = agent
            .profile
            .as_deref()
            .and_then(AgentProfile::parse)
            .unwrap_or(AgentProfile::Coding);
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
    /// bypasses per-tool approval for the resolver and verifier because the user
    /// already chose auto-resolution, but the shell sandbox still applies.
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
            permit = self.global_slots.clone().acquire_owned() => permit.ok(),
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
        .with_time(session.time_budget_secs);
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
            return Err(error);
        }

        let engine = self.engine.clone();
        let session_id = session_id.to_string();
        let prompt = prompt.to_string();
        let runs = self.runs.clone();
        let global_slots = self.global_slots.clone();
        tokio::spawn(async move {
            let permit = tokio::select! {
                permit = global_slots.acquire_owned() => permit.ok(),
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
                        serde_json::json!({"error": error.to_string()}),
                    )
                    .await;
                let _ = control
                    .finalize_session(&engine, &session_id, "failed")
                    .await;
            }
            if let Ok(mut runs) = runs.lock() {
                runs.remove(&session_id);
            }
        });
        Ok(())
    }

    pub fn cancel(&self, session_id: &str) -> bool {
        if let Ok(runs) = self.runs.lock()
            && let Some(control) = runs.get(session_id)
        {
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
    .with_time(session.time_budget_secs);
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

    let mut conversation = load_conversation(&engine, session_id).await?;
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
        worktree::commit_all(&task_worktree, &message)
    })
    .await?
}

async fn run_integration_command(worktree: PathBuf, command: String) -> Result<String> {
    let output = timeout(
        Duration::from_secs(300),
        tokio::process::Command::new("sh")
            .arg("-lc")
            .arg(&command)
            .current_dir(&worktree)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| anyhow!("integration command timed out after 300s"))??;

    let mut text = String::new();
    if !output.stdout.is_empty() {
        text.push_str(&String::from_utf8_lossy(&output.stdout));
    }
    if !output.stderr.is_empty() {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&String::from_utf8_lossy(&output.stderr));
    }
    let text = truncate(&text);
    if output.status.success() {
        Ok(text)
    } else {
        Err(anyhow!(
            "integration command exited with {}:\n{}",
            output.status.code().unwrap_or(-1),
            text
        ))
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

/// Merge and integration-check an existing queue entry. The entry must already
/// have a valid `base_commit` (the session HEAD before this merge).
async fn process_merge_entry(
    engine: &Engine,
    session: &Session,
    spec: &AgentSpec,
    entry: &MergeQueueEntry,
    integration_command: Option<&str>,
) -> Result<String> {
    engine
        .update_merge_queue_entry(&entry.id, "merging", None)
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
            "attempt": entry.attempt,
            "has_command": integration_command.is_some(),
        }),
    )
    .await?;

    let merge_worktree = spec.session_worktree.clone();
    let branch = entry.branch.clone();
    let message = format!("merge {}", spec.name);
    let merge_result = tokio::task::spawn_blocking(move || {
        worktree::merge_branch(&merge_worktree, &branch, &message)
    })
    .await?;
    if let Err(error) = merge_result {
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

    engine
        .update_merge_queue_entry(&entry.id, "checking", None)
        .await?;
    let check_worktree = spec.session_worktree.clone();
    let check_base = entry.base_commit.clone();
    let check_result = tokio::task::spawn_blocking(move || {
        worktree::check_merged_diff(&check_worktree, &check_base)
    })
    .await?;
    let check_result = match check_result {
        Ok(()) => {
            if let Some(command) = integration_command {
                run_integration_command(spec.session_worktree.clone(), command.to_string()).await
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
            let reset_base = entry.base_commit.clone();
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
            &worker_spec.worktree.to_string_lossy(),
            &worker_spec.branch,
        )
        .await?;
    engine
        .set_agent_profile(&verifier.id, AgentProfile::Verification.as_str())
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
        worktree: worker_spec.worktree.clone(),
        branch: worker_spec.branch.clone(),
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
         Worker summary: {worker_summary}. The worktree is {} on branch {}; base commit {}. \
         {focus} Do not edit files. Then call submit_verdict with approved and summary.",
        worker_spec.name,
        worker_spec.worktree.to_string_lossy(),
        worker_spec.branch,
        worker_spec.base_commit
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
        false,
    )
    .await;
    match result {
        Ok(LoopOutcome::Verdict(verdict)) => {
            let verify_root = verifier_spec.worktree.clone();
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
            success: true,
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
            if stop.is_session() {
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

fn shell_secret_roots(data_dir: &std::path::Path) -> Vec<PathBuf> {
    let mut roots = vec![
        data_dir.join("verlinpark.db"),
        data_dir.join("run"),
        data_dir.join("sessions"),
        data_dir.join("logs"),
    ];
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
    task_tokens: Option<u64>,
    task_cost: Option<f64>,
    department_tokens: Option<u64>,
    department_cost: Option<f64>,
}

impl PlanBudgetScope {
    fn from_plan(plan: Option<&TeamPlan>, spec: &AgentSpec) -> Self {
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
        Self {
            task_id: spec.task_id.clone(),
            department: spec.department.clone(),
            task_tokens: task.and_then(|task| task.token_budget),
            task_cost: task.and_then(|task| task.cost_budget_usd),
            department_tokens: department.and_then(|department| department.token_budget),
            department_cost: department.and_then(|department| department.cost_budget_usd),
        }
    }

    fn active(&self) -> bool {
        self.task_tokens.is_some()
            || self.task_cost.is_some()
            || self.department_tokens.is_some()
            || self.department_cost.is_some()
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
        task: (u64, f64),
        department: (u64, f64),
        input_price: f64,
        output_price: f64,
    ) -> Vec<ScopeRequest> {
        let mut requests = Vec::new();
        if (self.task_tokens.is_some() || self.task_cost.is_some())
            && let Some(id) = &self.task_id
        {
            requests.push(ScopeRequest {
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
        requests
    }

    fn after(&self, task: (u64, f64), department: (u64, f64)) -> Option<BudgetStop> {
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

#[allow(clippy::too_many_arguments)]
#[async_recursion]
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
    top_level: bool,
) -> Result<LoopOutcome> {
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
                        .acquire(spec.role, &session.id, control)
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
    let _scope_guard =
        if matches!(spec.role, AgentRole::Worker | AgentRole::Labour) && !spec.auto_approve {
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
    };
    let system = role_system_prompt(spec.role, project, session, spec, objective);
    let tools = tool_schemas(config, spec.role, spec.profile, spec.allow_delegation);
    let mut team_plan = engine.get_team_plan(&session.id).await?;
    let plan_budget = PlanBudgetScope::from_plan(team_plan.as_ref(), spec);

    for turn in 0..config.max_turns {
        if !wait_for_resume(control).await {
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
        let mut request = ChatRequest {
            model: model_for(config, spec.role),
            messages: conversation.clone(),
            system: Some(system.clone()),
            max_tokens: config.max_tokens,
            temperature: Some(0.2),
            tools: tools.clone(),
        };
        const MIN_OUTPUT_TOKENS: u64 = 64;
        let configured_output = config.max_tokens.unwrap_or(4096).max(1) as u64;
        let input_estimate = estimate_request_tokens(&request);
        let (input_price, output_price) = role_price(config, spec.role);
        let scope_spent = if plan_budget.active() {
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
        let desired_output = if budget.limit.is_some() {
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
        let scope_lease = if plan_budget.active() {
            let requests =
                plan_budget.requests(task_spent, department_spent, input_price, output_price);
            match engine.budget_registry().grant(
                &session.id,
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
        if budget.limit.is_some() {
            let Some(reservation) =
                budget.reserve_tokens(input_estimate.saturating_add(output_budget))
            else {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session_tokens(
                    budget,
                )));
            };
            token_reservation = Some(reservation);
        }
        if output_budget < configured_output {
            request.max_tokens = Some(output_budget.min(u32::MAX as u64) as u32);
        }
        let _scope_lease = scope_lease;
        let mut stream = config
            .provider
            .stream_chat(request.clone())
            .await
            .context("starting provider stream")?;

        let mut text = String::new();
        let mut tool_calls = Vec::new();
        let mut usage = (0u32, 0u32);
        let mut cancelled = false;
        let mut cancel_rx = control.subscribe();
        loop {
            if control.is_cancelled() {
                cancelled = true;
                break;
            }
            let item = tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_err() || *cancel_rx.borrow() {
                        cancelled = true;
                        break;
                    }
                    continue;
                }
                item = stream.next() => item,
            };
            let Some(item) = item else {
                break;
            };
            match item? {
                StreamEvent::TextDelta(delta) => {
                    text.push_str(&delta);
                    emit(
                        engine,
                        &session.id,
                        spec,
                        "agent.message_delta",
                        serde_json::json!({"text": delta}),
                    )
                    .await?;
                }
                StreamEvent::ToolCall(call) => tool_calls.push(call),
                StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                } => {
                    usage = (usage.0 + input_tokens, usage.1 + output_tokens);
                }
                StreamEvent::Done => {}
            }
        }
        if cancelled {
            return Ok(LoopOutcome::Cancelled);
        }
        let (input_tokens, output_tokens, estimated) = if usage != (0, 0) {
            (usage.0 as u64, usage.1 as u64, false)
        } else {
            // Some local endpoints omit usage. Persist a conservative estimate
            // so the budget still survives across turns and daemon restarts;
            // the event is marked `estimated` so it is not mistaken for
            // provider-reported usage.
            let input = estimate_request_tokens(&request);
            let output = estimate_text_tokens(&text).saturating_add(
                serde_json::to_string(&tool_calls)
                    .map(|calls| estimate_text_tokens(&calls))
                    .unwrap_or(0),
            );
            (input, output, true)
        };
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
        if token_reservation
            .take()
            .is_some_and(|reservation| reservation.settle(actual_usage))
            || budget.add_cost(cost)
        {
            return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
        }
        if plan_budget.active() {
            let (task_tokens, task_cost, department_tokens, department_cost) = engine
                .usage_spent(
                    &session.id,
                    spec.task_id.as_deref(),
                    spec.department.as_deref(),
                )
                .await?;
            if let Some(stop) = plan_budget.after(
                (task_tokens, task_cost),
                (department_tokens, department_cost),
            ) {
                emit_scope_budget_stop(engine, &session.id, spec, &stop).await?;
                return Ok(LoopOutcome::BudgetExceeded(stop));
            }
        }

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
        if top_level {
            engine
                .append_message(
                    &session.id,
                    "assistant",
                    &text,
                    serde_json::to_value(assistant_tool_calls.as_deref().unwrap_or(&[]))?,
                    None,
                )
                .await?;
        }
        emit(
            engine,
            &session.id,
            spec,
            "agent.message",
            serde_json::json!({
                "text": text,
                "tool_calls": tool_calls,
            }),
        )
        .await?;
        conversation.push(ChatMessage {
            role: "assistant".to_string(),
            content: text.clone(),
            tool_calls: assistant_tool_calls,
            tool_call_id: None,
        });

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

        for (index, call) in tool_calls.into_iter().enumerate() {
            if !wait_for_resume(control).await {
                return Ok(LoopOutcome::Cancelled);
            }
            if control.is_cancelled() {
                return Ok(LoopOutcome::Cancelled);
            }
            if budget.exhausted() {
                return Ok(LoopOutcome::BudgetExceeded(BudgetStop::session(budget)));
            }
            let call_id = if call.id.is_empty() {
                format!("call_{}_{}", turn, index)
            } else {
                call.id.clone()
            };
            let input: serde_json::Value =
                serde_json::from_str(&call.arguments).unwrap_or_else(|_| serde_json::json!({}));
            let is_delegate = call.name == "delegate" && spec.allow_delegation;
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
            let registry_tool = config
                .tools
                .get(&call.name)
                .filter(|_| spec.role.allows_tool(spec.profile, &call.name));
            let requires_approval = !spec.auto_approve
                && if is_delegate
                    || is_delegate_many
                    || is_verdict
                    || is_team_plan
                    || is_run_plan
                    || is_todo
                    || is_mailbox_tool
                {
                    false
                } else {
                    registry_tool
                        .as_ref()
                        .map(|tool| tool.requires_approval())
                        .unwrap_or(false)
                };

            let mut approved = true;
            if requires_approval {
                let mut context = serde_json::json!({});
                agent_fields(&mut context, spec);
                let (approval, receiver) = engine
                    .request_approval(
                        &session.id,
                        &call_id,
                        &call.name,
                        input.clone(),
                        Some(context),
                    )
                    .await?;
                match wait_for_approval(receiver, control).await {
                    ApprovalOutcome::Approved => {}
                    ApprovalOutcome::Denied => approved = false,
                    ApprovalOutcome::TimedOut => {
                        approved = false;
                        let _ = engine.resolve_approval(&approval.id, false).await;
                    }
                    ApprovalOutcome::Cancelled => {
                        let _ = engine.resolve_approval(&approval.id, false).await;
                        return Ok(LoopOutcome::Cancelled);
                    }
                }
            }

            let output = if !approved {
                ToolOutput::error("The user denied this tool call.")
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
                                return Ok(LoopOutcome::BudgetExceeded(stop));
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
                        engine, config, session, project, control, budget, spec, &plan,
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
                                                WorkerFinish::Done { report, .. } => {
                                                    ToolOutput::ok(format!(
                                                        "Worker {} finished:\n{}",
                                                        child_name, report
                                                    ))
                                                }
                                                WorkerFinish::Cancelled => {
                                                    return Ok(LoopOutcome::Cancelled);
                                                }
                                                WorkerFinish::BudgetExceeded(stop) => {
                                                    if stop.is_session() {
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
                }),
            )
            .await?;
            if top_level {
                engine
                    .append_message(
                        &session.id,
                        "tool",
                        &output.content,
                        serde_json::json!([]),
                        Some(call_id.clone()),
                    )
                    .await?;
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
                .ok_or_else(|| anyhow!("agent not found in this session: {to}"))?;
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
            let artifact = engine
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
                .await?;
            Ok(ToolOutput::ok(format!(
                "artifact {} published: {}",
                artifact.id, artifact.name
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
                        "{}  {}  {}  {} bytes  {}",
                        artifact.id,
                        artifact.name,
                        artifact.kind,
                        artifact.content.len(),
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
            Ok(ToolOutput::ok(format!(
                "{} [{}]
{}",
                artifact.name, artifact.mime_type, artifact.content
            )))
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
        description: "Send a typed message to another agent in this session.".to_string(),
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
                "content": {"type": "string", "maxLength": 100000}
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
                if stop.is_session() {
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
                    WorkerFinish::Done { report, .. } => {
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
            Ok(LoopOutcome::Verdict(_)) | Ok(LoopOutcome::Cancelled) => {}
            Ok(LoopOutcome::BudgetExceeded(stop)) => {
                if !stop.is_session() {
                    reports.push(format!("{}: {}", child_spec.name, stop.summary()));
                }
            }
            Err(error) => {
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
    Ok(ParallelResult::Output(ToolOutput::ok(reports.join("\n\n"))))
}

fn validate_team_plan(plan: &TeamPlan, max_managers: usize) -> Result<()> {
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
        if id.is_empty() || id.len() > 40 {
            return Err(anyhow!("task id must be 1-40 chars"));
        }
        if !ids.insert(id.to_string()) {
            return Err(anyhow!("duplicate task id: {id}"));
        }
        if !plan
            .departments
            .iter()
            .any(|department| department.name == task.department)
        {
            return Err(anyhow!(
                "task '{}' references unknown department '{}'",
                id,
                task.department
            ));
        }
        if task.title.trim().is_empty() {
            return Err(anyhow!("task '{id}' needs a title"));
        }
        if task.kind.trim().is_empty() {
            return Err(anyhow!("task '{id}' needs a kind"));
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
                "- {} [{}] {} deps={} scope={}{}",
                task.id,
                task.department,
                task.title,
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

/// Select a ready batch that can safely run concurrently. Tasks with no scope
/// are treated as unrestricted and run alone. A task that overlaps an earlier
/// task in the batch stays pending for the next batch instead of being merged
/// into a conflicting write set.
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
) -> Result<PlanRunResult> {
    let mut manager_specs: std::collections::HashMap<String, AgentSpec> =
        std::collections::HashMap::new();

    let mut statuses = plan
        .tasks
        .iter()
        .map(|task| (task.id.clone(), "pending".to_string()))
        .collect::<std::collections::HashMap<_, _>>();
    let mut remaining = plan.tasks.clone();
    let mut reports = Vec::new();

    while !remaining.is_empty() {
        if control.is_cancelled() {
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
            }
            return Ok(PlanRunResult::Cancelled);
        }
        if budget.exhausted() {
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
            }
            return Ok(PlanRunResult::BudgetExceeded(BudgetStop::session(budget)));
        }
        let ready = remaining
            .iter()
            .filter(|task| {
                task.deps.iter().all(|dep| {
                    statuses
                        .get(dep)
                        .is_some_and(|status| status == "completed")
                })
            })
            .cloned()
            .collect::<Vec<_>>();
        if ready.is_empty() {
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
                        "reason": "dependency not completed",
                    }),
                )
                .await?;
            }
            break;
        }
        let capacity = config
            .max_workers
            .max(1)
            .min(budget.available(AgentRole::Worker));
        if capacity == 0 {
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
            }
            break;
        }
        let batch = select_plan_batch(ready, capacity);
        let batch_ids = batch
            .iter()
            .map(|task| task.id.clone())
            .collect::<std::collections::HashSet<_>>();

        let mut prepared = Vec::new();
        let mut worker_slots = Vec::new();
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
            emit(
                engine,
                &session.id,
                &worker_spec,
                "plan_task.started",
                serde_json::json!({
                    "task_id": task.id,
                    "task_status": "running",
                    "department": task.department,
                    "title": task.title,
                }),
            )
            .await?;
            statuses.insert(task.id.clone(), "running".to_string());
            let objective = format!(
                "Execute plan task {}: {}. Department: {} ({}). Acceptance: {}. \
                 Work only in your task worktree and return a concise summary.",
                task.id,
                task.title,
                task.department,
                manager_spec.title.as_deref().unwrap_or(&manager_spec.name),
                acceptance
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
                    false,
                )
            });
        let results = futures_util::future::join_all(futures).await;

        for (index, result) in results.into_iter().enumerate() {
            let (task, worker_spec, manager_spec, objective, acceptance, _) = &prepared[index];
            match result {
                Ok(LoopOutcome::Final(summary)) => {
                    let summary = if summary.trim().is_empty() {
                        "The worker completed without a summary.".to_string()
                    } else {
                        summary
                    };
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
                            for task in remaining
                                .iter()
                                .filter(|task| !batch_ids.contains(&task.id))
                            {
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
                            }
                            return Ok(PlanRunResult::Cancelled);
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
                                    "reason": "token budget exhausted",
                                }),
                            )
                            .await?;
                            for task in remaining
                                .iter()
                                .filter(|task| !batch_ids.contains(&task.id))
                            {
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
                            }
                            return Ok(PlanRunResult::BudgetExceeded(stop));
                        }
                    }
                }
                Ok(LoopOutcome::Cancelled) => {
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
                        }),
                    )
                    .await?;
                    return Ok(PlanRunResult::Cancelled);
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
                    if stop.is_session() {
                        return Ok(PlanRunResult::BudgetExceeded(stop));
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
                    reports.push(format!("{} [failed]: {error}", task.id));
                }
            }
        }

        remaining.retain(|task| !batch_ids.contains(&task.id));
    }

    let completed = statuses
        .values()
        .filter(|status| status.as_str() == "completed")
        .count();
    let failed = statuses
        .values()
        .filter(|status| matches!(status.as_str(), "failed" | "blocked" | "cancelled"))
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
         Do not claim success without evidence.",
        project.name, project.root, session.goal, worktree, spec.branch, profile_note, scope_note
    );
    match role {
        AgentRole::Leader => format!(
            "{base}\n\
             You are VerlinPark's Leader agent, {}. Keep a short plan with todo_write. \
             For a goal that spans multiple areas, delegate department-level objectives to \
             Managers with delegate(role=\"manager\"). Give every Manager a distinct title \
             (for example Discovery, Implementation, Quality) via the title field; each title \
             defines that Manager's department role. Managers will split work and delegate to \
             Workers; Workers may use Labour. Do not delegate directly to Workers or Labour. \
             For a multi-department plan, call deliberate_team_plan first so Managers can \
             review peer positions and agree on handoffs; then call run_team_plan to execute. \
             Do not manually delegate the planned Managers unless you intentionally override \
             the plan. \
             You can still inspect and edit files yourself for small tasks. \
             Review summaries, verify the final diff, and report the result.",
            spec.name
        ),
        AgentRole::Manager => format!(
            "{base}\n\
             You are the {} Manager, reporting to {}. Objective: {objective}. \
             You may read and search, but you must not edit files or run shell commands. \
             Split the objective into focused Worker tasks. For independent tasks, use \
             delegate_many(tasks=[...]) with non-overlapping write_scope values so Workers \
             run in parallel; otherwise use delegate(role=\"worker\"). Review the returned \
             summaries, resolve gaps, then return a concise consolidated result. \
             Do not delegate to Managers or Labour.",
            spec.title.as_deref().unwrap_or(spec.name.as_str()),
            spec.parent_name.as_deref().unwrap_or("the Leader")
        ),
        AgentRole::Worker => format!(
            "{base}\n\
             You are Worker {}, reporting to a Manager. Task: {objective}. \
             You own implementation in the worktree. Inspect first, make focused edits, \
             and run tests. Delegate small research or verification subtasks to Labour with \
             delegate(role=\"labour\") when useful. Return a concise summary with evidence. \
             Do not delegate to Managers or Workers.",
            spec.name
        ),
        AgentRole::Verifier => format!(
            "{base}\n\
             You are Verifier {}, reporting to a Manager. Task under review: {objective}. \
             You are read-only: do not edit files. Inspect the task branch/diff and run \
             relevant tests or checks with shell. Approve only when the changes meet the \
             objective and evidence is sufficient. Then call submit_verdict with \
             approved and summary. You cannot delegate.",
            spec.name
        ),
        AgentRole::Labour => format!(
            "{base}\n\
             You are Labour {}, reporting to a Worker. Subtask: {objective}. \
             Execute it directly in the worktree and return a concise report. \
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

async fn load_conversation(engine: &Engine, session_id: &str) -> Result<Vec<ChatMessage>> {
    let messages: Vec<StoredMessage> = engine.list_messages(session_id).await?;
    Ok(messages
        .into_iter()
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
        .collect())
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

async fn wait_for_approval(
    receiver: oneshot::Receiver<bool>,
    control: &RunControl,
) -> ApprovalOutcome {
    if control.is_cancelled() {
        return ApprovalOutcome::Cancelled;
    }
    let mut cancel_rx = control.subscribe();
    tokio::select! {
        _ = cancel_rx.changed() => ApprovalOutcome::Cancelled,
        result = timeout(Duration::from_secs(600), receiver) => match result {
            Ok(Ok(true)) => ApprovalOutcome::Approved,
            Ok(Ok(false)) | Ok(Err(_)) => ApprovalOutcome::Denied,
            Err(_) => ApprovalOutcome::TimedOut,
        },
    }
}

fn truncate(content: &str) -> String {
    const MAX: usize = 8_000;
    if content.len() > MAX {
        format!("{}… (truncated)", &content[..MAX])
    } else {
        content.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::collections::VecDeque;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_providers::{EventStream, ProviderError};
    use vp_store::Store;

    struct QueueProvider {
        responses: Mutex<VecDeque<Vec<StreamEvent>>>,
    }

    #[async_trait]
    impl Provider for QueueProvider {
        fn id(&self) -> &str {
            "queue"
        }

        async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
            let events = self
                .responses
                .lock()
                .unwrap()
                .pop_front()
                .unwrap_or_else(|| vec![StreamEvent::TextDelta("done".into())]);
            Ok(Box::pin(futures_util::stream::iter(
                events.into_iter().map(Ok),
            )))
        }
    }

    fn test_service(dir: &TempDir) -> (Engine, AgentService, vp_protocol::types::Project) {
        let provider = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_read".into(),
                        name: "read_file".into(),
                        arguments: r#"{"path":"README.md"}"#.into(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("I read the README.".into()),
                    StreamEvent::Done,
                ],
            ])),
        });
        test_service_with_provider(dir, provider)
    }

    fn test_service_with_provider(
        dir: &TempDir,
        provider: Arc<dyn Provider>,
    ) -> (Engine, AgentService, vp_protocol::types::Project) {
        test_service_with_provider_cap(dir, provider, 4)
    }

    fn test_service_with_provider_cap(
        dir: &TempDir,
        provider: Arc<dyn Provider>,
        max_active_sessions: usize,
    ) -> (Engine, AgentService, vp_protocol::types::Project) {
        let store = Store::open(dir.path()).unwrap();
        let engine = Engine::new(store);
        let project_dir = dir.path().join("repo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("README.md"), "hello from test\n").unwrap();
        init_repo(&project_dir);
        let project = engine
            .store()
            .add_project(project_dir.to_str().unwrap(), Some("repo"))
            .unwrap();
        let config = AgentConfig {
            provider,
            provider_name: "queue".into(),
            model: "test".into(),
            role_models: RoleModels::default(),
            base_url: None,
            max_turns: 4,
            max_tokens: Some(1024),
            max_managers: 5,
            max_workers: 10,
            max_labour: 20,
            integration_command: None,
            tools: ToolRegistry::default_tools(),
            max_active_sessions,
            scope_locks: Arc::new(ScopeLocks::default()),
            capacity: Arc::new(CapacityController::new(
                global_worker_cap(),
                global_labour_cap(),
            )),
        };
        let service = AgentService::configured(engine.clone(), config);
        (engine, service, project)
    }

    fn set_worker_cap(service: &AgentService, max_workers: usize) {
        let mut config = service
            .config
            .read()
            .unwrap()
            .as_ref()
            .expect("test service is configured")
            .clone();
        config.max_workers = max_workers;
        *service.config.write().unwrap() = Some(config);
    }

    fn set_integration_command(service: &AgentService, command: Option<&str>) {
        let mut config = service
            .config
            .read()
            .unwrap()
            .as_ref()
            .expect("test service is configured")
            .clone();
        config.integration_command = command.map(ToOwned::to_owned);
        *service.config.write().unwrap() = Some(config);
    }

    struct SlowProvider;

    #[async_trait]
    impl Provider for SlowProvider {
        fn id(&self) -> &str {
            "slow"
        }

        async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
            Ok(Box::pin(futures_util::stream::once(async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                Ok(StreamEvent::TextDelta("late".into()))
            })))
        }
    }

    #[test]
    fn configures_provider_without_persisting_secrets() {
        let dir = TempDir::new().unwrap();
        let engine = Engine::new(Store::open(dir.path()).unwrap());
        let service = AgentService::unconfigured(engine);
        assert!(!service.info().configured);
        let info = service
            .configure(ConfigureAgentParams {
                provider: "openai-compatible".to_string(),
                base_url: Some("http://127.0.0.1:1234/v1/".to_string()),
                model: "local-model".to_string(),
                role_models: None,
                api_key: Some("secret".to_string()),
                max_turns: None,
                max_tokens: None,
                max_managers: None,
                max_workers: None,
                max_labour: None,
            })
            .unwrap();
        assert!(info.configured);
        assert_eq!(info.model.as_deref(), Some("local-model"));
        assert_eq!(info.base_url.as_deref(), Some("http://127.0.0.1:1234/v1"));

        let error = service
            .configure(ConfigureAgentParams {
                provider: "openai-compatible".to_string(),
                base_url: None,
                model: "   ".to_string(),
                role_models: None,
                api_key: None,
                max_turns: None,
                max_tokens: None,
                max_managers: None,
                max_workers: None,
                max_labour: None,
            })
            .unwrap_err();
        assert!(error.to_string().contains("model is required"));
    }

    struct RoutingProvider {
        active: std::sync::atomic::AtomicUsize,
        max_active: std::sync::atomic::AtomicUsize,
    }

    impl RoutingProvider {
        fn new() -> Self {
            Self {
                active: std::sync::atomic::AtomicUsize::new(0),
                max_active: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl Provider for RoutingProvider {
        fn id(&self) -> &str {
            "routing"
        }

        async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
            let last_is_tool = request
                .messages
                .last()
                .is_some_and(|message| message.role == "tool");
            let user_text = request
                .messages
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .map(|message| message.content.clone())
                .unwrap_or_default();
            let events = if last_is_tool {
                if user_text.contains("parallel goal") {
                    vec![
                        StreamEvent::TextDelta("leader done".into()),
                        StreamEvent::Done,
                    ]
                } else {
                    vec![
                        StreamEvent::TextDelta("department done".into()),
                        StreamEvent::Done,
                    ]
                }
            } else if user_text.contains("parallel goal") {
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_manager".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "manager",
                            "title": "Implementation",
                            "objective": "department"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ]
            } else if user_text.contains("department") {
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_many".into(),
                        name: "delegate_many".into(),
                        arguments: serde_json::json!({
                            "tasks": [
                                {
                                    "name": "Worker A",
                                    "objective": "task A",
                                    "write_scope": ["src/a"]
                                },
                                {
                                    "name": "Worker B",
                                    "objective": "task B",
                                    "write_scope": ["src/b"]
                                }
                            ]
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ]
            } else {
                let current = self
                    .active
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                self.max_active
                    .fetch_max(current, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(100)).await;
                self.active
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                let text = if user_text.contains("task A") {
                    "A done"
                } else {
                    "B done"
                };
                vec![StreamEvent::TextDelta(text.into()), StreamEvent::Done]
            };
            Ok(Box::pin(futures_util::stream::iter(
                events.into_iter().map(Ok),
            )))
        }
    }

    struct PlanRoutingProvider {
        active: std::sync::atomic::AtomicUsize,
        max_active: std::sync::atomic::AtomicUsize,
        overlapping_scopes: bool,
    }

    impl PlanRoutingProvider {
        fn new() -> Self {
            Self::with_overlapping_scopes(false)
        }

        fn with_overlapping_scopes(overlapping_scopes: bool) -> Self {
            Self {
                active: std::sync::atomic::AtomicUsize::new(0),
                max_active: std::sync::atomic::AtomicUsize::new(0),
                overlapping_scopes,
            }
        }
    }

    #[async_trait]
    impl Provider for PlanRoutingProvider {
        fn id(&self) -> &str {
            "plan-routing"
        }

        async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
            let last_message = request.messages.last();
            let last_role = last_message.map(|message| message.role.as_str());
            let last_content = last_message
                .map(|message| message.content.clone())
                .unwrap_or_default();
            let user_text = request
                .messages
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .map(|message| message.content.clone())
                .unwrap_or_default();
            let events = if user_text.contains("Execute plan task T1")
                || user_text.contains("Execute plan task T2")
            {
                let current = self
                    .active
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1;
                self.max_active
                    .fetch_max(current, std::sync::atomic::Ordering::SeqCst);
                tokio::time::sleep(Duration::from_millis(80)).await;
                self.active
                    .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
                let text = if user_text.contains("T1") {
                    "T1 done"
                } else {
                    "T2 done"
                };
                vec![StreamEvent::TextDelta(text.into()), StreamEvent::Done]
            } else if last_role == Some("tool") {
                if last_content.contains("Plan complete") {
                    vec![
                        StreamEvent::TextDelta("leader done".into()),
                        StreamEvent::Done,
                    ]
                } else {
                    vec![
                        StreamEvent::ToolCall(ToolCall {
                            id: "run_plan".into(),
                            name: "run_team_plan".into(),
                            arguments: "{}".into(),
                        }),
                        StreamEvent::Done,
                    ]
                }
            } else {
                let plan = serde_json::json!({
                    "goal": "auto plan",
                    "departments": [{
                        "name": "research",
                        "manager_title": "Sources",
                        "profile": "research",
                        "objective": "collect sources"
                    }],
                    "tasks": [
                        {"id": "T1", "department": "research", "title": "Task one", "kind": "research", "deps": [], "acceptance": ["done"], "write_scope": [if self.overlapping_scopes { "src/shared" } else { "src/t1" }]},
                        {"id": "T2", "department": "research", "title": "Task two", "kind": "research", "deps": [], "acceptance": ["done"], "write_scope": [if self.overlapping_scopes { "src/shared" } else { "src/t2" }]}
                    ]
                });
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "submit_plan".into(),
                        name: "submit_team_plan".into(),
                        arguments: plan.to_string(),
                    }),
                    StreamEvent::Done,
                ]
            };
            Ok(Box::pin(futures_util::stream::iter(
                events.into_iter().map(Ok),
            )))
        }
    }

    struct BudgetPlanProvider {
        department_tokens: Option<u64>,
    }

    #[async_trait]
    impl Provider for BudgetPlanProvider {
        fn id(&self) -> &str {
            "budget-plan"
        }

        async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
            let last = request.messages.last();
            let last_content = last
                .map(|message| message.content.clone())
                .unwrap_or_default();
            let user_text = request
                .messages
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .map(|message| message.content.clone())
                .unwrap_or_default();
            let events = if user_text.contains("Execute plan task") {
                vec![
                    StreamEvent::TextDelta("worker should not run".into()),
                    StreamEvent::Done,
                ]
            } else if last_content.contains("Plan complete") {
                vec![
                    StreamEvent::TextDelta("leader done".into()),
                    StreamEvent::Done,
                ]
            } else if last.map(|message| message.role.as_str()) == Some("tool") {
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "run_plan".into(),
                        name: "run_team_plan".into(),
                        arguments: "{}".into(),
                    }),
                    StreamEvent::Done,
                ]
            } else {
                let mut plan = serde_json::json!({
                    "goal": "budget plan",
                    "departments": [{
                        "name": "impl",
                        "manager_title": "Implementation",
                        "profile": "coding",
                        "objective": "build"
                    }],
                    "tasks": [
                        {"id": "T1", "department": "impl", "title": "One", "kind": "coding", "acceptance": ["done"], "write_scope": ["src/t1"], "token_budget": 1},
                        {"id": "T2", "department": "impl", "title": "Two", "kind": "coding", "acceptance": ["done"], "write_scope": ["src/t2"], "token_budget": 1}
                    ]
                });
                if let Some(tokens) = self.department_tokens {
                    plan["departments"][0]["token_budget"] = serde_json::json!(tokens);
                    plan["tasks"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("token_budget");
                    plan["tasks"][1]
                        .as_object_mut()
                        .unwrap()
                        .remove("token_budget");
                }
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "submit_plan".into(),
                        name: "submit_team_plan".into(),
                        arguments: plan.to_string(),
                    }),
                    StreamEvent::Done,
                ]
            };
            Ok(Box::pin(futures_util::stream::iter(
                events.into_iter().map(Ok),
            )))
        }
    }

    struct RoundtableProvider;

    #[async_trait]
    impl Provider for RoundtableProvider {
        fn id(&self) -> &str {
            "roundtable"
        }

        async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
            let last = request.messages.last();
            let last_content = last
                .map(|message| message.content.clone())
                .unwrap_or_default();
            let user_text = request
                .messages
                .iter()
                .rev()
                .find(|message| message.role == "user")
                .map(|message| message.content.clone())
                .unwrap_or_default();
            let events = if user_text.contains("Manager roundtable, round 1") {
                let title = if user_text.contains("Sources") {
                    "Sources"
                } else {
                    "Implementation"
                };
                vec![
                    StreamEvent::TextDelta(format!("{title} r1 position")),
                    StreamEvent::Done,
                ]
            } else if user_text.contains("Manager roundtable, round 2") {
                let title = if user_text.contains("Sources") {
                    "Sources"
                } else {
                    "Implementation"
                };
                vec![
                    StreamEvent::TextDelta(format!("{title} r2 decision")),
                    StreamEvent::Done,
                ]
            } else if last.map(|message| message.role.as_str()) == Some("tool")
                && last_content.contains("Team plan recorded")
            {
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "deliberate".into(),
                        name: "deliberate_team_plan".into(),
                        arguments: serde_json::json!({"rounds": 2}).to_string(),
                    }),
                    StreamEvent::Done,
                ]
            } else if last.map(|message| message.role.as_str()) == Some("tool") {
                vec![
                    StreamEvent::TextDelta("deliberation complete".into()),
                    StreamEvent::Done,
                ]
            } else {
                let plan = serde_json::json!({
                    "goal": "deliberate the plan",
                    "departments": [
                        {"name": "sources", "manager_title": "Sources", "profile": "research", "objective": "collect sources"},
                        {"name": "implementation", "manager_title": "Implementation", "profile": "coding", "objective": "implement"}
                    ],
                    "tasks": [
                        {"id": "T1", "department": "sources", "title": "Research", "kind": "research", "deps": [], "acceptance": ["sources"]},
                        {"id": "T2", "department": "implementation", "title": "Build", "kind": "coding", "deps": ["T1"], "acceptance": ["tests"]}
                    ]
                });
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "submit_plan".into(),
                        name: "submit_team_plan".into(),
                        arguments: plan.to_string(),
                    }),
                    StreamEvent::Done,
                ]
            };
            Ok(Box::pin(futures_util::stream::iter(
                events.into_iter().map(Ok),
            )))
        }
    }

    fn init_repo(path: &Path) {
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

    #[tokio::test]
    async fn runs_tool_call_and_final_message() {
        let dir = TempDir::new().unwrap();
        let (engine, service, project) = test_service(&dir);
        let session = engine
            .create_session(project.id, "inspect the repo".into(), None)
            .await
            .unwrap();
        service
            .start(&session.id, "inspect the repo")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let events = engine
            .list_events(session.id.clone(), 0, 100)
            .await
            .unwrap();
        assert!(events.iter().any(|event| event.kind == "tool.finished"));
        assert!(events.iter().any(|event| event.kind == "agent.message"));
        let messages = engine.list_messages(&session.id).await.unwrap();
        assert!(messages.iter().any(|message| message.role == "assistant"));
    }
    #[tokio::test]
    async fn writes_a_persisted_plan() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_todo".into(),
                        name: "todo_write".into(),
                        arguments: serde_json::json!({
                            "items": [
                                {"title": "Inspect", "status": "completed"},
                                {"title": "Implement", "status": "in_progress"}
                            ]
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("still working".into()),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "plan the work".into(), None)
            .await
            .unwrap();
        service.start(&session.id, "plan the work").await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let todos = engine.list_todos(&session.id).await.unwrap();
        assert_eq!(todos.len(), 2);
        assert_eq!(todos[1].status, "in_progress");
        let events = engine.list_events(session.id, 0, 100).await.unwrap();
        assert!(events.iter().any(|event| event.kind == "todo.updated"));
    }

    #[tokio::test]
    async fn delegates_through_four_tiers() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_manager".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "manager",
                            "title": "Discovery",
                            "objective": "handle the department"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_worker".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "worker",
                            "objective": "inspect the readme"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_labour".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "labour",
                            "objective": "read the readme"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_read".into(),
                        name: "read_file".into(),
                        arguments: r#"{"path":"README.md"}"#.into(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("labour done".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("worker done".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("manager done".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("leader done".into()),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "delegate the work".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service
            .start(&session.id, "delegate the work")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(400)).await;

        let agents = engine.list_agents(&session.id).await.unwrap();
        let roles = agents
            .iter()
            .map(|agent| agent.role.as_str())
            .collect::<Vec<_>>();
        assert!(roles.contains(&"leader"));
        assert!(roles.contains(&"manager"));
        assert!(roles.contains(&"worker"));
        assert!(roles.contains(&"labour"));
        assert!(
            agents
                .iter()
                .filter(|agent| agent.role != "leader")
                .all(|agent| agent.status == "idle")
        );

        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "agent.spawned")
                .count(),
            3
        );
        assert!(
            events.iter().any(
                |event| event.kind == "agent.message" && event.payload["text"] == "leader done"
            )
        );
        let sessions = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "idle");
    }

    #[tokio::test]
    async fn integrates_worker_changes_into_the_session_branch() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_manager".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "manager",
                            "title": "Implementation",
                            "objective": "deliver the file"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_worker".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "worker",
                            "objective": "write worker.txt",
                            "write_scope": ["worker.txt"]
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_write".into(),
                        name: "write_file".into(),
                        arguments: serde_json::json!({
                            "path": "worker.txt",
                            "content": "from worker\n"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("worker wrote".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_verdict".into(),
                        name: "submit_verdict".into(),
                        arguments: serde_json::json!({
                            "approved": true,
                            "summary": "worker.txt exists and matches the task"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("manager done".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("leader done".into()),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "integrate".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();

        let approver_engine = engine.clone();
        let approver_session = session.id.clone();
        let approver = tokio::spawn(async move {
            for _ in 0..100 {
                if let Ok(approvals) = approver_engine
                    .list_approvals(Some(approver_session.clone()))
                    .await
                {
                    for approval in approvals.iter().filter(|a| a.status == "pending") {
                        let _ = approver_engine.resolve_approval(&approval.id, true).await;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });

        let worktree = session.worktree.clone().unwrap();
        service.start(&session.id, "integrate").await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if std::fs::read_to_string(std::path::Path::new(&worktree).join("worker.txt"))
                .ok()
                .as_deref()
                == Some("from worker\n")
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "worker changes were not integrated"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let _ = wait_until_idle(&engine, &session).await;
        approver.abort();
        assert_eq!(
            std::fs::read_to_string(std::path::Path::new(&worktree).join("worker.txt")).unwrap(),
            "from worker\n"
        );
        let diff = engine.session_diff(&session.id).await.unwrap();
        assert!(diff.status.is_empty());
        assert!(diff.patch.contains("+from worker"));
        let agents = engine.list_agents(&session.id).await.unwrap();
        let worker = agents.iter().find(|agent| agent.role == "worker").unwrap();
        assert_eq!(worker.status, "idle");
        assert!(
            agents
                .iter()
                .any(|agent| agent.role == "verifier" && agent.status == "idle")
        );
        assert!(
            worker
                .branch
                .as_deref()
                .is_some_and(|branch| branch.starts_with("verlinpark/"))
        );
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "agent.finished"
                && event.payload["agent_role"] == "worker"
                && event.payload["merged"] == true
        }));
        let queue = engine.list_merge_queue(&session.id).await.unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].status, "merged");
        assert_eq!(queue[0].agent_name, "Worker 1");
        assert!(
            events
                .iter()
                .any(|event| event.kind == "merge_queue.enqueued")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == "integrator.started")
        );
        assert!(events.iter().any(
            |event| event.kind == "integrator.finished" && event.payload["status"] == "merged"
        ));
        let sessions = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "idle");
    }

    #[tokio::test]
    async fn records_merge_conflicts_in_the_queue() {
        let dir = TempDir::new().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let engine = Engine::new(store);
        let project_dir = dir.path().join("repo");
        std::fs::create_dir_all(&project_dir).unwrap();
        std::fs::write(project_dir.join("README.md"), "initial\n").unwrap();
        init_repo(&project_dir);
        let project = engine
            .add_project(
                project_dir.to_string_lossy().to_string(),
                Some("repo".into()),
            )
            .await
            .unwrap();
        let session = engine
            .create_session(project.id.clone(), "conflict".into(), None)
            .await
            .unwrap();
        let branch = session.branch.clone().unwrap();
        let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
        let task = engine
            .create_task_worktree(&project.root, &project.id, &session.id, "T1", &branch)
            .await
            .unwrap();
        std::fs::write(task.path.join("README.md"), "worker\n").unwrap();
        let commit = worktree::commit_all(&task.path, "worker change")
            .unwrap()
            .unwrap();
        std::fs::write(session_worktree.join("README.md"), "session\n").unwrap();
        worktree::commit_all(&session_worktree, "session change").unwrap();

        let spec = AgentSpec {
            id: "worker-test".into(),
            role: AgentRole::Worker,
            name: "Worker test".into(),
            depth: 2,
            parent_name: Some("Manager".into()),
            title: None,
            task_id: Some("T1".into()),
            department: Some("impl".into()),
            profile: AgentProfile::Coding,
            allow_delegation: true,
            auto_approve: false,
            worktree: task.path.clone(),
            branch: task.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: branch,
            write_scope: vec!["README.md".into()],
            base_commit: task.base_commit.clone(),
        };
        let error = integrate_worker(&engine, &session, &spec, &commit, "verified", None)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("merge conflict"), "{error}");

        let queue = engine.list_merge_queue(&session.id).await.unwrap();
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].status, "conflict");
        assert_eq!(
            std::fs::read_to_string(session_worktree.join("README.md")).unwrap(),
            "session\n"
        );
    }

    #[tokio::test]
    async fn retries_a_conflicted_merge_after_rebase() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::new()),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id.clone(), "retry conflict".into(), None)
            .await
            .unwrap();
        let worker = engine
            .create_agent(&session.id, "worker", "Worker retry", None, None, "test")
            .await
            .unwrap();
        let branch = session.branch.clone().unwrap();
        let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
        let task = engine
            .create_task_worktree(&project.root, &project.id, &session.id, "T1", &branch)
            .await
            .unwrap();
        engine
            .set_agent_worktree(&worker.id, &task.path.to_string_lossy(), &task.branch)
            .await
            .unwrap();
        std::fs::write(task.path.join("README.md"), "worker\n").unwrap();
        let commit = worktree::commit_all(&task.path, "worker change")
            .unwrap()
            .unwrap();
        std::fs::write(session_worktree.join("README.md"), "session\n").unwrap();
        worktree::commit_all(&session_worktree, "session change").unwrap();

        let spec = AgentSpec {
            id: worker.id.clone(),
            role: AgentRole::Worker,
            name: worker.name.clone(),
            depth: 2,
            parent_name: None,
            title: None,
            task_id: Some("T1".into()),
            department: Some("impl".into()),
            profile: AgentProfile::Coding,
            allow_delegation: true,
            auto_approve: false,
            worktree: task.path.clone(),
            branch: task.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: branch,
            write_scope: vec!["README.md".into()],
            base_commit: task.base_commit.clone(),
        };
        integrate_worker(&engine, &session, &spec, &commit, "verified", None)
            .await
            .unwrap_err();
        let queue = engine.list_merge_queue(&session.id).await.unwrap();
        assert_eq!(queue[0].status, "conflict");

        // Drop the conflicting session commit, then retry: the worker branch can
        // be rebased and merged cleanly.
        worktree::reset_hard(&session_worktree, &task.base_commit).unwrap();
        let retried = service.retry_merge(&queue[0].id).await.unwrap();
        assert_eq!(retried.status, "merged");
        assert_eq!(retried.attempt, 2);
        assert_eq!(
            std::fs::read_to_string(session_worktree.join("README.md")).unwrap(),
            "worker\n"
        );
    }

    #[tokio::test]
    async fn auto_resolves_a_conflicted_merge_with_an_agent() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "resolve_write".into(),
                        name: "write_file".into(),
                        arguments: serde_json::json!({
                            "path": "README.md",
                            "content": "resolved\n"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("resolved the README conflict".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "resolve_verdict".into(),
                        name: "submit_verdict".into(),
                        arguments: serde_json::json!({
                            "approved": true,
                            "summary": "conflict resolved cleanly"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id.clone(), "resolve conflict".into(), None)
            .await
            .unwrap();
        let worker = engine
            .create_agent(&session.id, "worker", "Worker resolve", None, None, "test")
            .await
            .unwrap();
        let branch = session.branch.clone().unwrap();
        let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
        let task = engine
            .create_task_worktree(&project.root, &project.id, &session.id, "T1", &branch)
            .await
            .unwrap();
        engine
            .set_agent_worktree(&worker.id, &task.path.to_string_lossy(), &task.branch)
            .await
            .unwrap();
        std::fs::write(task.path.join("README.md"), "worker\n").unwrap();
        let commit = worktree::commit_all(&task.path, "worker change")
            .unwrap()
            .unwrap();
        std::fs::write(session_worktree.join("README.md"), "session\n").unwrap();
        worktree::commit_all(&session_worktree, "session change").unwrap();

        let spec = AgentSpec {
            id: worker.id.clone(),
            role: AgentRole::Worker,
            name: worker.name.clone(),
            depth: 2,
            parent_name: None,
            title: None,
            task_id: Some("T1".into()),
            department: Some("impl".into()),
            profile: AgentProfile::Coding,
            allow_delegation: true,
            auto_approve: false,
            worktree: task.path.clone(),
            branch: task.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: branch,
            write_scope: vec!["README.md".into()],
            base_commit: task.base_commit.clone(),
        };
        integrate_worker(&engine, &session, &spec, &commit, "verified", None)
            .await
            .unwrap_err();
        let queue = engine.list_merge_queue(&session.id).await.unwrap();
        assert_eq!(queue[0].status, "conflict");

        let resolved = service.resolve_merge(&queue[0].id).await.unwrap();
        assert_eq!(resolved.status, "merged", "{:?}", resolved.detail);
        assert_eq!(resolved.attempt, 2);
        assert_eq!(
            std::fs::read_to_string(session_worktree.join("README.md")).unwrap(),
            "resolved\n"
        );
        let agents = engine.list_agents(&session.id).await.unwrap();
        assert!(
            agents
                .iter()
                .any(|agent| { agent.name == "Conflict Resolver" && agent.status == "idle" })
        );
        let sessions = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "idle");
    }

    #[tokio::test]
    async fn auto_resolver_policy_resolves_pending_conflicts() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "auto_write".into(),
                        name: "write_file".into(),
                        arguments: serde_json::json!({
                            "path": "README.md",
                            "content": "resolved by policy\n"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("policy resolver done".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "auto_verdict".into(),
                        name: "submit_verdict".into(),
                        arguments: serde_json::json!({
                            "approved": true,
                            "summary": "policy resolution verified"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id.clone(), "policy resolve".into(), None)
            .await
            .unwrap();
        let worker = engine
            .create_agent(&session.id, "worker", "Worker policy", None, None, "test")
            .await
            .unwrap();
        let branch = session.branch.clone().unwrap();
        let session_worktree = PathBuf::from(session.worktree.clone().unwrap());
        let task = engine
            .create_task_worktree(&project.root, &project.id, &session.id, "T1", &branch)
            .await
            .unwrap();
        engine
            .set_agent_worktree(&worker.id, &task.path.to_string_lossy(), &task.branch)
            .await
            .unwrap();
        std::fs::write(task.path.join("README.md"), "worker\n").unwrap();
        let commit = worktree::commit_all(&task.path, "worker change")
            .unwrap()
            .unwrap();
        std::fs::write(session_worktree.join("README.md"), "session\n").unwrap();
        worktree::commit_all(&session_worktree, "session change").unwrap();

        let spec = AgentSpec {
            id: worker.id.clone(),
            role: AgentRole::Worker,
            name: worker.name.clone(),
            depth: 2,
            parent_name: None,
            title: None,
            task_id: Some("T1".into()),
            department: Some("impl".into()),
            profile: AgentProfile::Coding,
            allow_delegation: true,
            auto_approve: false,
            worktree: task.path.clone(),
            branch: task.branch.clone(),
            session_worktree: session_worktree.clone(),
            session_branch: branch,
            write_scope: vec!["README.md".into()],
            base_commit: task.base_commit.clone(),
        };
        integrate_worker(&engine, &session, &spec, &commit, "verified", None)
            .await
            .unwrap_err();
        let queue = engine.list_merge_queue(&session.id).await.unwrap();
        assert_eq!(queue[0].status, "conflict");
        engine
            .set_integration_policy(
                &project.id,
                vp_protocol::types::IntegrationPolicy {
                    auto_resolve: true,
                    max_auto_attempts: 1,
                    max_resolve_tokens: Some(200_000),
                },
            )
            .await
            .unwrap();
        engine
            .update_session_status(&session.id, "idle")
            .await
            .unwrap();

        service.auto_resolve_project(&project.id).await;

        let resolved = engine
            .get_merge_queue_entry(&queue[0].id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(resolved.status, "merged", "{:?}", resolved.detail);
        assert_eq!(resolved.attempt, 2);
        assert_eq!(engine.auto_resolve_attempts(&queue[0].id).await.unwrap(), 1);
        assert_eq!(
            std::fs::read_to_string(session_worktree.join("README.md")).unwrap(),
            "resolved by policy\n"
        );
        let events = engine
            .list_events(session.id.clone(), 0, 300)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == "merge_queue.auto_resolve_started")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == "merge_queue.auto_resolve_finished")
        );
    }

    #[tokio::test]
    async fn rolls_back_a_merge_when_integration_checks_fail() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_manager".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "manager",
                            "title": "Implementation",
                            "objective": "deliver the file"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_worker".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "worker",
                            "objective": "write worker.txt",
                            "write_scope": ["worker.txt"]
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_write".into(),
                        name: "write_file".into(),
                        arguments: serde_json::json!({
                            "path": "worker.txt",
                            "content": "from worker\n"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("worker wrote".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_verdict".into(),
                        name: "submit_verdict".into(),
                        arguments: serde_json::json!({
                            "approved": true,
                            "summary": "worker.txt exists and matches the task"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("manager done".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("leader done".into()),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        set_integration_command(&service, Some("exit 7"));
        let session = engine
            .create_session(project.id, "integration failure".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();

        let approver_engine = engine.clone();
        let approver_session = session.id.clone();
        let approver = tokio::spawn(async move {
            for _ in 0..100 {
                if let Ok(approvals) = approver_engine
                    .list_approvals(Some(approver_session.clone()))
                    .await
                {
                    for approval in approvals.iter().filter(|a| a.status == "pending") {
                        let _ = approver_engine.resolve_approval(&approval.id, true).await;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });

        service
            .start(&session.id, "integration failure")
            .await
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        let queue = loop {
            let queue = engine.list_merge_queue(&session.id).await.unwrap();
            if queue.len() == 1 && queue[0].status == "failed" {
                break queue;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "integration failure was not recorded: {queue:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        };
        approver.abort();

        let worktree = session.worktree.clone().unwrap();
        assert!(
            !std::path::Path::new(&worktree).join("worker.txt").exists(),
            "a failed integration check must roll the merge back"
        );
        assert_eq!(queue.len(), 1);
        assert!(
            queue[0]
                .detail
                .as_deref()
                .is_some_and(|detail| detail.contains("integration command exited"))
        );
        let agents = engine.list_agents(&session.id).await.unwrap();
        let worker = agents.iter().find(|agent| agent.role == "worker").unwrap();
        assert_eq!(worker.status, "failed");
        let events = engine
            .list_events(session.id.clone(), 0, 300)
            .await
            .unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "integrator.finished" && event.payload["status"] == "failed"
        }));
    }

    #[tokio::test]
    async fn blocks_integration_when_the_verifier_rejects() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_manager".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "manager",
                            "title": "Quality",
                            "objective": "review the change"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_worker".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "worker",
                            "objective": "write rejected.txt",
                            "write_scope": ["rejected.txt"]
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_write".into(),
                        name: "write_file".into(),
                        arguments: serde_json::json!({
                            "path": "rejected.txt",
                            "content": "not ready\n"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("worker wrote".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_verdict".into(),
                        name: "submit_verdict".into(),
                        arguments: serde_json::json!({
                            "approved": false,
                            "summary": "the acceptance test fails"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("manager done".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("leader done".into()),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "verify".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        let approver_engine = engine.clone();
        let approver_session = session.id.clone();
        let approver = tokio::spawn(async move {
            for _ in 0..100 {
                if let Ok(approvals) = approver_engine
                    .list_approvals(Some(approver_session.clone()))
                    .await
                {
                    for approval in approvals.iter().filter(|a| a.status == "pending") {
                        let _ = approver_engine.resolve_approval(&approval.id, true).await;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        service.start(&session.id, "verify").await.unwrap();
        tokio::time::sleep(Duration::from_millis(900)).await;
        approver.abort();

        let worktree = session.worktree.clone().unwrap();
        assert!(
            !std::path::Path::new(&worktree)
                .join("rejected.txt")
                .exists()
        );
        let diff = engine.session_diff(&session.id).await.unwrap();
        assert!(diff.patch.is_empty());
        let agents = engine.list_agents(&session.id).await.unwrap();
        let worker = agents.iter().find(|agent| agent.role == "worker").unwrap();
        assert_eq!(worker.status, "changes_requested");
        let verifier = agents
            .iter()
            .find(|agent| agent.role == "verifier")
            .unwrap();
        assert_eq!(verifier.status, "idle");
        let events = engine
            .list_events(session.id.clone(), 0, 300)
            .await
            .unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "agent.finished"
                && event.payload["agent_role"] == "worker"
                && event.payload["status"] == "changes_requested"
                && event.payload["approved"] == false
        }));
    }

    #[tokio::test]
    async fn rejects_a_verifier_that_modifies_tracked_files() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_manager".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "manager",
                            "title": "Quality",
                            "objective": "review the change"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_worker".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "worker",
                            "objective": "write verifier-target.txt",
                            "write_scope": ["verifier-target.txt"]
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_write".into(),
                        name: "write_file".into(),
                        arguments: serde_json::json!({
                            "path": "verifier-target.txt",
                            "content": "original\n"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("worker wrote".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_shell".into(),
                        name: "shell".into(),
                        arguments: serde_json::json!({
                            "command": "printf 'tampered\n' > verifier-target.txt"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_verdict".into(),
                        name: "submit_verdict".into(),
                        arguments: serde_json::json!({
                            "approved": true,
                            "summary": "looks good"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("manager done".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("leader done".into()),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "verifier-write".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        let approver_engine = engine.clone();
        let approver_session = session.id.clone();
        let approver = tokio::spawn(async move {
            for _ in 0..200 {
                if let Ok(approvals) = approver_engine
                    .list_approvals(Some(approver_session.clone()))
                    .await
                {
                    for approval in approvals.iter().filter(|a| a.status == "pending") {
                        let _ = approver_engine.resolve_approval(&approval.id, true).await;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        service.start(&session.id, "verifier-write").await.unwrap();
        let _ = wait_until_idle(&engine, &session).await;
        approver.abort();

        let agents = engine.list_agents(&session.id).await.unwrap();
        let worker = agents.iter().find(|agent| agent.role == "worker").unwrap();
        assert_eq!(worker.status, "changes_requested");
        let verifier = agents
            .iter()
            .find(|agent| agent.role == "verifier")
            .unwrap();
        assert_eq!(verifier.status, "failed");
        let verifier_worktree = PathBuf::from(verifier.worktree.clone().unwrap());
        assert!(
            worktree::tracked_changes(&verifier_worktree)
                .unwrap()
                .is_empty(),
            "verifier edits must be reset"
        );
        let session_worktree = session.worktree.clone().unwrap();
        assert!(
            !std::path::Path::new(&session_worktree)
                .join("verifier-target.txt")
                .exists()
        );
        let diff = engine.session_diff(&session.id).await.unwrap();
        assert!(diff.patch.is_empty());
        let events = engine
            .list_events(session.id.clone(), 0, 300)
            .await
            .unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "agent.finished"
                && event.payload["agent_role"] == "verifier"
                && event.payload["approved"] == false
                && event.payload["summary"]
                    .as_str()
                    .is_some_and(|summary| summary.contains("modified tracked files"))
        }));
    }

    #[test]
    fn matches_write_scopes() {
        assert!(path_in_scope("src/a.rs", &["src".to_string()]));
        assert!(path_in_scope("src/a.rs", &["src/**".to_string()]));
        assert!(!path_in_scope("docs/a.md", &["src".to_string()]));
        assert!(path_in_scope("anything", &[]));
        assert!(scopes_overlap(
            &["src".to_string()],
            &["src/components".to_string()]
        ));
        assert!(!scopes_overlap(&["src".to_string()], &["docs".to_string()]));
        assert!(scopes_overlap(&["*".to_string()], &["src".to_string()]));
        assert!(scopes_overlap(&["**".to_string()], &["docs".to_string()]));
    }

    #[tokio::test]
    async fn runs_parallel_workers_in_isolated_scopes() {
        let dir = TempDir::new().unwrap();
        let provider = Arc::new(RoutingProvider::new());
        let provider_trait: Arc<dyn Provider> = provider.clone();
        let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
        let session = engine
            .create_session(project.id, "parallel goal".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, "parallel goal").await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            let agents = engine.list_agents(&session.id).await.unwrap();
            let workers = agents
                .iter()
                .filter(|agent| agent.role == "worker")
                .collect::<Vec<_>>();
            let status = engine
                .list_sessions(Some(session.project_id.clone()))
                .await
                .unwrap()
                .first()
                .map(|session| session.status.clone());
            if workers.len() == 2
                && workers.iter().all(|worker| worker.status == "idle")
                && status.as_deref() == Some("idle")
            {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "parallel workers did not finish; agents={agents:?}, session={status:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        assert!(
            provider
                .max_active
                .load(std::sync::atomic::Ordering::SeqCst)
                >= 2,
            "workers did not run concurrently"
        );
        let agents = engine.list_agents(&session.id).await.unwrap();
        let workers = agents
            .iter()
            .filter(|agent| agent.role == "worker")
            .collect::<Vec<_>>();
        assert_eq!(workers.len(), 2);
        assert!(workers.iter().all(|worker| worker.status == "idle"));
        assert!(workers.iter().all(|worker| worker.branch.is_some()));
        let events = engine
            .list_events(session.id.clone(), 0, 300)
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "agent.spawned")
                .count(),
            3
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == "agent.message" && event.payload["text"] == "A done")
        );
        assert!(
            events
                .iter()
                .any(|event| event.kind == "agent.message" && event.payload["text"] == "B done")
        );
        let sessions = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "idle");
    }

    #[tokio::test]
    async fn rejects_worker_changes_outside_the_write_scope() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_manager".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "manager",
                            "title": "Quality",
                            "objective": "deliver"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_worker".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "worker",
                            "objective": "write outside the scope",
                            "write_scope": ["src"]
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_write".into(),
                        name: "write_file".into(),
                        arguments: serde_json::json!({
                            "path": "docs/out.txt",
                            "content": "outside\n"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("worker wrote".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("manager done".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("leader done".into()),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "scope".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        let approver_engine = engine.clone();
        let approver_session = session.id.clone();
        let approver = tokio::spawn(async move {
            for _ in 0..100 {
                if let Ok(approvals) = approver_engine
                    .list_approvals(Some(approver_session.clone()))
                    .await
                {
                    for approval in approvals.iter().filter(|a| a.status == "pending") {
                        let _ = approver_engine.resolve_approval(&approval.id, true).await;
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        });
        service.start(&session.id, "scope").await.unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;
        approver.abort();

        let worktree = session.worktree.clone().unwrap();
        assert!(
            !std::path::Path::new(&worktree)
                .join("docs/out.txt")
                .exists()
        );
        let agents = engine.list_agents(&session.id).await.unwrap();
        let worker = agents.iter().find(|agent| agent.role == "worker").unwrap();
        assert_eq!(worker.status, "failed");
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "agent.finished"
                && event.payload["agent_role"] == "worker"
                && event.payload["status"] == "failed"
                && event.payload["summary"]
                    .as_str()
                    .is_some_and(|summary| summary.contains("write scope violation"))
        }));
    }

    #[tokio::test]
    async fn requires_a_title_for_managers() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_manager".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "manager",
                            "objective": "unowned department"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("no manager".into()),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "title check".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, "title check").await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        let agents = engine.list_agents(&session.id).await.unwrap();
        assert!(!agents.iter().any(|agent| agent.role == "manager"));
        let events = engine
            .list_events(session.id.clone(), 0, 100)
            .await
            .unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "tool.finished"
                && event.payload["name"] == "delegate"
                && event.payload["is_error"] == true
        }));
    }

    #[tokio::test]
    async fn submits_a_team_plan_and_applies_manager_profiles() {
        let dir = TempDir::new().unwrap();
        let plan = serde_json::json!({
            "goal": "research agent platforms",
            "departments": [{
                "name": "sources",
                "manager_title": "Sources",
                "profile": "research",
                "objective": "collect primary sources"
            }],
            "tasks": [{
                "id": "T1",
                "department": "sources",
                "title": "Collect sources",
                "kind": "research",
                "deps": [],
                "write_scope": [],
                "acceptance": ["at least three primary sources"]
            }]
        });
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_plan".into(),
                        name: "submit_team_plan".into(),
                        arguments: plan.to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_sources".into(),
                        name: "delegate".into(),
                        arguments: serde_json::json!({
                            "role": "manager",
                            "title": "Sources",
                            "objective": "collect primary sources"
                        })
                        .to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("sources collected".into()),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("research complete".into()),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "research".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, "research").await.unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;

        let stored = engine.get_team_plan(&session.id).await.unwrap().unwrap();
        assert_eq!(stored.departments[0].manager_title, "Sources");
        assert_eq!(stored.tasks[0].id, "T1");
        let agents = engine.list_agents(&session.id).await.unwrap();
        let manager = agents.iter().find(|agent| agent.role == "manager").unwrap();
        assert_eq!(manager.title.as_deref(), Some("Sources"));
        assert_eq!(manager.profile.as_deref(), Some("research"));
        let events = engine
            .list_events(session.id.clone(), 0, 300)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == "team_plan.submitted")
        );
        assert!(events.iter().any(|event| {
            event.kind == "agent.finished"
                && event.payload["agent_role"] == "manager"
                && event.payload["agent_profile"] == "research"
        }));
    }

    #[tokio::test]
    async fn rejects_cyclic_team_plans() {
        let dir = TempDir::new().unwrap();
        let plan = serde_json::json!({
            "goal": "bad plan",
            "departments": [{
                "name": "impl",
                "manager_title": "Implementation",
                "profile": "coding",
                "objective": "build"
            }],
            "tasks": [
                {"id": "A", "department": "impl", "title": "A", "kind": "coding", "deps": ["B"], "acceptance": ["done"]},
                {"id": "B", "department": "impl", "title": "B", "kind": "coding", "deps": ["A"], "acceptance": ["done"]}
            ]
        });
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![
                vec![
                    StreamEvent::ToolCall(ToolCall {
                        id: "call_plan".into(),
                        name: "submit_team_plan".into(),
                        arguments: plan.to_string(),
                    }),
                    StreamEvent::Done,
                ],
                vec![
                    StreamEvent::TextDelta("plan rejected".into()),
                    StreamEvent::Done,
                ],
            ])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "cycle".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, "cycle").await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(engine.get_team_plan(&session.id).await.unwrap().is_none());
        let events = engine.list_events(session.id, 0, 100).await.unwrap();
        assert!(events.iter().any(|event| {
            event.kind == "tool.finished"
                && event.payload["name"] == "submit_team_plan"
                && event.payload["is_error"] == true
        }));
    }

    #[tokio::test]
    async fn task_budget_blocks_only_the_task_not_the_plan() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(BudgetPlanProvider {
            department_tokens: None,
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "task budgets".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, "task budgets").await.unwrap();
        let sessions = wait_until_idle(&engine, &session).await;
        assert_eq!(sessions.status, "idle");
        let events = engine
            .list_events(session.id.clone(), 0, 400)
            .await
            .unwrap();
        let budget_events = events
            .iter()
            .filter(|event| event.kind == "agent.budget_exceeded")
            .collect::<Vec<_>>();
        assert_eq!(
            budget_events
                .iter()
                .filter(|event| event.payload["scope"] == "task")
                .count(),
            2
        );
        assert!(budget_events.iter().all(|event| {
            event.payload["reason"] == "tokens"
                && event.payload["limit"] == 1
                && event.payload["spent"] == 0
        }));
        let blocked = events
            .iter()
            .filter(|event| {
                event.kind == "plan_task.finished"
                    && event.payload["task_status"] == "blocked"
                    && event.payload["reason"]
                        .as_str()
                        .is_some_and(|reason| reason.contains("task T"))
            })
            .count();
        assert_eq!(blocked, 2);
        assert!(events.iter().any(|event| {
            event.kind == "agent.updated"
                && event.payload["agent"]["role"] == "leader"
                && event.payload["status"] == "idle"
        }));
        let ledger = engine.usage_ledger(&session.id).await.unwrap();
        assert!(
            ledger.entries.iter().all(|entry| entry.task_id.is_none()),
            "blocked tasks must not record usage"
        );
    }

    #[tokio::test]
    async fn department_budget_aggregates_across_ready_tasks() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(BudgetPlanProvider {
            department_tokens: Some(1),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "department budgets".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service
            .start(&session.id, "department budgets")
            .await
            .unwrap();
        let sessions = wait_until_idle(&engine, &session).await;
        assert_eq!(sessions.status, "idle");
        let events = engine
            .list_events(session.id.clone(), 0, 400)
            .await
            .unwrap();
        let department_stops = events
            .iter()
            .filter(|event| {
                event.kind == "agent.budget_exceeded"
                    && event.payload["scope"] == "department"
                    && event.payload["id"] == "impl"
            })
            .count();
        assert_eq!(department_stops, 2);
        assert!(events.iter().any(|event| {
            event.kind == "plan_task.finished"
                && event.payload["task_status"] == "blocked"
                && event.payload["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("department impl"))
        }));
    }

    async fn wait_until_idle(engine: &Engine, session: &Session) -> vp_protocol::types::Session {
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            let sessions = engine
                .list_sessions(Some(session.project_id.clone()))
                .await
                .unwrap();
            let current = sessions
                .into_iter()
                .find(|candidate| candidate.id == session.id)
                .unwrap();
            if matches!(current.status.as_str(), "idle" | "failed" | "cancelled") {
                return current;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "session did not finish: {}",
                current.status
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn auto_dispatches_a_team_plan_in_dependency_batches() {
        let dir = TempDir::new().unwrap();
        let provider = Arc::new(PlanRoutingProvider::new());
        let provider_trait: Arc<dyn Provider> = provider.clone();
        let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
        let session = engine
            .create_session(project.id, "auto plan".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, "auto plan").await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            let sessions = engine
                .list_sessions(Some(session.project_id.clone()))
                .await
                .unwrap();
            if sessions[0].status == "idle" {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "auto plan did not finish: {}",
                sessions[0].status
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        assert!(
            provider
                .max_active
                .load(std::sync::atomic::Ordering::SeqCst)
                >= 2,
            "plan tasks did not run concurrently"
        );
        let plan = engine.get_team_plan(&session.id).await.unwrap().unwrap();
        assert_eq!(plan.tasks.len(), 2);
        let agents = engine.list_agents(&session.id).await.unwrap();
        let manager = agents.iter().find(|agent| agent.role == "manager").unwrap();
        assert_eq!(manager.title.as_deref(), Some("Sources"));
        assert_eq!(manager.profile.as_deref(), Some("research"));
        let workers = agents
            .iter()
            .filter(|agent| agent.role == "worker")
            .collect::<Vec<_>>();
        assert_eq!(workers.len(), 2);
        assert!(workers.iter().all(|worker| worker.status == "idle"));
        assert!(
            workers
                .iter()
                .all(|worker| worker.profile.as_deref() == Some("research"))
        );
        let events = engine
            .list_events(session.id.clone(), 0, 400)
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "plan_task.started")
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.kind == "plan_task.finished"
                        && event.payload["task_status"] == "completed"
                })
                .count(),
            2
        );
        let sessions = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "idle");
    }

    #[tokio::test]
    async fn serializes_plan_tasks_with_overlapping_scopes() {
        let dir = TempDir::new().unwrap();
        let provider = Arc::new(PlanRoutingProvider::with_overlapping_scopes(true));
        let provider_trait: Arc<dyn Provider> = provider.clone();
        let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
        let session = engine
            .create_session(project.id, "overlapping plan".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service
            .start(&session.id, "overlapping plan")
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(700)).await;

        assert_eq!(
            provider
                .max_active
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "overlapping write scopes must not run concurrently"
        );
        let events = engine
            .list_events(session.id.clone(), 0, 400)
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.kind == "plan_task.finished"
                        && event.payload["task_status"] == "completed"
                })
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn plan_batches_respect_the_worker_cap() {
        let dir = TempDir::new().unwrap();
        let provider = Arc::new(PlanRoutingProvider::new());
        let provider_trait: Arc<dyn Provider> = provider.clone();
        let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
        set_worker_cap(&service, 1);
        let session = engine
            .create_session(project.id, "capped plan".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, "capped plan").await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        let events = loop {
            let events = engine
                .list_events(session.id.clone(), 0, 400)
                .await
                .unwrap();
            let completed = events
                .iter()
                .filter(|event| {
                    event.kind == "plan_task.finished"
                        && event.payload["task_status"] == "completed"
                })
                .count();
            if completed == 2 {
                break events;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "plan did not finish both tasks; events={events:?}"
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        };

        assert_eq!(
            provider
                .max_active
                .load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the worker cap must bound plan concurrency"
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| {
                    event.kind == "plan_task.finished"
                        && event.payload["task_status"] == "completed"
                })
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn managers_deliberate_with_peer_positions() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(RoundtableProvider);
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "deliberate".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, "deliberate").await.unwrap();
        tokio::time::sleep(Duration::from_millis(600)).await;

        let agents = engine.list_agents(&session.id).await.unwrap();
        let managers = agents
            .iter()
            .filter(|agent| agent.role == "manager")
            .collect::<Vec<_>>();
        assert_eq!(managers.len(), 2);
        let events = engine
            .list_events(session.id.clone(), 0, 400)
            .await
            .unwrap();
        let positions = events
            .iter()
            .filter(|event| event.kind == "manager.roundtable")
            .collect::<Vec<_>>();
        assert_eq!(positions.len(), 4);
        assert!(positions.iter().any(|event| event.payload["round"] == 1));
        assert!(positions.iter().any(|event| event.payload["round"] == 2));
        assert!(
            positions
                .iter()
                .any(|event| event.payload["position"].as_str() == Some("Sources r2 decision"))
        );
        assert!(events.iter().any(|event| {
            event.kind == "agent.message" && event.payload["text"] == "deliberation complete"
        }));
        assert!(
            !events.iter().any(|event| event.kind == "plan_task.started"),
            "deliberation should not execute plan tasks"
        );
        let sessions = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "idle");
    }

    #[tokio::test]
    async fn usage_ledger_groups_by_role_agent_and_task() {
        let dir = TempDir::new().unwrap();
        let (engine, _service, project) = test_service(&dir);
        let session = engine
            .create_session(project.id, "cost ledger".into(), None)
            .await
            .unwrap();
        engine
            .record_agent_usage(
                &session.id,
                "worker-1",
                "Worker One",
                "worker",
                Some("T1".into()),
                Some("impl".into()),
                "worker-model",
                100,
                50,
                false,
                0.25,
            )
            .await
            .unwrap();
        engine
            .record_agent_usage(
                &session.id,
                "worker-2",
                "Worker Two",
                "worker",
                Some("T2".into()),
                Some("impl".into()),
                "worker-model",
                40,
                20,
                true,
                0.10,
            )
            .await
            .unwrap();
        let ledger = engine.usage_ledger(&session.id).await.unwrap();
        assert_eq!(ledger.entries.len(), 2);
        assert_eq!(ledger.totals.calls, 2);
        assert_eq!(ledger.totals.input_tokens, 140);
        assert_eq!(ledger.totals.output_tokens, 70);
        assert!((ledger.totals.cost_usd - 0.35).abs() < 1e-9);
        assert_eq!(ledger.totals.by_role.len(), 1);
        assert_eq!(ledger.totals.by_role[0].key, "worker");
        assert_eq!(ledger.totals.by_role[0].calls, 2);
        assert_eq!(ledger.totals.by_task.len(), 2);
        assert_eq!(ledger.totals.by_department.len(), 1);
        assert_eq!(ledger.totals.by_department[0].key, "impl");
        assert_eq!(ledger.totals.by_department[0].calls, 2);
        assert_eq!(ledger.totals.by_agent.len(), 2);
        assert!(ledger.entries[1].estimated);
        let sessions = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap();
        assert_eq!(sessions[0].tokens_in, 140);
        assert_eq!(sessions[0].tokens_out, 70);
        assert!((sessions[0].cost_spent_usd - 0.35).abs() < 1e-9);
        let events = engine
            .list_events(session.id.clone(), 0, 100)
            .await
            .unwrap();
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "agent.usage")
                .count(),
            2
        );
        assert_eq!(
            events
                .iter()
                .filter(|event| event.kind == "agent.cost")
                .count(),
            2
        );
        assert!(events.iter().any(|event| {
            event.kind == "agent.usage"
                && event.payload["agent_role"] == "worker"
                && event.payload["task_id"] == "T1"
                && event.payload["model"] == "worker-model"
        }));
    }

    #[tokio::test]
    async fn warns_once_at_eighty_percent_of_token_budget() {
        let dir = TempDir::new().unwrap();
        let (engine, service, project) = test_service(&dir);
        let session = engine
            .create_session(project.id, "warn budget".into(), Some(1000))
            .await
            .unwrap();
        engine.record_usage(&session.id, 800, 0).await.unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, "warn budget").await.unwrap();
        let _ = wait_until_idle(&engine, &session).await;
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        let warnings = events
            .iter()
            .filter(|event| {
                event.kind == "agent.budget_warning" && event.payload["reason"] == "tokens"
            })
            .collect::<Vec<_>>();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].payload["limit"], 1000);
        assert_eq!(warnings[0].payload["spent"], 800);
        assert!(
            events
                .iter()
                .any(|event| event.kind == "agent.budget_exceeded")
        );
    }

    #[tokio::test]
    async fn stops_when_token_budget_is_exhausted() {
        let dir = TempDir::new().unwrap();
        let (engine, service, project) = test_service(&dir);
        let session = engine
            .create_session(project.id, "stay in budget".into(), Some(100))
            .await
            .unwrap();
        engine.record_usage(&session.id, 80, 40).await.unwrap();
        service.start(&session.id, "stay in budget").await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let events = engine
            .list_events(session.id.clone(), 0, 100)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| event.kind == "agent.budget_exceeded")
        );
        let sessions = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "failed");
        assert_eq!(sessions[0].tokens_in, 80);
        assert_eq!(sessions[0].tokens_out, 40);
    }

    #[tokio::test]
    async fn persists_estimated_usage_when_provider_omits_usage() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(QueueProvider {
            responses: Mutex::new(VecDeque::from(vec![vec![
                StreamEvent::TextDelta("estimated".into()),
                StreamEvent::Done,
            ]])),
        });
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "estimate".into(), Some(100_000))
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, "estimate").await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(8);
        loop {
            let sessions = engine
                .list_sessions(Some(session.project_id.clone()))
                .await
                .unwrap();
            if sessions[0].status == "idle" {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "estimated usage run did not finish: {}",
                sessions[0].status
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }

        let ledger = engine.usage_ledger(&session.id).await.unwrap();
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries[0].role, "leader");
        assert_eq!(ledger.entries[0].model, "test");
        assert!(ledger.entries[0].estimated);
        assert!(ledger.entries[0].input_tokens > 0);
        assert!(ledger.entries[0].output_tokens > 0);

        let session = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == session.id)
            .unwrap();
        assert!(session.tokens_in > 0);
        assert!(session.tokens_out > 0);
        let events = engine
            .list_events(session.id.clone(), 0, 200)
            .await
            .unwrap();
        assert!(
            events
                .iter()
                .any(|event| { event.kind == "agent.usage" && event.payload["estimated"] == true })
        );
    }

    #[test]
    fn team_plan_validates_task_and_department_budgets() {
        let base = serde_json::json!({
            "goal": "budgets",
            "departments": [{
                "name": "impl",
                "manager_title": "Implementation",
                "profile": "coding",
                "objective": "build",
                "token_budget": 100,
                "cost_budget_usd": 1.0
            }],
            "tasks": [{
                "id": "T1",
                "department": "impl",
                "title": "Task",
                "kind": "coding",
                "acceptance": ["done"],
                "token_budget": 40,
                "cost_budget_usd": 0.4
            }]
        });
        let plan: TeamPlan = serde_json::from_value(base.clone()).unwrap();
        validate_team_plan(&plan, 5).unwrap();

        let mut zero_department = base.clone();
        zero_department["departments"][0]["token_budget"] = serde_json::json!(0);
        assert!(validate_team_plan(&serde_json::from_value(zero_department).unwrap(), 5).is_err());

        let mut zero_task = base.clone();
        zero_task["tasks"][0]["cost_budget_usd"] = serde_json::json!(0);
        assert!(validate_team_plan(&serde_json::from_value(zero_task).unwrap(), 5).is_err());

        let mut task_over_department = base.clone();
        task_over_department["tasks"][0]["token_budget"] = serde_json::json!(101);
        assert!(
            validate_team_plan(&serde_json::from_value(task_over_department).unwrap(), 5).is_err()
        );

        let mut sum_over_department = base;
        sum_over_department["tasks"] = serde_json::json!([
            {"id": "T1", "department": "impl", "title": "One", "kind": "coding", "acceptance": ["done"], "token_budget": 60},
            {"id": "T2", "department": "impl", "title": "Two", "kind": "coding", "acceptance": ["done"], "token_budget": 60}
        ]);
        assert!(
            validate_team_plan(&serde_json::from_value(sum_over_department).unwrap(), 5).is_err()
        );
    }

    #[test]
    fn plan_batch_serializes_unscoped_and_overlapping_tasks() {
        fn task(id: &str, scope: &[&str]) -> PlanTask {
            PlanTask {
                id: id.to_string(),
                department: "impl".to_string(),
                title: id.to_string(),
                kind: "coding".to_string(),
                profile: None,
                deps: Vec::new(),
                write_scope: scope.iter().map(|scope| scope.to_string()).collect(),
                acceptance: vec!["done".to_string()],
                token_budget: None,
                cost_budget_usd: None,
            }
        }

        let batch = select_plan_batch(
            vec![task("T1", &[]), task("T2", &["src"]), task("T3", &["src"])],
            10,
        );
        assert_eq!(
            batch
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["T1"]
        );

        let batch = select_plan_batch(
            vec![
                task("T1", &["src"]),
                task("T2", &["docs"]),
                task("T3", &["src/sub"]),
            ],
            10,
        );
        assert_eq!(
            batch
                .iter()
                .map(|task| task.id.as_str())
                .collect::<Vec<_>>(),
            vec!["T1", "T2"]
        );
    }

    #[test]
    fn session_budget_tracks_cost_and_time() {
        let caps = AgentCaps {
            managers: 5,
            workers: 10,
            labour: 20,
        };
        let cost_budget = SessionBudget::new(None, 0, caps)
            .with_cost(Some(0.01), 0.0)
            .with_time(Some(60));
        assert!(!cost_budget.exhausted());
        assert!(cost_budget.add_cost(0.02));
        assert_eq!(cost_budget.exceeded_reason(), Some("cost"));

        let time_budget = SessionBudget::new(None, 0, caps).with_time(Some(0));
        assert_eq!(time_budget.exceeded_reason(), Some("time"));
        assert_eq!(usage_cost(1_000_000, 500_000, 2.0, 4.0), 4.0);
    }

    #[test]
    fn token_reservations_bound_concurrent_requests() {
        let budget = SessionBudget::new(
            Some(1000),
            0,
            AgentCaps {
                managers: 5,
                workers: 10,
                labour: 20,
            },
        );
        let first = budget.reserve_tokens(600).expect("first reservation");
        assert!(budget.reserve_tokens(500).is_none());
        assert_eq!(budget.available_tokens(), Some(400));
        assert!(!first.settle(300));
        assert_eq!(budget.spent(), 300);

        {
            let _unused = budget.reserve_tokens(200).expect("temporary reservation");
        }
        assert_eq!(budget.available_tokens(), Some(700));

        let second = budget.reserve_tokens(700).expect("second reservation");
        assert!(second.settle(700));
        assert_eq!(budget.spent(), 1000);
        assert!(budget.reserve_tokens(1).is_none());
    }

    struct PausingProvider {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl Provider for PausingProvider {
        fn id(&self) -> &str {
            "pausing"
        }

        async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
            let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if call == 0 {
                self.started.notify_one();
                self.release.notified().await;
                Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::ToolCall(ToolCall {
                        id: "pause-todo".into(),
                        name: "todo_write".into(),
                        arguments: serde_json::json!({
                            "items": [{"title": "Resume", "status": "in_progress"}]
                        })
                        .to_string(),
                    })),
                    Ok(StreamEvent::Done),
                ])))
            } else {
                Ok(Box::pin(futures_util::stream::iter(vec![
                    Ok(StreamEvent::TextDelta("resumed".into())),
                    Ok(StreamEvent::Done),
                ])))
            }
        }
    }

    #[tokio::test]
    async fn mailbox_tools_send_read_and_publish() {
        let dir = TempDir::new().unwrap();
        let (engine, _service, project) = test_service(&dir);
        let session = engine
            .create_session(project.id.clone(), "mailbox".into(), None)
            .await
            .unwrap();
        let leader = engine
            .create_agent(&session.id, "leader", "Leader", None, None, "test")
            .await
            .unwrap();
        let worker = engine
            .create_agent(&session.id, "worker", "Worker", None, None, "test")
            .await
            .unwrap();
        let spec = AgentSpec {
            id: leader.id.clone(),
            role: AgentRole::Leader,
            name: leader.name.clone(),
            depth: 0,
            parent_name: None,
            title: None,
            task_id: None,
            department: None,
            profile: AgentProfile::Generic,
            allow_delegation: true,
            auto_approve: false,
            worktree: PathBuf::from(session.worktree.clone().unwrap()),
            branch: session.branch.clone().unwrap(),
            session_worktree: PathBuf::from(session.worktree.clone().unwrap()),
            session_branch: session.branch.clone().unwrap(),
            write_scope: Vec::new(),
            base_commit: session.base_commit.clone().unwrap(),
        };
        let sent = run_mailbox_tool(
            &engine,
            &session,
            &spec,
            "send_message",
            serde_json::json!({
                "to": worker.name,
                "kind": "handoff",
                "body": "Please build the API",
                "payload": {"task": "T1"}
            }),
        )
        .await
        .unwrap();
        assert!(!sent.is_error, "{}", sent.content);
        let worker_spec = AgentSpec {
            id: worker.id.clone(),
            name: worker.name.clone(),
            ..spec.clone()
        };
        let read = run_mailbox_tool(
            &engine,
            &session,
            &worker_spec,
            "read_mailbox",
            serde_json::json!({"unread_only": true}),
        )
        .await
        .unwrap();
        assert!(
            read.content.contains("Please build the API"),
            "{}",
            read.content
        );
        let published = run_mailbox_tool(
            &engine,
            &session,
            &worker_spec,
            "publish_artifact",
            serde_json::json!({
                "name": "worker-notes.md",
                "kind": "note",
                "content": "# notes"
            }),
        )
        .await
        .unwrap();
        assert!(!published.is_error, "{}", published.content);
        let artifacts = run_mailbox_tool(
            &engine,
            &session,
            &worker_spec,
            "list_artifacts",
            serde_json::json!({}),
        )
        .await
        .unwrap();
        assert!(artifacts.content.contains("worker-notes.md"));
    }

    #[tokio::test]
    async fn role_models_override_the_base_model() {
        let dir = TempDir::new().unwrap();
        let (_engine, service, _project) = test_service(&dir);
        let mut config = service.config.read().unwrap().as_ref().unwrap().clone();
        config.role_models.worker.model = Some("worker-model".into());
        config.role_models.worker.input_usd_per_million = Some(2.0);
        config.role_models.worker.output_usd_per_million = Some(4.0);
        assert_eq!(model_for(&config, AgentRole::Worker), "worker-model");
        assert_eq!(model_for(&config, AgentRole::Manager), "test");
        assert_eq!(role_price(&config, AgentRole::Worker), (2.0, 4.0));
    }

    #[tokio::test]
    async fn global_capacity_serializes_worker_slots() {
        let capacity = Arc::new(CapacityController::new(1, 1));
        assert_eq!(
            capacity.snapshot(),
            CapacityInfo {
                worker_limit: 1,
                worker_available: 1,
                labour_limit: 1,
                labour_available: 1,
            }
        );
        let probe = capacity
            .try_acquire(AgentRole::Worker, "session-a")
            .unwrap();
        drop(probe);
        let control = RunControl::new();
        let first = capacity
            .acquire(AgentRole::Worker, "session-a", &control)
            .await
            .unwrap();
        assert_eq!(capacity.snapshot().worker_available, 0);
        assert!(
            capacity
                .try_acquire(AgentRole::Worker, "session-b")
                .is_none()
        );
        let second_capacity = capacity.clone();
        let second_control = Arc::new(RunControl::new());
        let mut waiter = tokio::spawn(async move {
            second_capacity
                .acquire(AgentRole::Worker, "session-b", &second_control)
                .await
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(120), &mut waiter)
                .await
                .is_err()
        );
        drop(first);
        let acquired = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap();
        assert!(acquired.is_some());
        assert_eq!(capacity.snapshot().worker_available, 0);

        let third_control = Arc::new(RunControl::new());
        let third_capacity = capacity.clone();
        let worker_control = third_control.clone();
        let cancelled = tokio::spawn(async move {
            third_capacity
                .acquire(AgentRole::Worker, "session-c", &worker_control)
                .await
        });
        tokio::time::sleep(Duration::from_millis(50)).await;
        third_control.cancel();
        let result = tokio::time::timeout(Duration::from_secs(2), cancelled)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn capacity_round_robins_across_sessions() {
        let capacity = Arc::new(CapacityController::new(1, 1));
        let first = capacity
            .try_acquire(AgentRole::Worker, "session-a")
            .unwrap();
        let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel();
        let a_capacity = capacity.clone();
        let a_control = Arc::new(RunControl::new());
        let a_sender = sender.clone();
        let a = tokio::spawn(async move {
            let permit = a_capacity
                .acquire(AgentRole::Worker, "session-a", &a_control)
                .await
                .unwrap();
            a_sender.send("a2").unwrap();
            tokio::time::sleep(Duration::from_millis(40)).await;
            drop(permit);
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        let b_capacity = capacity.clone();
        let b_control = Arc::new(RunControl::new());
        let b_sender = sender.clone();
        let b = tokio::spawn(async move {
            let permit = b_capacity
                .acquire(AgentRole::Worker, "session-b", &b_control)
                .await
                .unwrap();
            b_sender.send("b1").unwrap();
            tokio::time::sleep(Duration::from_millis(40)).await;
            drop(permit);
        });
        tokio::time::sleep(Duration::from_millis(30)).await;
        drop(first);
        let first_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        let second_grant = tokio::time::timeout(Duration::from_secs(2), receiver.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!((first_grant, second_grant), ("b1", "a2"));
        a.await.unwrap();
        b.await.unwrap();
    }

    #[tokio::test]
    async fn scope_locks_serialize_overlapping_sessions() {
        let locks = Arc::new(ScopeLocks::default());
        let control_a = RunControl::new();
        let guard_a = locks
            .acquire(
                "project",
                "session-a",
                "agent-a",
                &["src/**".into()],
                &control_a,
            )
            .await
            .unwrap()
            .unwrap();
        let same_session = locks
            .acquire(
                "project",
                "session-a",
                "agent-b",
                &["src/api/**".into()],
                &control_a,
            )
            .await
            .unwrap();
        assert!(same_session.is_some());
        drop(same_session);

        let locks_b = locks.clone();
        let control_b = RunControl::new();
        let mut waiter = tokio::spawn(async move {
            locks_b
                .acquire(
                    "project",
                    "session-b",
                    "agent-c",
                    &["src/api/**".into()],
                    &control_b,
                )
                .await
                .unwrap()
        });
        assert!(
            tokio::time::timeout(Duration::from_millis(120), &mut waiter)
                .await
                .is_err()
        );
        drop(guard_a);
        let acquired = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap();
        assert!(acquired.is_some());
    }

    struct BlockingProvider {
        started: tokio::sync::Notify,
        release: tokio::sync::Notify,
        calls: std::sync::atomic::AtomicUsize,
    }

    #[async_trait]
    impl Provider for BlockingProvider {
        fn id(&self) -> &str {
            "blocking"
        }

        async fn stream_chat(&self, _request: ChatRequest) -> Result<EventStream, ProviderError> {
            if self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
                self.started.notify_one();
                self.release.notified().await;
            }
            Ok(Box::pin(futures_util::stream::iter(vec![
                Ok(StreamEvent::TextDelta("done".into())),
                Ok(StreamEvent::Done),
            ])))
        }
    }

    #[tokio::test]
    async fn global_session_cap_queues_second_run() {
        let dir = TempDir::new().unwrap();
        let provider = Arc::new(BlockingProvider {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let (engine, service, project) = test_service_with_provider_cap(&dir, provider.clone(), 1);
        let first = engine
            .create_session(project.id.clone(), "first".into(), None)
            .await
            .unwrap();
        let second = engine
            .create_session(project.id.clone(), "second".into(), None)
            .await
            .unwrap();
        engine.create_leader_agent(&first.id, "test").await.unwrap();
        engine
            .create_leader_agent(&second.id, "test")
            .await
            .unwrap();
        service.start(&first.id, "first").await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
            .await
            .unwrap();
        service.start(&second.id, "second").await.unwrap();
        tokio::time::sleep(Duration::from_millis(120)).await;
        let sessions = engine
            .list_sessions(Some(project.id.clone()))
            .await
            .unwrap();
        assert_eq!(
            sessions
                .iter()
                .find(|session| session.id == first.id)
                .unwrap()
                .status,
            "running"
        );
        assert_eq!(
            sessions
                .iter()
                .find(|session| session.id == second.id)
                .unwrap()
                .status,
            "queued"
        );
        provider.release.notify_one();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let sessions = engine
                .list_sessions(Some(project.id.clone()))
                .await
                .unwrap();
            if sessions.iter().all(|session| session.status == "idle") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "queued session did not run: {:?}",
                sessions
                    .iter()
                    .map(|session| (&session.id, &session.status))
                    .collect::<Vec<_>>()
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    #[tokio::test]
    async fn pauses_and_resumes_between_turns() {
        let dir = TempDir::new().unwrap();
        let provider = Arc::new(PausingProvider {
            started: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
            calls: std::sync::atomic::AtomicUsize::new(0),
        });
        let provider_trait: Arc<dyn Provider> = provider.clone();
        let (engine, service, project) = test_service_with_provider(&dir, provider_trait);
        let session = engine
            .create_session(project.id, "pause".into(), None)
            .await
            .unwrap();
        engine
            .create_leader_agent(&session.id, "test")
            .await
            .unwrap();
        service.start(&session.id, "pause").await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), provider.started.notified())
            .await
            .unwrap();
        assert!(service.pause(&session.id).await.unwrap());
        provider.release.notify_one();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let sessions = engine
            .list_sessions(Some(session.project_id.clone()))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "paused");
        assert_eq!(provider.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(service.resume(&session.id).await.unwrap());
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let sessions = engine
                .list_sessions(Some(session.project_id.clone()))
                .await
                .unwrap();
            if sessions[0].status == "idle" {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "session did not resume: {}",
                sessions[0].status
            );
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        let events = engine
            .list_events(session.id.clone(), 0, 300)
            .await
            .unwrap();
        assert!(events.iter().any(|event| event.kind == "agent.paused"));
        assert!(events.iter().any(|event| event.kind == "agent.resumed"));
        assert!(!service.pause(&session.id).await.unwrap());
    }

    #[tokio::test]
    async fn cancels_a_slow_provider_stream() {
        let dir = TempDir::new().unwrap();
        let provider: Arc<dyn Provider> = Arc::new(SlowProvider);
        let (engine, service, project) = test_service_with_provider(&dir, provider);
        let session = engine
            .create_session(project.id, "cancel me".into(), None)
            .await
            .unwrap();
        service.start(&session.id, "cancel me").await.unwrap();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(service.cancel(&session.id));
        tokio::time::sleep(Duration::from_millis(200)).await;
        let events = engine
            .list_events(session.id.clone(), 0, 100)
            .await
            .unwrap();
        assert!(events.iter().any(|event| event.kind == "agent.cancelled"));
        let sessions = engine
            .list_sessions(Some(session.project_id))
            .await
            .unwrap();
        assert_eq!(sessions[0].status, "cancelled");
        assert!(!service.cancel(&session.id));
    }
}
