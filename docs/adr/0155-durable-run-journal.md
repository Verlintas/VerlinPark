# ADR-0155: Durable run journal and crash resume

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Store, Protocol
- **Related:** ADR-0151 (idempotent session operations), AGENTS.md invariants 1–5,
  `docs/LONG_TASK_RUNTIME_PLAN.md` R1

## Context

A daemon crash during an agent run loses the only precise record of what the run
did. `session_operations` prevents duplicate prompt admission, but it says
nothing about which provider calls, tool calls, checkpoints, approvals, or
questions already happened inside the run. Marking the session `failed` is
honest but not sufficient: a blind replay can repeat a completed write, while a
blind reset can discard work that actually landed.

The runtime needed an intent/outcome journal at the smallest safe boundary:
before a provider request or tool invocation, and after its outcome is durable.
The journal must let an operator answer:

1. Which run was active, for which agent/task/department, under which budget and
   worktree HEAD?
2. Which provider turns and tool calls completed?
3. Which calls have an unknown outcome because the process died between the side
   effect and the row commit?
4. Is it safe to create a successor attempt, and what must the model be told so
   it does not repeat a write or assume an ambiguous shell command succeeded?

This ADR records the durable model, the resume boundary, the duplicate-write
hard block, the operational/replay split, the crash windows, and the
exactly-once limits. It does not claim that provider calls are exactly-once, and
it does not implement mid-stream resume.

## Decision

### 1. Durable run model

The store gains four tables.

`session_runs` is the durable run header:

| Column | Meaning |
|---|---|
| `run_id` | UUIDv7 primary key |
| `session_id`, `project_id` | Owning session/project |
| `agent_id`, `agent_name`, `agent_role` | Agent identity and structural role |
| `task_id`, `department` | Task-scoped run context when present |
| `objective` | Bounded copy of the run objective (500 chars) |
| `provider`, `model` | Exact provider/model recorded at start |
| `status` | `running`, `completed`, `failed`, `interrupted`, `resumed` |
| `attempt` | 1-based attempt in the resume lineage |
| `resumed_from_run_id`, `resume_root_id` | Lineage links |
| `top_level` | Whether this is a session-level Leader run |
| `started_at`, `updated_at`, `ended_at` | Timestamps |
| `budget_json` | Snapshot of token/cost/time limits and spend at the boundary |
| `worktree_head` | HEAD captured at run start |
| `plan_revision` | Plan revision captured at run start |
| `last_event_seq` | Operational bookkeeping, not part of the replay contract |
| `failure_json` | Typed terminal classification and message |

`run_steps` records intent/outcome for `model_request`, `tool_call`,
`compaction`, `approval`, and `question` steps: step id, run id, sequence, turn,
kind, status (`intent`, `completed`, `failed`, `unknown`), input/output SHA-256,
bounded payload JSON, and timestamps. A step sequence is per run and allocated
inside the same transaction as its event.

`run_tool_calls` records one operational row per tool invocation: run, step,
provider tool-call id, tool name, canonical input fingerprint, status, bounded
result/error JSON, consumption metadata, and timestamps. It is intentionally
operational state (see §5).

`run_checkpoints` records one row per safe turn boundary: run, sequence, agent
turn, message cursor (count of persisted `message.created` events), event
cursor (latest event sequence), budget snapshot, worktree HEAD, and plan
revision.

A project-level `SessionRunPolicy` is persisted in
`project_session_run_policies` and the project manifest:

```text
SessionRunPolicy {
  auto_resume: bool = false,
  max_attempts: 1..=3 = 1,
  max_age_hours: 1..=168 = 24,
}
```

All new protocol fields are `serde(default)`; older clients and older project
manifests decode with the safe defaults (no auto resume, one attempt, 24 hours).

### 2. Events and projection ownership

The following session events are emitted before their projections are used:

| Event | Projection duty |
|---|---|
| `session.run.started` | Insert the run row |
| `session.run.checkpointed` | Insert the checkpoint row |
| `session.run.step_intent` | Insert the step row |
| `session.run.step_completed` | Update the step row |
| `session.run.tool_intent` | Audit event only; tool rows are operational |
| `session.run.tool_completed` | Audit event only; tool rows are operational |
| `session.run.interrupted` | Set run status/failure/end time |
| `session.run.resumed` | Set the previous run to `resumed`; carries its full row |
| `session.run.failed` | Set run status/failure/end time |
| `session.run.completed` | Set run status/end time |
| `session.run.cancel_requested` | Operational audit intent; recovery uses it to classify a crash as `cancelled` |

Each insert/update and its event commit in one SQLite transaction, and the event
mirror is appended before the operation returns. Run status, attempt, lineage,
budget snapshot, worktree head, plan revision, failure classification, step
rows, and checkpoints are replayable. `session.replay` compares them to the
event-derived projection, `session.replay {repair:true}` replaces them,
`session.export` carries them in the bundle, bundle import rebuilds them from
events, and `vp db-rebuild` restores them from the session event mirrors.

Tool outcome rows are explicitly exempt from replay/repair comparison. A repair
never deletes a run that still owns tool rows. This is the documented
operational-state exception required by the repository invariants.

### 3. Intent/outcome protocol and safe boundary

Before each provider request, the runtime:

1. emits/updates a checkpoint at the turn boundary;
2. records a `model_request` step with `intent` and a hash of the request;
3. calls the provider;
4. persists the assistant message (top-level) and records the step
   `completed` with output hash and usage.

Before each tool call, the runtime records a `tool_call` step `intent` and a
`run_tool_calls` row carrying the tool name and canonical input fingerprint.
After the tool returns, it records the step and tool row `completed`/`failed`
with a bounded result/error. The step/tool completion is written before the tool
message is appended, so a crash after the side effect but before the message is
recoverable from the journal.

Compaction is recorded as a completed step around the compaction call, with
before/after event cursors. Approval waits and user questions are recorded as
intent/completed steps so a crash while waiting is visible. Summarizer provider
calls inside compaction are covered by the compaction step, not by a separate
provider-level step.

The run finishes as:

| Outcome | Run status | Failure kind |
|---|---|---|
| Final answer / verdict | `completed` | none |
| Tool or provider error / max turns | `failed` | `error` or `max_turns` |
| Session/operator cancellation | `interrupted` | `cancelled` |
| Budget stop | `failed` | `budget_exceeded` |
| Daemon restart | `interrupted` | `interrupted` |
| New top-level prompt | unchanged | `superseded` (no run loop) |

Cancellation is deliberately terminal and not resumable. The schema follows the
R1 status allowlist; the failure object carries the `cancelled` distinction.

### 4. Startup recovery

On daemon startup, `Engine::recover_interrupted_sessions` first performs the
existing session/agent/approval/question/plan recovery, then
`interrupt_running_session_runs`:

- every `running` run becomes `interrupted` with failure kind `interrupted`,
  unless a durable `session.run.cancel_requested` audit event exists for that
  run, in which case the failure kind is `cancelled` and auto/manual resume
  refuse it;
- every `run_tool_calls` row still at `intent` becomes `unknown` and receives a
  bounded error explaining that the daemon could not commit the outcome;
- every replay claim is released, because a replay result claimed by the dead
  process may never have reached the persisted conversation;
- model/approval/question/compaction step intents stay `intent`; the resume
  classifier treats a model intent as retryable (no external side effect), while
  a tool intent is classified through its operational tool row as unknown.

After recovery the daemon calls `AgentService::auto_resume_interrupted_runs`.
It examines only top-level runs in `interrupted` or `failed`, reads the owning
project policy, and attempts at most one run per session per tick when
`auto_resume` is true. Auto resume is off by default and uses exactly the same
preflight as manual resume. A failed auto-resume attempt is logged and does not
retry in a loop.

