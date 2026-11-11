//! Deterministic fake provider for benchmark runs.
//!
//! The fake provider implements the real [`vp_providers::Provider`] trait and
//! returns ordinary `ChatRequest`/`StreamEvent` values. It performs no network
//! I/O, reads no credentials, and refuses to run past a bounded call count.
//!
//! Two modes are supported:
//!
//! * a fixed response sequence for deterministic unit tests;
//! * a role-aware benchmark script that behaves like a tiny, honest coding
//!   model for the catalog scenarios (read a marker file, write the expected
//!   target, submit a verdict).
//!
//! The benchmark script is not a smuggled model. It is test data: every
//! decision is a direct function of the system prompt and the last tool result,
//! and the transcript is recorded in the report metadata.

use std::collections::{BTreeMap, VecDeque};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::Stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use vp_providers::{
    ChatMessage, ChatRequest, EventStream, Provider, ProviderError, StreamEvent, ToolCall,
};

use crate::metrics::TokenUsage;

/// One fake assistant action. A turn is a list of actions emitted in order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScriptAction {
    Text(String),
    ToolCall { name: String, arguments: Value },
    Error(String),
}

impl ScriptAction {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text(text.into())
    }

    pub fn tool(name: impl Into<String>, arguments: Value) -> Self {
        Self::ToolCall {
            name: name.into(),
            arguments,
        }
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::Error(message.into())
    }
}

/// Token accounting used by the fake provider.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FakeUsageModel {
    Fixed {
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_write_tokens: u32,
    },
    Simulated {
        base_input_tokens: u32,
        output_tokens: u32,
        cache_read_bps: u16,
        cache_write_bps: u16,
    },
}

impl FakeUsageModel {
    /// Deterministic request-sized usage for catalog runs. It keeps cache reads
    /// and writes disjoint and bounded by the simulated input, and it makes
    /// context reduction visible (a shorter request has fewer input tokens).
    pub fn benchmark_default() -> Self {
        Self::Simulated {
            base_input_tokens: 32,
            output_tokens: 48,
            cache_read_bps: 5_000,
            cache_write_bps: 1_000,
        }
    }

    pub fn fixed(
        input_tokens: u32,
        output_tokens: u32,
        cache_read_tokens: u32,
        cache_write_tokens: u32,
    ) -> Self {
        Self::Fixed {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_write_tokens,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Fixed {
                cache_read_tokens,
                cache_write_tokens,
                input_tokens,
                ..
            } => {
                let read = u64::from(*cache_read_tokens);
                let write = u64::from(*cache_write_tokens);
                if read > u64::from(*input_tokens) || write > u64::from(*input_tokens) {
                    return Err("fixed cache tokens cannot exceed fixed input tokens".to_string());
                }
            }
            Self::Simulated {
                cache_read_bps,
                cache_write_bps,
                ..
            } => {
                if u32::from(*cache_read_bps) + u32::from(*cache_write_bps) > 10_000 {
                    return Err(
                        "simulated cache bps values cannot exceed 10000 combined".to_string()
                    );
                }
            }
        }
        Ok(())
    }

    pub fn source(&self) -> &'static str {
        match self {
            Self::Fixed { .. } => "fixed",
            Self::Simulated { .. } => "simulated",
        }
    }

    fn usage(&self, request: &ChatRequest, actions: &[ScriptAction]) -> TokenUsage {
        match self {
            Self::Fixed {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_write_tokens,
            } => TokenUsage {
                input_tokens: u64::from(*input_tokens),
                output_tokens: u64::from(*output_tokens),
                cache_read_tokens: u64::from(*cache_read_tokens),
                cache_write_tokens: u64::from(*cache_write_tokens),
                calls: 1,
            }
            .normalized(),
            Self::Simulated {
                base_input_tokens,
                output_tokens,
                cache_read_bps,
                cache_write_bps,
            } => {
                let input =
                    u64::from(*base_input_tokens).saturating_add(estimate_request_tokens(request));
                let cache_read = input
                    .saturating_mul(u64::from(*cache_read_bps))
                    .saturating_div(10_000);
                let cache_write = input
                    .saturating_sub(cache_read)
                    .saturating_mul(u64::from(*cache_write_bps))
                    .saturating_div(10_000);
                let output = u64::from(*output_tokens)
                    .saturating_add(actions.iter().map(estimate_action_tokens).sum::<u64>());
                TokenUsage {
                    input_tokens: input,
                    output_tokens: output.max(1),
                    cache_read_tokens: cache_read,
                    cache_write_tokens: cache_write,
                    calls: 1,
                }
                .normalized()
            }
        }
    }
}

