# ADR-0197: Reliability metrics and OTel GenAI projection

- **Status:** Accepted
- **Date:** 2026-11-12
- **Owners:** Runtime, Tooling, Protocol
- **Related:** ADR-0163, ADR-0192, ADR-0193, ADR-0194, ADR-0195,
  ADR-0196; C5 in `docs/CONTRACTED_EXECUTION_PLAN.md`

## Context

The cost-per-solved-task harness (ADR-0163) answers whether a benchmark run
solved a task and what it cost. It did not answer three questions that matter
for long-horizon multi-agent work:

1. **Is a success repeatable?** One solved run can hide a mostly failing
   process. Repeated deterministic trials need `pass@k`, `pass^k`, variance,
   and a flake rate.
2. **Did a passing run follow a defensible process?** A commit can merge after
   no passing test, a retry can repeat without new evidence, a pair of edits
   can return to the previous content hash, or verification can happen before
   implementation. These are deterministic ordering signals, not model prose.
3. **How much of the spend was coordination?** Leader, Manager, Verifier, and
   delegation work consume tokens without directly producing the task. The
   ratio to Worker/Labour productive tokens is an efficiency signal, not a
   quality verdict.

Separately, the runtime had no standard trace projection. Event data existed,
cache and usage telemetry existed, and the GUI had cache/cost surfaces, but
there was no way to inspect a session as GenAI spans without copying prompts,
tool arguments, results, or file content into a third-party collector.

C5 is explicitly **acceptance measurement**. It must not become a merge gate,
approval, Verifier verdict, scheduler input, or hidden quality score. It also
must not weaken any existing gate or add a provider call to tests or default
paths.

## Decision

### 1. Reliability is a projection over benchmark trials

A *trial* is one existing benchmark repetition: fresh fixture, fresh daemon
data directory, one ordinary session or stored TeamPlan, harness-owned
acceptance, normal Verifier/Integrator path. Reliability does not add a new
execution path and does not re-run model output.

A reliability report is built per scenario from repeated trials. It contains:

- `pass@k` and `pass^k`;
- success variance and flake rate;
- Lucky-Pass signals from event/tool ordering;
- coordination token attribution and ratio;
- cost per solved task and per solved scenario;
- a bounded per-trial outcome list.

The default report is per scenario only. There is no cross-model leaderboard
and no claim of statistical significance. The aggregate section is descriptive:
mean `pass@k`, mean `pass^k`, mean success variance, flaky scenario count, mean
flake rate, pooled coordination ratio, and pooled cost. The report caveat is
printed in JSON, Markdown, CLI output, and the ADR.

### 2. pass@k, pass^k, variance, and flake rate

For `n` deterministic trials with `c` solved trials:

```text
pass@k = 1 - C(n-c, k) / C(n, k)
pass^k = C(c, k) / C(n, k)
```

Both are `None` when `k` is zero or greater than `n`. `k` is bounded to
1..=20. `k=0` in the CLI resolves to `min(trials, 5)`. A saved report can be
analyzed with an explicit `k` and performs no provider call.

Success variance is the sample variance of the binary outcomes (`n-1`
denominator), `None` for fewer than two trials.

```text
success_variance = p * (1 - p) * n / (n - 1), p = c / n
```

Flake rate is the fraction of adjacent trial pairs whose outcome changed:

```text
flake_rate = outcome_transitions / (n - 1)
```

A scenario is `flaky` when it has both solved and unsolved trials. Deterministic
trials that all solve (or all fail) report `0.0`, not a missing value. Fewer
than two trials report `None` for both variance and flake rate.

The existing `vp bench run` report remains schema version 1. Additive
`trial_evidence`, `coordination`, and `scenario.reliability` fields use
`serde(default)` and do not change the existing fields. The dedicated
`vp bench reliability` report uses its own schema version 1 and writes JSON,
Markdown, and CSV atomically with an 8 MiB bound.

### 3. Lucky-Pass signals

Lucky-Pass is a set of deterministic ordering heuristics over the event log and
tool results. It is not a semantic judgment, not model prose, and not a claim
that a run was incorrect. Each signal has a stable code, a count, a first event
sequence when known, and a bounded detail string that never contains prompts,
tool arguments, results, file content, or raw errors.

