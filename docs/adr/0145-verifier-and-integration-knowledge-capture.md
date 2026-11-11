# ADR-0145: Verifier and integration knowledge capture

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Agent Orchestration
- **Related:** ADR-0074, ADR-0078, ADR-0142

## Context

ADR-0142 made project memory a claim with provenance and recorded completed
task results as `task_result` memories. Two of the most consequential outcomes
in a session were still invisible to later runs:

- the Verifier's verdict on a specific commit, including whether the approved
  commit actually integrated; and
- a successful conflict resolution after rebase, re-verification, and requeue.

Those facts already exist as events and merge-queue rows, but agents cannot
recall them through the normal brief. A later Worker asked to retry a task, or a
Manager planning around a conflicted branch, has to rediscover what happened.
The gap is not a need for model-written summaries. It is missing durable
provenance.

Capture must not become a second gate. A memory write failure, a policy lookup
failure, or a slow database must never turn an approved merge into a rejection,
a rejected verdict into a success, or a requeued conflict into a failure.

## Decision

### Capture Verifier verdicts

`finish_worker` records one `verification` memory for approved and rejected
verdicts, after the integration attempt for approvals so the body reflects the
real merge result:

| Field | Value |
|---|---|
| kind | `verification` |
| source | `verifier` |
| title | `Task {task_id} verification` when the Worker has a task id; otherwise `Worker {agent_name} verification {first 8 commit chars}` |
| body | `approved: true\|false`, `verdict: {summary}`, `integration: {detail}` (approvals only), `commit: {commit}` |
| evidence | `task` (when present), `session`, `commit` |
| tags | task id (when present), `verification` |
| TTL | project `MemoryPolicy.default_verify_days`, fallback 365 days |
| lifecycle | dedupe and auto-supersede enabled |

A rejected verdict writes `approved: false` and no `integration` line. It never
contains a success or merged claim. An approved verdict whose integration
attempt failed writes `integration: integration failed: ...`, not `merged`.

### Capture successful conflict resolutions

The explicit `merge_queue.resolve` path and the daemon auto-resolver both go
through `resolve_merge_inner`. After re-verification approves the rebased commit
and the queue entry is successfully requeued, the runtime records:

| Field | Value |
|---|---|
| kind | `integration` |
| source | `orchestrator` |
| title | `Integration {entry_id} resolution` |
| body | `resolved: true`, `verifier: {summary}`, `commit: {commit}`, bounded `conflict:` and `outcome:` detail |
| evidence | `session`, `commit`, `task` (when the queue entry has one) |
| tags | task id (when present), `integration` |
| TTL | project `MemoryPolicy.default_verify_days`, fallback 365 days |
| lifecycle | dedupe and auto-supersede enabled |

The memory is written only after the resolution and requeue succeeded. The
`outcome` line records the later deterministic Integrator result honestly: a
merge failure is described as an integration failure, never as a successful
merge. Paths, the resolver summary, and the outcome are bounded before writing.

### Best-effort capture

Both helpers use `Engine::create_memory_with_metadata` and the existing project
policy lookup. There is no parallel memory write path. Any failure is logged as
a warning; the Worker, Verifier, merge queue, Integrator, approval, sandbox,
write-scope, capacity, and budget results are unchanged.

Identical repeated capture is idempotent because the store's
`(project, kind, title)` dedupe key sees the same body, evidence, and source.
Changed verdicts, re-verification attempts, or changed integration outcomes
supersede the previous active row; event history keeps the prior claim.

### Deterministic ranking

`vp-core::memory` adds two kind weights and one source weight:

| Signal | Weight |
|---|---|
| kind `verification` | 0.24 |
| kind `integration` | 0.22 |
| source `verifier` | 0.18 |

These weights sit below operator decisions/conventions/handoffs and above a
generic fact. Exact task evidence remains the dominant structural signal
(0.60), so a same-task verification is selected in a brief for that task, while
an unrelated verification cannot outrank a same-task result. Ranking stays
lexical/structural/behavioural; no provider call is added.

## Consequences

- Later runs can recall that a task was verified, what the Verifier concluded,
  and whether the commit integrated, without replaying the transcript.
- A successful conflict resolution becomes inspectable project knowledge with
  the exact commit and queue entry, while failed resolutions are not recorded
  as successes.
- Re-verification and retries replace the active claim instead of accumulating
  contradictory active memories.
- Briefs stay bounded and deterministic; the new weights change ordering, not
  the selection or usage mechanism.
- Capture adds one small SQLite transaction per verdict and per successful
  resolution. A failed write is visible in logs and is not retried in the
  foreground.

## What is NOT claimed

- **No semantic truth.** The memory repeats the Verifier's verdict; it does not
  prove the verdict correct.
- **No LLM summarization.** Bodies are deterministic fields and bounded text
  from the existing verdict, resolver, and Integrator details.
- **No failure diagnosis beyond the Verifier's own summary.** Rejections and
  errors are not classified by a model.
- **No cross-project memory.** Capture and recall remain project-scoped.
- **No new RPC or agent tool.** Capture is internal to the runtime; agents read
  these memories through the existing brief and `recall` surfaces.
- **No gate change.** Memory capture never approves, rejects, merges, retries,
  or cancels anything.

## Alternatives considered

- **Reconstruct from events at recall time:** avoids a second projection, but
  makes every brief depend on bespoke event scanning and loses the shared
  dedupe, TTL, usage, and supersede lifecycle.
- **Only capture approvals:** cheaper, but erases the most useful negative
  knowledge (why a commit was rejected) and hides failed integrations.
- **LLM-written verdict summaries:** richer prose, but adds hidden routing,
  cost, and an unauditable claim layer on top of an already explicit verdict.
- **Record resolutions before requeue:** could capture intent, but would claim
  success for a conflict that never re-entered the deterministic Integrator.
