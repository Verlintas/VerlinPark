# ADR-0167: Encrypted and incremental backup repository

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, CLI, security review
- **Related:** ADR-0150 (v1 data-directory backup), ADR-0033 (session
  export/import), ADR-0039 (path jail), ADR-0134/0135 (tool output and
  sandboxing), ADR-0166 (opt-in ANN; unrelated, same round)

## Context

ADR-0150 introduced `vp backup --output <zip>` and
`vp restore --input <zip>`. The v1 format is a checksummed ZIP containing a
`VACUUM INTO` SQLite snapshot, `projects.jsonl`, per-session event mirrors, and
optionally content-addressed artifact blobs. It deliberately excludes discovery
tokens, locks, logs, worktrees, environment files, provider keys, and arbitrary
operator files. It is validated before the target data directory is touched and
published by an atomic directory swap.

Two operational gaps remain:

1. **Confidentiality.** A ZIP is readable by anyone who obtains it. SHA-256
   detects accidental corruption, but it is not authentication. A party able to
   rewrite the manifest and payload can produce a self-consistent archive.
2. **Storage cost.** Every invocation rewrites every byte. For a project with a
   large SQLite database, thousands of event mirrors, and artifact blobs, most
   of a daily backup is unchanged.

Changing the v1 ZIP in place would break existing archives and the existing
restore path. Encrypted ZIP is also a poor target: the standard ZIP format has
no authenticated encryption primitive that fits this codebase's dependency and
compatibility constraints, and adding a proprietary ZIP envelope would preserve
the full-rewrite cost.

This ADR therefore adds a separate v2 **backup repository** mode while keeping
v1 ZIP behavior unchanged.

## Decision

Add `vp-core::backup::repository` (with `backup::crypto` primitives) and the
following additive CLI surface:

```text
vp backup --repository <dir> [--incremental] [--passphrase-env NAME]
             [--passphrase-file PATH] [--plaintext] [--no-compress]
             [--include-artifacts]
vp restore --repository <dir> --snapshot <id> --data-dir <dir> [--force]
             [--passphrase-env NAME | --passphrase-file PATH]
vp backup-list --repository <dir> [--verify]
             [--passphrase-env NAME | --passphrase-file PATH]
vp backup-prune --repository <dir> --keep N [--dry-run]
             [--passphrase-env NAME | --passphrase-file PATH]
```

The existing commands remain:

```text
vp backup --output <zip> [--include-artifacts]
vp restore --input <zip> --data-dir <dir> [--force]
```

`--repository` and `--output`/`--input` are mutually exclusive. v1 archive
bytes, path allowlists, restore validation, and tests are unchanged. v1 remains
unencrypted; this ADR does not claim otherwise and does not attempt encrypted
ZIP.

There is no daemon RPC, agent tool, scheduler integration, cloud upload, or
automatic backup trigger. Backup is offline-safe; restore requires the daemon
to be stopped, exactly as in v1.

## Repository layout

```text
<repository>/
  repository.json              repository metadata, encryption parameters,
                               prune tombstones, optional metadata MAC
  repository.lock              advisory exclusive/shared lock
  snapshots/<uuid>.manifest    immutable authenticated snapshot manifests
  blobs/<sha256>.blob          immutable content-addressed sealed blobs
  nonces.jsonl                 append-only random-nonce audit table
  events.jsonl                 append-only created/restored/pruned log
  tmp/                         private staging for atomic publication
```

All directories are created `0700`, files `0600` on Unix. All regular files are
opened with `create_new` or `create + truncate(false)` as appropriate; blob and
manifest publication uses a temporary file in `tmp/` plus a hard link to the
immutable final name. Existing immutable files are never overwritten.

`repository.json` is the only mutable metadata file after initialization.
`events.jsonl` and `nonces.jsonl` are append-only operational logs; they are
best-effort audit records, not the source of repository validity. Snapshot
validity comes from the authenticated manifest, metadata, and verified blobs.
Blob and manifest filenames are validated before use:

