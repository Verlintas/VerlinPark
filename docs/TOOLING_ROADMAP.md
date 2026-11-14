# VerlinPark Agent Tooling Roadmap

> **Status:** Wave A/B/C, optional LSP and LSP-first symbol/reference/diagnostic tools, provider search, document/data readers, office documents, sandboxed WKWebView browser + unified GUI lease, runtime introspection, native macOS computer control, explicit SSH/stdio remote workspaces, tool-output paging, compaction budgets, fenced leases, hybrid memory, a cost-per-solved-task harness, ACP Streamable HTTP/WebSocket, IVF ANN memory, encrypted incremental backups, cache-first prompt sections/TTL/warm-plan-audit, uniform tool errors/recipes/activity, deterministic Worker/Labour scaling, durable process supervision, semantic project sources, remote bundle integration, a critical-path budget governor, approval attention control, a test-result cache/impact map, precise preemption, multi-path budget fairness, signed/rotatable backups, source ANN/rerank/expansion, safe archives/media, Bitbucket Cloud, backup revocation/rekey, LaunchAgent boot persistence, and remote helper bootstrap/test evidence implemented on 2026-11-12; see ADR-0134/0136/0137/0155–0187.<br>
> **Date:** 2026-11-11<br>
> **Code baseline:** 2026-11-11 tooling + long-task runtime wave; see ADR-0134/0136/0137/0155–0158.<br>
> **Implementation budget:** ~10,000–30,000 code lines, target core ~24k, hard planning cap 30k<br>
> **Purpose:** Preserve the complete missing-tool inventory and design intent across
> context compression. This file is the source of truth for the tooling work; update
> the status column and the "Current implementation queue" as work lands.

---

## 0. How to resume after context compression

1. Read this file, `docs/PROGRESS.md`, and `docs/AUDIT.md`.
2. Confirm the baseline tools in section 2 still match `crates/vp-core/src/tools.rs`
   and `tool_schemas` in `crates/vp-core/src/agent.rs`.
3. Pick the first unchecked item in **Current implementation queue** (section 8).
4. Read the detailed spec for that ID below; do not invent a smaller tool that
   forces the model to do deterministic work.
5. Every new tool needs: schema, runtime implementation, role/permission matrix,
   tests, CLI inspection path when appropriate, and a PROGRESS update.

Do not implement a tool merely because it has a row here. The priority and
budget gates in section 7 decide the order.

---

### Implementation status (2026-11-10)

Implemented in this wave (see ADR-0134/0136/0137 and PROGRESS):

- structured `ToolOutput` + 32 KiB event cap;
- atomic `write_file`/`edit_file`, `apply_patch`, `file_manage`, `find_files`,
  `file_info`, `read_files`;
- `project_profile`, `run_project_task`, `affected_tests`, read-only `git`;
- lexical `repo_map`, `find_symbol`, `find_references`, `dependency_graph`,
  `build_context`, `code_diagnostics`;
- parallel read-only registry tools (max 4, stable ordering);
- `process`, `wait_for`, `local_http`, upgraded `fetch_url` (covers the
  readability/metadata part of W-02; no separate `read_url` tool yet);
- optional `lsp_query` over installed rust-analyzer/sourcekit-lsp/
  typescript-language-server/pyright/gopls/clangd servers;
- provider-backed `web_search` (SearXNG, Brave, Tavily via environment config);
- `read_pdf`, `read_image` (metadata + optional tesseract OCR), and `data_query`
  for JSON/YAML/TOML/CSV;
- runtime `budget_status`, `task_detail`, `task_control`, `tool_search`, and
  `publish_artifact_file` with role-aware dispatch;
- `read_document` for DOCX/PPTX/XLSX and ODT/ODS/ODP with ZIP/XML hardening,
  OCR confidence, and PDF metadata/page counts;
- role/permission matrix, tests, UI tool labels;
- `read_tool_output` with bounded line/byte paging over daemon-owned spilled
  tool outputs, plus `tool_output.*` policy/status/prune/audit;
- optional explicit project-memory embeddings and hybrid `recall`;
- fenced runtime leases and continuation ownership (not a new agent tool).

Implemented browser: a daemon-launched WKWebView helper under its own Seatbelt
profile, with localhost/worktree navigation, DOM snapshots, input, console,
approval-gated `evaluate`, viewport screenshots, and a daemon-wide `gui` lease
shared with native computer. See ADR-0137 for the exact boundaries: page
JavaScript is not network-isolated, the browser requires a GUI session, and the
lease is single-daemon with a bounded FIFO wait, post-action grace, and
snapshot generations, not a distributed fencing token.

Implemented native computer control: a packaged Accessibility/CGEvent/
ScreenCaptureKit helper, bounded AX snapshots with secure-field redaction,
input actions, artifact screenshots, and a risk-classified `computer` tool under
the same daemon-wide `gui` lease. See ADR-0158 and ADR-0169. The helper is not a
sandbox: it runs with the logged-in user's TCC/CGEvent authority, requires
manual Accessibility and Screen Recording grants, AX trees vary by app, refs are
bound to a snapshot generation and stale refs fail closed, sensitive targets are
denied or approval-gated, and GUI/TCC behavior is manual-smoke tested rather
than CI-covered.

Implemented remote workspaces: the `vp-remote` crate and `vp-remote-agent`
helper provide bounded NDJSON JSON-RPC over stdio or SSH, a canonical-root path
jail, atomic expected-SHA writes, bounded exec with process-group cancellation,
structured Git/worktree operations, and an idempotency ledger. Projects opt in
through `ExecutionHost`; the six explicit `remote_*` tools never silently reroute
local tools, and remote write/exec remain approval-classified. See ADR-0156.
Remote exec is not Seatbelt-confined, the helper must be installed, and there is
no remote daemon, cross-host scheduler, or automatic remote-worktree merge.

---

## 1. Product principle: tools are the agent's hands and eyes

The central lesson from the current runtime is:

> **A model is only as good as the tool call it can express.**

A powerful model with a vague tool API will look weak; a modest model with a
precise, contextual tool API can look strong. The runtime should absorb every
deterministic decision that does not require judgment.

### Non-negotiable rules

1. **Push deterministic work to the daemon.** Detect the project type, build
   commands, test layout, language, path jail, nearest test, symbol boundaries,
   output format, and likely next action in Rust. Do not ask the model to guess.
2. **One intent should be one call.** A tool should return the answer, not require
   a chain of read/grep/parse calls. Example: `run_project_task` should resolve
   the test command and return extracted failures, not a 2 MB raw log.
3. **Return structured data plus a short human summary.** Preserve a text
   `content` field for compatibility, but add structured fields for the model and
   UI: `ok`, `summary`, `warnings`, `next_actions`, `truncated`, `cursor`,
   `artifacts`, `evidence`.
