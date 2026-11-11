# ADR-0164: Durable context checkpoints and restore

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime / context
- **Related:** ADR-0160 (compaction budget and prepared summaries), ADR-0155
  (durable run journal), ADR-0147 (session continuations), AGENTS.md
  invariants 1, 2, 4, and 7.

## Context

Compaction is intentionally one-way at the active-context boundary:
`context.compacted` records a summary and a through-message cursor, and
`conversation_from_events`/`load_conversation` reconstruct only the latest
boundary. Original `message.created` rows remain in SQLite for search, audit,
and transcript, but there was no durable, inspectable snapshot of the exact
(mode, summary, cursor, plan revision, provider/model/prefix, worktree head)
tuple that produced the active context. An operator could not return to an
earlier boundary after a bad summary, a plan change, or a long pause, and an
automatic compaction could not be audited independently of the raw event.

Worktree checkpoints (`session.checkpoint`) snapshot commits and conversation,
but restoring one resets the worktree branch. They are not a context-only
operation and must not be reused for this purpose.

## Decision

Add a first-class **context checkpoint**:

- The public `ContextCheckpoint` shape is `id`, `session_id`, `project_id`,
  `label`, `mode`, `summary`, `through_message_id`, `message_cursor`,
  `plan_revision`, `provider`, `model`, `prompt_prefix_hash`, `worktree_head`,
  `tokens_before`, `tokens_after`, `structured_state`, `source_compaction_seq`,
  `automatic`, and `created_at`. The summary and structured state are bounded
  (128 KiB and 16 KiB respectively) and contain no prompt text beyond the
  summary already present in `context.compacted`.
- A `context_checkpoints` SQLite table is an **event-backed projection**. The
  full row is carried by `context.checkpointed`, and the restore/deletion
  events also embed the checkpoint row for audit; deletion tombstones the
  projection and replay/repair rebuilds it from the create/delete events. The
  event is inserted before the projection row in the same
  transaction. Session bundles carry the rows and events, imported archives
  make them read-only, and `db-rebuild` reconstructs them from per-session
  event mirrors.
- Automatic compaction writes the compaction boundary and its checkpoint in
  one transaction: `context.compacted` (with an additive
  `context_checkpoint_id`) first, then `context.checkpointed`, then the row.
  A manual `session.context.checkpoint` captures the current active boundary
  without running a prompt and without creating a second compaction. When the
  session has no compaction, it records a full-history `manual` boundary;
  restoring that boundary rewinds to the beginning.

## Restore semantics

`session.context.restore` is **context-only**:

1. It appends a reconstructed `context.compacted` event from the checkpoint
   (mode, summary, `through_message_id`, structured state, token estimates,
   `restored_from_checkpoint`, and `context_checkpoint_id`).
2. It appends a `context.restored` audit event.
3. It never writes or deletes `message.created` rows, never resets the
   worktree, and never creates a task, run, merge-queue entry, approval, or
   provider call.

Because the ordinary loaders use the latest `context.compacted` event, the
active conversation afterwards is `[summary] + messages after the cursor`.
Original messages remain searchable and auditable. Restoring the same
checkpoint twice deterministically produces the same active context, appends
two restore events, and creates no duplicate message rows.

The restore preflight is deterministic and fails closed on:

- a session that is not `idle`, `failed`, or `cancelled`;
- an imported archive or a session carrying `session.imported` /
  `session.rehydrated`;
- a plan-revision change since the checkpoint;
- a provider or model change since the checkpoint;
- a missing worktree, a worktree with uncommitted changes, a missing
  checkpoint head, or a current head that is not equal to/descended from the
  checkpoint head;
- an exhausted session token, cost, or time budget;
- a restored active-context estimate above the model's hard limit.

An estimate at or above the compaction threshold but below the hard limit is
an advisory **warning**. `force=true` bypasses only that advisory. It never
bypasses model/plan/worktree/import/budget checks or the hard limit. The
restore result records whether force was used and the warning text.

## Retention

`ContextCheckpointPolicy` is project-scoped: sweep enabled (default false),
`max_checkpoints` (default 20, bounds 1–100), and `max_age_days` (default 30,
0 disables the age arm, maximum 365). Manual prune is oldest-first and may be
a dry run. The latest checkpoint of an active run (queued/running/paused/
waiting_input) is always protected, even when `keep_latest` is zero. Delete and
prune only remove projection rows and append deletion events; no messages,
worktree commits, branches, or artifacts are touched. The daemon sweep uses the
existing checkpoint sweeper interval and only runs for projects that opt in.

## What this is not

