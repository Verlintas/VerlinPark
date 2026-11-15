//! In-memory, bounded task projection and state machine.
//!
//! The store never executes a task. It records the host-provided lifecycle,
//! serializes one handler run per task, and fans out already-produced events.
//! All IDs are deterministic counters, not random UUIDs, so tests and replay
//! see stable ordering.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use serde_json::Map;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::{OwnedMutexGuard, broadcast, watch};

use crate::error::{A2aError, A2aResult};
use crate::jsonrpc::JsonRpcId;
use crate::limits::A2aLimits;
use crate::wire::{
    A2aStreamEvent, A2aTask, A2aTaskUpdate, Artifact, Message, TaskArtifactUpdateEvent, TaskState,
    TaskStatus, TaskStatusUpdateEvent,
};

/// Cooperative cancellation for one handler run. Cancellation never aborts a
/// host future; it only sets a flag the host may observe.
#[derive(Debug, Clone)]
pub struct A2aCancellation {
    inner: Arc<CancellationInner>,
}

#[derive(Debug)]
struct CancellationInner {
    sender: watch::Sender<bool>,
}

impl A2aCancellation {
    fn new() -> Self {
        let (sender, _receiver) = watch::channel(false);
        Self {
            inner: Arc::new(CancellationInner { sender }),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        *self.inner.sender.borrow()
    }

    /// Wait until cancellation is requested. Safe to call after a previous
    /// cancellation; the current value is checked before waiting.
    pub async fn cancelled(&self) {
        let mut receiver = self.inner.sender.subscribe();
        let _ = receiver.wait_for(|cancelled| *cancelled).await;
    }

    fn reset(&self) {
        self.inner.sender.send_replace(false);
    }

    fn cancel(&self) {
        self.inner.sender.send_replace(true);
    }
}

impl Default for A2aCancellation {
    fn default() -> Self {
        Self::new()
    }
}

/// A status or artifact frame retained for replay. `id` is monotonic within a
/// task and is also used as the SSE `id:` field.
#[derive(Debug, Clone)]
pub(crate) struct EventEnvelope {
    pub id: u64,
    pub event: A2aStreamEvent,
    pub is_final: bool,
}

/// A bounded replay snapshot plus the live broadcast receiver for one task.
pub(crate) struct TaskSubscription {
    pub replay: Vec<EventEnvelope>,
    pub receiver: broadcast::Receiver<EventEnvelope>,
    pub close_after_replay: bool,
}

struct TaskRecord {
    status: TaskStatus,
    history: Vec<Message>,
    artifacts: Vec<Artifact>,
    metadata: Option<Map<String, Value>>,
    events: Vec<EventEnvelope>,
    event_bytes: usize,
    next_event_id: u64,
    updated_at: Instant,
}

struct TaskSlot {
    id: String,
    context_id: String,
    record: Mutex<TaskRecord>,
    run: Arc<tokio::sync::Mutex<()>>,
    sender: broadcast::Sender<EventEnvelope>,
    cancellation: A2aCancellation,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct IdempotencyKey {
    context_id: Option<String>,
    message_id: String,
}

#[derive(Clone)]
struct IdempotencyRecord {
    task_id: String,
    fingerprint: [u8; 32],
}

struct Inner {
    tasks: BTreeMap<String, Arc<TaskSlot>>,
    idempotency: BTreeMap<IdempotencyKey, IdempotencyRecord>,
    next_sequence: u64,
}

/// Bounded task store used by one server.
pub(crate) struct TaskStore {
    limits: A2aLimits,
    inner: Mutex<Inner>,
}

/// Result of starting or deduplicating an inbound message.
// Boxing would add an allocation to every message; the enum is short-lived
// and only one of its variants exists on the hot path.
#[allow(clippy::large_enum_variant)]
pub(crate) enum BeginOutcome {
    Duplicate {
        task: A2aTask,
        subscription: Option<TaskSubscription>,
    },
    Run(RunStart),
}

/// The exclusive right to run one handler for one task.
pub(crate) struct RunStart {
    slot: Arc<TaskSlot>,
    _run_guard: Option<OwnedMutexGuard<()>>,
    limits: A2aLimits,
    message: Message,
    task_snapshot: A2aTask,
    is_new_task: bool,
    request_id: Option<JsonRpcId>,
    cancellation: A2aCancellation,
    subscription: Option<TaskSubscription>,
    finished: bool,
}

impl RunStart {
    pub(crate) fn request_context(&self) -> crate::server::A2aRequestContext {
        crate::server::A2aRequestContext {
            task_id: self.slot.id.clone(),
            context_id: self.slot.context_id.clone(),
            message: self.message.clone(),
            task: self.task_snapshot.clone(),
            is_new_task: self.is_new_task,
            cancellation: self.cancellation.clone(),
            request_id: self.request_id.clone(),
        }
    }