### 5. Resume preconditions

A fresh top-level prompt is an explicit abandonment of older unresolved attempts
in the same session. Before the new run starts, older top-level `interrupted` or
`failed` runs that are not already cancelled have their failure kind changed to
`superseded` (their status is preserved and their events remain inspectable).
Resume preflight refuses a superseded run, so a later resume cannot duplicate
writes that the new prompt may have performed. Starting a stored-plan resume is
treated the same way: older unresolved top-level Leader runs are superseded by
the plan-resume action, because the plan task rows and Worker journals are the
authoritative continuation.

Manual `session.run.resume` always exists, but it is bounded and explicit. Before
creating a new attempt it requires all of the following:

1. **Session terminal:** `idle`, `failed`, or `cancelled`. Running, queued,
   paused, waiting-input, draft, imported, and landed sessions are refused.
2. **Run resumable:** `interrupted` or `failed`, not `completed` or `resumed`,
   and failure kind not `cancelled`.
3. **Top-level Leader run:** task-scoped runs are retried through the existing
   plan-task retry path, not ad hoc. Plan Workers remain journaled for
   inspection, but `session.run.resume` refuses them explicitly.
4. **Attempt cap:** `run.attempt < policy.max_attempts`.
5. **Age cap:** run start time is no more than `policy.max_age_hours` old.
6. **Provider/model stable:** the configured provider and the current role model
   exactly match the run's recorded provider/model. There is no hidden routing.
7. **Budget not exhausted:** session token and USD budgets are checked (unlimited
   sessions bypass this check only; capacity, approval, sandbox, Verifier, and
   merge gates are unchanged). Plan/department/project reservations still apply
   on every provider call. The wall-clock time budget is per attempt (the
   existing `SessionBudget` starts its clock at run start), so there is no
   persisted elapsed-time value to compare here.
8. **Plan unchanged:** the current plan revision equals the latest checkpoint's
   plan revision (or the run's start revision when no checkpoint exists). A plan
   edit after the last safe boundary requires an explicit plan restore or a new
   prompt.
9. **Worktree compatible:** let `recorded` be the last checkpoint's HEAD (or the
   run-start HEAD when no checkpoint exists) and `current` be the live HEAD.
   - `current == recorded` is compatible, even if the worktree is dirty; the
     runtime never resets or discards those changes and the journal note
     describes known writes.
   - Otherwise the worktree must be clean and `recorded` must be an ancestor of
     `current`, which accepts commits layered on the safe boundary.
   - If no HEAD was recorded at all (Git was unavailable when the run
     started), the worktree must be clean.
   - Any other divergence is refused. The runtime never resets user work.
10. **Journal readable:** every step, tool, and checkpoint query must succeed.
    A read/decode failure fails closed before a provider call or side effect.

If the operation has an `operation_id`, the resume RPC uses a
`session_operations` receipt with method `session.run.resume`. This is separate
from prompt admission: it replays the same accepted resume result, refuses a
pending id, and fails closed on a crash-unknown receipt.

### 6. Successor attempt and conversation reconstruction

A successful preflight creates a new run row with:

- `resumed_from_run_id = previous.run_id`;
- `resume_root_id = previous.resume_root_id` (or the previous run id for the
  first resume);
- `attempt = previous.attempt + 1`;
- the current budget snapshot, worktree HEAD, and plan revision;
- the same objective, agent role/name, and department, with `task_id = None`
  because only top-level runs are resumed.

The previous run becomes `resumed` and both `session.run.resumed` and
`session.run.started` commit atomically. The next user turn is a bounded journal
note (≤4,000 chars) listing:

- the previous objective, status, and attempt;
- the last checkpoint turn/event cursor/message cursor;
- completed write tools with short results and a statement that duplicates will
  be blocked;
- other completed tools with short results (never full outputs);
- unknown/interrupted tools with “verify state before assuming success”;
- the absence of tool records when none exist.

