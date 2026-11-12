# ADR-0191: Cache epochs, compaction policy, and cross-session reuse

- **Status:** Accepted
- **Date:** 2026-11-12
- **Depends on:** ADR-0173 (cache-first prompt sections), ADR-0188
  (content-addressed instruction store and prompt layout v3), ADR-0190
  (tool-bundle partition and freeze)
- **Complements:** ADR-0153 (cache and preamble telemetry)

## Context

L1 made the prompt head materialized bytes and L3 froze a canonical tool
bundle. The runtime can now prove byte identity for a single run, but three
cache-lifecycle questions remained unanswered:

1. **What is the reusable identity?** `prefix_hash` includes session/run
   context because those bytes precede the conversation. It is the correct
   provider-prefix identity for one run, but it is the wrong key for
   cross-session reuse: two sessions with the same charter, sources, skills,
   and tools should share a *stable* epoch even when their goals differ.
2. **When is compaction allowed?** The old default compacted at 75% of the
   hard limit. On a 1M-token DeepSeek window that discards roughly 250k tokens
   of append-only history and the corresponding provider cache tail long
   before the window is actually at risk.
3. **Why did a call miss?** Cache telemetry could say "no reads" but could not
   separate a local model/tool/project change from provider eviction or from
   the first call of a session that was reusing another session's bytes.

This ADR defines the cache epoch as the shareable stable-prefix identity, makes
compaction late and explicit, attributes misses deterministically, and records
cross-session reuse only from provider-reported usage. It does not change any
approval, sandbox, path, write-scope, Verifier, merge, capacity, or budget gate.

## Decision

### 1. Epoch identity

A cache epoch commits to the stable instruction block only:

```text
cache_epoch = sha256(
  "verlinpark.cache_epoch/v1\n" +
  version + "\n" +
  model + "\n" +
  tools_hash + "\n" +
  core_hash + "\n" +
  project_hash + "\n" +
  ordered core section (key, ordinal, hash)* +
  ordered project section (key, ordinal, hash)*
)
```

The following bytes are explicitly excluded:

- the current prompt;
- `session/context` (goal, identity, worktree, branch);
- `run/task` (task objective);
- `tail/memory` and the memory brief;
- the conversation and its message-prefix hash;
- compaction boundaries and the conversation cache generation.

`core_sections` and `project_sections` are the ordered `PromptSectionRef`
values from L1. Including key and ordinal means a section reorder or rename
cannot silently reuse a provider prefix that represents different bytes. The
combined `core_hash` / `project_hash` additionally commit to the joined text.

The finalized epoch is recorded on `context.prompt_prefix`, every
`context.cache`, warm telemetry, and the epoch RPC. The change reason on
`context.prompt_prefix` is `first`, `model`, `tools`, `project`, `core`,
`sections`, `unchanged`, or `unknown`. L1's placeholder
(`cache_epoch = prefix_hash`, `cache_epoch_source = l1_placeholder`) is
replaced by `cache_epoch_source = "l4"` and `cache_epoch_placeholder = false`.
`CacheEpoch::from_event_payload` verifies the recorded identity hash matches the
recorded epoch and rejects placeholder events, so a legacy event cannot be
mistaken for a finalized one.

This is a *shareable* identity, not a claim that a provider has cached those
bytes. Provider reuse is evidence only when usage reports cache-read tokens.

### 2. Persisted projection

Two SQLite tables own the projection.

```sql
prompt_prefix_observations (
  project_id                 TEXT NOT NULL,
  cache_epoch                TEXT NOT NULL,
  session_id                 TEXT NOT NULL,
  model                      TEXT NOT NULL,
  tool_bundle                TEXT NOT NULL,
  tool_bundle_hash           TEXT NOT NULL,
  core_hash                  TEXT NOT NULL,
  project_hash               TEXT NOT NULL,
  core_sections              TEXT NOT NULL,   -- bounded JSON
  project_sections           TEXT NOT NULL,   -- bounded JSON
  first_seen                 TEXT NOT NULL,
  last_seen                  TEXT NOT NULL,
  observed_input_tokens      INTEGER NOT NULL,
  observed_cache_read_tokens INTEGER NOT NULL,
  calls                      INTEGER NOT NULL,
  PRIMARY KEY (project_id, cache_epoch, session_id)
);

prompt_prefixes (
  project_id                 TEXT NOT NULL,
  cache_epoch                TEXT NOT NULL,
  model                      TEXT NOT NULL,
  tool_bundle                TEXT NOT NULL,
  tool_bundle_hash           TEXT NOT NULL,
  core_hash                  TEXT NOT NULL,
  project_hash               TEXT NOT NULL,
  core_sections              TEXT NOT NULL,
  project_sections           TEXT NOT NULL,
  first_seen                 TEXT NOT NULL,
  last_seen                  TEXT NOT NULL,
  observed_input_tokens      INTEGER NOT NULL,
  observed_cache_read_tokens INTEGER NOT NULL,
  session_count              INTEGER NOT NULL,
  call_count                 INTEGER NOT NULL,
  PRIMARY KEY (project_id, cache_epoch)
);
```

