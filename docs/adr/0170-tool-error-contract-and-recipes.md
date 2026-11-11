# ADR-0170: Uniform tool errors, recipes, and per-tool observability

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, agent UX, SwiftUI
- **Related:** ADR-0134/0135 (tool output and shell sandboxing), ADR-0159
  (tool-output paging), ADR-0161 (lease fencing), ADR-0164 (context
  checkpoints), `docs/QUALITY_CACHE_GUI_TOOLING_PLAN.md` Q3

## Context

Tools already returned a provider-facing `content` string and an optional
`structured` value. That was enough for the runtime, but not uniform enough for
agents or operators:

1. **Failure shape was inconsistent.** Some tools returned
   `{"code": "PATCH_CONFLICT"}`, some returned a bare `ToolOutput::error`, and
   some propagated `anyhow::Error`, which the dispatcher rendered as
   `tool error: …`. The model could not reliably tell whether a retry was safe.
2. **Hints were implicit.** A path error, a stale patch, a missing language
   server, and a sandbox denial all looked like prose. A model could loop on a
   non-retryable failure because nothing said "do not retry unchanged".
3. **Structured errors could leak operational content.** A shell failure
   carried the full command, stdout, and stderr in `structured`; a process
   failure carried an output tail. Events and UI did not need those bodies and
   they could contain secrets.
4. **Tool guidance was not in the stable prompt.** Models discovered the large
   tool surface through schemas and `tool_search`, but common multi-step
   recipes (batch reads, symbol navigation, safe edits, output paging, GUI
   verification, remote operations, memory recall) were not stated.
5. **Operators had no bounded per-tool timeline.** The transcript showed tool
   calls, but Manage Dash could not answer "which tool is slow, which failures
   are retryable, and how large are the results?" without parsing raw events.

This ADR defines an additive contract. It does not change approval, sandbox,
path-jail, write-scope, Verifier, merge, capacity, or budget gates.

## Decision

### 1. Stable failure contract

Every error tool result carries the same object in `structured`:

```json
{
  "code": "PATCH_CONFLICT",
  "message": "old_string was not found",
  "retryable": true,
  "hint": "Re-read the file and regenerate the patch from its current content; do not retry the stale patch.",
  "evidence": "path=src/lib.rs",
  "tool": "edit_file",
  "action": null
}
```

Rules:

- `code` is a stable uppercase wire value from a bounded, documented catalog
  (`vp-core::tooling::errors::ToolErrorCode`).
- `message` is a short human explanation, capped at 2,000 characters.
- `retryable` is conservative. It is `true` only when retrying the same logical
  operation can reasonably succeed and the failed attempt has no known
  destructive side effect. Writes, shell commands, remote writes/exec, MCP
  calls, permission denials, invalid arguments, and `TOOL_FAILED` are
  non-retryable. `PATCH_CONFLICT`, `HASH_MISMATCH`, `WAIT_TIMEOUT`, `STALE_REF`,
  and transport timeouts are retryable after the hinted correction.
- `hint` is actionable and static per code unless a call site supplies a more
  specific safe hint. It is capped at 600 characters.
- `evidence` is an optional bounded summary (`path=…; line=…`) assembled from
  an allowlist of operational identity fields. It never copies command text,
  stdout/stderr, file content, request/response bodies, headers, or credentials.
  It is capped at 800 characters and redacts common bearer/token shapes and URL
  userinfo.
- `tool` is the concrete tool name. `action` is an optional safe discriminator
  for multi-action tools.
- Unknown or missing codes collapse to `TOOL_FAILED`. Unknown failures are not
  silently retried.

The provider-facing text is rendered from the same object:

```text
error[PATCH_CONFLICT] old_string was not found
retryable: yes
hint: Re-read the file and regenerate the patch from its current content; do not retry the stale patch.
tool: edit_file
evidence: path=src/lib.rs
```

That prefix is stable, parseable, and preserves the original message as a
substring. Existing tests and prompts that look for the message keep working.

