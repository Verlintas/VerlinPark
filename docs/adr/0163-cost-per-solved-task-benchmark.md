# ADR-0163: Cost-per-solved-task benchmark harness

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Tooling, Protocol
- **Related:** ADR-0026, ADR-0039, ADR-0040, ADR-0092, ADR-0138, ADR-0153,
  ADR-0155; C4 and E8 in `docs/CONTEXT_OWNERSHIP_MEMORY_BENCH_PLAN.md`

## Context

VerlinPark reports token and USD totals per session, per role, per task, and per
project. Those totals are useful for cost control, but they do not answer the
question an efficiency change is supposed to answer: did the run actually solve
the task, pass verification, and merge? A cheap run that fails acceptance is
not cheaper than a successful run. A run that spends tokens and is then graded
against a moving or human-judged target is not repeatable.

There was no harness that:

- creates a disposable fixture repository;
- executes the ordinary session/TeamPlan runtime rather than a private path;
- proves the fixture objective with deterministic acceptance;
- records verifier, merge, token, cache, tool, compaction, retry, intervention,
  and wall-clock outcomes together;
- classifies a failed acceptance or failed merge as **unsolved**;
- emits machine, human, and row-oriented reports with explicit model/pricing
  metadata and a no-universal-ranking caveat;
- refuses an A/B comparison when the fixture, acceptance, model, or pricing
  changed.

This ADR records the C4 harness. It does not claim that one local run ranks
models, providers, or agent designs. It does not change the runtime's scheduling,
approval, sandbox, Verifier, merge, capacity, or budget gates.

## Decision

### 1. Benchmark unit and the definition of solved

The benchmark unit is **one scenario repetition**. A repetition starts from a
fresh fixture repository and a fresh daemon data directory, runs one ordinary
session or stored TeamPlan, then evaluates harness-owned acceptance and the
runtime's verifier/merge outcomes.

A run is `solved` only when all of the following hold:

1. the session reaches `idle` within the scenario wall-clock timeout;
2. the harness-owned acceptance check passes;
3. the scenario's expected merge behavior is met (for plan scenarios, every
   expected Worker commit reached `merged`; for direct/Scout scenarios, no
   unexpected merge entry exists);
4. every required Verifier verdict exists and is approving;
5. plan tasks, when required, are all `completed`;
6. required Scout/compaction/parallelism observations are met;
7. no required agent failure, provider error, or harness fault occurred.

A run that spends tokens but fails acceptance or merge is `unsolved`. Cost per
solved task is `None` (`null`/`n/a`) when there are zero solved runs. It is never
reported as zero or infinity.

The unit is the scenario's acceptance task, not each internal plan subtask.
Plan metrics separately report the number of plan tasks and merged commits.

### 2. Scenario catalog

The checked-in catalog is:

| Id | Path under test | Expected gate |
|---|---|---|
| `baseline-single-session` | Leader edits directly in the session worktree | acceptance |
| `plan-worker-chain` | stored TeamPlan → Worker → Verifier → Integrator | acceptance + verdict + merge |
| `read-only-scout` | Leader delegates to a fresh-context Scout | acceptance + Scout completion + clean worktree |
| `tool-output-paging-off` | large tool output before a small edit | acceptance; spill metrics absent |
| `tool-output-paging-on` | same fixture with the C1 pager | **unavailable** in this build |
| `compaction-off` | preloaded history, context management disabled | acceptance; zero compactions |
| `compaction-on` | same history through summary/compaction | acceptance + at least one compaction |
| `worktree-parallelism-1` | four independent plan tasks, Worker cap 1 | acceptance + verdicts + merges + peak 1 |
| `worktree-parallelism-4` | same four tasks, Worker cap 4 | acceptance + verdicts + merges + peak ≥ 2 |

The paging-on entry remains in `vp bench list` with an explicit reason. Selecting
it is a configuration error (exit 2), not a silent skip and not a fake "paged"
measurement. The report records `tool_output_paging_supported` and reports spill
counts as absent unless the runtime actually emits spill events.

