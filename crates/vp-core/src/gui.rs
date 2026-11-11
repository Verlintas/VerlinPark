//! Unified GUI session ownership, grace, bounded queue, and action timeline.
//!
//! The browser and the native computer helper drive the same physical display,
//! keyboard focus, and Accessibility tree. Before this module they used
//! separate `browser` and `computer` [`ComputerLease`] rows, so two sessions
//! could drive the GUI concurrently. New acquisitions use one logical `gui`
//! surface. Legacy `browser`/`computer` rows stay readable and block a new
//! acquisition until they expire or an operator releases them.
//!
//! The lease and the helper process have different lifetimes:
//!
//! - a helper may stay warm for its own idle window (`VERLINPARK_*_IDLE_SECS`);
//! - the `gui` lease is retained only for a bounded grace period after the last
//!   action, then released while the helper remains warm;
//! - the next action reacquires the lease (with a bounded wait) and reuses the
//!   warm helper;
//! - `close` and session cleanup release the lease immediately.
//!
//! The queue is a deterministic FIFO inside one daemon process. A second daemon
//! or another host is not part of the queue: it receives a typed `GUI_BUSY`
//! with the SQLite holder and a retry hint. This is not a distributed lease,
//! not a cross-host queue claim, and not a fencing token for external effects.
//!
//! [`ComputerLease`]: crate::ComputerLeaseGuard

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use anyhow::Result;
use serde_json::Value;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use tokio::sync::Mutex as AsyncMutex;
use tokio::sync::oneshot;
use uuid::Uuid;

use vp_protocol::types::{GuiActionInfo, GuiLeaseInfo, GuiPolicy, GuiQueueInfo, GuiSensitiveMode};

use crate::ComputerLeaseGuard;
use crate::Engine;

/// Canonical surface stored in `computer_leases.surface`.
pub const GUI_SURFACE: &str = "gui";
/// Surfaces written by an older daemon. They remain authoritative until they
/// expire; new code never writes them.
pub const LEGACY_GUI_SURFACES: [&str; 2] = ["browser", "computer"];
/// Lease TTL used for heartbeat renewal. Grace is usually much shorter.
pub const GUI_LEASE_TTL_SECS: u64 = 120;
/// Bound the in-process FIFO. Beyond this the caller gets a typed busy result
/// instead of growing an unbounded waiter list.
pub const MAX_GUI_WAITERS: usize = 64;
/// Wait-loop responsiveness. The loop also wakes on the grant channel.
const WAIT_SLICE: Duration = Duration::from_millis(100);

/// Built-in sensitive bundle identifiers. System Settings, the authorization
/// agent, Keychain, and common password managers are never silently automated.
pub const DEFAULT_SENSITIVE_BUNDLES: &[&str] = &[
    "com.apple.systempreferences",
    "com.apple.systemsettings",
    "com.apple.securityagent",
    "com.apple.keychainaccess",
    "com.apple.passwords",
    "com.apple.loginwindow",
    "com.1password.1password",
    "com.1password.1password7",
    "com.agilebits.onepassword7",
    "com.bitwarden.desktop",
    "com.lastpass.lastpass",
    "com.dashlane.dashlane",
    "com.keepersecurity.passwordmanager",
    "com.nordpass.nordpass",
];

/// Built-in sensitive title/application patterns. Matching is case-insensitive
/// substring matching over the app name and window title.
pub const DEFAULT_SENSITIVE_TITLES: &[&str] = &[
    "system settings",
    "system preferences",
    "securityagent",
    "keychain access",
    "passwords",
    "privacy & security",
    "screen recording",
    "full disk access",
    "accessibility",
    "authentication",
    "1password",
    "bitwarden",
    "lastpass",
    "dashlane",
    "nordpass",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuiSurface {
    Browser,
    Computer,
}

impl GuiSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Browser => "browser",
            Self::Computer => "computer",
        }
    }

    pub fn from_tool(name: &str) -> Option<Self> {
        match name {
            "browser" => Some(Self::Browser),
            "computer" => Some(Self::Computer),
            _ => None,
        }
    }
}

/// One action's ownership record. The lease generation changes on every new
/// acquisition; the snapshot generation is filled by the computer helper.
#[derive(Debug, Clone)]
pub struct GuiActionTicket {
    pub ticket_id: String,
    pub lease_id: String,
    pub session_id: String,
    pub agent_id: String,
    pub agent_name: String,
    pub surface: GuiSurface,
    pub lease_generation: u64,
    pub waited_ms: u64,
}

/// Metadata recorded after an action. Deliberately excludes typed text,
/// element values, page text, and screenshot bytes.
#[derive(Debug, Clone)]
pub struct GuiActionOutcome {
    pub action: String,
    pub target_app: Option<String>,
    pub target_bundle_id: Option<String>,
    pub target_window: Option<String>,
    pub sensitive_target: bool,
    pub snapshot_generation: u64,
    pub latency_ms: u64,
    pub result: String,
    pub error_code: Option<String>,
}

impl GuiActionOutcome {
    pub fn new(action: impl Into<String>) -> Self {
        Self {
            action: action.into(),
            target_app: None,
            target_bundle_id: None,
            target_window: None,
            sensitive_target: false,
            snapshot_generation: 0,
            latency_ms: 0,
            result: "ok".to_string(),
            error_code: None,
        }
    }
}

/// Typed busy result. `retry_after_ms` is a hint derived from the holder's
/// remaining grace (or lease TTL for a legacy/cross-process row); callers do
/// not retry automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuiBusy {
    pub code: &'static str,
    pub message: String,
    pub holder_session_id: Option<String>,
    pub holder_agent_id: Option<String>,
    pub holder_agent_name: Option<String>,
    pub holder_surface: Option<String>,
    pub holder_expires_at: Option<String>,
    pub retry_after_ms: u64,
    pub waited_ms: u64,
    pub wait_ms: u64,
    pub queue_position: Option<u32>,
}

impl GuiBusy {
    pub fn code(&self) -> &'static str {
        self.code
    }

    pub fn to_structured(&self, action: &str) -> Value {
        serde_json::json!({
            "ok": false,
            "code": self.code,
            "action": action,
            "message": self.message,
            "retryable": true,
            "hint": "The GUI surface is held by another session. Retry after the hint delay; do not loop without a fresh action.",
            "holder": {
                "session_id": self.holder_session_id,
                "agent_id": self.holder_agent_id,
                "agent_name": self.holder_agent_name,
                "surface": self.holder_surface,
                "expires_at": self.holder_expires_at,
            },
            "retry_after_ms": self.retry_after_ms,
            "waited_ms": self.waited_ms,
            "wait_ms": self.wait_ms,
            "queue_position": self.queue_position,
            "lease_surface": GUI_SURFACE,
        })
    }
}

