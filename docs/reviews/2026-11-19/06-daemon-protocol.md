# Daemon & Protocol Review — `vp-daemon` HTTP/WS/JSON-RPC, lifecycle, and `vp-protocol` wire contracts

- Date: 2026-11-19
- Baseline: `d239dca` ("Fix live provider compatibility and budget preflight")
- Scope: `crates/vp-daemon/src/` (`main.rs`, `dispatch.rs`, `dispatch/`, `http.rs`, `server.rs`, `sweepers.rs`, `errors.rs`, `params.rs`, `metrics.rs`, `backup_status.rs`) plus `crates/vp-protocol/src/` (`rpc.rs`, `discovery.rs`, wire types used by the daemon)
- Method: read-only source review; `AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/AUDIT.md`, protocol ADRs (0007, 0073, 0193–0197, 0213–0217, 0227) and the existing `docs/reviews/2026-11-19/01-core-runtime.md` were read first. Targeted runs only: `cargo test -p vp-daemon` (23 unit + 1 governance, all pass). No live provider or network calls, no edits.

## 1. Executive summary

The daemon's security core is sound. The listener is loopback-only; `/rpc` and `/metrics` require a 256-bit random Bearer token compared in constant time; the token is read before the request body is buffered; the WebSocket path requires an `auth` first frame and checks both protocol and token; discovery is written with a 0700 directory, a 0600 file, and an atomic rename; a `flock` instance lock prevents two daemons per data dir; metrics are cardinality-capped and label-escaped; and the typed error mappers for identity/workspace/memory/gate/effect/OTel/forge/diagnosis/runtime-pin are careful about not leaking content. Route-family dispatch is complete: I mechanically checked all 466 method arms against the prefix→family router and found exactly one intentional cross-family alias (`task.stalls` → Session), with no dead arms and no unknown-prefix hole. Provider-affecting methods do not bypass the engine's authorization stack at this layer.

The remaining defects are lifecycle and contract problems rather than an auth bypass:

1. The daemon's 1 s GUI grace sweeper can revoke a `gui` lease **while a browser/computer action is still running** (browser deadline 75 s, native computer helper start + action up to ~60 s, default grace 45 s, and `grace_secs` can be configured to 0). A second session can then acquire the lease and drive the same GUI concurrently — the exact invariant the lease exists to prevent.
2. A slow WebSocket subscriber silently drops events on `broadcast::RecvError::Lagged`; there is no `resync_required`/gap notification and no `since_seq` on `event.subscribe`, so the app can show permanently stale status/plan/approval state without ever knowing it missed events.
3. Two project-scoped setters bypass the project-manifest refresh path, so `vp db-rebuild` (the documented disaster-recovery path) silently reverts `context_checkpoint_policy` and `cache_policy.prompt_layout`. The allowlist-based refresh mechanism in `dispatch.rs` is structurally fragile and duplicates engine-level refreshes.
4. The WebSocket handler parses the first (pre-auth) frame before checking the token and inherits axum's 64 MiB message ceiling. Any local process that can reach the loopback port can force large allocations without the token, and the documented 8 MB message bound is not enforced (`/rpc` is not affected because auth precedes the body read).
5. `parse_params` rejects JSON-RPC requests that omit `params` (serde `Null`), so a spec-conformant third-party client gets `INVALID_PARAMS` on most methods; the WS `event.subscribe` path already special-cases this, which shows the gap is known but only patched locally.

Beyond those, the review found inconsistent auth-error shapes between HTTP and WS, an unintended coupling that disables three lease/audit sweeps when the orchestration tick is disabled, a startup ordering issue that starts schedulers before the port is validated, and an idempotency/validation error taxonomy that collapses to `INTERNAL_ERROR`. The test suite has **no** HTTP, WS, auth, discovery, or instance-lock test at all, which is why several of these paths have never been exercised.

## 2. Findings table