- snapshot ids are canonical lowercase hyphenated UUIDs;
- manifest filenames are `<uuid>.manifest`;
- blob filenames are `<64 lowercase hex>.blob`;
- unknown regular files, directories, or symlinks in `snapshots/` or `blobs/`
  are rejected rather than silently skipped;
- snapshot entries are revalidated through the v1 data-directory allowlist, and
  must be strictly sorted by POSIX path.

A repository path that is symlinked, or overlaps the selected data directory in
either direction, is rejected. Configured data and repository roots are
canonicalized before this check.

## Snapshot manifests

Each snapshot is self-contained. Its manifest lists every allowlisted file in
that snapshot, not a delta:

```json
{
  "kind": "verlinpark-backup-snapshot",
  "format_version": 1,
  "snapshot_id": "<uuid>",
  "sequence": 3,
  "parent": "<uuid or null>",
  "incremental": true,
  "created_at": "<RFC 3339>",
  "app_version": "<build version>",
  "protocol_version": 1,
  "encrypted": true,
  "includes_artifacts": false,
  "kdf_fingerprint": "<sha256 of canonical KDF params>",
  "entries": [
    {
      "path": "verlinpark.db",
      "size": 123456,
      "sha256": "<plaintext sha256>",
      "compression": "zstd",
      "stored_bytes": 45678
    }
  ],
  "totals": { "entries": 1, "bytes": 123456 }
}
```

`sequence` starts at 1 and increases by one for every snapshot. The chain is
linear: the first snapshot has no parent; every later snapshot's parent is the
immediately preceding retained snapshot. A manifest is authenticated as a
whole (see below), so fields such as `sequence`, `parent`, `encrypted`, and
`kdf_fingerprint` cannot be altered without detection in encrypted mode.

The complete file list is intentional. It makes every retained snapshot
restorable after its parent manifests are pruned and makes reference protection
for blobs computable from any single retained manifest. The parent chain is
incremental provenance and integrity context, not a delta reconstruction
requirement.

## Cryptographic design

### Dependencies

- `argon2` 0.5, Argon2id raw KDF;
- `chacha20poly1305` 0.10, ChaCha20-Poly1305 AEAD;
- `zeroize` 1, passphrase and key buffers;
- `rand` 0.9, CSPRNG nonces and salts;
- existing `sha2` for SHA-256.

No custom block cipher, stream cipher, or password hash is implemented. HMAC-
SHA-256 is implemented locally only to authenticate mutable repository
metadata; it is tested against RFC 4231 test case 2.

### Key derivation and separation

A random 16-byte salt is generated when an encrypted repository is created and
persisted in `repository.json`. Argon2id derives a 32-byte root key. Three
domain-separated subkeys are derived with SHA-256 over a fixed label, a zero
separator, and the root key:

```text
blob_key      = SHA-256("vp-backup-v2/blob-key\0" || root)
manifest_key  = SHA-256("vp-backup-v2/manifest-key\0" || root)
metadata_key  = SHA-256("vp-backup-v2/metadata-key\0" || root)
```

The root and subkeys live in `Zeroizing` buffers. The passphrase lives in a
non-cloneable `Passphrase` wrapper with a redacted `Debug` implementation. Keys
and passphrases are never written to metadata, manifests, nonce records, logs,
reports, or errors.

The repository id and snapshot id are included in the manifest AEAD AAD. The
plaintext hash, encryption/compression flags, chunk size, chunk index, and
final-chunk marker are included in each blob-chunk AAD. Moving a manifest to a
different repository or snapshot, reordering chunks, concatenating chunks from
another blob, or truncating at a chunk boundary fails authentication.

This is a KDF plus key-separation construction, not a general-purpose
multi-recipient key hierarchy. There is no key wrapping, escrow, or rotation.

### KDF bounds

Every KDF parameter read from disk is validated before Argon2 allocates memory:

| Parameter | Default | Accepted range |
|---|---:|---:|
| algorithm | `argon2id` | exact |
| Argon2 version | `0x13` | exact |
| salt | 16 random bytes | exactly 16 bytes, canonical base64 |
| memory cost | 65,536 KiB (64 MiB) | 8,192..=262,144 KiB |
| time cost | 3 | 1..=10 |
| parallelism | 1 | 1..=8 |
| output length | 32 bytes | exact |

