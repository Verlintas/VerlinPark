#!/usr/bin/env bash
# Explicit live DeepSeek smoke. This script never stores the key.
#
# Required:
#   DEEPSEEK_API_KEY or VERLINPARK_LIVE_API_KEY
#   DEEPSEEK_MODEL (for example deepseek-flash)
#
# Optional:
#   DEEPSEEK_BASE_URL (default: https://api.deepseek.com)
#   SMOKE_TOKEN_BUDGET (default: 120000)
#   SMOKE_TIMEOUT_SECS (default: 300)
#
# It builds the local binaries, runs the provider stream test, a deliberately
# small live cache-bench, and a full temporary Leader -> write_file -> idle
# session. It never fakes a pass: a non-idle session, wrong file content, or
# provider error exits nonzero.

set -euo pipefail

ROOT=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
cd "$ROOT"

API_KEY=${DEEPSEEK_API_KEY:-${VERLINPARK_LIVE_API_KEY:-}}
if [[ -z "$API_KEY" ]]; then
    echo "error: set DEEPSEEK_API_KEY or VERLINPARK_LIVE_API_KEY" >&2
    exit 2
fi
if [[ -z "${DEEPSEEK_MODEL:-}" ]]; then
    echo "error: set DEEPSEEK_MODEL (for example deepseek-flash)" >&2
    exit 2
fi

