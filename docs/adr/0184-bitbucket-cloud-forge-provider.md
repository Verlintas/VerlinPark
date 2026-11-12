# ADR-0184: Bitbucket Cloud forge provider and PR/MR abstraction

- **Status:** Accepted
- **Date:** 2026-11-12
- **Deciders:** VerlinPark runtime maintainers
- **Related:** ADR-0139 (session cancel/pause), ADR-0156 (shell write sandbox),
  ADR-0170 (tool error contract), ADR-0171 (LSP-first code intelligence),
  `docs/GAP_CLOSURE_PLAN.md` I3

## Context

`session.pr` could push a session branch and open a pull request through three
CLI binaries: GitHub `gh`, GitLab `glab`, and Gitea/Forgejo `tea`. The provider
choice was a `PrCli` enum embedded in `vp-core/src/lib.rs`, detection was a
substring check for `gitlab`, and creation happened only after an implicit
substring-based choice. Three gaps followed from that shape:

1. **No Bitbucket support.** Bitbucket Cloud has a documented REST API and does
   not require a user-installed CLI. Teams using `bitbucket.org` had no explicit
   provider or typed error path.
2. **No provider contract.** Preflight, push ordering, create arguments, URL
   extraction, and failure text were duplicated inside two long functions. A new
   provider could silently skip preflight or invent a URL.
3. **No safe status surface.** Settings could not say which provider would be
   used, whether its CLI was installed, or whether Bitbucket credentials were
   configured, without exposing credential values.

Bitbucket Server/Data Center is a different product with a different API
(`/rest/api/1.0/...`), project/repository ids, and auth model. It was already an
explicit non-goal in the gap-closure plan. This ADR keeps that boundary and does
not add a self-hosted Bitbucket adapter.

## Decision

### 1. One `ForgeProvider` contract

`crates/vp-core/src/forge.rs` owns a provider kind, origin model, preflight
result, change request/result, and an async trait:

```rust
#[async_trait]
pub trait ForgeProvider: Send + Sync {
    fn kind(&self) -> ForgeProviderKind;
    fn change_name(&self) -> &'static str; // PR or MR

    async fn preflight(&self, target: &ForgeTarget)
        -> Result<ForgePreflight, ForgeError>;

    async fn create(&self, target: &ForgeTarget, request: &ForgeChangeRequest)
        -> Result<ForgeChange, ForgeError>;
}
```

`ForgeProviderKind` is the single source of truth for all four providers:

| Provider | Kind id | Transport | Change | CLI |
| --- | --- | --- | --- | --- |
| GitHub | `github` | `cli` | PR | `gh` (`VERLINPARK_GH_BIN`) |
| GitLab | `gitlab` | `cli` | MR | `glab` (`VERLINPARK_GLAB_BIN`) |
| Gitea/Forgejo | `gitea` | `cli` | PR | `tea` (`VERLINPARK_TEA_BIN`) |
| Bitbucket Cloud | `bitbucket` | `rest` | PR | none |

`PrCli` remains as a crate-private alias so existing internal call sites and
tests keep compiling. Public protocol and CLI surfaces use stable lowercase ids.
Aliases accepted at the boundary are `gh`, `glab`, `tea`, `forgejo`,
`bitbucket-cloud`, and `bitbucket_cloud`; an unknown name is a typed
`FORGE_UNSUPPORTED_PROVIDER` error, never a silent GitHub fallback.

The engine does not expose the trait as a plugin system. Providers are closed
over the binary: adding a fifth provider requires a code change, an ADR update,
and the same gates.

### 2. Selection and origin detection

Selection priority is deterministic:

1. explicit `session.pr.provider` / `vp pr --provider`;
2. `VERLINPARK_PR_PROVIDER` in the daemon environment;
3. origin detection.

Origin detection preserves the legacy behavior where it matters:

- a `bitbucket` substring selects Bitbucket Cloud (and then the origin must
  parse as Cloud or the operation fails closed);
- a `gitlab` substring selects GitLab;
- everything else defaults to GitHub.

Gitea/Forgejo remains explicit because a self-hosted `git.example.com` origin
is indistinguishable from a generic Git server. Auto-detecting `tea` would be a
guess and could run the wrong CLI.

