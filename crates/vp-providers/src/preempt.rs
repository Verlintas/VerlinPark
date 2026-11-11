//! Hard preemption primitives shared by provider adapters and the runtime.
//!
//! A [`PreemptToken`] is a bounded, cloneable cancellation signal with an
//! optional wall-clock deadline. It is deliberately separate from the
//! cooperative pause/cancel watch used by the operator surface:
//!
//! - cooperative cancel asks the run loop to stop at its next boundary;
//! - a preemption token aborts the in-flight provider request or tool call and
//!   records why, in which phase, and whether side effects are unknown.
//!
//! Tokens never replay a partial turn, never resume a stream, and never claim
//! exactly-once side effects. They are process-local: a cloned token shares
//! one trigger state inside one daemon process.
//!
//! The primitive lives in `vp-providers` because the [`crate::Provider`] trait
//! accepts it directly. `vp-core::preemption` re-exports it together with the
//! runtime policy and reporting helpers.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use tokio::sync::Notify;

/// Why a run or tool call was preempted.
///
/// `ToolDeadline` is scoped to one tool call; it does not stop the run.
/// `WallClock` and `Explicit` stop the current run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreemptReason {
    /// The session wall-clock budget ran out while a provider or tool call was
    /// in flight.
    WallClock,
    /// An operator called `session.preempt`.
    Explicit,
    /// A project policy set a per-tool deadline and the tool overran it.
    ToolDeadline,
}

impl PreemptReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::WallClock => "wall_clock",
            Self::Explicit => "explicit",
            Self::ToolDeadline => "tool_deadline",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "wall_clock" => Some(Self::WallClock),
            "explicit" => Some(Self::Explicit),
            "tool_deadline" => Some(Self::ToolDeadline),
            _ => None,
        }
    }

    /// True when this reason terminates the whole run instead of only one tool.
    pub const fn is_run_terminal(self) -> bool {
        matches!(self, Self::WallClock | Self::Explicit)
    }
}

impl fmt::Display for PreemptReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Phase in which the preemption was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreemptPhase {
    /// Waiting for or reading a provider stream.
    Provider,
    /// Running a tool call.
    Tool,
    /// Waiting for admission, a write lease, a capacity slot, or an approval.
    Queue,
}

impl PreemptPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Provider => "provider",
            Self::Tool => "tool",
            Self::Queue => "queue",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "provider" => Some(Self::Provider),
            "tool" => Some(Self::Tool),
            "queue" => Some(Self::Queue),
            _ => None,
        }
    }
}

impl fmt::Display for PreemptPhase {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Bounded, serializable snapshot of one preemption. It intentionally carries
/// no prompt text, tool input, file contents, or credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreemptSnapshot {
    pub reason: PreemptReason,
    pub phase: PreemptPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
    #[serde(default)]
    pub elapsed_ms: u64,
    #[serde(default)]
    pub side_effects_unknown: bool,
    #[serde(default)]
    pub partial_input_tokens: u64,
    #[serde(default)]
    pub partial_output_tokens: u64,
    #[serde(default)]
    pub partial_output_bytes: u64,
    #[serde(default)]
    pub usage_estimated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl PreemptSnapshot {
    /// A short, safe diagnostic. No raw output or input is included.
    pub fn summary(&self) -> String {
        let mut text = format!(
            "{} preempted during {} after {}ms",
            self.reason, self.phase, self.elapsed_ms
        );
        if let Some(tool) = &self.tool {
            text.push_str(&format!(" ({tool})"));
        }
        if self.side_effects_unknown {
            text.push_str("; side effects are unknown");
        }
        text
    }
}

#[derive(Debug)]
struct PreemptInner {
    triggered: AtomicBool,
    snapshot: Mutex<Option<PreemptSnapshot>>,
    phase: Mutex<PhaseSlot>,
    deadline: Mutex<Option<Instant>>,
    tool_grace: Mutex<ToolGrace>,
    started: Instant,
    notify: Notify,
    parent: Option<PreemptToken>,
}

#[derive(Debug, Clone)]
struct PhaseSlot {
    phase: PreemptPhase,
    tool: Option<String>,
}

/// Grace and escalation bounds copied from a project preemption policy. They
/// are durations, not deadlines, so every waiting site remains bounded.
#[derive(Debug, Clone, Copy)]
pub struct ToolGrace {
    pub grace: Duration,
    pub kill_escalation: Duration,
    pub provider_grace: Duration,
}

impl Default for ToolGrace {
    fn default() -> Self {
        Self {
            grace: Duration::from_millis(5_000),
            kill_escalation: Duration::from_millis(2_000),
            provider_grace: Duration::from_millis(2_000),
        }
    }
}

/// Cloneable preemption signal.
///
/// The first trigger wins. Later triggers, including the wall-clock deadline,
/// are no-ops so the recorded reason and phase are stable for one run.
#[derive(Clone)]
pub struct PreemptToken {
    inner: Arc<PreemptInner>,
}

impl fmt::Debug for PreemptToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreemptToken")
            .field("triggered", &self.is_preempted())
            .field("snapshot", &self.snapshot())
            .finish()
    }
}