| ID | Severity | Confidence | Location | Issue |
|----|----------|------------|----------|-------|
| D1 | High | Confirmed | `crates/vp-daemon/src/sweepers.rs:340-352`; `crates/vp-core/src/gui.rs:709-729`; `crates/vp-core/src/browser.rs:41,1211-1219`; `crates/vp-protocol/src/types.rs:2087-2089` | Grace sweeper releases the GUI lease while a browser/computer action is in flight; a second session can drive the shared GUI concurrently |
| D2 | Medium | Confirmed | `crates/vp-daemon/src/http.rs:380-396`; `crates/vp-core/src/lib.rs:302`; `docs/ARCHITECTURE.md:233` | Lagged WS subscribers lose events silently; no `resync_required`/gap frame and no `since_seq` resume |
| D3 | Medium | Confirmed | `crates/vp-daemon/src/dispatch.rs:248-320`; `dispatch/session.rs:535-547`; `dispatch/project.rs:512-522`; `crates/vp-core/src/engine/checkpoints.rs:601-622`; `engine/usage.rs:1149-1167`; `crates/vp-core/src/rebuild.rs:390,422` | `session.context.policy.set` and `project.prompt_layout.set` never append a manifest snapshot; `db-rebuild` reverts them |
| D4 | Medium | Confirmed | `crates/vp-daemon/src/http.rs:216-220`; `main.rs:212`; `docs/ARCHITECTURE.md:169-170` | Pre-auth WS frame is parsed before the token check and may be 64 MiB (axum default); documented 8 MB bound not enforced |
| D5 | Low‑Medium | Confirmed | `crates/vp-daemon/src/params.rs:4-9`; `crates/vp-protocol/src/rpc.rs:9-14`; `http.rs:338-340` | Omitting `params` (JSON-RPC-legal) yields `INVALID_PARAMS` on ~429 methods because `Null` is not normalized to `{}` |
| D6 | Low‑Medium | Confirmed | `crates/vp-daemon/src/dispatch/session.rs:282-341,1102-1115`; `crates/vp-core/src/operation.rs:96-120` | Operation-receipt and most validation errors collapse to `INTERNAL_ERROR` with text only; replay/in-progress/conflict/unknown are indistinguishable |
| D7 | Low | Confirmed | `crates/vp-daemon/src/http.rs:166-172` vs `:222-231`; `dispatch/daemon.rs:103` | HTTP 401 is not an RPC error body while WS 401 is; HTTP `event.subscribe` returns success without subscribing |
| D8 | Low | Confirmed | `crates/vp-daemon/src/sweepers.rs:440-490` | Disabling `VERLINPARK_ORCHESTRATION_TICK_SECS` also disables runtime-lease sweep, lease-audit prune, and computer-lease sweep |
| D9 | Low | Confirmed | `crates/vp-daemon/src/main.rs:147-203` | Schedulers, auto-resume, and all sweepers start before `--port` is bound and discovery is written; a bind failure aborts after durable work started |
| D10 | Low | Confirmed | `crates/vp-daemon/src/dispatch/session.rs:70-82` | `session.create` returns success after swallowing `create_leader_agent` and `agent.start` failures |

## 3. Detailed findings

### D1 — GUI lease grace expires during an in-flight action

**Severity:** High · **Confidence:** Confirmed

**Evidence.** The daemon starts a 1 s GUI grace sweeper (`crates/vp-daemon/src/sweepers.rs:340-352`). `GuiManager::sweep` releases the holder whenever `holder.last_action.elapsed() >= holder.grace` (`crates/vp-core/src/gui.rs:709-729`, check at `:716`). `last_action` is refreshed only in `begin` (`gui.rs:391-392`, `:461-462`) and after the action completes in `finish` (`gui.rs:647-664`). There is no in-flight counter and `sweep` does not look at tickets. The default grace is 45 s (`crates/vp-protocol/src/types.rs:2087-2089`) and `GuiPolicy::normalized` has no minimum (`types.rs:2192`), so 0 is accepted.

The action itself is not bounded by 45 s:

- `browser` passes `ACTION_TIMEOUT = 75 s` (`crates/vp-core/src/browser.rs:41`), and `wait_for` is `timeout_ms` clamped to 60 s plus 15 s (`browser.rs:1211-1219`). The ticket is acquired *before* `ensure_helper` (up to 15 s ready timeout) and held until `gui_finish_action`, so a single browser action can hold the ticket for ~90 s.
- `computer` acquires at `computer.rs:392`, starts the helper (up to 20 s `READY_TIMEOUT`) at `:452`, then runs `action_deadline` at `:477` (default 15 s, screenshot 30 s, wait 30 s + 10 s) — up to ~60 s total.

`finish` already documents the consequence: "An action completed by a stale ticket is still recorded (it happened), but it does not extend a lease now owned by another session" (`gui.rs:643-646`). That is exactly this race, and it can occur with default settings, not only after a crash or operator release.

**Impact.** After 45 s the sweeper takes the in-memory holder and calls `release_holder` → `guard.release()` (`gui.rs:731-751`), deleting the SQLite lease and waking a waiter. Session B acquires a new lease/generation while session A's `computer`/`browser` action is still in flight. For the native `computer` surface, two agents can then interleave AX focus changes, clicks, and keystrokes against the same screen — the documented invariant "one logical `gui` lease serializes browser and native computer surfaces" (`AGENTS.md` invariant 9) is broken, and the failure mode is wrong-window input, not just a stale status. A project with `grace_secs = 0` (accepted) makes this immediate after the first action starts.

**Reproduction / check.** Unit level: acquire a ticket for session A (`policy.grace_secs` small), do **not** call `finish`, sleep past grace, call `engine.sweep_gui_grace()`, then acquire from session B — both tickets are live. Existing tests only sweep *after* `finish` (`gui.rs:1373-1403`, `:1477-1520`). Action level: cold helper plus `browser` `wait_for` `timeout_ms: 60000` in session A; a `computer` action in session B succeeds after ~45 s instead of returning `GUI_BUSY`.

**Suggested fix.** Add `in_flight: usize` to `GuiState`: increment on successful acquire, decrement in `finish`/error paths, and make `sweep` return 0 while `in_flight > 0`. Alternatively run a heartbeat that refreshes `last_action` every `grace/3` for the life of the ticket, and treat `finish` on a stale ticket as a visible failure for the writer that lost the lease (today it is silently accepted as a normal action record). Do not try to fix this only by clamping action deadlines below grace: grace is user-configurable down to 0.