impl std::fmt::Display for GuiBusy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{}: {} (retry after {} ms)",
            self.code, self.message, self.retry_after_ms
        )
    }
}

impl std::error::Error for GuiBusy {}

#[derive(Debug)]
pub enum GuiAcquire {
    Acquired(GuiActionTicket),
    Busy(GuiBusy),
}

struct GuiHolder {
    lease_id: String,
    guard: Option<ComputerLeaseGuard>,
    session_id: String,
    agent_id: String,
    agent_name: String,
    surface: GuiSurface,
    generation: u64,
    acquired_at: OffsetDateTime,
    last_action_at: OffsetDateTime,
    last_action: Instant,
    grace: Duration,
}

struct GuiWaiter {
    ticket_id: String,
    session_id: String,
    agent_id: String,
    agent_name: String,
    surface: GuiSurface,
    enqueued_at: Instant,
    notify: oneshot::Sender<()>,
}

#[derive(Default)]
struct GuiState {
    holder: Option<GuiHolder>,
    waiters: VecDeque<GuiWaiter>,
    generation: u64,
    releasing: bool,
}

/// Single-daemon GUI coordinator. Cloned `Arc` by the engine; all mutable state
/// is behind one async mutex so acquisition, release, and queue ordering are
/// deterministic.
pub struct GuiCoordinator {
    state: AsyncMutex<GuiState>,
}

impl Default for GuiCoordinator {
    fn default() -> Self {
        Self::new()
    }
}

impl GuiCoordinator {
    pub fn new() -> Self {
        Self {
            state: AsyncMutex::new(GuiState::default()),
        }
    }

