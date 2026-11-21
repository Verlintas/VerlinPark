# 0230 — Reliability hardening round

- Status: Accepted
- Date: 2026-11-19
- Scope: authorization, ownership, journal replay, plan/task state, replay/repair, store integrity, daemon transport, provider completion, CLI policy updates, GUI lease lifetime, Swift reconnect

## Context

A ten-reviewer read-only audit of baseline `d239dca` found 109 findings across
the runtime, store, daemon, CLI, providers/bench, Swift app, and release
surface. Several were confirmed correctness defects rather than missing
features:

- a multi-call read-only tool batch bypassed the persisted approval broker;
- artifact/source/MCP-task ids were trusted across sessions/projects;
- a completed journaled write could be re-executed after two resumes;
- plan batch aborts could overwrite an already completed/merged sibling;
- a second `submit_team_plan` reset the task projection;
- `team_plan.revised` was replayed as if the wrapper were the plan;
- project manifests could be rebuilt from a silently truncated stream;
- provider `finish_reason`/`stop_reason` truncation was treated as success;
- CLI policy updates replaced unspecified security fields;
- the GUI lease could be released while a browser/computer action was running;
- Swift reconnect could stop live updates or miss approvals.

The fixes keep the existing authority model: the daemon owns state, approvals
and contracts remain fail-closed, Verifier/Integrator gates are unchanged, and
no hidden model or provider routing was added.

## Decision

### Authorization and ownership

- A parallel-safe read-only batch first runs a non-consuming approval preview.
  Only an all-`Allow` preview may take the fast path. Before execution, every
  call records its real contract `intent.decision` and the persisted broker
  decision exactly once; a concurrent policy change fails closed.
- Tool-visible artifact, source, and MCP-task ids are resolved with the calling
  session/project. Foreign ids are indistinguishable from missing ids.
- Default shell read-denial roots include daemon runtime state (tool outputs,
  process logs, verification clones, archive/media scratch, sibling worktrees,
  daemon temp/import scratch, backup keys). The agent's own worktree, scratch,
  and Git metadata are re-allowed after the denies.

### Durable execution

- The journal duplicate-write gate no longer consumes the source row during
  lookup. A replay is journaled as its own completed row, so a failure after
  the replay replays again instead of re-executing the write.
- Provider tool arguments that are not a JSON object become a typed
  `INVALID_ARGUMENT` tool result; they are never executed with guessed `{}`
  input. An assistant turn with neither text nor tool calls is a typed
  provider failure, not a successful empty answer.
- A plan-task batch evaluates every sibling result before synthesizing abort
  status. Completed/skipped/cancelled/failed siblings are terminal, and a
  `plan_task.finished` event is emitted only when the guarded store transition
  actually applied.
- `submit_team_plan` is create-only for the Leader tool. A second submission
  returns `PLAN_REVISION_REQUIRED`; task rows and attempts survive. Revisions
  use an expected-revision compare-and-set inside a `BEGIN IMMEDIATE`
  transaction.
- The scheduler, controller, and plan runner share one
  `dependency_satisfied = completed | skipped` predicate. Gate-deferred tasks
  are not treated as stale blocked work.
- `deferred` is a terminal run-journal state, and resumed runs distinguish it
  from `running`.
- Replay unwraps `team_plan.revised`, rebuilds the DAG from submitted plus
  revisions, folds only real merge-queue mutation events, and treats progress
  drift as inconsistency so repair can fix it.
- Archive bundles carry contracts, contract usage, progress rounds, and remote
  test evidence; import preserves retry backoff and remains read-only.

### Store and transport

- Project manifests are version 2. A complete replay stream is required;
  explicit truncation, malformed lines, and legacy capped streams fail closed
  instead of rebuilding stale state. Appends use a temporary file, `sync_all`,
  and atomic rename.
- Session creation and its `session.created` event commit in one transaction;
  fork creation preserves lineage in the same transaction.
- Bundle event sequences are bounded to `1..=i64::MAX` and payloads are capped.
- Orchestration-policy decode errors fail closed. Instruction-session prefix
  matching is exact and case-sensitive. Memory LIKE fallbacks escape `%`/`_`.
- WebSocket frames are bounded (8 MiB authenticated, 64 KiB pre-auth parse),
  omitted/null `params` decode as `{}`, and HTTP 401 uses the JSON-RPC error
  shape. Lagged subscribers receive `event.gap` with the last delivered seq;
  `event.subscribe {session_id, since_seq}` performs a bounded replay.
- The GUI coordinator tracks the active action ticket. Grace reclamation and
  the sweeper cannot release a lease while an action is in flight; the SQLite
  lease TTL bounds an abandoned ticket. The policy grace is not silently
  raised or rewritten.

### Provider, CLI, and client

- OpenAI `finish_reason` and Anthropic `stop_reason` complete only on normal
  reasons. `length`, `max_tokens`, `content_filter`, `refusal`, and unknown
  reasons fail the turn; buffered tool calls are not emitted. Usage is
  reconciled as additive deltas, and abnormal turns preserve billed usage.
- Anthropic 1h cache markers send the documented extended-cache beta header;
  parallel tool results become one user message in `tool_use` order.
