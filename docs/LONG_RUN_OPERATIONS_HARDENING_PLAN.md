# Long-Run Operations and Hardening Plan

> **Status:** active implementation plan.
> **Created:** 2026-11-12
> **Base:** `c344bf9` (scaling/process/source/remote/budget round complete).
> **Goal:** make multi-hour/multi-day sessions operationally cheaper and safer:
> approval attention control, test-result reuse, precise preemption, fair
> multi-path budget allocation, and authenticated/rotatable backups.
> **Budget:** one large round, target +30,000 net lines of production code and
> tests. No filler; every module must operate, fail closed, and be auditable.

---

## 0. Issue checklist

### Approval attention

- [ ] Every similar permission request prompts again; there is no project
      template or bounded auto-approval rule beyond exact file/folder grants.
- [ ] Identical pending approvals are separate rows, so one operator decision
      cannot release several waiting agents.
- [ ] No batch review surface; Manage Dash resolves one approval at a time.
- [ ] No approval queue priority, deduplication window, or stale-expiry policy.
- [ ] Notification noise is not coalesced per session/tool/risk.

### Test results

- [ ] `run_project_task kind=test` always executes, even when inputs, command,
      environment, and tool version are unchanged.
- [ ] `affected_tests` is lexical and has no historical mapping from changed
      paths to actually failing/covering tests.
- [ ] No cache key, invalidation, retention, stats, or replay provenance.
- [ ] Integrator/test flows cannot distinguish a fresh run from a cached replay.

### Preemption

- [ ] Wall-clock and tool deadlines are checked at turn/tool boundaries; a long
      provider stream, shell command, HTTP call, browser/computer RPC, or LSP
      request can overrun them.
- [ ] Cancellation does not interrupt the provider HTTP stream; it waits for the
      next boundary.
- [ ] Hard preemption has no typed result/event, no partial-usage accounting, and
      no process-group cleanup guarantee.
- [ ] No distinction between cooperative pause and hard deadline preemption.

### Budget fairness

- [ ] The governor protects one heaviest chain only; parallel branches can
      starve or consume the reserve.
- [ ] Weights use declared task budgets, never observed spend.
- [ ] No per-department minimum, multi-path allocation, or starvation aging.
- [ ] Deferred tasks are reconsidered, but ordering can still be dominated by a
      single path.

### Backup authenticity

- [ ] v2 backups are encrypted but not signed; manifest tampering is detected by
      AEAD only if the attacker lacks the key, and there is no key rotation.
- [ ] Restore cannot prove the repository generation is the newest one seen.
- [ ] No keyring, key id, rotation command, backup verify command, or rollback
      rejection.
- [ ] v1 ZIP remains unauthenticated.

### Deferred backlog (unchanged)

- [ ] Cross-host scheduler/fencing, distributed lease consensus.
- [ ] Host-reboot process persistence, automatic command restart.
- [ ] Source ANN/rerank/query expansion.
- [ ] Artifact archive extraction/media transcoding, Bitbucket adapter.

This round implements H1–H5 below. Deferred items are not claimed as shipped.

---

## H1 — Approval and attention optimizer

### Design

- Project `ApprovalTemplate` (persisted SQLite + manifest + db-rebuild):
  - `id`, `name`, `enabled`, `tool_pattern` (exact/prefix/wildcard subset),
    `risk_levels` (External / WorkspaceWrite only by default),
    `path_globs`, `action` (`allow|deny|escalate`), `expires_at`,
    `max_uses`, `used_count`, `created_by`, `created_at`.
  - Deterministic precedence: explicit deny > explicit template deny >
    exact approval grant > template allow > risk policy. Templates cannot
    auto-approve Destructive, HostState, remote-exec, or sensitive GUI targets;
    unknown risk fails closed.
  - Every match is audited with template id, scope, and remaining uses.
- Approval deduplication:
  - Identical pending requests (session+agent+tool+canonical input scope+risk)
    coalesce into one approval row with bounded waiter references.
  - One operator decision resolves all current waiters; a later identical
    request after resolution is a new approval unless a template/ grant covers it.
  - No automatic approval is created by dedup; it only reduces repeated prompts.
- Attention policy (`AttentionPolicy`, project-scoped):
  - `enabled`, `max_pending_per_session`, `coalesce_window_secs`,
    `stale_after_secs`, `priority_boost_risk`, `notify_coalesce_secs`.
  - Pending approvals are ordered deterministically by risk, blocked-task
    criticality, age, then id; stale rows are marked `expired` (not silently
    approved).
  - Notification coalescing emits one inbox item per session/tool/risk window
    while preserving every approval row.
- Batch resolution:
  - RPC `approval.batch.resolve` with an explicit per-id decision list
    (no blanket "approve all unknown"); partial failures are reported per id.
  - CLI `vp approvals`, `vp approval-batch`, `vp approval-template*`.
  - Manage Dash multi-select with per-item decision and template creation.
