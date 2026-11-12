# ADR-0198: Failure attribution and the error-cascade ledger

- **Status:** Accepted
- **Date:** 2026-11-13
- **Task:** C6 from `docs/CONTRACTED_EXECUTION_PLAN.md`
- **Related:** ADR-0193 (task contracts), ADR-0194 (effect receipts), ADR-0196 (runtime pinning), ADR-0205–0209 (project-scoped identities)

## Context

VerlinPark already records *what* happened: plan task lifecycle events, Worker
and Verifier results, merge queue outcomes, artifacts, contracts, effects,
workspace commits, and integration failures. When a session fails, operators
still have to read the event log backwards and guess which upstream claim was
the first unsupported one. The usual failure is not the last red step; it is an
earlier result that was accepted without evidence, an artifact that was never
cited, a check that predates the commit it supposedly covers, or a Worker and
Verifier that both accepted work integration later rejected.

C6 adds a deterministic, advisory diagnosis:

1. a bounded provenance DAG built only from existing events;
2. the earliest unsupported claim on the causal path to a failure;
3. an explicit cascade path from an upstream root to the target failure;
4. an optional event-backed ledger of explicitly recorded snapshots;
5. no automatic remediation of any kind.

The module is intentionally **not** a model judge. There is no summarizer, no
provider call, no hidden routing, and no prompt text in the result.

## Decision

### 1. Provenance graph

`vp-core::diagnosis` scans a bounded, newest session event window
(`500..=50_000`, default `20_000`) and builds a DAG from an allowlist of
structural fields.

Node kinds (stable strings):

| Kind | Source | Stored fields |
|---|---|---|
| `task` | `team_plan.submitted` / `team_plan.revised` / `plan_task.*` | task id, deps, status, plan seq |
| `agent` | `agent.started` / `agent.spawned` / `agent.finished` | agent id/name/role, task, status |
| `identity` | only when an event payload itself carries `identity_id` | identity id |
| `artifact` | `artifact.published` | artifact id, SHA-256, task, agent, version |
| `commit` | Worker `agent.finished`, merge queue entries | commit hash, task, agent, changed paths |
| `tool_call` | `tool.started` / `tool.finished` | tool name, task, agent, success/error code |
| `test_evidence` | `run_project_task` tool result, `test_cache.stored` | task, agent, pass/fail, inputs hash |
| `verification` | Verifier `agent.finished` | approved/rejected, agent, task, paired commit |
| `merge` | `merge_queue.enqueued` / `merge_queue.updated` | entry id, status, attempt, commit |
| `workspace` | `agent.workspace.*` when present in the window | workspace id, status, commit |
| `effect` | `effect.intent/settled/failed/unknown/reconciled` | receipt id, tool, state |
| `contract` | `contract.derived` | contract id/hash, task, role |
| `failure` | deterministic symptom scan | kind, status, task, agent, reason code |

Edges are `depends_on`, `produced`, `used`, `verified`, `merged`, and `failed`.
`depends_on` points from a downstream task to its upstream dependency. A
`used` edge is added only when a Worker cited an upstream hash in its result or
read an upstream artifact by id; a missing citation is a finding, never a
synthetic edge.

The graph never contains prompts, message bodies, tool arguments, tool output,
file contents, command text, raw URLs, provider keys, or approval tokens. The
only content-derived operation is a bounded scan for 12–64 character hexadecimal
hash tokens in a Worker/Verifier result summary; the token is matched against
upstream evidence and the surrounding prose is discarded.

Bounds are hard:

- `MAX_GRAPH_NODES = 2_000`, `MAX_GRAPH_EDGES = 4_000`;
- dependency depth is limited to `MAX_GRAPH_DEPTH = 64` upstream levels from
  the target task; when that depth is exceeded, the graph is focused on the
  target's upstream chain, other branches are dropped, and `truncated` /
  `depth_limited` are set;
- evidence is capped per node/finding and the report carries
  `MAX_FINDINGS = 16`, `MAX_CASCADE_STEPS = 24`, `MAX_RESPONSIBLE_AGENTS = 16`;
- the source event window is bounded and the report exposes
  `scanned_events`, `total_events`, `window_start_seq`, `window_end_seq`, and
  `truncated`.

Nodes, edges, and evidence are sorted deterministically. `graph_summary.digest`
is the SHA-256 of canonical `{session_id, nodes, edges}`; the same window and
events produce the same digest.

### 2. Failure symptoms

