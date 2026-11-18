# Security, Tests, and CI Review — cross-cutting boundaries and release reliability

- Date: 2026-11-19
- Baseline: `d239dca` ("Fix live provider compatibility and budget preflight")
- Scope: shell/Seatbelt/path jail/HOME/URL/MCP/ACP/GUI/remote boundaries; secret handling; approval/contract/effect/gate bypasses; archive/PDF/media/LSP/process/browser/computer helpers; dependency/CI/release scripts; security test gaps; docs overclaims.
- Method: read-only source review. Read `AGENTS.md`, `docs/ARCHITECTURE.md`, `docs/AUDIT.md`, the ADR index and relevant ADRs (0035, 0090, 0093, 0113, 0137, 0156, 0165, 0167, 0181, 0193–0198, 0213–0219), `justfile`, `.github/workflows/ci.yml`, `.github/workflows/release.yml`, and `scripts/`. No source, test, config, or doc changes; no commit. Only local, read-only commands were run (no destructive/live-network operations). The full test suite was not run for this review.

## 1. Executive summary

Most low-level boundaries are implemented with care: the daemon binds only `127.0.0.1`, auth is constant-time and the discovery file is `0600`; file tools use a canonical path jail with symlink and `.git` refusal; shell commands fail closed when `sandbox-exec` is unavailable, run with a cleared environment and process-group cleanup; archive extraction is bounded and rejects traversal/links; MCP pins gate tool execution independently of contract mode; gates store only token hashes; backup signing/keyring and remote-helper token/path-jail code are unusually complete. Sequential tool dispatch and Code Mode share the same authorization/effect helpers.

The remaining security risk is concentrated in **fast paths and identifier lookups**, not in the primary dispatch path:

1. The parallel read-only batch still uses the stateless `permissions::decide()` and never consults project approval-template evaluation or records `intent.decision`. A project deny template can be bypassed by issuing two read-only calls in one turn. This is a direct approval-policy bypass and is the highest-severity finding. (The core-runtime review covers the same fast path; this report states the security consequence independently.)
2. Agent-facing tools that accept an `artifact_id`, `source_id`, or MCP `task_id` resolve it globally. There is no session/project ownership check, so a known identifier permits cross-session artifact overwrite or cross-project reads; artifact updates and archive/media extraction also append events to the victim session.
3. Shell's read-denial list covers the database, run directory, event mirrors, logs, and artifacts, but not `tool-outputs`, `processes`, `verify`, `sandbox`, `archive-extracts`, `media-transcode`, or sibling `worktrees`. With default `ShellPolicy` (`restrict_home=false`, `read_jail=false`), a shell in one session can read another session's spilled tool outputs and process logs, bypassing the session-scoped `read_tool_output` and `process logs` APIs.
4. Raw tool inputs and outputs, including HTTP headers, URL query strings, file contents, and MCP arguments/results, are persisted in `tool.started`/`tool.finished` events and approval rows without redaction. They therefore appear in `session.search`, exported bundles, event mirrors, and backups. Two source comments claim the opposite ("never echoed"/"never copied into an event").
5. macOS notification actions let an operator approve or deny a pending approval from a notification banner (and potentially a lock-screen surface) that shows only the goal, tool, and agent. It does not show the command/path, risk, or details, and there is no risk gate on the action. A Destructive/HostState approval can be granted blind.
6. A caller-declared shell `effect_probe` can settle an `external` shell effect as `done` using only a local file/git observation. A Worker can append a local marker to an external command and mark an external side effect as settled; this is an intentional local-evidence design, but it weakens the `unknown_blocks_merge` gate.
7. ACP extra headers allow literal secrets for non-`authorization` names (for example `X-Api-Key`); those values are stored in SQLite/list surfaces, whereas MCP requires `${ENV_VAR}` for a sensitive-name list.
8. Remote ACP accepts cleartext `http://`/`ws://` for any host. The ADR says `ws` is accepted for localhost/development endpoints, but the implementation does not enforce the loopback restriction; an operator can send a bearer token and MCP headers in clear over a LAN.
9. Invalid `VERLINPARK_APPROVAL_MODE` values silently fall back to `smart`; a typo in `unattended` or `strict` downgrades the intended policy without an error.
10. The release workflow publishes on tag without running tests/lint; `package-release.sh` copies an unsigned `vp` CLI into the DMG, and CI/release actions are pinned to mutable major tags with no dependency audit.

No code changes are recommended in this review document; the fixes below are suggestions for the owning areas.

## 2. Findings table