4. **Bound everything.** Output size, file size, line count, process output,
   search results, URL bytes, browser snapshots, image/PDF pages. Large raw
   payloads go to artifacts, never into the context window.
5. **Writes are atomic, validated, and reversible where possible.** No direct
   half-write. No path outside the worktree. No `.git` writes. Dry-run and
   expected hashes for destructive operations.
6. **Tools never bypass existing safety gates.** Approval, risk classification,
   Seatbelt, path jail, write scope, Verifier, capacity, and budget rules stay.
   A tool is a better interface, not a new authority.
7. **Do not add hidden model routing or hidden tools.** Tool availability is
   explicit per role/profile; unknown tools are denied.
8. **Prefer deterministic fallbacks.** LSP unavailable → tree-sitter/regex;
   browser unavailable → HTTP/fetch; OCR unavailable → explicit error with a
   suggested alternative. Never silently degrade to an unsafe action.
9. **Tool schema must fit the model.** Small flat parameters, enums, defaults,
   one required action, descriptions that state when not to use the tool.
10. **Keep `shell` as the escape hatch.** New tools wrap common high-value
    operations; they must not make the system less capable than shell.

### What "smart" means in this roadmap

Not an LLM inside every tool. Smart means:

- project-aware defaults;
- deterministic parsing and classification;
- automatic context assembly;
- structured results with evidence;
- safe retries and precise error locations;
- caching with explicit invalidation;
- capability detection and actionable fallbacks.

---

## 2. Pre-wave tool surface (baseline `926d9ef`, historical)

This section records the original seven-tool baseline for design context. The
post-wave tool surface is listed in section 0 and in `docs/PROGRESS.md`.

### 2.1 Core file/code tools (the 7 base tools)

| Tool | Current capability | Known limitation |
|---|---|---|
| `read_file` | UTF-8 text, line numbers, offset/limit | no hash/encoding metadata, no batch |
| `list_dir` | one directory, names/sizes | no recursion, no pagination, no glob |
| `grep` | Rust regex, optional glob filter | raw line output, no context, no structured match |
| `edit_file` | exact string replace, `replace_all` | single file, non-atomic, no diff/return hash |
| `write_file` | full-file write, creates parents | non-atomic, no expected hash, no diff |
| `shell` | sandboxed one-shot command, 900s cap | no persistent process, output not split/structured |
| `fetch_url` | public HTTP(S) text fetch, no redirects | no search, no readability, no links/metadata |

### 2.2 Shared orchestration / knowledge / communication tools

| Tool | Purpose |
|---|---|
| `send_message`, `read_mailbox` | session typed mailbox |
| `publish_artifact`, `list_artifacts`, `read_artifact` | text/binary artifact access |
| `search_sources`, `read_source`, `list_sources` | project knowledge sources |
| `remember`, `recall` | project memory |
| `use_skill` | activate a project skill |
| `coordinate`, `team_status` | team board, blockers, handoffs |
| `ask_user` | blocking user question |
| `mcp` | MCP inspect/call plus explicit async task list/get/result/cancel |
| dynamic MCP tools | up to 32 tools for Leader/Worker/Labour |
| `todo_write` | Leader todo list |
| `delegate`, `delegate_many`, `delegate_acp` | delegation |
| `submit_team_plan`, `plan_fanout`, `run_team_plan`, `deliberate_team_plan` | Leader planning (fan-out is explicit HostState-approved plan surgery) |
| `submit_verdict` | Verifier decision |

### 2.3 Role matrix (current)

| Role | Base tools | Special restrictions |
|---|---|---|
| Leader | all 7 | can plan, delegate, use todo |
| Manager | read/list/grep; research can fetch | code read-only; delegates only |
| Verifier | read/list/grep/shell; research can fetch | no write/edit; isolated clone; verdict only |
| Worker/Labour coding | all 7 | can edit/write/shell/fetch |
| Worker/Labour research | read/list/grep/write/fetch | no shell/edit |
| Worker/Labour verification | read/list/grep/shell/fetch | no write/edit |
| Worker/Labour generic | read/list/grep | no external fetch |

### 2.4 Baseline verdict

The base seven tools are enough for **ordinary coding** because `shell` is a
universal escape hatch. They are not enough for **large engineering** because
editing is non-atomic and context-free, there is no code intelligence, no
persistent process, no browser/GUI, no web search, and tool calls execute
serially.

---

## 3. Gap summary

Legend: **P0** = core usability; **P1** = high-value; **P2** = stretch; **P3** =
defer. LOC is an implementation estimate including tests and wiring, not a
promise.

