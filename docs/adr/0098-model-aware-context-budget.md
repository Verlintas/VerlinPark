# ADR-0098: Model-aware context budgets and rolling trim

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

ADR-0097 introduced compaction with a fixed default threshold of 24,000 estimated
tokens and only at the start of a top-level run. That is wrong for both ends of
the market:

- A 1M-token model would compact at 2.4% of its window and lose usable context.
- An 8k local model would still overflow if the summary plus retained tail was
  too large, and a failed summary left the run with no fallback.

The threshold must be derived from the model's actual context window, with a
per-role override when roles use different models, and there must be a lossy
rolling-window fallback for small or failing models.

## Decision

1. **Configured context window.** `AgentConfig.context_window` is the base model
   window in tokens (`VERLINPARK_CONTEXT_WINDOW`, default 128,000; 0 disables
   automatic context management). Each `RoleModelConfig` can override it with
   `context_window`. The RPC `agent.configure` and Settings > Providers expose
   the base window and a per-role `ctx` field.
2. **Derived limits.** The hard limit is:
   `window - max_output_tokens - system_prompt_tokens - tool_schema_tokens - 512`.
   The summarization threshold is 75% of that hard limit. Context estimates
   use 4 ASCII characters per token and 1.5 tokens per non-ASCII character,
   closer to a real tokenizer than the conservative bytes/2 budget estimate;
   the summary transcript is capped by the same model window. An explicit
   `VERLINPARK_COMPACT_AFTER_TOKENS` still overrides the threshold (clamped to
   the hard limit); 0 means derive it from the window. A 1M model gets a
   ~750k threshold; an 8k model gets a small one based on its actual reserve.
3. **Per-turn compaction.** The check runs before every top-level provider call,
   not only at run start. The run now tracks stored message IDs for assistant
   and tool messages appended during the run, so a mid-run summary has a valid
   `through_message_id`.
4. **Summary plus rolling trim.** The primary strategy is still a persistent
   summary plus a retained tail. The summary transcript is capped by the same
   model window minus its output reserve. Safe cuts are at `user` or `assistant`
   boundaries, never at a `tool` message. If summarization fails, is skipped for
   budget, or the active context is still above the hard limit, older messages
   are dropped at safe boundaries as a rolling window. The summary (if any) is
   kept at index 0; the trim event records `mode: "trim"`, dropped counts, and
   the final `through_message_id`. Original messages remain in SQLite.
   Threshold and hard-limit comparisons include the system prompt and tool
   schema tokens, because they are part of the same provider request; comparing
   message tokens alone can defer compaction until the request already
   overflows.
5. **Persistence.** Both modes use `context.compacted`:
   `mode: "summary"` carries the summary text; `mode: "trim"` carries no summary.
   `load_conversation` starts after `through_message_id` and prepends the
   summary only for summary mode. This makes resume/import/replay respect both
   the summary and the rolling-window cut.
6. **Defaults.** Context management is on by default with a 128k base window;
   users of local/small models set 8k/16k/32k, and users of 1M models set 1M.
   Setting the window to 0 disables automatic compaction/trimming, leaving the
   provider to fail on overflow.

7. **Observability.** Before each top-level provider call the runtime emits
   `context.usage` with the estimated active tokens, configured window, summary
   threshold, hard limit, and message count. `session.context` / `vp context`
   returns the latest usage plus the latest `context.compacted` summary, and the
   Stage header shows `ctx used/threshold`. These are diagnostic events, not a
   projection; original messages and compaction events remain replayable.

## Consequences

- Model size is no longer hard-coded; the same runtime scales from 8k local
  models to 1M hosted models.
- Long single runs can compact mid-run and keep progressing; a failed summary
  degrades to a lossy rolling window instead of overflowing.
- Mid-run trimming can drop messages that were not included in the summary
  (the event marks `dropped_messages`); the system prompt still carries the
  task objective.
- The hard limit is estimated from text length and the configured output
  reserve; tokenizers differ and tool schemas can be large, so users should
  leave headroom or lower `max_tokens`. The estimate is closer to a real
  tokenizer than the conservative budget reservation but is not exact.
- `context.usage` is written once per top-level turn; long runs add one event
  per turn, which is acceptable for diagnostics and search but is not a
  high-frequency metrics stream.
- Each compaction/trim emits an event; no messages are deleted from storage.
- Per-role context windows are required when a local Worker/Verifier model runs
  beside a 1M-token Leader.

## Alternatives considered

- **Fixed 24k threshold:** too restrictive for large models and unsafe for
  small ones.
- **Provider-native context management:** not portable across OpenAI-compatible,
  Anthropic, and local endpoints.
- **Trim only (no summary):** cheap but loses decisions and tool evidence.
- **Summary only:** fails when the summary request itself is too large or the
  model cannot summarize; a rolling fallback is needed.
