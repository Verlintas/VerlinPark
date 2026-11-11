# ADR-0150: Data-directory backup and restore

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, CLI
- **Related:** ADR-0033, ADR-0041

## Context

VerlinPark keeps durable state in a local data directory: the SQLite metadata
database, `projects.jsonl`, per-session JSONL event mirrors, and content-addressed
artifact blobs. Operators had no supported way to take a consistent copy of that
directory or to restore it without manually copying files while hoping the
daemon was stopped.

The existing session export/import path (ADR-0033) archives one session as a
read-only bundle. It is not a data-directory backup: it does not preserve the
database as a whole, other sessions, project rows, or artifact blobs. The
rebuild path (`vp db-rebuild`) reconstructs projections from the manifest and
event mirrors but also relies on those files already being present.

Backups must be safe to create from a live daemon without copying a WAL, must
never export discovery tokens, locks, provider credentials, or Keychain data,
and must not let a malformed archive write outside the selected data directory.
Restore is an offline operation and must never stop or signal a running daemon.

## Decision

Add `vp-core::backup` with two local, non-RPC operations:

- `create_backup(data_dir, output, include_artifacts) -> BackupReport`
- `restore_backup(input, data_dir, force) -> RestoreReport`

The CLI exposes them as `vp backup` and `vp restore`. Both use the normal CLI
data-directory default and do not require a running daemon. Neither operation
adds an agent tool or an RPC method.

### Archive layout

A single ZIP file contains:

| Entry | When | Contents |
|---|---|---|
| `backup.json` | always | Manifest: format version, `created_at`, app version, protocol version, `includes_artifacts`, SQLite filename, and entries |
| `verlinpark.db` | always | SQLite snapshot created with `VACUUM INTO` |
| `projects.jsonl` | when present | Durable project manifest |
| `sessions/<id>/events.jsonl` | when present | Per-session event mirrors |
| `artifacts/blobs/<name>` | only with `--include-artifacts` | Content-addressed artifact blobs, direct children only |

Each manifest entry records a POSIX-style relative path, byte size, and
lowercase SHA-256. The manifest itself is not listed as a data entry and has no
checksum of its own; ZIP CRCs catch accidental corruption, but the format does
not authenticate the archive (see limitations).

Archive entry permissions are set to `0600` for all files, including the
database and event mirrors. Extracted directories are created `0700`, and
extracted files are created `0600`.

### Inclusion allowlist

Backup reads only the paths above. It does not walk the data directory. The
following are never read or archived:

- `run/daemon.json` (discovery endpoint and bearer token) and `run/daemon.lock`
- logs, terminal/browser consoles, and runtime scratch directories
- worktrees and Git repositories
- provider keys, Keychain material, environment files, and arbitrary operator
  files in the data directory
- SQLite `-wal` and `-shm` sidecars
- anything outside the selected data directory

Source snapshots under `sources/` are not part of this format. They are not
covered by `vp backup` and are called out as a limitation rather than silently
claimed as backed up.

Symlinked databases, project manifests, session directories, and artifact
directories are refused. Symlinked files inside otherwise valid directories are
skipped, so a link cannot pull outside content into the archive.

### SQLite snapshot

The database copy is produced by opening the source read-only and running
`VACUUM INTO`. The result is a consistent point-in-time database containing
committed WAL state; raw `-wal`/`-shm` files are never copied. Backup therefore
does not require the daemon to be stopped, but it also does not quiesce writers.
The event mirrors and artifact blobs are copied as individual files after the
database snapshot. Each archived file is hashed after it is copied, so the
manifest always describes the bytes that are actually in the archive, even if
the daemon appended to an event mirror during the backup. Cross-file
point-in-time consistency is not promised for a live daemon.

### Restore validation and ordering

Restore is offline and refuses to run when a daemon appears to be using the
target data directory. It validates in this order:

1. Open the ZIP and reject directory entries, symlink entries, duplicate names,
   absolute paths, `..`, backslashes, empty components, and any path outside the
   allowlist.