    pub(crate) fn take_subscription(&mut self) -> Option<TaskSubscription> {
        self.subscription.take()
    }

    pub(crate) fn complete(mut self, update: A2aTaskUpdate) -> A2aResult<A2aTask> {
        let result = self.slot.apply_update(&update, &self.limits);
        if result.is_ok() {
            self.finished = true;
        }
        result
    }

    pub(crate) fn fail(mut self) -> A2aTask {
        self.finished = true;
        self.slot
            .fail_handler(&self.limits)
            .unwrap_or_else(|_| self.slot.snapshot(None))
    }
}

impl Drop for RunStart {
    fn drop(&mut self) {
        if !self.finished {
            self.finished = true;
            let _ = self.slot.fail_handler(&self.limits);
        }
    }
}

impl TaskStore {
    pub(crate) fn new(limits: A2aLimits) -> A2aResult<Self> {
        limits.validate()?;
        Ok(Self {
            limits,
            inner: Mutex::new(Inner {
                tasks: BTreeMap::new(),
                idempotency: BTreeMap::new(),
                next_sequence: 0,
            }),
        })
    }

    /// Start a run or return the task that a duplicate `messageId` already
    /// created. This function performs no I/O and never calls the handler.
    pub(crate) fn begin(
        &self,
        message: Message,
        request_id: Option<JsonRpcId>,
        stream: bool,
    ) -> A2aResult<BeginOutcome> {
        self.limits.validate_message(&message)?;
        if message.role != crate::wire::MessageRole::User {
            return Err(A2aError::InvalidParams {
                reason: "inbound messages must use the user role".into(),
            });
        }
        let fingerprint = fingerprint(&message)?;
        let key = IdempotencyKey {
            context_id: message.context_id.clone(),
            message_id: message.message_id.clone(),
        };
        let mut inner = lock(&self.inner);
        self.sweep_locked(&mut inner);

        if let Some(existing) = inner.idempotency.get(&key).cloned() {
            if existing.fingerprint != fingerprint {
                return Err(A2aError::IdempotencyConflict {
                    message_id: message.message_id.clone(),
                });
            }
            if let Some(slot) = inner.tasks.get(&existing.task_id).cloned() {
                let subscription = if stream {
                    Some(self.subscription_locked(&slot, None))
                } else {
                    None
                };
                let task = slot.snapshot(None);
                return Ok(BeginOutcome::Duplicate { task, subscription });
            }
            // The task expired between the idempotency lookup and the task
            // lookup. The sweep should have removed both; remove the dangling
            // key and treat this as a new request.
            inner.idempotency.remove(&key);
        }

        let existing_task_id = message.task_id.clone();
        if let Some(task_id) = existing_task_id {
            let slot =
                inner
                    .tasks
                    .get(&task_id)
                    .cloned()
                    .ok_or_else(|| A2aError::TaskNotFound {
                        task_id: task_id.clone(),
                    })?;
            if self.expired(&slot) {
                remove_task_locked(&mut inner, &task_id);
                return Err(A2aError::TaskNotFound { task_id });
            }
            if let Some(context_id) = &message.context_id
                && context_id != &slot.context_id
            {
                return Err(A2aError::InvalidParams {
                    reason: "message contextId does not match the task".into(),
                });
            }
            let run_guard = slot
                .run
                .clone()
                .try_lock_owned()
                .map_err(|_| A2aError::TaskBusy {
                    task_id: task_id.clone(),
                })?;

            let mut record = lock(&slot.record);
            match record.status.state {
                state if state.is_terminal() => {
                    return Err(A2aError::TaskTerminal { task_id });
                }
                state if !state.accepts_followup() => {
                    return Err(A2aError::TaskBusy { task_id });
                }
                _ => {}
            }

            let receiver = if stream {
                Some(slot.sender.subscribe())
            } else {
                None
            };
            let previous_last = record.next_event_id.saturating_sub(1);
            slot.cancellation.reset();
            record.history.push(message.clone());
            trim_history(&mut record, &self.limits);
            record.status = TaskStatus::new(TaskState::Working);
            record.updated_at = Instant::now();
            push_event(
                &slot,
                &mut record,
                A2aStreamEvent::Status(TaskStatusUpdateEvent::new(
                    slot.id.clone(),
                    slot.context_id.clone(),
                    TaskStatus::new(TaskState::Working),
                    false,
                )),
                false,
                &self.limits,
            );
            let replay = if receiver.is_some() {
                record
                    .events
                    .iter()
                    .filter(|event| event.id > previous_last)
                    .cloned()
                    .collect()
            } else {
                Vec::new()
            };
            let subscription = receiver.map(|receiver| TaskSubscription {
                replay,
                receiver,
                close_after_replay: false,
            });
            let task_snapshot = record.task_snapshot(&slot.id, &slot.context_id, None);
            inner.idempotency_insert(&key, &slot, fingerprint);
            drop(record);
            drop(inner);
            return Ok(BeginOutcome::Run(RunStart {
                slot: Arc::clone(&slot),
                _run_guard: Some(run_guard),
                limits: self.limits.clone(),
                message,
                task_snapshot,
                is_new_task: false,
                request_id,
                cancellation: slot.cancellation.clone(),
                subscription,
                finished: false,
            }));
        }

        if inner.tasks.len() >= self.limits.max_stored_tasks {
            return Err(A2aError::StoreFull);
        }

        inner.next_sequence = inner.next_sequence.saturating_add(1);
        let sequence = inner.next_sequence;
        let task_id = format!("task-{sequence:016x}");
        let context_id = message
            .context_id
            .clone()
            .unwrap_or_else(|| format!("ctx-{sequence:016x}"));
        let (sender, _receiver) = broadcast::channel(self.limits.max_stream_queue);
        let cancellation = A2aCancellation::new();
        let slot = Arc::new(TaskSlot {
            id: task_id.clone(),
            context_id: context_id.clone(),
            record: Mutex::new(TaskRecord {
                status: TaskStatus::new(TaskState::Submitted),
                history: Vec::new(),
                artifacts: Vec::new(),
                metadata: None,
                events: Vec::new(),
                event_bytes: 0,
                next_event_id: 1,
                updated_at: Instant::now(),
            }),
            run: Arc::new(tokio::sync::Mutex::new(())),
            sender,
            cancellation: cancellation.clone(),
        });
        inner.tasks.insert(task_id.clone(), Arc::clone(&slot));
        inner.idempotency.insert(
            key,
            IdempotencyRecord {
                task_id: task_id.clone(),
                fingerprint,
            },
        );

        let run_guard = slot
            .run
            .clone()
            .try_lock_owned()
            .map_err(|_| A2aError::TaskBusy {
                task_id: task_id.clone(),
            })?;
        let mut record = lock(&slot.record);
        let receiver = if stream {
            Some(slot.sender.subscribe())
        } else {
            None
        };
        slot.cancellation.reset();
        record.history.push(message.clone());
        trim_history(&mut record, &self.limits);
        // A submitted marker makes the first stream self-describing and gives
        // resubscribe a deterministic event id 1.
        push_event(
            &slot,
            &mut record,
            A2aStreamEvent::Status(TaskStatusUpdateEvent::new(
                task_id.clone(),
                context_id.clone(),
                TaskStatus::new(TaskState::Submitted),
                false,
            )),
            false,
            &self.limits,
        );
        record.status = TaskStatus::new(TaskState::Working);
        record.updated_at = Instant::now();
        push_event(
            &slot,
            &mut record,
            A2aStreamEvent::Status(TaskStatusUpdateEvent::new(
                task_id.clone(),
                context_id.clone(),
                TaskStatus::new(TaskState::Working),
                false,
            )),
            false,
            &self.limits,
        );
        let replay = if receiver.is_some() {
            record.events.clone()
        } else {
            Vec::new()
        };
        let subscription = receiver.map(|receiver| TaskSubscription {
            replay,
            receiver,
            close_after_replay: false,
        });
        let task_snapshot = record.task_snapshot(&task_id, &context_id, None);
        drop(record);
        drop(inner);
        Ok(BeginOutcome::Run(RunStart {
            slot,
            _run_guard: Some(run_guard),
            limits: self.limits.clone(),
            message,
            task_snapshot,
            is_new_task: true,
            request_id,
            cancellation,
            subscription,
            finished: false,
        }))
    }