**Test to add.** `grace_sweep_does_not_release_while_a_ticket_is_live`: acquire, sleep 2× grace, assert `sweep_gui_grace() == 0` and that a second session gets `GUI_BUSY`; then `finish`, sleep, and assert the normal release path still works. Add a daemon-level test that `spawn_gui_grace_sweeper`'s call does not release an in-flight ticket.

---

### D2 — Lagged WebSocket subscribers lose events silently

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.** The engine broadcast channel holds 2048 events (`crates/vp-core/src/lib.rs:302`). The WS loop forwards events with an awaited `send_json` (`crates/vp-daemon/src/http.rs:380-396`) and on `Err(Lagged(skipped))` only logs `warn!(skipped, …)` and continues. The client is never told it missed events, and `event.subscribe` accepts only `session_id` (`crates/vp-protocol/src/types.rs:5054-5057`), with no `since_seq` to replay from. `docs/ARCHITECTURE.md:233` specifies the intended behavior: a bounded server buffer with `resync_required` so the client re-fetches a snapshot/lists events. That frame does not exist anywhere in the repo (`resync_required` has no producer).

Because the daemon processes requests and events on the same socket and awaits each `send`, a slow reader (large transcript, paused UI, sleeping machine) fills the 2048-event window during a single send. On resume the receiver reports `Lagged` and the skipped events are gone. The macOS client keeps its live stream open on lag and has no gap detection (`AppModel` merges by seq but never checks for holes); it only reloads events on selection/reconnect.

**Impact.** Any event can be lost: `session.status`/`session.failed` (UI stuck "running"), `approval.created` (approval never shown), `plan_task.*`/`merge_queue.*` (stale plan/queue), `context.*` (stale context pill). The loss is silent and unbounded in count; the audit trail is intact, but the client has no reason to re-read it. A busy session with tool/context events can exceed 2048 events quickly.

**Reproduction / check.** Open `/ws`, auth, `event.subscribe`, then stop reading the socket (pause the client process), emit > 2048 events (or use a fake engine channel with a small capacity in a unit test), resume reading. Observed: notifications jump forward with no marker; daemon has a single `warn!`.

**Suggested fix.** On `Lagged`, send a bounded `RpcNotification::new("event.gap", json!({"skipped": n, "resume_from_seq": last_delivered + 1, "resync_required": true}))` before forwarding further events (or close with code 1011 so the existing reconnect path reloads). Add `since_seq` to `SubscribeParams` and backfill from `event.list`/`session.replay`-safe reads. Keep the existing `session_id` filter.

**Test to add.** `lagged_subscriber_receives_a_gap_notification`: broadcast > capacity with a stalled reader, then assert the first resumed frame is `event.gap` with the correct skipped count and that no event after the gap is forwarded out of order. Also a test that `event.subscribe` with `since_seq` replays from the store.

---

### D3 — Two project policy setters never refresh `projects.jsonl`

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.** The daemon appends a full `ProjectManifestSnapshot` in three situations: startup (`crates/vp-daemon/src/main.rs:143`), a hard-coded `matches!` allowlist in `dispatch` (`crates/vp-daemon/src/dispatch.rs:248-320`), and explicit engine-level `record_project_manifest[_best_effort]` calls. The manifest is the only rebuild source for project policy (sessions come from event mirrors), and the reader keeps the **last** line per project (`crates/vp-store/src/store.rs:1546-1575`), so the newest write wins. `db-rebuild` restores manifest values unconditionally, including `cache_policy` (`rebuild.rs:390`) and `context_checkpoint_policy` (`rebuild.rs:422`).

Two mutating RPCs are in neither mechanism:

1. `session.context.policy.set` (`dispatch/session.rs:535-547`) → `Engine::set_context_checkpoint_policy` (`crates/vp-core/src/engine/checkpoints.rs:601-622`), which writes the policy and returns; no manifest call, and the method is absent from the allowlist.
2. `project.prompt_layout.set` (`dispatch/project.rs:512-522`) → `Engine::set_project_prompt_layout` (`crates/vp-core/src/engine/usage.rs:1149-1167`) → `Engine::set_cache_policy` (`crates/vp-core/src/engine/projects.rs:25-46`), which also does not refresh; the method is absent from the allowlist, unlike `project.cache_policy.set`.

Both fields are present in the snapshot (`crates/vp-store/src/store/projects.rs:180,193`), so the data model assumes they are durable.

**Impact.** Operator changes made after daemon start are absent from `projects.jsonl` until the next daemon start (which rewrites the manifest) or the next allowlisted mutation (which rewrites *all* projects). `vp db-rebuild` is normally run while the daemon is stopped, so a change → stop → rebuild sequence silently reverts: context-checkpoint retention/budget policy and prompt layout/compaction behavior go back to the previous values. This is a data-loss/operational-drift bug, not a security bypass, but it contradicts the `AGENTS.md` claim that policies are rebuildable from `projects.jsonl`.

