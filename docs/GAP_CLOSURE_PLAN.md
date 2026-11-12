# Gap Closure Plan

> **Status:** implemented and verified (2026-11-12).
> **Created:** 2026-11-12
> **Base:** `659b001` (long-run operations round H1–H5 complete).
> **Goal:** close every remaining gap that can be implemented and verified
> locally, and explicitly classify the rest as design non-goals or
> external-dependency limits instead of leaving them vague.
> **Budget:** one large round, target +42,000 net lines of production code and
> tests. No filler; every module must operate, fail closed, and be auditable.

---

## 0. Complete gap inventory

### Implemented this round

- [x] Source semantic retrieval is exact-scan only; no ANN, rerank, or query
      expansion.
- [x] Artifacts have no safe archive listing/extraction; media metadata and
      transcoding are absent.
- [x] PR/MR providers cover GitHub/GitLab/Gitea/Forgejo but not Bitbucket.
- [x] Signed backups have no key revocation, re-sign/rotation migration, or
      portable anti-rollback anchor.
- [x] Processes do not survive host reboot; there is no user LaunchAgent
      lifecycle. GUI action events share session retention; test-cache keys
      ignore operator-declared external inputs; parallel read-only batches do
      not honor per-tool deadlines.
- [x] Remote helpers must be installed manually; remote project checks are not
      captured as supplemental evidence.

### Explicit non-goals (documented, not claimed as shipped)

- [ ] Cross-host scheduler / lease consensus / distributed merge or program
      fencing. Requires a shared coordination service or consensus protocol;
      the daemon keeps same-host SQLite fenced leases and a bounded GUI FIFO.
- [ ] Full shell read/network confinement. The Seatbelt write sandbox, opt-in
      HOME/user-volume read jails, and per-project network policy remain; a
      default-deny read jail would break toolchains and is not a safe default.
- [ ] Default-on conflict auto-resolution or a hidden LLM Integrator. Conflict
      resolution stays explicit or project-opt-in; the deterministic Integrator
      remains the merge authority.
- [ ] Sparkle auto-updater / automatic tap upgrades. Distribution remains the
      ad-hoc DMG plus manual `update.check` / verified `update.download`;
      Sparkle requires Developer ID signing/notarization and a hosted appcast.
- [ ] Native GUI helper sandboxing. The computer helper requires TCC/AX/CGEvent
      authority and remains unsandboxed by design; browser helper writes stay
      Seatbelt-confined.
- [ ] Distributed semantic memory / cross-host source or test-cache sharing.
      No shared vector/cache service exists.
- [ ] Automatic Manager creation or plan rewrite. Managers remain plan-owned;
      only Worker/Labour slots scale deterministically.
- [ ] Provider-call exactly-once or mid-stream resume. Providers do not offer
      this; partial turns keep honest usage and are never replayed.
- [ ] Cross-host approval queue, distributed waiter fencing, or cross-host
      cancellation. These follow the same missing coordination service.
- [ ] Key escrow, KMS/HSM, remote attestation, or a transparency log. v3
      signing, revocation, rotation, and a local anchor are the supported
      boundary.
- [ ] Bitbucket Server/Data Center. Only Bitbucket Cloud's REST API is added;
      self-hosted Bitbucket requires a different API and auth model.

This round implements I1–I6. The non-goals above stay explicit in AGENTS.md,
README.md, PROGRESS.md, and AUDIT.md.

---

## I1 — Source retrieval v2: ANN, rerank, query expansion

### Design

- Add `SourceRetrievalPolicy` (project):
  - `ann_enabled` (default false), `ann_min_vectors` (default 256),
    `ann_nprobe` (1–32), `rerank` (`off|deterministic`, default deterministic),
    `query_expansion` (`off|lexical`, default off), `max_expanded_terms` (1–16).
- ANN:
  - Build a deterministic source IVF index from the current-generation vectors,
    scoped by project/model/dimensions/generation; seeded k-means++ centroids,
    bounded training sample, incremental assignment, persisted centroids and
    assignments under a derived-state table.
  - Search probes `nprobe` clusters and feeds cosine candidates into the
    existing RRF; exact scan remains the fallback for disabled/stale/corrupt/
    too-small indexes and must be explicitly reported.
  - Rebuild is idempotent, generation-fenced, and bounded; no vectors exposed.
