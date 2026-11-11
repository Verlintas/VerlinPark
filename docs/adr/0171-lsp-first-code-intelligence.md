# ADR-0171: LSP-first code intelligence with explicit lexical and build fallbacks

- **Status:** Accepted
- **Date:** 2026-11-11
- **Deciders:** VerlinPark runtime and tooling
- **Related:** ADR-0134 (agent tool surface), ADR-0135 (code index), ADR-0158
  (native computer control), `docs/TOOLING_ROADMAP.md`, Q4 of
  `docs/QUALITY_CACHE_GUI_TOOLING_PLAN.md`

## Context

The original code-intelligence tools were deterministic and dependency-free:
`repo_map`, `find_symbol`, `find_references`, `code_diagnostics`,
`dependency_graph`, `affected_tests`, and `build_context` used a bounded lexical
index or the project's detected check/lint command. That was useful on a clean
machine, but it was also easy for an agent to over-trust:

- `find_references` matched words, not bindings. A local variable named `run`
  looked like a reference to a method named `run`.
- `find_symbol` did not distinguish a macro invocation from a declaration and
  could not see generated or macro-expanded symbols.
- `code_diagnostics` could only run a build. That is correct but slow, and it
  cannot report editor diagnostics while a long build is unnecessary.
- Every result said `confidence: "lexical"`, but there was no machine-readable
  backend envelope, no fallback reason, and no result-level source label. A
  model could not tell whether an empty reference list meant "no references" or
  "the index could not see this file".
- A language server existed only as a low-level `lsp_query` escape hatch. It was
  not used by the navigation tools, so projects with a perfectly good server
  still received approximate answers.

At the same time, VerlinPark must remain usable without installing a language
server. It cannot make a hidden install, cannot silently route to a model, and
cannot weaken the existing sandbox, approval, write-scope, Verifier, capacity,
or budget gates. LSP precision is optional infrastructure, not a new source of
authority.

## Decision

VerlinPark treats an installed language server as the first-class optional
backend for symbol search, reference search, diagnostics, and bounded symbol
enrichment of repository maps/context packets. Lexical and build results remain
available, but every result must identify its backend, status, and confidence.
There is no silent precision claim.

### Backend selection

The navigation tools accept an additive `source` field:

| Tool | Values | Default | Meaning |
| --- | --- | --- | --- |
| `find_symbol` | `auto`, `lsp`, `lexical` | `auto` | `auto` tries LSP and falls back to lexical with a label. |
| `find_references` | `auto`, `lsp`, `lexical` | `auto` | Same. |
| `code_diagnostics` | `auto`, `lsp`, `build` | `auto` | `auto` tries LSP and falls back to the detected check/lint command. |
| `repo_map` | `auto`, `lsp`, `lexical` | `lexical` | `auto` only uses an already-running server; `lsp` may start one. |
| `build_context` | `auto`, `lsp`, `lexical` | `lexical` | Same. |
| `lsp_query` | low-level `action` | n/a | Explicit server escape hatch; never used as a hidden fallback. |

`auto` is not the same as `lsp`:

- `auto` may fall back. A fallback is a successful result with
  `source: "lexical"` or `source: "build"`, `status: "fallback"`, a
  `fallback_reason`, and the LSP install hint when one exists.
- `lsp` is exact-or-error. If no server is installed, the server does not
  support the method, the request times out, or the server crashes, the tool
  returns a structured error such as `LSP_UNAVAILABLE`,
  `LSP_METHOD_UNAVAILABLE`, `LSP_TIMEOUT`, or `LSP_CRASHED`. It does not
  silently return lexical references.
- `lexical` and `build` are explicit user choices. They are labeled as such and
  never described as compiler-accurate.

`repo_map` and `build_context` default to `lexical` to avoid spawning a server
on every map/context call. `auto` uses only an already-running server; an
explicit `lsp` request may start one. This keeps routine exploration cheap while
allowing a caller that wants exact symbols to ask for them.

### Result contract

Every code-intelligence result carries these additive fields:

```json
{
  "source": "lsp | lexical | build",
  "status": "ok | partial | fallback | unavailable | unsupported | timeout | crashed",
  "confidence": "lsp | lexical | compiler-or-parser",
  "confidence_score": 0.97,
  "language": "Rust",
  "language_server": "rust-analyzer",
  "language_server_version": "1.x",
  "method": "workspace/symbol",
  "fallback_reason": "lsp_unavailable",
  "install_hint": "Install rust-analyzer ...",
  "backend": {
    "source": "lsp",
    "status": "ok",
    "score": 0.97,
    "language": "Rust",
    "server": "rust-analyzer",
    "method": "workspace/symbol",
    "attempts": [
      {
        "language": "rust",
        "server": "rust-analyzer",
        "method": "workspace/symbol",
        "status": "ok"
      }
    ]
  }
}
```

`confidence` remains a categorical string for compatibility with the first
lexical tools. `confidence_score` is the numeric companion. `source` and
`status` are new and additive; clients decode them with defaults so an older
daemon or a cached event remains readable.

Symbol results include all of:

- `path`,
- `line`, `column`, `end_line`, `end_column`,
- a nested `range` object with the same four values,
- `kind` and a normalized `kind_group` (`function`, `type`, `module`,
  `constant`, `macro`, `symbol`),
- optional `container`,
- `signature` (a bounded source line for LSP symbols),
- `score`, `source`, `confidence`, `confidence_score`.

Reference results include the same range fields, an `is_definition` marker, the
declared symbol's `kind`, and per-result source/confidence. Duplicate locations
from a server or from multiple language servers are removed by a deterministic
key; ordering is path, line, column, end line, end column.

Diagnostic results are grouped by file and severity:

```json
{
  "source": "lsp",
  "status": "ok",
  "ok": false,
  "language_server": "rust-analyzer",
  "groups": [
    {
      "path": "src/lib.rs",
      "error_count": 1,
      "warning_count": 1,
      "information_count": 0,
      "hint_count": 0,
      "diagnostics": [
        {
          "path": "src/lib.rs",
          "line": 12,
          "column": 5,
          "end_line": 12,
          "end_column": 9,
          "severity": "error",
          "code": "E0425",
          "source": "rustc",
          "server": "rust-analyzer",
          "message": "...",
          "backend": "lsp"
        }
      ]
    }
  ]
}
```

Build diagnostics use the same group and per-diagnostic shape, with
`backend: "build"` and the detected command in `method`/`command`. The old
`groups` count is retained as `group_count` so no consumer loses the count.

The provider-facing text starts with a backend sentence, for example:

- `LSP · rust-analyzer · workspace/symbol · ok`
- `lexical fallback (lsp_unavailable) · not compiler-accurate · fallback`
- `build diagnostics · cargo check · fallback`

This is deliberately visible to the model. A model that sees "lexical fallback"
must not describe the result as exact.

### Supported methods

The client supports the LSP 3.17 methods needed by the tools:

- `initialize`, `initialized`
- `textDocument/didOpen`, `textDocument/didChange`
- `textDocument/definition` (including `LocationLink`)
- `textDocument/references`
- `textDocument/hover`
- `textDocument/documentSymbol` (hierarchical and flat)
- `workspace/symbol`
- `textDocument/diagnostic` (pull diagnostics)
- `textDocument/publishDiagnostics` (push diagnostics)
- `shutdown`, `exit` on process teardown

The server->client requests answered by the reader are
`workspace/configuration`, `workspace/workspaceFolders`,
`window/workDoneProgress/create`, and `window/showMessageRequest`.

Capabilities are parsed from the `initialize` result, not assumed. A method is
called only when the server advertises it. `references` falls back to
`definition` only when the server has no references provider but does have a
definition provider; that result is marked `partial` and reports the method
actually used. `find_symbol` uses `workspace/symbol` when advertised and falls
back to bounded `documentSymbol` calls for the scoped files when it is not. The
fallback method and `partial` status are visible in the result.

`lsp_query` remains the low-level escape hatch. It exposes `definition`,
`references`, `hover`, `symbols`, `workspace_symbols`, `diagnostics`,
`capabilities`, and `status`. It uses the same capability checks, mapping,
path jail, UTF-16 conversion, and error types as the high-level tools.

### Lifecycle, sandbox, and bounds

The existing lifecycle is reused:

- Servers are cached per canonical worktree root and language id.
- A server is started with `sandboxed_command`, the same Seatbelt write sandbox,
  environment allowlist, process-group cleanup, and network policy as other
  shell tools.
- Startup is bounded by a 60s initialize timeout; requests default to 30s;
  diagnostics use a 12s per-file request timeout and a 20s total budget.
- Idle servers are reaped after 900s on the next LSP request; removing a
  worktree kills all servers for that root.