| ID | Tool / change | Tier | Primary roles | Why it matters | LOC |
|---|---|---|---|---|---|
| E-01 | Atomic `write_file` / `edit_file` + expected hash + diff result | P0 | Leader, coding Worker/Labour | prevents corruption and blind edits | 500–800 |
| E-02 | Structured `read_file` metadata (hash, encoding, total lines) | P0 | read roles | lets agents detect stale/partial reads | 250–400 |
| E-03 | Structured `grep` (context, file grouping, type, path-only, cursor) | P0 | read roles | removes output parsing and repeat calls | 400–650 |
| E-04 | Paginated `list_dir` (hidden/ignored/type/limit) | P1 | read roles | large directory usability | 250–400 |
| E-05 | Structured `shell` result (exit/stdout/stderr/duration/log artifact) | P0 | shell roles | models stop parsing one blob | 400–700 |
| E-06 | `fetch_url` upgrade (redirects, readability, metadata, links) | P1 | research/Leader | research quality and token economy | 500–800 |
| F-01 | `apply_patch` multi-file, dry-run, conflict-safe | P0 | Leader, coding Worker/Labour | the single biggest editing gap | 1,200–1,800 |
| F-02 | `find_files` glob/recursive file discovery | P0 | read roles | replaces shell `find`/`rg --files` guessing | 400–600 |
| F-03 | `file_manage` move/copy/delete/mkdir, dry-run | P0 | Leader, coding Worker/Labour | structured, safe file operations | 500–800 |
| F-04 | `read_files` batch ranges | P1 | read roles | fewer tool calls for multi-file review | 300–500 |
| F-05 | `file_info` stat/hash/MIME/binary/git state | P0 | read roles | decide how to read/edit without trial | 250–400 |
| P-01 | `project_profile` detects languages/commands/conventions | P0 | all | agents stop guessing build/test commands | 1,000–1,600 |
| P-02 | `run_project_task` build/test/lint/format/typecheck with parsed results | P0 | Leader, Worker/Labour, Verifier | turns raw logs into actionable failures | 2,000–3,000 |
| G-01 | `git` read-only query: status/diff/log/show/blame/files | P0 | read roles | structured Git state instead of shell parsing | 800–1,200 |
| R-01 | Parallel read-only tool execution | P0 | runtime | lowers latency and token churn | 400–700 |
| R-02 | Structured tool-result contract | P0 | runtime/all tools | uniform errors, truncation, next actions | 500–800 |
| C-01 | `repo_map` symbol/file map | P1 | read roles | compact repository understanding | 1,800–2,800 |
| C-02 | `find_symbol` by name/kind/signature | P1 | read roles | precise navigation without grep loops | 500–800 |
| C-03 | `find_references` | P1 | read roles | safe refactor impact analysis | 800–1,200 |
| C-04 | `code_diagnostics` | P1 | Leader, Worker/Labour, Verifier | immediate errors without full build | 1,200–1,800 |
| C-05 | `lsp_query` definition/hover/symbols (optional server) | P2 | read roles | exact language intelligence when available | 1,500–2,200 |
| C-06 | `affected_tests` | P1 | Leader, Worker/Labour, Verifier | run the right tests, not the world | 500–800 |
| C-07 | `dependency_graph` imports/imported-by | P1 | read roles | refactor and architecture navigation | 500–800 |
| C-08 | `build_context` token-budgeted context packet | P1 | Leader/Manager/Worker | replaces exploratory tool chains | 1,200–1,800 |
| S-01 | `process` start/list/read/write/signal/wait/stop | P1 | Leader, Worker/Labour, Verifier | dev servers, watch, interactive tools | 1,500–2,200 |
| S-02 | `wait_for` port/URL/log condition | P1 | same as process | deterministic readiness, no sleep guessing | 200–350 |
| S-03 | `local_http` structured localhost/API requests | P1 | Leader, Worker/Labour, Verifier | API/backend testing without shell curl | 600–900 |
| W-01 | `web_search` provider-backed search | P1 | research/Leader | discover sources, not just fetch known URLs | 800–1,200 |
| W-02 | `read_url` readable article/metadata/citations | P1 | research/Leader | bounded research input | 500–800 |
| U-01 | `browser` accessibility/DOM actions + screenshots | P2 | Leader, coding/frontend | web UI verification | 3,000–5,000 |
| U-02 | `computer` macOS AX/screenshot/input + lease | P2 | Leader, coding | native app/UI verification | 2,500–4,000 |
| M-01 | `read_image` dimensions/OCR/artifact | P2 | all read roles | screenshots and design references | 600–1,000 |
| M-02 | `read_pdf` text/pages/metadata | P1 | research/Leader | document research | 800–1,500 |
| M-03 | `read_document` docx/pptx/xlsx | P3 | research | office documents | 1,500–2,500 |
| A-01 | `publish_artifact_file` | P1 | write-capable roles | screenshots/logs/reports as artifacts | 250–400 |
| T-01 | `budget_status` remaining/spent/unlimited | P1 | all | agent can decide when to stop/ask | 200–350 |
| T-02 | `task_detail` acceptance/deps/attempts/budget | P1 | Leader/Manager/Worker | self-directed task execution | 250–400 |
| T-03 | `tool_search` v2 BM25 + prerequisite closure + turn-boundary schema promotion | P1 | Leader/Worker/Labour | exact set discovery, no silent schema churn | 700–1,000 |
| T-04 | `task_control` skip/retry/cancel/pause/resume own plan task | P2 | Leader/Manager | controlled self-recovery for long plans | 250–400 |
| D-01 | `data_query` JSON/YAML/CSV/TOML query | P2 | read roles | deterministic config/data inspection | 500–800 |

### Budget shape

| Group | Estimated LOC |
|---|---:|
| Existing-tool upgrades (E) | 2,300–3,750 |
| File/editing/project/Git/runtime (F, P, G, R) | 7,600–10,600 |
| Code intelligence (C) | 6,800–10,100 |
| Processes/services (S) | 2,300–3,450 |
| Web + minimal documents (W, M-02) | 2,100–3,500 |
| Media/artifact/task introspection (M-01, A, T) | 1,550–2,550 |
| Data query (D-01) | 500–800 |
| **Core total without browser/computer/document office** | **23,650–36,900 conservative** |
| Browser/computer (U) | 5,500–9,000 stretch |
| Office documents (M-03) | 1,500–2,500 stretch |

The target is **~24k LOC** for the core. The upper ranges are conservative
estimates including tests and ecosystem parsers, not an additive promise. If the
core implementation crosses 24k, drop optional items in this order: `C-05`
LSP, `T-04` task control, `D-01` data query, then optional parsers/edge cases.
If it crosses 30k before the core is usable, stop and re-plan rather than
shipping half-tools. Browser and office documents were promoted after the core
was green; native macOS computer control and remote workspaces also shipped in
the 2026-11-11 long-task runtime round, beyond the original core cap.

---

## 4. Detailed specifications: core wave

### E-01 — Atomic file writes and edits

**Problem:** `write_file` and `edit_file` currently call `tokio::fs::write`
directly. A crash, cancellation, or full disk can leave a truncated file. The
model also gets no diff and must re-read to verify.

**Change:**

- Add a shared `atomic_write(path, bytes)` helper:
  - create a temp file in the same directory;
  - write, flush, fsync;
  - preserve existing mode where possible;
  - rename over the target;
  - fsync the parent directory on Unix;
  - clean up temp on failure.
- `write_file`:
  - optional `expected_sha256` (reject stale overwrite);
  - optional `create_parents` (default true);
  - return `sha256`, bytes, line count, and a bounded diff summary.
- `edit_file`:
  - optional `expected_sha256`;
  - optional `context_lines` for ambiguity resolution;
  - reject ambiguous replacements unless `replace_all` or a unique context;
  - return `replacements`, before/after hash, and a bounded diff;
  - support CRLF without changing unrelated line endings.
- Add a `dry_run` option for both.

**Deterministic intelligence:** hashing, context window extraction, unique-match
selection, atomicity, diff generation.

**Tests:** crash-safe temp cleanup, expected-hash conflict, ambiguous edit,
CRLF, file mode, symlink/path escape, dry-run no write.

**LOC:** 500–800.

---

### E-02 / E-03 / E-04 / E-05 — Better read/search/shell results

- `read_file`: add `sha256`, `encoding`, `total_lines`, `truncated`, `next_offset`.
- `grep`: return `matches` grouped by file, optional `context_before/after`,
  `type`/`glob`, `path_only`, `case_sensitive`, `max_files`, `cursor`. Keep a
  short text rendering for compatibility.
- `list_dir`: add `recursive`? No — keep one level; add `limit`, `cursor`,
  `include_hidden`, `include_ignored`, `kind`, `sort`.
- `shell`: split `stdout`/`stderr`, preserve exit code, duration, truncation,
  and write full log to an artifact when over the context cap. Add `stdin`? No;
  interactive stdin belongs to `process`.

