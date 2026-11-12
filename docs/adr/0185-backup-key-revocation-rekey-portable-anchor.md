# ADR-0185: Backup key revocation, rekey, and portable anchor

- **Status:** Accepted
- **Date:** 2026-11-12
- **Deciders:** VerlinPark runtime maintainers
- **Related:** ADR-0167 (encrypted backup repository), ADR-0181 (signed
  backups, key rotation, anti-rollback), ADR-0170 (typed tool/CLI error
  contract), `docs/GAP_CLOSURE_PLAN.md` I4

## Context

ADR-0181 introduced v3 backup repositories with Ed25519-signed metadata and
snapshot manifests, a local keyring with rotation, and a local anti-rollback
anchor. That round deliberately stopped before three operational problems:

1. **Revocation.** Rotation retires a key for selection, but a retired key can
   still sign if requested explicitly and its signatures are still trusted.
   There is no way to say "this key was compromised; do not accept new
   signatures from it" while preserving the public key needed to inspect and
   report historical snapshots.
2. **Rekey.** A repository adopts a rotated key only on its *next* signed backup
   or prune. If the old key is lost or revoked, every existing signed object
   still names the untrusted key, so `backup-verify`, `restore`, and further
   backups fail closed with no recovery path other than rewriting the
   repository from scratch.
3. **Portable rollback state.** The anti-rollback anchor is local to one data
   directory. A fresh host has no high-water mark and can silently restore an
   old signed repository that it has never seen. Operators need a way to carry
   *only* the public high-water evidence to another host without copying the
   whole data directory or claiming remote attestation.

The constraints from ADR-0181 still apply: no key escrow, no cloud KMS/HSM, no
transparency log, no trusted timestamps, and no secret in SQLite, events, logs,
errors, or the desktop UI.

## Decision

### 1. Revocation is an additive, fail-closed keyring state

- `KeyringEntry` gains an optional `revoked_at`. It is serde-defaulted so
  existing keyrings load unchanged.
- `keyring.json` remains 0600 and atomic. Revocation never removes the public
  key: historical reports must still be able to name and verify the signer.
  No private material is added anywhere.
- `vp backup-key-revoke <key-id>`:
  - is idempotent; a second call reports `changed=false` and the original
    timestamp;
  - refuses the **active** key with typed `BACKUP_KEY_REVOKE_ACTIVE` unless
    `--force` is explicit;
  - `--force` marks the active key revoked and clears `active_key_id`, which
    disables signing until another key is generated or rotated in. This is the
    only keyring shape in which entries exist without an active key, and the
    structural validator permits it only when every entry is revoked;
  - appends a public, secret-free `key.revoked` line to a 0600
    `keyring-events.jsonl` in the key directory. Key generation, import,
    registration, and rotation append matching lifecycle events; a torn final
    line from an interrupted append is repaired before the next write.
- Trust semantics:
  - signing with a revoked key is always refused (`BACKUP_KEY_REVOKED`), even
    when the key id is requested explicitly;
  - verification and restore of signed metadata/manifests fail with
    `BACKUP_KEY_REVOKED` unless the operator passes `--allow-revoked`;
  - `--allow-revoked` verifies the same Ed25519 signature but records
    `revoked=true` on the signature check, emits report warnings, and appends a
    `snapshot.revoked_key_allowed` audit event to the repository events log for
    both verify and restore;
  - `backup-list` reports `revoked_signatures` and `revoked_key_ids`; it does
    not write an event because listing is otherwise read-only;
  - a revoked key cannot be rotated back to active.
- Secret-free summaries expose `revoked_at` per key and `revoked_key_count`.
  `vp backup-key-events` reads the bounded lifecycle log newest-first and fails
  closed on malformed complete lines or lines that appear to contain secret
  material. `backup.status` propagates the revoked count plus the anchor's
  imported-row count.

Revocation is **not** a certificate revocation list served over a network and
does not invalidate signatures cryptographically. It is a local trust decision:
the same bytes still verify, but this keyring refuses to trust them by default.

### 2. Rekey rewrites signatures only

`vp backup-rekey --repository <dir>` is the recovery path after rotation or
revocation.

- Preconditions:
  - repository format must be 3 (typed `BACKUP_UNSIGNED_REPOSITORY` for v2;
    v1 ZIP archives are not repositories and are refused by root validation);
  - metadata must carry a signing block and a metadata signature;
  - the keyring must contain the historical public keys needed to verify every
    existing signed object, plus private material for the **active** key;
  - the active key must not be revoked;
  - the operation is bounded to 10,000 snapshots and 512 MiB of manifest
    envelopes per run, and `--max-snapshots <n>` can select a smaller
    resumable batch. An incomplete batch reports `complete=false`,
    `snapshots_remaining`, and leaves later signatures for the next run.
