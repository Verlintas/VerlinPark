# ADR-0183: Safe artifact archives and explicit media operations

- **Status:** Accepted
- **Date:** 2026-11-12
- **Deciders:** VerlinPark runtime maintainers
- **Related:** ADR-0170 (tool error contract), ADR-0134 (artifact store),
  ADR-0156 (tool-output paging), `docs/GAP_CLOSURE_PLAN.md` I2

## Context

Artifacts are already content-addressed, replayable, and versioned, but the
runtime treated an archive as an opaque binary and had no media metadata or
transcode surface. Two concrete gaps followed:

1. A zip/tar artifact could only be downloaded or opened in an external tool.
   The daemon had no bounded way to answer "what is inside this archive?" or to
   turn a selected entry into another artifact without writing into the agent
   worktree.
2. `read_image` and `read_pdf` covered still images and PDFs, but video/audio
   metadata was absent, and there was no sanctioned way to produce a smaller
   preview artifact. Shelling out to `ffmpeg` from an agent would bypass the
   approval and artifact-provenance model.

Archive extraction is a classic path-traversal and decompression-bomb surface:
zip-slip (`../`), absolute paths, Windows separators, symlinks/hardlinks,
device nodes, duplicate normalized names, malformed central directories,
password-protected entries, and compression ratios that expand a small upload
into an unbounded write. A production implementation must fail closed before
writing a single byte outside daemon scratch.

Media tooling has a different failure: it is easy to invent duration, codec, or
dimensions when `ffprobe` is absent. The runtime must never present guessed
metadata as probed metadata, and transcoding must be explicit rather than an
implicit side effect of opening a preview.

## Decision

### 1. Archive formats and parser boundary

The daemon supports exactly three containers:

- ZIP (`PK\x03\x04`, empty ZIP, and spanned-ZIP magic);
- POSIX/ustar tar (header checksum or `ustar` magic);
- gzip-compressed tar (`1f 8b` wrapping a tar stream).

The format is detected from bytes, never from a file extension. No other
decompressor is invoked. In particular, 7z, rar, xz, bzip2, zstd archives,
disk images, and bare gzip members return `ARCHIVE_FORMAT_UNSUPPORTED`.

The implementation uses the `zip`, `tar`, and `flate2` crates directly. It does
not run `unzip`, `tar`, `bsdtar`, `gunzip`, or any external process. A malformed
or truncated container returns `ARCHIVE_CORRUPT`; it never falls through to a
second decoder.

### 2. Hard caps

Every listing and extraction applies the same `ArchiveLimits` values, returned
in the list response:

| Limit | Default | Raising it |
| --- | --- | --- |
| archive bytes | 64 MiB | fixed |
| entries | 2,000 | request may lower, never raise |
| per-entry bytes | 32 MiB | fixed |
| total uncompressed bytes | 128 MiB | fixed |
| compression ratio | 100:1 | fixed |

Listing marks an entry `safe=false` and records a stable reason when it exceeds
a per-entry cap, is a duplicate normalized path, uses an unsafe path, is a
link/device/FIFO, or exceeds the ratio. The listing is metadata-only; it never
decompresses entry bodies.

A truncated listing (`truncated=true`) may still be displayed, but extraction
refuses it with `ARCHIVE_LIMIT_EXCEEDED`: an operator cannot select an entry
that was never fully validated against the cap.

### 3. Path normalization and entry rejection

`normalize_entry_path` rejects, before any filesystem access:

- empty paths;
- NUL bytes and control characters;
- absolute paths (`/...`) and Windows drive prefixes (`C:...`);
- `..` traversal after treating `\` as `/`;
- components longer than 255 characters or a path longer than 1,024;
- non-UTF-8 tar paths (the entry is listed as unsafe, not silently decoded).

Duplicate normalized paths are rejected. The first occurrence is safe; later
occurrences are marked `duplicate=true` and `safe=false`.

Entry kinds are classified from the container metadata, not the filename:

- ZIP: directory flag plus Unix mode (`S_IFLNK`, `S_IFCHR`, `S_IFBLK`,
  `S_IFIFO`, `S_IFSOCK`);
- tar: `EntryType` (`is_file`, `is_dir`, `is_symlink`, `is_hard_link`,
  block/character special, FIFO).

Extraction accepts only regular files. Directories are listable but are not
selected by `all`; links, hardlinks, devices, FIFOs, and sockets are rejected
with `ARCHIVE_UNSAFE_ENTRY` even if a caller names them explicitly.

### 4. Extraction target

Extraction writes into a fresh daemon-owned directory:

```
<data_dir>/archive-extracts/extract-<uuid>/
```

The directory is created 0700 and removed on every success and failure path by
a `Drop` guard. The destination path is built by joining normalized components
one at a time; the canonicalized parent must remain under the canonical scratch
root. Files are created with `create_new(true)` so an existing file is a
duplicate, not an overwrite.

The workspace is never a destination. The extractor has no API that accepts a
caller-supplied filesystem path. The only input is an artifact id; the bytes
come from the artifact's zstd blob (bounded decompression plus SHA-256
verification) or, for small inline test artifacts, the stored inline content.

Selected entries produce:

- a content-addressed file artifact when the entry is 1 byte–5 MiB;
- a bounded UTF-8 or hex preview (4 KiB) when requested;
- a `skipped_reason` for empty files and entries above the artifact cap.

Each published artifact carries `ArtifactProvenance`:

```json
{
  "operation": "archive_extract",
  "source_artifact_id": "art-...",
  "source_artifact_name": "bundle.zip",
  "source_sha256": "…",
  "source_mime_type": "application/zip",
  "entry_path": "docs/readme.txt",
  "archive_format": "zip",
  "extraction_id": "…",
  "entry_sha256": "…",
  "truncated": false
}
```

The source artifact is not modified and keeps its version. Derived artifacts
are new rows with their own SHA-256, version 1, and the normal artifact GC
semantics.

### 5. Encrypted and malformed archives

Any encrypted ZIP entry, including `ZipError::UnsupportedArchive(PASSWORD_REQUIRED)`,
returns `ARCHIVE_ENCRYPTED`. The daemon never prompts for a password, never
tries a dictionary, and never extracts the unencrypted portion of a mixed
archive as a partial success.

The archive is parsed before any scratch file is opened. A malformed header,
central directory, deflate stream, or tar entry returns `ARCHIVE_CORRUPT`. The
error detail is capped and never includes member bytes.

### 6. Media metadata

`media.info` materializes a file-backed artifact into the existing daemon
`artifacts/materialized` directory, then probes it. It never probes the
worktree, a URL, or a caller path.

When `ffprobe` is installed:

```
ffprobe -v error -print_format json -show_format -show_streams -i <path>
```

The process runs with:

- no shell;
- a scrubbed environment (PATH/TMPDIR/LANG/LC_ALL/TERM, HOME set to the temp
  directory);
- stdin closed;
- stdout/stderr bounded to 2 MiB / 16 KiB and drained on reader threads;
- a 20 s hard timeout, after which the child is killed and the result is
  `TIMEOUT`;
- JSON parsed into bounded known fields only (streams ≤ 64, tags ≤ 24,
  codec/format strings capped).

The response labels its source as `ffprobe`. If ffprobe is absent or fails and
the file has a still-image extension, the daemon may use macOS `sips` for
`pixelWidth`/`pixelHeight`/`format` and labels the result `image-metadata`,
including a warning that ffprobe was unavailable. For non-images, missing
ffprobe returns `MEDIA_TOOL_UNAVAILABLE`; a failed probe returns
`MEDIA_PROBE_FAILED`. The daemon never fabricates a duration, codec, or
dimension.

`VERLINPARK_FFPROBE` and `VERLINPARK_FFMPEG` may point at explicit binaries for
non-standard installs and deterministic tests. They do not change approval,
bounds, or trust.

### 7. Explicit transcoding

`media.transcode` requires `approved=true`. The CLI requires `--yes`; the Swift
card opens a confirmation alert. The agent tool surface classifies
`media_transcode` as `HostState`, so `smart` asks, `strict` asks, and
`unattended` denies. `media.info`, by contrast, is `ReadOnly`.

Transcoding:

- requires `ffmpeg`, otherwise `MEDIA_TOOL_UNAVAILABLE`;
- accepts only `mp4`, `webm`, `mp3`, `wav`, `png`, `jpeg`;
- clamps duration to 1–600 s (default 60 s);
- clamps the requested byte cap to 1 MiB–256 MiB, then further to the 5 MiB
  artifact-store cap because the output must become an artifact;
- runs ffmpeg with `-nostdin -v error -y -protocol_whitelist file -i <input> -t <cap>`
  and deterministic codec arguments per format;
- restricts both ffprobe and ffmpeg input protocols to `file` so a playlist or
  demuxer cannot pivot to HTTP/HLS/DASH; this is an input option, not a full
  network sandbox for a compromised helper binary;
- kills the helper's process group at `duration + 60 s` (30–600 s), so a shell
  wrapper cannot leave a grandchild holding output pipes open;
- checks the output size, rejects/destroys > cap, re-probes duration when
  ffprobe is available, and rejects/destroys a duration above the cap + 1 s;
- writes only under `<data_dir>/media-transcode/transcode-<uuid>/`, publishes
  the bounded output as a new artifact, and removes scratch.

The response includes a redacted `command_summary` (binary name, format, cap),
not the argv, source path, or environment. ffmpeg stderr is bounded and the
input/output paths are replaced with `<media>` before it reaches an event, log,
or UI.

The resulting artifact carries `operation: "media_transcode"`,
`source_artifact_id`, `source_format`, `output_format`, `tool: "ffmpeg"`, and
`max_duration_secs`.

### 8. Protocol, CLI, and UI surfaces

Additive JSON-RPC methods:

- `artifact.archive.list` → entries, caps, format, totals, warnings;
- `artifact.archive.extract` → extraction id, entries, published artifacts,
  total bytes, `scratch_retained: false`, warnings;
- `media.info` → source-labeled metadata;
- `media.transcode` → artifact, format, duration, bytes, elapsed, redacted
  command summary, effective bounds, warnings.

All new request fields are optional and all new response fields are
`serde(default)`-compatible. `Artifact` and `ArtifactVersion` add an optional
`provenance` object; older clients ignore it and older rows decode as `None`.
The SQLite schema adds `provenance TEXT NOT NULL DEFAULT ''` to `artifacts` and
`artifact_versions`, with the existing `ensure_column` migration path.

CLI commands are `artifact-archive-list`, `artifact-archive-extract`,
`media-info`, and `media-transcode` (requires `--yes`). Human renderers show
safe flags, caps, previews, provenance, and typed failures.

Swift Preview adds:

- an archive browser with entry selection, unsafe badges, filter, safe-file
  selection, extraction progress, published-artifact ids, and typed errors;
- a media card that loads metadata on demand, displays probe source and
  streams, and offers an explicit format/duration picker plus confirmation
  alert for transcoding.

`CoreError.rpc` now retains `error.data`, exposing `typedCode`/`typedHint`
without parsing the human message.

### 9. Error catalog

New stable Q3 codes:

- `ARCHIVE_ENCRYPTED`
- `ARCHIVE_FORMAT_UNSUPPORTED`
- `ARCHIVE_CORRUPT`
- `ARCHIVE_LIMIT_EXCEEDED`
- `ARCHIVE_UNSAFE_ENTRY`
- `ARCHIVE_ENTRY_NOT_FOUND`
- `ARCHIVE_EMPTY_SELECTION`
- `MEDIA_TOOL_UNAVAILABLE`
- `MEDIA_PROBE_FAILED`
- `MEDIA_TRANSCODE_FAILED`
- `MEDIA_LIMIT_EXCEEDED`
- `MEDIA_FORMAT_UNSUPPORTED`
- `APPROVAL_REQUIRED`

RPC failures return the same `{code,message,retryable,hint,evidence?}` object in
`error.data`. No error contains archive bytes, media bytes, a full argv, a
password, or a credential.

## Security and threat model

**In scope:** malicious or malformed archive uploads, zip-slip, absolute paths,
Windows separators, symlink/hardlink/device entries, duplicate names,
decompression bombs, encrypted archives, oversized metadata, ffprobe JSON
injection, ffmpeg path leakage, and accidental workspace writes.

**Out of scope:** a malicious local user who can already write the daemon data
directory; a compromised `ffprobe`/`ffmpeg` binary; kernel or filesystem bugs;
side channels; antivirus scanning of member contents; media codec memory-safety
bugs inside ffmpeg; and remote/network media URLs (rejected by construction).

The daemon does not claim that extracted bytes are benign. It claims that the
container cannot escape daemon scratch, that caps are enforced before writes,
and that derived artifacts are content-addressed and traceable.

## Non-goals

- No archive creation, editing, or repacking.
- No nested archive expansion (an extracted archive artifact must be listed in
  a separate explicit call).
- No archive password support, cracking, or partial encrypted extraction.
- No 7z/rar/xz/bzip2/zstd/disk-image/bare-gzip support.
- No extraction into the agent worktree or any caller-supplied path.
- No automatic publishing of every entry; selection is explicit or `all` is
  explicit.
- No network access for archive or media operations.
- No media streaming, playback server, thumbnails beyond the explicit
  single-frame `png`/`jpeg` transcode, codec probing fallback beyond still-image
  metadata, subtitle extraction, or DRM handling.
- No claim that ffprobe metadata is complete or that ffmpeg will decode every
  codec.
- No distributed archive/media worker; operations run in the local daemon.

## Consequences

- A user can inspect an archive safely and turn selected files into first-class
  artifacts without touching a worktree.
- A user can see real media metadata or a typed "tool unavailable" result, and
  can produce a bounded preview transcode deliberately.
- The artifact store grows only by explicit extraction/transcode; scratch is
  removed before the RPC returns.
- `ffmpeg` outputs above 5 MiB are rejected because they cannot become a single
  content-addressed artifact. A future chunked artifact format could relax
  this; it does not exist today.
- A hostile ffmpeg/ffprobe binary remains trusted local software. The daemon
  does not sandbox these helpers beyond the environment/output/time bounds.

## Alternatives considered

### Shell out to `unzip`/`tar`/`bsdtar`

Rejected. A subprocess adds an unbounded parser and a second process boundary,
and `tar` implementations differ on link/device handling. The in-process
crates give us deterministic header inspection, caps, and typed errors. The
cost is that only ZIP/tar/tar.gz are supported; that is an intentional scope
limit, not a claim of universal archive support.

### Extract into a temporary workspace directory

Rejected. A workspace path is visible to the agent, can be committed, can
collide with user files, and would need a separate cleanup policy. Daemon
scratch plus content-addressed artifacts keeps the write boundary identical to
screenshots and other daemon-produced media.

### Keep archive contents as one blob and offer a UI-only browser

Rejected. The requirement is safe extraction with provenance, not a display
trick. Publishing selected entries as artifacts makes them searchable,
versioned, previewable, and GC-eligible through the existing store.

### Use an external `libarchive` binding

Rejected for this round. It supports more formats but adds a large native
dependency and a new audit surface; the three required containers are covered
without it.

### Invent metadata when `ffprobe` is missing

Rejected. A guessed duration or codec is indistinguishable from a real one in
the UI and can mislead an operator. The typed unavailable result is honest and
actionable.

### Transcode implicitly when a preview is opened

Rejected. Transcoding consumes CPU/disk and invokes a host binary; it is a
state-changing operation. It requires an explicit RPC confirmation, a CLI
`--yes`, and a UI alert, and the agent tool is `HostState`.

### Write transcoded output into the worktree

Rejected for the same reason as archive extraction. The output is a new
artifact, and the source artifact remains unchanged.

### Stream archive entries directly into an artifact store writer

Not implemented this round. The scratch directory is the simplest way to give
the extractor a canonical path join and a single cleanup boundary. Artifacts
are published after the entry is fully validated and hashed. A future
streaming publisher could remove the scratch copy, but it must preserve the
same fail-closed ordering.

### Rely on MIME/extension checks instead of byte magic

Rejected. A caller can rename any file. Magic/header detection is required so
a tar payload cannot be routed to a ZIP parser or a bare gzip member to the tar
reader.

### Treat `all=true` as an implicit approval of every entry

Rejected. `all` selects only entries already marked safe and still applies
entry, total, ratio, and duplicate rules. It never upgrades an unsafe entry.

## Operational notes

- A failed extraction leaves no scratch directory; a failed transcode removes
  the partial output before returning.
- Extracted/transcoded artifacts appear through the normal `artifact.list`
  event flow. The UI refreshes the session mailbox after either operation.
- `artifact.archive.extract` is not a merge or verification gate. It creates
  evidence artifacts; it does not modify the session branch or plan state.
- The archive/media tools are available to Leader, coding/research Workers and
  Labour; Managers, Verifiers, and Scouts receive only the read-only
  `archive_list`/`media_info` pair. The permission broker still evaluates every
  call.
- `media.transcode` through the agent surface passes `approved: true` only
  after the broker has resolved the `HostState` request. The RPC caller must set
  the flag explicitly; a missing flag is `APPROVAL_REQUIRED` even if the user is
  an operator.
- If the daemon restarts, scratch directories from an interrupted operation are
  not treated as artifacts. A future sweeper may remove orphaned
  `archive-extracts`/`media-transcode` directories; none are referenced by
  SQLite, so they are safe to delete.
- `artifact.archive.list` and `media.info` are safe in `smart` and `unattended`
  modes; they do not write host state. `archive_extract` writes daemon-owned
  artifacts and is `External`; `media_transcode` is `HostState`.
- A 5 MiB artifact cap is a real product limitation: a large video will produce
  a bounded preview or a typed rejection rather than a multi-gigabyte artifact.
  This round does not add chunked artifacts.

## Failure behavior summary

| Situation | Result | Side effect |
| --- | --- | --- |
| Unknown magic | `ARCHIVE_FORMAT_UNSUPPORTED` | none |
| Truncated header | `ARCHIVE_CORRUPT` | none |
| Encrypted ZIP | `ARCHIVE_ENCRYPTED` | none |
| `../` or absolute path | entry `safe=false`; extraction `ARCHIVE_UNSAFE_ENTRY` | none |
| symlink/hardlink/device/FIFO | entry `safe=false`; extraction `ARCHIVE_UNSAFE_ENTRY` | none |
| duplicate normalized path | duplicate entry `safe=false`; extraction rejects | none |
| entry/total/ratio cap | `ARCHIVE_LIMIT_EXCEEDED` | scratch guard removes partial output |
| no selection | `ARCHIVE_EMPTY_SELECTION` | none |
| missing selected entry | `ARCHIVE_ENTRY_NOT_FOUND` | none |
| artifact blob hash mismatch | `HASH_MISMATCH`/`ARCHIVE_CORRUPT` | none |
| missing ffprobe, non-image | `MEDIA_TOOL_UNAVAILABLE` | none |
| missing ffprobe, still image | `source=image-metadata` with warning | none |
| ffprobe timeout | `TIMEOUT` | child killed |
| missing ffmpeg | `MEDIA_TOOL_UNAVAILABLE` | none |
| unapproved transcode | `APPROVAL_REQUIRED` | none |
| unsupported format | `MEDIA_FORMAT_UNSUPPORTED` | none |
| output over byte/duration cap | `MEDIA_LIMIT_EXCEEDED` | output removed |
| ffmpeg non-zero | `MEDIA_TRANSCODE_FAILED` | partial output removed |

## Tests

Rust:

- zip-slip, absolute path, backslash traversal, tar traversal;
- symlink, hardlink, block/char device, FIFO rejection;
- duplicate normalized entries;
- entry-count, per-entry, total-bytes, and compression-ratio caps;
- encrypted ZIP and malformed ZIP/tar;
- scratch-only extraction, empty selection, missing entry, all/entries
  conflict;
- ffprobe fixture parsing, invalid JSON, missing tool, timeout, output cap,
  still-image fallback;
- transcode approval, bounds, format allowlist, missing ffmpeg, output-size and
  duration post-conditions, path redaction;
- artifact provenance round-trip through insert, get, list, versions, and
  database reopen;
- daemon RPC typed success/failure and CLI parsing/renderers;
- protocol defaults remain backward compatible.

Swift:

- archive list/extract decode including unsafe entries, caps, provenance;
- media info decode for ffprobe and image-metadata sources;
- transcode decode with provenance/bounds/warnings;
- artifacts without provenance still decode;
- RPC error data exposes typed code/hint and legacy errors stay usable.

## References

- `crates/vp-core/src/tooling/archive.rs` — format detection, listing,
  normalization, scratch extraction.
- `crates/vp-core/src/tooling/media.rs` — bounded helper execution, ffprobe
  parsing, transcode plans and post-conditions.
- `crates/vp-core/src/archive_media.rs` — artifact-store bridge, provenance,
  scratch lifecycle.
- `crates/vp-protocol/src/types.rs` — additive archive/media DTOs.
- `crates/vp-store/src/store.rs` — provenance column and version preservation.
- `apps/macos/VerlinPark/Features/Preview/ArtifactArchiveMediaViews.swift` —
  archive browser and media card.


Rust:

- zip-slip, absolute path, backslash traversal, tar traversal;
- symlink, hardlink, block/char device, FIFO rejection;
- duplicate normalized entries;
- entry-count, per-entry, total-bytes, and compression-ratio caps;
- encrypted ZIP and malformed ZIP/tar;
- scratch-only extraction, empty selection, missing entry, all/entries
  conflict;
- ffprobe fixture parsing, invalid JSON, missing tool, timeout, output cap,
  still-image fallback;
- transcode approval, bounds, format allowlist, missing ffmpeg, output-size and
  duration post-conditions, path redaction;
- artifact provenance round-trip through insert, get, list, versions, and
  database reopen;
- daemon RPC typed success/failure and CLI parsing/renderers;
- protocol defaults remain backward compatible.

Swift:

- archive list/extract decode including unsafe entries, caps, provenance;
- media info decode for ffprobe and image-metadata sources;
- transcode decode with provenance/bounds/warnings;
- artifacts without provenance still decode;
- RPC error data exposes typed code/hint and legacy errors stay usable.