- Rerank (deterministic):
  - Blend cosine, lexical overlap, title match, and freshness with fixed
    weights and deterministic tie-breaks; every hit carries rerank scores.
  - No hidden model call.
- Query expansion (lexical):
  - When enabled, run the lexical pass first, extract bounded terms from the
    top hits/titles, and issue a second lexical/semantic pass; deduplicate and
    label expanded hits with the expansion terms. No LLM.
- Diagnostics: `source.embedding.index_status` reports generation, vector count,
  centroid count, nprobe, exact/ANN path, fallback reason, and rebuild time.
- Tests: IVF recall fixtures vs exact, nprobe bounds, generation invalidation,
  corrupt/stale fallback, deterministic rerank, expansion bounds/labels,
  no-vector leakage, protocol/CLI/Swift decode.

### Files

`vp-core/src/source_embedding.rs`, new `source_ann.rs`, store, protocol, daemon,
CLI, Swift Sources, ADR-0182.

---

## I2 — Safe artifact archives and media

### Design

- Archive support (zip, tar, tar.gz; no arbitrary decompressors):
  - `artifact.archive.list` reads a bounded local artifact and returns entries
    (path, size, compressed size, kind, safe flag) with hard caps (entries,
    per-entry bytes, total uncompressed bytes, ratio).
  - `artifact.archive.extract` extracts selected entries into a daemon-owned
    scratch directory with canonical path join, `..`/absolute/symlink/hardlink/
    device rejection, duplicate-entry normalization, and bomb-ratio rejection.
    Extracted files become local artifacts (content-addressed) or bounded
    previews; they never write into the workspace.
  - Password-protected/encrypted archives return typed `ARCHIVE_ENCRYPTED`.
- Media:
  - `media_info` reads bounded metadata via `ffprobe` when installed (JSON
    parsed and capped) and falls back to image metadata for still images;
    missing `ffprobe` returns `MEDIA_TOOL_UNAVAILABLE`, not fake data.
  - `media_transcode` is explicit, approval-classified, only writes to daemon
    scratch/artifact output, bounds duration/size, and requires `ffmpeg`.
    No claim of transcoding when ffmpeg is absent.
- Preview/Swift: archive browser sheet with entry selection and safe extraction;
  media card with metadata and an explicit transcode action.
- Tests: zip-slip/tar traversal, symlink/hardlink/device rejection, duplicate
  entries, bomb caps, encrypted archive, ffprobe fixture parsing, missing-tool
  typed error, transcode bounds/approval, artifact provenance.

### Files

`vp-core/src/tooling/media.rs` (new archive/media), artifact store, protocol,
daemon, CLI, Swift Preview, ADR-0183.

---

## I3 — Bitbucket Cloud and forge abstraction

### Design

- Introduce a `ForgeProvider` abstraction for PR/MR creation with:
  - origin detection + explicit override,
  - preflight/auth check,
  - create request and returned URL,
  - no fake success; typed `FORGE_*` errors.
- Providers: GitHub (`gh`), GitLab (`glab`), Gitea/Forgejo (`tea`), and
  **Bitbucket Cloud REST**:
  - API `POST /2.0/repositories/{workspace}/{repo_slug}/pullrequests`.
  - Auth from `BITBUCKET_USERNAME` + `BITBUCKET_APP_PASSWORD`, or
    `BITBUCKET_TOKEN` (Bearer); never persisted to SQLite/events/logs.
  - Origin parsing for `bitbucket.org` HTTPS/SSH; Cloud only.
  - Bounded request/response, timeout, same-origin redirect policy, error
    mapping (401/403/404/409/429).
- CLI `vp pr --provider bitbucket` and env detection; preflight reports missing
  credentials before pushing (or explicitly pushes only after preflight).
- Tests: origin parsing, request body/auth headers, success/error mapping,
  no-token non-leakage, Cloud-only refusal, wiremock fake server, existing
  gh/glab/tea tests green.

### Files