| ID | Severity | Confidence | Location | Issue |
|----|----------|------------|----------|-------|
| S1 | High | Confirmed | `crates/vp-core/src/agent/run_loop/body.rs:1620-1682`; `crates/vp-core/src/engine/approvals.rs:67-128`; `crates/vp-core/src/agent/dispatch.rs:256-298` | Parallel read-only batch bypasses project approval-template evaluation (including deny) and `intent.decision`; a deny template becomes an allow |
| S2 | High | Confirmed | `crates/vp-core/src/agent/tools/integrations.rs:1179`, `:1240`; `crates/vp-core/src/agent/tools/artifacts.rs:229`, `:303`; `crates/vp-core/src/archive_media.rs:364`, `:403`, `:497`, `:557`, `:619`, `:692`; `crates/vp-core/src/mcp/tasks.rs:405`, `:428`, `:471`; `crates/vp-core/src/engine/sources/retrieval.rs:27` | Agent tools trust caller-supplied artifact/source/MCP-task IDs globally; cross-session artifact update and cross-project reads are possible |
| S3 | Medium‑High | Confirmed | `crates/vp-core/src/agent.rs:2311-2323`; `crates/vp-store/src/store/tool_outputs.rs:6-21`; `crates/vp-core/src/process_supervisor.rs:234-302`; `crates/vp-core/src/worktree.rs:32`, `:743`; `crates/vp-core/src/archive_media.rs:410`, `:634`; `crates/vp-protocol/src/types.rs:35-43` | Shell secret-root denylist omits daemon state dirs; default shell can read other sessions' tool outputs/process logs and worktrees |
| S4 | Medium | Confirmed | `crates/vp-core/src/agent/run_loop/tool_calls.rs:1737-1747`; `crates/vp-core/src/agent/dispatch.rs:143-149`; `crates/vp-core/src/engine/approvals.rs:214-220`; `crates/vp-store/src/store/events.rs:30-45`, `:1002-1006`; `crates/vp-core/src/backup.rs:89-95`; `crates/vp-core/src/tools.rs:543-556`, `:916-935` | Secrets in tool inputs/outputs are persisted in events, approvals, search, export, and backup; comments claim they are never echoed |
| S5 | Medium | Confirmed | `apps/macos/VerlinPark/Core/SystemNotifier.swift:30-48`, `:62-67`, `:91-98`; `apps/macos/VerlinPark/Features/AppModel.swift:10315-10329` | Approve/Deny notification actions resolve approvals without showing command/path/risk; no risk gating |
| S6 | Low‑Medium | Confirmed | `crates/vp-core/src/effect.rs:654-687`, `:838`, `:1287-1405`; `docs/adr/0194-effect-receipts-and-postconditions.md` | Declared shell `effect_probe` can settle an external effect as `done` using a local marker; the deliberate local-evidence design weakens `unknown_blocks_merge` |
| S7 | Low‑Medium | Confirmed | `crates/vp-core/src/acp/config.rs:35-58`, `:281-321`; `crates/vp-protocol/src/types.rs:6221-6224`; contrast `crates/vp-core/src/mcp.rs:47-56`, `:281-293` | ACP extra headers allow literal credentials for non-`authorization` names and store them in SQLite; MCP requires env indirection for its sensitive list |
| S8 | Low‑Medium | Confirmed | `crates/vp-core/src/acp/config.rs:186-240`; `docs/adr/0165-acp-http-and-websocket-transport.md:155` | Cleartext `ws://`/`http://` accepted for non-loopback remote ACP endpoints despite the ADR's localhost/development wording |
| S9 | Low‑Medium | Confirmed | `crates/vp-core/src/agent/service/constructors.rs:76-79`, `:191-193`; `crates/vp-protocol/src/types.rs:237-244` | Invalid `VERLINPARK_APPROVAL_MODE` silently falls back to `smart` instead of failing closed |
| S10 | Medium | Confirmed | `.github/workflows/release.yml:68`; `scripts/package-release.sh:34`, `:64-76`, `:98-100`; `.github/workflows/ci.yml:13-19`, `:26-28` | Release publishes untested artifacts; `bin/vp` is copied unsigned/unnotarized; CI/release actions are not SHA-pinned and no dependency audit runs |

## 3. Detailed findings

### S1 — Parallel read-only batch bypasses project deny templates and intent audit

**Severity:** High · **Confidence:** Confirmed

**Evidence.** The parallel fast path in `run_loop_body` is taken when a turn contains two or more calls that are all in `is_parallel_safe_tool` (`body.rs:1620-1629`; list at `agent/dispatch.rs:1068-1090`). This is the same fast path reported in `docs/reviews/2026-11-19/01-core-runtime.md` R1; the security consequence is restated here because it is a cross-cutting approval bypass. It prepares each call with the stateless `assess_tool_call_with_gui` (`body.rs:1643-1648`) and its own `decide(config.approval_mode, &assessment, spec.auto_approve)` (`body.rs:1670-1673`). It does not call `Engine::evaluate_tool_permission` (`engine/approvals.rs:67-128`), which is where project `ApprovalTemplate`s, `ApprovalGrant`s, and the `approval.template.matched` audit are applied, and it does not call `Engine::record_intent_decision` (the sequential path does at `agent/dispatch.rs:256`). It also skips the `prepare_tool_effect_gate`/approval-broker path; for read-only tools this is mostly audit loss, but the template bypass is substantive.

`decide()` is deliberately stateless: in `smart`/`strict`/`unattended` it allows `ReadOnly` without consulting project rules (`crates/vp-core/src/permissions.rs:104-160`). A project can create a deny template for `risk_levels=["read_only"]` and a path glob such as `secrets/**`; `validate_risk_levels` only forbids high risk for `Allow` templates (`approvals.rs:918-945`), so a read-only deny is valid. With one `read_file` call the broker denies; with two `read_file` calls in the same assistant turn both are parallel-safe and both execute.

**Impact.** A project policy intended to block reads of sensitive workspace paths (the only path-level guard for roles without shell) can be bypassed by batching. The same applies to `git`, `remote_read`/`remote_list`/`remote_git`, `read_tool_output`, `build_context`, and the other names in `is_parallel_safe_tool`. The `approval.template.matched` audit trail and contract `intent.decision` events are also missing for allowed calls. This is a direct violation of the documented "deny always wins" invariant and of the requirement that every authorization decision is auditable.

**Reproduction / check.** Create a project deny template for `read_file` over `secrets/**` with `risk_levels=["read_only"]`. Use a fake provider that emits one `read_file("secrets/a.txt")`; it is denied. Then emit two `read_file` calls in one assistant message; both execute. The same test can be run for `git` with a path-scoped deny.