Malformed, non-canonical, weak, or excessive values fail before derivation.
The upper bound limits a hostile repository's denial-of-service surface. The
lower bound rejects trivial one-iteration/low-memory parameters. Operators
cannot select arbitrary KDF costs through the CLI; tests use the lower bound for
speed, and the public options type allows a bounded `KdfConfig`.

### Manifests

Encrypted manifest bodies are sealed with ChaCha20-Poly1305 and a random 96-bit
nonce. Envelope format:

```text
magic "VPMANI2\0" | version 1 | flags | reserved | sha256(plaintext)
| plaintext length | nonce | ciphertext (plaintext || 16-byte tag)
```

AAD binds the repository id, snapshot id, and encryption flag. The envelope
also records a plaintext SHA-256 and length so accidental truncation is
rejected before JSON parsing. For encrypted manifests the AEAD tag is the
authentication boundary; the plaintext hash is an additional consistency check.

Unencrypted manifests use the same envelope with a zero nonce and no tag. They
self-hash and reject corruption, but they are not authenticated. This mode is
explicit opt-in and is documented as providing no malicious-rewrite resistance.

Manifest plaintext is capped at 32 MiB (the entry count is capped at 100,000),
and the sealed envelope is capped just above that plus header/tag overhead.
`repository.json` is capped at 64 MiB; prune tombstones are bounded by the
100,000-entry chain limit.

### Blobs

Each blob stores one file's original bytes, keyed by the plaintext SHA-256. The
envelope is chunked so a multi-gigabyte database never needs to be held in
memory:

```text
magic "VPBLOB2\0" | version 1 | flags | reserved | sha256(plaintext)
| chunk size | random 96-bit base nonce
| repeated: chunk flags | u32 ciphertext length | ciphertext
```

- flags bit 0: encrypted; bit 1: zstd-compressed before encryption;
- every chunk has a final/not-final marker; exactly one empty final chunk ends
  a blob;
- non-final chunks are exactly `chunk_size` plaintext bytes (256 KiB by
  default, bounded 32 B..=1 MiB for tests/implementation);
- the per-chunk nonce is the random base nonce with the chunk index XORed into
  its trailing eight bytes;
- the chunk AAD includes the plaintext hash, flags, chunk size, index, and
  final marker.

The final empty chunk gives encrypted blobs an authenticated end marker.
Deleting it, appending data after it, or truncating a ciphertext chunk fails.
For unencrypted blobs, SHA-256 and byte-count checks reject the same corruption;
there is no authentication claim.

Compression (zstd level 3) is applied before encryption and is recorded per
entry. Existing blobs are reused with their original encoding even if a later
snapshot requests a different compression mode. Decompression is capped at the
manifest entry size while writing, and the zstd window log is capped, so a
malicious compressed blob cannot expand without bound. The original plaintext
SHA-256 and size are verified after decompression.

### Metadata authentication

`repository.json` contains the KDF salt/parameters and prune tombstones. In an
encrypted repository it also carries an HMAC-SHA-256 over the canonical JSON
representation with the MAC field removed. The HMAC key is the domain-separated
metadata subkey. This authenticates prune history and encryption parameters
after the passphrase has derived the key. A wrong passphrase therefore fails
metadata authentication before a manifest or blob is opened.

An attacker who changes the salt or KDF parameters causes either a validation
failure or a different derived key and HMAC failure. They cannot silently lower
KDF bounds or add/remove prune tombstones in encrypted mode. Plaintext
repositories have no metadata MAC and make no such claim.

## Nonce table

`nonces.jsonl` is an append-only audit table with records of the form:

```json
{"kind":"blob","object_id":"<sha256>","snapshot_id":"<uuid>",
 "nonce_b64":"<16 base64 chars for 12 bytes>","created_at":"..."}
```

