# ADR-0132: Bounded text diff, artifact comparison, and citation audit

- **Status:** Accepted
- **Date:** 2026-11-10
- **Owners:** Runtime, Artifacts, Knowledge, macOS UI
- **Related:** ADR-0129, ADR-0130, ADR-0131

## Context

Artifact version history without a diff is only a list of snapshots. A reviewer
needs to see what changed between two inline reports, and the daemon must stay
bounded when an agent produces a large or adversarial edit. The same trust gap
exists for citations: the event log stores `[cN]` evidence, but a model can cite
a missing key or attach evidence it never references. Neither problem should
require another model call.

## Decision

### 1. One deterministic diff engine for text

`vp-core::textdiff` implements a line diff with:

- CRLF/CR normalization;
- Myers O(ND) with a bounded edit distance (`MAX_EDIT_DISTANCE = 512`);
- a bounded output (`DEFAULT_MAX_DIFF_LINES = 4,000`, hard cap 20,000);
- an explicit delete-all/insert-all fallback when the edit cap is exceeded;
- structured hunks with old/new line numbers and context;
- honest `additions`, `deletions`, `unchanged`, `identical`, and `truncated`
  flags.

The output budget truncates rendered lines, not the summary counts. The engine
never shells out to `git diff` and never calls a model, so the result is stable
across environments and testable without a provider.

### 2. Artifact comparison is a versioned RPC

`artifact.diff` compares:

- one stored inline version to another stored version; or
- one stored version to the current artifact when `to_version` is omitted.

File-backed artifacts are rejected: their content lives in a compressed blob and
participates in content-addressed GC. Pretending to diff them through an
unbounded materialization would be misleading. `vp artifact-diff` prints a
unified-style view; Preview history rows expose a Diff action and a side-by-side
line-number sheet.

### 3. Citation audit is derived from events

`session.citation_audit` walks the same `agent.message` events used by
`session.citations` and reports:

- messages with and without citation evidence;
- total citation records and distinct sources;
- `[cN]` keys referenced in prose but absent from the message evidence
  (`dangling`);
- citation records the message never referenced (`unused`);
- a bounded excerpt per message.

Markers are extracted with a small ASCII scanner rather than a regex. The parser
is intentionally strict: `[c1]`/`[C1]` match, malformed brackets are ignored, and
no fuzzy key rewriting occurs. The audit is a review aid, not a truth claim: a
message can have citations and still make an unsupported statement, and a
summary without markers is reported as uncited rather than silently trusted.

### 4. Source organization and scoped retrieval

Sources gained a nullable collection and label filters:

- `source.collections` returns aggregate counts and distinct labels;
- `source.update` replaces title/collection/labels in one metadata update and
  keeps the durable snapshot in sync;
- `source.search` accepts collection and label filters that are applied to both
  the FTS path and the literal fallback; CJK and mixed-script queries fuse FTS
  and literal candidates instead of discarding one path;
- `search_sources` exposes the same filters to agents.

An empty collection filter list means all collections. An empty string selects
uncollected sources. Collections are organizational labels, not authorization
boundaries.

### 5. Skill activations are events, not a separate table

A successful `use_skill` call emits `skill.activated` with the skill name,
version, role/profile, tool policy, and task id. `skill.activations` derives the
session history from those events. This keeps replay, bundle export, and the
Settings → Skills history consistent with the rest of the runtime.

### 6. Context limits count the request once

The compaction path compares `messages + system + tools` with the hard limit.
`context_limits` now reserves only the model output budget and a small safety
margin; it no longer subtracts the system/tool estimate a second time. A manual
compaction whose summary fails trims to half of the hard limit so the explicit
action still creates headroom. This fixes a latent double-count exposed by the
larger source/skill tool schemas.

## Consequences

- Reviewers can inspect inline artifact changes without leaving Preview.
- Large diffs are bounded and explicitly marked truncated.
- Citation problems are visible instead of being inferred from prose.
- Collection/label filters work identically for agents, CLI, and UI.
- Skill activation history is replayable without a new projection.
- The context-limit fix gives larger models more usable message budget and keeps
  the 8k test window within its output reserve.
- Binary artifact diff, semantic citation verification, and embedding retrieval
  remain out of scope.

## Alternatives considered

- **Shell out to `git diff --no-index`.** Rejected: temporary files, platform
  differences, and no structured bounded output.
- **Full O(n*m) LCS.** Rejected: a 100 KB artifact with many lines can allocate
  hundreds of megabytes.
- **Store an audit projection table.** Rejected: a second source of truth that
  can drift from `agent.message`; derive on read.
- **Treat any `[cN]` as valid if any citation exists.** Rejected: hides dangling
  keys and unused evidence.
- **Keep the double-counted context limit and grow the test window.** Rejected:
  the original behavior silently penalized every large system/tool prompt.