- A missing binary is never installed. Discovery only checks executable files
  on `PATH`. The error includes the searched program names, the language, and a
  language-specific install hint.
- A crashed server is disconnected and restarted at most once for a request.
  The retry reopens the document, so it does not reuse a stale document session.
  If the restart fails, the result is `LSP_CRASHED` (or a labeled lexical/build
  fallback in `auto`).
- A timeout removes the pending request id, sends a best-effort
  `$/cancelRequest` notification, and returns `LSP_TIMEOUT`; it does not replay
  a partially applied request. Server responses after the timeout are ignored
  because their ids are no longer pending.
- A server that fails to initialize is recorded with bounded exponential
  cooldown (5s doubling to 60s). Repeated calls return a cooldown
  `LSP_UNAVAILABLE` instead of spawning a broken process on every tool call.
  Removing the worktree clears the failure state. A request-level crash still
  triggers at most one immediate restart; a restart failure is reported, not
  looped.

Resource bounds:

- Maximum framed message: 16 MiB.
- Maximum document: 8 MiB. Oversized documents are refused rather than silently
  truncated, because truncating would produce wrong ranges.
- Maximum locations returned by a low-level query: 500 (tools cap to 100/200).
- Maximum symbols: 500 at the parser, tools cap to 100/200/500.
- Maximum diagnostics: 200 at the tool boundary.
- Maximum files touched by one document-symbol/diagnostic batch: 24.
- Maximum languages per query: 3. A polyglot repository cannot spawn every
  server from one request.
- Diagnostic cache: at most 256 URIs and 500 diagnostics per URI; stale entries
  are evicted and oversized publishes are ignored.
- Hover text: 12,000 characters.
- Source lines used to derive context/signatures are capped at 300 characters
  and then capped again in the tool output.

### URI/path jail and UTF-16

Language servers speak file URIs and UTF-16 code-unit positions. The client
does not pass either straight through to user-visible output:

- Only `file:` URIs are accepted. A URI is parsed to a path, resolved against
  the canonical worktree root, and canonicalized when it exists. Symlinks that
  resolve outside the root are rejected. Relative paths are joined to the root
  and rejected if they escape. Non-file URIs and out-of-root locations are
  dropped from symbol, reference, and diagnostic results.
- Input positions are 1-based character columns. They are converted to 0-based
  offsets in the server's negotiated position encoding. UTF-16 is the default
  and is always supported; `utf-8` and `utf-32` are honored when a server
  advertises `positionEncoding`, so a server that negotiates a different
  encoding does not silently shift columns.
- Server ranges are converted back to 1-based character columns. A position
  inside a surrogate pair or a multi-byte character resolves to that character
  rather than a column past the end of the line. Lines beyond EOF are clamped
  to the last line. `\r` is stripped for display; the newline itself is never
  included in a column.
- The same conversion is used for definitions, references, hover ranges,
  document symbols, workspace symbols, pull diagnostics, and publish
  diagnostics. This avoids the common bug where an emoji or non-BMP character
  shifts every column on the rest of the line.

### Diagnostics freshness

Push diagnostics are cached with the URI, document version (when the server
sends one), and receipt time. A publish with a lower version than the cached
version is ignored. When opening a document, only a publish with a version at
least as new as the opened version is accepted; a no-version publish is accepted
only while it is recent. If no publish arrives within 4s, the result is
`timeout`/`partial` with no fabricated diagnostics. In `auto`, a timeout can
fall back to the build path with `fallback_reason: "lsp_timeout"`.

Pull diagnostics use `textDocument/diagnostic` with a null result id because the
tool does not persist server result ids across daemon restarts. Pull responses
are cached briefly for repeated queries but are never used as a claim of
workspace-wide correctness.

### No auto-install and no hidden model routing

The daemon never runs a package manager, never downloads a server, and never
invokes a model to "fill in" a missing reference or diagnostic. Missing servers
produce structured unavailability with an install hint. There is no hidden
model routing in this feature; the only model call remains the agent loop the
operator configured.

### Limitations

This is not a full IDE and does not claim to be:

- No rename, code action, formatting, completion, signature help, inlay hints,
  semantic tokens, or workspace edit application.
- No guarantee that a language server has finished indexing. An empty workspace
  symbol result is authoritative for the server's current state, not for all
  generated or unindexed code.
- No multi-root workspace server sharing; the key is one worktree + one
  language. A server started for one session worktree is not reused by another.
