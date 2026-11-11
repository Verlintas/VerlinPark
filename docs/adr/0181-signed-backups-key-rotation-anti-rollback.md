# ADR-0181: Signed backups, key rotation, and anti-rollback

- **Status:** Accepted
- **Date:** 2026-11-12
- **Deciders:** VerlinPark runtime maintainers
- **Related:** ADR-0167 (encrypted backup repository), ADR-0170 (tool error
  contract), `docs/LONG_RUN_OPERATIONS_HARDENING_PLAN.md` H5

## Context

The repository backup format (v2) already provides:

- Argon2id key derivation and ChaCha20-Poly1305 authenticated encryption for
  blobs and snapshot manifests;
- an HMAC-SHA-256 metadata MAC over `repository.json`, including the prune
  tombstone table;
- content-addressed blobs, a linear parent chain, and atomic restore.

That protects repository files from someone who does not know the passphrase,
but it does not answer three operational questions:

1. **Authenticity.** A passphrase holder, or a compromised process that has the
   passphrase, can rewrite every manifest, MAC, tombstone, and blob
   self-consistently. The repository cannot prove it was produced by this
   installation rather than by an attacker with the same passphrase.
2. **Key lifecycle.** There is no key identity, no public-key registry, no
   rotation command, and no way to keep old snapshots verifiable after a key
   change.
3. **Rollback.** Restore can pick any snapshot. Nothing records the newest
   generation this data directory has already verified, so replaying an older
   but internally valid signed repository is undetected.

v1 ZIP backups are unauthenticated and stay that way in this round.

## Decision

### 1. Format v3 adds Ed25519 signatures

- The content encryption stays Argon2id + ChaCha20-Poly1305. v3 does not change
  blob or manifest envelopes, nonces, compression, deduplication, or the restore
  swap protocol.
- A new repository is v3 only when the caller supplies a signing key. Without a
  key, new repositories remain **v2 unsigned**. This keeps existing scripts and
  tests on the v2 path and avoids silently claiming authenticity.
- v3 metadata contains a `signing` block:
  - `active_key_id`;
  - `required` (`true` for repositories created signed, `false` for an upgraded
    v2 chain with historical unsigned snapshots);
  - `signed_at`;
  - `metadata_signature`;
  - an optional `head` with the latest generation, snapshot id, manifest
    SHA-256, parent, entry-table SHA-256, tombstone-table SHA-256, and time.
- The **metadata signature** is Ed25519 over canonical `repository.json` with
  both `metadata_mac` and `signing.metadata_signature` absent. It therefore
  covers the repository id, format version, encryption parameters, timestamps,
  the complete prune-tombstone table, the active key id, and the signed head.
  The metadata MAC is computed first and included in the signed payload.
- Each signed **snapshot manifest** carries a `signature` record inside the
  authenticated manifest body. The signature is over a domain-separated
  canonical statement containing:
  - repository id, snapshot id, generation (`sequence`), parent, and
    `created_at`;
  - signing key id;
  - the SHA-256 of the canonical manifest body with the signature field absent,
    which covers the complete entry table and totals.
- Together these objects cover every field required by H5: repository id,
  generation, parent, entry table, tombstones, created_at, and key id. The
  metadata signature is authoritative for the mutable tombstone table; the
  per-snapshot signature is authoritative for the immutable chain and entries.
- On every signed backup, prune, or rotation, the metadata is re-signed and its
  head is updated to the latest retained snapshot. A crash between publishing a
  manifest and updating the head leaves a *stale but signed* head; `backup
  verify` reports it as an inconsistency, and the next signed backup repairs it.
- Signature verification happens when a snapshot is loaded, not only in
  `backup-verify`. A v3 `required` repository fails closed on an unsigned
  snapshot; a mixed upgraded repository reports historical snapshots as
  `unsigned` instead of pretending they are verified.

### 2. Keyring and rotation

- The keyring lives outside the data directory by default:
  `~/.verlinpark/backup-keys/keyring.json`, overridable with
  `VERLINPARK_BACKUP_KEY_DIR` or `--keyring-dir`. It is never written into a
  backup repository or restored over.