**Suggested fix.** Route each parallel candidate through `Engine::evaluate_tool_permission` and `record_intent_decision` exactly as the sequential path; if any result is not `Allow`, fall back to sequential dispatch (which handles `Ask`/`Deny`, approval records, and contract escalation). Prefer deleting the private decision logic and sharing `authorize_tool_call` with a read-only preflight flag.

**Tests.** Add a regression test with a read-only deny template and two parallel-safe calls; assert both are denied, `approval.template.matched` is emitted, and `intent.decision` is recorded. Add a parallel `git`/`remote_read` variant.

### S2 — Agent tools trust caller-supplied artifact/source/MCP task IDs

**Severity:** High · **Confidence:** Confirmed

**Evidence.** Artifact, source, and MCP-task APIs are globally addressed by ID, and the agent tool layer does not verify the current session/project:

- `read_artifact` calls `engine.get_artifact(id)` (`agent/tools/integrations.rs:1240-1246`), which calls `Store::get_artifact` (`engine/artifacts.rs:586-590`) with no session/project argument. For inline text artifacts the tool returns `artifact.content` directly (`integrations.rs:1263-1268`).
- `publish_artifact` accepts `artifact_id` and calls `engine.publish_artifact_version(...)` (`integrations.rs:1174-1180`); `Store::update_artifact_with_event` updates any row and appends the `artifact.version_published` event to `updated.session_id` (`vp-store/src/store/artifacts.rs:206-276`). This is a cross-session write and event injection.
- `read_source` calls `engine.get_source(source_id)` (`integrations.rs:606-622`); `Engine::get_source` (`engine/sources/retrieval.rs:27-52`) does not check `source.project_id` against the caller's project. Source content is returned directly.
- `media_info`/`media_transcode` accept `artifact_id` (`agent/tools/artifacts.rs:229-236`, `:303-312`); the engine fetches the artifact globally (`archive_media.rs:557`, `:619`) and transcode publishes the derived artifact into the source artifact's session (`archive_media.rs:692`).
- `archive_list`/`archive_extract` accept `artifact_id` (`agent/tools/artifacts.rs:77-99`); the engine fetches globally (`archive_media.rs:364`, `:403`) and `archive_extract` publishes entries into `artifact.session_id` (`archive_media.rs:497`).
- MCP `task_get`/`task_result`/`task_cancel` accept `task_id` (`integrations.rs:212-233`, `:260-277`); `mcp_task_get`, `mcp_task_result`, and `mcp_task_cancel` fetch by ID without a session filter (`mcp/tasks.rs:405`, `:428`, `:471`).

The relevant tools are classified as follows: `publish_artifact`, `read_artifact`, `read_source`, `archive_list`, and `media_info` are in the read-only list (`permissions.rs:436-451`); `mcp` is `HostState` only for `action=call` (`permissions.rs:190-197`), so `task_cancel` is read-only. `publish_artifact` (including the update path) and `read_artifact` are in the non-Scout artifact surface (`agent.rs:1350-1390`); `run_mailbox_tool` itself has no role/session check (`integrations.rs:1061-1068`).

**Impact.** A Worker/Labour that learns another session's artifact ID can read or overwrite its text artifact, and the update/archive/media paths append attacker-controlled events to the victim session. A Worker that learns another project's source ID can read that project's knowledge. A Worker that learns another session's MCP task ID can read its result or cancel it without approval. UUIDv7 IDs make guessing impractical, but they are not authorization; S3 provides a concrete way to learn IDs from shell-readable tool outputs/process logs. The impact is high because these are cross-session writes, not just reads.

**Reproduction / check.** In a test harness with two sessions A/B in the same project (and optionally two projects), create a B artifact/source/MCP task, then call the A tools with B's ID:

- A `publish_artifact {artifact_id: B, content: "tampered"}` changes B's artifact and appends `artifact.version_published` to B.
- A `read_artifact {artifact_id: B}` returns B's content.
- A `read_source {source_id: B_source}` returns B's project source.
- A `mcp {action:"task_cancel", task_id: B_task}` cancels B's task.

A positive control is `read_tool_output`/`process`; both reject foreign handles (`tool_output.rs:179-188`; `tooling/process.rs:70-90`).

**Suggested fix.** Add session/project-scoped engine methods or pass `session_id`/`project_id` into the tool calls and verify ownership before resolving. Store lookups can enforce ownership in SQL (`WHERE id=? AND session_id=?`/`project_id=?`) rather than checking after the fact. Do not rely on UUID unguessability.

**Tests.** Add cross-session tests for artifact read/update, archive list/extract, media info/transcode, source read, and MCP task get/result/cancel; assert a typed `NOT_FOUND`/`PERMISSION_DENIED` and no event appended to the victim.

### S3 — Shell read denylist omits daemon-owned state directories

**Severity:** Medium‑High · **Confidence:** Confirmed

**Evidence.** `shell_secret_roots` returns only database files/sidecars, `run`, `sessions`, `logs`, `artifacts`, `projects.jsonl`, rebuild backups, and selected home credential paths (`agent.rs:2311-2361`). `sandboxed_command` appends each of those as a Seatbelt `(deny file-read* (subpath ...))` (`tools.rs:1714-1718`). The default `ShellPolicy` is `network="allow"`, `restrict_home=false`, `read_jail=false` (`vp-protocol/src/types.rs:35-43`), so by default the shell has `(allow default)` read access to everything not in that denylist.

The data directory also contains runtime state used by session-scoped APIs:

