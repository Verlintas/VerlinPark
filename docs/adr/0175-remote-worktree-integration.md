# ADR-0175: Remote worktree integration through bounded Git bundles

- **Status:** Accepted
- **Date:** 2026-11-11
- **Supersedes:** none
- **Related:** ADR-0156 (remote workspaces), ADR-0134 (merge queue and
  deterministic Integrator), ADR-0170 (tool error contract), ADR-0126
  (session/task worktrees)

## Context

Remote workspaces can create, inspect, read, and write Git worktrees on an
explicitly configured SSH host (ADR-0156). Their commits could not enter the
local Verifier, merge queue, Integrator, or conflict/retry path. The only way to
integrate remote work was an out-of-band push that bypassed every local gate.

The project requires one merge path: a commit is verified in an isolated local
checkout, then merged with `--no-ff` by the deterministic Integrator, with a
persisted queue row, rollback target, conflict handling, and retry. Adding a
second merge implementation for remote branches would create a weaker path.

## Decision

Add an explicit, policy-gated remote integration path that transports one branch
as a Git bundle and reuses the existing local Verifier and Integrator.

### 1. Bundle transport and bounds

The helper exposes an optional `git.bundle` capability with three methods:

- `git_bundle_create`: resolves the requested branch to an exact commit, creates
  a temporary `refs/verlinpark/export/<bundle_id>` ref, runs
  `git bundle create - <export_ref>` into a helper-owned file, records the exact
  head and SHA-256, and deletes the export ref immediately.
- `git_bundle_read`: returns one bounded base64 chunk at a byte offset with a
  per-chunk SHA-256, total byte count, and total SHA-256.
- `git_bundle_delete`: deletes the retained bundle and export ref.

The helper stores temporary bundles under `<canonical-root>/.git/verlinpark/
remote-bundles` for a normal checkout, or `<canonical-root>/.verlinpark/
remote-bundles` for a linked worktree/non-standard root. Every path is
canonicalized and required to remain inside the configured root. Directories are
created `0700`, files are created atomically, and a helper shutdown removes
retained bundles. At most four active exports exist; older exports expire after
30 minutes.

The byte cap is enforced **while Git writes** the bundle, not only after it
finishes: the helper kills the Git process group and deletes the partial file as
soon as the configured cap is exceeded. The daemon independently enforces the
same cap while downloading and rejects a chunk sequence that grows past it.

The daemon requests 1 MiB chunks by default. A base64 chunk stays well below the
16 MiB protocol frame limit. Bundle generation has a 300 second helper timeout
and a 300–600 second client call bound.

### 2. Integrity and head checks

The daemon verifies, in order:

1. The reported `bytes` and `sha256` are within the policy cap.
2. Every chunk's decoded length and SHA-256 match the helper frame.
3. The accumulated bytes match the reported total and total SHA-256.
4. `git bundle verify` accepts the file against the local repository.
5. `git bundle list-heads` advertises the exact recorded remote head.
6. The local session HEAD is not used as the imported target.

The import fetches into `refs/verlinpark/remote/<session>/<import_id>` with
`--no-tags --no-write-fetch-head --no-recurse-submodules` and a forced
source-to-private-ref mapping. The fetched ref is resolved again and must equal
the recorded head. A mismatch deletes the temporary ref and fails closed. The
session branch, worktree, index, and HEAD are not touched during import.

A malformed bundle, wrong head, SHA mismatch, or failed fetch is a typed Q3
failure (`REMOTE_BUNDLE_INVALID`, `REMOTE_BUNDLE_HEAD_MISMATCH`,
`REMOTE_BUNDLE_TAMPERED`, `REMOTE_BUNDLE_IMPORT_FAILED`) and never creates a
queue row.

### 3. Local Verifier semantics and environment caveat

The imported commit is verified by the same `verify_worker` path as a local
Worker commit:

- a fresh isolated `git clone --shared --no-checkout` of the project, checked out
  detached at the imported commit;
- the same Verifier role, tool policy, turn/token/time budget, capacity slot,
  and post-run tracked-file guard;
