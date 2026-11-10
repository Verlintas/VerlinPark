#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="${1:-$ROOT/apps/macos/build/Build/Products/Debug/VerlinPark.app}"
HELPER_DIR="$APP/Contents/Helpers"

cargo build --manifest-path "$ROOT/Cargo.toml" -p vp-daemon
mkdir -p "$HELPER_DIR"
cp "$ROOT/target/debug/verlinpadd" "$HELPER_DIR/verlinpadd"
chmod +x "$HELPER_DIR/verlinpadd"

# The browser helper is a separate sandboxed WKWebView process. It must live
# next to the daemon so the daemon can discover it without PATH guessing.
BROWSER_HELPER="$ROOT/apps/macos/build/Build/Products/Debug/VerlinParkBrowserHelper"
if [[ -x "$BROWSER_HELPER" ]]; then
  cp "$BROWSER_HELPER" "$HELPER_DIR/VerlinParkBrowserHelper"
  chmod +x "$HELPER_DIR/VerlinParkBrowserHelper"
else
  echo "warning: VerlinParkBrowserHelper not built; browser tool will report unavailable" >&2
fi

echo "bundled verlinpadd into $HELPER_DIR"