Scenario validation enforces unique ids, bounded repetitions/timeouts, valid
paths, plan/acceptance consistency, expected merge counts equal to the number of
plan tasks, and peak/cap consistency. Requirements that cannot be met by the
declared configuration are rejected before a run.

### 3. Fixture and acceptance determinism

Each repetition receives a `tempfile` workspace containing:

- `repo/` — a fresh Git repository initialized on `main` with one fixture commit;
- `data/` — the SQLite database, worktrees, and session state;
- `home/` and `tmp/` — isolated HOME/TMPDIR for fixture Git and acceptance;
- `acceptance/` — expected bytes and the check script, outside the repo.

The acceptance script is harness-owned. It compares expected bytes with `cmp`,
checks absent paths, and optionally requires a clean Git worktree. The agent can
edit anything inside its worktree but cannot alter the expected bytes or script.
Paths use a deliberately narrow alphabet and reject absolute paths, `..`,
backslashes, empty components, control characters, and shell metacharacters.
The workspace is deleted recursively unless `--keep-worktrees` is explicit.

Acceptance runs with a cleared environment, a fixed `PATH`, isolated HOME/TMPDIR,
`GIT_CONFIG_NOSYSTEM=1`, and a timeout. It is not an agent tool and does not use
the approval broker. It is a deterministic patch/byte check, not a human grade.

The report includes a fixture fingerprint (repository template) and an
acceptance fingerprint (expected paths, exact bytes, absent paths, clean status).

### 4. Fake and real providers

The default provider is an in-process deterministic fake implementing the
`vp_providers::Provider` trait. It:

- performs no network I/O and reads no credentials;
- serves scripted role-aware behavior for the catalog (Leader read/write, Scout
  read, Worker target write, Verifier verdict, compaction summary);
- injects deterministic simulated usage derived from request size, with
  explicit and disjoint cache-read/cache-write shares;
- records every call in a transcript and has a hard call cap;
- reports script exhaustion as a harness fault (exit 2), never as solved.

The fake provider is test data, not a hidden model route. It is identified as
`mode=fake`, `usage=fixed|simulated` in every report, and the caveat states that
fake-provider runs measure harness/runtime mechanics, not model quality.

Real-provider mode requires **all** of:

1. `--provider real`;
2. `--allow-real-provider`;
3. `VERLINPARK_BENCH_REAL_PROVIDER=1`;
4. `VERLINPARK_BENCH_BASE_URL` (or `VERLINPARK_BASE_URL`);
5. `VERLINPARK_BENCH_MODEL` (or `VERLINPARK_MODEL`) or `--model`;
6. an API key only when the endpoint needs one
   (`VERLINPARK_BENCH_API_KEY` or `VERLINPARK_API_KEY`).

A missing prerequisite fails explicitly with exit 2 before any run. Real mode
uses the same scenarios, prompts, session/plan/Verifier/merge path, and
acceptance checks. It is covered by an ignored integration smoke test and is
never run by the default test suite.

Prices are not inferred from a provider or model catalog. The caller supplies
USD per million input, output, cache-read, and cache-write prices via the
scenario profile or `--input-price`/`--output-price`/`--cache-read-price`/
`--cache-write-price`. Reports include the exact values. These are caller
assumptions, not a billing reconstruction.

### 5. Metrics and formulas

Per run, the report records:

- solved, acceptance pass/fail/timeout, session status, timeout;
- Verifier checked/approved/rejected, verdict summary, and pass rate;
- merge entries/merged/failed/conflicts, expected count, success, and rate;
- wall-clock milliseconds from session creation through acceptance;
- provider total input, output, cache-read, and cache-write tokens;
- provider calls, usage-ledger calls, and whether usage was estimated;
- USD cost recomputed from the explicit prices, ledger cost, and reconciliation;
- tool calls, tool errors, event-visible output bytes, spill count (present only
  when supported), compactions/failures, stream retries, human interventions,
  Scout completions, peak concurrent Workers, agent failures, plan-task counts;
- session id and run id.

Cost uses disjoint input portions, exactly as the daemon ledger does:

```text
cache_read  = min(cache_read_tokens, input_tokens)
cache_write = min(cache_write_tokens, input_tokens - cache_read)
uncached    = input_tokens - cache_read - cache_write
cost_usd =
    uncached        / 1_000_000 * input_price
  + cache_read      / 1_000_000 * cache_read_price
  + cache_write     / 1_000_000 * cache_write_price
  + output_tokens   / 1_000_000 * output_price
```

Ratios use an absent denominator as `None`:

```text
cache_hit_ratio       = cache_read / (uncached + cache_read + cache_write)
verifier_pass_rate    = approved / checked
merge_rate            = merged / entries
cost_per_solved_task  = total cost / solved runs        (None if zero solved)
tokens_per_solved     = total tokens / solved runs      (None if zero solved)
```

Aggregates across repetitions use all spent cost, including failed repetitions,
divided by solved repetitions. A failed repetition is not omitted. USD values
are rounded to nanodollar precision before serialization so JSON round trips are
stable; Markdown/CSV display six decimals.

### 6. Reports and caveat

One run command writes three files with a shared stem:

- `<stem>.json` — the machine source of truth (schema version 1);
- `<stem>.md` — a human summary with metadata, caveat, scenario table, unsolved
  runs, and harness faults;
- `<stem>.csv` — one row per run with fixed columns.

Writes are atomic: a uniquely named temporary file is created in the target
directory, synced, and renamed. Every artifact is bounded to 8 MiB. Reports
include the commit SHA and dirty flag when available, host OS/arch/CPU/hostname,
provider mode/id/model/usage source, all prices, selection and repetition count,
and the summary.

Every report carries this caveat:

> These results are local measurements for the recorded fixture, provider mode,
> model, pricing profile, host, and commit. Fake-provider runs measure
> harness/runtime mechanics, not model quality. Caller-supplied prices may not
> match provider billing. One host and one model are not a universal ranking,
> and this report does not claim that any configuration is generally faster,
> cheaper, or better.

### 7. CLI and exit codes

The harness is available as `vp-bench` and as the additive `vp bench` namespace:

```text
vp bench list
vp bench run [--scenario ID]... [--repetitions N] [--output DIR]
             [--provider fake|real] [--allow-real-provider]
             [--input-price ... --output-price ...]
             [--cache-read-price ... --cache-write-price ...]
             [--acceptance-timeout-secs N] [--timeout-secs N]
vp bench report <file.json>
vp bench compare <a.json> <b.json> [--allow-mismatch]
```

Exit codes:

- `0` — every selected scenario solved;
- `1` — at least one scenario unsolved (including failed acceptance/merge);
- `2` — harness, configuration, provider-gate, report, or comparison error.

`report` exits 0 after successfully rendering a saved report. `compare` exits 0
only when the reports are comparable and both sides are fully solved; a refused
comparison is exit 2, and an unsolved side is exit 1.

The CLI prints a compact per-scenario summary and the three report paths. JSON
mode (`--json`) prints the full report/envelope.

### 8. A/B comparison refusal

`compare` checks, per scenario:

- schema version;
- fixture kind and fixture fingerprint;
- acceptance fingerprint;
- provider mode;
- model;
- pricing fingerprint.

Different commits, hosts, and repetition counts produce warnings but are not
refused because they do not by themselves change the fixture/acceptance/model
identity. Any compatibility mismatch is refused unless `--allow-mismatch` is
explicitly passed; an allowed comparison is labelled `comparable: false` and
never silently presented as apples-to-apples.

The comparison reports solved-rate point change, cost-per-solved percent change,
tokens-per-solved percent change, cache-hit point change, verifier/merge point
changes, and wall-clock percent change. Relative deltas are absent when the A
baseline is zero/absent. The comparison repeats the caveat that one host/model is
not a universal ranking.

### 9. Runtime fairness and gate preservation

A benchmark run uses the ordinary:

- `Store`/`Engine` project, session, worktree, and TeamPlan APIs;
- `AgentService` run, plan resume, Verifier, and Integrator path;
- write-scope and capacity mechanisms (Worker cap is the scenario knob);
- approval mode (`unattended` is explicit in configuration and still denies
  host-level operations; it is not a hidden bypass);