The structural issue is larger than the two methods: manifest durability depends on two independent lists (the dispatch allowlist and each engine setter) staying in sync. Most setters do refresh at the engine layer (gate, OTel, GUI, test cache, critical path, preemption, remote policies, attention policy, approval grants/templates, skills, intake, programs, session-run policy), which is why this is narrow — but a new setter that follows the `set_cache_policy` shape will drift silently.

**Reproduction / check.** Start the daemon, call `session.context.policy.set` with a distinctive value (e.g. `enabled: true`, `max_checkpoints: 7`, `max_age_days: 3`), stop the daemon, run `vp db-rebuild`, and read the policy back: the startup-time value (defaults) is restored. Same for `project.prompt_layout.set` with a non-default layout (2 or 3 vs. the current value).

**Suggested fix.** Refresh inside the engine setters (as `set_gate_policy` does at `crates/vp-core/src/gate.rs:472-481`) for `set_context_checkpoint_policy` and `set_cache_policy`, then remove the now-redundant dispatch allowlist entries (or keep the list but add both methods and a comment that every new project-scoped setter must be added). Best: centralize the refresh in the store's policy writers or add a debug assertion that a mutating RPC changed a manifest-relevant table without a new manifest line.

**Test to add.** `every_project_policy_setter_appends_a_manifest_line`: for each `*.policy.set`/`*.set` method in the allowlist and the two above, call through `dispatch` on a fixture project and assert the last `projects.jsonl` line contains the new value. At minimum `context_checkpoint_policy_and_prompt_layout_survive_db_rebuild`.

---

### D4 — Pre-auth WebSocket frame is parsed at up to 64 MiB

**Severity:** Medium · **Confidence:** Confirmed (reachability from another local account depends on port discovery)

**Evidence.** `/ws` is registered without a size layer (`crates/vp-daemon/src/main.rs:212`). `handle_socket` awaits the first frame for 10 s and immediately does `serde_json::from_str::<RpcRequest>(&text)` (`crates/vp-daemon/src/http.rs:216-220`); the token comparison happens later (`:282`). `WebSocketUpgrade` uses axum defaults because `ws_handler` never calls `max_message_size`/`max_frame_size` (`http.rs:208-213`); in axum 0.8.9 these default to 64 MiB per message and 16 MiB per frame. `docs/ARCHITECTURE.md:169-170` states the intended bound: "max 8 MB; over → disconnect/error", and also "3 failures → disconnect".

`/rpc` is **not** affected: it checks `authorized` before `to_bytes` (`http.rs:166-174`), so unauthenticated clients cannot make the daemon buffer a body. `/gate/resolve` is capped at 16 KiB (`main.rs:213-216`, `http.rs:41`).

**Impact.** The daemon listens on `127.0.0.1`, which is reachable by any local account/process that can learn the ephemeral port (listening ports are enumerable without root; the token file is not needed for this path). A pre-auth client can open many sockets and send 64 MiB text frames within the 10 s window; each is reassembled, copied into a `String`, and parsed into a `serde_json` DOM before rejection, so memory amplification is several hundred MiB per concurrent connection. This is a local DoS / stability issue rather than a confidentiality breach. It also means the documented 8 MB policy is not enforced on either transport (`MAX_RPC_BODY_BYTES = 64 MiB` at `http.rs:159`).

**Reproduction / check.** Connect to `ws://127.0.0.1:<port>/ws`, send one ~64 MiB text frame without auth; the daemon parses it and then returns `UNAUTHORIZED`. Repeat N times to observe RSS growth. Expected: close on size before parsing/auth.

**Suggested fix.** Set `.max_message_size(8 * 1024 * 1024)` and `.max_frame_size(1024 * 1024)` on the upgrade (or at minimum a small pre-auth cap), and check `text.len()` before `from_str`. Align `MAX_RPC_BODY_BYTES` with the documented 8 MB or update the doc; consider a `tower` request timeout and a connection/semaphore cap. If 64 MiB is required for bundle import, use a separate route or an explicit opt-in rather than the global RPC limit.

**Test to add.** `ws_rejects_an_oversized_frame_before_auth`: open the socket, send a frame above the cap with no auth, assert the connection closes/errors without a full parse (e.g. via a size check counter) and that a normal-size auth still succeeds.

---

### D5 — Omitting `params` is rejected on most methods

**Severity:** Low‑Medium · **Confidence:** Confirmed

**Evidence.** `RpcRequest.params` has `#[serde(default)]` and therefore deserializes an omitted field as `Value::Null` (`crates/vp-protocol/src/rpc.rs:9-18`). `parse_params` calls `serde_json::from_value(params)` directly (`crates/vp-daemon/src/params.rs:4-9`); deserializing `null` into a params struct fails (`invalid type: null, expected struct …`). Almost every RPC arm uses `parse_params` (429 arms), so a JSON-RPC 2.0 request such as `{"jsonrpc":"2.0","id":1,"method":"host.list"}` gets `INVALID_PARAMS` even though `params` is optional in the spec and most of these request structs are entirely optional fields that decode fine from `{}`. The WS subscribe path already special-cases this (`http.rs:338-340`), and `project.list` is one of the few arms that ignores params entirely.