`vp-core/src/lib.rs` PR flow (or new `forge.rs`), `vp-cli`, protocol, Swift
Settings status, ADR-0184.

---

## I4 — Backup key revocation, rekey, and portable anchor

### Design

- Keyring hardening:
  - add `revoked_at` (additive) and `backup-key-revoke`; revoked keys stay for
    historical verification but verify/restore reject a snapshot signed by a
    revoked key unless explicit `--allow-revoked`, recording a warning/event.
  - keyring write remains 0600/atomic; public summaries expose revoked state.
- `backup-rekey`:
  - rewrite v3 signed manifests/repository metadata with the active key without
    re-encrypting blobs; preserve generation/chain; atomic and resumable;
    refuse v2/v1 or missing private key; bounded batch.
- Portable anchor:
  - `backup-anchor-export` writes a signed anchor document (repository ids,
    highest generations, key id, timestamp); `backup-anchor-import` verifies the
    signature and only raises, never lowers, the local high-water marks.
  - Explicitly not remote attestation/escrow; a fresh host can import the
    anchor to reject old repositories.
- Tests: revocation reject/allow, rekey preserves ciphertext and updates
  signatures, old/new key verification, anchor export/import tamper/rollback,
  secret non-leakage, v2 compatibility.

### Files

`backup/signing.rs`, `backup/anchor.rs`, `backup/repository/*`, CLI, protocol/
daemon status, Swift Settings, ADR-0185.

---

## I5 — Boot persistence, retention, and preemption polish

### Design

- User LaunchAgent lifecycle (explicit, no sudo):
  - `vp daemon-agent install|uninstall|status` writes `~/Library/LaunchAgents/
    com.verlinpark.daemon.plist` (0600/0644 as appropriate), `RunAtLoad`,
    `KeepAlive`, log paths under the data dir, and the current binary path.
  - `install` requires confirmation and refuses if the daemon is already running
    under another supervisor; `status` reports loaded/disabled/unknown.
  - No automatic installation; no root daemon.
- Supervised process boot restart:
  - additive `ProcessPolicy.restart_on_boot` (default false), max attempts,
    backoff;
  - on daemon boot, rows that were `running` and opted in are respawned with a
    new nonce and a bounded attempt counter, emitting `process.restarted`;
    exhausted rows become `blocked` with a typed reason. No silent restart when
    the policy is off.
- GUI timeline retention:
  - project `GuiPolicy.timeline_retention_days` (default 30, 0 keeps forever);
    daemon sweep prunes only `gui.action` events older than the cutoff, bounded,
    audited, never touches other events.
- Per-tool deadlines in parallel read-only batches:
  - each tool observes its own deadline token in addition to the run token;
    the first deadline aborts that tool, not the batch/run.
- Test-cache external scopes:
  - `extra_env` (explicit names) and `extra_paths` (worktree-relative files)
    participate in the key; unreadable/oversized inputs fail closed.
- Tests: plist content/install/uninstall dry-run, restart policy/backoff/blocked,
  retention cutoff/audit, per-tool deadline isolation, extra env/path key
  invalidation, no secret leakage.

### Files

`vp-core/src/process_supervisor.rs`, `gui.rs`, `test_cache.rs`, `tooling/*`,
daemon startup/sweep, CLI, Swift, ADR-0186.

---

## I6 — Remote helper bootstrap and remote test evidence

### Design

- `remote_bootstrap`:
  - explicit, approval-classified action that uploads the local
    `vp-remote-agent` binary to a configured SSH host over the existing remote
    transport (chunked, atomic temp + rename), verifies SHA-256 and protocol
    version, installs to a user-owned path (0600/0700), and never uses sudo.
  - refuses remote `network=deny`, mismatched architecture, insufficient disk,
    or overwrite without `force`; typed errors.
- `remote_test`:
  - runs a bounded project check command (`check`/`lint`/`test` profile) on the
    remote host via `remote_exec`, captures exit/output refs, and records
    `remote_test.completed` evidence with host/commit/command/duration.
  - The local Verifier and deterministic Integrator remain mandatory; remote
    evidence is supplemental and explicitly caveated, never a merge bypass.
