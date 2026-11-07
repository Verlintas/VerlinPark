# ADR-0107: Manual release update check

- **Status:** Accepted
- **Date:** 2026-11-08

## Context

The ad-hoc DMG is the supported distribution path and Developer ID
signing/notarization is deferred. Users still need a safe way to see whether a
newer release exists without adding an auto-updater that could install an
unsigned or unexpected binary.

## Decision

1. **Manual check only.** `update.check` fetches the latest GitHub release for
   the configured repository. It does not download, replace, or launch an
   installer.
2. **Repository control.** The default is `Verlintas/VerlinPark`; the daemon
   accepts a validated `owner/repo` slug through the request or
   `VERLINPARK_UPDATE_REPO`. Full URLs and extra path segments are rejected.
3. **Version comparison.** Tags are stripped of a leading `v` and compared by
   numeric components; prerelease suffixes do not make a release newer. The
   current version comes from `APP_VERSION`.
4. **Result.** `UpdateCheckResult` contains current/latest versions, whether an
   update exists, repository, release name, release URL, and publish time.
   Network/API failures are RPC errors, not fake success.
5. **Interfaces.** CLI `vp update-check`, and Settings → About shows the current
   version, a Check for Updates action, update status, and an Open Release Page
   button when available. The GitHub page is opened with `NSWorkspace`.
6. **No silent network activity.** The check runs only on explicit user action;
   it is not part of startup or the scheduler.

## Consequences

- Users can discover releases without trusting an auto-updater with install
  privileges.
- GitHub rate limits or network failures are visible errors.
- The feature remains honest about the deferred signing/notarization path.
- No update is installed automatically; users still download the DMG and
  replace the app themselves.
- The check exposes only public release metadata; no API key is sent.

## Alternatives considered

- **Sparkle/auto-install:** requires signing/notarization and appcast
  infrastructure that is explicitly deferred.
- **Check in the Swift app directly:** duplicates HTTP/version logic and makes
  CLI/headless checks impossible.
- **Silent background checks:** unnecessary network activity and privacy
  surface for an explicit, low-frequency action.
