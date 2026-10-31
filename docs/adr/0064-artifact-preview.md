# ADR-0064: Artifact text and binary previews

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

The Preview queue showed inline text and images, but a published file artifact
was only openable in an external app. Binary artifacts had no inspection path
inside VerlinPark.

## Decision

1. **Materialize on demand.** The app asks the daemon to materialize the
   content-addressed blob, then reads it locally.
2. **Preview by MIME.**
   - `image/*`: render the image.
   - Text-like MIME (`text/*`, JSON, XML, YAML, JavaScript, TOML, CSV, SVG,
     SQL, shell): show UTF-8 text up to 64 KiB.
   - Everything else: show an offset/hex/ASCII dump of the first 1 KiB.
3. **Explicit limits.** Text is clipped at 64 KiB; the hex dump at 1 KiB. The
   row states when a preview is truncated.
4. **Open always available.** Any materialized artifact can be opened in the
   default app; the path is shown in the row.
5. **App-only concern.** No new protocol or storage format; this reuses
   `artifact.materialize`.

## Consequences

- Users can inspect logs, JSON, and binary headers without leaving the app.
- The preview is not a full editor and does not syntax-highlight.
- Reading happens off the main actor, but a 5 MiB artifact still costs one
  materialization and a bounded read.
- Unknown text encodings may render replacement characters; they can still be
  opened externally.

## Alternatives considered

- **Quick Look:** requires a temporary file and a GUI hand-off, and does not
  work for every binary type.
- **Full hex/text viewer:** more UI and memory than needed for a preview.
- **No binary preview:** leaves artifacts opaque.