The in-repo clients happen to send `{}` (CLI `rpc()` and the Swift `request`), so this is an interop bug for external/third-party clients and hand-written scripts, not a regression for the app.

**Impact.** Protocol-level incompatibility within major version 1; a client that follows the JSON-RPC spec cannot call most methods without a synthetic empty object. Error text is a serde message, not a hint.

**Reproduction / check.** `curl -s -H "Authorization: Bearer $TOKEN" -d '{"jsonrpc":"2.0","id":1,"method":"host.list"}' http://127.0.0.1:$PORT/rpc` → `-32602 invalid type: null, expected struct HostListParams`; adding `"params": {}` works.

**Suggested fix.** In `parse_params`, map `Value::Null` to `serde_json::json!({})` before `from_value`; add a unit test that an absent `params` decodes every params type used by no-argument methods (or at least `HostListParams`, `DashboardParams`, `ListSessionsParams`, `ProcessListParams`). Keep rejecting arrays/scalars.

**Test to add.** `absent_params_decode_as_an_empty_object` over `dispatch` for a representative set of methods, plus one negative case for a scalar `params`.

---

### D6 — Idempotency and validation failures are reported as `INTERNAL_ERROR`

**Severity:** Low‑Medium · **Confidence:** Confirmed

**Evidence.** `session.prompt`, `session.steer`, and `session.defer` route every engine error through `internal_error` (`crates/vp-daemon/src/dispatch/session.rs:282-341,1102-1115`), which is `-32603` with a message (`errors.rs:4-6`). The operation-receipt layer deliberately distinguishes four outcomes: completed replay, in-progress, conflicting payload, and crash-`unknown` (`crates/vp-core/src/operation.rs:96-120`), and `AGENTS.md` documents that clients must be able to fail closed on a differing payload and on `unknown` after restart. The errors are plain `anyhow` strings with no `data.code`, so a client can only string-match the message. The same collapse applies broadly: invalid budgets, `operation_id` with control characters, policy range errors (e.g. `critical_path.policy.set` → `internal_error` at `dispatch/team.rs:98-120`), and `session.create` validation all surface as `-32603`, while the typed-taxonomy families (identity/workspace/memory/gate/effect/OTel/forge/diagnosis/runtime-pin) correctly use `-32004`/`-32009`/`-32602` plus `data.code`.

**Impact.** A client retry policy cannot distinguish "same operation already accepted" (safe to ignore) from "different payload" (do not retry) from "unknown after restart" (investigate) — the documented fail-closed semantics are not programmatically accessible. `-32603` also suggests a server bug, which misleads operators and alerting. Conversely, `-32602` is skipped for genuine invalid input, so clients may retry bad requests.

**Reproduction / check.** Send `session.prompt` twice with the same `operation_id` but different `content`: the first succeeds, the second returns `-32603` with `"operation id … was already used … with a different method or payload"`. Send `session.prompt` with `operation_id` containing `\n`: `-32603` instead of `-32602`.

**Suggested fix.** Introduce typed errors at the operation boundary (`OperationError { IN_PROGRESS, CONFLICT, UNKNOWN }`) and a validation error type for engine setters, then map them in the daemon: `IN_PROGRESS`/`CONFLICT` → `-32009` (with `data.code` `OPERATION_IN_PROGRESS`/`OPERATION_CONFLICT`), `UNKNOWN` → `-32009` with `data.code = OPERATION_UNKNOWN` and `retryable:false`, invalid operation id/budget/policy → `-32602`. Keep `INTERNAL_ERROR` for actual store/IO failures.

**Test to add.** `operation_id_outcomes_are_typed_rpc_errors`: replay, differing payload, and a simulated crash-`unknown` row each assert the stable `data.code` and the correct JSON-RPC code.

---

### D7 — Auth errors have two shapes; HTTP `event.subscribe` is a silent no-op

**Severity:** Low · **Confidence:** Confirmed

**Evidence.** `/rpc` returns HTTP 401 with `{"error":"unauthorized"}` (`http.rs:166-172`), which is not an `RpcResponse` (no `jsonrpc`, no numeric `code`). The WS path returns a proper `RpcResponse` with `codes::UNAUTHORIZED` (`http.rs:222-231`, `:282-292`). A JSON-RPC client that ignores HTTP status (the CLI does check status, but a generic client may not) cannot decode the HTTP body as an RPC error. Separately, `dispatch` answers `event.subscribe` with `{"subscribed": true}` over HTTP (`dispatch/daemon.rs:103`), although only the WS loop implements subscription (`http.rs:337-361`); an HTTP client is told it subscribed and receives nothing. `docs/ARCHITECTURE.md` §4.3 also promises snapshot + `since_seq` replay on subscribe, which neither transport implements.

**Impact.** Client libraries must special-case the HTTP error shape; a caller that uses HTTP for `event.subscribe` (or an HTTP-only transport in the future) silently gets no events. This is a contract/consistency bug, not a security hole.