- Events: `approval.created`, `approval.resolved`, `approval.expired`,
  `approval.coalesced`, `approval.template.*`; replay/repair/bundle.
- Tests: precedence matrix, risk denial, use/expiry caps, dedup/resolve/all,
  stale expiry, batch partial failure, notification coalescing, replay,
  protocol/CLI/Swift decode. Existing approval/grant tests stay green.

### Files

`vp-core/src/permissions.rs`, `approvals.rs` (new), store, protocol, daemon,
CLI, Swift Manage Dash, ADR-0177.

---

## H2 — Test-result cache and impact map

### Design

- `TestCachePolicy` (project): `enabled` (default true for explicit tool runs,
  never for the Integrator command unless `include_integration` is true),
  `max_entries`, `retention_days`, `max_output_bytes`, `reuse_lint`.
- Cache key (content-addressed, versioned):
  - command + resolved argv + working directory + normalized environment subset
    + project profile hash + tool/runtime version + input hash.
  - Input hash covers the changed/relevant files' content, the dependency
    closure, and the test-impact map generation; commit alone is not enough.
- `test_results` row: id, project, session, kind (`test|lint|check`),
  command, inputs_hash, status (`passed|failed|error`), exit_code, duration_ms,
  output_ref, covered_paths, impact_generation, created_at, expires_at,
  hit_count, last_hit_at.
  - Output bodies use the existing spilled-output store; no unbounded inline.
- `TestImpactMap`:
  - deterministic changed-path → test-target mapping from (a) project profile
    conventions, (b) lexical import/reference graph, (c) optional LSP references
    when already available, (d) historical cache rows that covered a path;
  - bounded targets, explicit `source: profile|lexical|lsp|history` and
    confidence; no claim of completeness; no test execution during mapping.
- `run_project_task`:
  - `cache: auto|bypass|refresh` (default auto);
  - on hit, return the stored bounded result with `cache_hit: true`,
    `cached_at`, `inputs_hash`, `impact_generation`, and a clear note that no
    command ran; on miss/refresh, execute and persist;
  - failed results are cached only with a short TTL and are never reported as
    passes; flaky/manual bypass is explicit.
- `affected_tests` gains `mode: lexical|impact` and labels its source.
- Cache stats/prune/clear RPC/CLI/Swift; events for store/hit/invalidate/prune;
  replay rebuilds metadata, output bodies remain operational.
- Tests: key stability, env/command/config invalidation, hit/miss/refresh,
  failed-result TTL, impact mapping sources/bounds, cache-hit provenance,
  retention/prune, protocol/CLI/Swift decode. Existing project-target tests
  stay green.

### Files

`vp-core/src/test_cache.rs` (new), `tooling/project.rs`, `tooling/code.rs`,
store, protocol, daemon, CLI, Swift Preview/Stage, ADR-0178.

---

## H3 — Precise preemption and deadlines

### Design

- Add a bounded `RunDeadline` / `PreemptToken` propagated to:
  - provider stream reads (abort HTTP/SSE between chunks, record partial
    output/usage honestly, never resume mid-stream);
  - shell/process execution (kill the process group, preserve bounded output);
  - HTTP/fetch/local_http (abort the request);
  - browser/computer RPC (cancel request, release lease safely);
  - LSP/MCP calls (best-effort `$/cancelRequest` / protocol cancel).
- Semantics:
  - cooperative pause remains the default operator action;
  - hard preemption triggers only on exhausted wall-clock budget, explicit
    `session.preempt`, or a tool deadline; it is not a hidden scheduler;
  - typed results/events `PREEMPTED` / `DEADLINE_EXCEEDED` with reason, phase
    (`provider|tool|queue`), elapsed, and whether side effects are unknown;
  - partial provider output is persisted as partial, never replayed; unknown
    tool side effects fail closed on resume.
- Project `PreemptionPolicy`: `enabled` (default true for wall-clock),
  `provider_grace_ms`, `tool_grace_ms`, `kill_escalation_ms`; no infinite grace.
- Session/task checkpoints at preemption when safe; no merge of incomplete work.
- RPC `session.preempt`, `session.preemption`; CLI `vp preempt`,
  `vp preemption`; Swift Stage banner + Manage Dash metric.
- Tests: fake slow provider stream, shell sleep tree cleanup, HTTP abort, browser
  stub, LSP cancel, deadline race, partial usage accounting, typed event/replay,
  no orphan processes, existing cancel/pause tests green.

### Files

`vp-core/src/preemption.rs` (new), `agent.rs` run loop, `vp-providers` adapters,
`tooling/process.rs`, `tooling/http.rs`, `browser.rs`, `lsp.rs`, `mcp.rs`,
protocol, daemon, CLI, Swift, ADR-0179.

