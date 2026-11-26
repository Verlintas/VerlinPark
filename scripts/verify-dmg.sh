#!/usr/bin/env bash
# Verify a packaged VerlinPark DMG without installing or launching it.
# Checks the checksum, image, app/helper layout, and code signature.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

DMG="${1:-}"
if [[ -z "$DMG" ]]; then
  DMG="$(ls -1t dist/*.dmg 2>/dev/null | head -1 || true)"
fi
[[ -n "$DMG" && -f "$DMG" ]] || {
  echo "no DMG found; run ./scripts/package-release.sh first" >&2
  exit 1
}

CHECKSUM="$DMG.sha256"
[[ -f "$CHECKSUM" ]] || {
  echo "missing checksum file: $CHECKSUM" >&2
  exit 1
}

echo "==> checksum"
(
  cd "$(dirname "$DMG")"
  shasum -a 256 -c "$(basename "$CHECKSUM")"
)

echo "==> image"
hdiutil verify "$DMG" >/dev/null

MOUNT="$(mktemp -d "${TMPDIR:-/tmp}/vp-dmg.XXXXXX")"
cleanup() {
  hdiutil detach "$MOUNT" >/dev/null 2>&1 || true
  rmdir "$MOUNT" >/dev/null 2>&1 || true
}
trap cleanup EXIT

hdiutil attach "$DMG" -nobrowse -readonly -mountpoint "$MOUNT" >/dev/null
APP="$MOUNT/VerlinPark.app"
[[ -d "$APP" ]] || {
  echo "DMG does not contain VerlinPark.app" >&2
  exit 1
}
for helper in verlinpadd custosd custos vp-remote-agent; do
  [[ -x "$APP/Contents/Helpers/$helper" ]] || {
    echo "missing executable helper: Contents/Helpers/$helper" >&2
    exit 1
  }
done
for entry in bin/vp bin/verlinpadd bin/custosd bin/custos INSTALL.txt README.md LICENSE; do
  [[ -e "$MOUNT/$entry" ]] || {
    echo "missing staged entry: $entry" >&2
    exit 1
  }
done

echo "==> packaged CLI entrypoints"
for binary in vp verlinpadd custos custosd; do
  "$MOUNT/bin/$binary" --help >/dev/null || {
    echo "$binary --help failed from the DMG" >&2
    exit 1
  }
done

echo "==> signature"
codesign --verify --deep --strict "$APP"
codesign -dv --verbose=2 "$APP" 2>&1 | sed -n '1,12p'

echo "DMG verification passed: $DMG"
