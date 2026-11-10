#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VERSION="$(awk -F'"' '/^version = / { print $2; exit }' Cargo.toml)"
ARCH="$(uname -m)"
STAGE="$ROOT/dist/VerlinPark-${VERSION}-${ARCH}"
DMG="$ROOT/dist/VerlinPark-${VERSION}-${ARCH}.dmg"
SIGN_IDENTITY="${VERLINPARK_SIGN_IDENTITY:-}"
NOTARY_PROFILE="${VERLINPARK_NOTARY_PROFILE:-}"
SIGNED=0
NOTARIZED=0

if [[ -z "$VERSION" ]]; then
  echo "could not read workspace version" >&2
  exit 1
fi

echo "==> building release binaries"
cargo build --release -p vp-daemon -p vp-cli

echo "==> building release app"
(
  cd apps/macos
  xcodegen generate
  xcodebuild \
    -project VerlinPark.xcodeproj \
    -scheme VerlinPark \
    -configuration Release \
    -destination 'platform=macOS' \
    -derivedDataPath build \
    CODE_SIGNING_ALLOWED=NO \
    build
)

APP="$ROOT/apps/macos/build/Build/Products/Release/VerlinPark.app"
if [[ ! -d "$APP" ]]; then
  echo "release app not found at $APP" >&2
  exit 1
fi

HELPER_DIR="$APP/Contents/Helpers"
mkdir -p "$HELPER_DIR"
cp "$ROOT/target/release/verlinpadd" "$HELPER_DIR/verlinpadd"
chmod +x "$HELPER_DIR/verlinpadd"
BROWSER_HELPER="$ROOT/apps/macos/build/Build/Products/Release/VerlinParkBrowserHelper"
if [[ -x "$BROWSER_HELPER" ]]; then
  cp "$BROWSER_HELPER" "$HELPER_DIR/VerlinParkBrowserHelper"
  chmod +x "$HELPER_DIR/VerlinParkBrowserHelper"
else
  echo "warning: VerlinParkBrowserHelper not built; browser tool will report unavailable" >&2
fi

if [[ -n "$SIGN_IDENTITY" && "$SIGN_IDENTITY" != "-" ]]; then
  echo "==> Developer ID signing: $SIGN_IDENTITY"
  codesign --force --options runtime --timestamp --sign "$SIGN_IDENTITY" "$HELPER_DIR/verlinpadd"
  codesign --force --deep --options runtime --timestamp --sign "$SIGN_IDENTITY" "$APP"
  codesign --verify --deep --strict --verbose=2 "$APP"
  SIGNED=1
else
  echo "==> ad-hoc signing"
  codesign --force --sign - "$HELPER_DIR/verlinpadd"
  codesign --force --deep --sign - "$APP"
fi

if [[ -n "$NOTARY_PROFILE" ]]; then
  if [[ "$SIGNED" != "1" ]]; then
    echo "VERLINPARK_NOTARY_PROFILE is set but no Developer ID identity was configured" >&2
    exit 1
  fi
  echo "==> notarizing app"
  NOTARY_ZIP="$ROOT/dist/VerlinPark-${VERSION}-${ARCH}.zip"
  rm -f "$NOTARY_ZIP"
  ditto -c -k --keepParent "$APP" "$NOTARY_ZIP"
  xcrun notarytool submit "$NOTARY_ZIP" --keychain-profile "$NOTARY_PROFILE" --wait
  xcrun stapler staple "$APP"
  xcrun stapler validate "$APP"
  rm -f "$NOTARY_ZIP"
  NOTARIZED=1
fi

echo "==> staging"
rm -rf "$STAGE" "$DMG"
mkdir -p "$STAGE/bin" "$STAGE/docs"
ditto "$APP" "$STAGE/VerlinPark.app"
cp "$ROOT/target/release/verlinpadd" "$STAGE/bin/verlinpadd"
cp "$ROOT/target/release/vp" "$STAGE/bin/vp"
chmod +x "$STAGE/bin/verlinpadd" "$STAGE/bin/vp"
cp "$ROOT/README.md" "$STAGE/README.md"
cp "$ROOT/LICENSE" "$STAGE/LICENSE"
cp "$ROOT/THIRD_PARTY_NOTICES.md" "$STAGE/THIRD_PARTY_NOTICES.md"
cp "$ROOT/docs/PROGRESS.md" "$STAGE/docs/PROGRESS.md"
cp "$ROOT/docs/AUDIT.md" "$STAGE/docs/AUDIT.md"

INSTALL_NOTE="This build is ad-hoc signed. It is not notarized with an Apple Developer ID.
On first launch macOS may require System Settings -> Privacy & Security ->
\"Open Anyway\". No App Store account is required."
if [[ "$SIGNED" == "1" ]]; then
  INSTALL_NOTE="This build is signed with a Developer ID. It is not notarized."
fi
if [[ "$NOTARIZED" == "1" ]]; then
  INSTALL_NOTE="This build is Developer ID signed and notarized. Gatekeeper should
accept it without a privacy override."
fi
cat > "$STAGE/INSTALL.txt" <<EOF
VerlinPark is distributed directly (GPL-3.0-only), not through the App Store.

1. Drag VerlinPark.app into /Applications.
2. Launch it. The app starts its bundled verlinpadd helper and writes a
   discovery token under ~/Library/Application Support/VerlinPark.
3. For CLI use, copy bin/verlinpadd and bin/vp somewhere on PATH.

${INSTALL_NOTE}
EOF

echo "==> creating DMG"
mkdir -p "$ROOT/dist"
hdiutil create \
  -volname "VerlinPark" \
  -srcfolder "$STAGE" \
  -ov \
  -format UDZO \
  "$DMG" >/dev/null

echo "==> checksums"
(
  cd "$ROOT/dist"
  shasum -a 256 "$(basename "$DMG")" > "$(basename "$DMG").sha256"
)

echo
echo "package:   $DMG"
echo "checksum:  $DMG.sha256"
echo "staged:    $STAGE"
echo "signed:    $SIGNED"
echo "notarized: $NOTARIZED"
