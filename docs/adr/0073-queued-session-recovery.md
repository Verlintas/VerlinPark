# ADR-0073: Recover queued sessions across daemon restarts

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Global session admission queues runs in memory. If the daemon restarted while a
run was waiting for a global session slot, recovery marked the session failed
even though no provider call had started. The user’s prompt was still stored,
but the run was lost.

## Decision

1. **Queued is recoverable.** `recover_state` still fails `running` and `paused`
   sessions, but leaves `queued` sessions queued. Running agents, pending
   approvals, and in-flight merge entries keep their existing recovery behavior.
2. **Prompt from the event log.** `queued_session_prompts` reads the newest
   `agent.queued` event per queued session and returns its prompt. The user
   message is already in the conversation, so recovery does not append it again.
3. **Re-enqueue on startup.** `AgentService::resume_queued_sessions` registers a
   run control, enters the same global session admission path, and starts the
   existing `run_agent` flow without re-emitting the queued event or user
   message.
4. **Re-enqueue after configuration.** The daemon calls the same method after
   `agent.configure`, so queued sessions can wait for a provider to be
   configured instead of being failed.
5. **No new scheduler.** This preserves the existing session admission order;
   it does not add persistent Task-DAG priority, cross-session task leases, or
   fair queueing.
6. **No automatic re-run of active work.** Sessions that were running or paused
   when the daemon stopped are still failed; only not-yet-started queued runs are
   re-enqueued.

## Consequences

- A queued prompt survives a daemon restart and starts when capacity/provider
  configuration allows.
- Duplicate user messages are avoided by reusing the stored conversation.
- A queued session without a configured provider remains queued until
  `agent.configure` runs.
- Cancelling a recovered queued session still relies on an active run control;
  there is no separate persisted queue-cancel path yet.
- Queued sessions can be re-enqueued more than once only if no run control is
  registered; the in-memory guard prevents duplicate active runs.

## Alternatives considered

- **Mark queued failed:** honest but discards work the user already submitted.
- **Persist a separate queue table:** useful for a future global scheduler, but
  the event log already stores the prompt and status.
- **Resume running sessions:** unsafe because the provider stream and tool side
  effects may be partial.