impl PreemptToken {
    /// A token with no deadline. It still accepts an explicit operator
    /// preemption, which is why the runtime uses it even when the project
    /// policy disables automatic wall-clock preemption.
    pub fn disabled() -> Self {
        Self::new(None)
    }

    /// A token that fires when the optional wall-clock deadline elapses.
    pub fn new(deadline: Option<Instant>) -> Self {
        Self {
            inner: Arc::new(PreemptInner {
                triggered: AtomicBool::new(false),
                snapshot: Mutex::new(None),
                phase: Mutex::new(PhaseSlot {
                    phase: PreemptPhase::Provider,
                    tool: None,
                }),
                deadline: Mutex::new(deadline),
                tool_grace: Mutex::new(ToolGrace::default()),
                started: Instant::now(),
                notify: Notify::new(),
                parent: None,
            }),
        }
    }

    /// A token whose deadline is relative to now. `None` disables the
    /// deadline; zero is normalized to one millisecond so a misconfigured
    /// policy cannot become an accidental infinite grace.
    pub fn with_timeout(timeout: Option<Duration>) -> Self {
        let deadline =
            timeout.map(|timeout| Instant::now() + timeout.max(Duration::from_millis(1)));
        Self::new(deadline)
    }

    /// Derive a tool-scoped token that also observes its parent run token.
    /// The child's own deadline produces [`PreemptReason::ToolDeadline`]; a
    /// parent preemption propagates its original reason and phase.
    pub fn tool_child(parent: &PreemptToken, timeout: Option<Duration>) -> Self {
        let grace = *parent
            .inner
            .tool_grace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let deadline =
            timeout.map(|timeout| Instant::now() + timeout.max(Duration::from_millis(1)));
        Self {
            inner: Arc::new(PreemptInner {
                triggered: AtomicBool::new(false),
                snapshot: Mutex::new(None),
                phase: Mutex::new(PhaseSlot {
                    phase: PreemptPhase::Tool,
                    tool: None,
                }),
                deadline: Mutex::new(deadline),
                tool_grace: Mutex::new(grace),
                started: Instant::now(),
                notify: Notify::new(),
                parent: Some(parent.clone()),
            }),
        }
    }

    /// Copy the bounded grace durations from a project policy. Child tool
    /// tokens inherit them at construction time.
    pub fn set_grace(&self, grace: ToolGrace) {
        if let Ok(mut slot) = self.inner.tool_grace.lock() {
            *slot = grace;
        }
        self.inner.notify.notify_waiters();
    }