- the same `submit_verdict` contract and rejection handling.

The Verifier event and the queue detail record `source: remote`, host, remote
branch, bundle SHA-256, verdict, and this caveat:

> Local verification checks the imported commit in an isolated clone on this
> daemon host. It does not reproduce the remote host's OS, installed tools,
> environment variables, network, or uncommitted files.

`RemoteIntegrationPolicy.require_verifier` defaults to `true`. Setting it to
`false` skips the Verifier but still records `remote.integration.
verification_skipped`, still enqueues through the same Integrator, and still
shows the caveat. This is an explicit reduction of evidence, not a claim that
the remote environment was tested.

### 4. Merge queue and Integrator

The imported commit is enqueued with additive `MergeQueueEntry` fields:

| field | local row | remote row |
| --- | --- | --- |
| `source` | `local` (default) | `remote` |
| `remote_host` | null | configured SSH host |
| `remote_branch` | null | exported branch |
| `bundle_sha256` | null | verified bundle hash |
| `imported_ref` | null | `refs/verlinpark/remote/...` |

Existing rows decode unchanged because `source` defaults to `local` and the
remote fields are optional. `merge_queue.enqueued` embeds the complete entry, so
replay/repair and bundle import reconstruct the new fields without a second
event.

The deterministic Integrator continues to merge the exact commit hash with
`--no-ff`, run `git diff --check`, optionally run the project integration
command under the write sandbox and environment allowlist, roll back on failure,
and abort on conflicts. No remote-specific merge implementation exists.

A local worktree at the imported ref is created for conflict resolution and
retry. It is a normal `verlinpark/<session>--remote-<import_id>` worktree. The
queue row still points at the imported ref and exact commit.

### 5. Policy and approval

`RemoteIntegrationPolicy` is project-scoped and persisted in SQLite plus the
project manifest; `db-rebuild` restores it:

```text
enabled            default false
max_bundle_bytes   default 256 MiB, bounds 1 KiB..2 GiB
require_verifier   default true
allowed_hosts      exact host, user@host, or full host label; no wildcard
keep_import_refs   default false
```

The policy is never enabled by default. Enabling requires:

- a valid policy with at least one allowed host;
- the project execution host to be `ssh`;
- an exact allowlist match (case-insensitive) for the configured host;
- explicit operator confirmation (`remote.integration.policy.set` `confirm`).

`remote.integrate` separately requires explicit operator confirmation and emits
`remote.integration.approved` with `risk: host_state` before any helper call. The
RPC is not exposed as an agent tool, so a model cannot trigger a remote import.
Remote writes remain explicit `remote_*` tool calls governed by the existing
approval policy; remote integration only reads a bundle from the trusted host.

No automatic cross-host scheduling, background polling, or retry is performed.
There is no remote daemon, no remote test execution, and no distributed lease.

### 6. Temporary refs, cleanup, conflict, and retry

Temporary refs are removed after a successful merge unless
`keep_import_refs` is true. They are deliberately retained while a queue entry
is `conflict`, `failed`, or `interrupted`, because retry must operate on the
exact imported branch state.

Retry rebases the local remote worktree onto the current session branch, updates
the imported ref to the rebased head, re-runs the Verifier, updates the queue
row's commit/base/attempt, and re-enters the same deterministic Integrator.
Conflict resolution uses the same resolver path as a local Worker; after the
resolver commit is verified, the imported ref is updated again. When the retried
entry merges, the imported ref is pruned unless the policy retains refs.

Cleanup failure after a successful merge is non-fatal for the merge status: the
entry remains `merged`, the result reports `cleanup_error`, `refs_pruned: false`,
and `remote.integration.cleanup_failed` is emitted. It never reports a merge
failure for a merged commit.

### 7. Protocol, CLI, and UI

Protocol additions are backward compatible within major version 1:

- RPC: `remote.integrate`, `remote.integration.status`,
  `remote.integration.cleanup`, `remote.integration.policy.get`,
  `remote.integration.policy.set`.