**Suggested fix.** Return `Json(RpcResponse::error(None, RpcError::new(codes::UNAUTHORIZED, "unauthorized")))` with the 401 status (optionally also `WWW-Authenticate: Bearer`), and make the HTTP `event.subscribe` arm return `METHOD_NOT_FOUND`/an explicit `UNSUPPORTED_TRANSPORT` error instead of success. If §4.3 is still the target, implement `since_seq` + snapshot rather than dropping it.

**Test to add.** `http_auth_failure_uses_the_rpc_error_shape` and `http_event_subscribe_does_not_claim_success`.

---

### D8 — Disabling orchestration also disables three lease/audit sweeps

**Severity:** Low · **Confidence:** Confirmed

**Evidence.** `spawn_orchestration_controller` returns immediately when `VERLINPARK_ORCHESTRATION_TICK_SECS=0` (`crates/vp-daemon/src/sweepers.rs:440-447`), but the same task also runs `engine.sweep_runtime_leases()`, `engine.prune_runtime_lease_audit(...)`, and `engine.sweep_computer_leases()` (`:475-487`). There is no other automatic caller (`runtime.leases.sweep` and `computer.leases.sweep` are manual RPCs).

**Impact.** With the orchestration controller disabled, stale runtime leases stay visible in `runtime.leases`/dashboards, `runtime_lease_audit` grows without pruning, and stale `computer` lease rows persist. Session/plan acquisition is still safe because `acquire_runtime_lease_at` treats expired rows as free (`crates/vp-store/src/store/lease.rs:602-610`), so this is an observability/retention bug, not a lockout. The coupling is surprising: a scheduling opt-out silently changes lease hygiene.

**Suggested fix.** Move the three sweeps into independent `spawn_*` functions with their own intervals/env vars (or reuse a dedicated lease sweeper), so the orchestration tick only controls orchestration.

**Test to add.** `disabling_orchestration_keeps_lease_sweeps_active` (config matrix or a unit test on the spawn decision), or at least a test that `prune_runtime_lease_audit` is not reachable only through the orchestration task.

---

### D9 — Durable work starts before the listener is bound

**Severity:** Low · **Confidence:** Confirmed

**Evidence.** `run()` recovers sessions, writes the manifest, re-enqueues queued sessions, auto-resumes interrupted runs, starts the auto-resolver, task scheduler, and scaling controller, and spawns 21 sweepers *before* `TcpListener::bind(("127.0.0.1", port))` at `crates/vp-daemon/src/main.rs:147-193`. Discovery is written only after the bind (`:203`). If `--port` is occupied (two data dirs, or an unrelated process), `bind` returns an error and the daemon exits after the scheduler may already have dispatched an intake task and started a Worker/provider call; the aborted run is only repaired on the next start. A startup failure after the bind but before `write_discovery` (e.g. permission error) leaves the previous/stale `daemon.json` in place, so clients keep dialing a dead port.

**Impact.** Infrequent, but the failure mode is a half-started daemon that spent tokens and mutated durable state before failing to accept a single request. It also means `--port` validation is not fail-fast.

**Suggested fix.** Bind the listener (and, if possible, write discovery) before starting background work and before `resume_queued_sessions`/`auto_resume_interrupted_runs`; on any later startup error, remove the discovery file before exiting. Keep recovery/manifest work where it is (it is safe before a listener).

**Test to add.** `bind_failure_starts_no_scheduler_work_and_leaves_no_discovery`: occupy the port, run `run()` (or an extracted `start` helper), assert a typed bind error, no new agent runs/intake claims, and no stale discovery write for the new pid.

---

### D10 — `session.create` reports success when the agent cannot start

**Severity:** Low · **Confidence:** Confirmed

**Evidence.** After creating the session, the handler ignores `create_leader_agent` (`let _ = …`, `dispatch/session.rs:77`) and only logs `agent.start` failures (`:78-80`), then returns the `Session`. `agent.start` is the same call that returns a typed-ish error for `session.prompt` (`dispatch/session.rs:1102-1114`). When no provider is configured, `start_inner` appends an `agent.unavailable` event and returns an error (`crates/vp-core/src/agent/service_run/start.rs:67-80`), so the failure is visible in the transcript, but the RPC caller sees a successful creation with no indication the run did not start.

**Impact.** `vp session-new`/the app can report success for a session that will sit in `draft` and never run; the user must discover the `agent.unavailable` event. This is inconsistent with `session.prompt`, which returns the error. The ignored `create_leader_agent` error can also leave a session without a Leader roster row and no signal.

**Suggested fix.** Return the created session plus a bounded `start_error`/`warning` field (additive to the protocol), or fail the RPC with the agent error while keeping the created session (the CLI can then point at the transcript). At minimum, propagate `create_leader_agent` failures and include the error in the response.

**Test to add.** `session_create_without_provider_reports_the_start_failure`: unconfigured `AgentService`; assert the RPC error (or a `start_error` field), an `agent.unavailable` event, and that the session exists once.

## 4. Additional hardening observations (not ranked)