    pub fn grace(&self) -> ToolGrace {
        *self
            .inner
            .tool_grace
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Update or clear the wall-clock deadline. This wakes waiters so a token
    /// created before the session budget was known still observes its exact
    /// deadline.
    pub fn set_deadline(&self, deadline: Option<Instant>) {
        if let Ok(mut slot) = self.inner.deadline.lock() {
            *slot = deadline;
        }
        self.inner.notify.notify_waiters();
    }

    pub fn deadline(&self) -> Option<Instant> {
        *self
            .inner
            .deadline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Record the current phase so a later operator or deadline trigger is
    /// attributed to the operation that was actually in flight.
    pub fn set_phase(&self, phase: PreemptPhase, tool: Option<&str>) {
        if let Ok(mut slot) = self.inner.phase.lock() {
            slot.phase = phase;
            slot.tool = tool.map(|name| name.chars().take(120).collect());
        }
    }

    pub fn phase(&self) -> PreemptPhase {
        self.inner
            .phase
            .lock()
            .map(|slot| slot.phase)
            .unwrap_or(PreemptPhase::Provider)
    }

    fn current_tool(&self) -> Option<String> {
        self.inner
            .phase
            .lock()
            .ok()
            .and_then(|slot| slot.tool.clone())
    }

    /// Trigger once. Returns the stable snapshot. The first caller wins even
    /// when several phases race.
    pub fn trigger(
        &self,
        reason: PreemptReason,
        phase: PreemptPhase,
        side_effects_unknown: bool,
    ) -> PreemptSnapshot {
        if let Some(parent) = &self.inner.parent
            && parent.is_preempted()
        {
            return parent
                .snapshot()
                .unwrap_or_else(|| parent.trigger(reason, phase, side_effects_unknown));
        }
        if self.inner.triggered.swap(true, Ordering::SeqCst) {
            return self.snapshot().unwrap_or_else(|| PreemptSnapshot {
                reason,
                phase,
                tool: None,
                elapsed_ms: self.elapsed_ms(),
                side_effects_unknown,
                partial_input_tokens: 0,
                partial_output_tokens: 0,
                partial_output_bytes: 0,
                usage_estimated: false,
                detail: None,
            });
        }
        let tool = self.current_tool();
        let snapshot = PreemptSnapshot {
            reason,
            phase,
            tool,
            elapsed_ms: self.elapsed_ms(),
            side_effects_unknown,
            partial_input_tokens: 0,
            partial_output_tokens: 0,
            partial_output_bytes: 0,
            usage_estimated: false,
            detail: None,
        };
        if let Ok(mut slot) = self.inner.snapshot.lock() {
            *slot = Some(snapshot.clone());
        }
        self.inner.notify.notify_waiters();
        snapshot
    }

    /// Attach honest partial-usage accounting after a provider stream or tool
    /// output was cut short. Values are additive across repeated calls.
    pub fn annotate_usage(
        &self,
        input_tokens: u64,
        output_tokens: u64,
        output_bytes: u64,
        estimated: bool,
    ) {
        if let Ok(mut slot) = self.inner.snapshot.lock()
            && let Some(snapshot) = slot.as_mut()
        {
            snapshot.partial_input_tokens =
                snapshot.partial_input_tokens.saturating_add(input_tokens);
            snapshot.partial_output_tokens =
                snapshot.partial_output_tokens.saturating_add(output_tokens);
            snapshot.partial_output_bytes =
                snapshot.partial_output_bytes.saturating_add(output_bytes);
            snapshot.usage_estimated |= estimated;
        }
    }

    /// Attach a bounded, sanitized diagnostic. The runtime passes only stable
    /// reasons such as `process_group_terminated`, never command output.
    pub fn annotate_detail(&self, detail: impl Into<String>) {
        let detail = detail.into();
        let bounded = detail.chars().take(300).collect::<String>();
        if let Ok(mut slot) = self.inner.snapshot.lock()
            && let Some(snapshot) = slot.as_mut()
        {
            snapshot.detail = Some(bounded);
        }
    }

    /// Mark whether side effects may have happened. Only `true` is sticky:
    /// an earlier conservative value is never widened back to `false`.
    pub fn mark_side_effects_unknown(&self) {
        if let Ok(mut slot) = self.inner.snapshot.lock()
            && let Some(snapshot) = slot.as_mut()
        {
            snapshot.side_effects_unknown = true;
        }
    }

    /// The effective snapshot: the child's own trigger first, then the parent.
    /// A tool child that only observes a parent preemption reports the
    /// parent's reason and phase, not `ToolDeadline`.
    pub fn snapshot(&self) -> Option<PreemptSnapshot> {
        if let Ok(slot) = self.inner.snapshot.lock()
            && let Some(snapshot) = slot.as_ref()
        {
            return Some(snapshot.clone());
        }
        self.inner
            .parent
            .as_ref()
            .and_then(|parent| parent.snapshot())
    }

    /// True when this exact token (not a parent) fired.
    pub fn is_own_preempted(&self) -> bool {
        self.inner.triggered.load(Ordering::SeqCst)
    }

    pub fn is_preempted(&self) -> bool {
        self.is_own_preempted()
            || self
                .inner
                .parent
                .as_ref()
                .is_some_and(PreemptToken::is_preempted)
    }

    pub fn reason(&self) -> Option<PreemptReason> {
        self.snapshot().map(|snapshot| snapshot.reason)
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.inner
            .started
            .elapsed()
            .as_millis()
            .min(u128::from(u64::MAX)) as u64
    }

    pub fn started_at(&self) -> Instant {
        self.inner.started
    }

    fn deadline_preemption(&self) -> PreemptSnapshot {
        if self.inner.parent.is_some() {
            self.trigger(PreemptReason::ToolDeadline, PreemptPhase::Tool, true)
        } else {
            self.trigger(
                PreemptReason::WallClock,
                self.phase(),
                self.phase() == PreemptPhase::Tool,
            )
        }
    }

    /// Resolve when this token or its parent is preempted, or when the wall
    /// deadline elapses. A token with no deadline and no parent is pending
    /// forever, which is the correct behavior for a policy that disables
    /// automatic preemption.
    pub async fn cancelled(&self) {
        loop {
            if self.is_preempted() {
                return;
            }
            let deadline = self.deadline();
            let notified = self.inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.is_preempted() {
                return;
            }
            match deadline {
                Some(deadline) => {
                    let now = Instant::now();
                    if now >= deadline {
                        self.deadline_preemption();
                        return;
                    }
                    let sleep = tokio::time::sleep(deadline.saturating_duration_since(now));
                    tokio::pin!(sleep);
                    if let Some(parent) = &self.inner.parent {
                        let mut parent_wait = Box::pin(parent.cancelled());
                        tokio::select! {
                            _ = notified => continue,
                            _ = &mut sleep => {
                                self.deadline_preemption();
                                return;
                            }
                            _ = &mut parent_wait => continue,
                        }
                    } else {
                        tokio::select! {
                            _ = notified => continue,
                            _ = &mut sleep => {
                                self.deadline_preemption();
                                return;
                            }
                        }
                    }
                }
                None => {
                    if let Some(parent) = &self.inner.parent {
                        let mut parent_wait = Box::pin(parent.cancelled());
                        tokio::select! {
                            _ = notified => continue,
                            _ = &mut parent_wait => continue,
                        }
                    } else {
                        // A notify can mean a deadline or grace update, not
                        // necessarily a trigger. Re-check before returning.
                        notified.await;
                        continue;
                    }
                }
            }
        }
    }

    /// Await the preemption snapshot, or `None` when the token is never
    /// triggered. Primarily used by tests and callers that need the reason.
    pub async fn preempted(&self) -> Option<PreemptSnapshot> {
        self.cancelled().await;
        self.snapshot()
    }

    /// Run a future inside this token's task-local scope. Tool and protocol
    /// adapters read [`current`] instead of threading the token through every
    /// signature. Spawned tasks are separate tasks and must open their own
    /// scope.
    pub fn scope<F>(&self, future: F) -> tokio::task::futures::TaskLocalFuture<PreemptToken, F>
    where
        F: std::future::Future,
    {
        CURRENT_PREEMPT.scope(self.clone(), future)
    }

    /// Run a future inside an owned token scope.
    pub async fn scoped<F, T>(&self, future: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        CURRENT_PREEMPT.scope(self.clone(), future).await
    }
}

tokio::task_local! {
    static CURRENT_PREEMPT: PreemptToken;
}

/// The token scoped to the current task, if any. Provider and tool adapters
/// use this to observe preemption without changing their public signatures.
pub fn current() -> Option<PreemptToken> {
    CURRENT_PREEMPT.try_with(PreemptToken::clone).ok()
}

/// Run a future with an explicit token scope.
pub async fn scoped<F, T>(token: PreemptToken, future: F) -> T
where
    F: std::future::Future<Output = T>,
{
    CURRENT_PREEMPT.scope(token, future).await
}

/// A provider error snapshot. Keeping conversion here avoids every adapter
/// duplicating the field mapping.
pub(crate) fn error_snapshot(token: &PreemptToken) -> (String, String, u64, bool) {
    match token.snapshot() {
        Some(snapshot) => (
            snapshot.reason.as_str().to_string(),
            snapshot.phase.as_str().to_string(),
            snapshot.elapsed_ms,
            snapshot.side_effects_unknown,
        ),
        None => (
            PreemptReason::Explicit.as_str().to_string(),
            PreemptPhase::Provider.as_str().to_string(),
            token.elapsed_ms(),
            false,
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_deadline_update_wakes_waiters_without_triggering() {
        let token = PreemptToken::disabled();
        let waiter = token.clone();
        let observed = tokio::spawn(async move {
            waiter.cancelled().await;
            waiter.is_preempted()
        });
        tokio::time::sleep(Duration::from_millis(20)).await;
        token.set_deadline(Some(Instant::now() + Duration::from_secs(30)));
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            !observed.is_finished(),
            "a deadline update is not a preemption"
        );
        token.trigger(PreemptReason::Explicit, PreemptPhase::Provider, false);
        assert!(observed.await.unwrap());
    }

    #[tokio::test]
    async fn clearing_a_deadline_leaves_the_token_pending() {
        let token = PreemptToken::with_timeout(Some(Duration::from_millis(40)));
        token.set_deadline(None);
        assert!(
            tokio::time::timeout(Duration::from_millis(150), token.cancelled())
                .await
                .is_err(),
            "a cleared deadline must not preempt a healthy run"
        );
        assert!(!token.is_preempted());
        token.trigger(PreemptReason::Explicit, PreemptPhase::Queue, false);
        assert_eq!(token.reason(), Some(PreemptReason::Explicit));
    }

    #[tokio::test]
    async fn explicit_trigger_is_idempotent_and_keeps_the_first_reason() {
        let token = PreemptToken::disabled();
        token.set_phase(PreemptPhase::Tool, Some("shell"));
        let first = token.trigger(PreemptReason::Explicit, PreemptPhase::Tool, true);
        let second = token.trigger(PreemptReason::WallClock, PreemptPhase::Provider, false);
        assert_eq!(first, second);
        assert_eq!(first.reason, PreemptReason::Explicit);
        assert_eq!(first.phase, PreemptPhase::Tool);
        assert_eq!(first.tool.as_deref(), Some("shell"));
        assert!(first.side_effects_unknown);
        assert!(token.is_preempted());
    }

    #[tokio::test]
    async fn wall_clock_deadline_fires_the_token() {
        let token = PreemptToken::with_timeout(Some(Duration::from_millis(20)));
        let started = Instant::now();
        token.cancelled().await;
        assert!(started.elapsed() < Duration::from_secs(2));
        let snapshot = token.snapshot().expect("deadline snapshot");
        assert_eq!(snapshot.reason, PreemptReason::WallClock);
        assert!(snapshot.elapsed_ms >= 10);
    }

    #[tokio::test]
    async fn parent_preemption_propagates_to_a_tool_child() {
        let parent = PreemptToken::disabled();
        let child = PreemptToken::tool_child(&parent, Some(Duration::from_secs(30)));
        parent.trigger(PreemptReason::Explicit, PreemptPhase::Provider, false);
        child.cancelled().await;
        let snapshot = child.snapshot().expect("parent snapshot");
        assert_eq!(snapshot.reason, PreemptReason::Explicit);
        assert_eq!(snapshot.phase, PreemptPhase::Provider);
        assert!(!child.is_own_preempted());
    }

    #[tokio::test]
    async fn a_tool_deadline_only_fires_the_child() {
        let parent = PreemptToken::disabled();
        let child = PreemptToken::tool_child(&parent, Some(Duration::from_millis(15)));
        child.cancelled().await;
        assert_eq!(child.reason(), Some(PreemptReason::ToolDeadline));
        assert!(child.is_own_preempted());
        assert!(!parent.is_preempted());
    }

    #[tokio::test]
    async fn task_local_scope_exposes_the_current_token() {
        let token = PreemptToken::disabled();
        let seen = scoped(token.clone(), async { current() }).await;
        assert!(seen.is_some_and(|seen| seen.is_preempted() == token.is_preempted()));
        assert!(current().is_none());
    }

    #[test]
    fn usage_annotations_accumulate_and_are_bounded_to_the_snapshot() {
        let token = PreemptToken::disabled();
        token.trigger(PreemptReason::Explicit, PreemptPhase::Provider, false);
        token.annotate_usage(10, 2, 100, true);
        token.annotate_usage(5, 1, 50, false);
        token.annotate_detail("stream_aborted");
        let snapshot = token.snapshot().unwrap();
        assert_eq!(snapshot.partial_input_tokens, 15);
        assert_eq!(snapshot.partial_output_tokens, 3);
        assert_eq!(snapshot.partial_output_bytes, 150);
        assert!(snapshot.usage_estimated);
        assert_eq!(snapshot.detail.as_deref(), Some("stream_aborted"));
    }

    #[test]
    fn reason_and_phase_round_trip_their_wire_names() {
        for reason in [
            PreemptReason::WallClock,
            PreemptReason::Explicit,
            PreemptReason::ToolDeadline,
        ] {
            assert_eq!(PreemptReason::parse(reason.as_str()), Some(reason));
            assert_eq!(
                serde_json::from_str::<PreemptReason>(&serde_json::to_string(&reason).unwrap())
                    .unwrap(),
                reason
            );
        }
        for phase in [
            PreemptPhase::Provider,
            PreemptPhase::Tool,
            PreemptPhase::Queue,
        ] {
            assert_eq!(PreemptPhase::parse(phase.as_str()), Some(phase));
        }
        assert_eq!(PreemptReason::parse("nope"), None);
    }

    #[tokio::test]
    async fn multiple_waiters_all_resolve_on_one_trigger() {
        let token = PreemptToken::disabled();
        let mut waiters = Vec::new();
        for _ in 0..8 {
            let waiter = token.clone();
            waiters.push(tokio::spawn(async move {
                waiter.cancelled().await;
                waiter.snapshot()
            }));
        }
        tokio::time::sleep(Duration::from_millis(30)).await;
        token.trigger(PreemptReason::Explicit, PreemptPhase::Provider, false);
        for waiter in waiters {
            let snapshot = waiter.await.unwrap().expect("snapshot");
            assert_eq!(snapshot.reason, PreemptReason::Explicit);
        }
    }

    #[test]
    fn tool_child_copies_the_parent_grace_and_has_its_own_elapsed_clock() {
        let parent = PreemptToken::disabled();
        parent.set_grace(ToolGrace {
            grace: Duration::from_millis(123),
            kill_escalation: Duration::from_millis(45),
            provider_grace: Duration::from_millis(67),
        });
        std::thread::sleep(Duration::from_millis(10));
        let child = PreemptToken::tool_child(&parent, Some(Duration::from_secs(5)));
        let grace = child.grace();
        assert_eq!(grace.grace, Duration::from_millis(123));
        assert_eq!(grace.kill_escalation, Duration::from_millis(45));
        assert_eq!(grace.provider_grace, Duration::from_millis(67));
        assert!(child.elapsed_ms() < parent.elapsed_ms());
    }

    #[tokio::test]
    async fn a_parent_tool_trigger_keeps_its_unknown_side_effect_flag_on_children() {
        let parent = PreemptToken::disabled();
        parent.set_phase(PreemptPhase::Tool, Some("shell"));
        let child = PreemptToken::tool_child(&parent, None);
        parent.trigger(PreemptReason::Explicit, PreemptPhase::Tool, true);
        child.cancelled().await;
        let snapshot = child.snapshot().expect("propagated snapshot");
        assert!(snapshot.side_effects_unknown);
        assert_eq!(snapshot.tool.as_deref(), Some("shell"));
    }

    #[test]
    fn phase_updates_are_bounded_and_do_not_trigger() {
        let token = PreemptToken::disabled();
        token.set_phase(PreemptPhase::Tool, Some(&"x".repeat(500)));
        assert_eq!(token.phase(), PreemptPhase::Tool);
        let snapshot = token.trigger(PreemptReason::Explicit, PreemptPhase::Tool, true);
        assert!(snapshot.tool.as_deref().unwrap().len() <= 120);
        assert!(token.is_preempted());
    }

    #[test]
    fn mark_side_effects_unknown_is_sticky() {
        let token = PreemptToken::disabled();
        token.trigger(PreemptReason::Explicit, PreemptPhase::Provider, false);
        assert!(!token.snapshot().unwrap().side_effects_unknown);
        token.mark_side_effects_unknown();
        assert!(token.snapshot().unwrap().side_effects_unknown);
        token.mark_side_effects_unknown();
        assert!(token.snapshot().unwrap().side_effects_unknown);
    }

    #[test]
    fn annotations_only_apply_after_a_trigger_and_are_bounded() {
        let token = PreemptToken::disabled();
        token.annotate_usage(100, 100, 100, true);
        token.annotate_detail("x".repeat(10_000));
        assert!(token.snapshot().is_none());
        token.trigger(PreemptReason::Explicit, PreemptPhase::Provider, false);
        let snapshot = token.snapshot().unwrap();
        assert_eq!(snapshot.partial_input_tokens, 0);
        assert!(snapshot.detail.is_none());
        token.annotate_usage(1, 2, 3, false);
        token.annotate_detail("y".repeat(10_000));
        let snapshot = token.snapshot().unwrap();
        assert_eq!(snapshot.partial_output_tokens, 2);
        assert!(snapshot.detail.as_deref().unwrap().len() <= 300);
    }

    #[test]
    fn snapshot_survives_serde_round_trip() {
        let token = PreemptToken::disabled();
        token.trigger(PreemptReason::ToolDeadline, PreemptPhase::Tool, true);
        token.annotate_usage(3, 4, 5, true);
        token.annotate_detail("process_group_terminated");
        let snapshot = token.snapshot().unwrap();
        let encoded = serde_json::to_value(&snapshot).unwrap();
        assert_eq!(encoded["reason"], "tool_deadline");
        assert_eq!(encoded["phase"], "tool");
        let decoded: PreemptSnapshot = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn metric_rows_are_bounded_and_sorted_by_the_caller() {
        let token = PreemptToken::disabled();
        token.trigger(PreemptReason::WallClock, PreemptPhase::Queue, false);
        let snapshot = token.snapshot().unwrap();
        assert_eq!(snapshot.reason.as_str(), "wall_clock");
        assert_eq!(snapshot.phase.as_str(), "queue");
        assert!(snapshot.reason.is_run_terminal());
        assert!(!PreemptReason::ToolDeadline.is_run_terminal());
    }
}
