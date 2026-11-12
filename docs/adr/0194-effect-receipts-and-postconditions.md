# ADR-0194: Effect receipts and postcondition probes

- **Status:** Accepted
- **Date:** 2026-11-12
- **Depends on:** ADR-0193 (task contracts and argument-level intent authorization)
- **Plan:** `docs/CONTRACTED_EXECUTION_PLAN.md` C2

## Context

The run journal blocks a duplicate completed *workspace write* by input
fingerprint, but it has no settled/unknown record for external effects. A
retry or resume after `git push`, forge PR/MR creation, a remote write, or a
remote bundle integration could repeat the action. A crash between "the call
started" and "the result was committed" had no honest state: the runtime could
only guess.

The C1 contract answers *may this call happen?*. C2 answers a different
question: *what does the runtime know happened?*. These are deliberately
separate. A contract is derived from trusted plan text before the call; an
effect receipt is local evidence written before and after it.

## Decision

### 1. Receipt lifecycle

`effect_receipts` stores one row per logical effect:

```
id, session_id, task_id, attempt, tool, target, request_hash, state,
provider_key, external_id, response_hash, evidence_json, created_at, resolved_at
```

`state` is one of:

| State | Meaning | Retry | Merge |
|---|---|---|---|
| `intent` | written before the call can change the world | no automatic retry | blocks |
| `done` | the local runtime observed the declared postcondition | replays stored evidence | allowed (existing gates still apply) |
| `failed` | the call reported a failure and the postcondition was not observed | new attempt row or explicit reconcile | allowed |
| `unknown` | the runtime cannot prove whether the effect happened | blocked until reconcile | blocks |

`done` is terminal. `unknown` requires `effect.reconcile`. `failed` may retry
only by inserting a **new attempt row**, not by overwriting the failed
evidence. A partial unique index on
`(session_id, COALESCE(task_id,''), tool, target, request_hash)` for
`intent|done|unknown` makes the same logical effect idempotent while leaving
failed attempts inspectable.

`target` is a secret-free canonical `kind:key` string derived from trusted
arguments, never from model prose or tool output. Examples:

- `workspace_write:src/lib.rs`
- `workspace_patch:src/a.rs,src/b.rs`
- `git_push:github:owner/repo:feature/x`
- `forge_pr:github/owner/repo:feature/x->main`
- `remote_write:out/app.txt`
- `http_request:POST:example.test:8080/api`
- `shell:cmd-<sha12>`

The request hash is a SHA-256 over canonical JSON of the tool, target, and
bounded argument identities (content hashes, not content; URL without
credentials/query; command hashes, not command text). It never contains a
provider key or secret.

### 2. Transaction boundaries

`begin_effect()` inserts `intent` in the **same SQLite transaction** as the
run-journal step/tool intent and appends `effect.intent`. When the call is a
tool call, the agent loop passes the intent into
`RunJournal::tool_intent(...)`; there is no window in which the journal says
"tool started" while the receipt says nothing.

`finish_effect()` (via `RunJournal::tool_completed_with_effect`) writes the
step completion, tool completion, `done|failed|unknown` receipt, `effect.*`
event, and `tool.finished` in one transaction. A crash before the commit
leaves an `intent` row and no false completion.

Runtime-owned effects (Git push, forge PR, landing) use `begin_effect` before
the action and `settle_effect` after it. A remote bundle integration records
the verified import/enqueue as `done` before the deterministic local merge, so
the receipt cannot block its own queue entry; the merge itself is covered by
the queue row and a `git_merge` receipt. Effects already
observed after the fact (Worker commit, deterministic Integrator merge) use
`record_settled_effect`, which inserts a terminal row and its event atomically.

### 3. Events and replay

New events, all with `{"receipt": <EffectReceipt>}` and no file content:

- `effect.intent`
- `effect.settled`
- `effect.failed`
- `effect.unknown`
- `effect.reconciled` (adds the bounded operator label)