- For every signed snapshot manifest:
  1. decrypt/authenticate it (encrypted repositories) and validate its chain
     position and entry table;
  2. verify the old signature with a revocation-tolerant read-side keyring;
  3. replace only the `signature` field with a signature from the active key;
  4. re-seal the manifest envelope with a fresh AEAD nonce, append the new
     manifest nonce record, and publish the new envelope by atomic `rename`.
- Unsigned historical snapshots in a mixed v2→v3 chain are **kept unsigned**.
  Retroactively signing them would claim authenticity for bytes nobody
  authenticated.
- After all manifests are published, the metadata MAC is recomputed, the signed
  head is rebuilt from the actual on-disk envelopes, and metadata is re-signed
  and written atomically as the final commit point.
- Preservation checks fail the operation if the repository id, format version,
  encryption parameters, generation sequence, parent links, entry table,
  totals, creation timestamps, or prune tombstones change.
- Encrypted blobs are opened read-only for a ciphertext fingerprint before and
  after the run. Their bytes and nonce records are never written. The report
  exposes `blob_ciphertext_sha256` and `blob_ciphertext_unchanged`; a mismatch
  is a hard `BACKUP_REKEY_FAILED`.
- Idempotence/resumption:
  - manifests already signed by the active key are skipped;
  - metadata is only rewritten when a signature, head, or key changed;
  - a second run is byte-for-byte a no-op;
  - each manifest replacement is atomic under the repository's exclusive lock,
    so an interruption leaves a mixed-key chain where every signature still
    verifies under the keyring; re-running completes the job;
  - `complete` is derived from the number of signed snapshots that still point
    at a non-active key, not from wishful counting.
- The report includes previous/current key ids, revoked source keys, counts of
  resigned/unchanged/unsigned-preserved snapshots, generation, preservation
  booleans, blob counts/bytes/digest, and the public keyring summary. It never
  contains a seed, passphrase, derived key, or plaintext.

### 3. Portable anchor documents are signed, raise-only evidence

`vp backup-anchor-export` writes a signed JSON document with:

- `kind = verlinpark-portable-backup-anchor`, format version 1;
- `exported_at`, exporting `key_id`;
- one entry per repository containing only `repository_id`,
  `highest_generation`, `last_verified_generation`, optional last snapshot id
  and operation, and the observation timestamp;
- a detached Ed25519 `signature` record (algorithm, key id, base64 signature,
  signed time) over the canonical document body with the signature field absent,
  using a separate domain separator.

Repository paths are omitted because a portable document may be imported where
the repository lives elsewhere.

`vp backup-anchor-verify` is a read-only preview: it loads the document,
  verifies the signature under the same revocation policy, compares it with the
  local anchor, and reports which rows would be raised without writing
  anything.

`vp backup-anchor-import`:

- validates structure, bounds (1 MiB, 64 repositories), UUIDs, generations,
  timestamps, and the signature before touching the local anchor;
- verifies the signer through the same revocation policy as repository
  signatures (`BACKUP_KEY_REVOKED` unless `--allow-revoked`);
- rejects a missing/unknown key (`BACKUP_KEY_NOT_FOUND`), a tampered body or
  wrong key material (`BACKUP_ANCHOR_SIGNATURE_INVALID`), and malformed
  documents (`BACKUP_PORTABLE_ANCHOR_INVALID`);
- merges strictly **raise-only**: `highest_generation` and
  `last_verified_generation` can increase or stay equal, never decrease;
- creates new local rows with `origin=portable_import`, the synthetic path
  `(portable import)`, and operation `anchor_import`;
- preserves an existing local path/operation/timestamp when nothing is raised;
- reports per-repository previous/new generations and whether each row was
  raised;
- can reject an export older than the local anchor with `BACKUP_ANCHOR_STALE`
  (`--require-newer`), while the default still merges it because merging can
  only raise;
- appends a public `anchor.imported` audit event to a 0600
  `backup-anchor-events.jsonl` next to the anchor;
- serializes anchor read-modify-write cycles with a 0600
  `backup-anchor.lock` (`fs2` exclusive lock), so concurrent backups of
  different repositories cannot lose each other's high-water marks.

### 4. Compatibility

- `revoked_at`, keyring/status counts, anchor `origin`, portable-anchor fields,
  report warning arrays, and repository validation counters are additive and
  serde-defaulted. Existing keyrings, v2 repositories, v1 ZIP archives, and
  older anchor files load unchanged.
- A local anchor file written by this build may contain `origin` and
  `anchor_import`; older builds are not promised forward compatibility with
  those rows.
- v2 repositories remain readable, restorable, and explicitly unsigned. Rekey
  refuses them.
- Service remains protocol-major-version 1 and CLI-compatible for existing
  flags. `backup.status` adds fields only.

### 5. API, CLI, and desktop surface

