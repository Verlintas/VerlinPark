# ADR-0129: Project knowledge sources, lexical retrieval, and citation evidence

- **Status:** Accepted
- **Date:** 2026-11-10
- **Owners:** Runtime, Knowledge, macOS UI
- **Related:** ADR-0081, ADR-0082, ADR-0127, ADR-0131

## Context

VerlinPark had strong session artefacts, project memory, workspace search, and
evidence-oriented prompts, but no project-level source library. A research or
delivery task could inspect the worktree, but it could not index a note, a
project document, or a public page once and then retrieve a stable, citable
chunk across sessions. The SourceWeft review made the missing loop obvious:
ingest → retrieve → cite → inspect the original passage.

The implementation had to stay deterministic and local-first. In particular:

- no hidden embedding model or model routing;
- no arbitrary project-root write from the tool layer;
- no unbounded HTML/text ingest;
- no second source of truth that could drift from the event log;
- citations must remain inspectable after a source is refreshed or deleted.

## Decision

### 1. Sources are project-scoped durable snapshots

A `Source` has an id, project, kind (`note`, `file`, `url`), optional
collection/labels, title, URI, MIME type, status, size, SHA-256, revision,
chunk/token counts, and timestamps. The normalized text is written to a
private JSON snapshot under:

```
<data_dir>/sources/<project_id>/<source_id>.json
```

SQLite carries the queryable metadata and chunk index. The snapshot is the
durable source body used by `db-rebuild`; rebuild re-chunks each snapshot and
recreates the FTS projection. A failed manifest update must not turn a
successful source add into a user-visible retry.

Ingest paths are deliberately narrow:

- **note**: text supplied by the user;
- **project file**: relative, canonicalized under the project root, no `..`,
  no `.git`, UTF-8, no NUL, ≤2 MiB;
- **public URL**: the existing DNS-pinned SSRF checks and no-redirect policy,
  bounded body read, HTML stripped to readable text;
- **folder import**: text-like extensions only, at most 500 files, skips
  binary/oversized/empty files, and skips existing source URIs.

Source collections are labels, not security boundaries.

### 2. Chunking is deterministic and heading-aware

`vp-core::sources` normalizes line endings, walks Markdown headings outside
fenced code, keeps blocks together up to a target size, splits oversized blocks
at paragraph/line/word boundaries with overlap, and caps a source at 500
chunks. Every chunk records its heading path, source offsets, and an
approximate token count. Chunk ids are deterministic from source id, chunk
number, offsets, and content hash.

The chunker never calls a model. A refresh increments the source revision and
replaces the current chunks; citation events retain the excerpt that was used,
so the audit trail survives later edits and deletion.

### 3. Retrieval is FTS5 first, lexical fallback second

`source_chunks_fts` is an external-content FTS5 index with unicode61
tokenization. Search tries an all-terms query and then an any-term query.
If FTS misses, and for CJK/short queries where unicode61 is unreliable, the
daemon performs a bounded `LIKE` fallback and scores rows by literal occurrence
count and heading matches. Results are capped (default 8, hard cap 50) and
return source/chunk ids, title, heading, content, and score.

There is no embedding pipeline in this slice. This is intentional: it keeps
retrieval inspectable and works without another model call or API key. A future
embedding profile can be added without changing the citation contract.

### 4. Agents get read-only knowledge tools

Leader, Manager, Worker, Labour, and Verifier receive:

- `search_sources(query, source_ids?, limit?)`;
- `read_source(source_id, chunk_no?, max_chars?)`;
- `list_sources(limit?)`.

Successful routine calls are hidden from the transcript, but their outputs are
in the audit event stream. The system prompt includes a compact indexed-source
catalog, while full text is only loaded on demand.

### 5. Citations are per-run evidence, persisted in the message event

`run_loop` owns a `CitationRegistry` for the current agent run. Every search hit
or read chunk receives a stable `cN` key in first-use order; repeated access
reuses the key. Tool output formats the key as `[c1]` and the system prompt
requires exact keys.

The final `agent.message` event carries:

```json
"citations": [
  {
    "key": "c1",
    "source_id": "...",
    "source_title": "...",
    "chunk_id": "...",
    "chunk_no": 2,
    "heading": "Leases",
    "excerpt": "...",
    "score": 4.2,
    "origin": "search"
  }
]
```

`session.citations` derives the citation list from those events rather than a
second table, so replay has one source of truth. The UI renders inline `[cN]`
links and a source strip under the assistant bubble; a citation sheet shows the
snapshot excerpt even if the source was later changed.

### 6. UI is a source library and a citation inspector

Preview has a Sources surface with:

- library search (name/path or content retrieval);
- note/file/URL/folder import;
- source detail with chunk cards, refresh/reindex, delete, collection and
  labels;
- a Citations pane for the selected session, derived from the event log;
- click-through from a citation or search hit to the exact source chunk.

Source list state is project-scoped and refreshed explicitly or when the
surface opens. Source edits use the same bounded RPCs as the CLI.

## Consequences

- A team can build a small local knowledge base without a vector database.
- Source ingestion is bounded but not a document platform: PDFs, OCR, audio,
  and connector sync are out of scope for this slice.
- Retrieval quality is lexical, not semantic. Query wording and headings matter.
- The model can still omit a citation marker; the source strip and
  `session.citations` make the evidence visible regardless.
- Refresh replaces the current chunk projection. Old citation excerpts remain
  in events, but the old full chunk is not retained as a source revision in
  this slice.

## Alternatives considered

- **Store all source text in the project manifest.** Rejected: append-only
  manifest lines would carry megabytes of documents.
- **Create a new SQLite source database.** Rejected: unnecessary migration and
  backup surface for a local daemon.
- **Index with embeddings first.** Deferred: it requires a configured embedding
  model, dimension migration, and a second cost path. FTS5 is the reliable base.
- **Let the agent ingest arbitrary host paths.** Rejected: project-root jail and
  explicit UI/CLI ingestion keep the write/read authority with the user.
