# ADR-0042: Binary artifact storage and retention GC

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

ADR-0041 stored only text artifacts inline in SQLite. Real agent work produces
reports, screenshots, archives, and other binary files. Keeping every artifact
forever also makes a long-lived session grow without bound. The store needs a
bounded binary path and an explicit retention policy without letting agents write
outside their worktree.

## Decision

1. **Two storage modes.** Text artifacts remain inline in SQLite
   (`storage = "inline"`, content in the row). Binary artifacts are copied into
   a content-addressed blob at `<data>/artifacts/blobs/<sha256>`
   (`storage = "file"`, path in the row), with the content column empty.
   Publishing identical bytes reuses the same blob.
2. **Source confinement.** `publish_artifact` accepts either `content` or `path`.
   A path must canonicalize inside the publishing agent's worktree. The runtime
   reads at most 5 MiB, copies the bytes, and records size plus SHA-256.
3. **Integrity metadata.** Every artifact carries `storage`, `size_bytes`,
   `sha256`, and an optional absolute `path`. Paths are runtime-local and are
   not portable across machines.
4. **Retention policy.** A project `ArtifactPolicy` has optional `max_bytes`,
   `max_age_secs`, and `max_count`, plus `auto_gc` (default false). Limits must
   be positive when present. GC removes oldest artifacts first until every
   configured limit is satisfied.
5. **GC triggers.** `artifact.gc` / `vp gc` runs manually. If `auto_gc` is on,
   publishing an artifact triggers a project GC after the new row is committed.
   GC deletes the row; the backing blob is removed only when no remaining
   artifact row references that path. `bytes_freed` therefore reports physical
   bytes freed, not the sum of logical artifact sizes.
6. **APIs and UI.** RPC `artifact.policy.get/set` and `artifact.gc`; CLI
   `artifact-policy`, `artifact-policy-set`, `gc`; Manage Dash has an Artifact
   policy card. Artifact list/get expose metadata and local path; the Preview
   opens file artifacts with `NSWorkspace` and shows inline text inline.
7. **Bundle portability.** Export reads file artifacts and includes
   `data_base64`; import decodes them into the destination data directory and
   rewrites the path. Inline content remains inline. This preserves artifacts
   across machines at the cost of larger bundles.
8. **Limits.** Content-addressed dedupe is implemented; compression,
   content-level replay, binary preview, and cross-session artifact sharing are
   not. The 5 MiB publish cap is deliberate. GC is deterministic and
   oldest-first; it does not consult the LLM.

## Consequences

- Agents can publish real binary outputs without receiving a general write path
  outside their worktree.
- Retention prevents unbounded artifact growth, at the cost of deleting old
  artifacts without a trash can.
- Export/import is portable but can be large for binary artifacts. Import
  deduplicates by SHA-256 into the same blob directory.
- `artifact.path` is machine-local and is not a stable user-facing identity; use
  the artifact ID.
- Each artifact deletion now emits `artifact.pruned` with the artifact ID and
  metadata. Artifact content replay/rebuild is still not implemented; the event
  is an audit record, not a content store.

## Alternatives considered

- **Store binaries as SQLite BLOBs:** simple but bloats the DB and makes large
  exports/writes awkward.
- **Let agents write directly to a shared artifact directory:** violates the
  worktree write boundary and needs another path jail.
- **No retention:** unbounded disk growth.
- **Automatic deletion by LLM judgment:** non-deterministic and hard to audit.
