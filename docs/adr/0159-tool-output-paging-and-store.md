# ADR-0159: Tool-output paging and the daemon-owned output store

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Store, Protocol, CLI, macOS
- **Related:** ADR-0153 (cache telemetry), ADR-0154 (efficiency dashboard),
  ADR-0155 (durable run journal), ADR-0160 (compaction budget), AGENTS.md
  invariants 1, 2, 4, 5, and 6

## Context

The runtime already bounded tool results before they entered the provider
request. Seven original tools built their text through `truncate`, which kept
the first 40,000 bytes and appended a short marker. Later tooling used similar
local caps. That protected the provider from one runaway build log, but it had
four operational problems:

1. The discarded tail was gone. A model could not ask for bytes 40,001–80,000
   of a compiler log even though the daemon had produced them.
2. The truncated text was still large enough to dominate a small context
   window. A 40 KB grep result is roughly ten thousand tokens that remain in
   the conversation for the rest of the run.
3. The audit event was capped separately at 32 KiB structured / 40 KB text, so
   the persisted event and the provider text disagreed about what was retained.
4. There was no way for an operator to see how much output pressure a project
   was creating, no retention policy, and no safe cleanup.

C1 therefore needs a durable, daemon-owned reference for the full result and a
bounded, model-readable preview. The reference must not become a new filesystem
capability. An agent receives an opaque handle, never a path, and the daemon
resolves that handle against its own metadata.

The design also has to preserve the security and replay story:

- no new write path outside the agent worktree;
- no hidden model routing;
- full output is operational state, not replayable session state;
- content is already agent-visible, but secrets must not appear in logs or
  event payloads;
- a missing or corrupt store must fail *open to a bounded preview*, not fail
  the agent run;
- a bundle import must not claim to resolve handles that were never exported.

## Decision

### 1. Project policy

`ToolOutputPolicy` is project-scoped and persisted in SQLite
(`project_tool_output_policies.policy` JSON) plus the project manifest snapshot.
The defaults are deliberately conservative:

| Field | Default | Bounds | Meaning |
|---|---:|---|---|
| `enabled` | `true` | — | Spill and page oversized results |
| `spill_threshold_bytes` | 16 KiB | 256 B – 16 MiB | Full result size that triggers a spill |
| `preview_bytes` | 4 KiB | 64 B – 1 MiB, ≤ threshold | Provider-facing preview body |
| `max_total_bytes_per_turn` | 64 KiB | 1 KiB – 64 MiB, ≥ preview | Aggregate preview budget for one assistant turn |
| `retention_secs` | 7 days | 60 s – 365 days | Handle lifetime |
| `max_entries` | 2,000 | 1 – 1,000,000 | Per-project row cap after GC |
| `tool_thresholds` | `{}` | names non-empty; values 64 B – 64 MiB | Optional per-tool threshold overrides |

Recognized override names are `read_file`, `read_files`, `grep`, `shell`,
`run_project_task`, `git`, `remote_exec`, `code_diagnostics`, and `browser`.
Unknown names are retained for forward compatibility but do not affect a tool
until a call with that name appears. The store name is not a schema enum, so a
new tool can opt in without a protocol change.

Validation lives in `vp_core::tool_output::validate_tool_output_policy` and is
called by the Engine before every write. Store reads are tolerant: a stored
policy that predates a field decodes with serde defaults. A future client may
send unknown JSON fields; the protocol ignores them.

The policy is additive. Older daemons ignore the RPC, older clients ignore the
fields, and a project without a row gets the default policy rather than an
error.

### 2. `ToolOutput` metadata

`ToolOutput` keeps its existing `content`, `is_error`, and `structured` fields.
It gains five public fields and one internal field:

| Field | Purpose |
|---|---|
| `output_ref: Option<String>` | Opaque `out_<uuid>` handle |
| `truncated: bool` | Content is a bounded preview, not the full result |
| `total_bytes: u64` | Exact full result size before shaping |
| `preview_bytes: u64` | Bytes retained in the preview body |
| `lines: u64` | Logical lines in the full result |
| `full_content: Option<String>` | Pre-truncation text for tools that bound internally; never serialized, journaled, or provided to a model |

The constructors `ok`, `error`, `ok_structured`, and `error_structured` remain
source-compatible and fill the metadata from the content. `ToolOutput` is not a
wire type; it is an in-process result. Events and run-journal rows copy only
selected metadata.

