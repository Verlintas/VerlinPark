# ADR-0149: Rich artifact preview

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** macOS UI, Artifacts
- **Related:** ADR-0129, ADR-0131, ADR-0132

## Context

Artifact cards originally rendered only inline text, an `NSImage` when the MIME
type started with `image/`, and a 1 KB hex dump for everything else. That was
not enough to inspect the file-backed outputs agents already publish: PDFs,
audio, video, and binary blobs had no usable preview, and a preview failure was
indistinguishable from an empty artifact.

The daemon already exposes the needed trust boundary. `artifact.materialize`
returns the daemon-controlled path for file-backed content (decompressed when
stored as zstd), and inline content is already bounded by the publish tool. The
macOS client must not invent paths from artifact display names, execute
artifacts, fetch remote URLs, or mutate daemon state to preview a file.

## Decision

### 1. One deterministic classifier

`ArtifactPreviewClassifier` maps MIME type and filename extension to:

- `image`
- `pdf`
- `video`
- `audio`
- `text`
- `binary`
- `archive`
- `other`

Known MIME prefixes win, then common extensions. `text/*`, JSON/XML/YAML/TOML/
CSV/JavaScript/shell MIME types, and common source extensions are text. An
unknown MIME/extension falls back to `text` only when a bounded content sample is
valid UTF-8; an explicit binary MIME (`application/octet-stream`, Mach-O, WASM)
stays binary. Empty MIME, empty filename, and no sample classify as `other`.
Classification is a display decision and never opens or executes the artifact.

### 2. Local-file access rule

The only file paths the preview reads are:

1. an absolute `path` returned by the daemon for a `storage == "file"` artifact
   with a non-zstd compression; or
2. the path returned by the existing `artifact.materialize` RPC, which is also
   daemon-controlled.

The client never constructs a path from `Artifact.name`. Range reads use
`FileHandle` and the pager's byte range; no arbitrary UI input is accepted as a
filesystem path. Inline bytes come from `content` (UTF-8) or `data_base64` and
are capped at 100 MB before decoding/paging.

### 3. Temp materialization

Inline PDF/audio/video, Quick Look, Open, Reveal, and Save As may need a real
file. A temporary file is written under
`~/Library/Caches/com.verlintas.VerlinPark/ArtifactPreviews`, never `/tmp` or
the daemon data directory. `ArtifactTemporaryFile` removes the file on deinit;
SwiftUI preview state also clears it on dismiss and replaces it on the next
preview. A startup best-effort sweep removes stale preview files older than 24
hours. `Open` uses a stable per-artifact cache copy so the external application
can keep reading after the async call returns.

### 4. Quick Look

The preview sheet presents `.quickLookPreview` with either the daemon-provided
file path or a temporary inline copy. SwiftUI's modifier is available on the
deployment target; a separate `QLPreviewPanel` fallback is unnecessary. Temp
files are removed when the panel closes or the preview is dismissed.

### 5. Binary paging

Binary previews are paged at 64 KB. Each page shows a monospaced offset column,
16-byte hex groups, and a printable-ASCII column; the final line is padded so
the ASCII column stays aligned. Page/range clamping is deterministic, and a
zero-byte artifact has one empty page. File pages are read with `FileHandle`;
inline pages are sliced from the already-bounded `Data` in memory. Total size
and the current byte range are always visible.

### 6. Rich media and text

- Images keep the thumbnail but add a larger scrollable/zoomable sheet view
  with pixel dimensions.
- PDFs use PDFKit `PDFView` through `NSViewRepresentable`, with page count,
  previous/next page, and Open.
- Video and audio use AVKit `VideoPlayer` with a local file URL and a cheap
  `AVURLAsset` duration read.
- Text keeps the 64 KB bounded preview and adds wrap/no-wrap and Save As.
- Archive and unsupported artifacts show metadata and the safe actions
  (Quick Look, Open, Save As); archives are not extracted.

The metadata panel shows MIME, size, storage, compression, SHA-256 (copyable),
version, created timestamp, source path when available, and version history for
inline artifacts.

### 7. No execution and no network

Preview code never executes artifacts, runs archive extractors, invokes a media
transcoder, or fetches remote URLs. AVKit and PDFKit read local files only.
Existing theme tokens and `VPMotion.resolved` keep the surface consistent and
respect Reduce Motion.

## Consequences

- Reviewers can inspect images, PDFs, local video/audio, text, and binary
  artifacts without leaving the app or asking the daemon for a new feature.
- Inline previews remain bounded: 64 KB of text, 100 MB before Base64 decode,
  and one 64 KB binary page at a time.
- File-backed previews depend on `artifact.materialize` or a valid daemon path;
  a missing path is shown as an error rather than guessed.
- Quick Look and Save As create cache files. They are cleaned on dismissal and
  by the stale-file sweep, but a crash can leave one until the next sweep.
- There is no remote URL preview, no archive extraction, no media transcoding,
  and no native Quick Look thumbnail generation inside the card itself.

## Out of scope

- Remote artifact URLs or cloud storage.
- Archive member listing or extraction.
- Audio waveform generation or video frame thumbnails.
- Editing artifacts in the preview surface.
- A daemon-side preview RPC or any Rust/shared-runtime change.
