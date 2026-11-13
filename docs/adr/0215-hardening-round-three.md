# 0215 — Hardening round three: memory erasure, identity provenance, and contract/effect/gate accuracy

- Status: Accepted
- Date: 2026-11-13
- Scope: remaining S5/S6/S8, C1/C2/C3/C6, and migration hardening

## Context

ADR-0213 and ADR-0214 fixed the implementation defects and bounded replay gaps
from the first production-review rounds. A third review focused on the remaining
places where the runtime could still over-claim or leak:

- S5 `forget --prune` removed only the projection row; earlier events retained
  the memory content, so replay or a bundle export could still expose it.
- Leader/Manager/Worker/Labour run events had no identity id/generation,
  forcing S6 attribution and OTel projection to infer provenance from mutable
  rows.
- A plan scope of `src/**` was rejected by contract derivation even though the
  runtime's own path checks accept it, so a valid plan could be escalated as
  out-of-contract on every write. URLs with embedded credentials were not
  distinguished from their effective host.
- A malformed path/patch/URL argument made effect classification abort the run
  before the tool could return the normal typed error; the effects report could
  also hide an old unresolved receipt behind its 500-row display cap.
- Gate payloads accepted credential-like keys, the CLI required the one-time
  token on a process-visible command line, and gate/effect failures surfaced as
  generic internal errors.
- C6 diagnosis treated any hex-looking token in a summary as cited evidence and
  labeled task failures with a synthetic event kind.
- S8 bundle import and `db-rebuild` recomputed template hashes but did not run
  the portable shape validator.
- No test reopened a pre-hardening database to prove additive migrations.

## Changes

### S5 logical memory erasure

- `agent.memory.erase` (CLI `vp agent-memory-erase --yes`) requires explicit
  `approved: true`, an optional matching project guard, and rejects imported
  rows. It deletes the local projection row and every prior event payload for
  that memory id in one transaction, then appends one content-free
  `agent.memory.erased` tombstone carrying only the old content hash, scope,
  identity id, bounded reason, timestamp, and `approval_risk=host_state`.
- Replay treats the tombstone as terminal and removes the row. A repair cannot
  recreate it from prior events because those events no longer exist.
- Bundle validation accepts the content-free tombstone, rejects a tombstone that
  embeds a full memory snapshot or `content`, caps its payload, and keeps the
  existing hash validation for every other memory event.
- The CLI/RPC surface and tests cover approval refusal, imported-row refusal,
  removal of prior event content, export/import, replay/repair, and the
  no-resurrection path.

This is logical erasure from the live SQLite rows. It is **not** a secure wipe
of SQLite free pages, WAL segments, filesystem snapshots, or previously exported
backups.

### Runtime identity provenance

- `AgentSpec` carries `identity_generation` alongside `identity_id`. The
  top-level start and plan-resume paths resolve the effective identity before
  emitting their first event, persist the run binding, and use that effective
  spec for all run/tool events. Leader/Manager/Worker/Labour child specs carry
  the generation resolved for their workspace or parent.
- `agent_fields` adds `identity_id` and `identity_generation` to emitted
  payloads. Cache plan and warm results now derive the generation from the
  identity marker instead of always reporting `null`.
- This is observational provenance. It does not grant authority, change model
  routing, or widen any approval/sandbox/Verifier/merge gate.

### C1 contract scope and URL accuracy

- Plan write scopes are normalized like runtime path checks: `src/**` becomes
  the `src` subtree, `*`/`**` mean an unrestricted task scope, and a wildcard
  scope cannot silently coexist with narrower allowed paths.
- URLs with embedded username/password (for example
  `http://user:pass@example.com/`) escalate because the contract cannot verify
  the effective host from the userinfo form.
- A committed adversarial corpus covers credential URLs, `file:`/loopback
  URLs, absolute writes, shell substitution/redirection, `git push`, and
  pipe-to-shell forms. The corpus is bounded syntactic coverage, not a proof
  that every shell/URL form is recognized.

### C2 effect dispatch robustness

- An effect-classification error (invalid path, patch, or URL argument) becomes
  a recoverable `INVALID_ARGUMENT` tool result for that call instead of aborting
  the whole run before the normal tool error contract can run. The tool still
  does not execute and no effect intent is created.
- `effect_receipts(state, created_at)` indexes the stale sweep.
- When `session.effects` truncates its display window, it separately counts
  unresolved receipts outside the window and fails the `blocked` report closed
  when any exist.

### C3 gate payload and token hardening

- Gate payload validation rejects credential-like keys at any depth using the
  shared `json_has_sensitive_key` shape check. This is a key-name heuristic,
  not a content/secret classifier.
- `vp gate resolve` accepts `--token-stdin` or a `--token-file` that must not be
  group/world accessible on Unix; `--token` remains for compatibility but warns.
  The generated resolve hint uses the stdin form.
- Gate, effect, and `session.land` RPC failures preserve typed codes and
  bounded structured data instead of collapsing to `INTERNAL_ERROR`.

### C6 diagnosis evidence accuracy

- Cited hash tokens survive only when they match a known artifact, commit,
  merge, verification, or test-evidence hash in the scanned window. Arbitrary
  hex-looking text pasted into a summary no longer becomes evidence.
- Task evidence reports the actual event kind (`plan_task.finished`,
  `blocked`, `cancelled`, `interrupted`, or `started`) instead of always
  claiming `plan_task.finished`.

### S8 and S6 hardening

- The Engine bundle-import path and `db-rebuild` run the portable
  `TeamTemplate` shape validator before persisting a template. A self-consistent
  hash no longer makes an absolute path, URL, secret, or malformed slot
  importable.
- S6 performance evidence includes only active assignments or assignments inside
  the configured lookback window; older ended assignments cannot keep an
  identity's performance ledger artificially active.

### Migration fixture

- A store test reopens a synthetic pre-hardening database after removing
  `agents.identity_id`, `sessions.plan_runtime_snapshot`,
  `project_team_templates.origin`, `contracts`, and `effect_receipts`. Opening
  the store restores the additive columns/tables and the `agent_runs` view.

## Remaining explicit gaps

- S5 erasure is logical, not cryptographic. SQLite free pages/WAL, filesystem
  snapshots, and prior backups may retain old bytes; there is still no event
  retention/compaction or backup erasure policy.
- Path/URL/shell classification remains bounded and syntactic. It is not a
  sandbox or a complete parser.
- Gate payload screening is key-name based; it cannot recognize a secret in an
  innocently named field.
- C6 filtering can under-report citations when the evidence lies outside the
  bounded event window; it never turns an unknown hash into evidence. Diagnosis
  remains advisory and does not remediate.
- S8 shape validation covers the Engine import and rebuild paths; a direct
  low-level store snapshot still depends on its own hash validation.
- S6 assignment cutoff is a lookback heuristic, not a complete historical
  performance store.
- No coverage tooling, full old-schema fixture corpus, or exhaustive
  adversarial parser corpus is committed.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: 3,902 passed, 0 failed
- Swift was not changed in this round; the existing app/helper suites remain
  the validation baseline.

## Related

- ADR-0193 (task contracts and intent authorization)
- ADR-0194 (effect receipts)
- ADR-0195 (external gates)
- ADR-0198 (failure attribution)
- ADR-0209 (identity memory)
- ADR-0210 (hiring, retirement, performance)
- ADR-0212 (portable team templates)
- ADR-0213 and ADR-0214 (previous hardening rounds)
