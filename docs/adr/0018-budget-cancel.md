# ADR-0018: Session token budgets and cooperative cancellation

- **Status:** Accepted
- **Date:** 2026-10-25

> **Implementation note (2026-10-30):** Before a provider call the runtime
> reserves a conservative input estimate plus a dynamic output budget. Provider
> usage settles the reservation; endpoints without usage are charged a persisted
> estimate marked `estimated: true`. This removes the previous full-batch
> pre-flight overshoot, though tokenization is still an estimate rather than an
> exact tokenizer. See [AUDIT P0-2](../AUDIT.md).

## Context

The Leader loop can run for many turns and call tools repeatedly. Without a hard
stop, a bad prompt or a looping model can burn an unbounded amount of tokens.
Users also need a way to stop a run that is clearly going nowhere, without
killing the daemon or waiting for the approval timeout.

M1 has no pause/resume or checkpoint yet, so cancellation must be explicit about
what it guarantees.

## Decision

1. **Per-session token budget.** Sessions store `token_budget`, `tokens_in`, and
   `tokens_out`. New sessions default to 200,000 tokens; the New Session sheet and
   Settings can change the default. The budget is a hard stop on the sum of input
   and output tokens reported by the provider.
2. **Usage is persisted and evented.** After each provider turn the runtime adds
   the reported usage to the session counters and emits `agent.usage`. The UI
   shows used / budget in the session header.
3. **Exceeded means stop.** Before a turn and after recording usage, the runtime
   checks the budget. When the limit is reached it emits
   `agent.budget_exceeded`, marks the session failed, and does not issue another
   provider call.
4. **Cooperative cancellation.** `session.cancel` sets a per-run watch flag. The
   run checks it before turns, while streaming, before tools, and while waiting
   for approval. A dropped provider stream closes the connection; a dropped shell
   future kills the process (`kill_on_drop`). Pending approvals are denied.
5. **Cancel is terminal for the current run.** The session becomes `cancelled`;
   the user can send a new prompt to start a new run. Pause/resume is not claimed.
6. **CLI and UI.** The UI shows a Stop button while running; `vp cancel <session>`
   does the same from the terminal.

## Consequences

- A runaway loop stops at a predictable token count when the provider reports
  usage.
- Providers that omit usage cannot enforce a token budget. The current adapters
  parse OpenAI-compatible and Anthropic usage; other endpoints must report it.
- Token budgets are not yet expressed in USD or wall-clock time, and tool output
  is not separately metered.
- Cancellation is cooperative. A non-cancellable blocking operation can delay the
  stop until it returns; the shell path is killed on drop.
- A cancelled run does not automatically roll back worktree changes. Use the
  existing discard action or inspect the diff.

## Alternatives considered

- **Kill the whole daemon:** destroys all sessions and loses the event stream.
- **Hard-abort the task with `abort`:** cannot run cleanup or write terminal
  events reliably.
- **Budget only in the UI:** the daemon would remain unbounded when driven by the
  CLI.