    /// Snapshot a task. Used by handlers/tests and by `tasks/get`.
    pub(crate) fn get(&self, task_id: &str, history_length: Option<usize>) -> A2aResult<A2aTask> {
        let mut inner = lock(&self.inner);
        self.sweep_locked(&mut inner);
        let slot = self.live_slot_locked(&mut inner, task_id)?;
        let limit = history_length.map(|value| value.min(self.limits.max_task_history));
        Ok(slot.snapshot(limit))
    }

    /// Cancel a task, or return the same canceled task for an idempotent
    /// repeated cancellation.
    pub(crate) fn cancel(&self, task_id: &str) -> A2aResult<A2aTask> {
        let mut inner = lock(&self.inner);
        self.sweep_locked(&mut inner);
        let slot = self.live_slot_locked(&mut inner, task_id)?;
        drop(inner);
        let mut record = lock(&slot.record);
        match record.status.state {
            TaskState::Canceled => {
                return Ok(record.task_snapshot(&slot.id, &slot.context_id, None));
            }
            state if state.is_terminal() => {
                return Err(A2aError::TaskNotCancelable {
                    task_id: task_id.to_string(),
                });
            }
            state if !state.can_transition_to(TaskState::Canceled) => {
                return Err(A2aError::TaskNotCancelable {
                    task_id: task_id.to_string(),
                });
            }
            _ => {}
        }
        slot.cancellation.cancel();
        record.status = TaskStatus::new(TaskState::Canceled);
        record.updated_at = Instant::now();
        push_event(
            &slot,
            &mut record,
            A2aStreamEvent::Status(TaskStatusUpdateEvent::new(
                slot.id.clone(),
                slot.context_id.clone(),
                TaskStatus::new(TaskState::Canceled),
                true,
            )),
            true,
            &self.limits,
        );
        Ok(record.task_snapshot(&slot.id, &slot.context_id, None))
    }