- **`write_discovery` permissions window.** `std::fs::write` creates `daemon.json.tmp` with the process umask (`0600` is applied afterwards at `server.rs:83-84`), then renames. The `run/` directory is `0700` (`ensure_private_dir`), so another user cannot traverse into it; keep the current mitigation but consider `OpenOptions::mode(0o600)` + `fsync` for the file and the directory. No `fsync` means a crash can leave a zero-length/corrupt discovery file; clients then fail with a serde error rather than a "stale daemon" message.
- **`remove_discovery` is unconditional** (`server.rs:89-91`). It is safe today because the instance lock is held until after `run()` returns, but it does not verify pid/port/token before deleting. If the lock acquisition order ever changes, a late-exiting daemon could delete a replacement's discovery file.
- **`RpcResponse::success` / `RpcNotification::new` use `serde_json::to_value(...).unwrap_or(Value::Null)`** (`crates/vp-protocol/src/rpc.rs:75,102`). A result that fails to serialize becomes a successful `null` (or a notification with `params: null`) instead of an `INTERNAL_ERROR`; no current result should hit it, but the failure mode is silent.
- **`gate_error` maps `GATE_TOKEN_INVALID` to `CONFLICT` (`-32009`)** over `/rpc` (`errors.rs:71-89`), while the HTTP loopback endpoint maps the same condition to 401 (`http.rs:76-86`). An invalid credential is not a conflict; clients may treat `-32009` as retryable. A dedicated `UNAUTHORIZED`/`PERMISSION_DENIED` mapping would be clearer.
- **`authorized` only accepts the exact `"Bearer "` spelling** (`server.rs:29-35`). RFC 7235 schemes are case-insensitive; this is fine for the bundled clients but worth normalizing.
- **No `Host`/`Origin` validation** on `/rpc`, `/ws`, `/metrics`, or `/gate/resolve`. I found no token bypass (the bearer header forces a CORS preflight, and WS auth requires the token in the frame), so this is defense-in-depth against DNS rebinding and future browser clients, not a live vulnerability. Rejecting unknown `Host` values on loopback is cheap.
- **No request timeout or per-connection concurrency cap on `/rpc`**, and no cap on answered WebSocket connections. An authenticated local client can create many concurrent `spawn_blocking` snapshots (`/metrics`) or slow bodies. Local-only, but a single misbehaving script can saturate the blocking pool.
- **Documented lifecycle events are absent.** `docs/ARCHITECTURE.md:129,133` specify `daemon.started`/`daemon.stopped` events and a graceful shutdown that cancels agents/waits for tool process groups; `main.rs` only stops accepting and returns, relying on drop (`kill_on_drop` for shell, `Drop` for consoles) to clean up. Durable supervised processes intentionally survive. The docs should either be updated or the drain path implemented; as written, the doc overstates shutdown guarantees.
- **`/health` is unauthenticated** and reports the version/protocol to any local user. Expected for a health check, but note it as version disclosure in a multi-user threat model.
- **`Discovery` derives `Debug` with the token in cleartext** (`crates/vp-protocol/src/discovery.rs:6-14`). No current call site logs it, but `GateToken` already demonstrates the redacting-`Debug` pattern; worth copying.
- **`spawn_gui_timeline_retention_sweeper` treats `0` as "use default 6 h"**, unlike the other sweepers where `0` disables (`sweepers.rs:30-40`). Minor config inconsistency.
- **`spawn_coordination_sweeper` (30 s) and `spawn_gui_grace_sweeper` (1 s) have no env opt-out**, unlike the other periodic tasks; in tests/CI they cannot be turned off.
- **`session.create` writes the session, then the leader agent, then starts the run** — three non-atomic steps. D10 covers the visible symptom; a stronger fix is a single engine call that creates the roster and starts the run with compensating status on failure.

## 5. Test gaps

`crates/vp-daemon/src/tests.rs` (23 tests) and `crates/vp-daemon/tests/module_governance.rs` (1 test) contain **no** reference to `authorized`, `constant_time_eq`, `generate_token`, `write_discovery`, `acquire_instance_lock`, `rpc_http`, `metrics_http`, `health`, `ws_handler`, or `handle_socket`. `crates/vp-protocol/src/rpc.rs` has no unit tests. Specifically missing:

1. **Auth/transport:** missing header, wrong token, `Bearer` with wrong case, multiple headers, 401 body shape, 64 MiB body rejection, invalid UTF-8, malformed JSON, batch array rejection, notification (no `id`) behavior.
2. **WebSocket:** first-frame must be `auth`; wrong token; protocol mismatch; `event.subscribe` filtering; multiple sessions on one socket; ping/close; slow-reader `Lagged` (D2); oversized pre-auth frame (D4); socket dropped mid-request.
3. **Discovery/lifecycle:** file mode 0600, directory 0700, atomic replace, remove on shutdown, stale file on startup failure, second instance lock rejection, discovery contents (pid/port/token round-trip).
4. **Dispatch coverage:** a generated test that every RPC method string advertised by the CLI/Swift (or every `Command`) resolves to a family and an arm, and that unknown prefixes return `-32601`. The alias map (`task.stalls`/`session.stalls`, `workspace.*`) should be asserted explicitly.
5. **Params:** absent/`null` params (D5), scalar params, unknown fields tolerated, oversized string fields.
6. **Manifest durability:** D3's per-setter manifest assertion, plus `db-rebuild` round-trip for `context_checkpoint_policy` and `prompt_layout`.
7. **Sweepers:** env parsing (`0` disable, clamps, malformed values), first-tick timing, orchestration-off lease sweeps (D8), GUI grace with an in-flight ticket (D1), retention/prune idempotency.
8. **Error mapping:** operation receipt outcomes (D6), `gate_resolve_http` status mapping across all `GATE_*` codes, typed-family fallbacks (`internal_error` with `data: None`), and `RpcResponse::success` serialization failure.
9. **Startup ordering:** bind failure leaves no discovery and no started work (D9); startup recovery with a stale `daemon.json` from a dead pid.

