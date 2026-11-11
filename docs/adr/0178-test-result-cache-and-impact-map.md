# ADR-0178: Test-result cache and impact map

- **Status:** Accepted
- **Date:** 2026-11-12
- **Deciders:** VerlinPark runtime maintainers
- **Related:** ADR-0171 (code intelligence), ADR-0170 (tool error contract),
  ADR-0103 (tool-output spilling), ADR-0176 (critical-path governor),
  `docs/LONG_RUN_OPERATIONS_HARDENING_PLAN.md` H2

## Context

`run_project_task kind=test|lint|check` executed the same command every time,
even when the worktree content, detected command, environment, and tool version
were unchanged. Long runs therefore paid repeatedly for identical suites, and
agents could not distinguish a fresh execution from a replay. `affected_tests`
was lexical only and had no historical mapping from a changed path to tests that
actually covered it.

The cache addresses only this reuse problem. It is not a test runner, not a
coverage engine, and not a merge gate. A hit means "the same command over the
same inputs produced this recorded result"; it never means "the code is
verified" or "the Integrator may skip its command".

## Decision

### 1. Project policy

A persisted `TestCachePolicy` is stored per project in SQLite
(`project_test_cache_policies`), copied into `projects.jsonl`, and restored by
`db-rebuild`:

```json
{
  "enabled": true,
  "max_entries": 512,
  "retention_days": 14,
  "max_output_bytes": 262144,
  "reuse_lint": true,
  "include_integration": false,
  "failed_retention_secs": 900
}
```

- `enabled` defaults to true for explicit `run_project_task` calls.
- `include_integration` defaults to false. The deterministic Integrator never
  consults the cache unless a project explicitly opts in. When enabled, a hit
  skips only the integration command execution and returns the recorded bounded
  output plus an explicit cache-hit marker; the approval, sandbox, verification,
  merge, capacity, and budget gates are unchanged. A cached integration failure
  is returned as a failure, never as a pass.
- `max_entries` is bounded to `1..=10,000`; `retention_days` to `1..=365`;
  `max_output_bytes` to 4 KiB–8 MiB; `failed_retention_secs` to 60–86,400.
- `reuse_lint` can disable lint reuse while keeping tests/checks cached.
- Only `test`, `lint`, `check`, and `typecheck` are cacheable. `build`, `format`,
  `dev`, and `custom` are never cached: they are either destructive,
  long-running, or not a test signal. `typecheck` is stored as `check`.
- Invalid RPC values are rejected with `TEST_CACHE_POLICY_INVALID`; stored and
  manifest-decoded values are normalized to the same bounds.

### 2. Versioned, content-addressed key

The key is:

```
v1:<sha256(
  "vp-test-cache-v1",
  key_version,
  kind,
  raw command,
  tokenized argv,
  working directory,
  sorted normalized environment subset,
  profile_hash,
  inputs_hash,
  impact_generation,
  tool/runtime version
)>
```

- **argv:** a small deterministic tokenizer records the resolved argument
  shape. The raw command is hashed too, so quoting changes are visible.
- **cwd:** the tool always runs at the worktree root, recorded as `.`. Absolute
  worktree paths are excluded so different agent worktrees can share a result
  when content and commands match.
- **Environment subset:** a fixed allowlist of build/test-relevant variables
  (`CI`, `LANG`, `LC_*`, `TZ`, `PATH`, compiler flags, toolchain roots, package
  manager homes, `PYTHONPATH`, `GOFLAGS`, etc.). Values are only hashed; they
  never appear in a row, event, CLI output, or log. Secrets are not in the list.
- **profile_hash:** languages/counts, package managers, detected commands and
  sources, test layout, conventions, and CI commands. Root paths and warnings are
  excluded so two checkouts with the same detection agree.
- **inputs_hash:** every non-ignored worktree file (content, path, length) plus
  explicit changed paths and the lexical dependency closure. The walk is sorted,
  bounded (30,000 files / 512 MiB / 8 MiB per file), and **fails closed**: if it
  is incomplete or unreadable, reuse is disabled for that run. A commit hash is
  never sufficient because uncommitted edits, ignored config, and dependency
  closure edits can change a result.