2. Read and validate `backup.json`: supported format version, exact SQLite
   filename, exactly one database entry, one hash per entry, and no archive
   entry missing from or extra to the manifest. Artifact entries are rejected
   unless the manifest says artifacts are included.
3. Verify each entry's size and SHA-256 against the manifest.
4. Inspect the target: reject a symlink or non-directory target; refuse a
   discovery file or a held instance lock; without `--force`, refuse an existing
   database, an instance lock file, or any non-empty target.
5. Extract into a private temporary directory next to the target, hashing each
   file again while writing, and verify the SQLite header.
6. Re-check for a daemon, then move the validated directory into place.

`--force` permits replacing an existing database, an unlocked stale instance
lock, or any non-empty target. It does not permit restoring while a daemon is
actually running: discovery or a held lock is always a refusal. Restore never
kills, stops, or signals a daemon.

When artifacts were not included, the archive restores only the database,
`backup.json`, `projects.jsonl`, and event mirrors. A forced replacement also
removes pre-existing artifact blobs because restore replaces the target
directory rather than merging into it.

### Atomicity

All validation and extraction happen before the target is modified. The
validated tree is built in a sibling temporary directory on the same filesystem
and then published by renaming the existing target aside and renaming the new
tree into place. If the final rename fails, the previous target is renamed back.
The temporary tree is removed on every error path. This gives an all-or-nothing
replacement at the directory level for the normal success and validation-error
cases; it is not a crash-consistency protocol across sudden power loss.

The report includes the output path, format version, creation time, entry count,
session mirror count, artifact count, byte count, and whether an existing target
was replaced. A restore failure returns an error and does not report success.

## Consequences

- Operators can take one checksummed file with a consistent SQLite snapshot and
  restore it while the daemon is stopped.
- Discovery tokens and locks are not exported; a restored daemon creates a new
  discovery file and token.
- Corrupt, truncated, or malicious archives fail before the target is touched.
- A restored database is usable by the existing `Store::open` path without a
  rebuild step, provided the required tables are present.
- Backup of a live daemon is possible, but mirrors and artifacts are only as
  current as their individual copy time, not a single cross-file snapshot.

## Limitations

- **No live-daemon quiesce.** Backup does not pause writers or take a global
  cross-file snapshot. `VACUUM INTO` makes the database consistent, not the
  whole directory.
- **No worktree or Git backup.** Sessions may reference worktrees, branches, and
  commits that are not in the archive. Restore does not recreate them.
- **No secret export or import.** Discovery tokens, keys, Keychain data, and
  environment files are excluded. Restore cannot recover them.
- **No incremental backup.** Every invocation writes a complete ZIP.
- **No encryption or signature.** The ZIP can be read by anyone who obtains it,
  and a party able to rewrite both the manifest and payload can produce a
  self-consistent archive. SHA-256 detects accidental corruption and partial
  tampering, not a malicious re-signing.
- **No project source snapshots.** The `sources/` directory is outside the
  format.
- **No merge restore.** `--force` replaces the target directory; it does not
  merge an archive into existing state.
- **No daemon RPC, agent tool, or automatic start/stop.** These are local CLI
  operations only.
- **Not a database repair tool.** A successful restore does not run
  `replay`, `db-rebuild`, or integrity checks beyond the SQLite header and
  per-entry checksums.

## Alternatives considered

- **Copy the database file and its WAL:** faster, but not a point-in-time
  snapshot and easy to produce a torn database. Rejected in favor of
  `VACUUM INTO`.
- **Tar the whole data directory:** simpler, but includes discovery tokens,
  locks, logs, and arbitrary files. Rejected because the allowlist is the
  security boundary.
- **Reuse session bundles:** preserves one session but not the database,
  projects, other sessions, or artifacts. Rejected as a different feature.
- **Restore in place file by file:** avoids a directory swap but cannot leave
  the target untouched on a late failure. Rejected in favor of staged
  extraction plus rename.
- **Ask the daemon to quiesce over RPC:** would improve live consistency, but
  restore must work offline and must never depend on or stop a running daemon.
  Deferred as a separate capability.