`ToolOutput::error_structured` remains source-compatible: it parses a legacy
object, preserves safe keys, and enriches it with the contract. It deliberately
drops unsafe output-bearing keys. `ToolOutput::error` remains source-compatible
but now emits `TOOL_FAILED` with the prefix. New code uses
`ToolOutput::error_coded`, `ToolOutput::error_failure`, or
`ToolOutput::from_tool_error`.

A tool may return a typed `ToolFailure` through the existing
`Result<ToolOutput>` path. The dispatcher downcasts it, so path-jail and other
helpers can fail closed without losing their code. A bare `anyhow::Error` gets
a conservative per-tool category (`READ_FAILED`, `WRITE_FAILED`, `GIT_FAILED`,
`COMMAND_FAILED`, `PROCESS_FAILED`, `LSP_ERROR`, `MCP_FAILED`,
`REMOTE_FAILED`, `TOOL_OUTPUT_FAILED`, or `TOOL_FAILED`).

A non-zero command result is special: the command did run, and its stdout/stderr
are useful to the model. `ToolOutput::mark_failure` prefixes the stable contract
while keeping the body, but sanitizes the structured object so command text and
streams do not enter the event's machine-readable half.

### 2. Static recipe catalog and `tool_help`

`vp-core::tooling::recipes` is a process-wide `const` catalog. It is not
computed per turn, not user-configurable, and not part of the tool schema. Each
recipe has a slug, title, intent, audience, tool list, steps, one canonical
example, and caveats. The initial catalog covers:

| Slug | Purpose |
| --- | --- |
| `read_many_files` | bounded batch reads instead of shell cat/head loops |
| `symbol_navigation` | definition/reference lookup with explicit LSP vs lexical confidence |
| `safe_edit` | read/hash/edit/diff/verify with stale-write protection |
| `test_and_check` | detected project commands, focused checks, long-running processes |
| `long_output_paging` | `output_ref` + `read_tool_output` |
| `gui_verification` | localhost browser vs native computer, fresh snapshots |
| `remote_operation` | explicit `remote_*` tools and no local fallback |
| `memory_recall` | evidence-backed recall/remember |
| `git_inspection` | read-only Git state and diffs |
| `artifact_evidence` | durable logs/screenshots/reports |
| `error_recovery` | read code/retryability/hint/evidence before changing a call |

Audiences are `role:profile`, `role:*`, `*:profile`, or `*`, so a read-only
Verifier or Scout does not see edit or GUI recipes, and a research Worker does
not see coding-only recipes. Filtering changes which entries render, never the
catalog or tool schemas.

The stable system prompt includes a compact, bounded recipe block rendered for
the current role/profile. It is byte-identical across turns with the same
identity and contains no session data. The catalog's SHA-256 is stable across
runs. This is deliberately compatible with prefix caching: the block is part of
the stable prefix, not a per-turn insertion.

`tool_help` is a read-only special tool:

```json
{"recipe": "safe_edit", "max_chars": 14000}
```

It returns rendered guidance plus a structured catalog view. It does not
execute a tool, call a provider, find models, change schemas, or route a call.
An unknown or filtered slug returns `INVALID_ARGUMENT` with the visible catalog
hint. `tool_search` remains the discovery surface for built-ins and MCP tools;
`tool_help` is documentation, not discovery-by-execution.

### 3. Per-tool observability

`tool.finished` gains additive fields; existing fields are unchanged:

| Field | Meaning |
| --- | --- |
| `duration_ms` | tool execution wall time, excluding approval wait; capped at 24h |
| `output_bytes` | logical full result bytes before preview shaping |
| `truncated` | existing truncation flag, repeated for consumers that only read metrics |
| `error_code` | stable code, `null` on success |
| `retryable` | failure contract retryability, `false` on success |
| `error_hint` | bounded hint, `null` on success |
| `approval_risk` | `read_only`, `workspace_write`, `external`, `host_state`, or `destructive` |
| `approval_mode` | `smart`, `strict`, or `unattended` |
| `approval_wait_ms` | time spent waiting for a human decision, when an approval was requested |