- **impact_generation:** digest over changed paths, profile hash, the lexical
  reverse-import closure/digest, and LSP reference paths. Historical cache rows
  are deliberately excluded so recording a result cannot invalidate the key that
  produced it.
- **Tool version:** a crate/version/cache-model string. Bumping the key version
  or tool model safely invalidates every old row; old metadata remains until
  retention/prune but never matches.

### 3. Storage and lifecycle

`test_results` rows contain:

`id, project_id, session_id, kind, command, command_source, key_version,
cache_key, inputs_hash, impact_generation, status, exit_code, duration_ms,
output_ref, output_bytes, covered_paths, warnings, warning_count,
failure_count, result_json, created_at, expires_at, hit_count, last_hit_at`.

- `status` is `passed`, `failed`, or `error`. A cached failure is always
  returned as a failure with `cache_hit=true`; it is never converted into a pass.
- `covered_paths` records the changed paths, selected/affected test paths, and
  dependency closure used to produce the row. Historical mapping reads this
  bounded list; it is not coverage instrumentation.
- `result_json` is a bounded structured payload (failures, warnings, counts,
  bounded raw tail, next actions) used to render a hit. It is clipped at 8 KiB.
- **Output bodies** go through the existing content-addressed tool-output store
  as an opaque `output_ref`. The cache never inlines an unbounded command log.
  The body is bounded by `max_output_bytes` before spilling.
- **Retention:** a pass expires after `retention_days`; a failure/error expires
  after `failed_retention_secs`. Expired rows are never returned. A lookup that
  finds an expired row opportunistically sweeps expired metadata.
- **Max entries:** an insert evicts oldest rows beyond `max_entries`, grouped by
  owning session, with a `test_cache.pruned` event per affected session.
- **Manual lifecycle:** `test_cache.stats`, `test_cache.entries`,
  `test_cache.prune` (dry-run supported), and `test_cache.clear` (explicit
  `confirm=true`; CLI requires `--yes`). Clearing is metadata-only and cannot be
  undone.
- **Events:** `test_cache.stored`, `test_cache.hit`, `test_cache.invalidated`,
  and `test_cache.pruned` are persisted in the session event stream, mirrored to
  JSONL, and replayable. `stored` carries full bounded metadata plus a result
  preview; `hit` carries result/session/hit provenance; `invalidated` and
  `pruned` identify removed rows. Replay reconstructs row existence and durable
  metadata; hit counters and output bodies are operational and are not required
  to match. `session.replay {repair:true}` replaces a session's rows from events;
  bundle export carries metadata (not bodies), and import rebuilds from events
  (falling back to legacy projection rows).
- **Hit provenance:** every hit adds `cache_hit: true`, `cached_at`,
  `inputs_hash`, `impact_generation`, `cache_result_id`, `cache_status`, and a
  `cache` object whose note says no command ran.

### 4. `run_project_task` integration

- New optional inputs: `cache: auto|bypass|refresh` (default `auto`) and
  `mode: lexical|impact` (default `lexical`, preserving prior selection).
- `auto` reads and writes; `bypass` never reads or writes; `refresh` skips the
  lookup, executes, and replaces an existing row with the same key.
- On a hit, the command is not executed. The stored bounded result is rendered
  with the cached exit code/status, failures, warnings, raw tail, and next
  actions. A cached failure is marked as a failure with `TEST_FAILED` (or
  `COMMAND_FAILED` for an `error` row).
- On a miss/refresh, the existing resolution, package/filter/affected-test
  logic, timeout, approval, and sandbox path run unchanged. After the command,
  the result is persisted once; cache-write failures are reported as warnings
  and do not change the fresh command result.
- If input hashing is incomplete, the command still runs and the result is
  returned, with a `TEST_CACHE_INPUT_INCOMPLETE` note and no reuse.
- `dry_run` performs no lookup and no execution.
- Approval, Seatbelt, path-jail, write-scope, Verifier, merge, capacity, and
  budget gates are unchanged. A cache hit is not a new command execution; it is
  the recorded result of a prior execution under the exact same inputs.

### 5. Impact map