- `data_dir/tool-outputs/<sha256>.txt` (`vp-store/src/store/tool_outputs.rs:6-21`), the spilled full tool outputs.
- `data_dir/processes/.../output.log` (`process_supervisor.rs:234-302`), supervised process logs.
- `data_dir/worktrees/...` (`worktree.rs:32`, `:108`), session/task worktrees.
- `data_dir/verify/...` (`worktree.rs:743`), verification clones.
- `data_dir/sandbox/...` (`agent/run_loop/body.rs:134-139`), per-agent scratch.
- `data_dir/archive-extracts/...` and `data_dir/media-transcode/...` (`archive_media.rs:410`, `:634`), scratch for extracted/transcoded content.

None of those paths is in `shell_secret_roots`. Because the shell runs as the same user, mode `0600` on the files does not help.

**Impact.** With the default policy, a shell in session A can enumerate and read session B's spilled tool outputs and process logs, bypassing the session-scoped checks in `ToolOutputStore::read_for_session` (`tool_output.rs:179-188`) and `ProcessSupervisor::get/read_logs` (`process_supervisor.rs:265-320`). Tool outputs can contain full HTTP responses, file contents, MCP results, and artifact IDs (which then enables S2). Process logs can contain build/test output and environment-derived data. Sibling worktrees expose other projects' source and any `.env`/credential files they contain.

**Reproduction / check.** In a default project, have session B run a tool that spills output (or a supervised process that writes a marker), then from session A run:

```sh
ls "$HOME/Library/Application Support/VerlinPark/tool-outputs"
grep -R "MARKER" "$HOME/Library/Application Support/VerlinPark/tool-outputs"
grep -R "MARKER" "$HOME/Library/Application Support/VerlinPark/processes"
```

The reads succeed. With `restrict_home=true` or `read_jail=true`, they are denied; the test should cover both modes.

**Suggested fix.** Deny the daemon data directory as a whole (or add every runtime subdirectory to `shell_secret_roots`), then allow the agent's own worktree/scratch explicitly after the deny. Keep the current per-file DB/run denies as defense in depth. Document that `restrict_home`/`read_jail` are the supported mitigation if the denylist remains.

**Tests.** Add a macOS sandbox test that writes a marker into a sibling session's `tool-outputs` and `processes` directory and asserts `cat` returns `Operation not permitted` under the default policy. Add a test that the agent's own worktree/scratch remains readable. Add a test for `restrict_home`/`read_jail` variants.

### S4 — Secrets in raw tool inputs/outputs persist in events, approvals, search, export, and backup

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.** The sequential dispatcher emits the full parsed tool input in `tool.started` for every branch (`agent/run_loop/tool_calls.rs:1737-1747`, and the equivalent branches at `:97-105`, `:261-274`, `:545-567`, etc.) and the full output in `tool.finished` via `tool_finished_payload` (`agent/dispatch.rs:143-149`). Approval creation stores the same input (`engine/approvals.rs:214-220`). `append_event_mirror` writes event JSON verbatim to `sessions/<id>/events.jsonl` (`vp-store/src/store/events.rs:30-45`), and `session.search` returns event-payload snippets (`vp-store/src/store/events.rs:1002-1006`). `SessionBundle` contains `events` and `approvals` (`vp-protocol/src/types.rs:11282-11299`; `engine/runs/export.rs:237`), and `backup` snapshots the database (`backup.rs:89-95`). The `local_http` module says "Secret-bearing request headers are sent but never echoed" (`tooling/http.rs:1-5`), and its test only asserts the secret is absent from the structured result (`tooling/http.rs:465-485`), not from events. `sanitize_evidence_url` says query strings are "never copied into an event" (`tools.rs:543-556`), but `fetch_url` puts the raw URL and final URL in the tool output (`tools.rs:916-935`).

**Impact.** A tool call such as `local_http {headers: {authorization: "Bearer sk-..."}}`, `fetch_url("https://host/?token=...")`, `mcp {arguments: {api_key: ...}}`, or a `write_file` that writes a secret stores that secret in SQLite, the event mirror, `vp search`, approval rows/UI, exported bundles, and v1 ZIP backups (which are unauthenticated and unencrypted). The comments and README's "secrets ... are excluded" from backups (`README.md:118`) are therefore overclaims for operational tool data. This is local-operator data, but bundles and backups are routinely shared, and `session.search` makes the secret discoverable.

**Reproduction / check.** Run one `local_http` POST with `headers: {"authorization": "Bearer SUPERSECRET"}` and one `fetch_url` with `?token=SUPERSECRET`; inspect `vp events <session>` (input/output contain the values), `vp search SUPERSECRET`, `vp export <session>` (bundle contains events), and `vp backup` (database contains the event). The `local_http` unit test passes because it only inspects `ToolOutput.structured`.

**Suggested fix.** Apply a deterministic redaction pass to tool inputs and outputs before event/approval persistence, especially for secret-like keys (`authorization`, `cookie`, `token`, `api_key`, `password`, `secret`), URL query/fragment values, and `write_file`/`apply_patch` content (store a hash/preview instead of full content for events). Keep the full value only in memory for execution. Alternatively store event inputs by reference to an encrypted scratch record. Update the comments and README to match.

**Tests.** Add an integration test that runs `local_http`/`fetch_url`/`mcp` with secret-looking arguments and asserts `session.events`, `session.search`, `session.export`, and `backup` do not contain the secret. Add a test that `write_file` content is not echoed into events beyond a hash/preview.