The aggregate is a projection of observations, never a query of a session's
current configuration. `first_seen` / `last_seen` are min/max observation
times. `session_count` is the number of distinct sessions, and `call_count` is
the sum of their provider calls.

`append_cache_call_event` appends `context.cache` and folds the observation in
one SQLite transaction. Cross-session fields are computed inside that
transaction *before* the event payload is persisted, so replay derives exactly
the same observation from the event stream. There is no committed state where
the event and its projection disagree. `replace_prompt_prefix_observations`
replaces one session's rows and recomputes every aggregate for the project;
`recompute_prompt_prefix_aggregates` is the idempotent repair primitive.

No prompt text, token IDs, headers, or provider response bodies are stored in
either table. `core_sections` / `project_sections` contain key/hash/ordinal
references only.

### 3. Cross-session accounting

For every ordinary provider call:

- `cache_epoch_reused_from_sessions` is the number of *other* sessions that had
  already observed the epoch before this call;
- `cross_session_first_call` is true only when this session had zero prior
  observations for the epoch;
- `cross_session_cache_read_tokens` is the provider-reported cache-read token
  count, but only when `cross_session_first_call` is true and at least one
  other session had observed the epoch. Later calls include the conversation
  prefix this session itself created, so attributing them to another session
  would overclaim reuse.

A new session whose first call reports cache reads therefore reports both the
reuse count and the provider evidence. A new session whose first call reports
zero reads reports a miss, not a reuse.

This is project-scoped state, not global. Two projects that happen to produce
identical bytes have separate rows and separate counts.

### 4. Compaction policy

The default threshold is derived from the model window:

```text
hard_limit = max(window - output_reserve - 1024, 1024)
threshold  = if explicit_override > 0:
                 min(override, hard_limit)
             else:
                 min(floor(0.90 * window), hard_limit)
```

- `window` is the configured model context window, including per-role
  `RoleModelConfig.context_window`.
- `output_reserve` is the configured `max_tokens` (default 4096).
- 1024 tokens are reserved for provider framing and the final assistant turn.
- The default ratio is 90% (9000 basis points), not 75%.
- `VERLINPARK_COMPACT_AFTER_TOKENS` remains an explicit override and may be
  as small as the operator wants; it is only clamped to the hard limit. A tiny
  override is intentional and is not silently replaced by the default.
- `window == 0` disables automatic management and preserves an explicit
  override.

For DeepSeek `deepseek-flash` with a 1,048,576-token window, the default
threshold is 943,718 tokens. For a 128k window it is 115,200. For an 8k window
with a 2k output reserve the hard limit (4,928) wins.

Compaction is evaluated only when the runtime is assembling the next request
and the estimate includes the system prompt and tool schemas. It is not
scheduled because a session is old, because a turn count elapsed, or because a
timer fired.

`context.compacted` and `context.compaction` retain their existing fields.
They additionally record:

- `cache_epoch_before` / `cache_epoch_after` (the recorded run epoch, or null
  when a manual compaction has no prompt-prefix event);
- `cache_epoch_changed` (`false`; a compaction boundary is not a stable
  instruction change);
- `cache_epoch_source` (`recorded` or `unavailable`);
- `estimated_lost_cache_read_usd`.

A new bounded `context.compaction.cache_reset` event records the authoritative
lifecycle accounting:

```json
{
  "mode": "summary",
  "compaction_count": 3,
  "cache_epoch_before": "…",
  "cache_epoch_after": "…",
  "cache_epoch_changed": false,
  "cache_generation_before": "…",
  "cache_generation_after": "…",
  "reset_reason": "compaction",
  "conversation_tokens_before": 910000,
  "conversation_tokens_after": 18000,
  "invalidated_tokens": 892000,
  "estimated_lost_cache_read_tokens": 892000,
  "estimated_lost_cache_read_usd": 2.4084,
  "accounting_enabled": true,
  "threshold_tokens": 943718,
  "hard_limit_tokens": 1043456
}
```

