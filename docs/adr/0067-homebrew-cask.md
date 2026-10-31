# ADR-0067: Generated Homebrew cask

- **Status:** Accepted
- **Date:** 2026-10-31

## Context

Direct distribution currently produces an ad-hoc signed DMG and SHA-256 on
tagged releases. Homebrew users had no install path short of downloading the
DMG manually.

## Decision

1. **Generate from the built DMG.** `scripts/generate-homebrew-cask.sh` reads the
   workspace version (or a release tag), locates the DMG, computes SHA-256, and
   writes `dist/verlinpark.rb`.
2. **Release URL.** The cask points at
   `https://github.com/Verlintas/VerlinPark/releases/download/v<version>/VerlinPark-<version>-<arch>.dmg`;
   the tag and DMG version must match.
3. **Architecture and OS.** The cask depends on the built architecture and
   macOS 15+ (Sequoia).
4. **Release workflow.** Tagged releases generate the cask after packaging and
   upload it beside the DMG and checksum.
5. **No tap yet.** Users can `brew install --cask ./verlinpark.rb`; a shared tap
   and `brew upgrade` channel are future work.
6. **Honest signing caveat.** The cask caveat states that the build is ad-hoc
   signed and not notarized, so first launch may need **Open Anyway**.

## Consequences

- Homebrew users get a reproducible checksum-verified install.
- The cask is not automatically updated for older releases; it is generated per
  release asset.
- There is no `brew upgrade` path until a tap exists.
- A version/tag mismatch fails the release step instead of publishing a broken
  cask.

## Alternatives considered

- **Manual cask in a tap:** needs a second repository and release automation.
- **Homebrew formula building from source:** much slower and requires a full
  Xcode/Rust toolchain for users.
- **No cask:** forces manual DMG handling.
