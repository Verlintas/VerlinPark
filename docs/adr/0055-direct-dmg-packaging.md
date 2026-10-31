# ADR-0055: Ad-hoc direct DMG packaging

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

VerlinPark is GPL-3.0-only and cannot ship through the Mac App Store. The project
had only a debug build; there was no repeatable release artifact, no bundled
daemon, and no release workflow. Users had to build from source.

## Decision

1. **Direct DMG.** `just package` runs `scripts/package-release.sh` to build
   Release binaries, build the Release app, bundle `verlinpadd` under
   `Contents/Helpers`, ad-hoc sign the helper and app, stage `vp` and docs, and
   create `dist/VerlinPark-<version>-<arch>.dmg` plus SHA-256.
2. **Ad-hoc signing.** `codesign --sign -` is used. No Developer ID or
   notarization credentials are required, matching the current direct-distribution
   constraint. The install note explains Gatekeeper's "Open Anyway".
3. **Tag workflow.** Pushing a `v*` tag runs `.github/workflows/release.yml`,
   builds the DMG on macOS, and creates/updates a GitHub Release with the DMG and
   checksum.
4. **Contents.** The DMG includes the app, `bin/verlinpadd`, `bin/vp`, README,
   LICENSE, third-party notices, PROGRESS/AUDIT, and INSTALL.txt.
5. **Not notarized.** The artifact is ad-hoc signed only. A Developer ID and
   notarization can be added later without changing the packaging shape.

## Consequences

- Releases are reproducible and installable without a local Rust/Xcode setup.
- macOS may require a manual privacy/security override on first launch.
- The workflow still needs a real provider E2E test before calling the build
  production-ready.
- No updater, Homebrew tap, or automatic migration is included yet.

## Alternatives considered

- **App Store:** incompatible with GPL-3.0-only.
- **Unsigned zip:** worse Gatekeeper experience and no disk-image packaging.
- **Developer ID now:** requires external credentials and a paid account; this
  can be layered on later.