| Signal | Deterministic rule |
|---|---|
| `commit_without_passing_test` | At a `session.committed` or `merge_queue.enqueued` event, there is an implementation mutation, and no passing test occurred after the most recent implementation mutation before the commit. |
| `retry_without_new_evidence` | A `agent.stream_retry`, `plan_task.retry_scheduled`, or `merge_queue.retry` event repeats a retry with the same SHA-256 evidence hash as the previous retry. The evidence hash covers only tool name/call id/error code/error flag/visible output bytes; it is not a content hash. The first retry in a sequence does not count. |
| `edit_revert_cycle` | A successful same-path mutation has `after_sha256` equal to the `before_sha256` of the immediately preceding mutation on that path. `write_file` uses `sha256`, `edit_file` uses `after_sha256`, and `apply_patch` inspects each file entry. Dry runs and errors are ignored. |
| `test_before_implement` | The first test command precedes the first implementation mutation. Both must exist. |
| `verify_before_implement` | The first `submit_verdict` or approving-verifier completion precedes the first implementation mutation. Both must exist. |

Implementation mutations are `write_file`, `edit_file`, `apply_patch`, and
`file_manage` results that are not errors and not dry runs. Test evidence is a
`run_project_task` or `shell` result whose structured command tokenizes to a
known test runner (`test`, `pytest`, `vitest`, `jest`, `rspec`, `phpunit`,
`ctest`, `tox`, `unittest`, or a `test`/`tests` token). A passing test requires
no tool error and an explicit structured exit code of zero. A cached passing
result counts as test evidence because the cache key is input-bound; the ADR
does not upgrade it to independent proof.

Signals are observation-only. They never block a commit, merge, landing, or
approval, never alter a Verifier verdict, never re-run a task, and never change
scheduling or budgets. A run is `flagged` when it carries at least one signal;
`lucky_pass_rate` is flagged solved runs divided by solved runs, `None` when no
trial solved. Signal presence is reported alongside unsolved runs and is not
treated as a second failure classifier.

### 4. Coordination overhead

Coordination overhead uses the existing usage ledger; it adds no provider call
and no synthetic usage row. Tokens are `input_tokens + output_tokens` per
entry, already normalized by the store.

```text
orchestration_tokens =
    sum(role in leader, manager, verifier)

delegation_tokens =
    sum(role in scout)
  + sum(role in worker, labour with no plan task id)
  + sum(any other role)

productive_tokens =
    sum(role in worker, labour with a non-empty plan task id)

ratio = (orchestration_tokens + delegation_tokens) / productive_tokens
coordination_share = (orchestration_tokens + delegation_tokens)
                     / total_tokens
```

`ratio` is `None` when `productive_tokens` is zero, never zero or infinity.
`coordination_share` is `None` when the ledger is empty. The report exposes
`numerator_tokens`, `denominator_tokens`, the numerator components
(`orchestration_tokens`, `delegation_tokens`, `untasked_worker_tokens`,
`scout_tokens`), the productive denominator, total, ratio, and share. This attribution is an
approximation: a Leader token that mostly managed a task is still counted as
orchestration, and a Worker entry with a task id is counted as productive even
if that turn was spent coordinating. It is a stable accounting view, not a
work-classifier.

The Nature MI 2026 200–300% coordination sweet spot is not encoded as a gate or
a target. A report may name the ratio; it must not fail a run for being above or
below it.

### 5. Cost per solved task and per scenario

Cost per solved task is total scenario cost divided by solved trials, including
spend from unsolved trials. It is `None` when no trial solved. Cost per solved
scenario is total scenario cost when at least one trial solved, otherwise
`None`; the aggregate divides total cost by the number of solved scenarios.
These are caller-priced estimates under the ADR-0163 rules, not a provider
invoice.

### 6. CLI and report shape

The dedicated command is additive:

```text
vp bench reliability [--scenario ID]... [--trials N] [--k K]
                     [--output DIR]
                     [--provider fake|real] [--allow-real-provider]
                     [--input-price ... --output-price ...]
                     [--cache-read-price ... --cache-write-price ...]
                     [--acceptance-timeout-secs N] [--timeout-secs N]
vp bench reliability --from-report report.json [--scenario ID]... [--k K]
```

- New-run default is 5 trials; the CLI caps new runs at 20 trials and 20
  scenarios. Saved reports may contain up to 100 trials per scenario.
