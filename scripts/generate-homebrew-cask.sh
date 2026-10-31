#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

TAG="${1:-}"
VERSION="${TAG#v}"
if [[ -z "$VERSION" ]]; then
  VERSION="$(awk -F'"' '/^version = / { print $2; exit }' Cargo.toml)"
fi
ARCH="${2:-$(uname -m)}"
DMG="$ROOT/dist/VerlinPark-${VERSION}-${ARCH}.dmg"
OUT="${3:-$ROOT/dist/verlinpark.rb}"

if [[ ! -f "$DMG" ]]; then
  echo "DMG not found: $DMG" >&2
  echo "Run 'just package' (or pass a tagged version/arch) first." >&2
  exit 1
fi

SHA="$(shasum -a 256 "$DMG" | awk '{print $1}')"

cat > "$OUT" <<EOF
cask "verlinpark" do
  version "$VERSION"
  sha256 "$SHA"

  url "https://github.com/Verlintas/VerlinPark/releases/download/v#{version}/VerlinPark-#{version}-${ARCH}.dmg"
  name "VerlinPark"
  desc "Native macOS workspace for multi-agent coding teams"
  homepage "https://github.com/Verlintas/VerlinPark"

  depends_on arch: :${ARCH}
  depends_on macos: ">= :sequoia"

  app "VerlinPark.app"

  caveats <<~EOS
    This build is ad-hoc signed and is not notarized with an Apple Developer ID.
    On first launch, macOS may require System Settings -> Privacy & Security ->
    "Open Anyway". The app starts its bundled verlinpadd helper and writes local
    state under ~/Library/Application Support/VerlinPark.
  EOS
end
EOF

echo "cask: $OUT"
echo "sha256: $SHA"
