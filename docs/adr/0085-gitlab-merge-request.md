# ADR-0085: GitLab merge-request adapter

- **Status:** Accepted
- **Date:** 2026-11-04

## Context

`session.pr` pushed the session branch and opened a GitHub PR with `gh`. Users on
GitLab had to leave the app and create the MR manually.

## Decision

1. **Provider detection.** `open_pull_request` reads `origin`; hosts containing
   `gitlab` use `glab`, everything else keeps the GitHub default. Self-hosted
   hosts without `gitlab` in the name can set `VERLINPARK_PR_PROVIDER=gitlab`
   (or `github`).
2. **One execution path.** GitHub and GitLab share clean-worktree checks, branch
   ahead check, authenticated CLI preflight, `git push -u origin`, URL capture,
   and `session.pushed` / `session.pr_opened` events.
3. **CLI arguments.** GitHub uses `gh pr create --base/--head/--title/--body`.
   GitLab uses `glab mr create --source-branch/--target-branch/--title/
   --description/--yes`.
4. **Binary overrides.** `VERLINPARK_GH_BIN` and `VERLINPARK_GLAB_BIN` support
   tests and non-standard installations.
5. **No fake success.** Missing origin, unauthenticated CLI, failed push, and
   missing URL all fail with an error; branch state is not rolled back because the
   push may already have succeeded.
6. **No generic provider API.** Bitbucket/Gitea/other adapters are not implemented
   and require their own preflight/CLI semantics.

## Consequences

- `vp pr` and Preview's “Open PR / MR” work on GitHub and GitLab.
- GitLab self-hosted users may need the provider override.
- `glab` must be installed and authenticated; the daemon does not store CLI tokens.
- The result detail says “PR” or “MR” depending on the provider.

## Alternatives considered

- **Provider-specific RPC methods:** duplicates the push/clean/event flow.
- **Raw GitLab REST calls:** requires token storage, host discovery, and a new
  secret path; the CLI already owns auth.
- **Silent fallback to GitHub:** would fail confusingly on GitLab remotes.