### S5 — Notification Approve/Deny resolves approvals without the command/path/risk

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.** `SystemNotifier.requestAuthorization` registers `verlinpark.approval` with `Approve` and `Deny` actions (`apps/macos/VerlinPark/Core/SystemNotifier.swift:30-48`). `notify` sets `categoryIdentifier` whenever the notice ID starts with `approval-` (`:62-67`). For approvals, `AppModel.addNotice` sets `title` to `"Approval needed · \(goal)"` and `detail` to `"\(approval.tool) · \(approval.agentName ?? ...)"` (`AppModel.swift:10315-10329`); the body is `notice.detail` (`SystemNotifier.swift:62`). The action handler calls `onResolveApproval(approvalID, action == "verlinpark.approve")` directly (`SystemNotifier.swift:91-98`). There is no risk check, no path/command summary, and no requirement to open the app. System notifications are enabled by default (`SystemNotifier.swift:44-48`).

**Impact.** A user can approve a `HostState` or `Destructive` action (for example a `computer` focus/click, a `shell open`, or a remote write) from a lock-screen banner that says only "shell · Worker". The in-app `ApprovalNotice` shows risk and a Details disclosure (`TranscriptViews.swift:201-310`), but the notification action bypasses that context. This is an informed-consent/social-engineering weakness; it does not bypass the daemon's approval record, but it makes the human gate much weaker.

**Reproduction / check.** With the app backgrounded, trigger a `shell` `open`/HostState approval. The banner offers Approve/Deny with only goal/tool/agent. Tap Approve; the approval resolves without opening the app or showing the command. A lock-screen notification surface, where available, behaves the same.

**Suggested fix.** Include risk and a bounded command/path summary in the notification body, and disable Approve/Deny actions for `HostState`/`Destructive` (require app open). Add a notification-content test that asserts risk appears and that the action is absent for high-risk approvals. Keep `ask_user`/question notification text bounded and consider hiding it on lock screen.

**Tests.** Swift unit test for notification content/action registration; manual macOS smoke (notifications are hard to automate).

### S6 — Declared shell effect probe can self-attest an external effect as done

**Severity:** Low‑Medium · **Confidence:** Confirmed

**Evidence.** `classify_tool_effect` accepts `effect_probe` for any `shell` call whose risk is not `ReadOnly`/`WorkspaceWrite` (`effect.rs:654-687`). `probe_shell` uses the declared `file_hash`/`file_absent`/`git_head`/`git_branch` probe and can return `EffectProbe::done` (`effect.rs:1287-1405`; dispatched at `:838`). The probe is not tied to the command's external target; a command can perform an external mutation and then `touch marker`, and a `file_hash`/`file_absent` probe will settle the effect as `done`. `unknown_blocks_merge` is therefore bypassed for that external effect (`docs/adr/0194-effect-receipts-and-postconditions.md`, merge gates). The existing test `effect.rs:2260-2293` explicitly asserts this behavior (`RiskLevel::External` + declared file hash => `Done`), so this is an intentional local-evidence design with a security consequence, not an accidental code path.

The ADR honestly labels the evidence as local ("a local file hash is local evidence"), but the merge gate treats `done` as settled. The agent chooses the probe.

**Impact.** A Worker can make a `curl`/`git push`/`remote` side effect that was not observed by the daemon appear settled, allowing the commit to enter the merge queue without the `unknown` block that normally requires reconciliation. The Verifier still reviews the diff, but the effect receipt's purpose is to prevent unverifiable external writes from merging. This is a gate weakness, not a provider exactly-once claim.

**Reproduction / check.** Run a `shell` command `curl -X POST <external> ; touch .probe` with `effect_probe {kind:"file_absent", path:".probe"}` or a file hash; `curl` exits 0 even for an HTTP 500, so the receipt settles `done` even when the external effect was rejected or unverified. An external mutation with no local marker remains `unknown` only if no probe is declared.

**Suggested fix.** Restrict declared probes to `WorkspaceWrite` shell commands (where the Seatbelt sandbox and diff review are the evidence), or require a runtime-owned probe for `External`/`HostState` shell calls. At minimum, record `probe=declared` and treat declared probes on external-risk calls as `unknown` for merge gating. Add a test where an external command with a local marker and a declared probe stays `unknown`.

**Tests.** Add an effect test for `shell` risk `External` with a declared file probe; assert `EffectState::Unknown` or that `assert_effects_settled` blocks merge. Add the corresponding `WorkspaceWrite` case as allowed.

### S7 — ACP literal credential headers persist in SQLite

**Severity:** Low‑Medium · **Confidence:** Confirmed

**Evidence.** MCP's `normalize_headers` has a `SENSITIVE_HEADERS` list (`authorization`, `cookie`, `x-api-key`, etc.) and rejects a literal value unless it contains `${` (`mcp.rs:47-56`, `:281-293`). ACP's `validate_headers` only rejects transport-managed headers (`acp/config.rs:35-58`, `:295-318`); `x-api-key`, `x-auth-token`, and other credential names are not reserved or forced through `${ENV_VAR}`. `AcpAgentConfig.headers` is persisted (`types.rs:6221-6224`), and `acp.agent.list` returns the template values (`types.rs:6195-6229`). `auth_header_env` covers only `Authorization` (`acp/transport.rs:378-414`); a literal `X-Api-Key: sk-...` is accepted and stored. The CLI help recommends env indirection (`cli.rs:283`), but nothing enforces it.

**Impact.** An operator following the CLI can store a second credential in plaintext in SQLite/project state, contrary to the project's secret-handling intent; ACP list/detail or UI may render the literal value. This is lower than S4 because it requires an explicit operator configuration, but it is an avoidable secret-at-rest path.

