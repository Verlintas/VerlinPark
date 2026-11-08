# ADR-0119: Risk-based approval, compact transcript, and orchestration-only Collab

- Status: Accepted
- Date: 2026-11-08
- Supersedes: the "ask before every write/shell call" behavior introduced with the first approval broker

## Context

The first approval broker treated every `write_file`, `edit_file`, and `shell`
call as equally dangerous. That was safe as a prototype, but it made ordinary
agent work unusable: every small edit or test command stopped the run and asked
for a human decision. The same pattern also made the Stage crowded with
tool-call cards and made Collab look like a second plan viewer instead of an
org status surface.

The runtime still needs a real permission boundary:

- Shell runs under a macOS Seatbelt write sandbox confined to the worktree,
  Git metadata, and runtime scratch.
- File tools enforce a canonical worktree path jail and refuse symlink writes.
- Provider credentials never enter the event store or repository.
- The approval broker remains the only path through which an agent can run an
  elevated tool.

The missing piece was a deterministic policy between the broker and the tool
call: routine work should proceed, while host-level operations should still
interrupt or be refused.

## Decision

### 1. Three explicit approval modes

`ApprovalMode` is a protocol enum:

| Mode | Routine workspace write | Ordinary shell | External action | Host-state change | Destructive host action |
|---|---|---|---|---|---|
| `smart` (default) | allow | allow | allow | ask | ask |
| `strict` | ask | ask | ask | ask | ask |
| `unattended` | allow | allow | allow | deny | deny |

`smart` is the default. The app persists the selection in
`verlinpark.approval.mode`, sends it through `agent.configure`, and shows it in
Settings → General. The daemon also accepts
`VERLINPARK_APPROVAL_MODE=smart|strict|unattended` when no client supplies a
value. The daemon remains the authority; the UI only explains the choice.

Internal runs marked `auto_approve` (conflict resolver, verification helpers)
use the `smart` classifier but cannot block on a human. A host-level request is
denied instead of waiting. This is deliberately stricter than the old
"auto-approve everything" flag.

### 2. Deterministic risk classification

`crates/vp-core/src/permissions.rs` classifies tool calls without invoking a
model:

- Read-only tools, plan/todo/memory/mailbox operations, and delegation are
  `read_only`.
- File writes with a relative, non-traversing path are `workspace_write`.
- Shell commands are split into simple commands and classified by program,
  arguments, working directory, redirections, and command substitution.
- MCP and unknown externally supplied tools are `external`.
- Host services, process control, package managers that write outside the
  worktree, and daemon-mediated tools are `host_state`.
- System power/disk/process operations and writes to critical system paths are
  `destructive`.
- A small never-allow set is `blocked`: privilege escalation, `rm` of `/`,
  raw-device writes, `chroot`, and fork bombs. These are denied even in
  `strict` mode.

The classifier tracks `cd` across a command chain, recursively scans
`sh -c`, `eval`, interpreter `-e/-c`, `find -exec`, and `xargs`, and scans
double-quoted command substitutions. It preserves the original worktree root
even when the logical shell directory changes. `xargs` with no explicit target
and a destructive program is treated as host-state because its paths come from
stdin.

ACP permission requests use the same policy. `read` requests are read-only;
edits with an inspectable path use the path jail classification; execute
requests use the shell classifier. An unknown ACP permission is host-state and
asks in `smart` mode.

### 3. Approval records carry risk and reason

`Approval` now has optional `approval_risk` and `approval_reason` fields. They
are persisted in the `approvals` table with an additive migration, included in
`approval.requested`, carried through `session.replay`/repair/bundles, and
shown in the Stage and Manage Dash approval surfaces. Events also record
`permission.allowed` for auto-allowed external actions and `permission.denied`
for policy refusals, so the decision trail is inspectable without replaying the
classifier.

### 4. Transcript is a projection, not view logic

`TranscriptProjection` turns the event stream into stable rows. Tool calls are
rendered as one compact line with a tool icon, name, argument summary, agent,
duration, and status. Clicking the line expands a flat detail block. Tool calls
are no longer cards. Running tools appear immediately and resolve in place when
`tool.finished` arrives. Approval rows are flat and rare; they show the risk,
reason, and only the decision buttons.

The projection is pure Swift and covered by tests. It does not infer business
state and does not call the daemon.

### 5. Collab shows org status only

`CollabSnapshot` groups agents by their actual `parent_id`, derives active
counts and plan progress, and exposes only:

- Leader and current goal.
- Managers with their workers.
- Unassigned workers, verifiers, and labour.
- A single plan-progress strip.

Plan task lists, roundtable transcripts, leader todo details, and computer-lease
design notes are not rendered in Collab. Those belong to Preview, the Stage, or
the relevant policy card.

## Consequences

- Routine edits and tests no longer block; host-level commands still ask.
- The policy is deterministic and testable, but it is a classifier, not a proof.
  An arbitrary interpreter or build script can still perform surprising work
  inside the sandbox; the Seatbelt boundary and post-run guards remain the
  defense in depth.
- `strict` restores the old prompt-heavy behavior for users who want it.
- `unattended` is suitable for explicitly authorized background work but will
  fail host-level operations by design.
- Approvals are rarer, so the approval center and inbox become high-signal
  surfaces rather than a constant queue.
- Adding a new tool requires classifying it in `assess_tool_call`; unknown tools
  default to `external`, which asks in `strict` and runs in `smart`.

## Tests

- Rust: policy matrix, shell splitting, `cd`/`eval`/`xargs`/redirection,
  command substitution, privilege escalation, root deletion, download-to-shell,
  MCP/ACP classification, approval persistence/migration, and smart-mode
  integration tests.
- Swift: transcript running/finished tools, approval resolution, tool summaries,
  Collab grouping/counters, and approval-mode compatibility.