Tools that previously truncated internally can call
`ok_structured_preserving(full, structured, is_error)`. It stores the exact
pre-truncation text in `full_content` and keeps the legacy constructor output
bounded. The shaper consumes `full_content` first, so the store receives the
larger body when one exists. The field is cleared before the output enters the
conversation or event stream.

### 3. Content-addressed storage

Bodies live under `<data_dir>/tool-outputs/<sha256>.txt`. The data directory is
the same private daemon directory that holds SQLite, session mirrors, and
artifacts. There is no per-project directory and no user path input.

Write path:

1. hash the UTF-8 body with SHA-256;
2. reject bodies above the hard store cap (32 MiB);
3. refuse a `tool-outputs` directory that is a symlink;
4. create the directory with mode 0700 if needed;
5. if the hash path exists, require a regular file, read it, and verify its hash
   before accepting it;
6. otherwise create a unique `.tmp` file with `create_new` and mode 0600, write,
   `sync_all`, rename within the same directory, and enforce 0600 again;
7. remove the temporary file on any error.

The write is idempotent across sessions and projects: identical content shares
one blob. A later prune removes a blob only when its last metadata row is gone.
Temporary files left by a crash are ignored by normal reads and by the audit;
they are not valid handles.

Read path:

1. require an `out_<uuid>` handle shape;
2. look up the metadata row by primary key;
3. compare `session_id`; a mismatch is reported as an unknown handle, not as an
   existence oracle;
4. resolve the hash to a path using only 64 lowercase hex characters;
5. require `symlink_metadata` to report a regular, non-symlink file;
6. require the on-disk size to equal metadata `bytes`;
7. read the body and verify its SHA-256 before returning text.

A missing, symlinked, size-mismatched, or hash-mismatched body is an error for
the paging tool. It is never silently replaced with different content. A failed
store write during shaping is different: the tool result is still real, so the
shaper degrades to the legacy bounded preview and records a `store_error`
notice. The run continues.

The metadata row is:

```text
tool_outputs(
  id, project_id, session_id, tool_call_id, tool_name,
  sha256, bytes, lines, created_at, expires_at, preview
)
```

`preview` is the bounded body that was shown to the model. It is stored so an
operator can debug a preview without reading the full body; RPC list calls do
not return it. Rows cascade when their session is deleted, but blobs are
content-addressed and are cleaned by reference-counted prune/audit rather than
by a foreign-key action.

### 4. Deterministic shaping

Shaping happens in the agent runtime before journal completion, event
persistence, and message insertion. Both the parallel tool batch and the
sequential tool path call the same helper. That ordering is important:

- a `tool.finished` event and the conversation contain the same bounded text;
- the run journal records the shaped result, not an unbounded body;
- a replay of events cannot reconstruct a different provider context than the
  live run.

The per-tool threshold is `tool_thresholds[tool]`, else
`spill_threshold_bytes`. The comparison is strictly greater than, so an output
exactly at 16 KiB remains inline. Empty output never spills.

For a batch:

1. compute each result's exact byte size and preview cap;
2. mark every result above its per-tool threshold as a forced spill;
3. compute retained bytes as `prior turn bytes + full sizes of non-spilled
   results + preview caps of spilled results`;
4. while retained bytes exceed `max_total_bytes_per_turn`, spill the largest
   remaining result; ties use the earlier tool-call index;
5. allocate preview bodies in tool-call order from the remaining turn budget.

Step 4 makes aggregate pressure deterministic. Step 5 deliberately does not
continue largest-first: once size pressure is handled, preview space is shared
fairly in the order the model asked for tools. The marker and headers are
metadata and are not included in `preview_bytes`, so a turn can exceed the body
budget by the small marker overhead. The default is roughly 100 bytes per
spilled result.

The total `ShapeReport.provider_bytes` is added to the turn's prior bytes. A
spilled result consumes only its preview. Once the budget is exhausted, later
results in the same turn are spilled with a zero-byte preview and a valid
handle. No result is dropped.

### 5. Preview rendering

Head-only tools retain the beginning. `shell`, `run_project_task`,
`remote_exec`, `code_diagnostics`, `browser`, and `computer` use head+tail,
because a build log or diagnostic report usually puts the failure at the end.
The separator `\n…\n` is included inside the preview budget. Cuts are moved to
UTF-8 scalar boundaries:

- prefix cuts move backwards to a boundary;
- suffix cuts move forward to a boundary;
- a budget below 16 bytes uses head-only so a separator cannot consume the
  whole preview.