- CLI: `vp remote-integrate`, `vp remote-integration`,
  `vp remote-integration-cleanup`, `vp remote-integration-policy`,
  `vp remote-integration-policy-set`.
- Manage Dash → Git: a read-only remote integration card with a queue badge,
  policy/cap/verifier state, retained refs, entries, and the environment caveat.

Errors use the Q3 contract: RPC error `data` contains
`{code, message, retryable, hint, evidence?, tool}`. Codes added for this
feature include `REMOTE_INTEGRATION_DISABLED`,
`REMOTE_INTEGRATION_HOST_NOT_ALLOWED`,
`REMOTE_INTEGRATION_APPROVAL_REQUIRED`, `REMOTE_BUNDLE_TOO_LARGE`,
`REMOTE_BUNDLE_INVALID`, `REMOTE_BUNDLE_TAMPERED`,
`REMOTE_BUNDLE_HEAD_MISMATCH`, `REMOTE_BUNDLE_IMPORT_FAILED`,
`REMOTE_INTEGRATION_VERIFIER_REQUIRED`,
`REMOTE_INTEGRATION_VERIFIER_REJECTED`, and `REMOTE_INTEGRATION_FAILED`.

### 8. Tests

- Helper bundle unit tests: create/read/delete, chunk math, byte cap, partial
  cleanup, unsafe branch names, expiry, unknown ids, path-safe ids.
- In-process server/client contract: real `serve` over a duplex, chunked frames,
  capability negotiation, delete, unknown id, cap.
- Packaged helper over stdio: real handshake, multi-frame bundle round-trip,
  SHA-256, cap.
- Local import: malformed/empty bundle, SHA mismatch, head mismatch, private ref
  namespace, worktree creation, ref update, cleanup.
- Agent integration: policy disabled, missing confirmation, host not allowed,
  required and optional Verifier, merged queue row and events, tampered bundle,
  head mismatch, conflict abort with retained ref, manual resolution, re-verify,
  retry, and final cleanup.
- Store: additive queue fields, local filtering, event payload, retry preserves
  provenance, policy persistence and legacy manifest decoding.
- Protocol/CLI/Swift decode for legacy queue rows and new status payloads.

### 9. Failure matrix

| condition | detection | result | queue row? | ref/worktree |
| --- | --- | --- | --- | --- |
| policy disabled | daemon before connecting | `REMOTE_INTEGRATION_DISABLED` | no | none |
| host missing from allowlist | daemon before connecting | `REMOTE_INTEGRATION_HOST_NOT_ALLOWED` | no | none |
| no operator confirmation | daemon before connecting | `REMOTE_INTEGRATION_APPROVAL_REQUIRED` | no | none |
| helper lacks `git.bundle` | capability probe | `REMOTE_PROTOCOL_ERROR` | no | none |
| export over cap | helper stream + daemon total | `REMOTE_BUNDLE_TOO_LARGE` | no | partial helper file deleted |
| chunk hash mismatch | per-chunk SHA-256 | `REMOTE_BUNDLE_TAMPERED` | no | local file deleted |
| total SHA mismatch | accumulated SHA-256 | `REMOTE_BUNDLE_TAMPERED` | no | local file deleted |
| Git rejects bundle | `git bundle verify` | `REMOTE_BUNDLE_INVALID` | no | local file deleted |
| bundle head differs | `list-heads` / fetched ref | `REMOTE_BUNDLE_HEAD_MISMATCH` | no | temp ref deleted |
| fetch into private ref fails | local Git | `REMOTE_BUNDLE_IMPORT_FAILED` | no | temp ref best-effort deleted |
| Verifier rejects | `agent.finished` / verdict | `REMOTE_INTEGRATION_VERIFIER_REJECTED` | no | worktree/ref removed unless kept |
| Verifier cancelled/budget/error | run outcome | `REMOTE_INTEGRATION_FAILED` | no | worktree/ref removed unless kept |
| merge conflict | `git merge --no-ff` | `REMOTE_INTEGRATION_FAILED` | yes, `conflict` | ref retained for retry |
| integration check fails | diff check / command | `REMOTE_INTEGRATION_FAILED` | yes, `failed` | ref retained for retry |
| merge succeeds, ref cleanup fails | post-merge prune | success, `cleanup_error` | yes, `merged` | ref remains, event emitted |