**LOC:** 1,150–2,150 across E-02..E-05.

---

### F-01 — `apply_patch`

**Tool name:** `apply_patch`

**Input:**

```json
{
  "patch": "unified diff text",
  "dry_run": false,
  "create_files": false,
  "delete_files": false,
  "expected_hashes": {"path": "sha256"},
  "format_after": false,
  "max_files": 20
}
```

**Output:**

```json
{
  "ok": true,
  "files": [
    {"path": "src/a.rs", "additions": 12, "deletions": 3, "status": "modified"}
  ],
  "summary": "2 files changed, +18 -4",
  "diff": "bounded unified diff",
  "warnings": [],
  "next_actions": ["run_project_task test"]
}
```

**Deterministic behavior:**

- Parse unified diff or structured hunks.
- Normalize CRLF; reject NUL/binary.
- Resolve all paths under the worktree; reject absolute, `..`, `.git`, symlink
  escapes.
- Apply hunks with exact context; if a hunk is ambiguous, return a precise error
  with candidate line numbers. Do not guess.
- Validate all files before writing any file.
- Apply each file atomically; on failure, restore previously written files from
  in-memory backups.
- Optional post-format only through the resolved project formatter, under the
  shell sandbox.
- Return a compact diff, not the whole file.

**Why it matters:** This is the difference between "edit a file" and "perform a
reviewable change". It should become the preferred write tool for coding agents;
`write_file` remains for new/small files.

**Roles:** Leader, coding Worker/Labour. Research may use it only if an explicit
project policy allows code writes; generic/verification never.

**Tests:** add/update/delete/create, multi-file, overlapping hunks, context
mismatch, path escape, expected hash conflict, CRLF, dry-run, rollback.

**LOC:** 1,200–1,800.

---

### F-02 — `find_files`

**Input:** `pattern` (glob), `path`, `limit`, `cursor`, `kind`, `include_hidden`,
`include_ignored`, `sort`.

**Deterministic behavior:** use the same ignore rules as grep; reject path
escapes; return `path`, `type`, `size`, `mtime`, and a cursor. Group by directory
optionally. Do not ask the model to compose `find` pipelines.

**Roles:** all read roles.

**LOC:** 400–600.

---

### F-03 — `file_manage`

**Actions:** `move`, `copy`, `delete`, `mkdir`.

**Input:** action, `source`, `destination`, `recursive`, `dry_run`,
`expected_hashes`.

**Behavior:**

- Canonical worktree jail; never `.git`.
- Delete is reversible only to the extent of a trash/artifact copy; for v1,
  return an explicit warning and require approval for recursive delete.
- Move/copy are atomic where possible; reject overwriting without explicit
  `overwrite`.
- Return affected paths and a diff/stat summary.

**Roles:** Leader, coding Worker/Labour.

**LOC:** 500–800.

---

### F-04 / F-05 — batch read and file info

- `read_files`: list of `{path, offset, limit}`, total byte cap, result sections,
  `truncated` and per-file hash.
- `file_info`: `path`; returns size, mode, mtime, sha256, MIME, UTF-8/binary,
  line count, git tracked/ignored/status, symlink target.

**LOC:** 550–900.

---

### P-01 — `project_profile`

This is a high-leverage "do not make the model guess" tool.

**Input:** optional `path`, `refresh`.

**Output:**

```json
{
  "root": ".",
  "languages": [{"name": "rust", "confidence": 0.99, "files": 214}],
  "package_managers": ["cargo"],
  "commands": {
    "build": {"command": "cargo build --workspace", "source": "Cargo.toml", "confidence": 0.95},
    "test": {"command": "cargo test --workspace", "source": "Cargo.toml", "confidence": 0.95},
    "lint": {"command": "cargo clippy --workspace --all-targets -- -D warnings", "source": "justfile", "confidence": 0.9},
    "format": {"command": "cargo fmt --all", "source": "justfile", "confidence": 0.9}
  },
  "test_layout": {"roots": ["crates"], "patterns": ["tests/*.rs"]},
  "conventions": [
    {"path": "AGENTS.md", "summary": "Read before changing code"},
    {"path": "CONTRIBUTING.md", "summary": "..."}
  ],
  "ci": [{"path": ".github/workflows/ci.yml", "commands": ["..."]}],
  "warnings": [],
  "truncated": false
}
```

**Detectors:**

- Rust: `Cargo.toml`, workspace members, `justfile`, `Makefile`, CI.
- Swift: `Package.swift`, `.xcodeproj`, XcodeGen `project.yml`, `justfile`.
- Node: `package.json` scripts, lockfiles, workspace config, Vite/Next/Turbo.
- Python: `pyproject.toml`, `requirements.txt`, `tox.ini`, `pytest.ini`.
- Go: `go.mod`, `Makefile`.
- JVM/Kotlin: Gradle/Maven.
- C/C++: CMake/Makefile.
- Polyglot: return multiple command candidates with confidence and source.
- Conventions: `AGENTS.md`, `CONTRIBUTING.md`, `README`, `.editorconfig`, lint
  configs, test naming.
- Command provenance is mandatory. Never return an invented command without a
  source or a low-confidence marker.

**Roles:** all read roles. Manager/Verifier can use it; Manager still cannot run
the commands.

**LOC:** 1,000–1,600 (common ecosystems + fixture tests).

---

### P-02 — `run_project_task`

**Purpose:** replace "guess the test command, run it via shell, parse 3,000 lines
of output, guess the failure" with one deterministic call.

**Input:**

```json
{
  "kind": "test",
  "package": "vp-core",
  "filter": "sources_",
  "paths": ["crates/vp-core/src/sources.rs"],
  "changed_only": false,
  "timeout_secs": 300,
  "format": false,
  "dry_run": false
}
```

`kind`: `build | test | lint | format | typecheck | dev | custom`.

**Behavior:**

- Resolve the command from `project_profile`; allow an explicit `command` only
  for `custom`.
- Run under the existing shell sandbox, environment allowlist, process group,
  working directory, and approval policy.
- Parse known outputs:
  - `cargo test` / `cargo build` / `cargo clippy`;
  - `swift test` / `xcodebuild test`;
  - npm/pnpm/yarn scripts and Jest/Vitest;
  - pytest;
  - Go test;
  - common Makefile/just targets.
- Return:
  - exit code, duration, command/source;
  - failed tests/build errors with `path`, `line`, `code`, `message`, `test`;
  - warning count;
  - bounded raw tail;
  - full log artifact when output is large;
  - `next_actions` (for example "open file at line", "rerun failed tests").
- `changed_only` uses `affected_tests`; `filter` maps to the native test filter.
- No automatic fix, no model call, no hidden routing.

**Roles:** Leader, coding/research Worker/Labour (research may run tests if the
project has them), Verifier. Manager must not execute commands.

