# ADR-0090: Optional Developer ID signing, notarization, and tap publish

- **Status:** Accepted; Developer ID/notarization deferred by product decision
- **Date:** 2026-11-07

> **Product note (2026-11-07):** Developer ID signing and notarization are
> explicitly out of scope for now. The ad-hoc DMG is the supported distribution
> path and does not require an Apple Developer account. The optional code and
> workflow hooks stay for a possible later reversal, but are not an active
> roadmap item.

## Context

The direct-distribution DMG was always ad-hoc signed. Users had to override
Gatekeeper on first launch, and the generated Homebrew cask had to live only as
a release asset. Missing Developer ID/notarization and a real tap were the two
largest release-readiness gaps.

The project also must remain usable without an Apple Developer account: local
builds and forks should still produce a DMG.

## Decision

1. **Ad-hoc remains the default.** `just package` still works with no secrets.
2. **Optional Developer ID.** `VERLINPARK_SIGN_IDENTITY` switches the helper and
   app to `codesign --options runtime --timestamp`. The helper is signed first,
   then the app, and `codesign --verify` gates the result.
3. **Optional notarization.** `VERLINPARK_NOTARY_PROFILE` names a
   `notarytool` keychain profile. The app is zipped, submitted, stapled, and
   validated **before** it is staged into the DMG, so the installer note and
   cask caveat can accurately say the app is notarized.
4. **Cask metadata.** `VERLINPARK_SIGNED` / `VERLINPARK_NOTARIZED` select the
   cask caveat text. The cask script continues to work without them.
5. **Release workflow.** The tag workflow optionally imports
   `APPLE_CERTIFICATE_P12_BASE64`, creates a temporary keychain, reads
   `APPLE_SIGN_IDENTITY` (or discovers it), and configures notarization from
   `APPLE_NOTARY_APPLE_ID` / `APPLE_NOTARY_TEAM_ID` / `APPLE_NOTARY_APPLE_PASSWORD`.
   If the secrets are absent it falls back to ad-hoc.
6. **Optional tap publish.** `HOMEBREW_TAP_REPO` + `HOMEBREW_TAP_TOKEN` clone the
   tap repository and commit `Casks/verlinpark.rb`; otherwise the cask remains a
   GitHub release asset.
7. **Runner.** The release job uses `macos-15-arm64` so the native `uname -m`
   path produces an arm64 DMG.

## Consequences

- Maintainers with a Developer ID can produce Gatekeeper-friendly releases with
  no code changes.
- Forks and local builds keep the ad-hoc path.
- Signing/notarization/tap publishing cannot be exercised in CI without secrets;
  the fallback path is the tested default. The workflow secrets are documented
  in README.
- There is still no Sparkle/auto-update feed. Tap users can `brew upgrade` only
  when a new release rewrites the cask.
- `--deep` signing is retained for the app bundle; nested code should still be
  signed inside-out if new frameworks are added.

## Alternatives considered

- **Require Developer ID:** locks out local/fork builds and contradicts direct
  distribution without account dependency.
- **Notarize the DMG only:** leaves the app inside without a stapled ticket for
  users who extract it; notarizing the app first is more robust.
- **Auto-update now:** Sparkle adds a feed, signing key, and update UX; keep it
  as a separate release slice.