Manifest nonces are recorded as `kind: "manifest"`. The actual nonce is also in
the envelope; the table is operational audit state, not the source of
cryptographic truth. Repository open rejects malformed complete lines,
non-12-byte nonces, invalid object ids, and duplicate nonce values. An
incomplete final line (a crash during append) is ignored; complete malformed
lines are not. Nonce records are not secret and are not authenticated as a
separate object; deleting or editing the table cannot make a wrong key or
tampered envelope decrypt successfully.

Every encrypted blob and manifest uses a fresh CSPRNG nonce. Deduplicated blobs
are not re-encrypted, so their existing nonce is not appended again.

## Incremental semantics and deduplication

- Plaintext SHA-256 is the dedup address and the post-decryption verification
  hash.
- A blob file name is exactly the plaintext hash. If the blob already exists,
  it is verified end to end before reuse; it is never overwritten.
- `--incremental` requires at least one existing snapshot and uses the latest
  snapshot by `sequence` as its parent.
- Every snapshot (incremental or full) has a complete manifest and may reuse
  any existing verified blob. Full mode is a complete logical snapshot that
  can reuse unchanged content; incremental mode additionally requires and
  records a parent and is the explicit signal that the operator expects a
  delta-sized write.
- A parent manifest that references a missing blob makes an incremental backup
  fail rather than silently rewriting history.
- Reports distinguish logical bytes, newly written plaintext bytes,
  deduplicated bytes, parent-attributed dedup, store-attributed dedup, newly
  stored envelope bytes, and new/reused blob counts. For one snapshot:
  `written_bytes + deduplicated_bytes = logical_bytes`.
- Duplicate paths or repeated content within one snapshot are also deduplicated
  by hash and counted as reused.

Creation writes blobs first, then the authenticated manifest, then the
append-only created event. A crash before the manifest leaves unreferenced
blobs, which `backup-prune` can remove. An interrupted backup never leaves a
manifest that references a missing blob.

## Prune, tombstones, and crash behavior

`backup-prune --keep N` retains the latest N snapshots by sequence (`N >= 1`)
and removes older snapshots plus blobs not referenced by any retained manifest.
It validates first:

1. repository metadata authenticity and KDF bounds;
2. every manifest, including authentication, format, allowlist, totals, and
   strictly linear sequence/parent chain;
3. the nonce table;
4. presence and header hash of every blob referenced by a retained snapshot;
5. a complete inventory of `blobs/` with canonical hash-shaped names.

Dry-run performs the same validation and planning but writes nothing. The report
lists snapshots to remove, retained/protected counts and bytes, unreferenced blob
counts and bytes, and exact reclaimed bytes (pruned manifest envelopes plus
unreferenced on-disk blobs).

A real prune commits authenticated tombstones to `repository.json` before
deleting:

```json
{
  "id": "<snapshot uuid>",
  "sequence": 2,
  "created_at": "...",
  "pruned_at": "...",
  "manifest_sha256": "<sealed manifest sha256>"
}
```

The tombstone list is a contiguous prefix; the first retained snapshot's parent
must be the last tombstone and its sequence must be tombstones + 1. Scans ignore
manifests whose ids are tombstoned. Therefore a crash after the metadata commit
but before file cleanup leaves ignored manifest files that a later prune
removes, while retained snapshots remain valid. A crash before the metadata
commit leaves a fully consistent old repository. This is not a filesystem
transaction across sudden power loss, but the metadata commit is the logical
prune boundary and no retained snapshot ever depends on a deleted blob.

Only unreferenced blobs are removed. Shared content remains; a later dry-run
reports it as protected. The latest `N` boundary is never deleted, and `N = 0`
is rejected.

## Restore validation and atomicity

Repository restore is stricter than list:

1. Reject invalid snapshot ids, symlinked repositories, and data/repository
   overlap.
2. Open the repository, authenticate metadata and KDF bounds, require the
   passphrase for encrypted repositories, and validate the nonce table.
3. Load every active manifest, authenticate it, verify format/allowlist/totals,
   and validate the full linear chain (including tombstoned parents).
4. Resolve the requested snapshot; tombstoned/pruned snapshots are not found.
5. Inspect the target: reject symlinks/non-directories; refuse discovery or a
   held instance lock; without `--force`, refuse an existing database, an
   instance lock, or any non-empty target.
