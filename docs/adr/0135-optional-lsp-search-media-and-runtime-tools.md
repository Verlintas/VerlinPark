# ADR-0135: Optional LSP, provider search, document readers, and runtime introspection

- **Status:** Accepted
- **Date:** 2026-11-10
- **Owners:** Runtime, Agent Tools, Knowledge, macOS UI
- **Related:** ADR-0004, ADR-0021, ADR-0133, ADR-0134

## Context

ADR-0134 established the deterministic editing/project/Git/code-intelligence
layer and the explicit boundaries of the lexical index. The next gaps were:

- precise navigation when a language server is already installed;
- source discovery beyond a known URL;
- PDF/image/data inspection without shell parsing;
- runtime self-introspection and plan control;
- publishing generated files as session artifacts.

Browser/computer control was also evaluated. A headless Chrome/CDP spike could
start Chrome under the shell Seatbelt profile only after disabling Chrome's own
sandbox and widening temp/crashpad writes; even then local HTTP navigation
stalled because Chrome's network service is not usable inside the nested
sandbox. Running it unsandboxed would violate the worktree write boundary. The
browser tool was therefore not shipped; it needs a dedicated helper process or a
WebKit-backed host.

## Decision

### 1. Optional language servers, never auto-installed

`lsp_query` discovers `rust-analyzer`, `sourcekit-lsp`,
`typescript-language-server`, `pyright-langserver`/`pylsp`, `gopls`, `clangd`,
`jdtls`, and `kotlin-language-server` on `PATH`. A missing server is an explicit
`LSP_UNAVAILABLE` error, not a silent degradation.

Servers run under the shell sandbox, are cached per worktree/language, initialize
with workspace/configuration support, are shut down after 15 idle minutes or when
the worktree is removed, and expose definition, references, hover, document and
workspace symbols, and diagnostics. `lsp_query` is read-only and parallel-safe
only in the sense that multiple requests can reach one serialized process; it is
not injected into the parallel read batch.

### 2. Search is explicit provider configuration

`web_search` supports SearXNG, Brave, and Tavily. Configuration is read from the
daemon environment:

- `VERLINPARK_SEARCH_PROVIDER=searxng|brave|tavily`
- `VERLINPARK_SEARCH_ENDPOINT` (SearXNG requires it; optional override for others)
- `VERLINPARK_SEARCH_API_KEY`
- `VERLINPARK_SEARCH_LANGUAGE`, `VERLINPARK_SEARCH_SAFE`,
  `VERLINPARK_SEARCH_TIMEOUT_SECS`

There is no hidden model or search routing. Results are cached for five minutes,
normalized to title/URL/snippet/source/published/rank, and never include
credentials. The agent fetches selected URLs with `fetch_url`; search snippets
are not treated as page content.

### 3. Document and data readers are bounded and honest

- `read_pdf` uses a blocking PDF text extractor, caps input at 64 MiB and output
  at 2 MiB, supports best-effort page ranges via form-feed boundaries, and says
  when page boundaries are unavailable.
- `read_image` reads dimensions/format through sandboxed `sips` and runs OCR
  only when the user requests it and `tesseract` is installed. Missing OCR is a
  warning, not fabricated text.
- `data_query` parses JSON/YAML/TOML/CSV and applies a deterministic path
  subset (keys, indexes, wildcards, recursive descent). It never executes
  arbitrary code and never dumps a large file without a query.

### 4. Runtime introspection tools are special dispatch tools

`budget_status`, `task_detail`, `task_control`, `tool_search`, and
`publish_artifact_file` need live `Engine`/`Session`/budget/plan state. They are
not registry-backed tools; they are dispatched in the agent loop with explicit
role checks:

- `budget_status`, `task_detail`, `tool_search`, `publish_artifact_file`: all
  roles, read-only/workspace-write risk as appropriate;
- `task_control`: Leader/Manager only, WorkspaceWrite risk, event-backed retry/
  skip/cancel/pause/resume;
- `tool_search`: searches built-in and MCP tools, returns server/tool pairs for
  the generic `mcp` call action, and marks MCP execution non-callable for
  Manager/Verifier inspection-only roles.

`publish_artifact_file` reuses the existing content-addressed artifact store,
5 MiB cap, worktree path jail, and retention GC.

### 5. Browser/computer remained deferred at this ADR

No browser or computer tool was registered by this ADR. The target
`ComputerLease` was still unimplemented because there was no GUI/browser tool to
serialize. This was deliberate: a half-working or unsandboxed browser is worse
than an explicit gap.

**Later status (2026-11-10):** ADR-0137 supersedes the browser deferral. A
Seatbelt-confined WKWebView helper plus a single-daemon `ComputerLease` now
implement the browser surface. Native macOS computer control remains deferred.

## Consequences

Positive:

- Real code navigation when a user already has a language server.
- Research can discover sources instead of only fetching known URLs.
- PDFs, images, and structured configs no longer require shell pipelines.
- Agents can inspect budgets, task state, tool catalogs, and publish reports.
- All execution still passes approval, Seatbelt, path jail, write scope,
  Verifier, capacity, and budget gates.

Negative / limitations:

- LSP quality depends on the installed server and project setup; no auto-install.
- Search depends on an explicitly configured provider and network access.
- PDF extraction is best-effort for page boundaries and runs in-process.
- OCR requires `tesseract`; image understanding without OCR is metadata only.
- `data_query` is a small path language, not full JMESPath/JQ.
- Browser/computer and office documents remain unimplemented.
- `tool_search` exposes tool metadata but only MCP tools can be called through
  the generic `mcp` bridge without schema injection.

## Alternatives

- Bundle tree-sitter/LSP grammars: rejected for this wave; optional servers keep
  the binary and lifecycle smaller.
- Scrape a search engine without user configuration: rejected; unreliable and
  hides provider/terms.
- Ship headless Chrome unsandboxed: rejected; it would create a host write path
  outside the worktree and contradict the ADE security model.
- Use a model to query data files: rejected; deterministic path parsing is
  cheaper, inspectable, and safer.