    /// Acquire or wait for the logical GUI lease. The wait is bounded by
    /// `policy.wait_ms`; zero means a single immediate attempt. A waiter that
    /// times out is removed from the FIFO and receives a typed busy result.
    pub async fn begin(
        &self,
        engine: &Engine,
        policy: &GuiPolicy,
        surface: GuiSurface,
        session_id: &str,
        agent_id: &str,
        agent_name: &str,
    ) -> Result<GuiAcquire> {
        let policy = policy.clone().normalized();
        let grace = Duration::from_secs(policy.grace_secs);
        let wait = Duration::from_millis(policy.wait_ms);
        let started = Instant::now();
        let ticket_id = Uuid::now_v7().to_string();
        let mut queued_at: Option<Instant> = None;
        let mut queue_position: Option<u32> = None;
        let mut receiver: Option<oneshot::Receiver<()>> = None;

        loop {
            // Reclaim an expired holder before evaluating our own request.
            let expired = {
                let mut state = self.state.lock().await;
                if state.releasing {
                    None
                } else if let Some(holder) = state.holder.as_ref() {
                    if holder.session_id != session_id
                        && holder.last_action.elapsed() >= holder.grace
                    {
                        state.holder.take()
                    } else {
                        None
                    }
                } else {
                    None
                }
            };
            if let Some(holder) = expired {
                self.release_holder(holder, "grace_expired").await;
                self.remove_waiter(&ticket_id).await;
                receiver = None;
                queued_at = None;
                queue_position = None;
                continue;
            }

            // An operator release or a startup sweep can remove the SQLite row
            // while the in-memory holder still exists. A different session
            // must not wait out the grace window for a lease that is gone.
            let vanished = {
                let state = self.state.lock().await;
                state.holder.as_ref().and_then(|holder| {
                    (holder.session_id != session_id).then(|| holder.lease_id.clone())
                })
            };
            if let Some(lease_id) = vanished
                && !engine
                    .computer_lease_active(&lease_id)
                    .await
                    .unwrap_or(false)
            {
                let dead = {
                    let mut state = self.state.lock().await;
                    if state
                        .holder
                        .as_ref()
                        .is_some_and(|holder| holder.lease_id == lease_id)
                    {
                        state.holder.take()
                    } else {
                        None
                    }
                };
                if let Some(dead) = dead {
                    self.release_holder(dead, "lease_missing").await;
                }
                self.remove_waiter(&ticket_id).await;
                receiver = None;
                queued_at = None;
                queue_position = None;
                continue;
            }

            // Same-session reentry is immediate, but only while the SQLite
            // row is still alive. An operator release or sweep invalidates it.
            let reentrant = {
                let state = self.state.lock().await;
                if state.releasing {
                    None
                } else {
                    state.holder.as_ref().and_then(|holder| {
                        (holder.session_id == session_id)
                            .then(|| (holder.lease_id.clone(), holder.generation))
                    })
                }
            };
            if let Some((lease_id, generation)) = reentrant {
                let active = engine
                    .computer_lease_active(&lease_id)
                    .await
                    .unwrap_or(false);
                if active {
                    let mut state = self.state.lock().await;
                    if let Some(holder) = state.holder.as_mut()
                        && holder.lease_id == lease_id
                        && holder.session_id == session_id
                    {
                        holder.surface = surface;
                        holder.agent_id = agent_id.to_string();
                        holder.agent_name = agent_name.to_string();
                        holder.last_action = Instant::now();
                        holder.last_action_at = OffsetDateTime::now_utc();
                        self.remove_waiter_locked(&mut state, &ticket_id);
                        return Ok(GuiAcquire::Acquired(GuiActionTicket {
                            ticket_id,
                            lease_id,
                            session_id: session_id.to_string(),
                            agent_id: agent_id.to_string(),
                            agent_name: agent_name.to_string(),
                            surface,
                            lease_generation: generation,
                            waited_ms: elapsed_ms(started),
                        }));
                    }
                    continue;
                }
                let dead = {
                    let mut state = self.state.lock().await;
                    if state
                        .holder
                        .as_ref()
                        .is_some_and(|holder| holder.lease_id == lease_id)
                    {
                        state.holder.take()
                    } else {
                        None
                    }
                };
                if let Some(dead) = dead {
                    self.release_holder(dead, "lease_missing").await;
                }
                self.remove_waiter(&ticket_id).await;
                receiver = None;
                queued_at = None;
                queue_position = None;
                continue;
            }

            // No holder: try the SQLite row. Store acquisition also reports
            // legacy rows and rows owned by another Engine.
            let free = {
                let state = self.state.lock().await;
                state.holder.is_none() && !state.releasing
            };
            if free {
                match engine
                    .acquire_computer_lease(
                        GUI_SURFACE,
                        session_id,
                        agent_id,
                        agent_name,
                        Duration::from_secs(GUI_LEASE_TTL_SECS),
                    )
                    .await
                {
                    Ok(guard) => {
                        let mut state = self.state.lock().await;
                        if state.holder.is_none() && !state.releasing {
                            let generation = guard.lease.generation;
                            state.generation = state.generation.max(generation);
                            let now = OffsetDateTime::now_utc();
                            let holder = GuiHolder {
                                lease_id: guard.lease_id().to_string(),
                                guard: Some(guard),
                                session_id: session_id.to_string(),
                                agent_id: agent_id.to_string(),
                                agent_name: agent_name.to_string(),
                                surface,
                                generation,
                                acquired_at: now,
                                last_action_at: now,
                                last_action: Instant::now(),
                                grace,
                            };
                            let lease_id = holder.lease_id.clone();
                            self.remove_waiter_locked(&mut state, &ticket_id);
                            state.holder = Some(holder);
                            return Ok(GuiAcquire::Acquired(GuiActionTicket {
                                ticket_id,
                                lease_id,
                                session_id: session_id.to_string(),
                                agent_id: agent_id.to_string(),
                                agent_name: agent_name.to_string(),
                                surface,
                                lease_generation: generation,
                                waited_ms: elapsed_ms(started),
                            }));
                        }
                        drop(state);
                        // Someone won a race while the row was being created.
                        guard.release().await;
                        continue;
                    }
                    Err(error) => {
                        let message = error.to_string();
                        if !message.contains("busy") {
                            return Err(error);
                        }
                        // Another Engine (or a legacy row) owns the surface.
                        // There is no cross-Engine notification channel, so
                        // this path returns a typed busy result immediately
                        // instead of pretending the FIFO covers it.
                        if queued_at.is_none() {
                            let holder = engine.gui_lease_holder().await.ok().flatten();
                            return Ok(GuiAcquire::Busy(busy_from_store(
                                holder,
                                &policy,
                                started,
                                None,
                                policy.wait_ms,
                            )));
                        }
                    }
                }
            }

            if wait.is_zero() {
                let state = self.state.lock().await;
                return Ok(GuiAcquire::Busy(self.busy_locked(
                    &state,
                    &policy,
                    started,
                    queue_position,
                )));
            }

            if queued_at.is_none() {
                let (notify, receiver_new) = oneshot::channel();
                let waiter = GuiWaiter {
                    ticket_id: ticket_id.clone(),
                    session_id: session_id.to_string(),
                    agent_id: agent_id.to_string(),
                    agent_name: agent_name.to_string(),
                    surface,
                    enqueued_at: Instant::now(),
                    notify,
                };
                let mut state = self.state.lock().await;
                if state.waiters.len() >= MAX_GUI_WAITERS {
                    let mut busy = self.busy_locked(&state, &policy, started, None);
                    busy.code = "GUI_QUEUE_FULL";
                    busy.message = format!(
                        "the GUI wait queue is full ({MAX_GUI_WAITERS}); retry after the holder releases"
                    );
                    return Ok(GuiAcquire::Busy(busy));
                }
                state.waiters.push_back(waiter);
                queued_at = Some(Instant::now());
                queue_position = Some(state.waiters.len() as u32);
                receiver = Some(receiver_new);
            }

            let remaining = wait.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                self.remove_waiter(&ticket_id).await;
                let local = self.state.lock().await;
                if local.holder.is_some() {
                    let busy = self.busy_locked(&local, &policy, started, queue_position);
                    return Ok(GuiAcquire::Busy(busy));
                }
                drop(local);
                let holder = engine.gui_lease_holder().await.ok().flatten();
                return Ok(GuiAcquire::Busy(busy_from_store(
                    holder,
                    &policy,
                    started,
                    queue_position,
                    policy.wait_ms,
                )));
            }

            let slice = remaining.min(WAIT_SLICE);
            let granted = match receiver.as_mut() {
                Some(receiver) => matches!(tokio::time::timeout(slice, receiver).await, Ok(Ok(()))),
                None => {
                    tokio::time::sleep(slice).await;
                    false
                }
            };
            if granted {
                receiver = None;
                queued_at = None;
                queue_position = None;
            }
        }
    }

    async fn remove_waiter(&self, ticket_id: &str) {
        let mut state = self.state.lock().await;
        self.remove_waiter_locked(&mut state, ticket_id);
    }

    fn remove_waiter_locked(&self, state: &mut GuiState, ticket_id: &str) {
        state.waiters.retain(|waiter| waiter.ticket_id != ticket_id);
    }

    fn busy_locked(
        &self,
        state: &GuiState,
        policy: &GuiPolicy,
        started: Instant,
        queue_position: Option<u32>,
    ) -> GuiBusy {
        let (
            holder_session_id,
            holder_agent_id,
            holder_agent_name,
            holder_surface,
            expires_at,
            retry,
        ) = match state.holder.as_ref() {
            Some(holder) => {
                let remaining = holder
                    .grace
                    .saturating_sub(holder.last_action.elapsed())
                    .as_millis()
                    .max(100) as u64;
                (
                    Some(holder.session_id.clone()),
                    Some(holder.agent_id.clone()),
                    Some(holder.agent_name.clone()),
                    Some(holder.surface.as_str().to_string()),
                    Some(format_time(
                        holder.last_action_at
                            + time::Duration::seconds(holder.grace.as_secs() as i64),
                    )),
                    remaining.min(policy.wait_ms.max(250)),
                )
            }
            None => (None, None, None, None, None, 250),
        };
        GuiBusy {
            code: "GUI_BUSY",
            message: match &holder_session_id {
                Some(session) => format!(
                    "the GUI surface is held by session {session} ({})",
                    holder_agent_name.as_deref().unwrap_or("agent")
                ),
                None => "the GUI surface is being released; retry shortly".to_string(),
            },
            holder_session_id,
            holder_agent_id,
            holder_agent_name,
            holder_surface,
            holder_expires_at: expires_at,
            retry_after_ms: retry,
            waited_ms: elapsed_ms(started),
            wait_ms: policy.wait_ms,
            queue_position,
        }
    }

    /// Record the outcome and refresh the grace deadline. The timeline entry is
    /// returned so the engine can append one `gui.action` event. An action
    /// completed by a stale ticket is still recorded (it happened), but it
    /// does not extend a lease now owned by another session.
    pub async fn finish(
        &self,
        ticket: &GuiActionTicket,
        outcome: GuiActionOutcome,
    ) -> GuiActionInfo {
        {
            let mut state = self.state.lock().await;
            if let Some(holder) = state.holder.as_mut()
                && holder.lease_id == ticket.lease_id
                && holder.session_id == ticket.session_id
            {
                holder.last_action = Instant::now();
                holder.last_action_at = OffsetDateTime::now_utc();
            }
        }
        GuiActionInfo {
            id: Uuid::now_v7().to_string(),
            session_id: ticket.session_id.clone(),
            agent_id: ticket.agent_id.clone(),
            agent_name: ticket.agent_name.clone(),
            surface: ticket.surface.as_str().to_string(),
            action: outcome.action,
            target_app: outcome.target_app,
            target_bundle_id: outcome.target_bundle_id,
            target_window: outcome.target_window,
            sensitive_target: outcome.sensitive_target,
            snapshot_generation: outcome.snapshot_generation,
            lease_generation: ticket.lease_generation,
            queued_ms: ticket.waited_ms,
            latency_ms: outcome.latency_ms,
            result: outcome.result,
            error_code: outcome.error_code,
            occurred_at: vp_protocol::now_rfc3339(),
        }
    }

    /// Release on `close` or session cleanup. Returns true when this session
    /// actually held the lease.
    pub async fn release_session(&self, session_id: &str, reason: &str) -> bool {
        let holder = {
            let mut state = self.state.lock().await;
            if state
                .holder
                .as_ref()
                .is_some_and(|holder| holder.session_id == session_id)
            {
                state.holder.take()
            } else {
                None
            }
        };
        match holder {
            Some(holder) => {
                self.release_holder(holder, reason).await;
                true
            }
            None => false,
        }
    }

    /// Release holders whose grace elapsed. Called by the daemon's short GUI
    /// sweeper and opportunistically before every acquisition.
    pub async fn sweep(&self) -> usize {
        let expired = {
            let mut state = self.state.lock().await;
            if state.releasing {
                Vec::new()
            } else {
                match state.holder.as_ref() {
                    Some(holder) if holder.last_action.elapsed() >= holder.grace => {
                        state.holder.take().into_iter().collect::<Vec<_>>()
                    }
                    _ => Vec::new(),
                }
            }
        };
        let mut released = 0usize;
        for holder in expired {
            self.release_holder(holder, "grace_expired").await;
            released += 1;
        }
        released
    }

    async fn release_holder(&self, holder: GuiHolder, reason: &str) {
        let GuiHolder {
            guard,
            lease_id,
            session_id,
            ..
        } = holder;
        {
            let mut state = self.state.lock().await;
            state.releasing = true;
        }
        if let Some(guard) = guard {
            let _ = lease_id;
            guard.release().await;
        }
        let mut state = self.state.lock().await;
        state.releasing = false;
        wake_next(&mut state);
        drop(state);
        tracing::debug!(%session_id, reason, "released GUI session lease");
    }

    /// Operator-visible holder and FIFO. Timestamps are informational; the
    /// SQLite row remains the authority for acquisition.
    pub async fn status(&self) -> (Option<GuiLeaseInfo>, Vec<GuiQueueInfo>) {
        let state = self.state.lock().await;
        let holder = state.holder.as_ref().map(|holder| GuiLeaseInfo {
            lease_id: holder.lease_id.clone(),
            owner_id: String::new(),
            session_id: holder.session_id.clone(),
            agent_id: holder.agent_id.clone(),
            agent_name: holder.agent_name.clone(),
            surface: holder.surface.as_str().to_string(),
            lease_generation: holder.generation,
            acquired_at: format_time(holder.acquired_at),
            last_action_at: format_time(holder.last_action_at),
            expires_at: format_time(
                holder.last_action_at + time::Duration::seconds(holder.grace.as_secs() as i64),
            ),
            queue_position: None,
        });
        let now = Instant::now();
        let queue = state
            .waiters
            .iter()
            .enumerate()
            .map(|(index, waiter)| GuiQueueInfo {
                session_id: waiter.session_id.clone(),
                agent_id: waiter.agent_id.clone(),
                agent_name: waiter.agent_name.clone(),
                surface: waiter.surface.as_str().to_string(),
                position: index as u32 + 1,
                waiting_ms: now
                    .saturating_duration_since(waiter.enqueued_at)
                    .as_millis() as u64,
            })
            .collect();
        (holder, queue)
    }

    pub async fn holder_session(&self) -> Option<String> {
        self.state
            .lock()
            .await
            .holder
            .as_ref()
            .map(|holder| holder.session_id.clone())
    }

    pub async fn lease_generation(&self) -> u64 {
        self.state.lock().await.generation
    }
}