**LOC:** 2,000–3,000 including parsers and fixtures.

---

### G-01 — `git`

**Name:** `git` (one action-based tool to keep the schema count low) or
`git_status`/`git_diff`/`git_log` if tool-selection quality proves better. Start
with one action-based tool.

**Actions:**

- `status`: branch, HEAD, upstream, ahead/behind, staged/unstaged/untracked,
  conflicts, clean flag, worktree path.
- `diff`: scope (`unstaged`, `staged`, `base`, `commit`), paths, stat-only,
  context; return structured hunks and a bounded patch.
- `log`: path, limit, since; commits with hash, author, date, subject, files.
- `show`: commit and optional path; stat + bounded patch.
- `blame`: path, line range; author/commit/date per line.
- `files`: tracked files at a commit or in the worktree.

**Read-only:** no commit/stage/branch mutation in this tool. The runtime owns
Worker commits and integration. A future `git_write` tool is explicitly out of
scope until the approval/rollback design is proven.

**LOC:** 800–1,200.

---

### R-01 — Parallel read-only tool calls

`run_loop` currently executes every tool call sequentially. Keep writes and
approval-sensitive tools serial, but execute independent read-only calls
(`read_file`, `read_files`, `list_dir`, `grep`, `find_files`, `file_info`,
`git` read actions, `repo_map`, `find_symbol`, `web_search`, `read_url`, etc.)
with bounded concurrency.

**Rules:**

- Maximum 4–8 concurrent calls.
- Stable result ordering matching the model's call IDs.
- If any call is approval-sensitive or writes, flush/execute in order.
- Per-session concurrency and tool registry concurrency limits.
- Cancellation kills the whole batch consistently.
- No parallel calls to tools that mutate shared caches without locks.

**Why:** model-emitted parallel tool calls are a latency and token-efficiency
feature; serial execution wastes both.

**LOC:** 400–700 plus tests.

---

### R-02 — Structured tool result contract

Extend `ToolOutput` (or add a sibling structured field) without breaking the
existing text protocol:

```json
{
  "ok": true,
  "summary": "2 files changed, +18 -4",
  "data": {},
  "warnings": [],
  "next_actions": [],
  "artifacts": [],
  "truncated": false,
  "cursor": null,
  "elapsed_ms": 42
}
```

- The provider still receives a single tool message; the text is a concise
  rendering of `summary` + key data + warnings.
- The UI can render structured cards.
- Tool implementations return both forms through a helper.
- Never place secrets in `data`.
- Cap serialized structured data (for example 32 KB); spill to artifacts.

**LOC:** 500–800 across runtime/protocol/UI.

---

## 5. Detailed specifications: code intelligence wave

### C-01 — `repo_map`

**Purpose:** Give the agent a compact map before it reads files.

**Input:** `path`, `max_files`, `max_symbols`, `include_tests`, `refresh`,
`format` (`text|json`).

**Output:** languages, top-level modules, public symbols with path/line/signature,
test files, config files, package boundaries, symbol counts, truncation and
cache age.

**Implementation:**

- Tree-sitter for Rust, Swift, TS/JS, Python, Go, Java/Kotlin, C/C++ where
  grammars are practical; fallback to a bounded regex/indentation scanner for
  other languages.
- Cache under the session scratch directory keyed by worktree + file mtimes +
  content hashes; invalidate on change.
- Never follow symlinks outside the worktree; never read `.git`.
- No embeddings; deterministic symbol extraction.

**Why:** It removes the "grep for a class name, open five files, guess the
architecture" loop.

**LOC:** 1,800–2,800.

---

### C-02 / C-03 — `find_symbol` and `find_references`

- `find_symbol`: `query`, `kind`, `language`, `path`, `limit`, `include_body`.
  Returns symbol ID, path, line range, signature, container, visibility, and a
  bounded snippet.
- `find_references`: `symbol_id` or `path` + `line` + `name`; returns references
  with path/line/context and a `confidence` (`exact-lsp`, `tree-sitter`,
  `lexical`). If only lexical is available, say so.
- Never claim exact references when only textual matching was used.

**LOC:** 1,300–2,000.

---

### C-04 / C-05 — diagnostics and LSP

`code_diagnostics`:

- Input: `paths`, `severity`, `limit`, `source` (`auto|lsp|build`).
- Output: file/line/column/severity/code/message/source, grouped by file.
- Prefer an already-running LSP; otherwise resolve a project task (`check` or
  `lint`) and parse errors. Return a clear "no diagnostics available" state.

`lsp_query` (shipped, optional):

- Actions: `definition`, `references`, `hover`, `symbols`, `workspace_symbols`,
  `diagnostics`, `capabilities`, `status`.
- `find_symbol`, `find_references`, and `code_diagnostics` can use the same
  backend; every result labels `source: lsp|lexical|build`, confidence, backend
  method, and fallback reason.
- Manage one LSP server per language/worktree/session with idle shutdown,
  bounded frames, timeout/cancel, crash restart, and cooldown.
- Discover common servers (`rust-analyzer`, `sourcekit-lsp`,
  `typescript-language-server`, `pyright-langserver`, `gopls`, `clangd`) from
  PATH; never auto-install. Missing servers return `LSP_UNAVAILABLE` with hints.
- LSP is an optimization; lexical/build fallback is explicit. Rename,
  completion, formatting, code actions, and signature help are not implemented.

**LOC:** 1,200–2,200 combined, depending on LSP scope.

---

### C-06 — `affected_tests`

**Input:** `changed_paths` or `base_ref`, `test_command`, `limit`.

**Behavior:**

- Map changed source files to test files by naming conventions, nearest test
  directory, module declarations, and import edges from `repo_map`.
- Return `tests` with path, reason, confidence.
- If no mapping is found, return the project test command and a warning, not a
  fabricated test name.
- `run_project_task` can call it internally with `changed_only`.

**LOC:** 500–800.

---

### C-07 — `dependency_graph`

**Input:** `path` or `package`, `direction` (`imports|imported_by|both`), `depth`,
`include_external`.

**Output:** nodes/edges with path, import target, external/internal, cycles.
Use tree-sitter/import parsing; fallback to regex with confidence.

**LOC:** 500–800.

---

### C-08 — `build_context`

This is the highest-level "smart" context tool.

**Input:** `goal`, optional `paths`, `max_tokens` (default 4,000), `include`
(`code|tests|docs|git|sources|memory`), `focus`.

**Behavior:**

- Use lexical/symbol retrieval over `repo_map`, `grep`, git diff, project
  profile, existing sources, and memory.
- Return a token-budgeted context packet:
  - task summary / detected conventions;
  - relevant symbols with file/line/snippet;
  - relevant test files and command;
  - recent git changes;
  - warnings/uncertainties;
  - citations to files and source chunks.