**Reproduction / check.** `vp acp-add agent --header X-Api-Key=sk-live-secret ...` (or the Settings ACP header field) succeeds; inspect the SQLite row/manifest and `vp acp-list` output. MCP rejects the equivalent `X-Api-Key` literal unless it contains `${...}`.

**Suggested fix.** Port MCP's sensitive-header list/indirection rule to ACP `validate_headers`, rejecting literal values for `x-api-key`, `x-auth-token`, `x-access-token`, `cookie`, `proxy-authorization`, and similar names. Keep `${ENV_VAR}` expansion at connect time. Add a unit test for literal vs env-reference values.

**Tests.** Add `acp::config` tests for `X-Api-Key` literal rejection and `${ACP_API_KEY}` acceptance; add a persistence test that no literal secret is stored in `acp_agents.headers`.

### S8 — Remote ACP accepts cleartext non-loopback endpoints

**Severity:** Low‑Medium · **Confidence:** Confirmed

**Evidence.** `validate_acp_endpoint` accepts `http`/`https` for Streamable HTTP and `ws`/`wss` for WebSocket, checks host/userinfo/fragment/query, but does not require TLS or loopback for `http`/`ws` (`acp/config.rs:186-240`). `allow_remote` is required (`transport.rs:155-176`), but it is a single opt-in for any endpoint. ADR-0165 says "`ws` is accepted for localhost/development endpoints; operators explicitly opt in with `allow_remote` either way" (`docs/adr/0165-acp-http-and-websocket-transport.md:155`), but the implementation does not enforce the localhost half. ACP auth is a bearer token from `auth_header_env`; MCP headers/templates are also sent.

**Impact.** An operator can configure `ws://192.168.1.10:8080/acp` or `http://remote-host/acp` with `--allow-remote` and send the `Authorization` header (and any MCP headers) in cleartext. The trust model says remote agents are trusted services, but cleartext transport leaks the credential to the network; the ADR's localhost wording implies this should be limited.

**Reproduction / check.** Configure a remote WebSocket endpoint `ws://192.168.1.10:8080/acp` with `allow_remote=true` and `auth_header_env=ACP_TOKEN`; the config validates and the transport opens. `https`/`wss` is not required.

**Suggested fix.** Require `https`/`wss` for any non-loopback endpoint; allow `http`/`ws` only for `127.0.0.1`, `::1`, or `localhost` (and fail with a clear error suggesting `wss`). Update the ADR or enforce its wording. Add tests for loopback clear vs non-loopback clear.

**Tests.** Add `acp::config` tests: loopback `ws` accepted; `ws://example.com` rejected without TLS; `wss://example.com` accepted.

### S9 — Invalid approval mode silently becomes `smart`

**Severity:** Low‑Medium · **Confidence:** Confirmed

**Evidence.** `AgentService::from_env` parses `VERLINPARK_APPROVAL_MODE` with `ApprovalMode::parse(&value).unwrap_or_default()` (`constructors.rs:76-79`); `configure` does the same (`:191-193`). `ApprovalMode::parse` returns `None` for anything other than `smart`/`strict`/`unattended` (`types.rs:237-244`), and `Default` is `Smart`. A typo such as `stric` or `unattendedd` therefore starts in `smart`.

**Impact.** An operator intending `strict` (ask before every write) can silently get `smart` (routine workspace writes auto-allowed). An operator intending `unattended` can silently get a mode that asks for human approval and may block runs. The daemon gives no warning; `agent.info` reports the effective mode, but only if inspected. This is a fail-open configuration behavior.

**Reproduction / check.** Start `verlinpadd` with `VERLINPARK_APPROVAL_MODE=stric`; `agent.info`/`vp agent` reports `smart`, and a workspace write is not prompted.

**Suggested fix.** Treat an invalid non-empty env value as a startup error (or log a prominent warning and retain the previous mode). Add a unit test for `ApprovalMode` parsing/constructor behavior; document the fail-closed rule.

**Tests.** `constructors` test with invalid env yields an error or explicit fallback warning; `ApprovalMode::parse` table.

### S10 — Release publishes untested artifacts; unsigned CLI in the DMG

**Severity:** Medium · **Confidence:** Confirmed

**Evidence.** `.github/workflows/release.yml` runs on tag push and goes straight from checkout/toolchain install to `just package` (`:68`); it does not run `cargo test`, `cargo clippy`, Swift tests, or the security scripts before publishing. `scripts/package-release.sh` builds the app with `CODE_SIGNING_ALLOWED=NO` (`:34`), signs `Contents/Helpers/verlinpadd`/`vp-remote-agent` and the app (`:64-76`), then copies `target/release/vp` into `STAGE/bin/vp` (`:99`) and never signs it. The DMG itself is created with `hdiutil` and only gets a `.sha256` (`:130-143`); the app is notarized optionally, not the DMG or `bin/vp`. `ci.yml` pins `actions/checkout@v4`, `dtolnay/rust-toolchain@stable`, and `Swatinem/rust-cache@v2` to mutable major/stable tags (`:13-19`, `:26-28`) and runs no `cargo audit`/`cargo deny` or script shellcheck. `scripts/generate-homebrew-cask.sh` takes `VERLINPARK_SIGNED`/`VERLINPARK_NOTARIZED` from the environment (`:23-30`), not from a verified artifact, so a manual run can claim signing/notarization that did not happen.