fn wake_next(state: &mut GuiState) {
    while let Some(waiter) = state.waiters.pop_front() {
        if waiter.notify.send(()).is_ok() {
            break;
        }
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis() as u64
}

fn format_time(value: OffsetDateTime) -> String {
    value
        .format(&Rfc3339)
        .unwrap_or_else(|_| vp_protocol::now_rfc3339())
}

fn busy_from_store(
    holder: Option<vp_store::store::ComputerLease>,
    policy: &GuiPolicy,
    started: Instant,
    queue_position: Option<u32>,
    wait_ms: u64,
) -> GuiBusy {
    let mut retry_after_ms = 250u64;
    let mut holder_session = None;
    let mut holder_agent = None;
    let mut holder_agent_name = None;
    let mut holder_surface = None;
    let mut expires_at = None;
    if let Some(holder) = holder {
        holder_session = Some(holder.session_id.clone());
        holder_agent = Some(holder.agent_id.clone());
        holder_agent_name = Some(holder.agent_name.clone());
        holder_surface = Some(holder.surface.clone());
        expires_at = Some(holder.expires_at.clone());
        if let Ok(expiry) = OffsetDateTime::parse(&holder.expires_at, &Rfc3339) {
            let remaining = (expiry - OffsetDateTime::now_utc()).whole_milliseconds();
            if remaining > 0 {
                retry_after_ms = (remaining as u64).min(wait_ms.max(250));
            }
        }
    }
    GuiBusy {
        code: "GUI_BUSY",
        message: match &holder_session {
            Some(session) => format!(
                "the GUI surface is held by session {session} ({})",
                holder_agent_name.as_deref().unwrap_or("agent")
            ),
            None => "the GUI surface is busy in another runtime".to_string(),
        },
        holder_session_id: holder_session,
        holder_agent_id: holder_agent,
        holder_agent_name,
        holder_surface,
        holder_expires_at: expires_at,
        retry_after_ms,
        waited_ms: elapsed_ms(started),
        wait_ms: policy.wait_ms,
        queue_position,
    }
}

/// Effective sensitive bundle list: built-ins first, then project additions.
pub fn sensitive_bundles(policy: &GuiPolicy) -> Vec<String> {
    let mut bundles = DEFAULT_SENSITIVE_BUNDLES
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    for bundle in &policy.sensitive_bundles {
        let normalized = bundle.trim().to_ascii_lowercase();
        if !normalized.is_empty() && !bundles.contains(&normalized) {
            bundles.push(normalized);
        }
    }
    bundles
}

/// Effective sensitive app/title patterns.
pub fn sensitive_titles(policy: &GuiPolicy) -> Vec<String> {
    let mut titles = DEFAULT_SENSITIVE_TITLES
        .iter()
        .map(|value| value.to_string())
        .collect::<Vec<_>>();
    for title in &policy.sensitive_titles {
        let normalized = title.trim().to_ascii_lowercase();
        if !normalized.is_empty() && !titles.contains(&normalized) {
            titles.push(normalized);
        }
    }
    titles
}

/// True when the app/window is on the sensitive list. Bundle matching is
/// exact; app/title matching is a case-insensitive substring, which catches
/// localized variants such as "System Settings — Privacy & Security".
pub fn is_sensitive_target(
    policy: &GuiPolicy,
    bundle_id: Option<&str>,
    app: Option<&str>,
    title: Option<&str>,
) -> bool {
    if let Some(bundle_id) = bundle_id {
        let bundle_id = bundle_id.trim().to_ascii_lowercase();
        if sensitive_bundles(policy)
            .iter()
            .any(|candidate| candidate == &bundle_id)
        {
            return true;
        }
    }
    let haystacks = [app, title]
        .into_iter()
        .flatten()
        .map(|value| value.to_ascii_lowercase())
        .collect::<Vec<_>>();
    if haystacks.is_empty() {
        return false;
    }
    sensitive_titles(policy).iter().any(|candidate| {
        haystacks
            .iter()
            .any(|haystack| haystack.contains(candidate.as_str()))
    })
}

/// True when the agent explicitly named the target in the tool input. This is
/// what lets the approval layer classify a sensitive target before dispatch.
pub fn explicit_target(input: &Value) -> bool {
    ["bundle_id", "pid", "window_id", "window_title"]
        .iter()
        .any(|key| {
            input.get(*key).is_some_and(|value| {
                !value.is_null() && value.as_str().is_none_or(|text| !text.trim().is_empty())
            })
        })
}

/// True when the input statically names a sensitive target. Used by the
/// permission assessor so a sensitive bundle/title becomes an approval (or a
/// deterministic denial) instead of an ordinary host-state action.
pub fn input_names_sensitive_target(policy: &GuiPolicy, input: &Value) -> bool {
    let bundle = input
        .get("bundle_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let title = input
        .get("window_title")
        .and_then(Value::as_str)
        .or_else(|| input.get("title").and_then(Value::as_str))
        .unwrap_or_default();
    is_sensitive_target(policy, Some(bundle), None, Some(title))
}

/// Structured payload the daemon sends to the helper so the helper can enforce
/// the identical policy before it resolves or captures anything.
pub fn helper_sensitive_params(policy: &GuiPolicy, input: &Value, params: &mut Value) {
    params["sensitive_mode"] = Value::String(policy.sensitive_mode.as_str().to_string());
    params["sensitive_bundles"] = serde_json::json!(sensitive_bundles(policy));
    params["sensitive_titles"] = serde_json::json!(sensitive_titles(policy));
    params["sensitive_ack"] = Value::Bool(explicit_target(input));
    if policy.sensitive_mode == GuiSensitiveMode::Deny {
        params["sensitive_deny"] = Value::Bool(true);
    }
}

/// Build the typed error returned when a helper rejects a sensitive target.
pub fn sensitive_target_output(action: &str, message: &str, details: Option<&Value>) -> Value {
    let mut object = serde_json::json!({
        "ok": false,
        "code": "SENSITIVE_TARGET",
        "action": action,
        "message": message,
        "retryable": false,
        "hint": "Sensitive targets require an explicit bundle_id/window target and operator approval, or a project policy that changes the sensitive list. Do not bypass this by using coordinates.",
        "lease_surface": GUI_SURFACE,
    });
    if let Some(details) = details
        && let Some(map) = object.as_object_mut()
    {
        map.insert("details".to_string(), details.clone());
    }
    object
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_defaults_are_bounded_and_safe() {
        let policy = GuiPolicy::default();
        assert_eq!(policy.grace_secs, 45);
        assert_eq!(policy.wait_ms, 15_000);
        assert_eq!(policy.status_ttl_secs, 30);
        assert_eq!(policy.sensitive_mode, GuiSensitiveMode::Deny);
        assert_eq!(policy.clone().normalized().grace_secs, 45);
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn policy_validation_rejects_out_of_range_values() {
        let mut policy = GuiPolicy {
            grace_secs: 601,
            ..GuiPolicy::default()
        };
        assert!(policy.validate().is_err());
        policy.grace_secs = 0;
        policy.wait_ms = 60_001;
        assert!(policy.validate().is_err());
        policy.wait_ms = 0;
        policy.status_ttl_secs = 4;
        assert!(policy.validate().is_err());
        policy.status_ttl_secs = 601;
        assert!(policy.validate().is_err());
        policy.status_ttl_secs = 30;
        policy.sensitive_bundles = (0..65).map(|index| format!("b{index}")).collect();
        assert!(policy.validate().is_err());
    }

    #[test]
    fn sensitive_matching_covers_bundles_titles_and_extensions() {
        let mut policy = GuiPolicy::default();
        assert!(is_sensitive_target(
            &policy,
            Some("com.apple.SecurityAgent"),
            None,
            None
        ));
        assert!(is_sensitive_target(
            &policy,
            None,
            Some("System Settings"),
            None
        ));
        assert!(is_sensitive_target(
            &policy,
            None,
            Some("Safari"),
            Some("Privacy & Security")
        ));
        assert!(!is_sensitive_target(
            &policy,
            Some("com.apple.Safari"),
            Some("Safari"),
            Some("Example")
        ));
        policy.sensitive_bundles = vec!["com.example.Secret".to_string()];
        policy.sensitive_titles = vec!["vault".to_string()];
        assert!(is_sensitive_target(
            &policy,
            Some("com.example.secret"),
            None,
            None
        ));
        assert!(is_sensitive_target(&policy, None, Some("My Vault"), None));
    }

    #[test]
    fn explicit_target_detects_named_fields_only() {
        assert!(!explicit_target(
            &serde_json::json!({"action": "click", "ref": "ax-1"})
        ));
        assert!(explicit_target(
            &serde_json::json!({"bundle_id": "com.example.App"})
        ));
        assert!(explicit_target(&serde_json::json!({"pid": 42})));
        assert!(explicit_target(
            &serde_json::json!({"window_title": "Main"})
        ));
        assert!(!explicit_target(&serde_json::json!({"bundle_id": "  "})));
    }

    #[test]
    fn helper_params_carry_the_policy_and_ack() {
        let policy = GuiPolicy {
            sensitive_mode: GuiSensitiveMode::RequireApproval,
            ..GuiPolicy::default()
        };
        let mut params = serde_json::json!({});
        helper_sensitive_params(
            &policy,
            &serde_json::json!({"bundle_id": "com.example.App"}),
            &mut params,
        );
        assert_eq!(params["sensitive_mode"], "require_approval");
        assert_eq!(params["sensitive_ack"], true);
        assert!(params["sensitive_bundles"].as_array().unwrap().len() >= 10);
        assert!(params.get("sensitive_deny").is_none());
    }

    #[test]
    fn busy_output_is_typed_and_carries_the_holder() {
        let busy = GuiBusy {
            code: "GUI_BUSY",
            message: "held".into(),
            holder_session_id: Some("ses-1".into()),
            holder_agent_id: Some("agent-1".into()),
            holder_agent_name: Some("Agent One".into()),
            holder_surface: Some("browser".into()),
            holder_expires_at: Some("2026-11-11T00:00:00Z".into()),
            retry_after_ms: 1200,
            waited_ms: 15000,
            wait_ms: 15000,
            queue_position: Some(2),
        };
        let value = busy.to_structured("click");
        assert_eq!(value["code"], "GUI_BUSY");
        assert_eq!(value["holder"]["session_id"], "ses-1");
        assert_eq!(value["retry_after_ms"], 1200);
        assert_eq!(value["lease_surface"], "gui");
        assert!(busy.to_string().contains("retry after 1200 ms"));
    }

    #[test]
    fn sensitive_target_output_is_fail_closed() {
        let output = sensitive_target_output(
            "snapshot",
            "target is sensitive",
            Some(&serde_json::json!({"bundle_id": "com.apple.SecurityAgent"})),
        );
        assert_eq!(output["code"], "SENSITIVE_TARGET");
        assert_eq!(output["retryable"], false);
        assert_eq!(output["details"]["bundle_id"], "com.apple.SecurityAgent");
    }
}

#[cfg(test)]
mod coordinator_tests {
    use super::*;
    use std::path::Path;
    use std::process::Command;
    use tempfile::TempDir;
    use vp_protocol::types::GuiSensitiveMode;
    use vp_store::Store;

    fn init_repo(path: &Path) {
        let status = Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        std::fs::write(path.join("README.md"), "gui session\n").unwrap();
        let status = Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
        let status = Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "-m",
                "init",
            ])
            .current_dir(path)
            .status()
            .unwrap();
        assert!(status.success());
    }

    async fn fixture(
        label: &str,
        grace_secs: u64,
        wait_ms: u64,
    ) -> (TempDir, TempDir, Engine, String, String, String, GuiPolicy) {
        let data = TempDir::new().unwrap();
        let repo = TempDir::new().unwrap();
        init_repo(repo.path());
        let engine = Engine::new(Store::open(data.path()).unwrap());
        let project = engine
            .add_project(
                repo.path().to_string_lossy().to_string(),
                Some(label.into()),
            )
            .await
            .unwrap();
        let first = engine
            .create_session(project.id.clone(), "first session".into(), None)
            .await
            .unwrap();
        let second = engine
            .create_session(project.id.clone(), "second session".into(), None)
            .await
            .unwrap();
        let policy = GuiPolicy {
            grace_secs,
            wait_ms,
            ..GuiPolicy::default()
        };
        (data, repo, engine, first.id, second.id, project.id, policy)
    }

    #[tokio::test]
    async fn one_gui_lease_serializes_browser_and_computer() {
        let (_data, _repo, engine, first, second, _project, policy) =
            fixture("gui unite", 30, 150).await;
        let browser = engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Browser,
                &first,
                "agent-browser",
                "Browser Agent",
            )
            .await
            .unwrap();
        let browser = match browser {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(busy) => panic!("browser was busy: {busy}"),
        };
        assert_eq!(browser.surface, GuiSurface::Browser);
        assert_eq!(browser.lease_generation, 1);
        let started = Instant::now();
        let computer = engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Computer,
                &second,
                "agent-computer",
                "Computer Agent",
            )
            .await
            .unwrap();
        match computer {
            GuiAcquire::Busy(busy) => {
                assert_eq!(busy.code, "GUI_BUSY");
                assert_eq!(busy.holder_session_id.as_deref(), Some(first.as_str()));
                assert_eq!(busy.holder_surface.as_deref(), Some("browser"));
                assert!(busy.retry_after_ms > 0);
                assert_eq!(busy.wait_ms, 150);
                assert!(busy.waited_ms >= 140);
            }
            GuiAcquire::Acquired(_) => panic!("computer drove the GUI concurrently"),
        }
        assert!(started.elapsed() >= Duration::from_millis(140));
        let leases = engine.list_computer_leases().await.unwrap();
        assert_eq!(leases.len(), 1);
        assert_eq!(leases[0].surface, GUI_SURFACE);
        assert_eq!(leases[0].session_id, first);

        assert!(engine.gui_end_session(&first, "test_release").await);
        let computer = engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Computer,
                &second,
                "agent-computer",
                "Computer Agent",
            )
            .await
            .unwrap();
        let computer = match computer {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(busy) => panic!("computer was still busy: {busy}"),
        };
        assert_eq!(computer.surface, GuiSurface::Computer);
        assert_eq!(computer.lease_generation, 2);
    }

    #[tokio::test]
    async fn waiters_are_fifo_and_wake_after_release() {
        let (_data, _repo, engine, first, second, project, policy) =
            fixture("gui fifo", 30, 3_000).await;
        let third = engine
            .create_session(project, "third session".into(), None)
            .await
            .unwrap();
        let held = engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &first, "a", "A")
            .await
            .unwrap();
        let held = match held {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(_) => panic!("first was busy"),
        };
        let waiter_engine = engine.clone();
        let waiter_policy = policy.clone();
        let second_id = second.clone();
        let waiter_b = tokio::spawn(async move {
            waiter_engine
                .gui_begin_action_with_policy(
                    &waiter_policy,
                    GuiSurface::Computer,
                    &second_id,
                    "b",
                    "B",
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(120)).await;
        let third_engine = engine.clone();
        let third_policy = policy.clone();
        let third_id = third.id.clone();
        let waiter_c = tokio::spawn(async move {
            third_engine
                .gui_begin_action_with_policy(
                    &third_policy,
                    GuiSurface::Browser,
                    &third_id,
                    "c",
                    "C",
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(120)).await;
        assert!(engine.gui_end_session(&first, "handoff").await);
        let b = tokio::time::timeout(Duration::from_secs(2), waiter_b)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let b = match b {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(busy) => panic!("second waiter timed out: {busy}"),
        };
        assert_eq!(b.session_id, second);
        assert!(engine.gui_end_session(&second, "handoff").await);
        let c = tokio::time::timeout(Duration::from_secs(2), waiter_c)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let c = match c {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(busy) => panic!("third waiter timed out: {busy}"),
        };
        assert_eq!(c.session_id, third.id);
        engine.gui_end_session(&held.session_id, "cleanup").await;
        engine.gui_end_session(&c.session_id, "cleanup").await;
    }

    #[tokio::test]
    async fn same_session_reentry_keeps_generation_and_switches_surface() {
        let (_data, _repo, engine, first, _second, _project, policy) =
            fixture("gui reentry", 30, 100).await;
        let ticket = engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &first, "agent-1", "One")
            .await
            .unwrap();
        let ticket = match ticket {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(_) => panic!("first was busy"),
        };
        let again = engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Browser, &first, "agent-2", "Two")
            .await
            .unwrap();
        let again = match again {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(busy) => panic!("reentry was busy: {busy}"),
        };
        assert_eq!(again.lease_generation, ticket.lease_generation);
        assert_eq!(again.lease_id, ticket.lease_id);
        assert_eq!(again.surface, GuiSurface::Browser);
        let status = engine.gui.status().await;
        assert_eq!(status.0.unwrap().agent_name, "Two");
        assert!(status.1.is_empty());
    }

    #[tokio::test]
    async fn grace_expiry_releases_and_the_next_action_reacquires() {
        let (_data, _repo, engine, first, _second, _project, mut policy) =
            fixture("gui grace", 0, 100).await;
        policy.grace_secs = 0;
        let ticket = engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &first, "agent", "Agent")
            .await
            .unwrap();
        let ticket = match ticket {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(_) => panic!("first was busy"),
        };
        let mut outcome = GuiActionOutcome::new("apps");
        outcome.latency_ms = 4;
        let action = engine.gui_finish_action(&ticket, outcome).await.unwrap();
        assert_eq!(action.action, "apps");
        assert_eq!(engine.list_computer_leases().await.unwrap().len(), 1);
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(engine.sweep_gui_grace().await, 1);
        assert!(engine.list_computer_leases().await.unwrap().is_empty());
        let next = engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &first, "agent", "Agent")
            .await
            .unwrap();
        let next = match next {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(busy) => panic!("reacquire failed: {busy}"),
        };
        assert_eq!(next.lease_generation, ticket.lease_generation + 1);
        engine.gui_end_session(&first, "cleanup").await;
    }

    #[tokio::test]
    async fn legacy_rows_block_gui_until_retired() {
        let (_data, _repo, engine, first, _second, _project, policy) =
            fixture("gui legacy", 30, 50).await;
        let store = engine.store.clone();
        let attempt = store
            .acquire_computer_lease(
                "computer",
                "legacy-owner",
                &first,
                "legacy-agent",
                "Legacy",
                60,
            )
            .unwrap();
        assert!(matches!(
            attempt,
            vp_store::store::ComputerLeaseAttempt::Acquired { .. }
        ));
        let busy = engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &first, "agent", "Agent")
            .await
            .unwrap();
        match busy {
            GuiAcquire::Busy(busy) => {
                assert_eq!(busy.holder_surface.as_deref(), Some("computer"));
                assert_eq!(busy.holder_agent_name.as_deref(), Some("Legacy"));
                assert!(busy.retry_after_ms > 0);
            }
            GuiAcquire::Acquired(_) => panic!("gui acquired over a legacy row"),
        }
        assert_eq!(engine.retire_legacy_gui_leases().await.unwrap(), 1);
        let acquired = engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &first, "agent", "Agent")
            .await
            .unwrap();
        assert!(matches!(acquired, GuiAcquire::Acquired(_)));
        engine.gui_end_session(&first, "cleanup").await;
    }

    #[tokio::test]
    async fn zero_wait_returns_immediately_with_a_typed_busy() {
        let (_data, _repo, engine, first, second, _project, mut policy) =
            fixture("gui zero wait", 30, 0).await;
        policy.wait_ms = 0;
        let held = engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &first, "a", "A")
            .await
            .unwrap();
        let started = Instant::now();
        let busy = engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Browser, &second, "b", "B")
            .await
            .unwrap();
        assert!(started.elapsed() < Duration::from_millis(250));
        match busy {
            GuiAcquire::Busy(busy) => {
                assert_eq!(busy.code, "GUI_BUSY");
                assert_eq!(busy.queue_position, None);
                assert_eq!(busy.holder_session_id.as_deref(), Some(first.as_str()));
            }
            GuiAcquire::Acquired(_) => panic!("zero wait acquired a held surface"),
        }
        match held {
            GuiAcquire::Acquired(_) => {}
            GuiAcquire::Busy(_) => panic!("first acquire failed"),
        }
        engine.gui_end_session(&first, "cleanup").await;
        assert!(engine.gui.status().await.0.is_none());
    }

    #[tokio::test]
    async fn grace_expiry_wakes_a_waiter_without_an_explicit_release() {
        let (_data, _repo, engine, first, second, _project, mut policy) =
            fixture("gui grace waiter", 0, 2_000).await;
        policy.grace_secs = 0;
        let ticket = match engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &first, "a", "A")
            .await
            .unwrap()
        {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(busy) => panic!("first was busy: {busy}"),
        };
        let waiter_engine = engine.clone();
        let waiter_policy = policy.clone();
        let second_id = second.clone();
        let waiter = tokio::spawn(async move {
            waiter_engine
                .gui_begin_action_with_policy(
                    &waiter_policy,
                    GuiSurface::Browser,
                    &second_id,
                    "b",
                    "B",
                )
                .await
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut outcome = GuiActionOutcome::new("apps");
        outcome.latency_ms = 2;
        engine.gui_finish_action(&ticket, outcome).await.unwrap();
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert_eq!(engine.sweep_gui_grace().await, 1);
        let waiter = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        match waiter {
            GuiAcquire::Acquired(ticket) => {
                assert_eq!(ticket.session_id, second);
                assert_eq!(ticket.surface, GuiSurface::Browser);
            }
            GuiAcquire::Busy(busy) => panic!("waiter did not wake after grace: {busy}"),
        }
        engine.gui_end_session(&second, "cleanup").await;
    }

    #[tokio::test]
    async fn bounded_queue_rejects_the_sixty_fifth_waiter() {
        let (_data, _repo, engine, first, second, project, mut policy) =
            fixture("gui queue bound", 30, 30_000).await;
        policy.grace_secs = 30;
        policy.wait_ms = 30_000;
        let mut sessions = Vec::new();
        for _ in 0..64 {
            sessions.push(
                engine
                    .create_session(project.clone(), "queued".into(), None)
                    .await
                    .unwrap(),
            );
        }
        let _held = match engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &first, "holder", "Holder")
            .await
            .unwrap()
        {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(busy) => panic!("holder was busy: {busy}"),
        };
        let mut waiters = Vec::new();
        for session in sessions {
            let waiter_engine = engine.clone();
            let waiter_policy = policy.clone();
            waiters.push(tokio::spawn(async move {
                waiter_engine
                    .gui_begin_action_with_policy(
                        &waiter_policy,
                        GuiSurface::Computer,
                        &session.id,
                        "waiter",
                        "Waiter",
                    )
                    .await
            }));
        }
        // Give every waiter a chance to enter the FIFO before checking the
        // bound. The holder is still held, so none can acquire.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let overflow = engine
            .gui_begin_action_with_policy(
                &policy,
                GuiSurface::Computer,
                &second,
                "overflow",
                "Overflow",
            )
            .await
            .unwrap();
        match overflow {
            GuiAcquire::Busy(busy) => {
                assert_eq!(busy.code, "GUI_QUEUE_FULL", "{busy:?}");
                assert!(busy.message.contains("64"));
            }
            GuiAcquire::Acquired(_) => panic!("overflow acquired the held surface"),
        }
        for waiter in waiters {
            waiter.abort();
        }
        engine.gui_end_session(&first, "cleanup").await;
    }

    #[tokio::test]
    async fn timeline_events_are_bounded_and_redact_sensitive_titles() {
        let (_data, _repo, engine, first, _second, project, policy) =
            fixture("gui timeline", 30, 50).await;
        let ticket = engine
            .gui_begin_action_with_policy(&policy, GuiSurface::Computer, &first, "agent", "Agent")
            .await
            .unwrap();
        let ticket = match ticket {
            GuiAcquire::Acquired(ticket) => ticket,
            GuiAcquire::Busy(_) => panic!("first was busy"),
        };
        let mut outcome = GuiActionOutcome::new("type");
        outcome.target_app = Some("System Settings".into());
        outcome.target_bundle_id = Some("com.apple.systempreferences".into());
        outcome.target_window = None;
        outcome.sensitive_target = true;
        outcome.snapshot_generation = 9;
        outcome.latency_ms = 33;
        outcome.result = "error".into();
        outcome.error_code = Some("SENSITIVE_TARGET".into());
        engine.gui_finish_action(&ticket, outcome).await.unwrap();
        let actions = engine.gui_timeline(Some(&first), 10).await.unwrap();
        assert_eq!(actions.len(), 1);
        let action = &actions[0];
        assert_eq!(action.action, "type");
        assert!(action.sensitive_target);
        assert!(action.target_window.is_none());
        assert_eq!(action.snapshot_generation, 9);
        assert_eq!(action.lease_generation, ticket.lease_generation);
        assert_eq!(action.error_code.as_deref(), Some("SENSITIVE_TARGET"));
        assert_eq!(action.queued_ms, ticket.waited_ms);
        let value = serde_json::to_value(action).unwrap();
        let text = value.to_string();
        assert!(!text.contains("hunter2"));
        assert!(!text.contains("screen content"));

        for index in 0..12 {
            let mut outcome = GuiActionOutcome::new(format!("click-{index}"));
            outcome.latency_ms = index;
            engine.gui_finish_action(&ticket, outcome).await;
        }
        let limited = engine.gui_timeline(Some(&first), 5).await.unwrap();
        assert_eq!(limited.len(), 5);
        let project_actions = engine.gui_timeline(None, 500).await.unwrap();
        assert!(project_actions.len() >= 5);
        let _ = project;
    }

    #[tokio::test]
    async fn policy_validation_and_normalization_are_enforced_by_the_engine() {
        let (_data, _repo, engine, _first, _second, project, _policy) =
            fixture("gui policy engine", 30, 50).await;
        let policy = GuiPolicy {
            grace_secs: 601,
            ..GuiPolicy::default()
        };
        assert!(engine.set_gui_policy(&project, policy).await.is_err());
        let policy = GuiPolicy {
            grace_secs: 12,
            wait_ms: 900,
            status_ttl_secs: 11,
            sensitive_mode: GuiSensitiveMode::RequireApproval,
            sensitive_bundles: vec![" COM.Example.Vault ".into()],
            ..GuiPolicy::default()
        };
        let saved = engine.set_gui_policy(&project, policy).await.unwrap();
        assert_eq!(saved.grace_secs, 12);
        assert_eq!(saved.wait_ms, 900);
        assert_eq!(saved.status_ttl_secs, 11);
        assert_eq!(saved.sensitive_bundles, vec!["com.example.vault"]);
        let loaded = engine.get_gui_policy(&project).await.unwrap();
        assert_eq!(loaded, saved);
    }

    #[test]
    fn normalized_policy_clamps_limits_before_validation() {
        let policy = GuiPolicy {
            grace_secs: 9_999,
            wait_ms: 999_999,
            status_ttl_secs: 1,
            sensitive_bundles: vec![" COM.Example.Vault ".into(), "".into()],
            ..GuiPolicy::default()
        }
        .normalized();
        assert_eq!(policy.grace_secs, GuiPolicy::MAX_GRACE_SECS);
        assert_eq!(policy.wait_ms, GuiPolicy::MAX_WAIT_MS);
        assert_eq!(policy.status_ttl_secs, GuiPolicy::MIN_STATUS_TTL_SECS);
        assert_eq!(policy.sensitive_bundles, vec!["com.example.vault"]);
        assert!(policy.validate().is_ok());
    }

    #[test]
    fn surface_names_are_stable_and_tool_names_map_explicitly() {
        assert_eq!(GuiSurface::Browser.as_str(), "browser");
        assert_eq!(GuiSurface::Computer.as_str(), "computer");
        assert_eq!(GuiSurface::from_tool("browser"), Some(GuiSurface::Browser));
        assert_eq!(
            GuiSurface::from_tool("computer"),
            Some(GuiSurface::Computer)
        );
        assert_eq!(GuiSurface::from_tool("gui"), None);
        assert_eq!(GuiSurface::from_tool("shell"), None);
    }

    #[test]
    fn legacy_surface_names_are_documented_and_bounded() {
        assert_eq!(LEGACY_GUI_SURFACES, ["browser", "computer"]);
        assert_eq!(GUI_SURFACE, "gui");
        assert_eq!(GUI_LEASE_TTL_SECS, 120);
        assert_eq!(MAX_GUI_WAITERS, 64);
    }
}
