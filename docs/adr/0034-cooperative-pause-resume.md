# ADR-0034: Cooperative pause/resume at turn and tool boundaries

- **Status:** Accepted
- **Date:** 2026-10-30

## Context

A Leader run can last many turns and call slow or non-cancellable tools. Users
need a way to hold a run without losing the conversation or starting a new agent
run. Aborting a provider stream or a running shell command mid-flight would
discard partial output and could leave side effects behind; hard preemption also
needs checkpointing, which the runtime does not have.

Cancellation already exists ([ADR-0018](0018-budget-cancel.md)). Pause is a
different control: keep the current run alive, stop issuing new provider turns and
tools, then continue the same run on resume.

## Decision

1. **Session-level RPC.** `session.pause` / `session.resume` take a session ID and
   return `paused` / `resumed`. The CLI exposes `vp pause <session>` and
   `vp resume <session>`; the app shows Pause/Resume next to Stop.
2. **Cooperative boundaries.** Pause sets a per-run watch flag. The Leader checks
   it before each turn and before each tool call. An in-flight provider stream or
   tool is allowed to finish; the run then blocks before the next step. Pause does
   not abort the current stream or kill a running process.
3. **The same run resumes.** Resume clears the flag; the existing loop, message
   history, budget state, and run guard continue. No new agent run is created and
   the user does not need to resend the prompt.
4. **Events and status.** Pause writes session status `paused` plus
   `agent.paused`; resume writes `running` plus `agent.resumed`. `session.updated`
   drives the app state. Sending a new prompt while paused is rejected because the
   run is still active.
5. **Terminal state wins.** Pause, resume, and run finalization share a per-run
   finalized guard. Once the run has finalized, pause/resume return `false` and
   cannot overwrite `idle`, `cancelled`, or `failed`. If pause lands first, the
   later finalization overwrites `paused` with the terminal status.
6. **Cancel works while paused.** `session.cancel` remains available; the paused
   wait watches the cancel flag and finalizes the run as `cancelled`.
7. **Restart is not resume.** Daemon recovery still marks `running` and `paused`
   sessions `failed`; pause state is in-memory only and there is no checkpoint.

## Consequences

- Pause latency is bounded by the current provider stream or tool call, not by an
  immediate preemption guarantee.
- The conversation stays intact and resume costs no extra setup tokens.
- Paused sessions still occupy their run slot and cannot be prompted twice.
- No per-agent or per-task pause, no persisted pause across daemon restarts, and
  no checkpoint/continue after a crash.
- Shell commands started before pause are not interrupted by pause; use cancel to
  stop the run.

## Alternatives considered

- **Abort the provider stream and restart later:** loses partial output and needs
  checkpoint/conversation reconstruction.
- **Persist a paused state and resume after daemon restart:** requires durable run
  state plus worktree/provider reattachment; not implemented.
- **Only support cancel:** forces users to discard the run and resend context.
