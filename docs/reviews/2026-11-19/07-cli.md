# CLI / reliability review — `crates/vp-cli`

- Baseline: `d239dca` (`Fix live provider compatibility and budget preflight`)
- Scope: `crates/vp-cli/src/{main,cli,dispatch,render,rpc,tests}.rs`, `dispatch/`, `render/`, `tests/`
- Review type: read-only; no source, test, doc, or config edits
- Verification run: `cargo test -p vp-cli` → **145 unit + 10 backup integration + 1 governance = 156 passed**
- No live daemon, provider, or network calls were made. Daemon behavior cited below was read from source.

## Executive summary

The CLI is structurally complete: the 425 `Command` variants route to a dispatch family, local-only commands are handled before discovery, and a static comparison found no missing CLI→daemon method names or missing command-family arms. Backup/passphrase handling, gate-token handling, and CSV escaping are notably careful.

The highest-risk issues are **state-widening partial updates**: `shell-policy-set` can silently change `network=deny` to `allow` and drop read denies; `approval-template-set` can silently re-enable a disabled auto-approval template; `preemption-policy-set` can silently re-enable hard preemption. Two output/exit-contract defects are also significant: a broad `"removed"` human renderer shadows many command-specific renderers, and streaming commands (`console`, `terminal`, `process logs --follow`) return success regardless of the remote command's exit code and pollute `--json` output.

I found **10 primary findings** (3 high, 6 medium, 1 medium/low) plus a small set of lower-severity parsing/output issues. Several are confirmed with the already-built `target/debug/vp` binary or a direct shell repro; none require a provider call.

## Findings

| ID | Severity | Confidence | Finding | Primary location |
|---|---|---|---|---|
| F1 | **High** | High | `shell-policy-set` replaces the whole policy; omitting flags silently widens network and clears read denies/jail | `cli.rs:1626-1639`, `dispatch/runtime.rs:171-190` |
| F2 | **High** | High | `approval-template-set --id` silently re-enables a disabled auto-approval template; create path ignores `--disabled` | `dispatch/approval.rs:137-176`, `cli.rs:3977-4005` |
| F3 | **Medium** | High | Generic `"removed"` renderer shadows command-specific human output for many commands (MCP/ACP/host/model-preset delete, mailbox/artifact GC, memory operations) | `render/human.rs:16-46`, `render/human/orchestration.rs:362-367` |
| F4 | **High** | High | `console`/`terminal` always exit 0, ignore `--json`, leak the daemon console/terminal on mid-stream RPC error, and only check the 300s deadline between RPCs | `dispatch/preview.rs:8-73,143-210` |
| F5 | **Medium** | High | `cache warm` two-word alias breaks when a global flag precedes it (`vp --json cache warm …`, `vp --data-dir … cache warm …`) | `main.rs:41-48` |
| F6 | **Medium** | High | `plan-fanout --file -` advertises stdin but reads a literal file named `-`; same helper ambiguity affects `team-plan-revise`/`task-edit` | `cli.rs:1496`, `dispatch/team.rs:191,218-223`, `main.rs:1053-1063` |
| F7 | **Medium** | High | RPC transport has no request/connect timeout and no retry; every RPC failure becomes generic exit 1 with no machine-readable code | `rpc.rs:112-136`, `main.rs:839` |
| F8 | **Medium** | High | `preemption-policy-set` unconditionally enables preemption when changing any other field, contradicting “unspecified values are kept” | `cli.rs:2272-2288`, `dispatch/runtime.rs:399-419` |
| F9 | **Medium** | High | `memory-policy-set --embedding-*` re-enables retention and resets limits to defaults when only embedding config was requested | `cli.rs:3590-3604`, `dispatch/memory.rs:377-383,500-512` |
| F10 | **Medium/low** | High | Irreversible/destructive commands `discard`, `checkpoint-restore`, `restore-log`, `process-stop`, and `preempt` have no `--yes`/prompt, unlike peer destructive commands | `cli.rs:2183-2191,2250-2257,2420,3944-3947`; `dispatch/git.rs:116-142`, `dispatch/checkpoint.rs:61-70`, `dispatch/runtime.rs:349-373` |