- `keyring.json` is 0600 and contains only key ids, algorithm names, public
  keys, creation/retirement timestamps, the active key id, and **secret
  references**. It never contains private bytes.
- Private material comes only from:
  - a 0600 key file in the key directory (JSON with a 32-byte Ed25519 seed), or
  - an environment variable containing the canonical base64 seed, registered by
    `backup-key-generate --from-env` / `backup-key-import --from-env`.
- The seed is held in `zeroize` memory, has a redacted `Debug`, and is never
  serialized into reports, errors, events, SQLite, or the repository.
- Commands:
  - `vp backup-key-generate <key-id>`;
  - `vp backup-key-list`;
  - `vp backup-key-rotate <key-id>` (new key by default, `--use-existing`, or
    `--from-env`);
  - `vp backup-key-export-public`;
  - `vp backup-key-import`;
  - `vp backup-keyring`;
  - `vp backup-anchor` (show or explicit reset).
- Rotation changes `active_key_id` in the keyring. Old public keys stay in the
  keyring with `retired_at` and remain usable for verification, so historical
  snapshots are not invalidated. A repository adopts a rotated key on its next
  signed backup/prune; its metadata signature is replaced with the new key.
- `vp backup --signing-key <id>` (or `--sign` / `--keyring-dir`) creates or
  upgrades a signed repository. `vp backup-verify`, `backup-list`, and
  `backup-prune` accept `--keyring-dir`. Missing keyring, unknown key id, and
  mismatched key material are typed failures:
  `BACKUP_KEYRING_NOT_FOUND`, `BACKUP_KEY_NOT_FOUND`, `BACKUP_KEY_INVALID`,
  `BACKUP_KEYRING_REQUIRED`, `BACKUP_SIGNATURE_INVALID`.

### 3. Verification and anti-rollback

- `vp backup-verify --repository <dir>` verifies, without restoring:
  - metadata MAC (encrypted repositories) and metadata signature;
  - every snapshot signature, manifest structure, and parent chain;
  - the signed head against the actual latest snapshot;
  - tombstone contiguity and generation;
  - every referenced blob (full decrypt/hash by default, presence with
    `--presence-only`).
- The local anchor is `backup-anchor.json` in the data directory, 0600. It
  tracks, per repository id:
  - `highest_generation` (high-water mark);
  - `last_verified_generation`;
  - last snapshot id, repository path, operation
    (`backup|verify|restore`), and timestamps.
- A successful backup, verification, or restore records the observed
  generation. `backup-verify` and `restore` reject a generation below
  `highest_generation` with `BACKUP_ROLLBACK_DETECTED` unless
  `--allow-rollback` is explicit.
- `--allow-rollback` records the lower generation as `last_verified_generation`
  but **does not lower `highest_generation`**, so a later silent rollback is
  still rejected.
- A restore that replaces the data directory preserves the existing anchor
  across the swap before recording the restored generation. Without this, a
  restore would delete the anchor and accidentally reset rollback protection.
- Anchor read/write failures fail closed. Corrupt, symlinked, group-readable,
  or structurally invalid anchors are `BACKUP_ANCHOR_INVALID` / `_IO`, not
  silently ignored.
- Anchor rows are bounded to 64 repositories; the least recently verified row
  is evicted when the bound is exceeded.
- Verification is read-only except for the anchor update. It does not restore,
  mutate snapshots, or contact a network.

### 4. Compatibility

- v2 repositories remain fully readable, listable, restorable, and prunable.
  Reports include `signed=false`, `signing_status="unsigned"`,
  `authenticity="unsigned"`, and a clear issue that contents are encrypted/MACed
  but not authentic.
- A v2 repository can be upgraded by taking a signed backup:
  - metadata becomes v3;
  - `required=false` because older snapshots remain unsigned;
  - historical snapshots report `unsigned`; new snapshots are signed;
  - `backup-verify` reports `authenticity="mixed"` and an explicit issue rather
    than a false "fully signed" claim.
- v1 ZIP archives remain byte-for-byte on the old path, unauthenticated, with no
  signed status.