Bitbucket origin parsing accepts:

```text
https://bitbucket.org/<workspace>/<repo>[.git][/]
https://www.bitbucket.org/<workspace>/<repo>[.git]
https://<user>@bitbucket.org/<workspace>/<repo>.git
git@bitbucket.org:<workspace>/<repo>.git
ssh://git@bitbucket.org[:22]/<workspace>/<repo>.git
```

The parser lowercases the host, strips a trailing `.git`, trims one trailing
slash, rejects empty/`.`/`..`/percent-encoded path segments, and never echoes
URL userinfo. Exactly two path segments are required: Bitbucket Cloud has no
nested group path. Workspace and repository slugs are bounded to 100 characters
of `[A-Za-z0-9._-]`.

HTTP origins are rejected for Cloud because credentials and API responses must
not be sent over plaintext. Non-default HTTPS/SSH ports are rejected because they
signal a proxy or a self-hosted deployment. Bitbucket-looking hosts, `/scm/`
paths, `/projects/` paths, and port 7999 are reported as
`FORGE_SERVER_UNSUPPORTED`.

GitHub.com and GitLab origins are parsed for status/result metadata. GitLab
supports nested groups up to eight segments; GitHub is a single owner/repo pair.
Unknown hosts produce `FORGE_ORIGIN_UNSUPPORTED`, but an explicit CLI provider
can still use them because `gh`/`glab`/`tea` own their own host configuration.
Only Bitbucket requires the strict origin parse.

### 3. Bitbucket Cloud API

Creation uses:

```http
POST https://api.bitbucket.org/2.0/repositories/{workspace}/{repo_slug}/pullrequests
Accept: application/json
Content-Type: application/json
Authorization: Basic ... | Bearer ...
```

Request body:

```json
{
  "title": "Ship the change",
  "description": "Opened from VerlinPark session ...",
  "source": {"branch": {"name": "verlinpark/session-branch"}},
  "destination": {"branch": {"name": "main"}},
  "close_source_branch": false
}
```

Preflight uses a read-only request:

```http
GET https://api.bitbucket.org/2.0/repositories/{workspace}/{repo_slug}
```

It validates credentials and repository visibility before the branch is pushed.
It intentionally does **not** prove write permission: a token can have read
access and still receive `403` on creation. That limitation is documented rather
than simulated with a fake write probe. On an inaccessible repository or bad
credentials the engine stops before `git push`, so a failed preflight leaves no
remote branch behind.

`close_source_branch` is always `false`; VerlinPark never deletes remote branches
implicitly. There is no draft, reviewer, default-reviewer, or merge endpoint.

The response is treated as untrusted:

- `200` or `201` is required for creation;
- the PR URL comes from `links.html.href` only when it is HTTPS (in production)
  and shares the configured web origin;
- if the link is absent or invalid but the API returned a numeric `id`, the
  client builds the canonical
  `https://bitbucket.org/{workspace}/{repo}/pull-requests/{id}` URL;
- if neither a usable URL nor an id exists, the operation fails with
  `FORGE_RESPONSE_INVALID` and explicitly tells the operator to verify the
  provider. It never reports a fabricated success.

### 4. Authentication and secret handling

Bitbucket credentials are environment-only and mutually exclusive:

| Mode | Environment variables | Header |
| --- | --- | --- |
| Bearer | `BITBUCKET_TOKEN` | `Authorization: Bearer <token>` |
| Basic | `BITBUCKET_USERNAME` + `BITBUCKET_APP_PASSWORD` | `Authorization: Basic base64(user:app-password)` |

Rules:

- The token/password is trimmed only at the ends; empty values are treated as
  absent. Internal whitespace, control characters, and non-ASCII characters are
  rejected because they cannot form a valid header.
- Setting `BITBUCKET_TOKEN` together with either Basic variable is a typed
  `FORGE_AUTH_CONFLICT`; the client does not guess which one to use.
- A username containing `:` is rejected because it would make Basic encoding
  ambiguous.
- Secrets live in `SecretValue` wrappers that zero their allocation on drop and
  render as `[redacted]` in `Debug`.