- Never read the whole repo; always report `truncated` and why.
- No model call and no embeddings in v1.

**Why:** It replaces the exploratory chain of list/grep/read/grep/read and keeps
the model's context clean.

**LOC:** 1,200–1,800.

---

## 6. Detailed specifications: services, web, media, UI

### S-01 — `process`

**Actions:** `start`, `list`, `status`, `read`, `write`, `signal`, `wait`, `stop`.

**Input:** `command`, `cwd`, `env` (allowlisted explicit values), `name`,
`timeout_secs`, `idle_ttl_secs`, `process_id`, `input`, `signal`, `pattern`.

**Behavior:**

- One process registry per session, bounded (for example 8 processes).
- Start under the shell sandbox with process group, output ring buffer (for
  example 2 MB), and optional port detection.
- `read` returns incremental output with a cursor; full logs spill to artifacts.
- `wait` supports exit, regex, port readiness, or URL health.
- `write` handles stdin for REPLs/servers; no secrets in logs.
- Cleanup on session end, timeout, cancel, and daemon sweep.
- Persistent across agent turns but not across daemon restart (v1).

**Roles:** Leader, Worker/Labour, Verifier. Manager no.

**LOC:** 1,500–2,200.

---

### S-02 — `wait_for`

**Input:** `port` or `url` or `log_pattern`, `timeout_secs`, `interval_ms`.
Local-only by default; return readiness evidence and last error.

**LOC:** 200–350.

---

### S-03 — `local_http`

**Input:** `method`, `url` (localhost/127.0.0.1/[::1] or project allowlist),
`headers`, `json`/`body`, `timeout_secs`, `follow_redirects` (local only).

**Behavior:**

- Reject external hosts unless the project policy explicitly allows them.
- Redact `Authorization`, cookies, tokens from logs; never return secret headers.
- Return status, selected headers, parsed JSON/text, body cap, and a request ID.
- No ambient credentials; headers must be explicit or resolved from a
  Keychain-backed named secret reference.
- GET/HEAD read-only; other methods require the normal approval path.

**LOC:** 600–900.

---

### W-01 — `web_search`

**Input:** `query`, `limit`, `site`, `recency`, `provider`.

**Behavior:**

- Use an explicitly configured search provider (Brave/Tavily/SearXNG/custom);
  no hidden model or search route.
- Return title, URL, snippet, source, rank, and a stable citation ID.
- Never fabricate a result; provider errors are explicit.
- Cache with short TTL; respect result caps and rate limits.

**LOC:** 800–1,200 plus provider config/UI/CLI.

---

### W-02 / E-06 — `read_url`

**Behavior:**

- Follow redirects with SSRF revalidation on every hop (max 5).
- Resolve DNS once and pin addresses; block private/loopback/link-local.
- Extract readable text/Markdown, title, author/date when present, canonical URL,
  links, content hash, and content type.
- Support charset detection; reject binary unless `read_pdf`/`read_image` is
  selected.
- Return citation IDs; source ingestion can reuse the same snapshot.
- Bound bytes/chars and report truncation.

**LOC:** 500–800 beyond the existing fetch helper.

---

### U-01 — `browser`

**Status:** Implemented 2026-11-10; see ADR-0137.

**Actions:** `open`, `snapshot`, `click`, `type`, `press`, `select`, `wait_for`,
`screenshot`, `console`, `evaluate`, `close`. (`evaluate` is approval-gated and
classified `HostState`; the other write/input actions are `WorkspaceWrite`, and
inspection/close are read-only.)

**Behavior:**

- Prefer a local browser helper over shelling out to a user browser.
- `snapshot` returns an accessibility/DOM tree with stable role/name/selector
  labels, visible text, URL, title, and viewport; not raw HTML by default.
- `click`/`type` target role+name or a stable selector; deterministic wait after
  actions; return the changed snapshot or a minimal diff.
- `screenshot` stores an artifact, returns dimensions and optional OCR text.
- `console` returns bounded logs; network inspection is opt-in.
- `evaluate` JavaScript is an explicit approval-gated action, not the default.
- The shared GUI lease serializes browser and native computer use per daemon.
- Local URLs only by default; external navigation requires policy.

**Decision:** WKWebView helper selected over Chrome/CDP or a Playwright sidecar;
see ADR-0137. The helper runs under a write-confined Seatbelt profile and is
serialized by the shared `gui` lease with a bounded wait queue and post-action grace.

**Implemented boundaries:** localhost/worktree `file:` navigation only; DOM
snapshot rather than native AX; synthetic input; viewport-only screenshots;
no page-script network isolation; logged-in GUI session required;
single-daemon fail-fast lease with no queue or fairness policy.

**LOC:** 3,000–5,000. Shipped; native `computer` also shipped separately in the 2026-11-11 round (ADR-0158).

---

### U-02 — `computer`

**Actions (shipped):** `permissions`, `apps`, `windows`, `focus`, `snapshot`,
`click`, `type`, `key`, `scroll`, `drag`, `wait`, `screenshot`.

**Behavior:**

- macOS Accessibility tree with role/title/value/frame; stable snapshot-scoped
  `ax-…` refs; secure/password fields are redacted.
- Screenshots are bounded PNG artifacts under daemon scratch, never unbounded
  inline model data.
- The shared `gui` lease serializes native GUI and browser access on this daemon, with snapshot generations and stale-ref rejection.
- Never type secrets into arbitrary apps without approval.
- Destructive key combinations (cmd+q, cmd+shift+delete, …) are classified
  Destructive; focus/input is HostState; read-only inspection is External.
- The helper is launched by the daemon, uses a scratch HOME, and has bounded
  protocol frames, request timeouts, idle shutdown, and process-group cleanup.

**Boundaries:** not sandboxed; requires logged-in GUI session and manual TCC
grants; AX trees vary by application; no menu-bar walker, AppleScript, clipboard,
URL opener, or shell path. See ADR-0158 and
`apps/macos/VerlinParkComputerHelper/README.md`.

**LOC:** 2,500–4,000 planned; shipped in the 2026-11-11 long-task runtime round.

---

### M-01 — `read_image`

**Input:** worktree path or artifact ID, `ocr`, `max_chars`.

**Output:** dimensions, MIME, size, OCR text, optional artifact ID, warnings.
Use Apple Vision OCR when available; otherwise return metadata and a clear
"OCR unavailable" state. Never pretend to see image content without vision/OCR.

**LOC:** 600–1,000.

---

### M-02 — `read_pdf`

**Input:** path/artifact, pages/ranges, `max_chars`.

**Output:** page text with page numbers, metadata, truncation, warnings.
Use PDFKit/poppler or a bounded Rust parser; no OCR by default (pair with
`read_image` only if explicitly needed).

**LOC:** 800–1,500.

---

### M-03 — `read_document`