    /// Subscribe to retained events after `after_id` plus the live tail.
    pub(crate) fn subscribe(
        &self,
        task_id: &str,
        after_id: Option<u64>,
    ) -> A2aResult<TaskSubscription> {
        let mut inner = lock(&self.inner);
        self.sweep_locked(&mut inner);
        let slot = self.live_slot_locked(&mut inner, task_id)?;
        drop(inner);
        Ok(self.subscription_locked(&slot, after_id))
    }

    fn subscription_locked(&self, slot: &Arc<TaskSlot>, after_id: Option<u64>) -> TaskSubscription {
        let record = lock(&slot.record);
        let receiver = slot.sender.subscribe();
        let after = after_id.unwrap_or(0);
        let replay = record
            .events
            .iter()
            .filter(|event| event.id > after)
            .cloned()
            .collect();
        TaskSubscription {
            replay,
            receiver,
            close_after_replay: record.status.state.is_settled(),
        }
    }

    fn live_slot_locked(&self, inner: &mut Inner, task_id: &str) -> A2aResult<Arc<TaskSlot>> {
        let slot = inner
            .tasks
            .get(task_id)
            .cloned()
            .ok_or_else(|| A2aError::TaskNotFound {
                task_id: task_id.to_string(),
            })?;
        if self.expired(&slot) {
            remove_task_locked(inner, task_id);
            return Err(A2aError::TaskNotFound {
                task_id: task_id.to_string(),
            });
        }
        Ok(slot)
    }

    fn expired(&self, slot: &TaskSlot) -> bool {
        // A live handler owns the run guard. Do not evict a task while its
        // host callback is still running.
        if slot.run.try_lock().is_err() {
            return false;
        }
        let record = lock(&slot.record);
        Instant::now().saturating_duration_since(record.updated_at) > self.limits.max_task_ttl
    }

    fn sweep_locked(&self, inner: &mut Inner) {
        let stale: Vec<String> = inner
            .tasks
            .iter()
            .filter(|(_, slot)| self.expired(slot))
            .map(|(id, _)| id.clone())
            .collect();
        for task_id in stale {
            remove_task_locked(inner, &task_id);
        }
    }
}

impl TaskRecord {
    fn task_snapshot(&self, id: &str, context_id: &str, history_limit: Option<usize>) -> A2aTask {
        let history = match history_limit {
            Some(limit) => {
                let start = self.history.len().saturating_sub(limit);
                self.history[start..].to_vec()
            }
            None => self.history.clone(),
        };
        A2aTask {
            id: id.to_string(),
            context_id: context_id.to_string(),
            status: self.status.clone(),
            history,
            artifacts: self.artifacts.clone(),
            metadata: self.metadata.clone(),
            kind: "task".into(),
        }
    }
}

impl TaskSlot {
    fn snapshot(&self, history_limit: Option<usize>) -> A2aTask {
        let record = lock(&self.record);
        record.task_snapshot(&self.id, &self.context_id, history_limit)
    }

