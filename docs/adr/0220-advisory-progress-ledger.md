# 0220 — Advisory progress ledger

- Status: Accepted
- Date: 2026-11-16
- Scope: C12 bounded Manager progress evidence

## Context

Long-running Manager loops can repeat the same diagnosis without advancing the
plan. Existing stall detection is based on turn/tool progress timestamps, not
on what the Manager believes is satisfied, looping, blocked, or next. Letting a
model's prose drive retries or DAG edits would violate the deterministic
controller and could amplify false confidence after compaction.

C12 adds a **bounded, advisory progress ledger**. A Manager may report one
round after a planning/review pass. The runtime stores it as evidence and
derives a deterministic assessment. It never selects, claims, skips, retries,
cancels, replans, merges, or changes budgets/capacity from the ledger.

## Decision

### Wire model and bounds

Shared serde types live in `vp-protocol/src/progress.rs`:

- `ProgressRound`: `round_id`, agent/task identity, `satisfied`, `in_loop`,
  `progressing`, `blockers`, `next_task`, `instruction`, `evidence`, timestamp.
- `ProgressEvidence`: `task|test|commit|artifact|event|note`, bounded reference,
  optional note.
- `ProgressAssessment`: deterministic status/reason/counts plus
  `advisory_only=true`.
- `ProgressStatus`: `progressing|in_loop|stalled|blocked|completed|
  insufficient_evidence`.

Bounds: 8 items per list, 8 evidence pointers, 200 characters per item/reference,
1,000 characters for next task/instruction, 256 rounds max, 32 retained by
default, 1,000-character control-free fields. Validation rejects empty rounds,
control characters, absolute paths in identifiers, URL evidence references, and
obvious credential-key assignments. Screening is key-shape based, not a secret
classifier.

### Round sequence

`round_id` must end in a positive decimal sequence (`1`, `r1`, `manager-round-2`).
Sequences start at 1 and increase by exactly one. Missing ids are assigned by
`ProgressLedger::next_round_id`. Duplicate, out-of-order, and replayed rounds
fail closed without wall-clock dependence.

### Deterministic assessment

`vp-core/src/progress_ledger.rs` is pure: no DB, provider, tool, filesystem, or
network access. It normalizes rounds, retains a bounded prefix, and derives:

- `insufficient_evidence` for empty rounds or no evidence;
- `blocked` for latest explicit blockers;
- `in_loop` for a repeated in-loop item at/over the configured streak or an
  in-loop-only round without advance;
- `stalled` when no new satisfied/progressing item appears for the stall window;
- `progressing` otherwise.

`next_task` and `instruction` are copied suggestions, never applied. The
assessment always carries `advisory_only=true`. It is not an authorization
token and cannot satisfy a Verifier, contract, gate, or merge check.

### Persistence and surfaces

- `progress_rounds` is a session-scoped SQLite projection: plan revision,
  round id, agent/task identity, bounded lists/blockers/evidence, created time.
- `store.insert_progress_round_with_event` writes the projection and
  `progress.round` event in one transaction and mirrors the event. The event
  payload carries the plan revision and normalized round; the assessment is
  derived on read.
- `store.list_progress_rounds`, `latest_progress_revision`,
  `replace_progress_rounds`, and `list_progress_rows` are bounded.
- `session.replay` compares event-derived rounds with the projection; repair
  replaces the projection from events. `session.progress` /
  `session.progress.report` expose read/report RPCs.
- The Leader/Manager `report_progress` tool fills agent/task identity, accepts
  only bounded fields, and returns the resulting assessment. It is read-only
  risk: it records evidence but never mutates plan/task/merge state.
- `vp progress` reads a ledger; `vp progress-report --payload` reports one
  round. The SwiftUI `ProgressCard` shows status, counts, loops, blockers,
  evidence, and suggestions with an advisory-only banner.

### Security and non-claims

- No hidden model routing: the tool does not choose a model or policy.
- No automatic retry/replan: an assessment is an observation; the existing
  deterministic scheduler/orchestrator remains authoritative.
- No secret storage: evidence is a bounded pointer, never file or prompt
  content. The payload guard is key-shape based, not cryptographic.
- No cross-session or cross-project aggregation in this version.
- No event erasure: progress rows/events follow normal session retention.

## Limits

- The ledger is a model self-report, not ground truth. It can be incomplete or
  wrong; the assessment is conservative and advisory.
- The in-loop/stall rules are deterministic heuristics over bounded text, not
  semantic comparison.
- Only Leader/Manager may report; Workers/Labour/Verifier/Scout cannot.
- The projection is same-host SQLite; imported archives are read-only and
  cannot be reported into. `progress.round` event replay/repair is covered for
  normal sessions.
- No UI editing of rounds, no cross-session trends, no automatic pruning table
  beyond the per-ledger round cap.

## Verification

- `cargo test -p vp-core --test progress_ledger` — pure validation/assessment.
- `cargo test -p vp-core --test progress_session` — engine/store/event path.
- `cargo test -p vp-core --test progress_replay` — replay/repair path.
- `cargo test -p vp-store --lib` — projection CRUD/order/bounds.
- Full `cargo test --workspace`, fmt and clippy.

## Related

- ADR-0198 failure attribution (advisory evidence)
- ADR-0140 stall recovery
- ADR-0139 steering and progress
- `docs/CONTRACTED_EXECUTION_PLAN.md` C12
