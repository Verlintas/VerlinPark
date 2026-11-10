# ADR-0134: Deterministic agent tooling wave

- **Status:** Accepted
- **Date:** 2026-11-10
- **Owners:** Runtime, Agent Tools, macOS UI
- **Related:** ADR-0004, ADR-0008, ADR-0021, ADR-0054, ADR-0124, ADR-0125, ADR-0133

## Context

The original seven file/shell tools (`read_file`, `list_dir`, `grep`,
`edit_file`, `write_file`, `shell`, `fetch_url`) are enough for ordinary coding
because `shell` is a universal escape hatch. They are not enough for large
engineering work: writes are non-atomic, there is no multi-file patch, agents
must guess build/test commands, Git and repository structure must be scraped
from text, tool calls execute serially, and there is no long-lived process,
local HTTP, or deterministic code navigation.

The product principle is that the runtime should absorb every deterministic
decision that does not require judgment. A model with a precise, contextual tool
API performs better than a stronger model forced through a vague tool chain.
This ADR records the first large tooling wave and its boundaries.

## Decision

### 1. Structured results, bounded events

`ToolOutput` keeps the provider-facing text `content` and adds an optional
machine-readable `structured` value. The text must remain a concise, actionable
rendering; the structured value is for events, UI cards, and later inspection.
Event persistence caps structured data at 32 KiB and falls back to a summary.

### 2. Atomic, reviewable writes

`write_file` and `edit_file` use a same-directory temp file, fsync, and rename.
Both support `expected_sha256` and `dry_run`. `edit_file` rejects ambiguous
matches unless `replace_all` is explicit.

`apply_patch` accepts a unified diff, validates every target path and hunk
before writing any file, applies each file atomically, and restores earlier
files if a later write fails. It supports create/delete toggles and expected
hashes. `file_manage` covers mkdir/move/copy/delete with dry-run,
expected-hash checks, and an explicit recursive-delete guard.

All of these keep the existing canonical worktree jail, `.git` refusal, and
symlink checks. No tool gains authority outside the worktree.

### 3. Project detection instead of command guessing

`project_profile` detects languages, package managers, build/test/lint/format/
typecheck commands, test layout, conventions, and CI files. Every command has a
source and confidence. `run_project_task` resolves from that profile, applies
package/filter/affected-test selection where the ecosystem supports it, runs
under the existing shell sandbox, and parses common failure formats into
`path:line:column` diagnostics.

### 4. Read-only Git, runtime-owned writes

The `git` tool exposes status, diff, log, show, blame, and tracked files. It has
no stage/commit/branch actions. Worker commits and integration remain owned by
the runtime and the merge queue. A write-capable Git tool needs a separate
rollback design and is not part of this wave.

### 5. Lexical code intelligence with explicit confidence

`repo_map`, `find_symbol`, `find_references`, `dependency_graph`,
`affected_tests`, `code_diagnostics`, and `build_context` use a bounded lexical
index. Results report `confidence: "lexical"`; they are not LSP-accurate. The
index is cached for 20 seconds per worktree and invalidated eagerly after file
writes, file management, and shell commands. `code_diagnostics` runs the
detected check/lint task and returns compiler/parser output; an optional LSP
backend remains deferred.

`build_context` assembles a token-budgeted, cited packet from symbols, files,
tests, project commands, and recent Git changes. It does not call a model and
does not embed a repository.

### 6. Parallel read-only tools only

When a turn contains only registry-backed, role-allowed, non-special read tools
(`read_file`, `list_dir`, `grep`, `find_files`, `file_info`, `read_files`,
`project_profile`, `git`, `repo_map`, `find_symbol`, `find_references`,
`dependency_graph`, `build_context`, `affected_tests`), the runtime executes up
to four concurrently with stable result ordering. Shell, network, approvals,
writes, delegation, and skill activation stay serial in model order.

### 7. Worktree-scoped long-lived processes

`process` starts sandboxed commands with piped stdio, a 2 MiB per-stream ring
buffer, per-worktree limit of 8, and an idle TTL (default one hour). It supports
start/list/status/read/write/signal/wait/stop. Processes do not survive a daemon
restart in v1; this is explicit rather than pretended. `wait_for` handles local
ports, localhost URLs, and process log patterns without sleep guessing.

### 8. Local HTTP only

`local_http` accepts localhost, `127.0.0.1`, and `[::1]`. It supports explicit
headers and JSON bodies, keeps redirects local, and redacts secret-bearing
headers from results. It never injects ambient credentials. External HTTP remains
`fetch_url`.

`fetch_url` now follows up to five redirects with SSRF revalidation on every hop,
reduces HTML to readable text via the existing source extractor, and returns the
final URL, title, links, and content hash. Binary responses are rejected by the
tool rather than silently lossy-decoded.

### 9. Role matrix and approvals stay explicit

Every new tool is in the role/profile matrix. Managers remain code-read-only;
Verifiers gain read-only intelligence plus `run_project_task`/`code_diagnostics`;
coding Workers/Labour gain writes, tasks, Git reads, code intelligence, process,
and local HTTP; research gains read-only intelligence and localhost HTTP;
generic stays read-only. Approvals, Seatbelt, write scopes, capacity, Verifier
gates, and budgets are unchanged.

## Consequences

Positive:

- One call replaces long read/grep/parse chains for common operations.
- Multi-file edits become reviewable and atomic.
- Agents no longer guess build/test commands in supported ecosystems.
- Large repositories gain bounded navigation and context assembly.
- Dev servers and local APIs become testable without shell pipelines.

Negative / limitations:

- The code index is lexical, not compiler-accurate; it can misclassify unusual
  syntax and cannot provide type information.
- Cache invalidation is eager for our own writes and shell calls, but external
  edits made outside the runtime can be stale for up to 20 seconds.
- Processes are not daemon-restart durable and are not a distributed process
  manager.
- `local_http` is localhost-only; external API testing needs a future policy.
- Web search, PDF/image reading, office documents, browser/computer control,
  budget/task introspection, and tool-catalog search remain deferred.
- No write-capable Git tool or plugin-selected tools were added.

## Alternatives

- Add tree-sitter/LSP immediately: higher accuracy, but a large dependency and
  lifecycle cost before project/task tools were solid. Keep as the next code
  intelligence upgrade.
- Keep everything in `shell`: cheaper, but preserves the exact guessing and
  parsing burden that makes tool use fragile.
- Add a model-based tool router: rejected; tool availability stays explicit and
  deterministic.
- Add browser/computer control now: rejected as a stretch item until the core
  edit/verify/research loop is measured in real use.