    fn apply_update(&self, update: &A2aTaskUpdate, limits: &A2aLimits) -> A2aResult<A2aTask> {
        update.validate(limits)?;
        let target = update.status.state;
        if matches!(target, TaskState::Submitted | TaskState::Working) {
            return Err(A2aError::InvalidHandlerUpdate);
        }
        let mut record = lock(&self.record);
        let current = record.status.state;
        if current.is_terminal() {
            if current == TaskState::Canceled {
                // A canceled run may still return; it must not mutate state.
                return Ok(record.task_snapshot(&self.id, &self.context_id, None));
            }
            if current == target {
                return Ok(record.task_snapshot(&self.id, &self.context_id, None));
            }
            return Err(A2aError::InvalidTransition {
                from: current.to_string(),
                to: target.to_string(),
            });
        }
        if !current.can_transition_to(target) {
            return Err(A2aError::InvalidTransition {
                from: current.to_string(),
                to: target.to_string(),
            });
        }

        record.history.extend(update.messages.iter().cloned());
        // A2A status messages are part of the conversation projection. Keep
        // one copy if the host also supplied the same message id explicitly.
        if let Some(status_message) = &update.status.message
            && !record
                .history
                .iter()
                .any(|message| message.message_id == status_message.message_id)
        {
            record.history.push(status_message.clone());
        }
        trim_history(&mut record, limits);
        if let Some(metadata) = &update.metadata {
            record.metadata = Some(metadata.clone());
        }
        record.artifacts.extend(update.artifacts.iter().cloned());
        trim_artifacts(&mut record, limits);
        record.status = update.status.clone();
        record.updated_at = Instant::now();

        for artifact in &update.artifacts {
            push_event(
                self,
                &mut record,
                A2aStreamEvent::Artifact(TaskArtifactUpdateEvent::new(
                    self.id.clone(),
                    self.context_id.clone(),
                    artifact.clone(),
                )),
                false,
                limits,
            );
        }
        push_event(
            self,
            &mut record,
            A2aStreamEvent::Status(TaskStatusUpdateEvent::new(
                self.id.clone(),
                self.context_id.clone(),
                update.status.clone(),
                true,
            )),
            true,
            limits,
        );
        Ok(record.task_snapshot(&self.id, &self.context_id, None))
    }