- Fake provider is the default and performs no network I/O.
- Real-provider mode keeps the ADR-0163 triple gate.
- `--from-report` performs no provider call, workspace creation, or session
  mutation and rejects provider/pricing/worktree/timeout flags that would be
  silently ignored.
- Exit codes remain: `0` all selected trials solved, `1` any unsolved, `2`
  configuration/harness/report refusal. Reliability is observation-only even
  when it returns `1`; callers must not turn that into a merge gate.

Markdown contains the metadata, caveat, per-scenario table, summary, and a
bounded trial-outcome list. CSV is one row per scenario with stable columns.
JSON is the machine source of truth. All three artifacts are atomically written
and bounded to 8 MiB.

### 7. OTel GenAI export is an event-log projection

OpenTelemetry export is implemented as a **read projection** in
`vp-core/src/otel.rs`, with wire types in `vp-protocol/src/otel.rs`. It never
appends an event, mutates usage, changes a budget, submits a Verifier verdict,
enters a merge queue, or makes a scheduling decision. A status/export request is
safe to retry; the projection is deterministic from the same event window.

The span model is:

| Span | Source | Attributes |
|---|---|---|
| session root | `session.created` or first/last event | `gen_ai.operation.name=session`, `gen_ai.conversation.id`, `verlinpark.session.id`, project id, event count, truncated |
| agent | `agent.spawned` / `agent.started` → `agent.finished` | agent id/name/role/depth, provider, request model, attempt, stream retries, task id, department, finish status, optional approval |
| tool | `tool.started` → `tool.finished` | conversation id, tool name/call id, parent agent, duration via span bounds, error status, bounded error code, retryable, output bytes, truncation, approval risk |
| usage | `agent.usage` | conversation id, agent id/name/role, provider, request/response model, input/output tokens, cache-read/cache-write tokens, estimated flag |

`gen_ai.usage.cache_read.input_tokens` and
`gen_ai.usage.cache_write.input_tokens` are explicit VerlinPark extensions for
cache accounting. They are disjoint subsets of input and are already normalized
by the usage ledger. The projection does not infer cache savings or claim a
provider TTL.

Span ids are deterministic SHA-256 prefixes of the session id, event sequence,
and span kind. Trace ids are deterministic per session. This makes fixture tests
stable; it is not a security or cryptographic uniqueness claim. Timestamps use
the event `created_at`; a tool span uses the finished event and its
`duration_ms` when present. A missing start event still produces a bounded
tool span from the finish duration.

The OTLP/HTTP JSON body contains one `resourceSpans` entry with fixed resource
attributes (`service.name`, `service.version`, `telemetry.sdk.*`,
`verlinpark.projection=event_log`, event count, truncated) and one scope
(`verlinpark.genai`). No provider-specific or vendor endpoint is required.

### 8. Bounded attributes and content exclusions

The following are **never exported**:

- prompt, goal, objective, message text, summaries, model output;
- tool arguments, shell commands, tool results, file content, diffs;
- raw provider/tool error strings, stack traces, request/response bodies;
- API keys, tokens, headers, environment variables, URLs with credentials;
- workspace paths, branch names, commit messages.

Only whitelisted fields are read from events. `agent.started` carries a prompt
and `tool.finished` carries output, but the projection reads only the metadata
keys named above. Attribute values are control-character-sanitized and capped
at 256 characters; keys at 96; spans at 24 attributes; span names at 160
characters; one request at 1,000 spans and 8 MiB. Duplicate keys are ignored, so
one event cannot multiply attributes.

### 9. Endpoint and policy

`OtelPolicy` is project-scoped and persisted in SQLite plus the project
manifest. Older manifests and project rows decode as disabled with
`http://127.0.0.1:4318`, `verlinpark`, batch limit 200, timeout 5s.

- `enabled` defaults false. A disabled policy refuses export with
  `OTEL_DISABLED`; `telemetry.otel.test` is still allowed because it is an
  explicit synthetic connectivity check that reads no session content.
- The endpoint must be absolute `http`/`https`, loopback
  (`127.0.0.0/8`, `localhost`, or `::1`), with no credentials, query, fragment,
  or path other than empty or `/v1/traces`. Export appends `/v1/traces`.
- Public endpoints are refused with `OTEL_ENDPOINT_NOT_LOOPBACK`. v1 has **no**
  remote-endpoint opt-in. A public collector is a future decision with its own
  ADR, egress policy, credential handling, and operator consent.
