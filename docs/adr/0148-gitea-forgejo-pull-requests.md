# ADR-0148: Gitea/Forgejo pull requests through tea

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Delivery
- **Related:** ADR-0030

## Context

`session.pr` / `vp pr` explicitly pushes the session branch and opens a change
request through a provider CLI. Before this decision the adapter supported only
GitHub (`gh`, `PrCli::GitHub`) and GitLab (`glab`, `PrCli::GitLab`). Users who
host on Gitea or Forgejo had no supported path even though both expose the `tea`
CLI.

Self-hosted Git forges can live on arbitrary hostnames, so inferring Gitea or
Forgejo from an origin URL would be guessing. A wrong guess would run the wrong
CLI after a push, which is worse than an explicit selection. The existing
provider boundary (ADR-0030) already requires an explicit adapter and no hidden
provider detection.

## Decision

### Explicit provider selection

`VERLINPARK_PR_PROVIDER` selects the adapter:

| Value | Adapter | CLI |
|---|---|---|
| `github` | `PrCli::GitHub` | `gh` or `VERLINPARK_GH_BIN` |
| `gitlab` | `PrCli::GitLab` | `glab` or `VERLINPARK_GLAB_BIN` |
| `gitea` | `PrCli::Gitea` | `tea` or `VERLINPARK_TEA_BIN` |
| `forgejo` | `PrCli::Gitea` | `tea` or `VERLINPARK_TEA_BIN` |

The value is trimmed and lowercased. Unknown values keep the existing clear
`unsupported PR provider: <value>` error. When the variable is unset, origin
detection is unchanged: an origin containing `gitlab` selects GitLab, everything
else selects GitHub. VerlinPark does not auto-detect Gitea or Forgejo from an
arbitrary hostname.

`PrCli::Gitea` uses `tea` for both Gitea and Forgejo. Its reported change name is
`PR`, its binary name is `tea`, and its create label is `tea pr create`.

### Preflight and auth

The existing gates are unchanged: the session must not be running, the session
worktree must be clean, the branch must be ahead of the project default branch,
and an `origin` remote must exist. The adapter does not weaken clean-worktree,
push, or result semantics.

Before pushing, the adapter checks whether the CLI is authenticated. For Gitea
and Forgejo that check is:

```text
tea login list
```

A non-zero exit aborts before `git push` and before PR creation. The error names
`tea` and includes the CLI's stderr, so an unauthenticated or unconfigured `tea`
installation is not reported as a pushed branch or an opened PR.

### Create command

After the branch is pushed with the existing `git push -u origin <branch>` path,
the adapter runs:

```text
tea pr create --head <branch> --base <base> --title <title> --description <body>
```

`tea` runs in the session worktree, where the session branch is checked out. The
title and body are the same deterministic values used by the other adapters.
The created PR URL is read from the first stdout line beginning with `http`, and
the existing `session.pushed` / `session.pr_opened` events and
`PullRequestResult` shape are reused. No provider field is added to the protocol
result.

### No fake success

If `tea login list` fails, nothing is pushed. If the push fails, no PR command
is attempted. If `tea pr create` fails after a successful push, the error states
that the branch was pushed and includes the `tea` stderr. If `tea` succeeds but
prints no URL, the result is an error rather than a synthetic URL. A pushed
branch left behind by a failed create is surfaced honestly.

### Token handling

VerlinPark does not read, store, pass, or log Gitea/Forgejo access tokens. `tea`
owns its login configuration and credential storage. The daemon shells out to
the user's `tea` binary exactly as the existing adapter shells out to `gh` and
`glab`.

## Consequences

- Gitea and Forgejo users can complete the explicit push → PR flow from
  `vp pr` and the app without changing the RPC or result schema.
- GitHub and GitLab behavior, including their binary overrides and origin
  detection, is unchanged.
- Users must install and authenticate `tea` themselves. Local landing remains
  available without any provider CLI.
- Selecting `gitea` or `forgejo` is deliberate; the operator is responsible for
  the provider matching the `origin` remote.

## Limitations

- **No auto-detection from arbitrary self-hosted hostnames.** Gitea/Forgejo must
  be selected with `VERLINPARK_PR_PROVIDER`; unknown hostnames still default to
  GitHub when the variable is unset.
- **No Gitea/Forgejo API fallback.** VerlinPark does not call the forge REST API
  directly, manage tokens, or implement provider-specific retries.
- **No Bitbucket or other forge adapter.** Only the adapters listed above exist.
- **`tea` configuration is external.** VerlinPark does not choose a tea login or
  instance; `tea` resolves its own login from its configuration and the
  repository remote.
- **Push/create remain separate side effects.** A failed create can leave a
  pushed branch, which the error reports rather than rolling back.

## Alternatives considered

- **Guess Gitea from the origin hostname or URL shape:** rejected because
  self-hosted forges use arbitrary domains and a wrong guess would run the wrong
  CLI after pushing.
- **Call the Gitea/Forgejo HTTP API directly:** rejected because it requires
  token handling, instance discovery, and a second credential path inside the
  daemon.
- **Add a provider field to `PullRequestResult`:** rejected for this slice; the
  existing branch/base/url/detail result is sufficient and backward compatible.
- **Reuse `gh` or `glab` against Gitea:** rejected; `tea` is the supported CLI
  for both Gitea and Forgejo.