**Impact.** A tag can publish a DMG from a commit that fails tests/lint. A Developer ID-signed/notarized app can contain an unsigned `bin/vp` that users are told to copy onto `PATH`; Gatekeeper behavior for the CLI is weaker and the binary is outside the notarized ticket. Mutable action tags and no dependency audit are standard supply-chain risks. The Homebrew cask caveat can be wrong if the env flags are set without the corresponding artifact state. This is primarily release reliability/integrity, not a runtime exploit.

**Reproduction / check.** Push a `v*` tag to a commit with a failing unit test; the release job still builds/uploads. Run `codesign -dv --verbose=4` on `bin/vp` from the DMG; it reports unsigned/ad-hoc while the app is signed. Run `just package` with `VERLINPARK_SIGNED=1` but no identity; the cask says signed.

**Suggested fix.** Make the release job depend on the CI workflow (or run `just lint`/`just test` before `just package`); sign `bin/vp` (and any other standalone binaries) with the same identity/hardened runtime; sign and notarize the DMG or document that only the app is notarized; derive cask caveats from the actual `package-release.sh` output/artifact metadata rather than unverified env; pin third-party actions to commit SHAs; add `cargo audit`/`cargo deny` and `shellcheck`/`bash -n` for scripts.

**Tests.** Add a release dry-run CI job that builds the DMG without signing and verifies `codesign` status of every binary; add a small test that the cask generator's `SIGNED`/`NOTARIZED` values come from the artifacts. Add `cargo audit` (or `cargo deny`) to CI.

## 4. Explicit documentation overclaims

- `crates/vp-core/src/tooling/http.rs:1-5` says secret-bearing request headers are "sent but never echoed"; S4 shows they are persisted in `tool.started` input, approval rows, `session.search`, exported bundles, and backups.
- `crates/vp-core/src/tools.rs:543-556` says query strings/fragments are "never copied into an event"; `fetch_url` returns the raw URL in its tool output (`tools.rs:916-935`), and the input is in `tool.started`.
- `README.md:118` says `vp backup` excludes "secrets"; the database snapshot includes event payloads/approval rows that may contain tool-provided secrets (S4). Provider API keys are still memory-only, but the blanket "secrets excluded" wording is too broad.
- `docs/adr/0165-acp-http-and-websocket-transport.md:155` says `ws` is accepted for localhost/development endpoints; `crates/vp-core/src/acp/config.rs:186-240` accepts `ws`/`http` for any host (S8).
- `crates/vp-protocol/src/types.rs:6221-6224` says expanded header values are never persisted; this is true for `${ENV_VAR}` expansion, but literal values for non-reserved sensitive header names are persisted (S7).
- `AGENTS.md` invariant 5 ("Do not log API keys or full secrets") is honored for provider keys and discovery/gate/keyring material, but not for secrets passed through tools; the invariant should distinguish provider credentials from arbitrary tool/environment data, or S4 should be fixed.

## 5. Test gaps

The following are not all bugs, but they are places where the current tests would not catch the findings above:

- **No actual shell secret-root denial test.** `crates/vp-core/src/tools.rs:2815-2897` exercises `restrict_home`/`read_jail` with `secret_roots: Vec::new()`; `crates/vp-core/src/agent/tests.rs:5441` only checks that `shell_secret_roots` returns names. There is no macOS test that `cat ~/.ssh/id_rsa`, `cat <data_dir>/tool-outputs/...`, or `cat <data_dir>/processes/.../output.log` fails under the default policy.
- **No cross-session artifact/source/MCP-task test.** `read_tool_output` and `process` have foreign-handle tests (`tool_output.rs:179-188`, `tooling/process.rs:70-90`), but `read_artifact`, `publish_artifact` update, `read_source`, `media_info`/`media_transcode`, `archive_list`/`archive_extract`, and `mcp` task get/result/cancel do not.
- **No secret-redaction test for events/search/export/backup.** `local_http` checks only `ToolOutput.structured` (`tooling/http.rs:465-485`); there is no test that a secret header/query is absent from `tool.started`/`tool.finished`, `session.search`, `SessionBundle`, or `backup`.
- **No parallel-batch deny-template test.** The existing parallel path tests appear to use ordinary read-only tools; there is no test with a project deny template or approval grant and two or more calls.
- **No integration test for a declared external-effect probe at the merge gate.** `effect.rs:2260-2293` asserts the current `External` + declared probe => `Done` behavior in isolation, but no test runs an actual external shell command with a local marker and then checks `assert_effects_settled`/merge admission.
- **Notification action tests are missing.** There is no Swift test that a `HostState`/`Destructive` approval notification hides/disables Approve or includes risk/path; the action is UI-only.
- **No ACP header secret-indirection test.** `acp::config` tests cover reserved headers and env references, but not a literal `X-Api-Key` or `X-Auth-Token`.
- **No cleartext non-loopback ACP test.** `validate_acp_endpoint` tests accept/deny schemes but do not assert loopback-only clear transport.
- **No invalid approval-mode test.** `ApprovalMode::parse` is tested for valid values, but not for constructor fail-closed behavior.
- **Release/CI:** no job runs tests before release, no `cargo audit`/`cargo deny`, no shellcheck for `scripts/*.sh`, no test that every binary in the DMG is signed, and no check that cask signing/notarization caveats match the artifact.
- **CI Swift coverage:** `ci.yml` runs `swiftformat --lint` only on `VerlinPark`/`VerlinParkTests` (`:28`), not the helper directories; `just lint` covers the helpers, so CI and local lint differ.
- **Manual-only security surfaces:** computer helper TCC/AX, native GUI, browser page network, and notification actions remain manual-smoke or helper-unit tested; this is documented, but it means CI does not exercise the actual privileged paths.

