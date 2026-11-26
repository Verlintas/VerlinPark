#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

MODE="${1:-full}"
CARGO_VERSION="$(python3 - <<'PY'
import tomllib
with open('Cargo.toml', 'rb') as handle:
    data = tomllib.load(handle)
print(data['workspace']['package']['version'])
PY
)"
APP_VERSION="$(python3 - <<'PY'
import re
text = open('apps/macos/project.yml').read()
match = re.search(r'MARKETING_VERSION:\s*([^\s]+)', text)
print(match.group(1) if match else 'missing')
PY
)"

echo "==> release preflight"
echo "    cargo workspace version: $CARGO_VERSION"
echo "    macOS marketing version: $APP_VERSION"
if [[ "$CARGO_VERSION" == *-dev ]]; then
  echo "    status: development version (not a release tag candidate)"
fi
if [[ "${CARGO_VERSION%-dev}" != "$APP_VERSION" ]]; then
  echo "error: Cargo and macOS marketing versions do not align" >&2
  exit 1
fi
if [[ -n "$(git status --porcelain)" ]]; then
  echo "error: working tree is dirty; commit or stash before a release check" >&2
  git status --short >&2
  exit 1
fi

for script in scripts/bundle-daemon.sh scripts/package-release.sh; do
  for binary in custosd custos verlinpadd vp-remote-agent; do
    if ! grep -q "$binary" "$script"; then
      echo "error: $script does not mention $binary" >&2
      exit 1
    fi
  done
done

echo "==> cargo fmt"
cargo fmt --all -- --check
echo "==> clippy"
cargo clippy --workspace --all-targets -- -D warnings
echo "==> rust tests"
cargo test --workspace --locked

if [[ "$MODE" == "full" ]]; then
  echo "==> swiftformat"
  swiftformat --lint apps/macos/VerlinPark apps/macos/VerlinParkTests \
    apps/macos/VerlinParkBrowserHelper apps/macos/VerlinParkComputerHelper \
    apps/macos/VerlinParkComputerHelperCore apps/macos/VerlinParkComputerHelperTests
  echo "==> xcodebuild"
  (
    cd apps/macos
    xcodegen generate
    xcodebuild -project VerlinPark.xcodeproj -scheme VerlinPark \
      -configuration Debug -destination 'platform=macOS' \
      -derivedDataPath build CODE_SIGNING_ALLOWED=NO test
  )
  echo "==> backup/restore drill"
  cargo build --locked -p vp-cli -p vp-daemon
  ./scripts/backup-restore-drill.sh
fi

echo "==> release binaries"
cargo build --release --locked -p vp-daemon -p vp-cli -p vp-custos -p vp-remote-agent
for binary in verlinpadd vp custosd custos vp-remote-agent; do
  test -x "target/release/$binary" || { echo "missing target/release/$binary" >&2; exit 1; }
done
echo "release preflight passed (not a signed/notarized release)"