The conversation is reconstructed from durable `message.created` events plus the
compaction summary event, not from an in-memory vector. Persisted assistant
responses and tool results are therefore present exactly once. The resumed run
starts a **new** provider call after the journal note; it does not replay a
provider call whose response was already persisted.

A persisted assistant message may contain tool calls whose tool-result messages
never landed. Sending that history directly would violate provider protocol.
The resume path therefore walks every assistant tool-call block and fills each
missing result from the operational journal: a completed call gets its bounded
journaled result plus “not re-run”, a failed call gets its error, and an
unknown/missing call gets an explicit “Unknown outcome; verify state” result.
These synthetic results exist only in the provider request for this attempt;
they are not persisted as session messages and are rebuilt on every resume.

### 7. Duplicate-write hard block

`write_file`, `edit_file`, `apply_patch`, and `file_manage` are hard-blocked when
the same canonical input fingerprint already has a `completed` tool row in the
same `resume_root_id` lineage. The gate returns the journaled bounded result with
a note that the call was not re-run. The note also tells the model to inspect the
file if it needs current content.

Matching uses the operation fingerprint canonicalization: object keys are sorted
recursively, so wire-map order cannot change identity. The claim is a SQLite
compare-and-set (`consumed_by_run_id IS NULL`), making it consume-once:
two genuinely distinct identical fresh calls cannot both short-circuit on one
completed row. The second call executes normally, which preserves the ability to
perform two intentional identical writes.

If the successor attempt fails or is interrupted, its claims are released so the
next attempt can return the journaled result instead of re-running a write whose
replay result never reached the persisted conversation. A completed successor
keeps the claim; there is no further resume of a completed run.

Read-only tools always re-run and are never blocked. `shell`, `process`,
`local_http`, and `browser` are side-effecting/ambiguous: they are not
hard-blocked because the journal cannot prove whether the world changed, but an
interrupted or missing outcome is reported as unknown and the model must verify
state. Approval denials, skill denials, and policy denials are journaled as
failed tool outcomes; they are retryable only through the normal model path.

### 8. Crash windows and honest classification

| Crash point | Durable state | Resume behavior |
|---|---|---|
| Before checkpoint | Previous checkpoint remains | Resume from previous safe boundary; note says last provider turn |
| After checkpoint, before provider intent | New checkpoint, no model step | Resume from checkpoint |
| After provider response, before step completion | `model_request` intent | Classified retryable (no external side effect). If the assistant message was not persisted, the next provider call regenerates it; if it was persisted, event reconstruction already contains it |
| After tool side effect, before tool row commit | `tool_call` intent + `run_tool_calls` intent | Startup marks the tool row `unknown`; the note warns to verify; writes are not hard-blocked from an unknown row |
| After tool row completion, before message append | Completed tool row, no tool message | Resume returns the journaled result for a write instead of executing it; other tools are listed as completed |
| During approval/question wait | Approval/question step intent | Startup denies/expires the pending item; the step remains intent and the run is interrupted; resume note describes the journal state |
| After run completion event | Terminal run | Resume refused |

A provider stream that has produced text or tool calls is never replayed by the
provider adapter; a failed/stalled stream that produced no output is retried by
the existing provider retry policy. Mid-stream resume is not implemented.

### 9. Exactly-once limits

This design is intentionally precise about what it does **not** provide:

- **No provider-call exactly-once.** A provider call may be billed twice if the
  process dies after a response that was not persisted. The journal labels the
  affected model step `intent` and the ADR does not claim otherwise.
- **No mid-stream resume.** Streaming is restarted from the reconstructed
  conversation, not resumed at a byte/token offset.
- **Delegation is not hard-blocked.** A completed `delegate`/`scout` result is
  reported in the journal note, but only the four named file-write tools are
  blocked by fingerprint.
