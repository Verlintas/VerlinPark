# ADR-0131: Inline artifact versions and idempotent publication

- **Status:** Accepted
- **Date:** 2026-11-10
- **Owners:** Runtime, Artifacts, macOS UI
- **Related:** ADR-0041, ADR-0042, ADR-0048, ADR-0129

## Context

Artifacts were content-addressed session outputs, but every publish created a
new row. A retried tool call could duplicate a report, and an agent could not
revise an existing inline artifact without losing the previous text. SourceWeft's
artifact-write safety review highlighted the same core concerns: idempotency
keys, compare-and-swap, explicit versions, and inspectable history.

## Decision

### 1. Inline artifacts get an explicit version history

`artifacts.version` starts at 1. Every create writes an `artifact_versions` row
with the exact name/kind/MIME/content/size/SHA/path. Updating through
`publish_artifact` with an `artifact_id`:

- accepts an optional `expected_version`;
- rejects a stale expected version;
- writes a new version row and updates the current artifact;
- emits `artifact.version_published` with `previous_version`.

File-backed artifacts remain immutable in this slice because their blobs also
participate in content-addressed GC. Bulk replacement of a binary version would
need a separate blob-retention contract.

### 2. Restore creates a new version, never rewrites history

`artifact.restore` copies an old inline version into a new current version and
emits `artifact.version_restored` with `restored_from_version`. The old version
rows remain immutable (apart from idempotent upsert of the same version).

Artifacts created before the version table existed still have a valid current
version. `list_artifact_versions` / `get_artifact_version` synthesize that
current snapshot when no row exists, and the next update backfills the current
version before writing the new one. This keeps legacy history and diffs usable
without a destructive migration.

### 3. Request keys make retried publication idempotent

`publish_artifact` accepts an optional `request_key` scoped to the session.
`artifact_requests(session_id, request_key, artifact_id)` records the winner.
A repeated key returns the first committed artifact, even if the tool call
content differs. The key is bound to one session and is not a global cache.

The local implementation checks the key before creating and registers after the
create. A cross-process race can still create an orphan row; the registered
winner remains stable. A future server-mode transaction can close that window
without changing the RPC/tool contract.

### 4. Version history is inspectable in Preview

The artifact card shows `vN` and a history button for inline artifacts. The
history sheet lists version number, size, timestamp, content excerpt, current
marker, and a Restore action. Restore uses the same RPC as CLI and then reloads
the history.

### 5. Events and GC boundaries

Version events are in the session event stream, so the current artifact can be
replayed from `artifact.published` / `artifact.version_published`. The
`artifact_versions` table is a query projection; bundle/rebuild retain the
current artifact through events, while historical version rows are a local
convenience in this slice. Deleting an artifact cascades its versions and
request-key rows.

## Consequences

- A repeated publish tool call with a request key is safe to retry.
- Inline reports can be revised and restored with an auditable version number.
- Optimistic expected-version checks prevent silent overwrites.
- Binary artifacts keep the older immutable behavior; this is explicit rather
  than pretending versioning is uniform.
- Cross-process request-key atomicity is not claimed; the data-dir instance
  lock and session lease remain the current coordination model.

## Alternatives considered

- **Always create a new artifact row.** Rejected: duplicates on retry and no
  revision identity.
- **Overwrite content in place.** Rejected: loses evidence and allows stale
  writers to win.
- **Version binary blobs without GC.** Rejected: would orphan content-addressed
  objects and complicate retention.
- **A global request-key cache.** Rejected: collisions across sessions and
  projects would be surprising; keys stay session-scoped.