/// One recorded provider call. Sequence numbers are unique even when several
/// Worker tasks call concurrently.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderCall {
    pub sequence: usize,
    pub role: String,
    pub last_role: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_write_tokens: u64,
    pub action_count: usize,
    pub error: Option<String>,
}

/// Aggregate provider telemetry used in the report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FakeProviderSummary {
    pub calls: u64,
    pub errors: u64,
    pub exhausted: bool,
    pub usage_source: String,
    pub simulated_latency_ms: u64,
}

/// The role-aware catalog behavior.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BenchBehavior {
    direct: Option<DirectBehavior>,
    plan_targets: BTreeMap<String, String>,
    verifier_approves: bool,
    scout: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DirectBehavior {
    target_path: String,
    target_content: String,
    prefetch_path: Option<String>,
}

impl BenchBehavior {
    pub fn direct(
        target_path: impl Into<String>,
        target_content: impl Into<String>,
        prefetch_path: Option<String>,
    ) -> Self {
        Self {
            direct: Some(DirectBehavior {
                target_path: target_path.into(),
                target_content: target_content.into(),
                prefetch_path,
            }),
            plan_targets: BTreeMap::new(),
            verifier_approves: false,
            scout: false,
        }
    }

    pub fn plan(targets: BTreeMap<String, String>, verifier_approves: bool) -> Self {
        Self {
            direct: None,
            plan_targets: targets,
            verifier_approves,
            scout: false,
        }
    }

    pub fn scout() -> Self {
        Self {
            direct: None,
            plan_targets: BTreeMap::new(),
            verifier_approves: false,
            scout: true,
        }
    }

    fn actions(
        &self,
        system: &str,
        messages: &[ChatMessage],
    ) -> Result<Vec<ScriptAction>, ProviderError> {
        if system.contains("compacting an AI coding agent conversation") {
            return Ok(vec![ScriptAction::text(
                "Earlier conversation summary (benchmark fixture): the task remains to create \
                 the expected target file. No files were changed by the summary step.",
            )]);
        }
        if system.contains("You are a Verifier") {
            return Ok(vec![ScriptAction::tool(
                "submit_verdict",
                json!({
                    "approved": self.verifier_approves,
                    "summary": if self.verifier_approves {
                        "The benchmark target and acceptance evidence match the task."
                    } else {
                        "The benchmark task was rejected by the deterministic negative fixture."
                    }
                }),
            )]);
        }
        if system.contains("You are Scout") {
            let last = messages.last();
            if last.is_some_and(|message| message.role == "tool") {
                return Ok(vec![ScriptAction::text(
                    "README.md:5 contains BENCH-MARKER: verlinpark-bench-v1. The fixture is \
                     read-only and the Scout made no writes.",
                )]);
            }
            return Ok(vec![ScriptAction::tool(
                "read_file",
                json!({"path": "README.md"}),
            )]);
        }
        if system.contains("You are a Worker") {
            let last = messages.last();
            if last.is_some_and(|message| message.role == "tool") {
                return Ok(vec![ScriptAction::text("Worker task complete.")]);
            }
            let haystack = request_haystack(system, messages);
            let target = self
                .plan_targets
                .keys()
                .find(|path| haystack.contains(path.as_str()))
                .cloned();
            let Some(target) = target else {
                return Err(ProviderError::Unsupported(
                    "benchmark Worker objective did not contain a configured plan target path"
                        .to_string(),
                ));
            };
            let content = self.plan_targets.get(&target).cloned().unwrap_or_default();
            return Ok(vec![ScriptAction::tool(
                "write_file",
                json!({"path": target, "content": content}),
            )]);
        }
        if system.contains("You are a department Manager") {
            return Ok(vec![ScriptAction::text("Manager brief complete.")]);
        }
        if system.contains("You are the Leader agent") {
            if let Some(direct) = &self.direct {
                let last = messages.last();
                if last.is_some_and(|message| message.role == "tool") {
                    let content = last
                        .map(|message| message.content.as_str())
                        .unwrap_or_default();
                    let read_was_prefetch =
                        direct.prefetch_path.is_some() && content.contains("BENCH-LARGE-MARKER");
                    if read_was_prefetch {
                        return Ok(vec![ScriptAction::tool(
                            "write_file",
                            json!({
                                "path": direct.target_path,
                                "content": direct.target_content,
                            }),
                        )]);
                    }
                    return Ok(vec![ScriptAction::text("Direct benchmark task complete.")]);
                }
                if let Some(prefetch) = &direct.prefetch_path {
                    return Ok(vec![ScriptAction::tool(
                        "read_file",
                        json!({"path": prefetch}),
                    )]);
                }
                return Ok(vec![ScriptAction::tool(
                    "write_file",
                    json!({
                        "path": direct.target_path,
                        "content": direct.target_content,
                    }),
                )]);
            }
            if self.scout {
                let last = messages.last();
                if last.is_some_and(|message| message.role == "tool") {
                    return Ok(vec![ScriptAction::text("Scout brief received.")]);
                }
                return Ok(vec![ScriptAction::tool(
                    "scout",
                    json!({
                        "objective": "Read README.md and report the benchmark marker.",
                        "files": ["README.md"],
                        "max_steps": 3,
                    }),
                )]);
            }
            return Ok(vec![ScriptAction::text(
                "The stored benchmark plan is already queued; no Leader plan submission is needed.",
            )]);
        }
        Ok(vec![ScriptAction::text("Benchmark provider fallback.")])
    }
}