6. Decrypt, decompress, hash, and size-check every referenced blob into a
   private sibling scratch directory. Each entry is materialized only after
   its hash matches the manifest. A missing or tampered blob aborts before the
   target is touched.
7. Verify the restored database has a SQLite header, re-check the daemon, then
   move the existing target aside and rename the validated tree into place. If
   the final rename fails, the previous target is restored. Scratch is removed
   on all error paths.

Wrong passphrase, bad KDF parameters, tampered metadata/manifest/blob,
truncated/missing parent, unsupported format, path traversal, symlink entry,
artifact-flag mismatch, and partial chains all fail closed before the existing
data directory is modified. Restore never stops or signals a daemon.

`backup-list` validates metadata, manifests, chain, nonce table, canonical blob
names, and blob presence by default. `--verify` additionally decrypts and hashes
every unique referenced blob. It returns a report with missing/corrupt counts
and bounded issues rather than claiming validity when a blob is absent. A list
failure is a hard error for authentication/parse/format problems.

## Secrets and CLI convention

- A passphrase is never accepted as a command-line value. There is no
  `--passphrase` option; `--passphrase-env NAME` reads the named environment
  variable and `--passphrase-file PATH` reads a file.
- The two options are mutually exclusive. `--plaintext` is mutually exclusive
  with both.
- Encrypted repository mode is the default when `--repository` is used. If
  neither a passphrase source nor `--plaintext` is supplied, the CLI fails with
  an explicit instruction. It never prompts.
- Library code takes an owned [`Passphrase`] only; it never reads the
  environment or prompts. Tests provide deterministic passphrases directly.
- Environment values must be UTF-8 and non-empty. Passphrases are capped at
  4096 bytes and may not contain NUL.
- Passphrase files must be regular, non-symlinked, `1..=4096` bytes, and not
  accessible by group or others on Unix (`mode & 0o077 == 0`). One trailing
  `\n` (and an optional preceding `\r`) is stripped.
- `Debug` for `Passphrase` is `Passphrase(<redacted>)`. No error includes the
  passphrase, derived key, or plaintext. Repository files are scanned by tests
  for the test passphrase and plaintext database/event markers.
- A wrong or missing passphrase reports only that authentication failed or that
  a passphrase is required. It does not distinguish which field failed in a way
  that would help an offline attacker beyond AEAD/HMAC failure.

`VERLINPARK_BACKUP_PASSPHRASE` is a reasonable environment-variable name to
document for operators, used as `--passphrase-env VERLINPARK_BACKUP_PASSPHRASE`.
The name is not built into the library. A service manager or secret file can
populate it; the CLI does not read a default variable implicitly, so an
unrelated environment cannot silently change encryption behavior.

The v1 ZIP path is explicitly unencrypted. Supplying repository passphrase
options to a v1 backup or restore is rejected at the CLI, and the library API
for v1 has no encryption parameter.

## Reports

`RepositoryBackupReport` includes repository path, snapshot format/repository
format versions, snapshot id, sequence, parent, creation time, incremental and
encrypted flags, artifact inclusion, configured compression, entry/session/
artifact counts, logical bytes, written/deduplicated bytes, parent/store dedup
attribution, blob counts, stored envelope bytes, manifest bytes and hashes,
KDF summary (algorithm/version/costs/output length/fingerprint, no salt or key),
and app/protocol versions.

`RepositoryListReport` includes repository format/encryption/KDF, creation time,
per-snapshot id/sequence/parent/time/incremental/encrypted/artifact flags,
entries/sessions/artifacts/logical bytes/unique blobs/manifest bytes/hash,
logical and unique plaintext totals, referenced stored bytes, unreferenced blob
counts/bytes, nonce record count, and validation results.

`RepositoryRestoreReport` includes repository/snapshot/sequence/parent/time,
output paths, format/encryption/artifact flags, entry/session/artifact counts,
logical bytes, validated unique blobs, stored blob bytes, force replacement
status, and KDF summary.