No content is duplicated: metrics never copy the output or structured payload.
Legacy events decode with zeroes/nulls and `TOOL_FAILED` is derived only when a
legacy error event has no code.

`session.tool_activity` (`vp tool-activity <session>`, `--json` supported)
returns a newest-first, bounded window (1–200 rows, default 50) plus a summary:
calls, errors, retryable errors, truncated results, total/max/average duration,
output bytes, and per-tool counts (top 12). It is a read-only projection of the
event stream, not a new mutable table. `total` and `truncated` report older
events outside the window. The daemon does not scan an unbounded session log.

Swift Manage Dash adds a `ToolActivityCard` with the summary and at most eight
recent rows (tool, status/code, latency, output bytes, approval risk, retry
hint). The Stage transcript tool row also shows the code, retryability, hint,
duration, output bytes, and truncation flag when present, while legacy events
continue to render exactly as before. Views contain no scheduling or retry
logic.

## Error code catalog

The catalog is intentionally bounded. Codes are grouped by surface; adding a
code requires a default hint and a retryability decision.

**General:** `TOOL_FAILED`, `INVALID_ARGUMENT`, `NOT_FOUND`, `PATH_ESCAPE`,
`PERMISSION_DENIED`, `APPROVAL_TIMEOUT`, `SKILL_TOOL_DENIED`,
`TOOL_NOT_AVAILABLE`, `BUDGET_EXHAUSTED`, `CAPACITY_EXCEEDED`, `CANCELLED`,
`TIMEOUT`, `INTERNAL_ERROR`, `UNSUPPORTED_OPERATION`, `READ_ONLY_VIOLATION`,
`RESOURCE_LIMIT`, `CONFLICT`, `INVALID_ACTION`.

**Files/read/write/edit/patch:** `FILE_NOT_FOUND`, `FILE_TOO_LARGE`,
`FILE_NOT_TEXT`, `BINARY_FILE`, `HASH_MISMATCH`, `PATCH_CONFLICT`,
`WRITE_FAILED`, `READ_FAILED`, `SYMLINK_REFUSED`, `NOT_A_DIRECTORY`,
`NOT_A_FILE`, `FILE_EXISTS`, `RECURSIVE_REQUIRED`, `UNSUPPORTED_CONTENT`,
`FILE_DECODE_FAILED`.

**Git:** `GIT_NOT_REPOSITORY`, `GIT_FAILED`, `GIT_INVALID_REF`,
`GIT_CONFLICT`, `GIT_UNAVAILABLE`, `GIT_DIRTY`.

**Shell/project:** `COMMAND_FAILED`, `COMMAND_TIMEOUT`, `SHELL_DENIED`,
`SHELL_SANDBOX_FAILED`, `SHELL_NETWORK_DENIED`, `PROJECT_UNSUPPORTED`,
`TASK_FAILED`, `TEST_FAILED`.

**Process/wait:** `PROCESS_EXITED`, `PROCESS_LIMIT`, `PROCESS_NOT_FOUND`,
`PROCESS_FAILED`, `WAIT_TIMEOUT`, `PROCESS_SPAWN_FAILED`,
`PROCESS_IO_FAILED`.

**LSP/code intelligence:** `LSP_UNAVAILABLE`, `LSP_ERROR`, `LSP_TIMEOUT`,
`SYMBOL_NOT_FOUND`, `DIAGNOSTICS_UNAVAILABLE`, `INDEX_UNAVAILABLE`,
`REFERENCE_NOT_FOUND`.

**Browser/computer:** `BROWSER_UNAVAILABLE`, `BROWSER_ERROR`,
`BROWSER_NAVIGATION_DENIED`, `BROWSER_EVALUATION_FAILED`,
`COMPUTER_UNAVAILABLE`, `COMPUTER_ERROR`, `COMPUTER_PERMISSION_DENIED`,
`COMPUTER_BUSY`, `STALE_REF`, `WINDOW_NOT_FOUND`, `APPS_REJECTED`,
`SNAPSHOT_REJECTED`, `WINDOWS_REJECTED`.