- CLI policy updates read the stored policy and patch only explicit flags.
  Approval-template enabled state is preserved unless explicitly set.
  Destructive commands require confirmation or `--yes`; `--file -` reads
  stdin; RPC timeouts are configurable and typed error codes are preserved.
- The Swift client buffers events until a consumer attaches, binds stream and
  receive-loop termination to a connection generation, handles `event.gap`,
  uses per-operation RPC timeouts/cancellation/operation ids, clears the daemon
  provider only through an acknowledged reconfiguration, treats partial
  `SESSION_START_FAILED` as a created session, and limits notification approval
  actions to routine workspace writes.

### Tool schema and policy alignment

- Archive/media schemas are filtered by the same role policy as executable
  tools: Managers and Verifiers no longer see `archive_extract` or
  `media_transcode`, while Scout receives only the read-only `archive_list` and
  `media_info` views.
- The policy surface no longer advertises `delegate_many` or `submit_verdict`
  to the Leader, `delegate_acp` to Workers, or `ask_user`/`coordinate`/`remember`
  to Verifiers. Manager/Verifier MCP inspect/read access is included in the
  surface explicitly.
- A regression test asserts the advertised schema and policy surface do not
  drift for these role-specific tools.

## Consequences

- The approval, ownership, journal, plan, replay, and truncation paths now fail
  closed and have regression tests.
- Project manifests are larger because complete event streams are stored.
  Rebuild rejects an old capped manifest instead of silently losing events.
- `StreamEvent::Usage` is explicitly additive per stream; adapters reconcile
  cumulative provider snapshots before emitting deltas.
- `submit_team_plan` is no longer a silent "replace the plan" operation. A
  second submission is rejected; changing a plan requires the explicit
  operator revision path (the Leader tool surface currently has no general
  revision tool).
- A GUI action is protected for its full bounded action window without
  imposing a long grace on every action. An abandoned in-memory ticket is
  bounded by the existing 120s lease TTL.
- The host code-signing queue can delay newly linked test binaries; compile
  checks and previously warmed binaries are used when execution is blocked.

## Remaining explicit gaps

- `fanout_plan` still derives its expected revision from a bounded recent-event
  window; the store compare-and-set fails such a stale fan-out closed, but the
  typed `FANOUT_CONFLICT` mapping is a follow-up.
- The Leader tool surface has no general `team_plan.revise` tool; a second
  `submit_team_plan` is rejected and the operator must apply a revision through
  RPC/CLI. A bounded in-run revision tool remains a follow-up.
- Store merge-queue/plan terminal guards are row-count based but `plan_task`
  terminal protection is not a full expected-status API.
- Task-worktree retention is bounded and opt-in; it is not a complete
  retention/GC subsystem.
- The GUI in-flight marker is in-memory and single-ticket; same-session reentry
  can overwrite an older ticket, and abandonment is bounded by the lease TTL.
- Provider abnormal finishes use `ProviderError::Stream`; a zero-output
  abnormal finish can still consume the existing bounded stream retry budget.
- The Swift client has no global event sequence, so cross-session backfill is
  still projection-based; per-session tail backfill is bounded.
- This round did not attempt the lower-priority findings (daemon test-client
  coverage, full workspace secret redaction, release signing/notarization,
  archive formats, or cross-host scheduling).

## Verification

- `cargo fmt --all -- --check`, `cargo check --workspace --all-targets`, and
  `cargo clippy --workspace --all-targets -- -D warnings` pass.
- `cargo test -p vp-core --lib`: 2,480 passed / 0 failed / 2 ignored.
- `cargo test -p vp-store --lib`: 418 passed / 0 failed.
- `cargo test -p vp-cli`: 162 unit + 10 backup integration + 1 governance.
- `cargo test -p vp-daemon`: 42 unit + 1 governance.
- `cargo test -p vp-providers`: 159 lib + 27 append-only; 3 live tests ignored.
- `cargo test -p vp-bench --lib`: 146 passed; harness: 11 passed / 1 ignored.
- `vp-protocol`, `vp-remote`, `vp-a2a`, and `vp-remote-agent` suites pass.
- `xcodebuild test`: 430 app tests + 132 helper tests passed.
- Live DeepSeek `deepseek-flash` smoke (key only in the environment):
  provider stream 53 chunks / 65 input / 281 output; small cache-bench 95.5471%
  (marked `unrealistic`, not a long-prefix acceptance claim); full Leader →
  `write_file` E2E 4 calls / 48,072 input / 318 output / 46,976 cache-read /
  **97.7201%** hit ratio, session idle, exact file content.
- Ten review reports and the consolidated report are in
  `docs/reviews/2026-11-19/`.
- Detailed per-workstream results and the remaining gaps are recorded in the
  consolidated report. The host's code-signing queue can delay newly linked
  binaries; the numbers above are from the final frozen tree.

## Alternatives considered

- Keep the 105s GUI grace floor: rejected because it imposes a long post-action
  wait on every session and hides the real in-flight state.
- Silently truncate project manifests: rejected because `db-rebuild` would
  treat stale state as authoritative.
- Auto-retry abnormal provider finishes: rejected because it can double-bill
  and can execute a truncated tool call.
- Treat omitted parameters as invalid: rejected because JSON-RPC 2.0 permits
  omitting `params`.
