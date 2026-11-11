# vp-bench

Deterministic, disposable benchmark harness for VerlinPark. It measures
**cost per solved task**, not just tokens: a run is solved only when its fixture
acceptance passes and the expected Verifier/Integrator gates succeed. A failed
acceptance or merge is unsolved even if tokens were spent.

## Quick start

```bash
# No daemon, no network, no credentials.
vp bench list
vp bench run --scenario baseline-single-session --repetitions 3 --output ./bench-reports
vp bench report ./bench-reports/<stem>.json
vp bench compare ./bench-reports/a.json ./bench-reports/b.json
```

The standalone binary exposes the same commands:

```bash
cargo run -p vp-bench -- list
cargo run -p vp-bench -- run --scenario plan-worker-chain --repetitions 1
```

`vp bench run` prints a compact summary and the JSON, Markdown, and CSV paths.
JSON is the machine source of truth.

## Scenarios

| Id | What runs | Solved check |
|---|---|---|
| `baseline-single-session` | Leader edits directly in a session worktree | exact file bytes |
| `plan-worker-chain` | stored TeamPlan → Worker → Verifier → Integrator | bytes + approving verdict + merged commit |
| `read-only-scout` | Leader calls the read-only Scout tool | Scout completed, no writes, clean worktree |
| `tool-output-paging-off` | Leader reads a large fixture output before editing | exact file bytes |
| `tool-output-paging-on` | C1 paged-output path (8 KiB threshold) | available; requires a spill event |
| `compaction-off` | preloaded history, context management disabled | bytes + zero compactions |
| `compaction-on` | preloaded history through compaction | bytes + at least one compaction |
| `worktree-parallelism-1` | four independent plan tasks, Worker cap 1 | bytes + 4 verdicts/merges + peak 1 |
| `worktree-parallelism-4` | same tasks, Worker cap 4 | bytes + 4 verdicts/merges + peak ≥ 2 |

Unavailable scenarios are listed with a reason. Selecting one is exit 2; the
harness does not silently skip it or pretend to measure a feature that is not
present.

## What one repetition does

1. Creates a unique temp workspace with `repo/`, `data/`, `home/`, `tmp/`, and
   `acceptance/`.
2. Initializes a fresh Git repository on `main` and commits the fixture.
3. Creates a normal VerlinPark project, session, worktree, and (for plan
   scenarios) a stored TeamPlan.
4. Runs the ordinary `AgentService` path with a deterministic fake provider.
5. Waits for a terminal session and the expected merge queue state.
6. Runs the harness-owned acceptance script outside the agent worktree.
7. Collects the usage ledger, events, plan-task rows, Verifier verdicts, merge
   entries, tool counts, compaction/retry/intervention counts, and wall clock.
8. Deletes the workspace unless `--keep-worktrees` is set.

The fake provider performs no network I/O and reads no credentials. It returns
ordinary `ChatRequest`/`StreamEvent` values and records a bounded transcript.
Simulated usage is derived deterministically from request size.

## Solved and cost

A run is solved only if the session is idle, acceptance passes, expected merges
are merged, required Verifier verdicts are approving, required plan tasks are
completed, required Scout/compaction/parallelism observations hold, and there is
no provider error, agent failure, or harness fault.

```text
uncached        = input - cache_read - cache_write
cost            = uncached/1e6*input_price
                + cache_read/1e6*cache_read_price
                + cache_write/1e6*cache_write_price
                + output/1e6*output_price
cost_per_solved = total cost / solved runs       (null when solved == 0)
tokens_per_solved = total tokens / solved runs   (null when solved == 0)
cache_hit_ratio = cache_read / input             (null when input == 0)
verifier_pass_rate = approved / checked          (null when checked == 0)
merge_rate = merged / entries                    (null when entries == 0)
```

All prices are caller-supplied USD per million tokens. They are not inferred
from a vendor catalog and are not a billing reconstruction. Use the flags:

```bash
vp bench run --scenario baseline-single-session \
  --input-price 3 --output-price 15 \
  --cache-read-price 0.30 --cache-write-price 3.75
```

## Reports

One run writes three bounded (≤8 MiB) files with a shared stem:

- `.json` — schema-versioned machine report;
- `.md` — human summary, metadata, caveat, unsolved runs, harness faults;
- `.csv` — one fixed-schema row per run.

Writes are atomic. The report includes commit/dirty status, host metadata,
provider id/mode/model/usage source, exact prices, selection/repetition count,
scenario aggregates, per-run metrics, and the caveat that one host/one model is
not a universal ranking.

## A/B comparison

```bash
vp bench compare a.json b.json
vp bench compare a.json b.json --allow-mismatch
```

Comparison refuses mismatched fixture fingerprints, acceptance fingerprints,
provider modes, models, pricing profiles, and schema versions. Different
commits/hosts/repetition counts are warnings. `--allow-mismatch` is explicit and
labels the output `comparable: false`; the harness never silently compares
apples to oranges.

## Real provider mode

Real mode is never the default and is not exercised by the test suite. It
requires all of:

```bash
export VERLINPARK_BENCH_REAL_PROVIDER=1
export VERLINPARK_BENCH_BASE_URL=http://127.0.0.1:8080/v1   # or VERLINPARK_BASE_URL
export VERLINPARK_BENCH_MODEL=my-model                     # or VERLINPARK_MODEL
export VERLINPARK_BENCH_API_KEY=...                        # only if needed; or VERLINPARK_API_KEY

vp bench run --provider real --allow-real-provider \
  --scenario baseline-single-session
```

If a required setting is missing, the command fails before any run with exit 2.
There is an ignored integration smoke test:

```bash
cargo test -p vp-bench --test harness real_provider_baseline_smoke -- --ignored
```

Real-provider runs may incur provider charges.

## Exit codes

- `0` — every selected scenario solved;
- `1` — at least one selected scenario unsolved;
- `2` — harness, configuration, provider-gate, report, or comparison error.

`vp bench report` exits 0 after rendering a saved report. `vp bench compare`
exits 0 only when both reports are comparable and fully solved; a refused
comparison is 2, and an unsolved side is 1.

## Safety and limitations

- Fixtures live only in a unique temp workspace; the harness never touches the
  user's project, daemon data directory, or credentials by default.
- Acceptance expected bytes and the check script are outside the agent worktree.
- The harness adds no hidden model routing and no private merge/approval/
  verification shortcut. Runs use the ordinary session/plan/Verifier/merge
  gates.
- Fake-provider usage is simulated. It is useful for runtime mechanics and
  regression comparisons, not model quality.
- Tool-output spill metrics require an observed `tool.output_spilled` event; paging-off records 0 and paging-on requires at least one.
- Peak Worker parallelism is measured from overlapping Worker provider streams,
  not from eager plan-task creation.
- One host, one model, and one fixture set are not a universal ranking. No
  statistical significance is claimed.

See `docs/adr/0163-cost-per-solved-task-benchmark.md` for the full decision.