- budget/usage ledger and pricing fields;
- session event stream, plan-task rows, merge queue, and usage ledger.

The harness calls no private merge, approval, or verification helper. It does
not flip a sandbox, Verifier, merge, capacity, or budget gate. It does not add a
protocol field or RPC. It does not route a model based on scenario, cost, or
content.

## Alternatives considered

1. **Wiremock HTTP server only.** It would exercise the OpenAI adapter, but the
   Provider trait is already covered by the provider crate and real wiremock
   tests; a direct fake provider keeps the harness hermetic and deterministic
   while still using the real agent runtime.
2. **Fixture acceptance as free-form shell commands declared by callers.**
   Rejected: arbitrary acceptance commands make reports non-comparable and
   increase the blast radius. The fixed byte/path/clean checks are enough for
   the catalog; richer fixtures can be added in code with review.
3. **Measure only tokens/cost.** Rejected: it rewards spending nothing and
   failing. Solved/verifier/merge are first-class outcomes.
4. **Compare reports by scenario id alone.** Rejected: same id with changed
   acceptance bytes or model is not the same benchmark.
5. **Use the user's repository or daemon data directory.** Rejected: the harness
   must never mutate real worktrees, sessions, or credentials by default.

## Consequences and limitations

- The catalog is intentionally small and CPU-cheap. It is a regression harness,
  not a model leaderboard.
- Fake-provider usage is simulated from request size. It is deterministic and
  useful for comparing runtime mechanics (compaction, context size, tool
  output), but it says nothing about a real model's tokenization or quality.
- Wall-clock includes fixture setup, session creation, run, merge, and
  acceptance within the harness. It is not a provider-latency benchmark, and
  simulated fake latency is recorded separately.
- Peak Worker parallelism is measured from overlapping Worker provider streams,
  not from eager plan-task creation. This is the honest execution signal for the
  current runtime.
- Tool-output spill metrics are absent until the C1 pager exists. Event-visible
  output bytes are bounded by the runtime's event cap and are not the full
  process output.
- The harness is single-host, single-daemon, and sequential across repetitions.
  It does not measure cross-process contention or distributed scheduling.
- Real-provider runs need explicit operator setup and may cost money; the
  default test suite never invokes them.
- A report is not a claim of statistical significance. Repeated runs are
  recorded, but no confidence interval or significance test is computed.
- The fixture workspace is deleted on success unless retained for debugging.
  Retained workspaces are temp paths, not project worktrees.

## Test coverage

- scenario catalog validation, duplicate/id/requirements rejection, unavailable
  paging entry, fixture fingerprints;
- fixture Git initialization, cleanup, path-jail rejection, exact-byte
  acceptance, absent paths, clean worktree, timeout behavior;
- fake provider deterministic sequences, role behavior, target parsing,
  compaction handling, usage simulation, call caps, metering;
- cost math with cache reads/writes and explicit prices, clamping, zero
  denominators, zero-solved handling, rounding;
- solved/unsolved classification for acceptance failure, verifier rejection,
  missing/failed merges, plan-task incompleteness, compaction/parallelism
  requirements;
- report JSON round trip, schema rejection, oversized rejection, Markdown caveat,
  CSV escaping/shape, atomic writes and temp cleanup;
- A/B mismatch refusal and `--allow-mismatch` labelling, null deltas;
- end-to-end baseline, plan/Worker/Verifier/merge, Scout, deterministic
  repetition, CLI run/report/compare, unavailable-scenario refusal;
- ignored real-provider smoke test gated by the environment.

## References

- `crates/vp-bench/README.md`
- `crates/vp-bench/src/{scenario,fixture,provider,runner,metrics,report,compare,cli}.rs`
- `docs/CONTEXT_OWNERSHIP_MEMORY_BENCH_PLAN.md` C4 and E8
- ADR-0138 (cache usage), ADR-0153 (cache telemetry), ADR-0155 (run journal)