- New fields are serde-defaulted and optional. v2 readers are not promised
  forward compatibility with v3 files; this is a format version bump.
- The repository format validator accepts versions 2..=3. Unknown versions fail
  with `BACKUP_REPOSITORY_FORMAT`.

### 5. API and desktop surface

- Library: `vp_core::backup::signing` (keyring/signing), `backup::anchor`
  (anti-rollback), `repository::{verify_repository, RepositoryVerifyOptions,
  RepositorySigningOptions}`, and signed restore/prune variants.
- CLI: `backup --signing-key`, `backup-verify`, `backup-key-*`,
  `backup-keyring`, `backup-anchor`, `--allow-rollback`, `--keyring-dir`,
  `--anchor-file`.
- Daemon: read-only `backup.status` returns the anchor summary and public
  keyring summary. It never returns private material. Signing and restore stay
  CLI-first because the daemon does not own passphrases or key files.
- Swift Settings → About shows signed-format support, active public key id, key
  count, anchor generation, and the local-anchor limitation note.

## Threat model

### Covered

- A party who obtains repository files but not the signing key cannot:
  - add, remove, or modify an entry while keeping a valid snapshot signature;
  - alter the parent chain, generation, created_at, or repository id in a
    manifest;
  - alter prune tombstones, the active key id, or the signed head without
    invalidating the metadata signature;
  - replay an older signed repository generation without tripping the local
    anchor.
- A v2 repository is never reported as authenticated.
- Private key bytes do not appear in repository files, events, reports, errors,
  or the keyring.

### Not covered

- **Local anchor tampering.** The anchor is owned by the same OS user as the
  data directory. That user, an administrator, or root can rewrite both the
  repository and the anchor. This is not tamper-proof anti-rollback, hardware
  monotonic counters, or remote attestation.
- **Compromised host during signing/restore.** An attacker who can read process
  memory or the 0600 key file while signing has the key.
- **Passphrase disclosure.** Signing does not replace encryption. A passphrase
  holder still decrypts content; signatures prove origin, not confidentiality.
- **Key loss.** Losing the keyring does not destroy encrypted data, but v3
  verification fails closed and restore refuses without a keyring. Public keys
  must be exported and private key files backed up out-of-band.
- **Revocation/transparency.** Rotation retires keys for selection but there is
  no revocation list, transparency log, certificate chain, or proof that a key
  was not compromised.
- **Multi-user access control.** The keyring is a single-user local file, not a
  shared HSM/KMS policy.
- **Remote or cross-host guarantees.** A copied repository plus a copied anchor
  can be replayed on another host that has not seen the newer generation.

## Non-goals

- No key escrow, cloud KMS, HSM integration, or recovery service.
- No remote attestation, signed timestamps, transparency log, or hardware
  anti-rollback.
- No automatic key upload, directory service, or PKI.
- No signing of v1 ZIP backups.
- No change to approval, sandbox, path-jail, merge, capacity, or budget gates.
- No secret in SQLite, daemon events, logs, or the Swift UI.

## Consequences

- Operators who want authenticity must generate a keyring and pass
  `--signing-key`/`--sign` on backup; otherwise the repository stays explicitly
  unsigned v2.
- Losing the keyring makes signed repositories unverifiable and un-restorable
  until the keyring/public keys are recovered. This is intentional fail-closed
  behavior, documented in CLI errors.
- Prune and signed backup require signing key material for a v3 repository
  because metadata tombstones and the head must be re-signed.
- A stale head after a crash is visible in `backup-verify` and is repaired by
  the next signed backup.
- The anchor adds one 0600 file to the data directory and a small write on
  backup/verify/restore; it does not change repository contents.
- Tests cover sign/verify round-trip, tampered entry/metadata/tombstone/generation,
  wrong/missing key, rotation with old and new snapshots, rollback
  reject/allow, v2/v1 compatibility, mixed upgrade reporting, stale head,
  secret non-leakage, prune re-signing, atomic restore, CLI parsing/round-trip,
  and the daemon status surface. Existing backup tests stay green.
