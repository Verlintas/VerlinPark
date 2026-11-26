#!/usr/bin/env bash
# Release drill: seed a real daemon data directory, write a project through the
# normal RPC path, then prove v1 ZIP and signed/encrypted repository backups can
# be verified and restored offline. No provider key is used and no network
# service is contacted.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

VP="${VP_BIN:-$ROOT/target/debug/vp}"
DAEMON="${VP_DAEMON_BIN:-$ROOT/target/debug/verlinpadd}"
PASSPHRASE_ENV="VERLINPARK_DRILL_BACKUP_PASSPHRASE"
PASSPHRASE="drill-only-passphrase-not-a-secret"

fail() {
  echo "backup drill failed: $*" >&2
  exit 1
}

for binary in "$VP" "$DAEMON"; do
  [[ -x "$binary" ]] || fail "missing $binary; run: cargo build -p vp-cli -p vp-daemon"
done

WORK="$(mktemp -d "${TMPDIR:-/tmp}/vp-backup-drill.XXXXXX")"
DAEMON_PID=""
cleanup() {
  if [[ -n "$DAEMON_PID" ]] && kill -0 "$DAEMON_PID" 2>/dev/null; then
    kill "$DAEMON_PID" 2>/dev/null || true
    wait "$DAEMON_PID" 2>/dev/null || true
  fi
  rm -rf "$WORK"
}
trap cleanup EXIT

DATA="$WORK/data"
REPO="$WORK/repo"
OUT="$WORK/out"
mkdir -p "$DATA" "$REPO" "$OUT"

git -C "$REPO" init -q
git -C "$REPO" config user.name "VerlinPark Drill"
git -C "$REPO" config user.email "drill@verlinpark.local"
echo "backup drill fixture" >"$REPO/README.md"
git -C "$REPO" add README.md
git -C "$REPO" commit -q -m "initial"

"$DAEMON" --data-dir "$DATA" --port 0 >"$WORK/daemon.log" 2>&1 &
DAEMON_PID=$!
for _ in $(seq 1 200); do
  [[ -f "$DATA/run/daemon.json" ]] && break
  kill -0 "$DAEMON_PID" 2>/dev/null || fail "daemon exited early; see $WORK/daemon.log"
  sleep 0.05
done
[[ -f "$DATA/run/daemon.json" ]] || fail "daemon discovery file was not written"

"$VP" --data-dir "$DATA" --json project-add "$REPO" --name "Backup Drill" >"$OUT/project.json"
python3 - "$OUT/project.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1]))
assert value.get("name") == "Backup Drill", value
assert value.get("id"), value
PY

kill "$DAEMON_PID"
wait "$DAEMON_PID" 2>/dev/null || true
DAEMON_PID=""

project_count() {
  sqlite3 "$1/verlinpark.db" 'SELECT COUNT(*) FROM projects;'
}
expected_count="$(project_count "$DATA")"
[[ "$expected_count" == "1" ]] || fail "seed database has $expected_count projects, expected 1"

echo "==> v1 ZIP backup"
"$VP" --json --data-dir "$DATA" backup --output "$OUT/v1.zip" >"$OUT/v1-backup.json"
"$VP" --json restore --input "$OUT/v1.zip" --data-dir "$OUT/v1-restore" >"$OUT/v1-restore.json"
[[ "$(project_count "$OUT/v1-restore")" == "1" ]] || fail "v1 restore lost the project row"

echo "==> signed and encrypted repository backup"
KEYRING="$WORK/keyring"
"$VP" --json backup-key-generate drill-key --keyring-dir "$KEYRING" >"$OUT/key.json"
export "$PASSPHRASE_ENV=$PASSPHRASE"
"$VP" --json --data-dir "$DATA" backup \
  --repository "$OUT/repository" \
  --passphrase-env "$PASSPHRASE_ENV" \
  --sign --keyring-dir "$KEYRING" >"$OUT/repository-backup.json"
SNAPSHOT="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["snapshot_id"])' "$OUT/repository-backup.json")"
[[ -n "$SNAPSHOT" ]] || fail "repository backup did not return a snapshot id"

"$VP" --json backup-verify \
  --repository "$OUT/repository" \
  --passphrase-env "$PASSPHRASE_ENV" \
  --keyring-dir "$KEYRING" >"$OUT/verify.json"
python3 - "$OUT/verify.json" <<'PY'
import json, sys
value = json.load(open(sys.argv[1]))
assert value.get("valid") is True, value
assert value.get("signed") is True, value
assert value.get("authenticity") == "signed", value
assert value.get("blob_verification") == "full", value
PY

"$VP" --json restore \
  --repository "$OUT/repository" \
  --snapshot "$SNAPSHOT" \
  --data-dir "$OUT/repository-restore" \
  --passphrase-env "$PASSPHRASE_ENV" \
  --keyring-dir "$KEYRING" >"$OUT/repository-restore.json"
[[ "$(project_count "$OUT/repository-restore")" == "1" ]] || fail "repository restore lost the project row"

python3 - "$OUT/v1-restore/verlinpark.db" "$OUT/repository-restore/verlinpark.db" <<'PY'
import sqlite3, sys
for path in sys.argv[1:]:
    con = sqlite3.connect(path)
    rows = con.execute("SELECT name, root FROM projects").fetchall()
    assert rows and rows[0][0] == "Backup Drill", (path, rows)
print("restored project rows match")
PY

echo "backup drill passed (v1 ZIP + signed encrypted repository; no live provider)"
