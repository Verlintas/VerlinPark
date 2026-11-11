#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="${1:-$ROOT/apps/macos/build/Build/Products/Debug/VerlinPark.app}"
HELPER_DIR="$APP/Contents/Helpers"

cargo build --manifest-path "$ROOT/Cargo.toml" -p vp-daemon -p vp-remote-agent
mkdir -p "$HELPER_DIR"
cp "$ROOT/target/debug/verlinpadd" "$HELPER_DIR/verlinpadd"
chmod +x "$HELPER_DIR/verlinpadd"

# The remote workspace helper is launched by the daemon over ssh or stdio.
# It is a separate binary so it can be copied to a remote host.
cp "$ROOT/target/debug/vp-remote-agent" "$HELPER_DIR/vp-remote-agent"
chmod +x "$HELPER_DIR/vp-remote-agent"

# The browser helper is a separate sandboxed WKWebView process. It must live
# next to the daemon so the daemon can discover it without PATH guessing.
BROWSER_HELPER="$ROOT/apps/macos/build/Build/Products/Debug/VerlinParkBrowserHelper"
if [[ -x "$BROWSER_HELPER" ]]; then
  cp "$BROWSER_HELPER" "$HELPER_DIR/VerlinParkBrowserHelper"
  chmod +x "$HELPER_DIR/VerlinParkBrowserHelper"
else
  echo "warning: VerlinParkBrowserHelper not built; browser tool will report unavailable" >&2
fi

# The computer helper uses the user's Accessibility, CGEvent, and
# ScreenCaptureKit privileges. It is daemon-launched and deliberately not
# Seatbelt-confined; see ADR-0158 for the trust model.
COMPUTER_HELPER="$ROOT/apps/macos/build/Build/Products/Debug/VerlinParkComputerHelper"
if [[ -x "$COMPUTER_HELPER" ]]; then
  cp "$COMPUTER_HELPER" "$HELPER_DIR/VerlinParkComputerHelper"
  chmod +x "$HELPER_DIR/VerlinParkComputerHelper"
else
  echo "warning: VerlinParkComputerHelper not built; computer tool will report unavailable" >&2
fi

echo "bundled verlinpadd into $HELPER_DIR"