### F1 — `shell-policy-set` silently widens the shell policy

**Evidence**

- `crates/vp-cli/src/cli.rs:1626-1639` declares `network` with `default_value = "allow"`, `read_deny` as an empty `Vec`, and `restrict_home`/`read_jail` as plain `bool` defaulting to false.
- `crates/vp-cli/src/dispatch/runtime.rs:171-190` sends a complete policy object:
  ```json
  {"network": network, "read_deny": read_deny,
   "restrict_home": restrict_home, "read_jail": read_jail}
  ```
- `crates/vp-core/src/engine/projects.rs:462-480` replaces the stored policy; `normalize_shell_policy` (`projects.rs:709-748`) only validates/normalizes fields and never merges the previous policy.

**Impact**

A project configured with `network=deny`, a `read_deny` list, or `read_jail=true` can be widened by a seemingly additive edit:

```bash
vp shell-policy-set prj --network deny --read-deny ~/.ssh --read-jail
# later, intending only to add HOME restriction:
vp shell-policy-set prj --restrict-home
# stored policy is now: network=allow, read_deny=[], read_jail=false, restrict_home=true
```

This violates the documented “project policy can only narrow, never widen” invariant for a project-level shell policy and silently discards security settings. It is also easy to trigger with no flags at all: `vp shell-policy-set prj` resets network to `allow`.

**Fix**

Read the current policy, then patch only flags that were explicitly supplied (`Option<bool>` for booleans, `Option<String>` for network). Make `--read-deny` additive, with an explicit `--replace-read-deny`/`--clear-read-deny` if replacement is needed. Alternatively require `--network` explicitly and make a no-flag invocation an error.

**Test**

Add a CLI/daemon-backed regression test: seed a restrictive policy, update only `--restrict-home`, and assert `network`, `read_deny`, and `read_jail` are unchanged. Add a test that a no-flag invocation never widens. Current tests do not exercise the update payload.

### F2 — `approval-template-set` silently re-enables disabled templates

**Evidence**

- `crates/vp-cli/src/dispatch/approval.rs:137-146`, update path always sends `"enabled": !disabled`:
  ```rust
  "enabled": !disabled,
  ```
  The CLI reads the existing template and preserves name/tool/risk/path/action/expiry/max-uses, but not `enabled`.
- `crates/vp-cli/src/cli.rs:4003-4005` documents `--disabled` as “update only”, but `--disabled` defaults to false. Therefore any update that does not pass `--disabled` enables the template.
- `crates/vp-cli/src/dispatch/approval.rs:160-176` create path does not send `enabled` at all, so `--disabled` is silently ignored on create and the template is created enabled.
- The daemon applies the supplied `enabled` verbatim (`vp-core/src/engine/approvals.rs:525-580`; SQL update at `vp-store/src/store/approvals.rs:157-186`).

**Impact**

Approval templates auto-approve external/workspace-write actions. A disabled template is the operator’s deny/temporarily-off state. Running:

```bash
vp approval-template-set --project prj --id tmpl --max-uses 10
```

silently turns an auto-approval rule back on. Creating with:

```bash
vp approval-template-set --project prj --name x --tool-pattern write_file --action allow --disabled
```

creates an enabled template because `--disabled` is ignored on create. Both are privilege-expanding partial updates.

**Fix**

Model `enabled` as `Option<bool>` derived from explicit `--enabled`/`--disabled`, preserve the current value on update, and either reject `--disabled` on create or honor it by sending `"enabled": false`. Add a test proving a disabled template stays disabled after an unrelated update.

### F3 — Generic `"removed"` renderer shadows many command-specific human renderers

**Evidence**

`print_human` is a first-match chain in `crates/vp-cli/src/render/human.rs:16-46`:

```text
briefs → programs → sessions → orchestration → sources → projects → mcp → memory → maintenance → status
```

`crates/vp-cli/src/render/human/orchestration.rs:362-367` matches **any** object containing `removed` and prints:

```text
Removed {removed as u64 or 0} expired runtime lease(s).
```

This branch is intended for `runtime.leases.sweep`, but it runs before the specific renderers:

- `render/human/mcp.rs:151-154` has a correct `"MCP server removed={removed}"` branch that is unreachable for `mcp.server.remove` (`dispatch/mcp.rs:80-87`).
- `render/human/status.rs:118-132` has correct mailbox-GC and artifact-GC messages that are unreachable for `mailbox.gc` and `artifact.gc` (`dispatch/mailbox.rs:60`, `dispatch/artifact.rs:161`).
- `render/human/mcp.rs:140-150` has a `memory_id` + `removed` branch that is unreachable for project `memory.delete` (`dispatch/memory.rs:180`).
- `render/human/maintenance.rs:7-13` has the memory-prune renderer, but `memory.prune` returns `{candidates, removed, dry_run}` and is caught earlier by the generic branch.

Bare `{"removed": bool}` results from `mcp.server.remove`, `acp.agent.remove`, `host.remove`, and `model.preset.delete` all hit the generic branch; for a bool, `as_u64()` is `None`, so the output is always `Removed 0 expired runtime lease(s).` even when the command succeeded.

**Impact**

Human output is actively misleading for routine operations:

```text
$ vp mcp-remove mcp-1
Removed 0 expired runtime lease(s).
$ vp model-preset-delete preset-1
Removed 0 expired runtime lease(s).
$ vp mailbox-gc prj-1
Removed 0 expired runtime lease(s).
```

This hides whether a delete/GC actually happened, breaks operators/scripts parsing the human text, and makes several carefully written renderers dead code. JSON output is unaffected.

**Fix**

Do not use a bare `removed` key as the discriminator. Require the lease-sweep shape (for example `leases`/`runtime_id` context or an exact known key set), or move the lease-sweep branch to the end of the chain. Add command-specific branches for MCP/ACP/host/model-preset removal and mailbox/artifact GC.

**Test**

`tests.rs:3504-3516` calls `print_human` once but asserts nothing. Add assertions on captured output for each result shape above, or a table-driven renderer test that asserts the first matching renderer. The existing helper tests (`tests.rs:1545-1615`) call `context_checkpoint_lines` directly, so they cannot catch ordering collisions.

### F4 — `console`/`terminal` swallow exit codes, ignore `--json`, and leak on error

**Evidence**

`crates/vp-cli/src/dispatch/preview.rs:8` discards the JSON flag (`_json`). The console path (`preview.rs:11-73`) and terminal path (`preview.rs:143-210`):

- Prints `console ready…` / `terminal ready…` to stdout without regard to `--json`.
- Streams command output to stdout.
- Prints `[console {status}, exit {exit_code}]` / `[terminal {status}, exit {exit_code}]` but **always returns** `json!({"console_streamed": true})` / `{"terminal_streamed": true}`.
- Returns before `console.close`/`terminal.close` if `console.open`, `console.write`, `console.read`, or `terminal.*` returns an error (the `?` exits the arm).
- Checks the 300s deadline only after a `console.read`/`terminal.read` returns; with the untimed RPC client (F7), a wedged daemon can hang the loop forever.

`crates/vp-cli/src/main.rs:840-849` then prints the marker value: as pretty JSON when `--json`, and as human output otherwise.

**Impact**

- `vp console ses "exit 7"; echo $?` exits **0**, so scripts cannot detect a failed remote command. The same applies to `terminal`.
- `vp console ses --json "cmd"` writes non-JSON text followed by a JSON marker to stdout, so the documented raw-JSON mode is unusable for these commands. Same for `terminal`.
- A mid-stream RPC error leaves the daemon-side console/terminal open until the idle sweeper; the caller sees a generic error and partial output.
- A deadlocked daemon is not bounded by the advertised 300s deadline.

**Fix**

Track the final `exit_code`/status and return a typed error (or `std::process::exit`) when it is nonzero. In `--json` mode, send streaming text to stderr or suppress it and emit one JSON object. Wrap the open/read loop in a guard that best-effort closes on every exit path. Apply a per-RPC timeout so the deadline is enforceable.