- `BitbucketCredentials` has no `Serialize` implementation and is never stored
  in SQLite, events, manifests, bundles, logs, or status rows.
- Errors, evidence, status detail, and upstream provider messages pass through
  the shared Q3 redaction filter, which covers `Authorization`, bearer shapes,
  URL userinfo, GitHub/GitLab/Slack/AWS tokens, and Atlassian `ATATT…`/`ATBB…`
  token shapes.
- The daemon status surface reports only `configured: bool`, auth mode
  (`basic`/`bearer`), and the environment variable names. It never runs a
  provider request and never returns a value.

CLI providers (`gh`, `glab`, `tea`) remain responsible for their own credential
stores. VerlinPark runs only their documented auth-check command and never reads
or proxies their config files.

### 5. HTTP bounds and redirect policy

The Bitbucket client is deliberately small and bounded:

- 30-second default timeout, clamped to 1 ms–120 s in configuration;
- 1 MiB default response cap, enforced against `Content-Length` and while
  streaming the body (hard cap 8 MiB in the builder);
- same-origin redirects only, maximum three; the origin is the configured API
  base. A response that still carries a `3xx` after the policy refuses it maps to
  `FORGE_REDIRECT_REFUSED`;
- cookies disabled, HTTPS enforced in production, no invalid-certificate
  acceptance, a `verlinpark/<version>` user agent;
- no automatic retry of the `POST`. A timeout or `5xx` during creation may have
  reached Bitbucket, so the error says the request may have succeeded and asks
  the operator to verify before retrying. Preflight `5xx`/`429` and connection
  failures are marked retryable because they are read-only or never opened a
  connection;
- request validation rejects an empty/overlong/single-line title, an
  overlong description, and empty/whitespace/leading-`-` branch names before any
  network call.

### 6. Typed `FORGE_*` errors

All failures use the Q3 `ToolErrorCode` catalog, category `forge`, with a stable
code, conservative retryability, a default hint, and bounded safe evidence. The
initial set is:

| Code | Meaning | Retryable default |
| --- | --- | --- |
| `FORGE_UNSUPPORTED_PROVIDER` | unknown/empty provider name | no |
| `FORGE_ORIGIN_UNSUPPORTED` | not a supported HTTPS/SSH origin | no |
| `FORGE_SERVER_UNSUPPORTED` | Bitbucket Server/Data Center detected | no |
| `FORGE_AUTH_MISSING` | no usable credentials in the environment | no |
| `FORGE_AUTH_CONFLICT` | bearer and basic variables both set | no |
| `FORGE_AUTH_FAILED` | provider rejected credentials | no |
| `FORGE_PERMISSION_DENIED` | token cannot write to the repository | no |
| `FORGE_REPO_NOT_FOUND` | workspace/repository missing or invisible | no |
| `FORGE_CONFLICT` | PR already exists or repository state conflicts | no |
| `FORGE_RATE_LIMITED` | `429`, safe to retry after the window | yes |
| `FORGE_SERVICE_UNAVAILABLE` | `5xx` or provider outage | read-only only |
| `FORGE_TIMEOUT` | request timed out; creation may have succeeded | read-only only |
| `FORGE_NETWORK_FAILED` | DNS/TLS/connection failure | connect failures |
| `FORGE_RESPONSE_INVALID` | success status without a usable URL/id/JSON | no |
| `FORGE_RESPONSE_TOO_LARGE` | bounded response cap exceeded | no |
| `FORGE_REDIRECT_REFUSED` | cross-origin redirect with credentials | no |
| `FORGE_REQUEST_REJECTED` | other `4xx`/CLI rejection | no |
| `FORGE_REQUEST_INVALID` | local payload validation failed | no |
| `FORGE_CLI_UNAVAILABLE` | CLI missing or failed to start | no |
| `FORGE_CONFIG_INVALID` | invalid client base URL/timeout/bound | no |

HTTP mapping is `401 → AUTH_FAILED`, `403 → PERMISSION_DENIED`,
`404 → REPO_NOT_FOUND`, `409 → CONFLICT`, `429 → RATE_LIMITED`,
`3xx → REDIRECT_REFUSED`, `5xx → SERVICE_UNAVAILABLE`, other `4xx →
REQUEST_REJECTED`. `Retry-After`, the status, and a sanitized `x-request-id` may
appear as evidence; request headers and bodies never do.