The provider text ends with an explicit marker:

```text
[output truncated: showing <preview_bytes> of <total_bytes> bytes (<lines> lines); use read_tool_output handle=<out_...>]
```

If spilling is disabled or the store fails, the fallback marker has no handle:

```text
[output truncated: showing <n> of <m> bytes; request a bounded section with the tool's offset/limit parameters]
```

The fallback is still capped at the legacy 40 KB. A policy change or a full disk
can therefore never produce an unbounded provider request.

### 6. `read_tool_output`

`read_tool_output` is an agent tool, not an RPC that returns arbitrary content.
Its schema is:

```json
{
  "handle": "out_...",
  "offset": 0,
  "limit": 200,
  "max_bytes": 32768,
  "from_end": false,
  "pattern": "literal substring"
}
```

Semantics:

- `offset` is zero-based in the filtered line set. With `from_end`, offset is
  zero-based from the end, and returned lines remain chronological.
- `limit` defaults to 200 and is clamped to 2,000.
- `max_bytes` defaults to 32 KiB and is clamped to 256 B – 64 KiB. The daemon
  cap cannot be raised by an argument.
- `pattern` is a literal, case-sensitive substring, capped at 500 bytes. It is
  not a regular expression.
- The result is line-numbered, never splits a UTF-8 scalar, and includes a
  continuation hint with the next offset when more matching lines remain.
- Empty output, no pattern matches, and an offset past the end are successful
  reads with explanatory text, not errors.
- An empty or foreign handle is an error with no body disclosure.

The role matrix is read-only: Leader, Manager, Verifier, Scout, and every
Worker/Labour profile may call it. It is in the parallel-safe read-only set, so
several pages can be fetched in one assistant turn. It has no write, shell,
network, delegation, or merge effect.

Handles are validated against metadata only. The tool has no path parameter, no
way to enumerate handles, and no way to read another session's handle. Session
ownership is the authorization boundary; a future cross-session sharing feature
would need a separate ADR.

### 7. Events and privacy

Shaping emits two operational events, in addition to the normal
`tool.finished`:

`tool.output_spilled`:

```json
{
  "tool_call_id": "...",
  "name": "shell",
  "handle": "out_...",
  "sha256": "...",
  "bytes": 210000,
  "returned_bytes": 4300,
  "lines": 5000,
  "returned_lines": 120,
  "preview_bytes": 4096,
  "reason": "threshold",
  "error": null
}
```

`tool.output_truncated` has the same metadata and is emitted whenever content
was reduced, including the policy-disabled and store-error paths. `reason` is
one of `threshold`, `turn_budget`, `spilled`, `policy_disabled`, or
`store_error`. `error` contains a store error string, never content.

`tool.finished` gains top-level `output_ref`, `truncated`, `total_bytes`,
`preview_bytes`, and `lines`. The shaped structured result also carries those
keys under `output_ref`/`truncated`/etc. No event contains the full body. The
`sha256` is a content identifier, not a secret.

The preview stored in the metadata table and the preview embedded in
`tool.finished` are both already visible to the agent that produced them. They
are not copied to logs by the store, the sweep, the CLI, or the audit.

### 8. Operational-state boundaries

Full tool output is **operational state**, not a session replay projection.
The consequences are explicit:

- `session.replay` and `session.replay {repair}` do not compare tool-output
  rows. Dropping every row is a valid operational cleanup, not drift.
- `session.export` does not copy bodies or the metadata table. An imported
  session may contain a `tool.output_spilled` event and a preview marker, but
  the handle cannot resolve. Import deletes any stale rows for the imported
  session id before inserting the archive. The archive is read-only and this is
  the intended behavior; an operator who needs the body must keep the source
  data directory or republish the content as an artifact.
- `session.import` never follows a handle from the bundle and never accepts a
  path from the bundle.
- `db-rebuild` rebuilds sessions, events, and replay projections from the
  manifest and mirrors. It does not reconstruct bodies. If the existing SQLite
  database survives, rows and blobs are preserved because they are not part of
  the rebuild's projection tables. A rebuild from a clean database loses the
  handles; old event markers remain honest about the loss.
- `vp backup` includes `artifact` blobs only when requested and never includes
  `tool-outputs`. A backup restore therefore does not promise handle
  resolution.
- A checkpoint snapshot is a worktree/conversation snapshot, not a tool-output
  store snapshot. Forking a session copies messages and previews, not handles.