DOCX/PPTX/XLSX text and tables, bounded pages/sheets. Defer until research
demand is proven.

**LOC:** 1,500–2,500.

---

### A-01 — `publish_artifact_file`

Publish an existing worktree file (screenshot, log, report) as a binary artifact:
MIME detection, size cap, SHA-256, retention, returned artifact ID. Reuses the
existing content-addressed store and GC.

**LOC:** 250–400.

---

### T-01 / T-02 / T-03 — introspection and tool discovery

- `budget_status`: session/task/department/project remaining/spent, unlimited
  flag, reservations; no secrets. Helps the agent decide when to ask or split.
- `task_detail`: acceptance, deps, attempts, write scope, budget, changed paths,
  current status. `team_status` remains the broad board.
- `tool_search` v2: BM25 over name/category/task-kind/description plus bounded
  prerequisite closure and explicit `missing`/`completeness_estimate`; MCP
  results stay pin/trust gated. `promote=true` applies out-of-bundle built-in
  schemas only at the next turn boundary through a priced `bundle.revision`;
  role restrictions and approvals still apply.
- `task_control` (P2): Leader/Manager-only, event-backed `skip`, `retry`,
  `cancel`, `pause`, `resume` for a task in its own plan. Do not expose this
  before the task lifecycle atomicity gaps in AUDIT A-14 are closed.
- `data_query` (P2): bounded query over JSON/YAML/CSV/TOML with a documented
  JSONPath/JQ-like subset; return matches/paths and never execute arbitrary code.
  Useful for config, manifests, and test fixtures without dumping whole files.

**LOC:** 1,600–2,550.

---

## 7. Implementation waves and budget gates

### Wave A — "Single agent can edit and verify reliably" (~8–11k)

Order:

1. R-02 structured results
2. E-01/E-02/E-03/E-05 upgrades
3. F-01 `apply_patch`
4. F-02 `find_files`, F-03 `file_manage`, F-05 `file_info`
5. P-01 `project_profile`
6. P-02 `run_project_task`
7. G-01 `git`
8. R-01 parallel read-only calls

Exit criteria:

- A Worker can inspect a repo, make a multi-file patch, run the detected tests,
  parse failures, fix them, and verify, using fewer than half the tool calls of
  the current flow.
- No direct non-atomic write path remains in agent tools.
- The model never needs to guess the build/test command for a supported project.
- All new write paths pass path-jail, expected-hash, dry-run, and rollback tests.

### Wave B — "No blind repository exploration" (~6–9k)

1. C-01 `repo_map`
2. C-02 `find_symbol`
3. C-03 `find_references`
4. C-04 `code_diagnostics`
5. C-06 `affected_tests`
6. C-07 `dependency_graph`
7. C-08 `build_context`
8. C-05 LSP only if server management is stable

Exit criteria:

- Common "where is this defined / used / what should I read" questions resolve in
  one call.
- `build_context` produces a bounded, cited packet for a task goal.
- Supported languages navigate without a grep/read loop; unsupported languages
  degrade explicitly.

### Wave C — "Services, web research, and media" (~5–8k)

1. S-01 `process`
2. S-02 `wait_for`
3. S-03 `local_http`
4. W-01 `web_search`
5. W-02/read_url
6. M-02 `read_pdf`
7. M-01 `read_image`
8. A-01 `publish_artifact_file`
9. T-01/T-02/T-03

Exit criteria:

- A frontend/backend agent can start a dev server, wait for readiness, make a
  local API request, inspect logs, and stop the process.
- Research can discover a source, read it with citations, and report evidence.
- Images/PDFs produce bounded text/metadata instead of forcing shell guessing.

### Wave D — "UI/computer" (shipped)

The browser slice shipped with a WKWebView helper and a daemon lease (now the unified `gui` lease); native
`computer` control and remote SSH workspaces shipped in the 2026-11-11 long-task
runtime round (ADR-0156/0158). Both remain bounded by the documented trust
model: browser page JavaScript is not network-isolated, and native GUI control
is not sandboxed. Keep `browser` and `computer` on the same lease surface.

### Hard stop rules

- If Wave A exceeds 11k, stop and reassess before Wave B.
- If total exceeds 24k before Wave C, drop browser/computer/office documents and
  finish the core.
- Never reduce safety tests to fit the line budget.
- Never add a tool without a role matrix and an integration test.

---

## 8. Current implementation queue

Statuses: `[ ]` not started, `[~]` in progress, `[x]` done, `[–]` deferred.

```text
[x] R-02 structured tool result contract
[x] E-01 atomic write/edit + hash + diff
[x] E-02 structured read_file
[x] E-03 structured grep
[x] E-04 paginated list_dir
[x] E-05 structured shell result
[x] E-06 fetch_url upgrade
[x] F-01 apply_patch
[x] F-02 find_files
[x] F-03 file_manage
[x] F-04 read_files batch
[x] F-05 file_info
[x] P-01 project_profile
[x] P-02 run_project_task
[x] G-01 git
[x] R-01 parallel read-only tool calls

[x] C-01 repo_map
[x] C-02 find_symbol
[x] C-03 find_references
[x] C-04 code_diagnostics
[x] C-06 affected_tests
[x] C-07 dependency_graph
[x] C-08 build_context
[x] C-05 lsp_query
[x] C-05b LSP backend for find_symbol/find_references/code_diagnostics with `lsp`/`lexical`/`build` labels

[x] S-01 process
[x] S-02 wait_for
[x] S-03 local_http
[x] W-01 web_search
[~] W-02 read_url (readability/metadata landed in fetch_url; separate tool not added)
[x] M-02 read_pdf
[x] M-01 read_image
[x] A-01 publish_artifact_file
[x] T-01 budget_status
[x] T-02 task_detail
[x] T-03 tool_search
[x] T-04 task_control
[x] D-01 data_query

[x] U-01 browser (WKWebView helper + shared GUI lease; see ADR-0137/0169)
[x] U-02 computer (native macOS AX/CGEvent/ScreenCaptureKit; ADR-0158)
[x] remote_read/write/list/exec/git/worktree (SSH/stdio; ADR-0156)
[x] read_tool_output (spilled tool output paging; ADR-0159)
[x] M-03 read_document
```

---

## 9. Role and permission matrix for new tools