**Test**

Add a mock/stub daemon test for exit 0, exit nonzero, timeout, and `--json`; assert stdout is valid JSON and the process exit code matches. Existing tests only cover parse/render helpers, not this path.

### F5 — `cache warm` normalization fails when global flags come first

**Evidence**

`crates/vp-cli/src/main.rs:41-48` only rewrites the two-word alias when `args[1] == "cache"` and `args[2] == "warm"`:

```rust
if args.get(1) == Some("cache") && args.get(2) == Some("warm") {
    args.remove(2);
    args[1] = "cache-warm";
}
```

It does not skip global options (`--json`, `--data-dir`). Confirmed with the built binary:

```text
$ vp --json cache warm ses-1
error: unexpected argument 'ses-1' found
Usage: vp cache [OPTIONS] <SESSION>
exit=2

$ vp --data-dir /tmp cache warm ses-1
error: unexpected argument 'ses-1' found
exit=2
```

`vp cache warm ses-1` and `vp cache-warm ses-1` still work. The unit test `cache_warm_accepts_both_spellings` (`tests.rs:793-828`) only passes the bare four-token form, so it misses this.

**Impact**

`--json` and `--data-dir` are global and documented as usable before the subcommand. Users following that convention get a clap usage error instead of a warm. This is a user-visible parsing inconsistency, not just a cosmetic alias issue.

**Fix**

Normalize on the first non-option tokens (or use a clap alias/subcommand with an explicit `warm` subcommand), not fixed indices. Add tests with `--json` and `--data-dir` on both sides of `cache`.

### F6 — `--file -` is documented as stdin but reads a literal file

**Evidence**

`crates/vp-cli/src/cli.rs:1496` documents `plan-fanout --file` as “JSON file (or stdin with `-`)”. The dispatch path at `crates/vp-cli/src/dispatch/team.rs:218-223` calls:

```rust
let raw = read_json_input(Some(&path))?;
```

`read_json_input` (`main.rs:1053-1063`) reads stdin only when `file` is `None`; `Some("-")` is passed to `std::fs::read_to_string("-")`. With a running daemon, `vp plan-fanout ses --template t --file - --yes` fails with `reading -: No such file or directory` instead of reading stdin.

`team-plan-revise` (`dispatch/team.rs:252`) and `task-edit` (`dispatch/team.rs:191`) use the same helper. `team-plan-revise` without `--file` reads stdin, but explicit `--file -` fails; `task-edit` has no stdin documentation but also blocks on stdin when `--file` is omitted.

**Impact**

The documented non-interactive/scripted path for large fan-out item arrays is broken; users must use `--file /dev/stdin` or write a temporary file. This is particularly relevant for an explicit host-state action typically driven by automation.

**Fix**

Treat `Path::new("-")` as stdin inside `read_json_input`, or add a dedicated `read_json_input_or_stdin` and use it for all `--file` options. Document the behavior on `team-plan-revise` and `task-edit`, and add a test that spawns the CLI with piped stdin.

### F7 — RPC transport has no timeout, retry, or structured exit mapping

**Evidence**

`crates/vp-cli/src/rpc.rs:119-127` builds a fresh client per call:

```rust
let response = reqwest::Client::new()
    .post(discovery.rpc_url())
    .bearer_auth(&discovery.token)
    .json(&body)
    .send()
    .await
    .with_context(...)?;
```

There is no `.timeout(...)`, `.connect_timeout(...)`, or retry loop. The workspace uses `reqwest 0.12`; in the local `reqwest-0.12.28` source, `ClientBuilder` defaults are `connect_timeout: None` and `timeout: None` (`client.rs:299,314`). A daemon that accepts the socket but never responds hangs the CLI indefinitely.

Errors are flattened to `anyhow` at `rpc.rs:128-135`, then `main` returns `Err`, producing exit code 1 and a text-only `Error: … (code N)` message (`rpc.rs:138-151`). There is no stable CLI exit-code mapping for `NOT_FOUND`, `INVALID_ARGUMENT`, `CONFLICT`, `BUDGET_EXHAUSTED`, etc., and no JSON error output under `--json`.