The daemon maps `ForgeError` to JSON-RPC error data containing the code,
retryability, hint, provider, and evidence. The message starts with
`error[FORGE_*]` through the shared `ToolFailure` rendering, so the CLI prints an
actionable error even without parsing `data`.

### 7. Engine flow and invariants

`Engine::open_pull_request_with_provider` resolves the target from the project
origin, then runs one sequence:

1. reject an imported session;
2. require an idle session, a branch, a clean worktree, and an origin;
3. require at least one commit ahead of the project default branch;
4. build the provider (CLI path from env or explicit test path, or the REST
   client);
5. run preflight;
6. push the session branch to `origin` under the existing git mutex with
   `GIT_TERMINAL_PROMPT=0`;
7. append `session.pushed`;
8. create the PR/MR;
9. on creation failure, prefix the error with `branch pushed, but ...` without
   claiming a PR exists;
10. append `session.pr_opened` and return the result.

The existing approval, worktree, write-scope, Verifier, merge queue, capacity,
and budget gates are untouched. PR creation is still an explicit operator
action; no agent tool or scheduler calls it. `session.pr` is not an agent tool.

Events gain additive fields only:

```json
{
  "branch": "verlinpark/task",
  "base_branch": "main",
  "url": "https://bitbucket.org/acme/widgets/pull-requests/7",
  "provider": "bitbucket",
  "repository": "acme/widgets",
  "change": "PR",
  "change_id": 7
}
```

`PullRequestResult` gains `provider`, `repository`, and `change`, all
`serde(default)`/optional. Older clients decode the legacy shape; older daemons
simply omit the fields. Existing `gh`/`glab`/`tea` argument construction, cwd
behavior, and messages remain compatible; the preflight now runs before the
push for all providers, which is stricter and prevents a pushed branch when the
CLI is unauthenticated.

### 8. CLI and desktop surfaces

- `vp pr <session> [--provider github|gitlab|gitea|forgejo|bitbucket]` sends an
  optional `provider`; the daemon env and origin detection remain fallbacks.
- Human output is `<PR|MR> opened via <provider>: <url>  (<branch> → <base>)`.
  Errors render the `FORGE_*` code and hint; no token is printed.
- `vp forge status [--project <id|name>]` calls the read-only `forge.status`
  RPC and prints the selected provider, origin, each provider's transport,
  CLI availability, and credential state.
- `vp forge check [--project <id|name>] [--provider ...]` calls
  `forge.preflight`, which runs the same read-only auth/repository probe used
  before a push and returns the provider, repository, auth mode, and detail. It
  never pushes, creates a request, or writes an event.
- Settings → General has a Pull requests card that reads `forge.status`. It
  shows auto/explicit selection, detected origin, Bitbucket credential state,
  CLI availability, and the Server non-goal. It never displays a credential
  value. The explicit "Check credentials" button calls `forge.preflight`; status
  refresh itself makes no provider network call.
- The Stage PR confirmation text names all four providers and states that
  preflight happens before push. A successful PR link shows the provider and
  PR/MR terminology.

`forge.status` does not spawn `gh`, `glab`, or `tea` and does not call
Bitbucket. CLI authentication is reported as `unknown` until a real preflight.
Bitbucket credential presence is reported from environment variables only.
`forge.preflight` is the explicit probe; it is not called by status refresh,
the scheduler, or any agent tool.

### 9. Compatibility

- Protocol major version remains 1. New request fields are optional; new result
  fields are optional with `serde(default)`.
- `OpenPullRequestParams` without `provider` behaves exactly as before, except
  that `bitbucket.org` origins now route to Bitbucket Cloud instead of the
  previous GitHub default.
- `VERLINPARK_PR_PROVIDER` keeps its existing values and accepts the new
  `bitbucket`/`bitbucket-cloud` aliases.
- Existing PR tests for `gh`, `glab`, and `tea` remain green.
- `PrCli` is a private alias, not a public API.