| Tool group | Leader | Manager | Verifier | Worker/Labour coding | research | verification | generic |
|---|---|---|---|---|---|---|---|
| `apply_patch`, `file_manage` | yes | no | no | yes | no | no | no |
| `project_profile`, `repo_map`, `find_*`, `git`, `build_context`, `file_info`, `find_files`, `read_files` | yes | yes | yes | yes | yes | yes | yes |
| `run_project_task` | yes | no | yes (test/build/check) | yes | test/check only | test/check only | no |
| `process`, `wait_for` | yes | no | yes (test/build) | yes | no | yes | no |
| `read_tool_output` | yes | yes | yes | yes | yes (read-only) | yes | yes |
| `local_http` | yes | no | yes | yes | yes (localhost) | yes | no |
| `web_search`, `read_url` | yes | research only | research only | research/coding | yes | research only | no |
| `browser` | yes | no | no | coding/frontend | no | no | no |
| `computer` | yes | no | no | coding | no | no | no |
| `remote_read`/`remote_list`/`remote_git` | yes | yes | yes | yes | yes | yes | yes |
| `remote_write`/`remote_worktree` | yes | no | no | coding | no | no | no |
| `remote_exec` | yes | no | yes (test/build) | coding | no | no | no |
| `read_pdf`, `read_image` | yes | yes | yes | yes | yes | yes | yes |
| `budget_status`, `task_detail` | yes | yes | yes | own/session | own/session | own/session | own/session |
| `task_control` | yes | own plan only | no | no | no | no | no |
| `data_query` | yes | yes | yes | yes | yes | yes | yes |
| `tool_search` | yes | inspect only | inspect only | yes | yes | inspect only | yes |

The exact profile names may expand; keep the matrix in code and test it.

---

## 10. Structured tool-result contract (v1)

Every new tool should be able to return:

```json
{
  "ok": true,
  "summary": "string",
  "data": {},
  "warnings": ["string"],
  "next_actions": ["string"],
  "artifacts": [{"id": "artifact-id", "name": "test.log", "mime_type": "text/plain"}],
  "truncated": false,
  "cursor": null,
  "elapsed_ms": 12
}
```

Compatibility:

- Keep `ToolOutput.content: String` as the provider-facing text.
- Add `structured: Option<Value>` internally and expose it through tool events
  and UI cards.
- The text rendering must include the summary, critical data, warnings, and the
  next actions; it must not contain an unbounded `data` dump.
- Full raw outputs go to artifacts with explicit IDs.
- Errors use the same shape with `ok: false`, a stable `code`, and a
  `retryable` flag.

Stable error codes (examples): `PATH_ESCAPE`, `HASH_MISMATCH`, `PATCH_CONFLICT`,
`COMMAND_NOT_FOUND`, `PROJECT_UNSUPPORTED`, `PROCESS_LIMIT`, `OUTPUT_TRUNCATED`,
`APPROVAL_REQUIRED`, `NETWORK_DENIED`, `LSP_UNAVAILABLE`.

---

## 11. Testing and acceptance strategy

### Fixture repositories

- tiny Rust crate;
- Rust workspace with tests and clippy config;
- Swift package and XcodeGen project;
- TypeScript monorepo with npm/pnpm scripts;
- Python project with pytest;
- Go module;
- polyglot repo with a `justfile` and `AGENTS.md`;
- 10k-file repo for pagination/performance;
- CRLF/latin-1/binary/oversized files;
- symlink escape and `.git` cases.

### Test classes

- Unit: parsers, path jail, patch algorithm, command detection, symbol extraction.
- Golden: tool JSON outputs for fixture repos.
- Security: path escape, `.git`, symlink, SSRF, secret redaction, output caps.
- Role matrix: each tool is present/absent exactly as specified.
- Integration: scripted provider runs a realistic tool sequence with a mock
  filesystem; no network.
- Performance: `repo_map`, `grep`, `find_files`, `run_project_task` on the 10k-file
  fixture; enforce bounded latency and output.
- UI: structured tool cards, truncation, artifact links.
- Regression: all existing 618 Rust / 163 Swift tests remain green.

### Acceptance for the whole program

- A single coding agent can complete a multi-file change using only the new
  tools and shell as a fallback, with no guessed build/test command.
- A Manager/Verifier can inspect the same repo with read-only tools and receive
  structured evidence.
- A research agent can search, fetch, cite, and summarize bounded sources.
- A frontend agent can start a local server, wait, test HTTP, and (stretch)
  inspect a browser.
- No new tool bypasses approval, sandbox, path jail, capacity, or budgets.
- Tool output is bounded and full logs are artifacts.
- The model's average tool calls per task drop measurably on the fixture suite.

---

## 12. Explicit non-goals / deferred

Do not implement these as core agent tools in this budget:

- `git_commit` / `git_stage` / `git_branch` mutations: the runtime owns commits
  and integration; a write-capable Git tool needs a separate rollback design.
- Arbitrary database clients / SQL execution: use shell/MCP until a safe,
  project-scoped connector exists.
- Package installation / model download: shell or explicit user action.
- Secret reading / keychain export: agents never receive secrets.
- Generic unrestricted external HTTP: `local_http` is local-first; external
  access is `web_search`/`read_url` with explicit policy.
- Hidden model routing or a model-based tool selector.
- A plugin marketplace, remote daemon, team sync, MLX runtime, ACP server:
  these belong to later roadmap milestones.
- Auto-fixing code inside tools: tools report evidence; the agent decides.
- Full IDE/LSP replacement: LSP is optional assistance, not a requirement.
- Binary artifact preview beyond the existing 1 KB hex dump; `read_image`/OCR
  cover the useful cases.
- Audio/video transcription, image generation, email/Notion/Gmail connectors, and
  cloud browser infrastructure: use MCP/external capabilities until a dedicated
  product slice exists.

---

## 13. Decisions to make before implementation

1. **Tool result transport:** extend `ToolOutput` vs add a parallel structured
   envelope. Keep the provider-facing protocol stable.
2. **Tree-sitter grammar footprint:** which languages ship in the binary? Start
   with Rust, Swift, TS/JS, Python, Go; fallback for the rest.
3. **LSP lifecycle:** per-session server process vs per-worktree vs global; idle
   timeout and crash restart policy.
4. **Browser backend:** Resolved for v1 with the WKWebView helper (ADR-0137);
   a Playwright sidecar was rejected because it does not fit the write-confined
   helper model. The tool schema remains backend-independent.
5. **Search provider:** Brave/Tavily/SearXNG/custom; where keys live (Keychain)
   and how results are cached.
6. **Process persistence:** session-scoped only for v1; explicit non-goal across
   daemon restart.
7. **`apply_patch` format:** standard unified diff plus structured hunks; decide
   whether to accept Git-style binary patches (no in v1).
8. **Role matrix expansion:** add `frontend`/`docs` profiles only when the tools
   and prompts are ready; do not overload `coding`.

---

## 14. Relationship to existing docs

- Current implementation truth: `docs/PROGRESS.md`.
- Residual risks and audit status: `docs/AUDIT.md`.
- Long-term product plan: `docs/PLAN.md`.
- Architecture invariants: `docs/ARCHITECTURE.md`.
- Decisions: `docs/DECISIONS.md`.
- This file is the tooling-specific bridge between product intent and code; it
  does not override the above.