- **Not lossless compaction.** The model summary is advisory; the structured
  task state is authoritative but bounded. Restore recovers a boundary, not
  bit-exact prompt engineering, hidden provider state, or omitted tool output
  bodies.
- **Not run resume.** It does not resume an interrupted provider stream, replay
  tools, restore run-journal state, or create an agent run. Durable run resume
  remains ADR-0155 and is explicit.
- **Not a worktree reset.** Worktree checkpoints and forks remain the only
  operations that move commits/branches.
- **Not a distributed guarantee.** The row/event transaction is same-host
  SQLite. It does not provide cross-host consensus or exactly-once provider
  calls.
- **Not semantic fact checking.** The summary and structured state are
  recovered as recorded; citing messages remain searchable but are not
  re-validated.

## Alternatives considered

- **Store only the latest boundary on the session row.** Rejected: it is not
  inspectable, cannot be replayed, and makes a second automatic compaction
  overwrite the evidence needed to recover from the first.
- **Reuse worktree checkpoints.** Rejected: `session.checkpoint.restore`
  resets the session branch and is explicitly a worktree operation. Mixing the
  two would make a context recovery capable of discarding code.
- **Delete compacted-away messages so loaders are simple.** Rejected: search,
  audit, citations, and transcript export depend on the original rows.
- **Rewrite the event log on restore.** Rejected: the log is append-only.
  Restore appends a new boundary and a new audit event; the old events remain
  available for forensics.
- **Mutate the checkpoint row into an `active` flag.** Rejected: rows are
  event-backed projections. Activity is derived from the latest compaction
  event, so a replay cannot disagree with a boolean.
- **Allow `force` to bypass model or worktree checks.** Rejected: those checks
  detect a boundary that cannot be reproduced safely. Force only accepts a
  token-estimate advisory; the hard limit remains absolute.
- **Use an LLM to regenerate the summary on restore.** Rejected: it would
  create hidden model routing, extra spend, and non-deterministic recovery.
  Restore is a deterministic event append.

## Rollout and compatibility

- The table is created with `CREATE TABLE IF NOT EXISTS`; existing databases
  gain it on open without a destructive migration.
- `ContextCheckpoint` fields, `SessionContextStatus` checkpoint counters, the
  `SessionBundle` field, and the replay report field are additive and
  `serde(default)` where decoded.
- Older sessions have no checkpoint events and list as empty. Older clients
  ignore unknown fields.
- The daemon sweep is off unless a project enables `ContextCheckpointPolicy`.
  `VERLINPARK_CHECKPOINT_SWEEP_SECS` controls the shared worktree/context
  checkpoint sweep interval; setting it to zero disables both sweeps.
- Operator flow: inspect with `vp context-checkpoints <session>`, capture with
  `vp context-checkpoint <session>`, preview with
  `vp context-checkpoint-detail`, then restore with
  `vp context-restore <session> <checkpoint>`. The Stage context popover and
  Manage Dash context card use the same preflight and never restore silently.

## Operational runbook

1. `vp context-checkpoints <session>` shows the boundary list, token deltas,
   plan revisions, and the current retention policy.
2. `vp context-checkpoint-detail <session> <id>` prints the preflight errors
   and warnings. A `warning:` line means the estimate is above the compaction
   threshold; the hard limit is still enforced.
3. `vp context-restore <session> <id>` refuses on any `error:` line. If only a
   warning is present, `--force` is required and is recorded on the
   `context.restored` event.
4. `vp context-checkpoint-delete` removes one row, and
   `vp context-checkpoint-prune --dry-run` previews oldest-first retention.
5. `vp context-checkpoint-policy-set <project> --enabled --max-checkpoints N
   --max-age-days D` opts the project into the daemon sweep. Disable it with
   `--enabled` omitted.
6. To audit a recovery, compare `context.restored` and `context.checkpointed`
   event payloads with `vp events <session>`; the checkpoint id, compaction
   sequence, forced flag, and warning are all recorded.

## Consequences

- Operators can inspect token deltas, plan revision, provider/model/prefix
  hashes, and worktree identity for every compaction boundary, then rewind
  context without touching code.
- A bad automatic summary can be superseded by restoring an earlier checkpoint
  or by taking a fresh manual checkpoint after a new compaction.
- Replay now reports `context_checkpoints_match`; repair/rebuild, bundle
  export/import, and `db-rebuild` cover the projection. Existing compaction,
  context, replay, and run-journal behavior is unchanged.
- Protocol additions are `serde(default)`; older clients decode new fields as
  absent, and older sessions simply have no checkpoints. Restore is an
  explicit RPC/CLI/UI action, never silent.