**Remote:** `REMOTE_NOT_CONFIGURED`, `REMOTE_UNAVAILABLE`,
`REMOTE_AUTH_FAILED`, `REMOTE_PATH_ESCAPE`, `REMOTE_WRITE_FAILED`,
`REMOTE_READ_FAILED`, `REMOTE_EXEC_FAILED`, `REMOTE_GIT_FAILED`,
`REMOTE_WORKTREE_FAILED`, `REMOTE_NETWORK_DENIED`, `REMOTE_TIMEOUT`,
`REMOTE_FAILED`, `REMOTE_PROTOCOL_ERROR`.

**Tool output paging:** `TOOL_OUTPUT_NOT_FOUND`, `TOOL_OUTPUT_EXPIRED`,
`TOOL_OUTPUT_INVALID`, `TOOL_OUTPUT_FAILED`.

**MCP:** `MCP_SERVER_NOT_FOUND`, `MCP_DISABLED`, `MCP_TOOL_NOT_FOUND`,
`MCP_TRANSPORT_ERROR`, `MCP_TIMEOUT`, `MCP_PROTOCOL_ERROR`,
`MCP_RESPONSE_TOO_LARGE`, `MCP_FAILED`, `MCP_RESPONSE_INVALID`.

**Network/fetch/web:** `FETCH_INVALID_URL`, `FETCH_BLOCKED`, `FETCH_TIMEOUT`,
`FETCH_FAILED`, `FETCH_HTTP_ERROR`, `FETCH_TOO_LARGE`, `NETWORK_DENIED`,
`HTTP_ERROR`, `WEB_SEARCH_UNAVAILABLE`, `WEB_SEARCH_FAILED`.

**Media/office/data:** `PDF_EXTRACT_FAILED`, `UNSUPPORTED_FORMAT`,
`MALFORMED_XML`, `MISSING_DOCUMENT_PART`, `SHEET_NOT_FOUND`,
`IMAGE_OCR_UNAVAILABLE`, `DATA_QUERY_INVALID`, `DATA_DECODE_FAILED`.

**Session/orchestration/ACP:** `MAILBOX_FAILED`, `MEMORY_FAILED`,
`SOURCE_FAILED`, `TASK_CONTROL_FAILED`, `TASK_NOT_FOUND`, `ARTIFACT_FAILED`,
`SKILL_FAILED`, `COORDINATION_FAILED`, `QUESTION_TIMEOUT`, `VERDICT_INVALID`,
`TODO_INVALID`, `ACP_AGENT_FAILED`, `ACP_TOOL_FAILED`, `SUBAGENT_FAILED`.

Legacy code strings that already exist (`PATCH_CONFLICT`, `FILE_TOO_LARGE`,
`STALE_REF`, office parser codes, and so on) keep their wire spelling. A code
not present in the catalog is replaced by `TOOL_FAILED` in structured output;
the original provider message is retained in the bounded message.

## Wire examples

A successful tool event keeps its existing shape and adds metrics:

```json
{
  "tool_call_id": "call_read_1",
  "name": "read_file",
  "output": "src/lib.rs (120 lines, sha256 …)\n…",
  "is_error": false,
  "structured": {"path": "src/lib.rs", "sha256": "…"},
  "truncated": false,
  "total_bytes": 1820,
  "preview_bytes": 1820,
  "lines": 120,
  "duration_ms": 4,
  "output_bytes": 1820,
  "error_code": null,
  "retryable": false,
  "error_hint": null,
  "approval_risk": "read_only",
  "approval_mode": "smart",
  "approval_wait_ms": null
}
```

A failed edit event carries the contract and no file body in `structured`:

