# 0219 — Code Mode `run_script`

- Status: Accepted
- Date: 2026-11-14
- Scope: C10 bounded JavaScript composition over the frozen tool bundle

## Context

Agents often need to read several files, filter results, and compose a small
summary. Expressing that as N provider turns costs tokens and adds retry
surface. Code Mode lets one tool call run a small JavaScript program that
composes existing tools. The risk is obvious: a script runtime that can reach
the filesystem, network, shell, process table, or native APIs becomes a new
capability boundary that bypasses approvals. C10 therefore starts with a
read-only spike and a capability-minimal runner, not a general Node runtime.

## Decision

### Runner and budgets

- `run_script` is a runtime-managed tool for Leader, Worker, and Labour. It is
  absent from Manager, Verifier, Scout, and ACP/layout-v2 surfaces.
- The runner uses `rquickjs` (QuickJS) for execution and `oxc` for static
  syntax/ambient checks. Each call creates a fresh runtime and context.
- Fixed limits: 64 MiB QuickJS heap, 512 KiB stack, 64 KiB source, 64 KiB
  optional JSON input, 256 KiB returned JSON, 64 KiB stdout, and 128 KiB of
  text plus 32 KiB of structured data per inner call.
- Caller-declared limits: `timeout_ms` 100–30,000 (default 5,000), `max_calls`
  1–32 (default 16), and `max_write_calls` 1–8 (default 4). The CPU deadline is
  enforced by the QuickJS interrupt handler; dropping a cancelled run sets the
  interrupt flag and closes the reply channel.
- Scripts are synchronous. There is no async/await, timer, module loader,
  `import`, `require`, filesystem, network, shell, process, GUI, MCP, mailbox,
  artifact, delegation, or native binding. `eval` and the `Function` global are
  deleted before user code runs.
- Static validation rejects syntax errors, excessive nesting, and a bounded
  list of forbidden ambient identifiers (`process`, `require`, `fetch`,
  `WebSocket`, `Deno`, `os`, `std`, and similar). This is defense-in-depth, not
  the capability boundary: the only native function reachable from user code is
  a private dispatch closure retained by the generated wrappers.

### Bindings

- Bindings are generated from the current advertised tool set (the frozen
  bundle plus explicit C8 promotions), intersected with a static catalog.
  A tool that is not advertised is not bound; a name that is not bound throws
  `tool ... is not bound` at call time, and the proxy never exposes an unknown
  property.
- Read-only bindings: `read_file`, `list_dir`, `grep`, `find_files`,
  `file_info`, `read_files`, `git`, `repo_map`, `find_symbol`,
  `find_references`, `dependency_graph`, `build_context`, `affected_tests`,
  `read_pdf`, `read_image`, `read_document`, and `data_query`.
- Write bindings are opt-in: `write_file`, `edit_file`, and `apply_patch`.
  `file_manage`, shell, process, network, remote, GUI, MCP, delegation, plan,
  memory, and artifact tools are never bound inside a script.
- Each binding validates `args` as a JSON object and applies a bounded,
  best-effort check of the advertised schema (required fields, primitive types,
  enums, arrays, string/number bounds, `additionalProperties: false`). The tool
  still performs its own authoritative validation after dispatch.
- A generated `tools.<name>(args)` call returns `{text, data, truncated}`.
  Tool errors throw a JavaScript `Error` with `.code` (for example
  `FILE_NOT_FOUND`); scripts may catch and summarize them. `input` is frozen
  JSON; `print`/`console.*` append to the bounded stdout buffer.

### Inner-call authorization

An inner call is not a shortcut. It re-enters a dedicated dispatcher path that
runs the same policy stack as a normal tool call:

- role/profile policy and the script binding catalog;
- task-contract authorization, including intent decisions and mutation usage;
- risk assessment and the approval broker (a synthetic
  `"<parent>:inner:<seq>"` call id, with script parent/sequence context);
- run-journal intent/completion and duplicate-write protection;
- effect classification, prepare, postcondition probe, settlement, and replay
  or `unknown` blocking for writes;