**Impact**

- A wedged or half-restarted daemon can hang `vp` forever; this also defeats every deadline that only checks between RPCs (F4, `process logs --follow`).
- A transient connection failure causes an immediate failure with no bounded retry, even for idempotent reads.
- Scripts cannot distinguish a missing session from an invalid argument or a budget denial except by parsing English text.

**Fix**

Use `Client::builder().connect_timeout(...).timeout(...)` (or a per-command override), retry only idempotent reads on connection/5xx/429 failures, and map typed RPC codes to documented process exit codes. Emit a structured JSON error object when `--json` is set.

**Test**

Add a local stub listener that accepts and stalls; assert the CLI exits with a timeout error. Add a read-only transient-failure retry test, and an error-code→exit-code table test. The current CLI tests have no HTTP/RPC layer tests.

### F8 — `preemption-policy-set` silently re-enables preemption

**Evidence**

`crates/vp-cli/src/cli.rs:2272` says: “Update a project's hard-preemption policy; unspecified values are kept.” But `crates/vp-cli/src/dispatch/runtime.rs:409-419` reads the current policy and then unconditionally does:

```rust
object.insert("enabled".into(), json!(!disabled));
```

`--disabled` defaults to false, and there is no `--enabled` flag. Therefore any update that does not pass `--disabled` turns preemption on:

```bash
# current policy: enabled=false
vp preemption-policy-set prj --provider-grace-ms 1000
# stored policy: enabled=true
```

**Impact**

Hard preemption aborts in-flight provider streams and tool calls (and can escalate to kill). Re-enabling it as a side effect of changing a grace period can unexpectedly terminate active work, contradicting the command’s own “unspecified values are kept” promise.

**Fix**

Make `disabled` part of an explicit tri-state (`Option<bool>` with `--enable`/`--disable`), or preserve the stored `enabled` value unless a flag explicitly changes it. Add a regression test for an unrelated field update.

### F9 — `memory-policy-set` re-enables retention and resets limits when only embedding config changes

**Evidence**

`crates/vp-cli/src/cli.rs:3590-3604` describes the command as “Update the opt-in project memory retention policy.” In `dispatch/memory.rs:377-383`, when neither `--max-items` nor `--max-age-days` is supplied, limits become `Some(1000)`/`Some(180)` rather than the current values; at `memory.rs:504` the payload always sets:

```json
"enabled": !clear && !disable
```

`--disable` defaults to false. `memory.policy.set` therefore receives `enabled=true` and default limits whenever the operator only sets an embedding field:

```bash
# current policy: enabled=false, max_items=50, max_age_days=30
vp memory-policy-set prj --embedding-enable \
  --embedding-endpoint-env OPENAI_BASE --embedding-model text-embedding-3-small
# stored policy: enabled=true, max_items=1000, max_age_days=180
```

**Impact**

Enabling the semantic lane can silently enable automatic retention/pruning and widen the age/count limits. The daemon sweep may then delete memories the operator did not intend to expose to retention. This is a data-loss footgun in an otherwise explicit “opt-in” policy.

**Fix**

Read the current policy and preserve `enabled`, `max_items`, and `max_age_days` unless `--enabled`/`--disable`/`--clear` or the corresponding limit flags are explicitly supplied. Add tests for embedding-only, limit-only, and disable-only updates.

### F10 — Several irreversible commands bypass confirmation

**Evidence**

Commands with explicit confirmation or `--yes`:

- `workspace-land` requires `--yes` (`dispatch/agent.rs:819-823`).
- `media-transcode` requires `--yes` (`dispatch/artifact.rs:103-108`).
- `test-cache-clear` requires `--yes` (`dispatch/testing.rs:148-152`).
- `gui-timeline-prune` prompts unless `--yes` (`dispatch/runtime.rs:142-147`).
- `process-restart` prompts unless `--yes` (`dispatch/runtime.rs:332-334`).
- `daemon-agent install/uninstall` prompt (`main.rs:801-822`).