- Remote integration queue rows can attach the supplemental evidence digest.
- Tests: fake helper over stdio, bootstrap checksum/version/architecture,
  no-sudo/force policy, remote test provenance/bounds, merge still requires
  local Verifier, no network sandbox claim.

### Files

`vp-remote`, `vp-core/src/remote.rs`, store, protocol, daemon, CLI, Swift
Manage Dash, ADR-0187.

---

## 5. Integration and acceptance

Integration order: **I4 → I3 → I2 → I1 → I6 → I5**. I5 lands last because it
touches daemon boot/sweeps and several shared policies.

- +42,000 net lines of production code/tests/docs from `659b001` (hard minimum
  30,000).
- No filler, no dead code, no untested happy path.
- `cargo fmt --check`, `cargo clippy -D warnings`, `cargo test --workspace`,
  `just lint`, `xcodebuild test`, `just bundle`.
- Approval, Seatbelt, path jail, write scope, Verifier, merge, capacity, and
  budget gates unchanged or stricter.
- Archive extraction never writes outside daemon scratch/artifacts; media tools
  fail closed when ffmpeg/ffprobe are absent.
- Bitbucket credentials never touch SQLite/events/logs; no fake PR success.
- Revoked backup keys fail closed unless explicitly allowed; anchor import can
  only raise generations.
- LaunchAgent installation is explicit/user-level; process restart_on_boot is
  opt-in and bounded; GUI retention only prunes `gui.action`.
- Remote bootstrap never uses sudo, verifies checksum/version, and remote test
  evidence never bypasses the local Verifier.

## 6. Non-goals

See section 0. Distributed consensus/scheduler, default-deny read/network
confinement, Sparkle, native GUI sandboxing, distributed semantic memory,
automatic Manager creation, provider exactly-once/mid-stream resume, key
escrow/KMS/transparency, Bitbucket Server, and cross-host approval/cache/source
sharing remain explicit non-goals, documented in the current-state docs.

---

## 7. Implementation record

Implemented on `main` from base `9141680` in six commits:

```text
389edc5 feat(backup): key revocation, rekey, and portable anchor (I4)
ef22413 feat(forge): Bitbucket Cloud provider and PR/MR abstraction (I3)
549ce0f feat(artifacts): safe archive extraction and explicit media tools (I2)
427ae2e feat(retrieval): source ANN, deterministic rerank, and lexical expansion (I1)
9bc0e8b feat(remote): helper bootstrap and remote test evidence (I6)
5a6054a feat(runtime): boot persistence, retention, and per-tool deadline polish (I5)
```

- Diff: **+45,288 / -1,926 lines across 80 files** (target +42,000).
- Rust: `cargo test --workspace` **2,938 passed / 0 failed / 6 ignored**;
  `cargo fmt --check` and `cargo clippy -D warnings` pass.
- Swift: `xcodebuild test` **341 app + 132 helper = 473 passed / 0 failed**;
  `swiftformat --lint` passes (0/108).
- `just bundle` builds the Debug app and bundles `verlinpadd`.
- ADR-0182–0187 record the six decisions.

Merge-resolution notes:

- The CLI's generated clap parser and helper-test parse stacks outgrew 8 MiB;
  all explicit test parse stacks and `RUST_MIN_STACK` were raised to 32 MiB.
  The production `vp` binary already parses/dispatches on a 32 MiB thread.
- CLI/daemon/protocol/store test tails were reconstructed from pre-merge
  versions so both sides' tests and typed-error helpers were retained.
- The test-cache wall-clock prune assertion keeps the deterministic far-future
  cutoff rather than a time-of-day-dependent clock.
- The errors catalog cap is 30,000 characters to cover the grown code list.

Explicit limits are recorded in AGENTS.md, README.md, PROGRESS.md, and AUDIT.md:
ANN is local derived state with fixture-measured recall; archive/media tools are
bounded but ffprobe/ffmpeg are trusted local binaries; Bitbucket is Cloud-only
and not exactly-once; revocation/anchor are local trust state; LaunchAgent
behavior is fixture-tested rather than installed; remote test evidence never
bypasses the local Verifier; and the section-0 non-goals remain unimplemented.