No code treats a missing row as a reason to re-run a tool. The paging tool
returns a bounded error and the model can re-run the original tool or ask for a
different page.

### 9. Retention, prune, and audit

Retention is enforced by three paths:

1. **Opportunistic cap enforcement** after a spill batch. It prunes expired or
   excess rows for the project, but active-session unexpired handles are
   protected. The project may temporarily exceed `max_entries` if every
   candidate belongs to a live run.
2. **Daemon sweep** (`VERLINPARK_TOOL_OUTPUT_SWEEP_SECS`, default 3,600; 0
   disables) over every project, using that project's `retention_secs` and
   `max_entries`.
3. **Manual RPC/CLI** `tool_output.prune` / `vp tool-output-prune`, with an
   optional project/session filter, age override, entry override, and dry run.

Prune ordering is oldest first by `(created_at, id)`. A row is a candidate when:

- `expires_at <= now`, regardless of session status; or
- it is older than the resolved age cutoff and its session is not active; or
- it is beyond the newest `max_entries` rows and its session is not active.

Active means `queued`, `running`, `paused`, or `waiting_input`. The sweep never
deletes an unexpired handle belonging to one of those sessions. Expiry is the
only exception; a handle whose expiry has passed is eligible even if a stale
session row still says running. This prevents a crashed session from pinning
storage forever.

The prune result reports `candidates`, `removed`, `removed_bytes`,
`expired_removed`, `overflow_removed`, `protected_active`, and `dry_run`. A dry
run never deletes rows or blobs. After a real prune, each affected hash is
checked for remaining references; the blob is removed only at reference count
zero. The blob removal requires `symlink_metadata` to report a regular file and
never follows a symlink.

`tool_output.audit` / `vp tool-output-audit` verifies the boundary:

- selected metadata rows;
- missing blobs;
- size/hash mismatches (hashing is opt-in because it is O(bytes));
- unsafe blob files (symlinks or non-files);
- orphan `*.txt` files with a valid hash name but no metadata row.

Audit never deletes metadata. `clean_orphans` removes only unreferenced,
hash-shaped regular files older than a five-minute grace window, and is
therefore safe to run while sessions are active. The grace window exists
because a concurrent spill writes its blob milliseconds before inserting the
metadata row; deleting a fresh unreferenced file would open a missing-blob
window. It is not a substitute for prune: metadata rows remain and continue to
make a missing blob visible.

### 10. Worked shaping example

Assume the default policy (16 KiB threshold, 4 KiB preview, 64 KiB turn
budget), no prior tool bytes, and one assistant turn with four calls:

| Call | Tool | Full bytes | Forced spill? |
|---|---|---:|---|
| A | `read_file` | 40,000 | yes |
| B | `grep` | 30,000 | yes |
| C | `shell` | 10,000 | no |
| D | `git` | 9,000 | no |

Step 1 marks A and B as forced. Retained bytes are `4,096 (A preview) +
4,096 (B preview) + 10,000 + 9,000 = 27,192`, below 64 KiB, so no aggregate
spill is needed. A and B get handles. C and D remain inline. The provider sees
the two previews plus 19,000 bytes of full content, about 31 KiB including
markers. The full 70,000 bytes remain retrievable.

Now assume the turn budget is 12 KiB instead. Retained is `4,096 + 4,096 +
10,000 + 9,000 = 27,192 > 12,288`. The largest non-spilled result is C
(10,000), so it spills. Retained becomes `4,096 + 4,096 + 4,096 + 9,000 =
21,288`, still over. D (9,000) spills. Retained becomes `4,096 × 4 = 16,384`,
still over. There are no remaining candidates, so previews are allocated in
call order from the 12,288-byte body budget: A 4,096, B 4,096, C 4,096, D 0.
Each result still has a valid handle and a marker. The model can page D even
though its inline preview is empty.

This example is deliberately pathological. It shows the two invariants that
matter: the full bodies are retained, and the provider text is bounded and
explainable even when the configured budget cannot hold one preview per result.

### 11. Handle lifecycle

| State | Row | Blob | Readable? | Prunable? |
|---|---|---|---|---|
| Spilled | yes | yes | yes, until expiry | only overflow/age with inactive session |
| Expired, not swept | yes | yes | no (`unknown handle`) | yes, even active session |
| Pruned | no | maybe, until refcount 0 | no | n/a |
| Orphan (write-before-row or lost row) | no | yes | no | audit after grace window |
| Missing (row without blob) | yes | no | read error | prune can remove the row |
| Corrupt (size/hash mismatch) | yes | yes | read error | prune can remove the row |