Commands that do not:

- `discard` — removes uncommitted changes (`cli.rs:3946-3947`, `dispatch/git.rs:136-142`).
- `checkpoint-restore` — resets the session branch/worktree to a checkpoint (`cli.rs:2420`, `dispatch/checkpoint.rs:61-70`).
- `restore-log` — rewrites the event table from the JSONL mirror and rebuilds projections (`cli.rs:3944-3945`, `dispatch/git.rs:116-123`).
- `process-stop` — SIGTERM with SIGKILL escalation (`cli.rs:2183-2191`, `dispatch/runtime.rs:349-365`).
- `preempt` — hard-aborts an in-flight provider/tool (`cli.rs:2250-2257`, `dispatch/runtime.rs:367-373`).

**Impact**

A typo or a copied command can irreversibly discard work, overwrite event state, or kill a running task without a second check. The inconsistency also means operators cannot rely on a uniform “requiring `--yes` means destructive” rule.

**Fix**

Add the same explicit `--yes`/interactive confirmation contract (failing closed on non-TTY) to these commands, and document it in help. Prefer daemon-side typed confirmation parameters where the operation is also reachable through the agent API.

## Secondary, lower-severity observations

These are confirmed by code or the built binary but did not make the top 10:

1. **Non-UTF-8 argv panics.** `main.rs:61` calls `std::env::args()`, which panics on non-UTF-8 arguments. Repro:
   ```text
   $ python3 -c 'import subprocess; subprocess.run([b"./target/debug/vp", b"ping", b"--data-dir", b"/tmp/\xff"])'
   thread 'vp-cli' panicked ... invalid utf-8 sequence ...
   Error: vp CLI thread panicked
   exit=1
   ```
   Use `args_os` and `OsString` for path arguments. This is low likelihood on macOS but produces an ugly panic rather than a clap error.

2. **Evidence references containing `:` are misparsed.** `main.rs:871-882` uses `splitn(3, ':')`, so `--evidence source:https://example.com/x` becomes reference `https`, note `//example.com/x`. The protocol permits opaque references up to 200 chars (`docs/adr/0209-…`); use a JSON form or a documented escaping rule.

3. **`a2a-policy-set --enable --disable` is silently accepted** and resolves to disabled (`dispatch/system.rs:124-145`; `cli.rs:2364-2377`). Other policy setters reject this pair. Add `conflicts_with` or a local bail.

4. **`--json`/`--csv` precedence is inconsistent.** `vp tool-activity --csv --json` prints CSV (`main.rs:838-844`), while `vp usage --csv --json` sends `session.usage_csv` and then prints JSON containing the CSV string (`dispatch/session.rs:190-196`). Declare the flags conflicting or document precedence.

5. **`model-preset-update` is a full replacement despite “Update” wording.** `dispatch/project.rs:370-404` and `cli.rs:1665-1680` require `name`/`model`, default `provider=openai-compatible`, and default notes/prices/context to empty/None. Updating one field can reset the others. Read the current preset and patch only supplied fields, as `acp-update` does.

6. **`export --output` ignores `--json`.** `dispatch/git.rs:85-91` writes the bundle and prints `exported <path>`; with `--json` the process still emits plain text and exits 0. Either emit a JSON result or reject `--json` with `--output`.

7. **Streaming JSON pollution in `process logs --follow` is separate from F4.** `dispatch/runtime.rs:312` returns `{"process_streamed": true}`, and no renderer consumes `process_streamed` (`render/human/sessions.rs:70` only knows console/terminal). The marker is therefore printed as pretty JSON after the log text. The command also ignores `--from-end` in follow mode (`runtime.rs:290` hardcodes `false`) and exits 0 on timeout (`runtime.rs:306-308`).

## Test gaps