- No server-side type-checking guarantee. Diagnostics are whatever the installed
  server reports at query time.
- No remote-host LSP. Remote workspace tools are separate and do not proxy a
  language server.
- No persistent pull-diagnostic result ids or cross-restart diagnostic cache.
  A daemon restart starts with an empty cache.
- No streaming or progress reporting; requests are bounded request/response.
- No LSP over WebSocket/HTTP transport; this ADR covers stdio language servers.
- No automatic server discovery beyond `PATH` and the static candidate table.
  Project-local servers, version managers that require shell initialization, and
  custom command lines are not discovered.
- No claim that a fallback is exact. The lexical index remains a bounded,
  heuristic index; build diagnostics remain parser-derived and may miss
  diagnostics if the project has no check/lint command.

The implementation is deliberately stdio-only because the existing lifecycle,
sandbox, and process cleanup are built around child processes. Remote LSP would
need its own trust boundary and is deferred.

## Consequences

### Positive

- Projects with an installed server get exact definitions/references and
  editor diagnostics without a build.
- Projects without a server keep working, and the model is told that the answer
  is lexical/build rather than compiler-accurate.
- Backend provenance is machine-readable, so the UI/CLI can show an `LSP`,
  `Lexical`, or `Build` badge and future metrics can attribute failures.
- Bounded frames, documents, files, languages, diagnostics, and restarts prevent
  a hostile or broken server from consuming unbounded memory or time.
- The same mapping code is used by high-level tools and `lsp_query`, so the
  escape hatch cannot drift from the safe path.

### Negative / trade-offs

- `auto` is slower than pure lexical on projects with a large language server,
  because it may start/index a server. `repo_map`/`build_context` therefore
  default to lexical.
- Results can differ between servers for the same query. The source and server
  fields make that visible, but they do not normalize semantics.
- The client has no cross-restart diagnostic cache, so the first query after a
  restart can wait for the server.
- Lexical fallback remains approximate; the implementation intentionally reports
  a lower confidence score rather than pretending otherwise.
- The server process is trusted user-installed software. The Seatbelt sandbox
  confines writes, but arbitrary reads and network access under the project
  policy are still governed by the existing shell policy, not by a new LSP-
  specific sandbox.

## Alternatives considered

1. **Always use a build for diagnostics.** Rejected: slow, and it conflates
   "the compiler has not run" with "there are no errors." LSP is optional and
   labeled; build remains the fallback.
2. **Make lexical results look exact when LSP is unavailable.** Rejected: this
   is the bug the ADR exists to prevent. Fallbacks are explicit.
3. **Auto-install language servers.** Rejected: network/package-manager actions
   are host-level changes, violate the no-hidden-install rule, and are not
   reproducible across machines.
4. **Use a model to resolve ambiguous references.** Rejected: hidden model
   routing, non-deterministic, and outside the tool boundary.
5. **Embed a full IDE engine.** Rejected: disproportionate dependency and
   security surface. The daemon owns scheduling; an external server owns
   language semantics.
6. **Implement only `lsp_query` and leave high-level tools lexical.** Rejected:
   it preserves the precision gap and makes agents choose the wrong tool by
   default.

## Verification

The implementation is covered by:

- unit tests for source/status/confidence envelopes, de-duplication, ordering,
  caps, kind grouping, UTF-16 conversion, URI/path jail, capability parsing,
  framing bounds, and status reporting;
- a fake language server over stdio exercising initialize/capabilities,
  workspace symbols, definitions, references, hover, pull diagnostics, publish
  diagnostics, unsupported methods, timeouts, crash/restart, and missing
  servers;
- tool-level tests for lexical fallback labels, explicit-LSP unavailability,
  symbol/reference range contracts, diagnostic grouping, and repo/context
  labeling;
- protocol round-trip tests for `lsp.status`;
- CLI parsing and human rendering for the `vp lsp <project>` status command;
- Swift tests for `ToolTrace` source labels and transcript structured-result
  decoding;
- the existing lexical code-index and LSP lifecycle suites, which remain green.

`cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D
warnings`, and `cargo test --workspace` are the merge gates for this ADR. Swift
changes require `swiftformat lint` and the Xcode test target.

## Status note

This ADR is accepted as the contract for the Q4 implementation. It does not
claim that every language server behaves identically, that a server has indexed
all code, or that lexical/build fallbacks are exact. Those limits are part of
the design, not implementation gaps to be papered over.