/// Internal fake response mode.
enum FakeMode {
    Bench(BenchBehavior),
    Sequence(Mutex<VecDeque<Vec<ScriptAction>>>),
}

/// A no-network provider with a bounded, inspectable transcript.
pub struct FakeProvider {
    id: String,
    model: String,
    mode: FakeMode,
    usage_model: FakeUsageModel,
    latency: Duration,
    max_calls: usize,
    calls: Mutex<Vec<ProviderCall>>,
    next_sequence: AtomicUsize,
    exhausted: AtomicBool,
    errors: AtomicUsize,
}

impl FakeProvider {
    pub fn bench(
        model: impl Into<String>,
        behavior: BenchBehavior,
        usage_model: FakeUsageModel,
        latency: Duration,
        max_calls: usize,
    ) -> Self {
        Self {
            id: "vp-bench-fake".to_string(),
            model: model.into(),
            mode: FakeMode::Bench(behavior),
            usage_model,
            latency,
            max_calls: max_calls.max(1),
            calls: Mutex::new(Vec::new()),
            next_sequence: AtomicUsize::new(0),
            exhausted: AtomicBool::new(false),
            errors: AtomicUsize::new(0),
        }
    }

    pub fn sequence(
        model: impl Into<String>,
        turns: Vec<Vec<ScriptAction>>,
        usage_model: FakeUsageModel,
        max_calls: usize,
    ) -> Self {
        Self {
            id: "vp-bench-fake-sequence".to_string(),
            model: model.into(),
            mode: FakeMode::Sequence(Mutex::new(turns.into())),
            usage_model,
            latency: Duration::ZERO,
            max_calls: max_calls.max(1),
            calls: Mutex::new(Vec::new()),
            next_sequence: AtomicUsize::new(0),
            exhausted: AtomicBool::new(false),
            errors: AtomicUsize::new(0),
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn summary(&self) -> FakeProviderSummary {
        let calls = self.calls.lock().map(|calls| calls.len()).unwrap_or(0);
        FakeProviderSummary {
            calls: calls as u64,
            errors: self.errors.load(Ordering::SeqCst) as u64,
            exhausted: self.exhausted.load(Ordering::SeqCst),
            usage_source: self.usage_model.source().to_string(),
            simulated_latency_ms: self.latency.as_millis() as u64,
        }
    }

    pub fn transcript(&self) -> Vec<ProviderCall> {
        let mut calls = self
            .calls
            .lock()
            .map(|calls| calls.clone())
            .unwrap_or_default();
        calls.sort_by_key(|call| call.sequence);
        calls
    }

    pub fn remaining_sequence(&self) -> usize {
        match &self.mode {
            FakeMode::Bench(_) => 0,
            FakeMode::Sequence(turns) => turns.lock().map(|turns| turns.len()).unwrap_or(0),
        }
    }

    fn record_error(&self) {
        self.errors.fetch_add(1, Ordering::SeqCst);
    }

    fn record_call(&self, call: ProviderCall) {
        if let Ok(mut calls) = self.calls.lock() {
            calls.push(call);
        }
    }
}

#[async_trait]
impl Provider for FakeProvider {
    fn id(&self) -> &str {
        &self.id
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        let sequence = self.next_sequence.fetch_add(1, Ordering::SeqCst);
        if sequence >= self.max_calls {
            self.exhausted.store(true, Ordering::SeqCst);
            self.record_error();
            return Err(ProviderError::Stream(format!(
                "fake provider call cap {} exceeded",
                self.max_calls
            )));
        }
        if !self.latency.is_zero() {
            tokio::time::sleep(self.latency).await;
        }
        let system = request.system.clone().unwrap_or_default();
        let role = role_for(&system).to_string();
        let last_role = request
            .messages
            .last()
            .map(|message| message.role.clone())
            .unwrap_or_default();
        let actions = match &self.mode {
            FakeMode::Bench(behavior) => match behavior.actions(&system, &request.messages) {
                Ok(actions) => actions,
                Err(error) => {
                    self.record_error();
                    self.record_call(ProviderCall {
                        sequence,
                        role,
                        last_role,
                        input_tokens: 0,
                        output_tokens: 0,
                        cache_read_tokens: 0,
                        cache_write_tokens: 0,
                        action_count: 0,
                        error: Some(error.to_string()),
                    });
                    return Err(error);
                }
            },
            FakeMode::Sequence(turns) => {
                let next = turns
                    .lock()
                    .map_err(|_| ProviderError::Stream("fake sequence lock poisoned".into()))?
                    .pop_front();
                match next {
                    Some(actions) => actions,
                    None => {
                        self.exhausted.store(true, Ordering::SeqCst);
                        self.record_error();
                        let message = "fake provider response sequence exhausted".to_string();
                        self.record_call(ProviderCall {
                            sequence,
                            role,
                            last_role,
                            input_tokens: 0,
                            output_tokens: 0,
                            cache_read_tokens: 0,
                            cache_write_tokens: 0,
                            action_count: 0,
                            error: Some(message.clone()),
                        });
                        return Err(ProviderError::Stream(message));
                    }
                }
            }
        };
        let usage = self.usage_model.usage(&request, &actions);
        let mut events = vec![StreamEvent::Usage {
            input_tokens: usage.input_tokens.min(u64::from(u32::MAX)) as u32,
            output_tokens: usage.output_tokens.min(u64::from(u32::MAX)) as u32,
            cache_read_tokens: usage.cache_read_tokens.min(u64::from(u32::MAX)) as u32,
            cache_write_tokens: usage.cache_write_tokens.min(u64::from(u32::MAX)) as u32,
        }];
        let mut error = None;
        for action in &actions {
            match action {
                ScriptAction::Text(text) => events.push(StreamEvent::TextDelta(text.clone())),
                ScriptAction::ToolCall { name, arguments } => {
                    events.push(StreamEvent::ToolCall(ToolCall {
                        id: format!("bench-{sequence}-{name}"),
                        name: name.clone(),
                        arguments: arguments.to_string(),
                    }));
                }
                ScriptAction::Error(message) => {
                    error = Some(message.clone());
                    break;
                }
            }
        }
        self.record_call(ProviderCall {
            sequence,
            role,
            last_role,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cache_read_tokens: usage.cache_read_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            action_count: actions.len(),
            error: error.clone(),
        });
        if let Some(message) = error {
            self.record_error();
            return Err(ProviderError::Stream(message));
        }
        events.push(StreamEvent::Done);
        let stream = futures_util::stream::iter(events.into_iter().map(Ok));
        Ok(Box::pin(stream) as EventStream)
    }
}

/// Concatenate all provider-visible request text for deterministic marker
/// discovery. This is only used by the fake script; it is never sent anywhere.
fn request_haystack(system: &str, messages: &[ChatMessage]) -> String {
    let mut haystack = String::from(system);
    for message in messages {
        haystack.push('\n');
        haystack.push_str(&message.content);
    }
    haystack
}

/// Extract the first configured-looking `bench-target-*` path from arbitrary
/// provider-visible text. The alphabet matches the fixture path validator.
pub fn extract_target_marker(haystack: &str) -> Option<String> {
    let mut offset = 0;
    while let Some(relative) = haystack[offset..].find("bench-target-") {
        let start = offset + relative;
        let rest = &haystack[start..];
        let end = rest
            .find(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.'))
            })
            .unwrap_or(rest.len());
        let candidate = &rest[..end];
        if candidate.len() > "bench-target-".len() {
            return Some(candidate.to_string());
        }
        offset = start + end.max(1);
    }
    None
}

fn estimate_request_tokens(request: &ChatRequest) -> u64 {
    let mut characters = request.system.as_deref().unwrap_or_default().len();
    for message in &request.messages {
        characters = characters.saturating_add(message.content.len());
        if let Some(calls) = &message.tool_calls {
            characters = characters.saturating_add(
                serde_json::to_string(calls)
                    .map(|value| value.len())
                    .unwrap_or(0),
            );
        }
    }
    characters = characters.saturating_add(
        serde_json::to_string(&request.tools)
            .map(|value| value.len())
            .unwrap_or(0),
    );
    (characters as u64).div_ceil(4).saturating_add(1)
}

fn estimate_action_tokens(action: &ScriptAction) -> u64 {
    let bytes = match action {
        ScriptAction::Text(text) => text.len(),
        ScriptAction::ToolCall { name, arguments } => name.len() + arguments.to_string().len(),
        ScriptAction::Error(message) => message.len(),
    };
    (bytes as u64).div_ceil(4).saturating_add(1)
}

/// Classify a provider-visible system prompt for accounting and concurrency.
pub(crate) fn role_for(system: &str) -> &'static str {
    if system.contains("compacting an AI coding agent conversation") {
        "compaction"
    } else if system.contains("You are a Verifier") {
        "verifier"
    } else if system.contains("You are Scout") {
        "scout"
    } else if system.contains("You are a Worker") {
        "worker"
    } else if system.contains("You are a department Manager") {
        "manager"
    } else if system.contains("You are the Leader agent") {
        "leader"
    } else {
        "unknown"
    }
}