No row in this table creates a "merged" entry without the Integrator's
`git merge --no-ff` and post-merge check succeeding.

### 10. Operations

A typical explicit flow is:

```bash
vp remote-integration-policy-set prj-1 \
  --enable --allow-host agent@build.example.com \
  --max-bundle-bytes 268435456 --confirm

vp remote-integrate prj-1 ses-1 feature/remote \
  --expected-commit <sha> --confirm

vp remote-integration prj-1 --session ses-1
```

When a remote entry conflicts, resolve the local remote worktree as usual and
retry with `vp retry <entry-id>` (or the Manage Dash action). The retry rebases
the imported ref, re-verifies, and re-enters the Integrator. If the branch is
abandoned instead, prune the retained ref explicitly:

```bash
vp remote-integration-cleanup prj-1 --entry <entry-id> --force
```

Without `--force`, entries in `conflict`, `failed`, or `interrupted` are
protected; `queued`, `merging`, and `checking` entries are never touched.
Cleanup emits `remote.integration.refs_pruned` per entry and is idempotent when
the ref is already absent.

The Manage Dash → Git card is read-only for integration itself. It shows the
policy, cap, verifier requirement, queue badge, per-entry remote provenance and
verifier verdict, retained-ref count, and the environment caveat. It can prune
merged refs but cannot force-delete a retryable ref; that remains an explicit
CLI/operator decision.

### 11. Implementation map

| surface | file |
| --- | --- |
| helper bundle registry and bounded Git export | `crates/vp-remote/src/bundle.rs` |
| helper dispatch, capabilities, shutdown cleanup | `crates/vp-remote/src/server.rs` |
| client framing, `connect_stream`, bundle calls | `crates/vp-remote/src/client.rs` |
| wire DTOs and capability name | `crates/vp-remote/src/protocol.rs` |
| project execution-host handle | `crates/vp-core/src/remote.rs` |
| local import, ref namespace, worktree, failure type | `crates/vp-core/src/remote_integration.rs` |
| Verifier/dispatch/Integrator orchestration | `crates/vp-core/src/agent.rs` |
| policy and merge-queue schema | `crates/vp-store/src/store.rs` |
| manifest rebuild | `crates/vp-core/src/rebuild.rs` |
| RPC handlers | `crates/vp-daemon/src/main.rs` |
| CLI commands and human output | `crates/vp-cli/src/main.rs` |
| Manage Dash card and models | `apps/macos/VerlinPark/Features/RemoteIntegrationCard.swift`, `Core/Models.swift` |
| ADR | this file |

The same commit contains unit tests for each layer plus stdio/stream contract
tests. A release reviewer can trace one remote integration from the CLI RPC
through the helper frames, local import, Verifier event, queue row, Integrator
result, and cleanup event without consulting code outside this map.

## Limits and non-goals

- The remote host is trusted infrastructure. The helper is not sandboxed on that
  host, and this ADR does not claim network or credential isolation.
- Local verification does not reproduce the remote environment. Remote test
  execution is not a substitute for local Verifier evidence and is not
  implemented here.
- There is no distributed scheduler, cross-host lease/fencing, remote daemon,
  or automatic cross-host dispatch.
- There is no automatic remote retry, background fetch, or remote push. Retry
  rebases the already-imported local ref; it does not re-read the remote branch.
- `keep_import_refs` retains objects/refs for operator inspection but does not
  make them eligible for automatic merge.
- Bundle export is a snapshot of one branch at one commit. Uncommitted remote
  work is not exported.
- The policy permits only explicitly allowlisted hosts. A wildcard, CIDR, or
  substring match is rejected.
- This is not a general artifact transfer mechanism. Bundles are temporary,
  bounded, and deleted after import on both sides.