```json
{
  "tool_call_id": "call_edit_2",
  "name": "edit_file",
  "output": "error[PATCH_CONFLICT] old_string was not found\nretryable: yes\nhint: Re-read the file and regenerate the patch from its current content; do not retry the stale patch.\ntool: edit_file\nevidence: path=src/lib.rs; line=42",
  "is_error": true,
  "structured": {
    "code": "PATCH_CONFLICT",
    "message": "old_string was not found",
    "retryable": true,
    "hint": "Re-read the file and regenerate the patch from its current content; do not retry the stale patch.",
    "evidence": "path=src/lib.rs; line=42",
    "tool": "edit_file",
    "path": "src/lib.rs",
    "line": 42
  },
  "duration_ms": 3,
  "output_bytes": 210,
  "error_code": "PATCH_CONFLICT",
  "retryable": true,
  "error_hint": "Re-read the file and regenerate the patch from its current content; do not retry the stale patch.",
  "approval_risk": "workspace_write",
  "approval_mode": "smart",
  "approval_wait_ms": null
}
```

`session.tool_activity` returns a bounded window:

```json
{
  "session_id": "ses-1",
  "entries": [
    {
      "tool_call_id": "call_edit_2",
      "name": "edit_file",
      "seq": 41,
      "is_error": true,
      "error_code": "PATCH_CONFLICT",
      "retryable": true,
      "hint": "Re-read the file and regenerate the patch from its current content; do not retry the stale patch.",
      "duration_ms": 3,
      "output_bytes": 210,
      "truncated": false,
      "approval_risk": "workspace_write",
      "approval_mode": "smart",
      "approval_wait_ms": 0,
      "created_at": "2026-11-11T00:00:03Z"
    }
  ],
  "total": 12,
  "limit": 1,
  "truncated": true,
  "summary": {
    "calls": 1,
    "errors": 1,
    "retryable_errors": 1,
    "truncated_results": 0,
    "total_duration_ms": 3,
    "max_duration_ms": 3,
    "average_duration_ms": 3,
    "output_bytes": 210,
    "approval_waits": 0,
    "total_approval_wait_ms": 0,
    "max_approval_wait_ms": 0,
    "by_tool": [
      {"name": "edit_file", "calls": 1, "errors": 1, "total_duration_ms": 3, "output_bytes": 210}
    ],
    "error_codes": [{"code": "PATCH_CONFLICT", "calls": 1}]
  }
}
```

The response never includes the provider `output` or the full `structured`
payload; it contains only operational metrics and the bounded hint.

## Compatibility and migration

- Older event payloads remain readable. Missing metrics decode as `0`/`false`;
  a legacy error with no code is displayed as `TOOL_FAILED`.
- Older clients ignore additive fields. Existing field names, meanings, and
  ordering are unchanged.
- A tool that still returns a plain `anyhow::Error` is mapped at dispatch; it
  never escapes as an unstructured tool event.
- `ToolOutput::error` remains callable but now produces a coded
  `TOOL_FAILED` output. Code that pattern-matched on exact provider text should
  match the message substring instead.
- `error_structured` preserves safe top-level keys (`path`, `action`,
  `exit_code`, `process_id`, and similar) but removes command/output/credential
  keys. UI code must not depend on those removed keys for error rows.
- The recipe block changes the stable system prefix once. The prompt-prefix
  hash changes on the first run after upgrade and is stable afterward for the
  same role/profile. This is an expected cache generation change, not per-turn
  churn.
- `tool_help` is additive. A role that used to see only `tool_search` now sees
  `tool_help`; the built-in registry and MCP tool schemas are unchanged.
- The CLI `vp tool-activity` uses the same RPC as the Swift client. `--json`
  emits the raw payload; `--csv` emits a flat metrics table with stable columns.
  `vp tool-codes` prints the static catalog without contacting the daemon.

## Consequences

### Positive

- Models receive one stable failure grammar and a conservative retry decision.
- UI can group failures by code without parsing prose.
- Structured events no longer carry shell commands, full output, or obvious
  credential shapes.
- Common workflows are documented in a cache-friendly, deterministic block.
- Operators can identify slow/erroring tools from a bounded timeline.
- Existing tools and tests keep working because the contract is additive and
  `ToolOutput::error`/`error_structured` remain callable.

### Trade-offs