A handle is therefore a capability to a row, not a guarantee that the body is
intact. The daemon never substitutes a different body or re-runs the original
tool to repair a missing one.

### 12. File lifecycle and crash windows

```text
create tool-outputs/ (0700)
        │
        ├── write .<sha>.<uuid>.tmp (create_new, 0600)
        │       │
        │       ├── crash ──▶ ignored temp file; no handle exists
        │       │
        │       └── fsync + rename to <sha>.txt (0600)
        │               │
        │               ├── crash ──▶ orphan blob; audit after grace
        │               │
        │               └── insert metadata row ──▶ readable handle
        │
        └── prune row ──▶ refcount check ──▶ remove blob at zero
```

The ordering is intentional. Writing the row first would make readers observe a
handle before its body exists. Writing the body first leaves a small orphan
window, which is harmless because the audit has a five-minute grace period and
because no handle points at the file until the row commits.

A crash after the row commits but before the `tool.output_spilled` event leaves
a readable handle and a missing audit event. This is acceptable operational
state: the transcript contains the marker and handle, and the event stream is
less complete than the store. No replay projection depends on either. A later
`session.replay` remains consistent.

### 13. RPC, CLI, and client surface

Additive RPCs:

- `tool_output.policy.get` / `tool_output.policy.set`
- `tool_output.status` (metadata counts/bytes, no bodies)
- `tool_output.list` (metadata only, bounded)
- `tool_output.prune`
- `tool_output.audit`

CLI:

- `vp tool-output-policy <project>`
- `vp tool-output-policy-set <project> [--disable|--enable] [--spill-threshold ...] [--tool-threshold shell=...]`
- `vp tool-outputs [--project ...] [--session ...]`
- `vp tool-output-status`
- `vp tool-output-prune [--dry-run]`
- `vp tool-output-audit [--verify-hashes] [--clean-orphans]`

The macOS Manage Dash efficiency card shows stored handle count, stored bytes,
active handles, last compaction latency and token delta, and prepared-summary
reuse/fallback. The Stage context popover shows the latest compaction latency
and whether it reused a prepared summary. Both decode the new fields
additively; older daemons simply leave them nil. Views do not read the blob
directory or derive policy semantics.

### 14. Failure modes

| Failure | Behavior |
|---|---|
| Policy disabled | Legacy 40 KB bounded preview; no handle; `tool.output_truncated` with `policy_disabled` |
| Body > 32 MiB | Store rejects; legacy preview; `store_error` notice; run continues |
| Disk full on blob write | Store rejects; legacy preview; run continues |
| Existing blob hash mismatch | Store rejects rather than overwrite; legacy preview |
| `tool-outputs` is a symlink | Store refuses; legacy preview |
| Blob missing at page time | Paging error; no alternate content |
| Blob size/hash mismatch at page time | Paging error; no alternate content |
| Foreign/expired handle | Paging error `unknown tool output handle`; no disclosure |
| Malformed handle / path-like input | `invalid tool output handle`; no filesystem access |
| Pattern too long | Input error before store read |
| More lines than `max_bytes` | Bounded prefix with an elision marker |
| Prune during an active run | Unexpired active-session rows are protected |
| Daemon crash after write, before row insert | Orphan blob; audit can clean it |
| Daemon crash after row insert, before event | Handle exists; event is missing but operational state is still readable |
| Bundle import | Handles are not restored; import deletes stale rows for the id |
| Replay/repair | Tool-output rows are ignored; no replay drift claim |

### 14.1 Operator runbook

**A page says "unknown tool output handle".** Check, in order: (1) the handle
belongs to the same session; (2) the row still exists (`vp tool-outputs`); (3)
the row is not expired; (4) the blob exists and matches (`vp tool-output-audit
--verify-hashes`). A foreign, expired, missing, or malformed handle intentionally
returns the same bounded error.

**Disk usage is growing.** Run `vp tool-output-status --project <id>` to see
rows and bytes, then `vp tool-outputs --project <id>` for the largest or newest
handles. `vp tool-output-prune --project <id> --dry-run` shows what the policy
would remove. A real prune protects active-session rows; use `--older-than-secs`
or `--max-entries` to override the policy for one pass. `vp tool-output-audit
--clean-orphans` removes unreferenced files after the grace window; it never
deletes metadata.