A failure symptom is one of:

- task `failed` / `blocked` / `interrupted`;
- Verifier `approved = false`;
- merge queue `conflict` / `failed` / `interrupted`;
- remote integration `failed` / `rejected`;
- a Worker `failed` / `changes_requested` result when no task-level event
  exists.

The target is selected explicitly by `target_task_id`, otherwise the latest
failure symptom in the window wins. Secondary failures are reported only as a
bounded count; siblings are not copied into the attribution or cascade.

### 3. Earliest unsupported claim

The deterministic checks run in this order and produce candidate findings with
an event sequence. The root is the earliest candidate by sequence, with fixed
tie-breaking by reason rank and finding id.

1. **Cascade upstream.** Walk task dependencies from the target. A failed,
   blocked, or interrupted ancestor becomes `cascade_upstream`. The report
   uses the ancestor failure, not every downstream red step.
2. **Missing upstream evidence.** A completed ancestor with no commit and no
   artifact hash becomes `missing_evidence`. A missing link is a finding.
3. **Unsubstantiated handoff.** The target has dependencies with evidence, but
   the Worker result cites none of the upstream commit/artifact hashes and did
   not read an upstream artifact by id. A cited hash that does not match any
   upstream evidence is included as a bounded evidence pointer with the note
   `hash did not match an upstream result`.
4. **Stale evidence.** If a task has at least two distinct commits and its last
   passing `run_project_task` check or Verifier verdict covers the previous
   commit but nothing covers the latest commit, the report emits
   `stale_evidence` at the latest commit. A Verifier verdict paired to a commit
   that differs from the latest task commit is also stale.
5. **False consensus.** For a merge/integration failure, if a Worker
   `run_project_task` check passed and a Verifier approved the same task before
   the failure, both agents are flagged with `false_consensus`. The Worker
   finding is the root candidate; the Verifier finding is a `flag`. A Verifier
   approval without a recorded Worker check does not invent a consensus claim;
   the absence remains a missing-evidence signal, not a fabricated edge.
6. **Missing task result evidence.** A task that reports completion without a
   commit or artifact hash contributes `missing_evidence`.
7. **Symptom fallback.** If no earlier claim is evidenced, the symptom itself is
   the root with its specific `verifier_rejection`, `merge_conflict`,
   `integration_failure`, or `task_failure` reason, plus a
   `missing_evidence` finding that says the bounded window contains no earlier
   unsupported claim.

Every finding carries `finding_id`, `reason_code`, `severity`, `node_id`,
`task_id`, `at_seq`, `at`, fixed summary text, responsible agents, bounded
evidence pointers, and flags. `earliest` is the root finding. `responsible_agents`
deduplicates agents from all findings by id/name/role.

### 4. Cascade path

If the root node belongs to an ancestor task, the module searches the task
dependency DAG from the root to the target and returns only that path (bounded
to 24 steps). Intermediate steps are `task_dependency`; the first is `root`,
the last is `failure`. Sibling failures are never included. If the root is the
target itself, the cascade contains a single root step.

### 5. Persistence and ledger

The default `session.diagnosis` path is read-only. It does not append events,
write a table, retry, cancel, merge, reassign, block, or change budgets,
capacity, approvals, or plans.

An operator may call the separate `session.diagnosis.record` RPC (CLI
`vp diagnose --record`). The plain `session.diagnosis` RPC has no record flag
and is always read-only. Recording appends one bounded `failure.attribution`
event containing a `FailureAttributionRecord` (target, earliest node/sequence,
reason, responsible agents, capped findings/cascade, graph digest/counts,
advisory flag). The event is the ledger row.

There is deliberately **no new projection table**. The graph and report are
derived from the immutable event stream, and the ledger is a bounded event
kind. This avoids a second source of truth, avoids replay/rebuild drift, and
keeps the feature read-only by construction. `session.replay` ignores the
advisory event, so recording a snapshot does not create projection drift.
`session.attributions` / `vp diagnose --history` parse the newest
`failure.attribution` events with a hard cap.

### 6. RPC, CLI, and UI

- RPC: `session.diagnosis` (`session_id`, optional `target_task_id`,
  `limit`, `include_graph`) returns `SessionDiagnosisResult` and never writes.
- RPC: `session.diagnosis.record` takes the same fields and appends the one
  bounded ledger event.
- RPC: `session.attributions` (`session_id`, optional `limit`) returns the
  bounded ledger.