- The explicit HTTP client does not follow redirects and ignores environment
  proxies. A loopback collector that returns 3xx is reported as a failed export
  rather than letting a redirect pivot the request to another host.
- `batch_limit` is 1..=1,000; `timeout_secs` is 1..=60; `service_name` is 1..=64
  non-control characters.

There is no background exporter, no automatic export after a turn, no
scheduled flush, and no retry queue. The daemon exposes only:

```text
telemetry.otel.status  { project_id, session_id? }
telemetry.otel.export  { session_id, limit? }
telemetry.otel.test    { project_id }
project.otel_policy.get / set
vp otel-status <project> [--session ID]
vp otel-policy <project>
vp otel-policy-set <project> [--enable|--disable] [--endpoint ...]
vp otel-export <session> [--limit N]
vp otel-test <project>
```

`telemetry.otel.export` requires an explicit session id, maps the bounded event
window (5,000 events), selects at most the policy/request span cap, and returns
`exported`, `span_count`, `available_spans`, `truncated`, `http_status`,
`duration_ms`, `request_bytes`, and a typed `error_code`. A transport failure
is a returned failed result, not a silent success. A 2xx response is the only
success signal; the daemon does not claim collector ingestion beyond that HTTP
status.

`telemetry.otel.status` is also bounded. It reads at most 5,000 events,
reports the projection counts by span kind, last event sequence, and warnings
when the window is truncated. It never returns a tracker, a token, or content.

### 10. Event and protocol compatibility

- All new protocol fields are `serde(default)`; older clients decode.
- OTel policy changes are project configuration, not session events, and are
  recorded in the project manifest for `db-rebuild`.
- OTel export does not append an event and is not an effect receipt. It is a
  read projection; the event log remains the source of truth.
- OTel policy is not part of the C4 `policy_snapshots` bundle. It cannot change
  a plan's contract, tools, prompt, model, or budget, so a policy edit must not
  cause a resume drift warning.
- Reliability fields are additive to the existing benchmark report schema.
  `vp bench reliability` has a separate schema and does not alter `vp bench
  run` output.

### 11. GUI

The SwiftUI Efficiency card shows a minimal read-only OTel section: enabled
state, span/event counts, endpoint and loopback hint, and the bounded warning
messages. It exposes two explicit operator actions, Export and Test collector.
The app does not store, display, or transmit prompts, tool arguments, results,
file content, or secrets. Export is disabled unless the project policy is
enabled and a session is selected. No policy editor is added in this round.

## Alternatives considered

1. **Add reliability metrics to the existing report schema only.** Rejected as
   the sole surface: pass@k needs an explicit `k`, and a saved report may be
   analyzed without re-running trials. Additive fields remain in the ordinary
   report, and the dedicated subcommand provides the explicit trial/k policy.
2. **Compute Lucky-Pass from model prose or a second LLM judge.** Rejected:
   nondeterministic, prompt-sensitive, and outside the runtime's authority. The
   chosen signals are ordering facts from events and tool metadata.
3. **Make Lucky-Pass a merge gate.** Rejected: C5 is acceptance measurement.
   A signal can be a false positive (for example a legitimate revert to fix a
   regression), and a gate would change merge semantics without a separate ADR.
4. **Export full prompt/tool content to the collector.** Rejected: that would
   create a second sensitive-data surface, break the event log's authority, and
   make redaction unverifiable. Metadata-only spans are sufficient for latency,
   token, tool, and role analysis.
5. **Use an official OTel SDK and background batch processor.** Rejected for
   v1: the SDK's default lifecycle is a background exporter, and the module must
   be explicit, bounded, daemon-owned, and easy to test with a local wiremock.
   The OTLP/HTTP JSON payload is small and stable; a future SDK addition can
   replace the transport without changing the policy or span mapping.
6. **Allow arbitrary collector endpoints behind a flag.** Rejected for v1:
   endpoint egress is a new authority surface. Loopback-only is the conservative
   default; remote export needs an explicit design for TLS trust, credentials,
   redaction policy, and operator consent.

## Consequences and limitations

- Reliability is a **descriptive** measurement. It does not prove statistical
  significance, model quality, or correctness beyond the harness acceptance and
  runtime gates.
- Fake-provider trials are deterministic and cheap, so they measure harness and
  runtime mechanics, not model reliability. Real trials require the same
  explicit triple gate as ADR-0163 and can cost money.