**A build log should stay inline.** Raise `spill_threshold_bytes` or set a
per-tool override (`vp tool-output-policy-set <project> --tool-threshold
shell=131072`). Raising `preview_bytes` increases the context cost; raising
`max_total_bytes_per_turn` allows more previews per turn.

**A turn is too large.** Lower `max_total_bytes_per_turn` or `preview_bytes`.
The largest results spill first; smaller results remain inline until the turn
budget is met. The marker tells the model exactly which handle to page.

**An operator wants evidence.** `tool_output.audit` reports missing, unsafe,
mismatched, and orphan blobs without returning content. It is safe to run while
the daemon is live. Do not copy the blob directory into a bug report; it
contains agent-visible repository data.

### 14.2 Alternatives considered

**Raise the 40 KB cap.** Rejected: it makes the original context-pollution
problem worse and still loses the tail eventually.

**Truncate head+tail without a store.** This is better than head-only for build
logs, but the model can never ask for the missing middle. The task explicitly
needs paging, so the store is part of the contract.

**Store the full body in an event.** Rejected: events are replayed, exported,
searched, and mirrored; putting megabytes in an event would multiply storage,
slow replay, and blur the operational/replay boundary. Events carry metadata
only.

**Expose a file path.** Rejected: it would give the agent a new filesystem
capability outside the worktree and make retention/symlink safety impossible to
reason about. Opaque handles preserve the existing path jail.

**Use one row per unique content hash.** Rejected for authorization and
lifecycle: a shared row would need a separate reference table to express which
sessions may read it, and pruning one session's reference would have to edit the
shared row. One row per handle plus a shared blob is simpler and makes session
ownership explicit.

**Keep previews in SQLite only.** Rejected: the preview is an operator/debug
convenience, but the body must be available to the paging tool. Storing bodies
in SQLite would also make the database grow with build output.

**Compress blobs.** Deferred: the current store uses plain `.txt` for
inspectability and because the daemon's local disk is not the bottleneck yet.
Compression would require a format marker, a read/verify path, and a migration;
it is a possible future change, not part of this contract.

### 15. Limits and non-goals

- The store is local to one daemon data directory and does not replicate.
- There is no cross-session handle sharing, signed URL, or remote store.
- There is no archive extraction or media transcoding.
- The store is not encrypted at rest beyond the daemon's private directory
  permissions; it contains agent-visible tool output, not a credential vault.
- The audit does not repair a corrupted blob from events. Events deliberately do
  not contain the body.
- The 40 KB legacy cap remains for tools that have not opted into
  `full_content`; the store still preserves the exact output that the tool
  produced, but it cannot recover bytes a tool discarded before returning.
- `max_total_bytes_per_turn` bounds preview bodies, not marker overhead. A turn
  with hundreds of spilled results can exceed the configured byte count by the
  marker text; this is explicit and observable in `tool.finished`.
- The paging tool is read-only; it does not edit or delete handles. Operators
  use prune/audit.

## Consequences

Positive:

- A model can inspect a large build log without retaining all of it in context.
- Large results become retrievable rather than silently lost.
- The provider, event, journal, and message representations agree on what was
  retained.
- Retention is bounded, observable, and safe around active sessions.
- Handles are opaque and path-free, preserving the worktree write boundary.

Negative:

- A second storage area needs monitoring and cleanup.
- The store adds a filesystem dependency to the tool path; the fallback is
  important and must remain tested.
- Imported archives contain preview markers that cannot be paged. This is a
  deliberate operational-state boundary, not a bug.
- The policy has many bounds. They are centralized in one validator so RPC,
  CLI, manifest restore, and direct store writes cannot disagree.

## Verification

- `cargo test -p vp-core tool_output` covers exact thresholds, UTF-8, empty
  output, aggregate largest-first spilling, handle/foreign validation,
  offset/limit/from-end/pattern paging, byte caps, symlink escape, GC active
  protection, and policy validation.
- `cargo test -p vp-store tool_output` covers policy round-trip, content
  addressing, mode 0600, dedup/refcount cleanup, prune dry-run, expiry, active
  protection, audit missing/mismatch/orphan, and path rejection.
- `cargo test -p vp-core spills_large_tool_results_and_pages_them_with_read_tool_output`
  exercises the runtime shaping path, event metadata, paging, and foreign
  handle denial.
- `cargo test --workspace`, `cargo clippy --workspace --all-targets -- -D
  warnings`, and `cargo fmt --all -- --check` remain the merge gate.