- CLI: `vp diagnose <session> [--task <id>] [--limit N] [--graph] [--record]`
  and `vp diagnose <session> --history`.
- Swift: `SessionDiagnosisReport`, `SessionAttributionsReport`, and provenance
  models decode the same JSON. `FailureDiagnosisBadge` is shown in the Stage
  header when a root exists; `FailureDiagnosisSection` is shown in the
  Efficiency inspector. Both are read-only and have no action controls.
- Typed Q3 errors: `DIAGNOSIS_SESSION_NOT_FOUND`, `DIAGNOSIS_INVALID_WINDOW`,
  `DIAGNOSIS_RECORD_INVALID`, with `{code,message,retryable,hint,advisory}`.

### 7. Advisory boundaries (hard)

The diagnosis may not:

- retry, skip, cancel, pause, resume, or reassign a task/session;
- submit a Verifier verdict or enqueue/merge a commit;
- approve or deny an approval, gate, or effect;
- change a budget, capacity, priority, write scope, contract, or plan;
- call a model or provider;
- write prompt/tool/file content into an event, report, CLI output, or UI;
- synthesize an edge or claim when evidence is missing.

`advisory: true` is always set and is not configurable.

## Evidence rules

1. Use only allowlisted event fields. Ignore prompts, `input`, `output`,
   `summary` bodies, command text, and file bytes.
2. Hash citations are the only exception: extract 12–64 hex tokens from a
   result summary and keep only tokens that match known evidence as `used`;
   unmatched tokens are bounded evidence pointers, not text.
3. A missing event, hash, commit, artifact, or Verifier pairing is a finding.
4. A claim is attributed only when a deterministic rule names it and at least
   one event evidence pointer exists.
5. Assignments are shown as context, not proof that a historical run used the
   current identity. Identity nodes appear only when the event itself carries
   `identity_id`.
6. Changed paths come from the additive `changed_paths` field on Worker
   `agent.finished` events. They are paths, not file content.
7. The graph is bounded before serialization; truncation is explicit.

## Tests

- Upstream failed task is blamed before a downstream failure.
- Missing/unmatched handoff hash becomes `unsubstantiated_handoff`; a matching
  hash or an upstream artifact read is accepted as evidence.
- Stale test evidence before the latest commit becomes `stale_evidence`.
- Worker + Verifier acceptance followed by integration failure flags both as
  `false_consensus`.
- Completed upstream with no artifact/commit becomes `missing_evidence`.
- Cascade path contains root → target and excludes a failing sibling.
- Graph node/edge/depth caps set `truncated`/`depth_limited`.
- Prompt, tool input/output, and summary secrets do not appear in the report or
  ledger record.
- Deterministic digest/findings across repeated runs.
- Recording a diagnosis appends exactly one `failure.attribution` event, leaves
  session status, merge queue, and task scheduling unchanged, and adds no
  replay drift.
- Protocol decode/defaults, daemon parameter/error shape, CLI parse/render, and
  Swift model decode.

## Limits / non-goals

- The event window is bounded; older failures are not attributed and this is
  surfaced as `truncated` plus a warning.
- Attribution is evidence linkage, not semantic correctness. `false_consensus`
  means two local claims and a later integration failure, not proof that a test
  was meaningless.
- Shell tests run through raw `shell` are not classified as test evidence
  because tool arguments/output are intentionally unread. Only
  `run_project_task` and `test_cache.stored` metadata count.
- A Worker result with no hash citation and no artifact read is a finding even
  when the upstream evidence is valid; the feature does not guess which
  upstream result was used.
- Historical identity is not inferred. Current assignments are context only.
- The ledger is event-only; there is no projection table, no retention sweep,
  and no cross-session or cross-project attribution.
- No automatic retry, escalation, plan rewrite, LLM diagnosis, provider call,
  or merge gate. C6 only emits evidence.
- Graph visualization is a client concern; this ADR adds a bounded inspector
  card and badge, not a graph editor.
- This does not implement S6 hiring/retirement or C7+.

## Consequences

- Operators get a stable answer to “where did the error enter?” without reading
  a full event log.
- The result is reproducible and secret-free, so it can be recorded, exported,
  and compared by digest.
- Missing evidence is visible instead of hidden behind the last failing step.
- The feature cannot remediate anything, so it cannot weaken a gate or create a
  retry storm.
- The bounded window means very old failures need a larger explicit limit or a
  separate archived-log workflow in a future ADR.