## 6. Clean areas (verified in code, not exhaustive)

- **Daemon exposure/auth:** bind is `127.0.0.1` only (`vp-daemon/src/main.rs:193`); first WebSocket message must be `auth`; token comparisons are constant-time (`http.rs:282`, `server.rs:34-37`); discovery is written atomically with `0600` (`server.rs:78-86`); `/metrics` uses the same Bearer token.
- **Path jail:** `safe_existing_path`/`safe_write_path` reject absolute paths, `..`, symlink targets, and `.git` writes (`tools.rs:2319-2450`); `find_files`/`read_files` use canonical paths; `apply_patch` validates every path and rolls back.
- **Shell:** fail-closed if `sandbox-exec` is missing (`tools.rs:1601-1604`); cleared env with an explicit allowlist (`tools.rs:493-540`, `:1720-1725`); process-group TERM→KILL cleanup (`:2130-2150`, `:2200-2220`); Apple Events denied; Git hooks/config/common refs denied except the agent branch.
- **Archive/media:** archive extraction is bounded, rejects absolute/traversal/symlink/device/duplicate entries, uses canonical scratch paths and `create_new` (`tooling/archive.rs:799-1043`); `ffmpeg`/`ffprobe` run scrubbed (`tooling/media.rs:792-840`).
- **MCP:** pin trust is checked independently of contract mode for the generic `mcp` call (`agent/tools/integrations.rs:130-147`); dynamic tools are only advertised when `pin_is_callable` (`mcp_pins.rs:110-115`); HTTP/SSE same-origin redirects, reserved/sensitive header handling, env expansion, and response caps are implemented (`mcp.rs:265-321`, `mcp/http.rs:834-848`).
- **Gates:** only a SHA-256 verifier is stored; raw tokens are returned once; resolve is one-shot/expiry-bound; loopback resolve is policy-gated (`gate.rs:141-155`, `:306-330`, `:486-520`).
- **Backups:** Argon2id + ChaCha20-Poly1305 v2; Ed25519-signed v3 manifests; 0600 keyring; key revocation/rekey/portable anchor; path/checksum validation on restore (`backup/signing.rs`, `backup/repository/`).
- **Remote:** helper token from env/0600 file; constant-time token comparison (`vp-remote/src/server.rs:366`); canonical path jail with symlink/`.git` rejection (`vp-remote/src/jail.rs:41-156`); scrubbed exec env (`vp-remote/src/exec.rs:233-250`); checksum/version-checked bootstrap with no sudo (`vp-remote/src/bootstrap.rs`).
- **ACP:** endpoint parsing rejects userinfo, fragments, whitespace, credential-like query keys; reserved transport headers cannot be overridden; `${ENV_VAR}` expansion is in memory; `allow_remote` is rechecked at connect (`acp/config.rs:186-321`, `acp/transport.rs:155-194`).
- **GUI:** one daemon-wide lease, monotonic lease generation, snapshot generations, `STALE_REF`, sensitive-target default deny, bounded FIFO wait, and a timeline without screen content/typed text (`gui.rs`, `computer.rs` tests).
- **Contract/effect/script sharing:** sequential dispatch and Code Mode inner calls share `authorize_tool_call`, `prepare_tool_effect_gate`, and `finish_tool_call` (`agent/dispatch.rs:182-298`, `agent/script.rs:309-375`), so a contract/approval/effect change is less likely to apply to only one path. (S1 is the separate parallel fast path that does not use this stack.)

## 7. Assumptions and limits

- Severity assumes a compromised or malicious agent running in one session, with a same-user local daemon. A local operator with the discovery token is already trusted to administer the daemon.
- S2 depends on learning an identifier. UUIDv7 is not easily guessable; this is an authorization defect, not a claim of remote unauthenticated exploitation. S3 makes IDs learnable from shell-readable tool outputs/process logs under the default policy.
- S3 is mitigated by opting into `restrict_home` or `read_jail`; the default is permissive by design. The finding is that the explicit `secret_roots` list (which is meant to cover daemon state) is incomplete, not that the docs promise a full read jail.
- S4 is local data exposure; `session.search`, bundles, and backups are operator surfaces. The finding is confirmed by code (raw inputs/outputs persisted) and by the contradictory "never echoed"/"never copied" comments, not by a live secret in this repository.
- S5 is an informed-consent weakness, not a daemon authorization bypass; the user still chooses Approve/Deny.
- S6 is a gate-evidence weakness, not a claim that the Verifier/Integrator can be skipped. The Verifier still reviews the diff.
- S8 depends on an operator choosing `allow_remote=true` and a cleartext endpoint. The trust model treats remote agents as trusted services, but the ADR's localhost wording is not enforced.
- S10 is release reliability/integrity; no live signing or notarization was attempted. The signing path remains only partially exercisable without Apple secrets, as ADR-0090 states.
- No full `cargo test`/`xcodebuild test` was run for this review; the report is based on reading the code and the existing tests. No source, test, config, or doc changes were made and no commit was created.

## 8. Suggested priority

1. Fix S1 first: it is a project policy bypass in a supported fast path.
2. Fix S2 next: add session/project ownership to every ID-addressed agent tool; cross-session writes are the highest-impact data-integrity issue.
3. Fix S3: deny the daemon state directories in the shell profile or default new projects to a read jail.
4. Fix S4: add event/approval redaction and update the comments/README.
5. Fix S5/S6/S7/S8/S9 in the next hardening round; they are narrower but cheap to test.
6. Add the release/CI gates in S10 before the next tagged release.