/// Wraps any provider and measures the peak number of simultaneously active
/// Worker provider streams. This is the honest parallelism signal: plan Worker
/// creation is eager, but actual Worker execution is gated by capacity and
/// write scopes.
pub struct MeteringProvider {
    inner: Arc<dyn Provider>,
    active_workers: Arc<AtomicUsize>,
    peak_workers: Arc<AtomicUsize>,
}

impl MeteringProvider {
    pub fn new(inner: Arc<dyn Provider>) -> Self {
        Self {
            inner,
            active_workers: Arc::new(AtomicUsize::new(0)),
            peak_workers: Arc::new(AtomicUsize::new(0)),
        }
    }

    pub fn peak_workers(&self) -> u64 {
        self.peak_workers.load(Ordering::SeqCst) as u64
    }
}

#[async_trait]
impl Provider for MeteringProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    async fn stream_chat(&self, request: ChatRequest) -> Result<EventStream, ProviderError> {
        let track = request
            .system
            .as_deref()
            .is_some_and(|system| role_for(system) == "worker");
        if track {
            let active = self.active_workers.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak_workers.fetch_max(active, Ordering::SeqCst);
        }
        match self.inner.stream_chat(request).await {
            Ok(stream) => Ok(Box::pin(MeteredStream {
                inner: stream,
                active_workers: self.active_workers.clone(),
                track,
                released: false,
            })),
            Err(error) => {
                if track {
                    self.active_workers.fetch_sub(1, Ordering::SeqCst);
                }
                Err(error)
            }
        }
    }
}