- Lucky-Pass heuristics are ordering signals. A legitimate revert, a
  pre-existing test run, a transport retry with no tool call, or a verifier
  checking an already-committed artifact can produce a flag without a defective
  process. The report says this explicitly.
- Test evidence is limited to commands whose structured metadata names a test
  runner. A project with an unusual test command may show no test evidence; the
  signal is then absent rather than inferred from stdout.
- The evidence hash for retry detection hashes tool metadata, not model inputs
  or file contents. Two retries with the same tool outcomes but a changed
  prompt are still "no new tool evidence"; that is the intended narrow rule.
- Coordination overhead is role/task attribution, not a work classifier. A
  Worker turn spent coordinating is still productive by this accounting.
- OTel status/export windows are bounded at 5,000 events. Older events are
  outside the report until a future paged exporter exists; `truncated` is
  visible.
- The OTel projection is metadata-only. It cannot answer "what did the model
  say?" or "what did the file contain?"; those remain in the local event log,
  tool output store, and artifacts under their existing access controls.
- There is no background exporter, retry queue, collector authentication,
  remote endpoint, or cross-host trace correlation in v1.
- Span ids are deterministic, not random; this is convenient for fixtures but
  is not an unguessable identifier. Trace data is local metadata.
- The OTel policy is project configuration, not an event. Replay/repair does
  not reconstruct it from session events; `db-rebuild` restores it from the
  project manifest.
- No gate is weakened. Export, status, and reliability never approve a request,
  bypass Seatbelt or path jails, alter write scope, skip a Verifier, enter a
  merge queue, change capacity, or spend provider budget. They add no hidden
  model routing and no automatic provider calls.

## Test coverage

- `vp-bench` unit tests: combination-based `pass@k`/`pass^k`, zero/absent
  denominators, sample variance, flake transitions, deterministic commit/test
  ordering, failed-test non-evidence, retry evidence hashing, same-path
  edit→revert, cross-path non-revert, verify/test-before-implement, apply_patch
  file inspection, coordination role/task attribution, zero-productive ratio,
  per-scenario combination, saved-report analysis with no provider call, report
  round trip, atomic/bounded artifacts, unsafe stem rejection.
- `vp-bench` end-to-end tests: ordinary fake-provider trials, report generation,
  `--from-report` analysis, existing benchmark/cache/usage tests unchanged.
- `vp-core` OTel tests: loopback/public/credential/path endpoint refusal;
  deterministic mapping of session/agent/tool/usage spans; duration, status,
  error code, attempt, retry, and cache token attributes; content sentinel
  exclusion from serialized OTLP; bounded attributes/names; repeated starts;
  disabled-by-default export; session/project ownership; wiremock OTLP export
  and synthetic test; manifest policy round trip.
- `vp-daemon` tests: OTel parameter defaults and typed Q3 error data without
  secrets.
- `vp-cli` tests: `otel-status`, `otel-policy-set`, `otel-export`, and
  `otel-test` parsing; `vp bench reliability` parsing and `--from-report`.
- Swift tests: legacy policy/status defaults, count/endpoint labels, export and
  test result decoding, loopback hint, and no prompt/content fields.
- `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`,
  `cargo test --workspace`, `swiftformat lint`, `xcodegen generate`, and
  `xcodebuild test` are the acceptance commands.

## References

- `crates/vp-bench/src/reliability.rs`
- `crates/vp-bench/src/{runner,report,cli}.rs`
- `crates/vp-core/src/otel.rs`
- `crates/vp-protocol/src/otel.rs`
- `crates/vp-store/src/store.rs`
- `apps/macos/VerlinPark/Core/OtelModels.swift`
- `apps/macos/VerlinPark/Features/OtelStatusSection.swift`
- ADR-0163 (cost-per-solved harness), ADR-0192 (cache telemetry),
  ADR-0193 (task contracts), ADR-0194 (effect receipts), ADR-0195 (external
  gates), ADR-0196 (runtime pinning and controller replay)
- AgentLens, <https://arxiv.org/abs/2607.06624>, 2026-07.
- Nature Machine Intelligence multi-agent reliability,
  <https://www.nature.com/articles/s42256-026-01268-y.pdf>, 2026-07.
- OpenTelemetry GenAI semantic conventions and OTLP/HTTP JSON.