The event contains counts and estimates only. It never contains the summary
text. `accounting_enabled` reports the legacy `compact_aware` policy switch;
the L4 estimate is computed regardless so the boundary is never hidden.
`estimated_lost_cache_read_usd` is `invalidated_tokens * max(0, input_price -
cache_read_price)` using configured per-role prices and is explicitly an
estimate, not a provider invoice. Zero configured prices produce zero.

The stable instruction prefix is not rewritten. The conversation cache
generation changes, and subsequent provider calls are attributed to
`compaction_reset` until a call hits again.

### 5. Miss attribution

`CacheCallTracker::observe_call` receives provider input tokens, cache-read
tokens, the finalized epoch, and the projection's reuse facts. It records a
reason only for a low hit (cache-read share below 50%):

| condition | reason |
|---|---|
| model changed | `model_changed` |
| tool hash changed | `tools_changed` |
| project hash changed | `project_changed` |
| core/prefix changed within the same epoch | `prefix_changed` |
| first call after a known compaction | `compaction_reset` |
| same epoch, low hit, previous call exists | `provider_cache_miss` |
| no previous call, no prior session for the epoch | `first_call` |
| no previous call but another session used the epoch | `provider_cache_miss` |
| legacy event without component hashes | `ttl_or_provider_miss` |
| new epoch with no attributable component | `unknown` |

`provider_cache_miss` means "our bytes did not change; the provider did not
report a hit." It is not a generator or project-change claim. Normal partial
hits above 50% produce no reason. `ttl_or_provider_miss` is retained only for
legacy events; new calls never emit it.

A model switch is classified as `model_changed`, not provider eviction. The
fingerprint carries the provider-facing model precisely so this case does not
collapse into `unknown`.

### 6. Warm remains explicit and epoch-neutral

An explicit `session.cache.warm` request:

- sends only stable head sections and tool schemas;
- records its own input/output/cache-read/cache-write usage and cost;
- emits `context.cache` with `warm = true`, the epoch, and
  `cache_epoch_unchanged = true`;
- does **not** create or update a prompt-prefix observation, because a warm is
  not evidence that a session used the epoch;
- does not change the epoch.

`warm_on_resume` remains false by default. Nothing in L4 schedules a warm, and
a normal run still makes exactly one provider call per turn. Warm reports
`cache_epoch_reused_from_sessions` for context, but its own usage is not
cross-session reuse evidence.

### 7. Protocol and CLI surface

`session.cache` (`vp cache`) gains additive fields:

- `cache_epoch`, `cache_epoch_source`, session first/last seen, project
  first/last seen, session/call counts, observed input/cache-read tokens;
- `cache_epochs_seen`;
- `prefix_reused_from_sessions`, `cross_session_hit_tokens`,
  `cross_session_first_calls`;
- `provider_cache_misses`, `prefix_changes`;
- `compaction_cache_resets`, `compaction_lost_cache_usd`.

`context.prompt_prefix` and `context.cache` carry the finalized epoch and its
ordered core/project section references. `SessionCacheCall` carries the epoch,
reuse count, cross-session hit tokens, and first-call flag.

New read-only RPC `session.cache.epoch` (`vp cache-epoch <session> [--limit]`)
returns:

- the current epoch summary with project/session counts and this session's
  contribution;
- a bounded newest-first list of project epochs;
- a bounded chronological timeline of `prefix` and `compaction_reset` entries;
- a bounded `reused_sessions` list (session id, title, first/last seen, calls,
  input/cache-read tokens) for the current epoch;
- provider-miss, cross-session, and compaction totals;
- `generated_at`.

A second read-only RPC `project.cache_epochs` (`vp cache-epochs <project>`)
lists project-wide epochs, a tool-bundle bucket table (epochs, sessions, calls,
tokens, hit ratio), and project totals. It is useful before starting a session:
an operator can see which model/tool-bundle epochs have already paid for cache
writes. It also never calls a provider or mutates state.

Both limits are clamped to 1..200 (session RPC) and 1..100 (project RPC).

### 7a. Explicit retention

Epoch telemetry grows with every model/tool/project change and every session
that observes one. `project.cache_epochs.prune` (`vp cache-epochs-prune`) is an
explicit project maintenance operation:

- it always keeps the newest `keep_epochs` epochs (default 50, 1..1000);
- it removes epochs whose `last_seen` is older than `older_than_days` (default
  90, 1..3650);
- it protects any epoch with an observation attached to a session in
  `running`, `queued`, `paused`, or `waiting_input` status, even when older;
