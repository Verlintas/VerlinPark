# ADR 0124: Role-aware context management and persistent team memory

- Status: Accepted
- Date: 2026-11-09
- Area: Agent runtime

## Context

Context compaction was only applied to top-level runs. Plan Workers, Labour,
Managers, and Verifiers could grow their conversation until the provider
rejected the request. Compaction also used one strategy for every role:
summarize with a model, then trim. That is the right trade-off for a Leader or
Manager, where preserving intent and cross-task decisions matters, but it is
wasteful for a Worker or Labour agent whose useful context is mostly the
current objective and recent tool results.

Project memory was persisted and exposed through `remember`/`recall`, but a
Worker only used it if it explicitly called `recall`. Completed task results
were not stored automatically, so a new session started with no durable team
history.

## Decision

1. **Two context strategies.**
   - `Leader` and `Manager`: summarize older messages with the provider, keep a
     persistent summary at the head, then roll the tail. This favors precision
     and continuity.
   - `Worker`, `Labour`, and `Verifier`: skip the summarizer and use a
     provider-free rolling window. This favors efficiency and avoids spending
     output tokens on a summary the task does not need.
2. **Compaction runs for every role.** The old `top_level` gate is removed.
   The threshold still includes the system prompt and tool schemas.
3. **Hard-limit safety over retention.** If the configured retained tail still
   exceeds the hard limit, the trim loop may reduce retention to two messages
   before giving up. The summary message is preserved when one exists.
4. **Non-persisted conversations can summarize.** Manager/Worker runs without
   message IDs still use the role strategy; `through_message_id` is simply
   absent for those ephemeral conversations.
5. **Task results become team memory.** A completed plan task writes a
   `task_result` project memory (idempotent by title) with the Worker/Verifier
   summary and the task id tag. The Orchestrator is the author.
6. **Memory is recalled automatically.** Every agent system prompt receives a
   small project-memory brief relevant to the session goal; plan Workers also
   receive a task-specific brief. This supplements, not replaces,
   `remember`/`recall`, which remain explicit tools.
7. **No hidden model routing.** Context policy is deterministic by role and
   does not choose a different provider/model.

## Consequences

- A long plan Worker now trims instead of failing the provider request.
- Leaders and Managers keep a summary across a long conversation; Workers and
  Labour avoid a summarization call.
- Task history survives session boundaries and is visible to new agents.
- Memory grows over time; the existing project memory retention policy and
  manual delete/prune controls remain the cleanup mechanism.
- The memory brief consumes context. It is capped at a few items and 420 chars
  per item.