The projection is event-backed:
`effect::receipts_from_events` is used by `session.replay`, `session.repair`,
`session.rebuild`, `db-rebuild`, bundle import, and `vp replay --repair`.
`SessionRepair.effects` replaces the rows; a malformed effect payload is
reported as drift rather than dropped. `SessionBundle` gains `effects`
(additive, `serde(default)`), while archived events remain authoritative.

### 4. Postcondition probe matrix

| Family | Probe | `done` evidence | Honest limit |
|---|---|---|---|
| `write_file` / `edit_file` | worktree file exists + SHA-256 vs tool-reported after hash | path + SHA-256 | a local file hash is local evidence; it does not prove who wrote it |
| `apply_patch` | every changed path exists/absent + `after_sha256` | up to 32 path rows | more than 32 changed paths settles `unknown` |
| `file_manage` | mkdir exists; delete absent; move source absent/destination present; copy hashes match | path rows | directories are observed by existence |
| `run_project_task` | `ok`/`exit_code` + bounded result hash | exit code + result hash | command completion is not proof of an external side effect |
| runtime Worker commit | `git rev-parse`/commit identity | commit + branch | local commit, not a push |
| runtime Integrator merge | session HEAD after final lease check | merge head + branch | local merge, not remote replication |
| `git push` | command completion; remote head must be reconciled if disputed | branch + commit | no `ls-remote` network probe by default; a timed-out push is `unknown` |
| forge PR/MR (`session.pr`) | provider response URL/change | URL + change + repository + head commit | provider response is not provider exactly-once |
| `remote_write` | helper post-write SHA-256 (remote stat/hash) | remote path + SHA-256 | helper evidence, not a second independent read |
| remote bundle integration | verified import + queue entry identity | host, branch, imported commit, queue entry | the local merge is tracked by the merge queue/`git_merge`, not by this receipt |
| `remote_exec` / `remote_git` / `remote_worktree` | none | — | always `unknown` |
| `local_http` mutating methods | HTTP status + body hash | status + body hash | response proves observation, not a remote side effect |
| `fetch_url` GET | optional (`track_read_http=false` by default) | status + body hash | a GET is a read; no receipt by default |
| raw `shell` | none unless `effect_probe` is declared | declared file hash/absence or Git head/branch | exit code alone is not proof; mutating shell defaults `unknown` |
| MCP `call` / dynamic MCP tools | none | — | mutating MCP defaults `unknown`; inspection creates no receipt |
| `deploy` / `webhook` | no current producer | — | modeled target kinds; no runtime invents a success receipt |
| `browser` / `computer` | GUI lease/action timeline | — | outside C2; no receipt claim |

Policy (`EffectPolicy`, project-scoped, `enabled` default true):

- `unknown_blocks_merge` default true.
- `stale_intent_grace_secs` default 120, bounded 1..=86 400.
- `shell_mode` default `external_only`: shell commands classified
  `workspace_write` stay covered by the Seatbelt worktree sandbox and the
  Verifier/Integrator diff; `external`+ shell is tracked. `all` tracks every
  non-read-only shell command.
- `track_mutating_http` default true; `track_read_http` default false.
- `track_mcp_mutations` default true.
- `track_workspace_writes` default true so a receipt can replay a completed
  write even when the run journal is gone.

### 5. Merge, landing, and retry gates

`enqueue_merge_with_source` calls `assert_effects_settled` before creating a
queue row. The check includes the task's rows **and** session-scoped rows, so
a partially failed PR or remote integration blocks every task merge in that
session. `process_merge_entry_fenced` re-checks immediately before merge under
the integration lease; if an effect became unresolved while the entry waited,
the entry is marked `failed` with `reason: "effect_unsettled"` and no branch
mutation occurs. `session.land` applies the same check.

The error is typed: `EffectBlocked` (message prefix `EFFECT_UNKNOWN`), which
callers can downcast. Tool dispatch returns `ToolErrorCode::EFFECT_UNKNOWN`
and never executes an unresolved effect. A `done` lookup replays stored
evidence (tool result or `EFFECT_REPLAYED` for runtime actions); it does not
re-run the action. `failed` permits a new attempt row.

