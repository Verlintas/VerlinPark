#!/usr/bin/env bash
# Aggregate-only OpenCode2 cache baseline extraction.
#
# This script reads an OpenCode2 SQLite database in read-only mode. It never
# selects message text, titles, tool arguments, or any other user content. The
# only columns are ids, token counters, timestamps, roles, and instruction
# hashes. Output is intentionally aggregate so it can be shared in an issue.
#
# Usage:
#   scripts/cache-benchmark/opencode2_baseline.sh [DATABASE]
#   OPENCODE_SESSION=ses_... scripts/cache-benchmark/opencode2_baseline.sh
#
# Default database: ~/.local/share/opencode/opencode.db
# Requires: sqlite3 with JSON1 (the stock macOS binary is sufficient).
#
# Denominator note: OpenCode2 stores uncached input in `tokens_input`, so its
# ratio is cache_read / (uncached_input + cache_read). VerlinPark reports total
# input, so its ratio is cache_read / total_input. The script labels both.

set -euo pipefail

DB="${1:-${HOME}/.local/share/opencode/opencode.db}"
if [[ ! -f "$DB" ]]; then
  echo "opencode2 baseline: database not found: $DB" >&2
  exit 2
fi
if ! command -v sqlite3 >/dev/null 2>&1; then
  echo "opencode2 baseline: sqlite3 is required" >&2
  exit 2
fi

# Read-only + immutable avoids creating WAL/SHM files in the user's data dir.
URI="file:${DB}?mode=ro&immutable=1"

query() {
  sqlite3 -readonly "$URI" "$1"
}

echo "# OpenCode2 cache baseline"
echo
echo "database: ${DB}"
echo "generated_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
echo "content: aggregate token counters only; no message text or titles"
echo

echo "## Session totals (sessions with >100k cache-read tokens)"
echo
query "
SELECT
  id,
  COALESCE(tokens_input, 0) AS uncached_input,
  COALESCE(tokens_cache_read, 0) AS cache_read,
  COALESCE(tokens_output, 0) AS output,
  CASE
    WHEN (COALESCE(tokens_input,0) + COALESCE(tokens_cache_read,0)) = 0 THEN 0.0
    ELSE 100.0 * COALESCE(tokens_cache_read,0)
         / (COALESCE(tokens_input,0) + COALESCE(tokens_cache_read,0))
  END AS hit_pct
FROM session
WHERE COALESCE(tokens_cache_read, 0) > 100000
ORDER BY hit_pct DESC;
"
echo

echo "## Aggregate across sessions with cache telemetry"
echo
query "
SELECT
  COUNT(*) AS sessions,
  COALESCE(SUM(tokens_input), 0) AS uncached_input,
  COALESCE(SUM(tokens_cache_read), 0) AS cache_read,
  CASE
    WHEN (COALESCE(SUM(tokens_input),0) + COALESCE(SUM(tokens_cache_read),0)) = 0 THEN 0.0
    ELSE 100.0 * COALESCE(SUM(tokens_cache_read),0)
         / (COALESCE(SUM(tokens_input),0) + COALESCE(SUM(tokens_cache_read),0))
  END AS aggregate_hit_pct
FROM session
WHERE COALESCE(tokens_cache_read, 0) > 0;
"
echo

SESSION="${OPENCODE_SESSION:-}"
if [[ -n "$SESSION" ]]; then
  echo "## Per-call shape for ${SESSION} (no content)"
  echo
  query "
  SELECT
    json_extract(data,'\$.role') AS role,
    json_extract(data,'\$.tokens.input') AS uncached_input,
    json_extract(data,'\$.tokens.cache.read') AS cache_read,
    json_extract(data,'\$.tokens.output') AS output,
    time_created
  FROM message
  WHERE session_id = '${SESSION//\'/\'\'}'
  ORDER BY time_created
  LIMIT 100;
  "
  echo
  echo "## Instruction identity keys for ${SESSION} (content hashes only)"
  echo
  query "
  SELECT key, value
  FROM instruction_state
  WHERE session_id = '${SESSION//\'/\'\'}'
  ORDER BY key;
  "
else
  echo "## Per-call shape"
  echo
  echo "Set OPENCODE_SESSION=ses_... to extract one session's per-call rows."
fi
echo
echo "## Metric definition"
echo
echo "cache_hit_ratio = cache_read / (uncached_input + cache_read)"
echo "Targets: 99.84% stretch for stable-prefix >=200k-token sessions; 99.0% working day; 95% short sessions."