### 10. Limits and explicit non-goals

- **Bitbucket Server/Data Center** is not implemented. Its API, project/repo
  identifiers, and auth model differ; only Cloud `bitbucket.org` is supported.
- **No OAuth/3LO flow.** Credentials must already exist in the daemon
  environment. VerlinPark does not open a browser or store refresh tokens.
- **No secret manager integration.** Keychain is used for model/provider
  secrets elsewhere; forge credentials are deliberately environment-only so
  they are never persisted by this runtime.
- **No reviewer, draft, label, or merge-request automation.** This module
  creates a request and returns its URL; review remains a human/provider action.
- **No write-permission probe.** Preflight proves read visibility only. A `403`
  on creation is typed and never retried blindly.
- **No exactly-once creation.** A timeout can occur after Bitbucket created the
  PR. The error reports uncertainty; operators must verify. The client does not
  retry POST automatically.
- **No cross-origin redirect trust.** Credentials are never forwarded to a
  different origin, even if the provider returns a redirect.
- **No provider-specific webhook/polling state.** VerlinPark does not track PR
  status after creation.
- **No remote/hosted workspace interaction.** PR creation runs from the local
  daemon and pushes the local session branch through its configured origin; the
  remote workspace integration path is separate and unchanged.
- **No arbitrary forge plugin loading.** The provider set is compiled in.

## Implementation map

| Concern | Location |
| --- | --- |
| Provider kind, origin parsing, selection | `crates/vp-core/src/forge.rs` |
| Bitbucket credentials and redaction | `crates/vp-core/src/forge.rs`, `crates/vp-core/src/tooling/errors.rs` |
| Bounded HTTP client, redirects, status mapping | `crates/vp-core/src/forge.rs` |
| CLI provider parity and preflight | `crates/vp-core/src/forge.rs` (`CliForgeProvider`) |
| Engine sequence, events, status | `crates/vp-core/src/lib.rs` |
| Wire types | `crates/vp-protocol/src/types.rs` |
| RPC dispatch and error data | `crates/vp-daemon/src/main.rs` |
| CLI `pr --provider`, `forge status`, `forge check` | `crates/vp-cli/src/main.rs` |
| Settings card and Stage copy | `apps/macos/VerlinPark/Features/SettingsView.swift`, `StudioView.swift`, `AppModel.swift`, `Core/Models.swift`, `Core/CoreClient.swift` |

## Test matrix

The I3 tests cover:

- HTTPS/SSH origin parsing, `.git`/trailing slash/userinfo normalization,
  uppercase hosts, extra path segments, HTTP refusal, non-default ports, and
  Server/Data Center detection;
- explicit/env/detected provider priority, aliases, unknown providers, and
  Bitbucket origin mismatch;
- Basic and Bearer headers, missing/partial/conflicting credentials, invalid
  secret characters, and `Debug`/error redaction;
- exact POST path, JSON body, `Accept`/`Authorization` headers, request count,
  and returned URL/id/state/title;
- canonical URL fallback, cross-origin HTML link refusal, and no fake success
  when both URL and id are missing;
- preflight path and request count, `401`/`404` mapping, and "no push/no event"
  on preflight failure;
- `400/401/403/404/409/422/429/500/503` mapping, retryability, `Retry-After`
  evidence, mutating `5xx` not retrying, and read-only `5xx` retryability;
- same-origin redirect following, cross-origin redirect refusal, response-size
  cap, timeout uncertainty, non-JSON error bodies, upstream message redaction,
  and invalid client configuration;
- full engine path: local fake Bitbucket server, pre-push preflight, branch push,
  `session.pushed`/`session.pr_opened` fields, and token absence from events;
- `forge.status` detected origin/credential mode and token absence from JSON;
- `forge.preflight` CLI auth success/failure without a push, Bitbucket read-only
  request with a fake server, and no branch/PR event;
- protocol legacy decode/defaults, CLI flag/output parsing, and Swift model
  decode for legacy, current, and preflight payloads;
- existing `gh`/`glab`/`tea` tests remain green.

## Credential lifecycle