- Library:
  - `backup::signing::{Keyring::revoke_key, Keyring::public_key_for_verification,
    Keyring::with_revoked_keys_allowed, KeyRevocationReport}`;
  - `backup::repository::{rekey_repository, RepositoryRekeyOptions,
    RepositoryRekeyReport, list_repository_snapshots_with_policy}` and
    `allow_revoked` on verify/restore options;
  - `backup::anchor::{export_portable, import_portable, load_portable_anchor,
    verify_portable_anchor, PortableAnchor*}`.
- CLI: `backup-key-revoke`, `backup-key-events`, `backup-rekey`
  (`--max-snapshots`), `backup-anchor-export`, `backup-anchor-verify`,
  `backup-anchor-import`, and `--allow-revoked` on `backup-verify`,
  `backup-list`, `restore --repository`, and portable-anchor verification.
- Daemon: `backup.status` adds revoked-key counts, signed portable-anchor format
  version, imported-row count, and per-key/per-anchor revoked/origin fields. It
  remains read-only and returns no private material.
- Swift Settings → About shows revoked-key count, portable-anchor format and
  imports, and the expanded local-only/no-escrow note.

## Threat model

### Covered

- A stolen or compromised **retired** signing key cannot create new accepted
  signatures after revocation: signing is refused and verification rejects it
  unless the operator explicitly overrides for a historical read.
- A repository whose objects name a revoked key can be recovered without
  re-encrypting content; ciphertext is proven unchanged, and the chain,
  entries, tombstones, and generations are proven preserved.
- A portable anchor cannot silently lower local rollback protection, cannot be
  tampered with without detection, and cannot be signed by a revoked or unknown
  key by default.
- Private seeds, passphrases, and derived keys do not appear in reports, events,
  audit logs, SQLite, or the UI.

### Not covered

- **Compromised active key.** Revocation is retrospective. An attacker who holds
  the active, unrevoked private key can sign new valid objects until the key is
  rotated and revoked.
- **Local trust root.** A user, administrator, or root who can rewrite the
  keyring, the local anchor, and the repository can defeat revocation and
  rollback checks. These are local files, not tamper-proof hardware.
- **Key escrow/recovery.** Losing all copies of the private key makes rekey
  impossible. Rekey does not reconstruct a lost key or decrypt data.
- **Remote attestation.** A portable anchor proves which key exported the
  document and what that host observed. It is not a signed timestamp, a
  transparency log, remote attestation, or proof that the repository contents
  are correct.
- **Replay across fully compromised hosts.** An attacker with both the signed
  repository and a signed *old* portable anchor can still replay on a host that
  has never seen a newer observation.
- **Network policy.** A portable anchor is a file; this round adds no key
  directory service, revocation feed, or online freshness protocol.
- **Rekey of unsigned history.** Mixed chains keep their unsigned snapshots
  unsigned; rekey does not retroactively authenticate them.
- **Mid-stream exactness.** Rekey is atomic per object and resumable, but a
  process kill between the metadata MAC update and the metadata signature write
  fails closed on the next open (`BACKUP_SIGNATURE_INVALID`/`_MISSING`) and
  requires a resume run; it is not a distributed transaction.

## Non-goals

- No key escrow, cloud KMS, HSM, recovery service, or key transparency log.
- No certificate chain, PKI, revocation feed, or automatic key distribution.
- No remote attestation, trusted timestamp, or hardware anti-rollback.
- No re-encryption or passphrase rotation in `backup-rekey`; blob ciphertext and
  KDF parameters are unchanged.
- No automatic signing of previously unsigned snapshots.
- No change to approval, sandbox, path-jail, write-scope, Verifier, merge,
  capacity, or budget gates.
- No secret in SQLite, daemon events, logs, error messages, or the Swift UI.

## Consequences

- Revoking the active key without `--force` is refused. Operators must generate
  or rotate to a replacement first. Forced revocation intentionally leaves the
  keyring unable to sign until a replacement is active.
- After revoking the key that signed existing repositories, verify/restore/backup
  fail closed until `backup-rekey` completes or the operator explicitly reads
  with `--allow-revoked`.
- Rekey changes every signed manifest envelope (new signature and AEAD nonce) but
  not blob ciphertext, entries, generations, or tombstones. Reports make that
  distinction explicit rather than claiming the repository bytes are unchanged.
- Rekey requires the passphrase for encrypted repositories and private material
  for the active key; it is therefore a CLI-first operation.
- Portable anchors add two small 0600 files (`backup-anchor-events.jsonl`, plus
  the exported document) and remain local state. Exporting an anchor reveals
  repository ids and generation counts, not content or paths.
- Tests cover revoke reject/allow, active-key force guard, keyring lifecycle
  audit reading and torn-line repair, rekey
  ciphertext-preserving and signature-valid, old/new key verification,
  idempotence, resumption after a partial rewrite, bounded batches, prune
  tombstone preservation, v2/mixed compatibility, anchor export/import
  tamper/wrong-key/revoked/stale behavior, raise-only semantics, concurrent
  anchor writers, secret non-leakage, CLI round-trip, and the daemon status
  surface. Existing backup tests remain green.