`RepositoryPruneReport` includes keep, dry-run/applied status, totals, kept
count, per-pruned-snapshot id/sequence/parent/time/bytes/manifest bytes, blob
counts, protected bytes, reclaimed bytes, and validation results.

All reports are serializable to JSON and have CLI human-readable forms. No
report contains a passphrase, salt, key, or plaintext file content.

## Concurrency

Mutations (backup and real prune) take an exclusive advisory `flock` on
`repository.lock` with a non-blocking failure mode. Listing, dry-run prune, and
restore take a shared lock. Snapshot and blob files are immutable and published
with hard links. `repository.json` is replaced by an atomic same-directory
rename after a fully written private temporary file. There is no daemon
coordination; concurrent backups against the same repository fail rather than
racing. This is a single-host file lock, not a distributed lease.

## Security properties and non-claims

Provided:

- confidentiality of blob and manifest plaintext at rest against someone who
  obtains the repository without the passphrase;
- integrity/authentication of encrypted manifests, every encrypted blob chunk,
  and mutable metadata in encrypted mode;
- fail-closed behavior for wrong passwords, KDF tampering, truncation, missing
  parents, missing/tampered blobs, path traversal, symlinks, and partial
  chains;
- plaintext hash/size verification after decryption;
- atomic data-directory replacement on the normal success/failure paths;
- no secret in argv, repository metadata, manifests, nonce records, event logs,
  or errors.

Not claimed:

- **No v1 encryption.** ZIP archives remain unencrypted and unauthenticated.
- **No authenticated plaintext repository.** `--plaintext` is explicit but
  provides only accidental-corruption detection. Anyone who can rewrite the
  repository can rewrite a self-consistent plaintext snapshot.
- **No cloud escrow, key recovery, key rotation, or multi-recipient access.**
  Losing the passphrase loses the encrypted data. There is no admin recovery
  key. Changing the passphrase is not supported without a future re-encryption
  tool.
- **No hardware-backed key or HSM integration.** Keys exist in process memory
  during backup/restore.
- **No protection against a compromised host while the operation runs.** A
  process with the passphrase, memory access, or debugger access can read
  plaintext.
- **No distributed or concurrent multi-writer repository.** The lock is local
  and advisory; network filesystems that do not implement reliable `flock` are
  outside the claim.
- **No crash-consistent multi-file prune transaction.** Tombstone-first
  ordering makes crashes recoverable and keeps retained snapshots valid, but a
  sudden power loss can leave cleanup files that a later prune removes.
- **No live-daemon quiesce.** Backup can run against a live daemon, but the
  SQLite snapshot and per-file copies are not a cross-file point-in-time
  snapshot. Restore refuses while a daemon appears to be running.
- **No worktree, Git, `sources/`, logs, scratch, discovery token, lock, or
  secret backup.** The v1 allowlist and exclusions are preserved exactly.
- **No encryption of the data directory or daemon state in place.** Only
  repository backup blobs/manifests are encrypted.
- **No full filesystem read confinement for the backup process.** It reads the
  selected data directory allowlist and writes the repository/staging paths.
- **Crash staging cleanup is best effort.** SQLite snapshots and source copies
  are staged in a private `0700` repository `tmp/` directory before sealing.
  Normal and error paths remove it; a process killed mid-backup can leave a
  private staging directory until the next write operation, which sweeps
  `.verlinpark-*` entries while holding the exclusive lock. Full-disk
  encryption is still recommended for hosts where physical media theft matters.
- **No anti-rollback or anti-replay protection.** An attacker who can replace
  the entire repository with an older valid copy can present an older snapshot.
  The chain, tombstones, and metadata MAC provide internal consistency, not a
  trusted external monotonic counter. Keeping a copy of the latest metadata or
  external snapshot ids is an operator control, not a format guarantee.
- **No authenticated nonce-table guarantee.** Nonce records are an audit aid;
  the actual nonce is inside each envelope. Deleting or editing the table cannot
  make a wrong key or tampered ciphertext decrypt, but the table itself is not
  an independently signed object.
- **Environment passphrases remain in the process environment** for the life of
  the process, where same-user processes may be able to inspect them. A `0600`
  passphrase file or a service-manager secret injection is preferable when that
  threat matters.