- Provider text is longer for errors: a prefix, retryability, hint, tool, and
  short evidence. This is deliberate; the body is still bounded by the existing
  40,000-character tool cap and the new fields are capped.
- Structured failure messages are not byte-for-byte legacy objects. Safe keys
  survive, but unsafe output-bearing keys are removed.
- Retryability is a static conservative judgment, not a guarantee. A retryable
  code still requires the hinted correction; the runtime never auto-retries a
  tool.
- The recipe block adds a fixed preamble cost to every role. It is stable per
  role/profile, so it participates in prefix caching rather than defeating it.
- The timeline is derived from events and is not persisted as a separate
  projection. It is bounded by the requested window and `total` only counts
  `tool.finished` events.

## Operator runbook

### A model is looping on a failure

1. Open Manage Dash → Usage → Tool Activity (or `vp tool-activity <session>`).
2. Read the repeated `error_code`, retryable flag, and last hint.
3. If retryable is `false`, the next step is a changed input or environment,
   not another identical call. Consider `vp steer <session>` with the specific
   correction or stop the run.
4. If retryable is `true` but repeated, inspect `duration_ms` and the evidence.
   A persistent timeout or stale ref can indicate a blocked server or a wrong
   target, not a transient failure.

### A tool is slow

- Sort or scan the activity list by `duration_ms`. The summary reports the max
  and average for the returned window; `vp tool-activity` prints per-row latency.
- `output_bytes` distinguishes a slow computation from a large result that was
  shaped/spilled.
- `approval_wait_ms` separates human wait from tool execution. A high approval
  wait is not a slow tool.
- The timeline is bounded and event-derived. It is not a full APM trace and
  does not include provider time.

### A failure code is unfamiliar

- Run `vp tool-codes` or `tool_help {"error_code":"<CODE>"}`. Both use the same
  static catalog as the runtime.
- `TOOL_FAILED` means the runtime could not classify the failure confidently.
  Do not treat it as retryable; inspect evidence and logs.
- Code categories are stable enough for dashboards, but the hint is the
  actionable part. A code without its hint can still mislead.

### A recipe seems wrong for the role

- Recipes are filtered by `role:profile`. A Verifier does not see edit recipes;
  a research Worker does not see coding-only GUI recipes.
- The compact prompt block and `tool_help` use the same catalog and audience
  rules. If a recipe is unavailable, the role policy is the source of truth,
  not the recipe text.
- Recipes never grant tool access and never change schemas. A missing recipe is
  a documentation gap, not an execution path.

### Upgrading a client

- Additive `tool.finished` fields are safe to ignore.
- A client that displayed error prose should prefer `error_code` + `error_hint`
  when present and keep the prose as a fallback.
- A client must tolerate unknown future codes. The daemon collapses unknown
  stored codes to `TOOL_FAILED`, but a newer daemon may add a code that an
  older client has not seen; display it verbatim rather than dropping the row.
- The activity RPC is optional. Older daemons return a method-not-found error;
  the dashboard card shows a diagnostic status and keeps the rest of the
  efficiency surface usable.

## Non-goals

- No hidden model routing, model selection, or automatic tool retry.
- No change to approval risk classification, Seatbelt policy, path jail,
  write-scope checks, Verifier gates, merge queue, capacity, or budgets.
- No arbitrary error text in structured evidence.
- No dynamic or per-turn tool schema changes.
- No provider-call exactly-once or mid-stream resume.
- No claim that a recipe guarantees success or that lexical references are
  exact.
- No separate metrics database, time-series store, or unbounded activity log.

## Implementation map

- `crates/vp-core/src/tooling/errors.rs` — bounded catalog, `ToolFailure`,
  rendering, redaction, evidence allowlist, legacy enrichment, classifier, and
  all-failure tests.
- `crates/vp-core/src/tooling/recipes.rs` — static recipes, audience filtering,
  compact/detail/catalog rendering, stable hash, and rendering tests.
- `crates/vp-core/src/tools.rs` — `ToolOutput` failure field, coded
  constructors, `mark_failure`, normalization, path-jail typed failures, and
  tool-level contract tests.