`TestImpactMap` is a deterministic, bounded mapping from changed paths to test
targets. Every target carries exactly one best source and a confidence:

- **profile:** project conventions and test-layout heuristics (`_test`,
  `tests/`, `spec`, language layouts). No execution.
- **lexical:** files in the reverse-import closure of a changed file that are
  test-like, plus their convention candidates. The closure and edge digest are
  bounded (300 paths, depth 3); a truncated scan is labeled.
- **lsp:** references from an **already-running** language server for up to four
  defined symbols per changed file. No server is spawned by mapping. If no
  server is running or a request fails, the map continues with profile/lexical/
  history and records a note; explicit `mode=impact` never claims LSP coverage.
- **history:** cached `passed` rows whose `covered_paths` include the changed
  path or a directory prefix. Confidence decreases with age and rises with hits,
  clamped to `0.5..=0.95`.

Targets are deduplicated by path, choose the highest confidence (ties prefer
LSP > lexical > profile > history), sorted by confidence/source/path, and capped
at 32. `generation` covers changed paths, profile, lexical digest/closure, and LSP
paths. The map explicitly says it is heuristic, does not prove completeness, and
executes no tests.

`affected_tests` gains `mode: lexical|impact` (default lexical) and labels each
target's source. Legacy callers keep the old selection and `confidence:
"lexical"` behavior. `run_project_task` may use the map to derive cache covered
paths and the impact generation; with default `mode=lexical` it still selects
tests through the existing lexical path.

### 6. Protocol, CLI, Swift

Backward-compatible additive RPCs:

- `test_cache.policy.get` / `test_cache.policy.set`
- `test_cache.stats`
- `test_cache.entries`
- `test_cache.prune`
- `test_cache.clear`
- `test_cache.impact`

All new fields are `serde(default)`/optional. CLI commands: `vp test-cache`,
`test-cache-entries`, `test-cache-policy-set`, `test-cache-prune`,
`test-cache-clear` (requires `--yes`), and `test-impact`. Swift decodes the same
models, exposes a Preview → Tests panel with counters, policy editor, prune,
clear, entry metadata, and impact preview, and renders a `CACHED` badge in the
transcript only when the runtime reported `cache.cache_hit=true`.

### 7. Errors

Typed Q3 codes: `TEST_CACHE_POLICY_INVALID`, `TEST_CACHE_UNAVAILABLE`,
`TEST_CACHE_INPUT_INCOMPLETE`, `TEST_CACHE_CONFLICT`,
`TEST_CACHE_CLEAR_UNCONFIRMED`, `TEST_CACHE_KEY_INVALID`, and
`IMPACT_MAP_UNAVAILABLE`. Unknown failures collapse to a safe non-retryable
tool failure; no cache error auto-retries a command.

## Consequences

- Identical test/lint/check runs avoid repeated execution and receive explicit
  hit provenance.
- A cached pass is impossible for different content, command, environment,
  profile, tool version, or impact scope; an incomplete hash disables reuse.
- Failed results are short-lived and clearly labeled; no pass is fabricated.
- Long runs can use historical coverage to narrow test selection without
  claiming completeness or running tests during mapping.
- Cache metadata is auditable, replayable, prunable, and bounded; output bodies
  retain the existing operational-storage lifecycle.
- Integrator behavior is unchanged unless explicitly opted in.

## Non-goals

- No claim of test coverage, correctness, or semantic verification.
- No cache for the Integrator by default.
- No arbitrary environment capture; only the documented subset is keyed.
- No external-input keying: network responses, wall-clock/randomness, services,
  and files outside the worktree are not part of the key. Use `cache=bypass` or
  `cache=refresh` when a result depends on those inputs.
- No remote/shared cache, distributed invalidation, or cross-host cache.
- No test execution during impact mapping, no automatic test generation, and no
  completeness guarantee.
- No coverage instrumentation, flaky-test detection, or rerun-until-green.
- No cache entry overrides approval, sandbox, write-scope, verification, merge,
  capacity, or budget gates.
- No persistent cache of `dev`, `format`, `build`, or arbitrary `custom` tasks.
- No replacement for the existing tool-output retention store.