- **No exactly-once shell.** A shell/process/local_http call that was in flight
  is reported as unknown. The journal cannot prove whether it ran, so it does
  not fabricate a result or block a retry.
- **No full filesystem isolation.** The duplicate-write gate is a journal rule
  for four named file tools, layered on top of the existing path jail, Seatbelt,
  approval, and write-scope gates. It is not a transaction over arbitrary
  processes.
- **No distributed fencing.** `session_operations` and the journal use the local
  SQLite database. The data-dir instance lock remains the single-daemon
  guarantee; this is not a cross-host exactly-once protocol.
- **Tool outcomes are not replayable.** Losing `run_tool_calls` loses the
  duplicate-write gate and unknown classification for old runs. The runtime fails
  closed on an unreadable journal before resuming.
- **Imported/rehydrated archives are not resumable.** Bundles intentionally omit
  operational tool outcomes. A session carrying `session.imported` or
  `session.rehydrated` is refused by resume preflight even after rehydration, so
  the runtime never claims a duplicate-write guarantee it cannot support. Start a
  new prompt in the rehydrated session instead.

### 10. RPC surface

The daemon exposes:

- `session.runs { session_id, limit? }` — newest-first run list.
- `session.run.detail { run_id }` — run, steps, operational tool calls,
  checkpoints.
- `session.run.resume { session_id, run_id?, operation_id? }` — explicit manual
  resume; omitted run id selects the newest resumable top-level run.
- `session.run.cancel { session_id, run_id? }` — cooperative cancellation of an
  active run; the run loop writes the terminal journal row.
- `session.run.policy.get { project_id }`
- `session.run.policy.set { project_id, policy }` — validated 1..=3 attempts,
  1..=168 hours.

The CLI mirrors the read/control surface with `vp runs`, `vp run-detail`,
`vp run-resume`, `vp run-cancel`, `vp run-policy`, and `vp run-policy-set`.
All methods and flags are additive and backward compatible within protocol major
version 1.

## Consequences

### Positive

- A crash no longer forces a blind choice between replay and reset. Operators can
  inspect precise intent/outcome rows, and resume is bounded by policy, budget,
  plan, provider/model, and worktree safety.
- Completed file writes are not repeated within a resume lineage; unknown shell
  side effects are surfaced rather than guessed.
- Run/step/checkpoint projections replay, repair, export/import, and rebuild with
  the rest of the session.
- The journal is useful without resume: `session.runs` and
  `session.run.detail` provide an audit trail for every provider/tool boundary.

### Costs and limitations

- Every turn writes a checkpoint and step intent; every tool writes intent and
  outcome rows. This is local SQLite work at turn/tool boundaries, not per token.
- `max_attempts` defaults to 1, so manual resume is refused until an operator
  raises the policy. This is intentional: resume is opt-in, not automatic.
- Only top-level Leader runs are resumable through this RPC. Task-scoped runs
  are journaled but use plan-task retry, preserving the existing Verifier and
  merge gates.
- Compaction summarizer provider calls are represented by a compaction step, not
  an individual provider step.
- Project manifests carry the policy; an older manifest decodes with the safe
  defaults.

## Tests

The implementation covers, in `vp-core`:

- checkpoint/step/tool/finish round trip and detail;
- crash after a model response before completion (model intent retryable);
- crash after a tool result before a message/checkpoint;
- crash before a tool starts (tool intent becomes unknown);
- duplicate-write hard block, consume-once, plain-read re-run, and claim release
  after a failed attempt;
- attempt/age/status/plan/provider/model/budget/worktree/task-scope refusals;
- plan change, clean descendant acceptance, and divergent worktree refusal;
- compaction/approval/question steps;
- replay/repair, bundle export/import, and manifest/db rebuild;
- operation receipt replay;
- an explicit manual resume end-to-end with a deterministic fake provider,
  event-reconstructed conversation, cancel, and opt-in auto resume.

The tests use a scripted in-process provider and Git fixtures; no network calls
are made.