Bitbucket Cloud app passwords are being replaced by scoped API tokens, so the
adapter treats the credential as an opaque environment value rather than
assuming a fixed length or prefix. The two supported modes are deliberately
simple:

- **Basic** (`BITBUCKET_USERNAME` + `BITBUCKET_APP_PASSWORD`) sends an HTTP Basic
  header. The username is the Atlassian account email or username; the password
  is an app password with repository read/write scope.
- **Bearer** (`BITBUCKET_TOKEN`) sends a workspace/repository access token. The
  token must be able to read the repository for preflight and create pull
  requests for creation.

Both values are read at operation time. VerlinPark does not cache them on disk
or in memory beyond the provider instance that is executing the current
operation. Restarting the daemon is not required to pick up a changed value if
the daemon process environment is updated by its supervisor; changing the
environment of a running process directly is outside the supported model.

Rotation sequence:

1. Create the replacement token/app password in Bitbucket with the same or
   narrower repository scope.
2. Export the new value in the daemon supervisor environment and remove the old
   value. Never export both Basic and Bearer modes.
3. Restart or signal the daemon so the new environment is visible to the
   process.
4. `vp forge check --project <project>` and verify the reported repository/auth
   mode.
5. Revoke the old token in Bitbucket only after the preflight succeeds.

If a token is believed exposed, revoke it first, then update the environment.
The error and event surfaces are designed to be safe to share, but a token that
was pasted into a shell history, an issue, or a third-party log is outside
VerlinPark's control. The daemon cannot revoke provider credentials.

Credential diagnostics intentionally stop at:

- whether the expected environment variables are present;
- which auth mode would be used;
- whether the provider accepted the credential in a preflight.

They do not report token prefixes, lengths, scopes, account identity, or a raw
provider response. A `401` means the provider rejected the credential; a `403`
means the credential was accepted but lacks repository write permission. Those
are different operator actions and stay different codes.

## Operational examples

Configure Bitbucket Cloud Basic authentication for the daemon process. The
variables are read at operation time and are never written by VerlinPark:

```bash
export BITBUCKET_USERNAME="automation@example.com"
export BITBUCKET_APP_PASSWORD="..." # app password with repository write scope
vp forge status --project acme
```

Or use a repository/workspace access token:

```bash
export BITBUCKET_TOKEN="..." # never pass this as a CLI argument
unset BITBUCKET_USERNAME BITBUCKET_APP_PASSWORD
vp forge check --project acme --provider bitbucket
```

`forge check` output is intentionally credential-free:

```json
{
  "provider": "bitbucket",
  "change_name": "PR",
  "repository": "acme/widgets",
  "auth": "bearer",
  "detail": "Bitbucket Cloud credentials accepted for acme/widgets (bearer auth)"
}
```

Open a pull request from an idle session:

```bash
vp pr 01J... --provider bitbucket
```

```text
PR opened via bitbucket: https://bitbucket.org/acme/widgets/pull-requests/7  (verlinpark/task → main)
```

A typed failure from the daemon looks like this; the CLI prints the same
`error[CODE]`, hint, and evidence:

```json
{
  "code": -32603,
  "message": "error[FORGE_PERMISSION_DENIED] the Bitbucket credentials cannot write to this repository during pull request creation (HTTP 403)\nretryable: no\nhint: Grant the token repository write access or use an account that can create pull requests.",
  "data": {
    "code": "FORGE_PERMISSION_DENIED",
    "retryable": false,
    "hint": "Grant the token repository write access or use an account that can create pull requests.",
    "provider": "bitbucket",
    "evidence": "status=403; request_id=..."
  }
}
```

Inspect the origin and CLI availability without side effects:

```bash
vp forge status
```

```text
forge  selected bitbucket  override auto
  origin bitbucket.org  provider bitbucket  repository acme/widgets  supported true
  bitbucket        PR   rest REST                     credentials configured
  github           PR   cli  gh                       auth checked at preflight
  gitlab           MR   cli  glab                     unavailable
  gitea            PR   cli  tea                      unavailable
```

The Settings → General Pull requests card mirrors this read-only inventory. Its
"Check credentials" button is the only GUI path that contacts the forge, and it
runs the same read-only preflight as `vp forge check`.

