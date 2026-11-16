use super::*;

pub(super) const STREAM_ATTEMPTS: u32 = 3;

pub(super) const STREAM_RETRY_BASE: Duration = Duration::from_millis(250);

#[derive(Default)]
pub(super) struct StreamTurn {
    pub(super) text: String,
    pub(super) tool_calls: Vec<ToolCall>,
    pub(super) usage: TokenUsage,
    /// Wall-clock latency to the first text or tool-call chunk. Zero means the
    /// stream produced neither.
    pub(super) first_token_ms: u64,
    pub(super) cancelled: bool,
    /// Set when a hard preemption token fired during setup or between chunks.
    /// The partial text/usage are persisted; the stream is never resumed or
    /// replayed.
    pub(super) preempted: Option<crate::preemption::Snapshot>,
    pub(super) error: Option<anyhow::Error>,
}

/// Provider usage for one request. `input` is the provider's total input and
/// already includes cache reads and cache writes; the cache fields are subsets
/// used for pricing and hit-rate telemetry, never additional tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct TokenUsage {
    pub(super) input: u64,
    pub(super) output: u64,
    pub(super) cache_read: u64,
    pub(super) cache_write: u64,
}

impl TokenUsage {
    pub(super) fn is_empty(self) -> bool {
        self.input == 0 && self.output == 0
    }

    pub(super) fn add(&mut self, input: u32, output: u32, cache_read: u32, cache_write: u32) {
        self.input = self.input.saturating_add(u64::from(input));
        self.output = self.output.saturating_add(u64::from(output));
        self.cache_read = self.cache_read.saturating_add(u64::from(cache_read));
        self.cache_write = self.cache_write.saturating_add(u64::from(cache_write));
    }
}

pub(super) fn stream_error_retryable(error: &ProviderError) -> bool {
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
        ProviderError::MissingApiKey
        | ProviderError::Parse(_)
        | ProviderError::Unsupported(_)
        | ProviderError::Preempted { .. } => false,
    }
}

pub(super) async fn emit_stream_retry(
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

pub(super) async fn stream_retry_delay(control: &RunControl, attempt: u32) -> bool {
    if control.is_cancelled() || control.is_preempted() {
        return false;
    }
    let delay = STREAM_RETRY_BASE.saturating_mul(attempt.max(1));
    let mut cancel_rx = control.subscribe();
    tokio::select! {
        _ = tokio::time::sleep(delay) => !control.is_cancelled() && !control.is_preempted(),
        _ = cancel_rx.changed() => false,
        _ = control.preempt.cancelled() => false,
    }
}

/// After a preemption, give the provider at most `grace` to deliver one final
/// usage event. Text/tool chunks are discarded: they are not appended to the
/// partial turn, persisted as a completed message, or replayed. This keeps
/// partial-usage accounting honest without extending generation arbitrarily.
pub(super) async fn drain_provider_grace(
    stream: &mut vp_providers::EventStream,
    grace: Duration,
    turn: &mut StreamTurn,
) {
    if grace.is_zero() {
        return;
    }
    let timer = tokio::time::sleep(grace);
    tokio::pin!(timer);
    loop {
        tokio::select! {
            _ = &mut timer => return,
            item = stream.next() => match item {
                Some(Ok(StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                })) => {
                    turn.usage.add(
                        input_tokens,
                        output_tokens,
                        cache_read_tokens,
                        cache_write_tokens,
                    );
                }
                Some(Ok(StreamEvent::Done)) | None => return,
                // The first non-usage chunk means the provider is still
                // generating. Stop instead of draining its output.
                Some(Ok(StreamEvent::TextDelta(_) | StreamEvent::ToolCall(_))) => return,
                Some(Err(_)) => return,
            },
        }
    }
}

/// Consume one provider stream turn. Transient failures before any output are
/// retried; after partial output the turn returns its error together with the
/// text/usage received so far so the caller can persist a partial message and
/// settle the budget honestly.
#[allow(clippy::too_many_arguments)]
pub(super) async fn consume_stream(
    engine: &Engine,
    session_id: &str,
    spec: &AgentSpec,
    control: &RunControl,
    provider: &dyn Provider,
    request: &ChatRequest,
    idle_timeout: Duration,
) -> Result<StreamTurn> {
    let mut turn = StreamTurn::default();
    let started = Instant::now();
    let mut attempt = 0;
    let preempt = control.preempt.clone();
    preempt.set_phase(crate::preemption::Phase::Provider, None);
    'streams: loop {
        attempt += 1;
        if let Some(snapshot) = preempt.snapshot() {
            turn.preempted = Some(snapshot);
            break 'streams;
        }
        let mut setup_cancel = control.subscribe();
        let setup = tokio::select! {
            changed = setup_cancel.changed() => {
                if changed.is_err() || *setup_cancel.borrow() {
                    turn.cancelled = true;
                    break 'streams;
                }
                continue;
            }
            _ = preempt.cancelled() => {
                turn.preempted = preempt.snapshot();
                break 'streams;
            }
            result = tokio::time::timeout(
                idle_timeout,
                provider.stream_chat_preemptible(request.clone(), Some(preempt.clone())),
            ) => result,
        };
        let mut stream = match setup {
            Ok(Ok(stream)) => stream,
            Ok(Err(error)) => {
                // The provider's own send path already retries transient
                // startup failures; do not multiply those attempts here.
                if error.is_preempted() {
                    turn.preempted = preempt.snapshot();
                    break;
                }
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
            if preempt.is_preempted() {
                drain_provider_grace(
                    &mut stream,
                    control.preemption_policy().provider_grace(),
                    &mut turn,
                )
                .await;
                turn.preempted = preempt.snapshot();
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
                _ = preempt.cancelled() => {
                    drain_provider_grace(
                        &mut stream,
                        control.preemption_policy().provider_grace(),
                        &mut turn,
                    )
                    .await;
                    turn.preempted = preempt.snapshot();
                    break 'streams;
                }
                item = tokio::time::timeout(idle_timeout, stream.next()) => match item {
                    Ok(item) => item,
                    Err(_) => {
                        let error = format!(
                            "provider stream was idle for {}s",
                            idle_timeout.as_secs()
                        );
                        if !produced
                            && turn.usage.is_empty()
                            && attempt < STREAM_ATTEMPTS
                            && !control.is_cancelled()
                            && !preempt.is_preempted()
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
                                if preempt.is_preempted() {
                                    turn.preempted = preempt.snapshot();
                                } else {
                                    turn.cancelled = true;
                                }
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
                    if turn.first_token_ms == 0 && !delta.is_empty() {
                        turn.first_token_ms =
                            started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                    }
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
                    if turn.first_token_ms == 0 {
                        turn.first_token_ms =
                            started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                    }
                    turn.tool_calls.push(call);
                }
                Ok(StreamEvent::Usage {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_write_tokens,
                }) => {
                    turn.usage.add(
                        input_tokens,
                        output_tokens,
                        cache_read_tokens,
                        cache_write_tokens,
                    );
                }
                Ok(StreamEvent::Done) => {}
                Err(error) => {
                    if error.is_preempted() {
                        turn.preempted = preempt.snapshot();
                        break 'streams;
                    }
                    if !produced
                        && turn.usage.is_empty()
                        && stream_error_retryable(&error)
                        && attempt < STREAM_ATTEMPTS
                        && !control.is_cancelled()
                        && !preempt.is_preempted()
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
                            if preempt.is_preempted() {
                                turn.preempted = preempt.snapshot();
                            } else {
                                turn.cancelled = true;
                            }
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