- **No proof against traffic analysis.** Blob sizes and snapshot counts reveal
  approximate data volume and change patterns; filenames and timestamps are
  visible in plaintext metadata/manifests' envelope headers.
- **No post-quantum claim.** Argon2id and ChaCha20-Poly1305 are used with their
  standard security assumptions.
- **No repair of a corrupt repository.** Restore and prune fail closed; there
  is no automatic reconstruct-from-parity, remote replica, or salvage mode.

## Alternatives considered

- **Encrypt the v1 ZIP.** Rejected: no standard authenticated ZIP primitive in
  the chosen stack, full rewrite remains, and it would risk the compatibility
  contract of the existing restore path.
- **Encrypt each ZIP and use filesystem snapshots for incrementality.**
  Rejected: depends on host filesystem features, does not provide a portable
  content-addressed repository, and complicates partial-chain validation.
- **A single encrypted tar/database file.** Rejected: full rewrite, no
  content-addressed dedup, and restore would need to materialize huge temporary
  trees before validation.
- **AES-GCM instead of ChaCha20-Poly1305.** Both are acceptable AEADs; this
  codebase already standardizes streaming ChaCha20-Poly1305 for local artifacts
  and it has no AES hardware-dispatch dependency requirement. Argon2id is used
  for memory-hard password derivation rather than a fast hash.
- **Store a passphrase-derived key encrypted in metadata.** Rejected: would
  reduce the passphrase to a key file and require another secret root. No
  escrow is claimed.
- **Delta manifests or tree objects.** Rejected for this round: complete
  manifests make prune/tombstone and restore validation simple and auditable.
  Content addressing already provides byte-level dedup.
- **Delete old snapshots without tombstones.** Rejected: retained chains would
  reference missing parents and could not distinguish intentional prune from
  corruption.
- **Use the daemon to quiesce backups.** Rejected: backup must work offline and
  restore must never depend on or stop a running daemon.

## Consequences

- Operators gain an authenticated, encrypted repository whose unchanged data is
  not rewritten, while v1 ZIP workflows continue to work.
- Repository files are not interchangeable with v1 ZIPs; tooling must choose the
  mode explicitly.
- A lost passphrase is unrecoverable by design.
- Prune is destructive but retains the latest N and protects all blobs
  referenced by retained manifests; dry-run is available and recommended.
- The KDF upper bound and per-file/manifest caps impose operational limits that
  are documented and enforced rather than discovered at OOM time.
- The repository is a CLI/offline surface, not an event-sourced or daemon-owned
  projection. Created/restored/pruned events and nonce records are operational
  audit logs; repository validity comes from metadata authentication, manifests,
  and blob verification.
- Future format changes require a repository format version bump and a new ADR.
  Silent changes to canonical JSON, envelope bytes, KDF semantics, chain
  validation, or prune tombstones are not permitted within format version 2.

## Test coverage

The implementation is covered by unit and integration tests for:

- v1 ZIP round-trip and v1+v2 coexistence;
- encrypted and plaintext repository round-trips;
- exact incremental dedup byte accounting and blob reuse;
- full and incremental parent-chain restore;
- wrong passphrase, tampered metadata, manifest, blob, trailing data, and
  truncation;
- missing/truncated parent and partial chain;
- KDF bounds and bad persisted parameters;
- path traversal, absolute paths, symlink blob/manifest/repository, and
  repository/data overlap;
- restore atomicity with pre-existing target sentinels;
- prune dry-run, real prune, zero-keep refusal, missing retained blob refusal,
  shared-blob protection, and prune tombstones;
- nonce table uniqueness and malformed record rejection;
- no passphrase or plaintext marker in any repository file or error;
- CLI parse, missing-mode errors, environment/file passphrase resolution,
  permission refusal, exit codes, help, v1 commands, and v2 list/restore/prune.

The tests use temporary directories, deterministic fast KDF parameters, and no
network. They do not prove cryptographic security; they exercise the format
and fail-closed control flow. A real deployment should still review Argon2id
costs against its hardware and protect the passphrase distribution channel.