BASE_URL=${DEEPSEEK_BASE_URL:-https://api.deepseek.com}
TOKEN_BUDGET=${SMOKE_TOKEN_BUDGET:-120000}
TIMEOUT_SECS=${SMOKE_TIMEOUT_SECS:-300}

if [[ ! "$TOKEN_BUDGET" =~ ^[1-9][0-9]*$ ]]; then
    echo "error: SMOKE_TOKEN_BUDGET must be a positive integer" >&2
    exit 2
fi

DATA=$(mktemp -d "${TMPDIR:-/tmp}/vp-live-data.XXXXXX")
REPO=$(mktemp -d "${TMPDIR:-/tmp}/vp-live-repo.XXXXXX")
DAEMON_PID=""
cleanup() {
    if [[ -n "$DAEMON_PID" ]]; then
        kill "$DAEMON_PID" 2>/dev/null || true
        wait "$DAEMON_PID" 2>/dev/null || true
    fi
    rm -rf "$DATA" "$REPO"
}
trap cleanup EXIT

VP="$ROOT/target/debug/vp"
VERLINPARKD="$ROOT/target/debug/verlinpadd"

echo "== build =="
cargo build -q -p vp-daemon -p vp-cli

echo "== provider stream =="
VERLINPARK_LIVE_BASE_URL="$BASE_URL" \
VERLINPARK_LIVE_MODEL="$DEEPSEEK_MODEL" \
VERLINPARK_LIVE_API_KEY="$API_KEY" \
cargo test -q -p vp-providers --test live \
    openai_compatible_local_live_streams_text -- --ignored --nocapture

echo "== live cache-bench (small scenario; not the 200k acceptance run) =="
DEEPSEEK_API_KEY="$API_KEY" \
DEEPSEEK_MODEL="$DEEPSEEK_MODEL" \
DEEPSEEK_BASE_URL="$BASE_URL" \
"$VP" cache-bench run \
    --live --allow-live \
    --turns 4 \
    --stable-prefix-tokens 4000 \
    --initial-context-tokens 500 \
    --allow-unrealistic-scenario \
    --max-live-cost-usd 0.05 \
    --model "$DEEPSEEK_MODEL" \
    --base-url "$BASE_URL" \
    --output "$DATA/cache-bench"

echo "== full agent E2E =="
git -C "$REPO" init -q
printf '# Live smoke\n' > "$REPO/README.md"
git -C "$REPO" add README.md
git -C "$REPO" -c user.email=smoke@example.com -c user.name='Live Smoke' \
    commit -qm 'initial'

VERLINPARK_DATA_DIR="$DATA" \
VERLINPARK_API_KEY="$API_KEY" \
VERLINPARK_BASE_URL="$BASE_URL" \
VERLINPARK_MODEL="$DEEPSEEK_MODEL" \
VERLINPARK_MAX_TURNS=6 \
VERLINPARK_MAX_TOKENS=1024 \
VERLINPARK_APPROVAL_MODE=unattended \
VERLINPARK_TASK_SCHEDULER=0 \
"$VERLINPARKD" --data-dir "$DATA" > "$DATA/daemon.log" 2>&1 &
DAEMON_PID=$!

for _ in $(seq 1 150); do
    [[ -f "$DATA/run/daemon.json" ]] && break
    sleep 0.2
done
if [[ ! -f "$DATA/run/daemon.json" ]]; then
    echo "error: daemon did not publish discovery" >&2
    tail -40 "$DATA/daemon.log" >&2 || true
    exit 1
fi

PROJECT_JSON=$("$VP" --data-dir "$DATA" --json project-add "$REPO" --name live-smoke)
PROJECT=$(printf '%s' "$PROJECT_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')

SESSION_JSON=$(
    "$VP" --data-dir "$DATA" --json session-new "$PROJECT" \
        "Create a file named LIVE_SMOKE.md at the repository root. Its exact content must be: live provider smoke ok. Do not modify README.md. Use the write_file tool, then report success in one sentence." \
        --token-budget "$TOKEN_BUDGET" \
        --time-budget-secs "$TIMEOUT_SECS"
)
SESSION=$(printf '%s' "$SESSION_JSON" | python3 -c 'import json,sys; print(json.load(sys.stdin)["id"])')
echo "session=$SESSION"

STATUS=unknown
for _ in $(seq 1 120); do
    SESSIONS=$("$VP" --data-dir "$DATA" --json sessions --project "$PROJECT" 2>/dev/null || true)
    STATUS=$(printf '%s' "$SESSIONS" | python3 -c '
import json, sys
data = json.load(sys.stdin)
session_id = sys.argv[1]
print(next((row["status"] for row in data["sessions"] if row["id"] == session_id), "missing"))
' "$SESSION" 2>/dev/null || echo missing)
    echo "status=$STATUS"
    case "$STATUS" in
        idle|failed|cancelled|landed|missing) break ;;
    esac
    sleep 3
done

FILE=$(find "$DATA/worktrees" -name LIVE_SMOKE.md -print -quit 2>/dev/null || true)
if [[ "$STATUS" != "idle" ]]; then
    echo "error: session ended with status $STATUS" >&2
    "$VP" --data-dir "$DATA" events "$SESSION" --limit 120 >&2 || true
    exit 1
fi
if [[ -z "$FILE" || ! -f "$FILE" ]]; then
    echo "error: LIVE_SMOKE.md was not created" >&2
    exit 1
fi
if [[ "$(cat "$FILE")" != "live provider smoke ok" ]]; then
    echo "error: LIVE_SMOKE.md content mismatch" >&2
    cat "$FILE" >&2
    exit 1
fi

echo "== result =="
"$VP" --data-dir "$DATA" --json usage "$SESSION" | python3 -c '
import json, sys
data = json.load(sys.stdin)
totals = data["totals"]
print("calls=%s input=%s output=%s cache_read=%s cache_hit_ratio=%.6f" % (
    totals["calls"],
    totals["input_tokens"],
    totals["output_tokens"],
    totals["cache_read_tokens"],
    totals["cache_hit_ratio"],
))
'
"$VP" --data-dir "$DATA" --json cache "$SESSION" | python3 -c '
import json, sys
data = json.load(sys.stdin)
print("cache_calls=%s last_miss_reason=%s" % (
    data.get("cache_calls"),
    data.get("last_miss_reason"),
))
'
echo "live DeepSeek smoke passed; key was never written to disk"