- `dry_run` reports the hypothetical remaining count without deleting rows;
- removing an observation is derived-telemetry deletion only. A later provider
  call recreates the row; no conversation, message, or worktree state is
  touched.

Nothing in the scheduler or daemon calls prune automatically. It is not an
automatic retention policy and it does not delete event history.

### 8. Replay, repair, and rebuild

`session.replay` derives this session's expected observations from its
`context.cache` events. Missing, extra, or drifted rows are reported as
mismatches. The aggregate rows for the session's epochs are checked against the
observation table; a missing aggregate row or a divergent aggregate is also a
mismatch.

`session.replay {repair:true}` replaces the session's observations from events
and recomputes all project aggregates from observations. Other sessions' rows
are preserved. `vp db-rebuild` restores each session's event mirror, then
repairs the same projection, so `prompt_prefixes` is rebuilt without a provider
call or a second agent run. Session export bundles carry the session's
observation rows; import restores them and recomputes project aggregates, so a
read-only archive reports the same epoch reuse before any replay. Older bundles
without the field decode as an empty list and remain replay-repairable. Legacy
sessions with no epoch-bearing cache events keep their projection rather than
being reported as drift.

### 9. Migration and compatibility

Both tables are created with `CREATE TABLE IF NOT EXISTS`; opening an existing
database adds them without a destructive migration. All new protocol fields are
`serde(default)` and decode as zero/`None`/empty on older payloads. Existing
`prefix_hash`, `cache_generation`, and compaction fields are unchanged.

L1 instruction keys and L3 bundle selection/hash behavior are untouched. The
epoch is derived from their existing hashes; it does not change the bytes sent
to a provider.

### 10. Limits and non-goals

- The epoch is a stable-byte identity, not a provider cache guarantee. A
  provider may evict or never cache the bytes.
- Cross-session reuse is claimed only from provider-reported usage on a
  first call. Later-call hits are not attributable without provider-side
  per-prefix telemetry.
- The runtime does not measure provider eviction time, TTL expiry, or cache
  capacity. `provider_cache_miss` is an observation, not a diagnosis.
- The 90% threshold is a safety heuristic, not a proof that a request fits.
  The hard limit and the provider's own context enforcement still apply.
- Compaction loss is estimated from configured prices. It is not an invoice,
  and unset prices produce a zero estimate rather than a fabricated cost.
- No automatic warm, no hidden model routing, no model-switching policy, and
  no gate changes.
- The aggregate is project-scoped and rebuilt from local events. It is not a
  cross-project or cross-host cache index, and it does not coordinate multiple
  daemons.
- Imported archive bundles are read-only historical events. Their epoch
  projection is repaired by replay/rebuild, not by a live session.
- No distributed lease, no remote cache attestation, and no cross-host
  scheduling are introduced.

## Consequences

Operators can now answer "is this the same stable prefix?", "did a local change
or the provider cause the miss?", "which other sessions used these bytes?", and
"what did the last compaction cost?" from persisted telemetry. A 1M-token
DeepSeek session keeps its append-only history until 90% of the window instead
of compacting at 75%. A project with a stable charter and tool bundle can show
cross-session reuse on a new session's first provider call. None of these
claims are made without persisted evidence.

The cost is additional SQLite projection work per provider call (one upsert and
one bounded aggregate recomputation), two small tables, and a new read-only
surface. The write path is transactional with the event, and the projection is
repairable from the event stream.

## Tests

- `vp-core` unit tests: epoch stability across prompt/memory/tail changes;
  epoch changes on model/tool/project/core; determinism and placeholder
  rejection; change reasons; cross-session attribution; low-hit threshold;
  event grouping; projection drift; aggregate math; bounded timeline.
- `vp-core` agent tests: two sessions share an epoch while `prefix_hash`
  differs; cross-session reuse from provider usage; provider-cache miss on a
  known epoch with zero reads; compaction epoch/reset/lost-estimate event;
  warm usage without epoch change or observation.
- `vp-core` integration: real Leader runs, replay detection and repair of a
  removed projection, `db-rebuild` restoration, second-session reuse report,
  bundle export/import, partial provider hits, audit findings, and explicit
  prune dry runs.
- `vp-store` tests: atomic event+observation append, cross-session counts,
  aggregate partitioning, replay replacement, drift detection, invalid
  epoch/session rejection, reopen persistence.
- `vp-protocol` tests: backward-compatible decode, status/call/epoch round trips, compaction epoch defaults, threshold policy fields, params defaults.
- CLI decode tests for `vp cache-epoch`, `vp cache-epochs`, and
  `vp cache-epochs-prune`.
