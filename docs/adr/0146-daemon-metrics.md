# ADR-0146: Daemon metrics endpoint and snapshot

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Observability
- **Related:** ADR-0123, ADR-0128

## Context

Operators can inspect sessions, tasks, leases, and usage through the SwiftUI
client, the RPC surface, and the CLI. There was no low-cost way to scrape the
daemon with standard infrastructure tooling, and no single read-only aggregate
view suitable for dashboards and alerting.

Adding metrics is not free. The daemon holds prompts, file paths, provider API
keys, model/provider configuration, and per-session agent identity. A metrics
surface that accepts arbitrary labels or exports per-session fields would leak
private data and create unbounded cardinality.

## Decision

### Prometheus endpoint

`GET /metrics` is served by the existing axum listener on `127.0.0.1`. It is
**loopback-only**, like `/rpc`, and requires the same `Authorization: Bearer
<discovery token>` header. Prometheus can set the header in its scrape config.
Missing or invalid credentials return `401` without metric data. `/health`
remains unauthenticated and unchanged.

A successful response uses `Content-Type: text/plain; version=0.0.4` and is
hand-rendered from the typed snapshot. No Prometheus client dependency is
added.

The exported series are:

| Metric | Labels | Type |
|---|---|---|
| `verlinpark_up` | none | gauge |
| `verlinpark_info` | `version`, `protocol` | gauge |
| `verlinpark_uptime_seconds` | none | gauge |
| `verlinpark_projects_total` | none | gauge |
| `verlinpark_sessions` | `status` | gauge |
| `verlinpark_plan_tasks` | `status` | gauge |
| `verlinpark_merge_queue` | `status` | gauge |
| `verlinpark_intake` | `status` | gauge |
| `verlinpark_continuations` | `status` | gauge |
| `verlinpark_recoveries` | `status` | gauge |
| `verlinpark_memories` | `status` | gauge |
| `verlinpark_memory_conflicts` | `status` | gauge |
| `verlinpark_runtime_leases` | `kind` | gauge |
| `verlinpark_tokens_total` | `direction=in\|out` | counter |
| `verlinpark_cache_tokens_total` | `direction=read\|write` | counter |
| `verlinpark_cost_usd_total` | none | counter |

HELP and TYPE lines, metric order, and label values are deterministic. Status
and kind groups are ordered lexicographically. Label values are escaped for
backslash, double quote, newline, and carriage return.

### RPC and CLI

`daemon.metrics` returns `DaemonMetricsResult`, containing the typed
`DaemonMetrics` object and the exact rendered Prometheus string. It is an
additive protocol method within major version 1.

`vp metrics` calls the RPC, prints pretty JSON by default, and prints the
rendered text with `--prometheus`. Discovery and token handling are the same as
every other CLI command.

### Snapshot shape

`Store::daemon_metrics_snapshot()` is read-only. It uses `GROUP BY` counts for
the nine status/kind tables and one aggregate query over `usage_ledger` for
input/output tokens, cache-read/cache-write tokens, and cost. Uptime, version,
and protocol are added by the daemon, not the store.

Counts are clamped to non-negative `u64`. Each grouped vector is capped at 64
entries as defense against a corrupted or imported database; the daemon's own
status vocabularies are much smaller. Token totals come from the usage ledger,
so pre-ledger session counters are not double-counted.

`verlinpark_up` is a process liveness value, not a readiness guarantee:
`/health` remains the simple health probe.

## Why no per-session labels

A `session_id`, task id, agent id, branch, model, provider, or path label would
grow without bound as the project history grows. It would also turn a metrics
scrape into an inventory of user work and provider configuration. High-
cardinality identity belongs in the RPC/CLI surfaces that already authorize and
scope access, not in a metric label.

## What is not exposed

The metrics surface does not expose:

- prompts, goals, messages, tool input/output, or event payloads;
- repository, worktree, artifact, or source paths;
- API keys, tokens, authorization headers, or other secrets;
- model names, provider names, agent names, session ids, task ids, or commit
  hashes;
- per-session or per-model usage breakdowns.

The existing `session.usage`, `dashboard.snapshot`, and related RPCs remain the
place for detailed operator inspection.

## Consequences

- Standard scrapers can monitor bounded daemon health and queue pressure.
- The endpoint is not exposed beyond loopback and cannot be read without the
  discovery token.
- Counts are projections over SQLite, not event-derived; a scrape reflects the
  current database state.
- Adding a new status vocabulary entry changes label cardinality by one. Adding
  a new metric requires a deliberate HELP/TYPE entry and ADR review if it would
  introduce identity labels.
- Runtime leases are counted by `kind`; expired-but-not-yet-swept rows remain
  visible until the sweeper or next acquisition removes them.