struct MeteredStream {
    inner: EventStream,
    active_workers: Arc<AtomicUsize>,
    track: bool,
    released: bool,
}

impl MeteredStream {
    fn release(&mut self) {
        if self.track && !self.released {
            self.released = true;
            self.active_workers.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

impl Stream for MeteredStream {
    type Item = Result<StreamEvent, ProviderError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let item = this.inner.as_mut().poll_next(cx);
        let terminal = matches!(
            &item,
            Poll::Ready(None)
                | Poll::Ready(Some(Err(_)))
                | Poll::Ready(Some(Ok(StreamEvent::Done)))
        );
        if terminal {
            this.release();
        }
        item
    }
}

impl Drop for MeteredStream {
    fn drop(&mut self) {
        self.release();
    }
}

/// Real-provider settings are never inferred. The caller must pass
/// `--provider real`, `--allow-real-provider`, and `VERLINPARK_BENCH_REAL_PROVIDER=1`.
#[derive(Clone)]
pub struct RealProviderSettings {
    pub provider_id: String,
    pub base_url: String,
    pub model: String,
    api_key: Option<String>,
}

impl RealProviderSettings {
    pub const GATE_ENV: &'static str = "VERLINPARK_BENCH_REAL_PROVIDER";

    pub fn gate_enabled() -> bool {
        std::env::var(Self::GATE_ENV)
            .ok()
            .is_some_and(|value| matches!(value.trim(), "1" | "true" | "yes"))
    }

    pub fn from_env(model_override: Option<&str>) -> anyhow::Result<Self> {
        if !Self::gate_enabled() {
            anyhow::bail!(
                "real-provider mode is disabled; set {} = 1 and pass --allow-real-provider",
                Self::GATE_ENV
            );
        }
        let base_url = std::env::var("VERLINPARK_BENCH_BASE_URL")
            .ok()
            .or_else(|| std::env::var("VERLINPARK_BASE_URL").ok())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "real-provider mode needs VERLINPARK_BENCH_BASE_URL (or VERLINPARK_BASE_URL)"
                )
            })?;
        let model = model_override
            .map(str::to_string)
            .or_else(|| std::env::var("VERLINPARK_BENCH_MODEL").ok())
            .or_else(|| std::env::var("VERLINPARK_MODEL").ok())
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "real-provider mode needs VERLINPARK_BENCH_MODEL (or VERLINPARK_MODEL)"
                )
            })?;
        let api_key = std::env::var("VERLINPARK_BENCH_API_KEY")
            .ok()
            .or_else(|| std::env::var("VERLINPARK_API_KEY").ok())
            .filter(|value| !value.is_empty());
        Ok(Self {
            provider_id: "openai-compatible".to_string(),
            base_url,
            model,
            api_key,
        })
    }

    /// Constructor for tests and embedders. The key is deliberately not
    /// exposed through a Debug implementation.
    pub fn from_parts(
        base_url: impl Into<String>,
        model: impl Into<String>,
        api_key: Option<String>,
    ) -> Self {
        Self {
            provider_id: "openai-compatible".to_string(),
            base_url: base_url.into(),
            model: model.into(),
            api_key,
        }
    }

    pub fn into_provider(self) -> std::sync::Arc<dyn Provider> {
        std::sync::Arc::new(vp_providers::OpenAiCompatibleProvider::new(
            self.provider_id,
            self.base_url,
            self.api_key,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(system: &str, messages: Vec<ChatMessage>) -> ChatRequest {
        ChatRequest {
            model: "bench".to_string(),
            messages,
            system: Some(system.to_string()),
            system_stable_prefix: None,
            max_tokens: Some(128),
            temperature: Some(0.0),
            tools: Vec::new(),
        }
    }

    async fn collect(
        provider: &FakeProvider,
        request: ChatRequest,
    ) -> Result<Vec<StreamEvent>, ProviderError> {
        use futures_util::StreamExt;
        let mut stream = provider.stream_chat(request).await?;
        let mut events = Vec::new();
        while let Some(item) = stream.next().await {
            events.push(item?);
        }
        Ok(events)
    }

    #[test]
    fn target_marker_extraction_is_bounded_to_safe_paths() {
        assert_eq!(
            extract_target_marker("Create bench-target-main.txt with content").unwrap(),
            "bench-target-main.txt"
        );
        assert_eq!(
            extract_target_marker("bench-target-T1.txt; done").unwrap(),
            "bench-target-T1.txt"
        );
        assert!(extract_target_marker("no marker here").is_none());
        assert!(extract_target_marker("bench-target-").is_none());
    }

    #[tokio::test]
    async fn sequence_provider_is_deterministic_and_charges_usage() {
        let provider = FakeProvider::sequence(
            "bench",
            vec![
                vec![ScriptAction::text("first")],
                vec![ScriptAction::tool(
                    "write_file",
                    json!({"path": "x.txt", "content": "x"}),
                )],
            ],
            FakeUsageModel::fixed(100, 10, 40, 5),
            8,
        );
        let first = collect(&provider, request("system", vec![ChatMessage::user("go")]))
            .await
            .unwrap();
        assert!(matches!(
            first[0],
            StreamEvent::Usage {
                input_tokens: 100,
                ..
            }
        ));
        assert!(matches!(first[1], StreamEvent::TextDelta(_)));
        let second = collect(
            &provider,
            request("system", vec![ChatMessage::user("again")]),
        )
        .await
        .unwrap();
        assert!(matches!(second[1], StreamEvent::ToolCall(_)));
        let transcript = provider.transcript();
        assert_eq!(transcript.len(), 2);
        assert_eq!(transcript[0].input_tokens, 100);
        assert_eq!(transcript[1].cache_read_tokens, 40);
        assert_eq!(provider.remaining_sequence(), 0);
    }

    #[tokio::test]
    async fn sequence_exhaustion_is_visible_and_bounded() {
        let provider = FakeProvider::sequence(
            "bench",
            vec![vec![ScriptAction::text("only")]],
            FakeUsageModel::fixed(10, 1, 0, 0),
            8,
        );
        let _ = collect(&provider, request("system", vec![ChatMessage::user("go")]))
            .await
            .unwrap();
        let error = collect(&provider, request("system", vec![ChatMessage::user("go")]))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("exhausted"));
        let summary = provider.summary();
        assert!(summary.exhausted);
        assert_eq!(summary.errors, 1);
    }

    #[tokio::test]
    async fn simulated_usage_is_request_sized_and_cache_disjoint() {
        let provider = FakeProvider::bench(
            "bench",
            BenchBehavior::direct("bench-target.txt", "content\n", None),
            FakeUsageModel::benchmark_default(),
            Duration::ZERO,
            16,
        );
        let small = collect(&provider, request("short", vec![ChatMessage::user("go")]))
            .await
            .unwrap();
        let large = collect(
            &provider,
            request("short", vec![ChatMessage::user("go ".repeat(100))]),
        )
        .await
        .unwrap();
        let small_usage = match small[0] {
            StreamEvent::Usage {
                input_tokens,
                cache_read_tokens,
                cache_write_tokens,
                ..
            } => (input_tokens, cache_read_tokens, cache_write_tokens),
            _ => panic!("first event was not usage"),
        };
        let large_usage = match large[0] {
            StreamEvent::Usage {
                input_tokens,
                cache_read_tokens,
                cache_write_tokens,
                ..
            } => (input_tokens, cache_read_tokens, cache_write_tokens),
            _ => panic!("first event was not usage"),
        };
        assert!(large_usage.0 > small_usage.0);
        assert!(small_usage.1 + small_usage.2 <= small_usage.0);
        assert!(large_usage.1 + large_usage.2 <= large_usage.0);
    }

    #[tokio::test]
    async fn direct_behavior_writes_then_finishes() {
        let provider = FakeProvider::bench(
            "bench",
            BenchBehavior::direct("bench-target.txt", "expected\n", None),
            FakeUsageModel::fixed(100, 10, 50, 5),
            Duration::ZERO,
            16,
        );
        let first = collect(
            &provider,
            request(
                "You are the Leader agent of VerlinPark",
                vec![ChatMessage::user("go")],
            ),
        )
        .await
        .unwrap();
        assert!(first.iter().any(|event| matches!(
            event,
            StreamEvent::ToolCall(ToolCall { name, .. }) if name == "write_file"
        )));
        let second = collect(
            &provider,
            request(
                "You are the Leader agent of VerlinPark",
                vec![
                    ChatMessage::user("go"),
                    ChatMessage::assistant_tool_calls(
                        "",
                        vec![ToolCall {
                            id: "write".into(),
                            name: "write_file".into(),
                            arguments: "{}".into(),
                        }],
                    ),
                    ChatMessage::tool("write", "{\"sha256\":\"abc\"}"),
                ],
            ),
        )
        .await
        .unwrap();
        assert!(second.iter().any(
            |event| matches!(event, StreamEvent::TextDelta(text) if text.contains("complete"))
        ));
    }

    #[tokio::test]
    async fn worker_behavior_uses_the_configured_target() {
        let mut targets = BTreeMap::new();
        targets.insert("bench-target-T2.txt".to_string(), "T2\n".to_string());
        targets.insert("bench-target-T3.txt".to_string(), "T3\n".to_string());
        let provider = FakeProvider::bench(
            "bench",
            BenchBehavior::plan(targets, true),
            FakeUsageModel::fixed(100, 10, 50, 5),
            Duration::ZERO,
            16,
        );
        let events = collect(
            &provider,
            request(
                "You are a Worker, reporting to a Manager.",
                vec![ChatMessage::user(
                    "Execute plan task T3: Create bench-target-T3.txt",
                )],
            ),
        )
        .await
        .unwrap();
        let arguments = events
            .iter()
            .find_map(|event| match event {
                StreamEvent::ToolCall(ToolCall {
                    name, arguments, ..
                }) if name == "write_file" => Some(arguments),
                _ => None,
            })
            .expect("write_file call");
        let parsed: Value = serde_json::from_str(arguments).unwrap();
        assert_eq!(parsed["path"], "bench-target-T3.txt");
        assert_eq!(parsed["content"], "T3\n");
    }

    #[tokio::test]
    async fn verifier_behavior_approves_or_rejects() {
        for approved in [true, false] {
            let provider = FakeProvider::bench(
                "bench",
                BenchBehavior::plan(BTreeMap::new(), approved),
                FakeUsageModel::fixed(100, 10, 50, 5),
                Duration::ZERO,
                16,
            );
            let events = collect(
                &provider,
                request(
                    "You are a Verifier, reporting to a Manager.",
                    vec![ChatMessage::user("verify")],
                ),
            )
            .await
            .unwrap();
            let arguments = events
                .iter()
                .find_map(|event| match event {
                    StreamEvent::ToolCall(ToolCall {
                        name, arguments, ..
                    }) if name == "submit_verdict" => Some(arguments),
                    _ => None,
                })
                .expect("submit_verdict call");
            let parsed: Value = serde_json::from_str(arguments).unwrap();
            assert_eq!(parsed["approved"], approved);
        }
    }

    #[tokio::test]
    async fn compaction_prompt_wins_over_the_agent_role() {
        let provider = FakeProvider::bench(
            "bench",
            BenchBehavior::direct("bench-target.txt", "content\n", None),
            FakeUsageModel::fixed(100, 10, 50, 5),
            Duration::ZERO,
            16,
        );
        let events = collect(
            &provider,
            request(
                "You are compacting an AI coding agent conversation.",
                vec![ChatMessage::user("long transcript")],
            ),
        )
        .await
        .unwrap();
        assert!(events.iter().any(
            |event| matches!(event, StreamEvent::TextDelta(text) if text.contains("summary"))
        ));
    }

    #[test]
    fn usage_validation_rejects_overlapping_fixed_cache() {
        assert!(FakeUsageModel::fixed(10, 1, 11, 0).validate().is_err());
        assert!(FakeUsageModel::benchmark_default().validate().is_ok());
        assert!(
            FakeUsageModel::Simulated {
                base_input_tokens: 0,
                output_tokens: 0,
                cache_read_bps: 6_000,
                cache_write_bps: 5_000,
            }
            .validate()
            .is_err()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn metering_provider_measures_peak_worker_streams() {
        use futures_util::StreamExt;
        let mut targets = BTreeMap::new();
        targets.insert("bench-target.txt".to_string(), "content\n".to_string());
        let base = Arc::new(FakeProvider::bench(
            "bench",
            BenchBehavior::plan(targets, true),
            FakeUsageModel::fixed(100, 10, 50, 5),
            Duration::from_millis(50),
            32,
        ));
        let metered = Arc::new(MeteringProvider::new(base));
        let mut tasks = Vec::new();
        for _ in 0..4 {
            let provider = metered.clone();
            tasks.push(tokio::spawn(async move {
                let mut stream = provider
                    .stream_chat(request(
                        "You are a Worker, reporting to a Manager.",
                        vec![ChatMessage::user(
                            "Execute plan task T1: Create bench-target.txt",
                        )],
                    ))
                    .await
                    .unwrap();
                while stream.next().await.is_some() {}
            }));
        }
        for task in tasks {
            task.await.unwrap();
        }
        assert_eq!(metered.peak_workers(), 4);
    }

    #[test]
    fn real_provider_threshold_is_explicit() {
        let settings = RealProviderSettings::from_parts("http://127.0.0.1:9/v1", "local", None);
        assert_eq!(settings.provider_id, "openai-compatible");
        assert_eq!(settings.model, "local");
    }
}