- `crates/vp-core/src/tooling/project.rs` — non-zero project task failure gets
  `COMMAND_FAILED`/`COMMAND_TIMEOUT` and safe evidence while preserving output.
- `crates/vp-core/src/agent.rs` — `tool_help`, recipe prompt block, tool
  dispatch normalization, duration/approval metrics, additive
  `tool.finished`, and special-tool error codes.
- `crates/vp-core/src/lib.rs` — bounded `session.tool_activity` projection,
  event-derived summary, caps, and legacy code normalization.
- `crates/vp-core/src/run_journal.rs` — failure code/retryability persisted in
  the durable run result for resume/reporting.
- `crates/vp-protocol/src/types.rs` — additive activity params/results,
  summary buckets, and decode back-compat tests.
- `crates/vp-store/src/store.rs` — newest-first bounded event query.
- `crates/vp-daemon/src/main.rs` — `session.tool_activity` RPC.
- `crates/vp-cli/src/main.rs` — `vp tool-activity [--json|--csv]` and static
  `vp tool-codes`, plus formatter/CSV/catalog tests.
- `apps/macos/.../Models.swift`, `CoreClient.swift`, `AppModel.swift` —
  decode/load bounded activity; no view-layer business logic.
- `apps/macos/.../ToolActivityMetrics.swift` — pure formatting, filtering,
  ranking, and summary rebuild.
- `apps/macos/.../ToolActivityCard.swift` — Manage Dash summary and recent
  rows.
- `apps/macos/.../ToolActivitySheet.swift`, `StageView.swift` — full bounded
  inspector and Stage entry point.
- `apps/macos/.../TranscriptModel.swift`, `TranscriptViews.swift` — additive
  transcript error code/hint/metrics with legacy fallback.
- `apps/macos/VerlinParkTests/ToolActivityTests.swift` — RPC decode, legacy
  decode, filters, summaries, CSV-adjacent formatting, and transcript tests.
- `crates/vp-core/tests/tool_error_contract.rs` — public-surface contract
  tests across registry tools, recipes, catalog, and activity projection.
- `docs/adr/0170-tool-error-contract-and-recipes.md` — this decision.

## Tests

- `vp-core::tooling::errors`: catalog uniqueness and bounds, conservative
  retryability, unknown-code collapse, structured enrichment and unsafe-key
  removal, secret redaction, provider prefix rendering, `ToolOutput`
  compatibility, and dispatch fallback categories.
- `vp-core::tooling::recipes`: required recipes, audience validity, role/profile
  filtering, stable catalog hash, deterministic/bounded compact block, bounded
  detail/help rendering, and no execution/schema hook.
- `vp-core::agent::tests`: `tool_help` role filtering and read-only behavior,
  `tool.finished` metrics/failure fields, and bounded/legacy
  `session.tool_activity` projection.
- `vp-protocol`: legacy tool-activity defaults and round-trip.
- `vp-store`: newest-first bounded event window.
- Swift `ToolActivityTests`: RPC decode, empty/legacy payloads, metrics
  formatting, newest-first bounded rows, and transcript decode with and without
  the additive fields.
- Existing tool, agent, protocol, and Swift tests remain green.

## Threat model

The failure contract crosses three trust boundaries: tool output is untrusted,
the provider is an external service, and the operator UI is a local client.

- **Untrusted tool output must not become instructions.** Error messages and
  evidence are rendered as data. Recipes are static and never interpolate tool
  output. `tool_help` does not execute, search, or route.
- **Credentials must not enter events or prompts through the failure path.**
  Structured errors drop command/output/header/body keys. Evidence uses an
  allowlist and caps. Messages, hints, and evidence pass through a redactor for
  bearer tokens, common token prefixes, assignments, and URL userinfo. The
  provider body can still contain the tool's original output by design (for
  example a failed command's stderr); that body is bounded by the existing tool
  output cap and is not duplicated into structured data.