    fn fail_handler(&self, limits: &A2aLimits) -> A2aResult<A2aTask> {
        let mut record = lock(&self.record);
        let current = record.status.state;
        if current.is_terminal() {
            return Ok(record.task_snapshot(&self.id, &self.context_id, None));
        }
        if !current.can_transition_to(TaskState::Failed) {
            return Err(A2aError::InvalidTransition {
                from: current.to_string(),
                to: TaskState::Failed.to_string(),
            });
        }
        let failure_message = Message::agent_text(
            format!("a2a-failure-{}", self.id),
            "The host handler could not complete the task.",
        );
        record.history.push(failure_message.clone());
        trim_history(&mut record, limits);
        let status = TaskStatus::new(TaskState::Failed).with_message(failure_message);
        record.status = status.clone();
        record.updated_at = Instant::now();
        push_event(
            self,
            &mut record,
            A2aStreamEvent::Status(TaskStatusUpdateEvent::new(
                self.id.clone(),
                self.context_id.clone(),
                status,
                true,
            )),
            true,
            limits,
        );
        Ok(record.task_snapshot(&self.id, &self.context_id, None))
    }
}

fn push_event(
    slot: &TaskSlot,
    record: &mut TaskRecord,
    event: A2aStreamEvent,
    is_final: bool,
    limits: &A2aLimits,
) {
    let id = record.next_event_id;
    record.next_event_id = record.next_event_id.saturating_add(1);
    let event_bytes = serde_json::to_vec(&event)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX);
    let envelope = EventEnvelope {
        id,
        event,
        is_final,
    };
    record.events.push(envelope.clone());
    record.event_bytes = record.event_bytes.saturating_add(event_bytes);
    let byte_limit = limits
        .max_task_artifact_bytes
        .saturating_add(limits.max_body_bytes);
    while record.events.len() > limits.max_retained_events || record.event_bytes > byte_limit {
        if record.events.is_empty() {
            break;
        }
        let removed = record.events.remove(0);
        record.event_bytes = record
            .event_bytes
            .saturating_sub(serde_json::to_vec(&removed.event).map_or(0, |bytes| bytes.len()));
    }
    // No receiver is normal for a non-streaming send. The retained events
    // still support a later resubscribe.
    let _ = slot.sender.send(envelope);
}

impl Inner {
    fn idempotency_insert(
        &mut self,
        key: &IdempotencyKey,
        slot: &Arc<TaskSlot>,
        fingerprint: [u8; 32],
    ) {
        self.idempotency.insert(
            key.clone(),
            IdempotencyRecord {
                task_id: slot.id.clone(),
                fingerprint,
            },
        );
    }
}

fn trim_history(record: &mut TaskRecord, limits: &A2aLimits) {
    while record.history.len() > limits.max_task_history {
        record.history.remove(0);
    }
    let mut total = record
        .history
        .iter()
        .map(message_size)
        .fold(0usize, usize::saturating_add);
    while total > limits.max_task_history_bytes && !record.history.is_empty() {
        let removed = record.history.remove(0);
        total = total.saturating_sub(message_size(&removed));
    }
}

fn trim_artifacts(record: &mut TaskRecord, limits: &A2aLimits) {
    while record.artifacts.len() > limits.max_artifacts_per_task {
        record.artifacts.remove(0);
    }
    let mut total = record
        .artifacts
        .iter()
        .map(artifact_size)
        .fold(0usize, usize::saturating_add);
    while total > limits.max_task_artifact_bytes && !record.artifacts.is_empty() {
        let removed = record.artifacts.remove(0);
        total = total.saturating_sub(artifact_size(&removed));
    }
}

fn message_size(message: &Message) -> usize {
    serde_json::to_vec(message)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

fn artifact_size(artifact: &Artifact) -> usize {
    serde_json::to_vec(artifact)
        .map(|bytes| bytes.len())
        .unwrap_or(usize::MAX)
}

fn remove_task_locked(inner: &mut Inner, task_id: &str) {
    inner.tasks.remove(task_id);
    inner
        .idempotency
        .retain(|_, record| record.task_id != task_id);
}

fn fingerprint(message: &Message) -> A2aResult<[u8; 32]> {
    let bytes = serde_json::to_vec(message).map_err(|_| A2aError::Internal)?;
    let digest = Sha256::digest(&bytes);
    let mut output = [0u8; 32];
    output.copy_from_slice(&digest);
    Ok(output)
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    match mutex.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::{A2aTaskUpdate, Message, TaskState};

    fn store() -> TaskStore {
        TaskStore::new(A2aLimits::default()).expect("valid limits")
    }

    fn message(id: &str, text: &str) -> Message {
        Message::user_text(id, text)
    }

    fn begin_error(result: A2aResult<BeginOutcome>) -> A2aError {
        match result {
            Err(error) => error,
            Ok(_) => panic!("expected a begin error"),
        }
    }

    #[test]
    fn lifecycle_and_event_order_are_deterministic() {
        let store = store();
        let start = match store
            .begin(message("m-1", "hello"), None, false)
            .expect("begin")
        {
            BeginOutcome::Run(run) => run,
            BeginOutcome::Duplicate { .. } => panic!("unexpected duplicate"),
        };
        let task_id = start.slot.id.clone();
        assert_eq!(task_id, "task-0000000000000001");
        let working = store.get(&task_id, None).expect("working task");
        assert_eq!(working.status.state, TaskState::Working);
        assert_eq!(working.history.len(), 1);

        let completed = start
            .complete(
                A2aTaskUpdate::new(TaskState::Completed)
                    .with_message(Message::agent_text("a-1", "done")),
            )
            .expect("complete");
        assert_eq!(completed.status.state, TaskState::Completed);
        assert_eq!(completed.history.len(), 2);

        let subscription = store.subscribe(&task_id, None).expect("subscribe");
        let states: Vec<_> = subscription
            .replay
            .iter()
            .filter_map(|frame| match &frame.event {
                A2aStreamEvent::Status(event) => Some(event.status.state),
                A2aStreamEvent::Artifact(_) => None,
            })
            .collect();
        assert_eq!(
            states,
            vec![
                TaskState::Submitted,
                TaskState::Working,
                TaskState::Completed
            ]
        );
        assert!(
            subscription
                .replay
                .windows(2)
                .all(|pair| pair[0].id < pair[1].id)
        );
        assert!(subscription.replay.last().expect("events").is_final);
        assert!(subscription.close_after_replay);
    }

    #[test]
    fn input_required_allows_a_followup_then_completes() {
        let store = store();
        let first = match store
            .begin(message("m-1", "question"), None, false)
            .expect("begin")
        {
            BeginOutcome::Run(run) => run,
            BeginOutcome::Duplicate { .. } => panic!("unexpected duplicate"),
        };
        let task_id = first.slot.id.clone();
        first
            .complete(A2aTaskUpdate::new(TaskState::InputRequired))
            .expect("input required");
        assert_eq!(
            store.get(&task_id, None).expect("task").status.state,
            TaskState::InputRequired
        );

        let followup = message("m-2", "answer")
            .with_task(task_id.clone())
            .with_context(store.get(&task_id, None).expect("task").context_id);
        let second = match store.begin(followup, None, false).expect("follow-up") {
            BeginOutcome::Run(run) => run,
            BeginOutcome::Duplicate { .. } => panic!("unexpected duplicate"),
        };
        assert_eq!(
            store.get(&task_id, None).expect("task").status.state,
            TaskState::Working
        );
        let completed = second
            .complete(A2aTaskUpdate::new(TaskState::Completed))
            .expect("complete");
        assert_eq!(completed.status.state, TaskState::Completed);
        assert_eq!(completed.history.len(), 2);
        let states: Vec<_> = store
            .subscribe(&task_id, None)
            .expect("subscribe")
            .replay
            .iter()
            .filter_map(|frame| match &frame.event {
                A2aStreamEvent::Status(event) => Some(event.status.state),
                A2aStreamEvent::Artifact(_) => None,
            })
            .collect();
        assert_eq!(
            states,
            vec![
                TaskState::Submitted,
                TaskState::Working,
                TaskState::InputRequired,
                TaskState::Working,
                TaskState::Completed
            ]
        );
    }

    #[test]
    fn terminal_tasks_are_immutable_but_idempotent_same_outcome() {
        let store = store();
        let run = match store
            .begin(message("m-1", "hello"), None, false)
            .expect("begin")
        {
            BeginOutcome::Run(run) => run,
            BeginOutcome::Duplicate { .. } => panic!("unexpected duplicate"),
        };
        let task_id = run.slot.id.clone();
        let completed = run
            .complete(A2aTaskUpdate::new(TaskState::Completed))
            .expect("complete");
        assert_eq!(completed.status.state, TaskState::Completed);

        let slot = {
            let inner = lock(&store.inner);
            inner.tasks.get(&task_id).cloned().expect("task")
        };
        assert!(
            slot.apply_update(&A2aTaskUpdate::new(TaskState::Working), &store.limits)
                .is_err()
        );
        assert_eq!(
            slot.apply_update(&A2aTaskUpdate::new(TaskState::Completed), &store.limits)
                .expect("same outcome")
                .status
                .state,
            TaskState::Completed
        );
        assert_eq!(
            store.cancel(&task_id).expect_err("terminal cancel").code(),
            crate::error::A2aErrorCode::TaskNotCancelable
        );
    }

    #[test]
    fn cancel_is_idempotent_and_terminal() {
        let store = store();
        let run = match store
            .begin(message("m-1", "hello"), None, false)
            .expect("begin")
        {
            BeginOutcome::Run(run) => run,
            BeginOutcome::Duplicate { .. } => panic!("unexpected duplicate"),
        };
        let task_id = run.slot.id.clone();
        assert!(!run.cancellation.is_cancelled());
        let canceled = store.cancel(&task_id).expect("cancel");
        assert_eq!(canceled.status.state, TaskState::Canceled);
        assert!(run.cancellation.is_cancelled());
        let again = store.cancel(&task_id).expect("idempotent cancel");
        assert_eq!(again.status.state, TaskState::Canceled);

        let completed = run.complete(A2aTaskUpdate::new(TaskState::Completed));
        // The late completion is a no-op on a canceled task, and the run must
        // not resurrect it.
        assert_eq!(
            completed.expect("late completion").status.state,
            TaskState::Canceled
        );
    }

    #[test]
    fn duplicate_message_is_idempotent_and_conflicts_are_typed() {
        let store = store();
        let run = match store
            .begin(message("m-1", "hello"), None, false)
            .expect("begin")
        {
            BeginOutcome::Run(run) => run,
            BeginOutcome::Duplicate { .. } => panic!("unexpected duplicate"),
        };
        let task_id = run.slot.id.clone();
        run.complete(A2aTaskUpdate::new(TaskState::Completed))
            .expect("complete");

        let duplicate = match store
            .begin(message("m-1", "hello"), None, false)
            .expect("duplicate")
        {
            BeginOutcome::Duplicate { task, .. } => task,
            BeginOutcome::Run(_) => panic!("created a second task"),
        };
        assert_eq!(duplicate.id, task_id);
        assert_eq!(store.inner.lock().expect("lock").tasks.len(), 1);

        let conflict = begin_error(store.begin(message("m-1", "different"), None, false));
        assert_eq!(
            conflict.code(),
            crate::error::A2aErrorCode::IdempotencyConflict
        );
    }

    #[test]
    fn ids_are_monotonic_per_store_and_per_task() {
        let store = store();
        let first = match store
            .begin(message("m-1", "one"), None, false)
            .expect("first")
        {
            BeginOutcome::Run(run) => run,
            BeginOutcome::Duplicate { .. } => panic!("duplicate"),
        };
        let first_id = first.slot.id.clone();
        first
            .complete(A2aTaskUpdate::new(TaskState::Completed))
            .expect("first complete");
        let second = match store
            .begin(message("m-2", "two"), None, false)
            .expect("second")
        {
            BeginOutcome::Run(run) => run,
            BeginOutcome::Duplicate { .. } => panic!("duplicate"),
        };
        let second_id = second.slot.id.clone();
        second
            .complete(A2aTaskUpdate::new(TaskState::Completed))
            .expect("second complete");
        assert_ne!(first_id, second_id);
        assert!(first_id < second_id);
        for task_id in [first_id, second_id] {
            let events = store.subscribe(&task_id, None).expect("subscribe").replay;
            assert_eq!(events[0].id, 1);
            assert!(events.windows(2).all(|pair| pair[0].id < pair[1].id));
        }
    }

    #[test]
    fn retained_events_are_bounded_without_breaking_final_state() {
        let limits = A2aLimits {
            max_retained_events: 1,
            ..A2aLimits::default()
        };
        let store = TaskStore::new(limits).expect("store");
        let run = match store
            .begin(message("m-1", "one"), None, false)
            .expect("begin")
        {
            BeginOutcome::Run(run) => run,
            BeginOutcome::Duplicate { .. } => panic!("duplicate"),
        };
        let task_id = run.slot.id.clone();
        run.complete(A2aTaskUpdate::new(TaskState::Completed))
            .expect("complete");
        let replay = store.subscribe(&task_id, None).expect("subscribe").replay;
        assert_eq!(replay.len(), 1);
        assert!(replay[0].is_final);
        assert!(matches!(
            &replay[0].event,
            A2aStreamEvent::Status(event) if event.status.state == TaskState::Completed
        ));
    }

    #[test]
    fn capacity_is_bounded_and_expiry_is_lazy() {
        let limits = A2aLimits {
            max_stored_tasks: 1,
            ..A2aLimits::default()
        };
        let store = TaskStore::new(limits).expect("store");
        let first = match store
            .begin(message("m-1", "one"), None, false)
            .expect("first")
        {
            BeginOutcome::Run(run) => run,
            BeginOutcome::Duplicate { .. } => panic!("duplicate"),
        };
        first
            .complete(A2aTaskUpdate::new(TaskState::Completed))
            .expect("complete");
        let error = begin_error(store.begin(message("m-2", "two"), None, false));
        assert_eq!(error.code(), crate::error::A2aErrorCode::StoreFull);
    }

    #[test]
    fn invalid_transition_is_typed() {
        let store = store();
        let run = match store
            .begin(message("m-1", "hello"), None, false)
            .expect("begin")
        {
            BeginOutcome::Run(run) => run,
            BeginOutcome::Duplicate { .. } => panic!("unexpected duplicate"),
        };
        let task_id = run.slot.id.clone();
        let _ = run.complete(A2aTaskUpdate::new(TaskState::Completed));
        let followup = message("m-2", "again").with_task(task_id);
        let error = begin_error(store.begin(followup, None, false));
        assert_eq!(error.code(), crate::error::A2aErrorCode::TaskTerminal);
    }

    #[test]
    fn cancellation_wait_is_not_sleep_based() {
        let cancellation = A2aCancellation::new();
        let clone = cancellation.clone();
        cancellation.cancel();
        assert!(clone.is_cancelled());
        // Already-cancelled waiters observe the value immediately.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async {
            tokio::time::timeout(std::time::Duration::from_millis(50), clone.cancelled())
                .await
                .expect("already cancelled");
        });
    }
}