---

## H4 — Multi-path budget fairness

### Design

- Extend the F5 governor additively:
  - `mode: single|multi` (default `single` for compatibility),
  - `max_paths` (2–16), `department_min_ratio`, `observed_weight_ratio`
    (0–1, default 0), `starvation_after_secs`.
- Pure multi-path allocator:
  - enumerate bounded root-to-leaf paths; weight each path by declared budget
    (or priority unit), optionally blended with observed spend from the usage
    ledger;
  - deterministic water-filling for the reserve across top-K paths;
  - per-department minimums are floors, then remaining reserve is distributed by
    path weight; ties by path id sequence.
- Admission:
  - critical membership = task on any protected path; ordering by path deficit,
    priority, age, task id;
  - deferred tasks receive aging credit after `starvation_after_secs` so a
    single heavy path cannot starve the rest;
  - hard budgets, min_task_budget, throttle, and provider pre-flight clamp are
    unchanged.
- Preview returns the path table, weights, floors, allocations, deferred
  ordering, and `inputs_hash`; bounded rows.
- Tests: two/three-path allocation, water-fill boundaries, department floors,
  observed-spend clamp, starvation aging, revision reset, determinism, disabled
  compatibility, protocol/CLI/Swift decode.

### Files

`vp-core/src/critical_path.rs`, `budget_governor.rs`, `budget.rs`, store,
protocol, daemon, CLI, Swift Efficiency/Stage, ADR-0180.

---

## H5 — Signed backups, key rotation, and anti-rollback

### Design

- Repository format v3:
  - keep Argon2id + ChaCha20-Poly1305 content encryption;
  - add an Ed25519 signing key over the authenticated manifest (repository id,
    generation, parent, entry table, tombstones, created_at, key_id);
  - v2 repositories remain readable and explicitly reported as unsigned;
    v1 ZIP remains unauthenticated.
- Keyring:
  - `keyring.json` in the backup key directory, 0600, containing key ids,
    public keys, and encrypted/secret references; private signing key material
    comes from an env var or a 0600 file, never SQLite/events/logs.
  - `backup key generate`, `backup key list`, `backup key rotate`,
    `backup key export-public`;
  - rotation changes the active key id; old public keys remain for verification.
- Anti-rollback:
  - each verified backup/restore records the repository generation in a local
    anchor file (data dir, 0600);
  - `backup verify` / `restore` reject a generation older than the anchor
    unless explicit `--allow-rollback`; the rejection is typed and audited;
  - document that a local anchor is not tamper-proof against the OS user/root
    and is not remote attestation.
- Commands/RPC:
  - `vp backup --signing-key`, `vp backup-verify`, `vp backup-key-*`,
    `vp backup-keyring`, restore verification;
  - RPC `backup.verify`, `backup.keyring`, `backup.key.rotate` (or CLI-first if
    the daemon does not own key files).
- Tests: sign/verify round-trip, tampered manifest/entry/tombstone, wrong key,
  missing key, rotation verify with old and new keys, rollback reject/allow,
  v2/v1 compatibility, secret non-leakage, atomic restore. Existing backup
  tests stay green.

### Files

`vp-core/src/backup/crypto.rs`, `backup/repository/*`, `vp-cli` backup commands,
protocol/daemon if surfaced, Swift Settings/About status, ADR-0181.

---

## 5. Integration and acceptance

Integration order: **H2 → H1 → H4 → H5 → H3**. H3 lands last because it changes
provider/tool cancellation plumbing; H5 is independent.

- +30,000 net lines of production code/tests/docs from `c344bf9`.
- No filler, no dead code, no untested happy path.
- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`,
  `just lint`, `xcodebuild test`, `just bundle`.
- Approval, Seatbelt, path jail, write scope, Verifier, merge, capacity, and
  budget gates unchanged or stricter.
- Approval templates never auto-approve Destructive/HostState/remote-exec/
  sensitive GUI; deny always wins; unknown risk fails closed.
- Test cache never reports a pass for different inputs; failed results are
  short-lived and clearly labeled; cache bypass is explicit.
- Preemption never merges incomplete work, never claims exactly-once side
  effects, and never leaves an uncancelled process group.
- Multi-path allocation remains deterministic and cannot exceed hard budgets.
- Backup signing never stores private keys in SQLite/events/logs; anti-rollback
  is a local anchor, not remote attestation.

## 6. Non-goals

- No cross-host scheduler/fencing or distributed approval queue.
- No host-reboot process persistence or automatic command restart.
- No source ANN/rerank/query expansion in this round.
- No automatic approval of Destructive/HostState operations.
- No test-result cache for the Integrator unless explicitly enabled.
- No key escrow, remote KMS, or tamper-proof anti-rollback.
- No archive extraction/media transcoding or Bitbucket adapter.