- **Paths are not a secret, but host layout is sensitive.** Evidence may
  include a worktree-relative path or a remote root-relative path. Absolute
  host paths should be avoided by call sites; the sanitizer does not attempt to
  prove that a supplied path is relative.
- **The timeline is local and read-only.** `session.tool_activity` is available
  through the authenticated loopback RPC and the operator CLI. It returns no
  file content, environment variables, command text, or request headers.
- **Unknown codes are data, not authority.** A future code is displayed
  verbatim by the client but never treated as an instruction or automatically
  retried.
- **Approval metadata is descriptive.** `approval_risk` and `approval_mode` in
  the event describe the decision that already happened. They do not grant
  permission, and a client editing the payload cannot change the broker.
- **Recipes cannot widen access.** They are documentation. The role/profile
  policy, active skill policy, approval broker, sandbox, path jail, write
  scope, and Verifier/merge gates remain the enforcement points.

Residual risks:

- Error prose can still contain a secret if a tool embeds one outside the
  blocked keys; the redactor is best-effort, not a DLP system.
- `structured` success payloads can contain output (the shell tool does this
  deliberately). The contract only tightens the failure half; existing success
  caps and the 32 KiB event cap still apply.
- The provider sees the bounded failure text. A malicious tool could try prompt
  injection inside its own error message; the role prompt already treats tool
  output as untrusted.
- The timeline is not a tamper-evident audit log. It is derived from the same
  event stream the daemon already owns.

## Prompt-cache accounting

Q3 does not implement Q1's cache-first request changes, but it must not make
caching harder:

- The recipe block is rendered from a `const` catalog and contains no session,
  provider, model, time, or tool-result data. For one role/profile it is
  byte-identical across turns.
- It is placed inside the existing stable system prefix, before run context. It
  does not change the tool schema list, tool order, or MCP discovery result.
- `tool_help` returns guidance in a tool result, not in the system prefix. A
  model that needs detail on demand pays for it once in that turn; repeated
  calls for the same recipe are deterministic and may still be cached by the
  provider as part of the longer conversation prefix.
- The compact block is capped at 2,400 characters. The full catalog/detail
  response is capped at 14,000 characters and cannot make the stable prefix
  grow without bound.
- `context.prompt_prefix` continues to hash the model, sorted tool bundle, and
  stable system text. The first upgrade changes that hash once because the
  recipe block is new; subsequent turns do not churn it.
- Recipes do not call `tool_search`, probe MCP servers, inspect the filesystem,
  or depend on provider capability. Rendering cannot fail open into a different
  schema set.
- The activity timeline runs after the fact and is not injected into the
  conversation. It cannot increase prompt size or invalidate a cached prefix.

If a future change wants to make recipe text user-configurable, it must become a
new stable-prefix generation with an explicit version, not a per-turn mutation.

## Rollout notes

The daemon, CLI, and Swift client can ship together; each is additive. A staged
rollout is also safe:

1. Daemon first: new events carry metrics, old clients ignore them.
2. CLI/Swift second: they display the new fields when present and keep the
   legacy transcript shape when absent.
3. Provider prompt: the recipe block changes the stable prefix once per
   role/profile. This is a cache generation change, not a correctness event.
4. No migration or backfill is required. Historical events remain readable and
   the activity projection handles missing metrics.

## Alternatives considered

1. **Keep code-only structured errors.** Rejected: retryability and hints are
   the model's main recovery signal, and adding them at every call site by hand
   duplicated policy.
2. **Put the full error object only in provider text.** Rejected: UI and
   orchestration need machine-readable codes, and text parsing is brittle.
3. **Persist a separate tool-metrics table.** Rejected for this round: the
   event stream already carries the metrics, and a derived table adds migration,
   retention, and replay surface without a current query need.
4. **Inject all recipes in full every turn.** Rejected: unbounded prompt growth
   and cache churn. The compact block plus `tool_help` keeps the stable prefix
   small while allowing detail on demand.
5. **Auto-retry retryable codes.** Rejected: a tool call can have side effects
   even when the code is retryable, and hidden retries obscure failures. The
   operator/model decides with the hint.