- **No daemon-backed CLI tests.** `crates/vp-cli/tests/` contains only backup/repository CLI tests and module governance; there is no HTTP/RPC stub. Therefore parameter forwarding, response rendering, error propagation, and exit codes are untested for essentially the entire command surface.
- **Renderer order is untested.** `tests.rs:3504-3516` calls `print_human` but makes no assertion. The context-checkpoint tests (`tests.rs:1545-1615`) invoke helper functions directly, bypassing the `print_human` first-match order. This is why F3 passes CI.
- **Alias/normalization test only covers the happy path.** `tests.rs:793-828` tests bare `cache warm`, not `--json`/`--data-dir` before the subcommand (F5).
- **No stdin `-` tests.** No test drives `plan-fanout`, `team-plan-revise`, or `task-edit` with piped stdin or `--file -` (F6).
- **No streaming/exit-code tests.** No test covers `console`/`terminal` nonzero exit, timeout, JSON mode, or mid-stream RPC failure (F4), nor `process logs --follow` output/exit behavior.
- **No partial-policy-update tests.** Shell, approval-template, preemption, and memory policies have parse tests but no payload/merge tests (F1, F2, F8, F9).
- **No confirmation-refusal tests.** No test asserts that a noninteractive invocation of `discard`, `checkpoint-restore`, `restore-log`, `process-stop`, or `preempt` fails closed (F10).
- **No RPC timeout/retry/error-code tests.** F7 is unexercised.
- **No non-UTF-8 argv test.** The panic in the secondary observations is unexercised.
- **Backup tests are a positive exception:** `tests/backup_repository_cli.rs` has 10 real subprocess tests covering passphrase env/file rejection, exit codes 1 vs 2, keyring signing/rotation/revocation, rollback, and anchor reset. This is the right shape for the rest of the CLI.

## Clean areas / notable strengths

- **Routing completeness:** a static comparison of all 425 `Command` variants found every variant assigned to a family and every family's inner `match` covering its assigned commands (the only “extras” were `GateCommand` variants, which are nested, not routing errors). All 426 CLI method strings were found in daemon dispatch literals; the only special case is `ping` (`vp-daemon/src/dispatch.rs` routes it to the daemon family explicitly).
- **Backup passphrase handling:** no `--passphrase` value in argv; env name or 0600/symlink-checked file only (`main.rs:947-1009`), explicit mutual exclusion with `--plaintext`, and good subprocess tests.
- **Gate token handling:** `--token` warns about process listings; `--token-stdin` is bounded to 4 KiB; `--token-file` checks size and group/world permissions (`rpc.rs:63-110`).
- **CSV escaping:** `tool_activity_csv` quotes commas, quotes, CR, and LF per cell (`render/support.rs:45-106`); ordering is stable and documented.
- **Explicit confirmations already present:** workspace land, media transcode, test-cache clear, GUI timeline prune, process restart, and daemon-agent install/uninstall are good models for F10.
- **Path handling:** `project add` canonicalizes before sending (`dispatch/project.rs:13-20`); `HostSet` supports literal JSON, a file, or `-` stdin correctly (`dispatch/remote.rs:12-22`); backup restore output is explicit.
- **No `unwrap()`/`expect()` in non-test CLI source** (grep over `crates/vp-cli/src`, excluding tests, found none); error contexts are generally descriptive.
- **Tests pass cleanly:** `cargo test -p vp-cli` → 156 passed, including 10 subprocess backup tests.

## Assumptions and limits

- Baseline is `d239dca`; the working tree was clean before this review (untracked `docs/reviews/` exists from other review sessions).
- No daemon was running and no provider/network calls were made. Daemon behavior for F1/F2/F8/F9 was confirmed by reading engine/store source, not by a live RPC reproduction.
- F3 is established by deterministic renderer ordering and result shapes; I did not run a daemon-backed command to capture the literal output.
- F6 is established by the `read_json_input` implementation and help text; I could not execute the post-discovery path without a running daemon, but the code path is unambiguous.
- The `reqwest` no-timeout claim was verified against the local `reqwest-0.12.28` source (`connect_timeout: None`, `timeout: None`), not against a network reproduction.
- This review does not cover the SwiftUI client, daemon correctness beyond the CLI-facing behavior cited, or provider transports. It also does not claim that every warning-level issue above is a release blocker.
