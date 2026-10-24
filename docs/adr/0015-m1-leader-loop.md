# ADR-0015: M1 Leader loop, tool approvals, and provider configuration

- **Status:** Accepted
- **Date:** 2026-10-24

## Context

M1 starts the agent runtime. The first slice must be useful on a real repository
before the four-tier scheduler and per-task worktrees exist. It also needs a
credential path that respects the invariant that API keys live in Keychain and
never in logs or the SQLite store.

The runtime must remain inspectable: every prompt, streamed delta, tool call,
approval, and result has to be an event that a client can replay.

## Decision

1. **One Leader per session.** `session.create` and `session.prompt` start a
   Leader run. The run loads persisted messages, streams provider output, executes
   tool calls sequentially, and stops on a final assistant message, an empty turn,
   a denial, or the turn limit.
2. **Read-only tools run automatically.** `read_file`, `list_dir`, and `grep` are
   auto-approved. `write_file` and `shell` always create a persisted approval and
   block the run until the user approves or denies. A timeout counts as a denial.
3. **Approvals are durable state.** Approvals are rows in SQLite, emitted as
   `approval.requested` / `approval.resolved` events. The in-memory waiter only
   wakes the run; the row remains the source of truth for clients.
4. **Tools are jailed to the project root.** Relative paths only; canonicalized
   ancestors must remain inside the root; writes through symlinks are refused.
   Shell runs in the root with a timeout and a small deny-list. This is a
   guardrail, not the full permission broker from the architecture.
5. **Provider configuration is an RPC.** The app stores the API key in Keychain
   and calls `agent.configure` on launch and whenever settings change. The daemon
   keeps the key in memory and never persists it. Environment variables remain a
   headless/CLI fallback.
6. **Both adapters support tool calls.** OpenAI-compatible streams reassemble
   indexed `tool_calls`; Anthropic streams reassemble `tool_use` input JSON and
   map tool results back into `tool_result` blocks.

## Consequences

- A single Leader can inspect a repository, propose edits, ask for approval, run
  commands, and report results end-to-end.
- A daemon restart does not resume an in-flight run or pending approval waiter.
  Persisted messages and approvals remain inspectable; automatic resume is a
  later milestone.
- Session worktrees landed in ADR-0016; the Leader writes on its session branch,
  not in the project root. Per-task worktrees for Managers/Workers/Labour are
  still future work.
- There is no token budget enforcement or context compaction yet. The turn limit
  is the only hard stop.
- The deny-list is deliberately conservative and incomplete; the real permission
  broker will classify commands and enforce role/path policies.

## Alternatives considered

- **Auto-approve writes in M1:** rejected; it would make the first run unsafe and
  hide the approval trail the product depends on.
- **Store provider keys in SQLite:** rejected by the security invariant.
- **Wait for the full worktree/scheduler design before any agent loop:** rejected;
  a single-Leader loop produces real evidence and exercises the event model now.