An operator-facing upgrade sequence for an existing GitHub/GitLab project is:

1. `vp forge status` to confirm detection.
2. `vp pr <session> --provider <provider>` once to verify preflight and creation.
3. only then set `VERLINPARK_PR_PROVIDER` for unattended/manual repeat use.

## Failure modes

| Situation | Behavior |
| --- | --- |
| No git origin | local error before selection; no provider call or push |
| Unknown provider name | `FORGE_UNSUPPORTED_PROVIDER`; no fallback provider |
| Bitbucket Server/Data Center origin | `FORGE_SERVER_UNSUPPORTED`; no API request |
| Bitbucket origin with wrong host/extra path | `FORGE_ORIGIN_UNSUPPORTED`; no API request |
| Token and Basic variables both set | `FORGE_AUTH_CONFLICT`; no API request or push |
| Missing token/password | `FORGE_AUTH_MISSING`; no API request or push |
| Bad credentials/expired token | `FORGE_AUTH_FAILED` from preflight; no push |
| Repository invisible to token | `FORGE_REPO_NOT_FOUND` from preflight; no push |
| Token cannot write | `FORGE_PERMISSION_DENIED` from create; branch is already pushed |
| Existing open PR | `FORGE_CONFLICT` from create; branch is already pushed; inspect the provider |
| Rate limited | `FORGE_RATE_LIMITED`, retryable, `Retry-After` evidence/hint; no automatic retry |
| 5xx during create | `FORGE_SERVICE_UNAVAILABLE`, non-retryable for create; may have succeeded |
| Timeout during create | `FORGE_TIMEOUT`, non-retryable for create; may have succeeded |
| Cross-origin redirect | `FORGE_REDIRECT_REFUSED`; credentials are not forwarded |
| Oversized/hostile response | `FORGE_RESPONSE_TOO_LARGE`; body is not parsed or logged |
| 201 without URL/id | `FORGE_RESPONSE_INVALID`; never reported as success |
| CLI missing | `FORGE_CLI_UNAVAILABLE`; no fallback to another provider |
| CLI auth check fails | `FORGE_AUTH_FAILED`; no push |
| CLI create fails after push | typed failure with `branch pushed, but ...`; no `session.pr_opened` |
| Daemon restarts mid-create | no exactly-once guarantee; verify the provider before retrying |

## Consequences

- Bitbucket Cloud users get a real provider without installing a CLI, with
  preflight-before-push and typed failures.
- All four providers now share one preflight/push/create contract. A future
  provider must implement preflight and cannot fabricate a URL.
- `FORGE_*` codes extend the Q3 catalog. Operators can branch on them in
  scripts; unknown failures still collapse conservatively.
- `forge.status` gives Settings and the CLI an honest, read-only provider
  inventory. CLI auth remains `unknown` until preflight, which avoids spawning
  arbitrary binaries from a status refresh.
- The Bitbucket credential path is intentionally narrow and environment-only.
  Operators must restart/relaunch the daemon with the variables present; the app
  cannot enter them into SQLite.

## Alternatives considered

- **Run `git push` first, then discover missing credentials:** rejected. It
  leaves a pushed branch and a confusing failure. Bitbucket preflight uses a
  read-only request and runs before push.
- **Use `curl`/a shell script instead of a Rust client:** rejected. It loses
  typed errors, bounded responses, same-origin redirect control, and secret
  redaction.
- **Add a `bitbucket` CLI dependency:** rejected. Bitbucket Cloud is API-first;
  requiring a tool that most users do not have would be worse than a bounded
  REST client.
- **Support Bitbucket Server with a host heuristic:** rejected. The APIs and
  auth models differ; a heuristic would claim support that is not implemented.
- **Retry `POST` on timeout or `5xx`:** rejected. Creation is not idempotent;
  a retry can create duplicate pull requests. The contract reports uncertainty.
- **Persist credentials for convenience:** rejected. Forge secrets must not
  enter SQLite, events, bundles, or logs; environment-only is the supported
  boundary.
- **Expose a provider plugin interface:** rejected for this round. A compiled-in
  closed set is easier to audit and keeps the approval/sandbox/merge invariants
  in one place.