- the active skill's tool allow/deny list;
- preemption tokens and the per-tool deadline;
- `tool.started` and `tool.finished` events with `script=true`,
  `script_parent_tool_call_id`, and `script_seq`.

`allow_write: true` raises the outer `run_script` approval to `HostState`. Each
inner write then goes through the normal approval policy again (WorkspaceWrite
under `smart`, a prompt under `strict`, denied under `unattended` when
host-state). Outer approval is not a blanket write grant. The script's declared
call and write caps bound approval fatigue.

The outer `run_script` call is journaled by the normal loop. Inner writes carry
their own effect receipts; a daemon restart that re-runs the script replays a
completed inner write instead of executing it again. This is idempotency by
effect replay, not an exactly-once remote guarantee or a deterministic replay
engine.

### Output and telemetry

- Script stdout and the returned JSON are combined into the tool result and
  then pass through the normal `ToolOutputPolicy` batch shaping, including
  spill/paging for oversized results.
- Inner call records (seq, tool, read-only flag, error code, duration, risk,
  approval wait) are part of the structured result. Arguments, file content,
  and stdout are not duplicated into a separate telemetry stream.
- New stable error codes: `SCRIPT_INVALID`, `SCRIPT_FAILED`, `SCRIPT_TIMEOUT`,
  `SCRIPT_MEMORY_LIMIT`, `SCRIPT_CALL_BUDGET`, `SCRIPT_WRITE_BUDGET`,
  `SCRIPT_OUTPUT_LIMIT`, `SCRIPT_CANCELLED`, `SCRIPT_DISABLED`.
- `tool_help` gains a `code_mode_composition` recipe. `run_script` joins the
  Leader and Coding bundles; the Leader target moves from 50 to 51 because the
  coding profile already adds the two GUI tools.

## Security properties and non-claims

- The script has no ambient host capability. Even dynamic `Function`
  construction reaches only the generated tool wrappers, whose names are
  checked natively.
- The QuickJS heap/stack/CPU limits bound resource use inside the daemon, not
  the whole process. The runner is not an OS-level sandbox, cgroup, separate
  process, or seccomp profile; a QuickJS memory-safety bug would run in the
  daemon process. Tools executed by the bridge still inherit the existing
  Seatbelt/path-jail/approval constraints.
- Scripts cannot call `run_script` recursively, cannot create plan tasks, and
  cannot approve their own inner calls.
- `SCRIPT_TIMEOUT` and `SCRIPT_MEMORY_LIMIT` are non-retryable by default;
  repeating an identical script is not a recovery strategy.
- The call/write caps are pre-declared budgets, not a global quota. A model can
  call `run_script` repeatedly across turns subject to the normal turn and
  session budgets.

## Verification

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: 3,951 passed, 0 failed
- New tests cover syntax/forbidden-global rejection, nesting/source caps,
  frozen-bundle binding construction, unknown proxy bindings, absence of
  ambient capabilities (including via dynamic `Function`), typed argument
  checks, call/write budgets, CPU and heap limits, stdout caps, deterministic
  inner-call sequences, catchable tool errors, stable error mapping, role
  advertisement, and end-to-end read-only/write scripts with inner
  `tool.finished` and effect evidence.

## Limits

- No async/await, timers, modules, generators, or event loop. Long-running
  orchestration still belongs to the agent loop.
- Only registry-backed tools are bound. Mailbox, MCP, artifacts, sources,
  memory, team status, gates, and delegation are unavailable inside a script.
- Schema checking is a bounded best-effort guard, not a full JSON Schema
  validator.
- `oxc` checks the original source; dynamically constructed code is not
  statically inspected, only capability-limited at the bridge.
- The runner is one daemon process with no per-script OS isolation and no
  cross-host execution. Output/result bodies remain tool output, not a new
  event projection.
- Write expansion is limited to three file tools. Shell/process/network/GUI
  bindings remain explicit normal tool calls, not script capabilities.

## Related

- ADR-0193 (task contracts and intent authorization)
- ADR-0194 (effect receipts)
- ADR-0213 (hardening: contracts/effects/identities)
- ADR-0217 (tool search v2 and frozen bundle promotion)
- `docs/CONTRACTED_EXECUTION_PLAN.md` C10