A daemon restart leaves `intent` rows. A bounded sweep
(`VERLINPARK_EFFECT_SWEEP_SECS`, default 30, first tick immediate) marks rows
older than the project grace as `unknown` with
`reason: "stale_intent_after_restart"`. The sweep never retries. Operators can
trigger it with `effect.sweep` / `vp effect-sweep`, but the daemon does it
automatically.

### 6. Operator surfaces

RPC:

- `session.effects` (`SessionEffectsParams` → `SessionEffectsResult`) returns
  bounded receipts, counts, blocked state/reason, and policy.
- `effect.reconcile` (`EffectReconcileParams` → `EffectReconcileResult`)
  accepts only `done|failed`; a `done` decision requires a hash, commit,
  external id, HTTP status, or a note of at least eight characters.
- `project.effect_policy.get` / `project.effect_policy.set`.
- `effect.sweep` for manual bounded sweeps.

CLI: `vp effects`, `vp effect-reconcile`, `vp effect-policy`,
`vp effect-policy-set`, `vp effect-sweep`. Human output prints target labels,
states, attempts, and short anchors only.

Swift: `SessionEffectsReport`/`EffectReceiptView` decode additively;
`CoreClient.sessionEffects` and `reconcileEffect`; `AppModel.sessionEffects`;
a read-only `EffectBadge` in Stage and `EffectReceiptsSection` in Efficiency.
The GUI never renders file content, command text, or secrets and never
auto-reconciles.

## Consequences

Positive:

- A crash between "started" and "finished" is `unknown`, not a blind retry.
- `git push`/PR creation/remote writes are idempotent by request identity for
  the local runtime; a retry after `done` replays evidence.
- Merge cannot integrate work with an unresolved external effect.
- Replay/repair/db-rebuild/bundle import reconstruct receipts from events.
- Unknown is explicit and honest; the UI shows it.

Costs:

- Mutating shell and MCP calls can create `unknown` rows that block merge until
  an operator reconciles. This is deliberate. Projects that need observation
  first can set `unknown_blocks_merge=false`, which is visible in the policy.
- `git push` probes command completion rather than a network `ls-remote`, so an
  ambiguous network failure is `unknown`, not `done`. The operator must
  reconcile with the actual remote state.
- Remote writes rely on the helper's post-write hash; it is not a second
  independent read.
- Foreground Worker merges can fail with `EFFECT_UNKNOWN` after a partial
  external action. Reconcile, then retry; the commit is preserved.

## Non-goals

- No provider or remote exactly-once claim. A `done` receipt is local evidence.
- No distributed effect ledger across hosts; this is same-host SQLite.
- No deploy/webhook producer is invented; only the target kinds exist.
- No GUI auto-reconcile and no model-based reconciliation.
- No change to approval, Seatbelt, path jail, write scope, Verifier,
  Integrator, capacity, or budget gates.
- No attempt to receipt browser/computer GUI actions beyond the existing GUI
  lease/action timeline.
- No automatic retention/pruning policy for settled effect receipts yet. Queries
  are bounded; operational storage can grow with long sessions. A future ADR
  may add a retention sweep for `done`/`failed` rows without touching
  unresolved ones.
- `tool.started` is emitted by the dispatch branch. For a replayed or blocked
  effect no branch runs, so the runtime emits a start marker with
  `effect_blocked`; the durable intent boundary is the run-journal step, not
  the transcript marker.

## Verification

- Rust: effect classification/probe unit tests; store idempotency, attempt,
  reconcile, stale sweep, run-step transaction, and policy tests; Engine merge
  gate/reconcile/replay-repair tests; agent-level write-receipt replay test.
- Protocol: additive decode/default tests.
- CLI: parse and human-rendering tests.
- Swift: model decode and safe-default tests.
