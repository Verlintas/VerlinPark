#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
APP="${1:-$ROOT/apps/macos/build/Build/Products/Debug/VerlinPark.app}"
HELPER_DIR="$APP/Contents/Helpers"

cargo build --manifest-path "$ROOT/Cargo.toml" -p vp-daemon
mkdir -p "$HELPER_DIR"
cp "$ROOT/target/debug/verlinpadd" "$HELPER_DIR/verlinpadd"
chmod +x "$HELPER_DIR/verlinpadd"

echo "bundled verlinpadd into $HELPER_DIR"
