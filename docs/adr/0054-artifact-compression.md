# ADR-0054: Compressed content-addressed artifact blobs

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Binary artifacts are content-addressed but stored uncompressed. Repositories and
agents produce logs, JSON, screenshots, and test fixtures that compress well.
Users can open file artifacts directly, so compression must not expose raw
`.zst` files as the user-visible result.

## Decision

1. **Zstd at rest.** New binary blobs are compressed with zstd level 3 and stored
   as `<data>/artifacts/blobs/<sha256>.zst`. The SHA-256 is over the original
   bytes, not the compressed frame.
2. **Metadata.** `Artifact.compression` is `"zstd"` for compressed blobs and
   `"none"` for inline/text artifacts. Existing rows default to `"none"`.
3. **Materialization.** `artifact.materialize` decompresses to
   `<data>/artifacts/materialized/<sha256>`, verifies SHA-256, and returns the
   uncompressed path. The UI Open action and the agent `read_artifact` tool use
   it. The compressed blob remains the stored artifact.
4. **Export.** Bundles carry original bytes: export decompresses and verifies
   SHA-256 before base64 encoding. Import re-compresses and writes the
   content-addressed `.zst` blob.
5. **GC.** Refcount deletion removes the compressed blob, not the materialized
   cache. `bytes_freed` now reports the physical compressed bytes actually
   removed. The materialized cache is disposable and may be cleaned manually.
6. **Compatibility.** `compression` has a serde default of `"none"`; old
   artifacts and bundles remain readable. The file path changes for new blobs.

## Consequences

- Disk usage drops for compressible artifacts.
- Opening an artifact may pay a one-time decompression cost.
- A missing/corrupt compressed blob fails materialization with a SHA-256 error
  rather than returning wrong bytes.
- The materialized cache is not GC'd yet; it is bounded by artifact content and
  is safe to delete while the daemon is stopped.
- Compression is not applied to inline text artifacts because they are already
  small and replay relies on inline content.

## Alternatives considered

- **Leave blobs uncompressed:** simple but wasteful.
- **Store only compressed and open `.zst`:** poor UX.
- **Compress inline text in SQLite:** complicates replay and yields little gain.
- **Content-addressed dedupe without compression:** already implemented; does
  not help unique large artifacts.
