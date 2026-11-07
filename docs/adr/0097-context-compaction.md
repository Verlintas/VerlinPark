# ADR-0097: Context compaction for long sessions

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

A top-level session reloads its stored messages on every run. Long sessions,
large tool outputs, and repeated plan work can make that history exceed the
provider's context window or make every turn unnecessarily expensive. The
session token budget limits spending, but it does not reduce the prompt itself.
VerlinPark had no context compaction or checkpoint mechanism.

## Decision

1. **Compact only top-level runs, at run start.** Before the first provider
   call, a top-level conversation is estimated from message content and tool
   calls. If it reaches `VERLINPARK_COMPACT_AFTER_TOKENS` (default 24,000;
   `0` disables) and there is a safe user-message boundary before the current
   prompt, the older portion is summarized. `VERLINPARK_COMPACT_KEEP_MESSAGES`
   (default 8) controls how many recent messages remain outside the summary.
   Child Leader/Manager/Worker/Verifier runs are short-lived and are not
   compacted.
2. **Summarize with the configured provider.** The summary request uses the
   same user-configured model and provider as the agent, with a fixed
   preservation-oriented system prompt and a 1,024-token output cap. The call is
   recorded in the usage ledger as role `compaction`; an estimated reservation
   is settled when the call completes. Compaction is best-effort: an error emits
   `context.compact_failed` and the run continues with the full history.
3. **Persist a summary event, not a fake message.** `context.compacted` carries
   the summary, `through_message_id` (the last summarized stored message),
   summarized/kept counts, estimated tokens before compaction, and usage. The
   original messages remain in the `messages` table for search, replay, export,
   transcript, and audit. No synthetic assistant/user message is inserted into
   the message projection.
4. **Resume from the summary.** `load_conversation` finds the latest
   `context.compacted` event and returns `[summary user message] + messages
   after through_message_id`. The summary is included in the active provider
   context; it is not reinserted into the stored history.
5. **Only one compaction per run.** Compaction runs before the turn loop; it
   does not rewrite the conversation between tool calls. This keeps tool-call /
   tool-result pairing intact and avoids repeatedly paying for summaries inside
   one turn.

## Consequences

- Long sessions send less context and cost less per turn after compaction.
- The summary may omit details that were not deemed important; the original
  messages remain available for the user and for search.
- Compaction itself consumes tokens and adds one provider round trip on the
  first run after the threshold is crossed.
- The summary is injected as a user-role message because Anthropic and OpenAI
  handle system-role messages differently. It is prefixed with "Earlier
  conversation summary (auto-compacted)".
- A compaction failure does not fail the run, but the next run may retry and
  incur the same extra call.
- There is still no checkpoint/resume or provider-native context management;
  compaction is a local summary strategy.

## Alternatives considered

- **Sliding window:** cheap but drops decisions and tool evidence without a
  summary.
- **Hard truncation:** even cheaper, but loses causality and can strand open
  tasks.
- **Provider-native context caching/management:** not portable across
  OpenAI-compatible, Anthropic, and local endpoints.
- **Compact every turn:** keeps context small but adds latency/cost to every
  turn and risks repeated information loss.