## 6. Clean areas (verified, no finding)

- **Loopback-only binding:** the only daemon `TcpListener::bind` is `("127.0.0.1", …)` (`main.rs:193`); no `0.0.0.0`/`::` path.
- **Token generation:** 32 random bytes from `rand::rng()` (a CSPRNG) encoded base64url, 43 chars; no token appears in logs, `/health`, `server.info`, metrics, or error text.
- **Auth ordering on HTTP:** `/rpc` and `/metrics` check the bearer before reading a body or taking a DB snapshot; `/gate/resolve` is capped at 16 KiB and gated per-project by the stored gate policy.
- **Constant-time comparison:** length-mismatch early return is the only timing signal and the token length is fixed/public; the byte fold is correct for ASCII. `GateToken`/`verify_gate_token` use the same pattern over SHA-256.
- **Discovery permissions/lifecycle:** `run/` and the data dir are forced to 0700, `daemon.json` to 0600, and the write is `write tmp → rename`; the `flock` instance lock is acquired before `Store::open` and held for the process lifetime.
- **Route-family coverage:** all 466 method arms were checked against the prefix→family map; only `task.stalls` is intentionally cross-family and is special-cased (`dispatch.rs:201-206`); every family ends with a `METHOD_NOT_FOUND` fallback.
- **Metrics:** fixed metric names, status/kind-only labels sorted deterministically, `escape_label` handles `\`, `"`, `\n`, `\r`, non-finite/negative clamping, no ids/paths/models/secrets, and DB-side cardinality caps.
- **Typed error mappers:** identity/workspace/memory/gate/fanout/diagnosis/runtime-pin/OTel/forge/remote/archive-media all preserve stable `data.code` and avoid echoing prompts/tool output/credentials; untyped failures stay `INTERNAL_ERROR` with no `data`.
- **Gate loopback result:** the one-time token is returned only from `gate.open`; `GateResolveResult` contains no token, and the loopback endpoint maps policy-disabled/expired/cancelled/conflict to distinct statuses.
- **Backup status:** reports public key material, paths, anchor generations, and revocation counts only; tests assert no `seed_b64`/`private_key`/`passphrase`, and `keyring`/anchor paths are user-visible by design.
- **Graceful shutdown vs WebSockets:** axum/hyper resolves an upgraded connection future on upgrade, so `with_graceful_shutdown` does not hang on live WS sockets (verified against `hyper-1.12.0` `UpgradeableConnection::poll`).
- **Manifest refresh coverage is mostly correct:** engine-level refreshes cover gate, OTel, GUI, test cache, critical-path, preemption, remote test/integration, attention policy, approval grants/templates/audit, skills, intake, programs, session-run policy, and most identity/memory/workspace mutations; D3 is the narrow exception.
- **Existing test hygiene:** the process/archive/backup/metrics/preemption/forge tests use real `TempDir` stores and assert bounded, secret-free payloads; the daemon module-governance test keeps the split modules bounded.

## 7. Assumptions and limits

- **Threat model:** single-user macOS daemon; the discovery file and its token are readable only by the owning user; `127.0.0.1` is reachable by other local accounts for D4's pre-auth path (port discoverable, token not).
- **axum 0.8.9 defaults:** 64 MiB WS message / 16 MiB frame ceilings were confirmed from the vendored crate source; a future axum upgrade may change them, so an explicit limit is still the right fix.
- **serde behavior:** `serde_json::from_value::<Struct>(Value::Null)` errors for derived structs; D5 is based on that standard behavior plus the code path, not a live request.
- **No live network/provider calls** were made; `cargo test -p vp-daemon` was the only executed test command. Findings about provider calls in D9/D10 are inferred from the start path, not observed.
- **Not re-reviewed here:** engine authorization/approval semantics, Verifier/Integrator correctness, merge/land/fork flows, `vp-a2a` (separate crate, own bearer token), backup crypto, the Swift client's reconnect logic, and the `vp-cli` transport beyond what was needed to assess wire compatibility. Those are covered by the other 2026-11-19 reviews or are out of scope.
- **D8 impact** is scoped by reading `acquire_runtime_lease_at` (`crates/vp-store/src/store/lease.rs:602-610`), which reclaims expired rows; it is an observability/retention issue, not a session lockout.
