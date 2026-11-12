# Cache benchmark scripts

## `opencode2_baseline.sh`

Extracts the aggregate cache baseline described in
`docs/CACHE_LOSSLESS_PLAN.md` §0.1 from a local OpenCode2 database.

```bash
scripts/cache-benchmark/opencode2_baseline.sh
OPENCODE_SESSION=ses_f1755d... scripts/cache-benchmark/opencode2_baseline.sh
```

The script opens SQLite read-only/immutable and selects only ids, roles, token
counters, timestamps, and instruction hashes. It never selects message content,
titles, tool arguments, or file paths. Output is safe to paste into an issue as
aggregate evidence.

OpenCode2's `tokens_input` is uncached input, so the script uses
`cache_read / (uncached_input + cache_read)`. VerlinPark's `input_tokens` is
total input, so its ratio is `cache_read / input_tokens`. Do not compare the two
denominators without relabeling.

## `vp cache-bench`

The deterministic benchmark lives in `vp-bench` and is exposed without a
daemon:

```bash
vp cache-bench                         # 50-turn simulator, Markdown on stdout
vp cache-bench --csv                   # per-call CSV on stdout
vp cache-bench --output /tmp/cache-bench
vp cache-bench --cold-start            # negative control: no previous-session prime
vp cache-bench --prefix-break          # negative control: one edited early token
vp cache-bench compare a.json b.json   # mismatches are refused
vp cache-bench --live --allow-live --model deepseek-chat  # explicit network mode
vp cache-bench --live --allow-live --model deepseek-chat --max-live-cost-usd 0.05
vp cache-bench verify report.json --min-hit-ratio 0.995
vp cache-bench matrix --output /tmp/cache-matrix
```

`matrix` runs the warm baseline, cold start, and prefix-break control with the
same shape and writes three JSON/Markdown pairs. The controls are expected to
miss the long-prefix floor; compare them with the warm baseline, not with each
other.

`verify` exits 0 only when the report validates and meets the requested floor;
a simulated report fails `--require-provider-reported`, and a cold-start or
prefix-break control fails unless `--allow-control` is explicit.

Live mode additionally requires `VERLINPARK_LIVE_API_KEY` or
`DEEPSEEK_API_KEY`. It is never selected by default and makes real provider
calls. The key is never printed or written to a report. The optional cost guard
is a pre-call stop-loss, not a provider-side budget.

The simulator's scenario size check rejects a short prefix, too few turns, a
large moving tail, or a prefix above twice the long-context floor. The check
exists so the benchmark cannot inflate a ratio with filler tokens.
