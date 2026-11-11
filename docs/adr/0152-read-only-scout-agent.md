# ADR-0152: Read-only Scout agent

- **Status:** Accepted
- **Date:** 2026-11-11
- **Owners:** Runtime, Protocol
- **Related:** ADR-0026, ADR-0039, ADR-0040, ADR-0049, ADR-0092

## Context

Long-running Leaders and Managers accumulate tool output, file excerpts, and
dead-end exploration in the same conversation that must preserve the user's
intent and the team plan. The efficiency playbook measured this shape in
OpenCode: roots carry most of the cost and most of the messages, while short
child sessions are read-heavy and cheap. The playbook's E2 workstream calls for
a context firewall: exploration in a fresh context whose bounded result returns
to the parent.

VerlinPark already has a read-only Verifier, but the Verifier is part of the
Worker → Verifier → Integrator gate. It owns a task, runs in an isolated
checkout, and produces a verdict. It cannot be reused as a general-purpose,
cheap exploration child: that would mix an approval gate with a research tool,
create task worktrees for questions, and pollute the merge queue.

The new path must not weaken any existing gate. It must not bypass the session
token budget, global Worker/Labour capacity, role caps, approval policy, shell
sandbox, write-scope registry, Verifier, or merge queue. It must be additive to
the protocol and honest in the event stream and agent roster.

## Decision

### 1. A structural `Scout` role

Add `AgentRole::Scout` with the stable agent/event role string `"scout"`. Proto
`AgentRecord.role` is already a string, so this is additive. `RoleModels` gains
a `scout` entry with `#[serde(default)]`; when it has no model, Scout uses the
configured base model. There is no hidden model selection.

Scout is not a fifth tier in the Leader → Manager → Worker → Labour chain.
`child_role()` returns `None`, `allow_delegation` is false, and the `scout`
tool is advertised to the Leader and to Managers that hold the child-spawning
permission. The read-only roundtable Managers intentionally run without
child-spawning tools. A Scout cannot call `scout`, so nested scouts are
impossible.

### 2. Read-only tool allowlist

Scout receives a frozen, small bundle built by the same `AgentRole::allows_tool`
policy used by Manager and Verifier:

- repository reads: `read_file`, `list_dir`, `read_files`, `find_files`,
  `file_info`, `grep`, `repo_map`, `find_symbol`, `find_references`,
  `dependency_graph`, `affected_tests`, `code_diagnostics`, `build_context`
- read-only Git: `git` (status/diff/log/show/blame/files only)
- source retrieval: `list_sources`, `search_sources`, `read_source`
- context: `recall`, `team_status`

The following are absent from the advertised schema, not merely denied at call
time: `shell`, `write_file`, `edit_file`, `apply_patch`, `file_manage`,
`run_project_task`, `process`, `wait_for`, `delegate`, `delegate_many`,
`delegate_acp`, `merge`-changing tools, `remember`, artifact publishing,
`ask_user`, `coordinate`, `use_skill`, `budget_status`, `task_control`,
`tool_search`, and `browser`. `mcp` is absent, and dynamic MCP tools are never
collected for Scout, so no MCP server can add a shell or write schema. There is
also an execution-time backstop: if a model hallucinates an unadvertised tool,
Scout receives a deterministic "not available to Scout" error before approval
or dispatch.

### 3. `scout` tool

Leader and Manager receive:

```json
{
  "objective": "string, 1..4000 chars",
  "files": ["optional path hints, max 20"],
  "max_steps": "integer 1..12, default 6"
}
```

The result is a bounded structured brief (`summary`), `agent_id`, `usage`, and
`cancelled`. The parent sees the Scout's final brief, not its raw reads; the
full Scout transcript remains in the event stream for audit. The tool
description states that Scout is read-only, fresh-context, not a coder, and
must return `file:line` evidence.

The child brief repeats the hard rules: do not modify files or run shell,
return the smallest useful evidence with `path:line` references, and say
explicitly when evidence is missing or a question cannot be answered with
read-only tools.

### 4. No worktree, commit, verification, or merge

Scout runs in the session worktree. It gets no task branch or task worktree,
never calls `commit_worker`, never enters the Verifier path, and never creates a
`merge_queue` row. As defense in depth, the runtime snapshots
`git status --porcelain --untracked-files=all` before the run and compares it
after the run. Any new tracked or untracked line fails the Scout and rejects its
result; pre-existing session changes are kept and are not reset. The check is
not a substitute for the tool policy, and it does not attempt to repair the
worktree.

### 5. Concurrency, budget, and cancellation

- A per-session-run cap of 3 concurrent Scouts (`SCOUT_CAP`). The slot is
  reserved before the agent row is created and is released on every return
  path.
- Scout provider calls go through the same `SessionBudget`, token reservation,
  cost reservation, provider usage ledger, and project/department scope
  settlement as every other agent. The parent's task/department is attached to
  the Scout usage so plan budgets are not bypassed.
- Scout accounts against the daemon-wide Worker capacity lane
  (`CapacityController`). A team cannot evade global capacity by spawning
  read-only assistants.
- `max_steps` bounds provider turns by cloning the agent config with a smaller
  `max_turns`; it does not change the parent's limits.
- Scout shares the parent's `RunControl`. Cancelling the session or parent run
  cancels the Scout. A cancelled Scout emits `scout.completed` with
  `cancelled: true` and propagates cancellation rather than returning findings.
- The Scout context strategy is `Trim` (provider-free rolling trim) and it
  starts from one user brief; it has no parent conversation.

### 6. Events and roster

Scout emits `agent.started`, normal turn/tool/message events with
`agent_role: "scout"`, `agent.finished`, and one terminal `scout.completed`
carrying `agent_id`, `parent_id`, status, a summary bounded to 6,000 chars, and
a usage object (calls, input/output/cache tokens, total, USD). It updates its
`AgentRecord` status and worktree (the session worktree) so `agent.list`,
`team_status`, replay, and the roster can represent it honestly. It has no
`task_id` unless the parent had one, and it never presents itself as a Worker.

## Consequences

- A Leader can ask "map the auth flow" and receive a small evidence brief with
  `path:line` references while the raw exploration stays out of the parent
  conversation.
- Scout usage is visible in the normal usage ledger and counts against session,
  project, and (when applicable) department budgets.
- The role is additive: older clients that only know the five existing role
  strings can still deserialize agent records; they may render an unknown role
  generically until the UI is updated.
- `RoleModels.scout` is additive and defaults to the base model, so no
  configuration is required for a first run.

## What this does not claim

- Scout is not a security sandbox. It is a policy and schema restriction plus
  a post-run status guard. The read tools still read the session worktree and
  project sources; the guard only detects new tracked/untracked lines.
- `code_diagnostics` is included because the playbook allowlists it, but it
  runs the project's detected check task under the shell sandbox rather than a
  pure parser. It can use the project shell policy's network setting, and
  ignored build outputs are not counted by the status guard.
- Scout has no network research by default: `fetch_url`, `web_search`,
  `local_http`, and MCP are not advertised. Project sources are the supported
  knowledge path.
- Scout cannot spawn nested Scouts, cannot delegate, and cannot be delegated to
  through `delegate`.
- The guard does not roll back a violation and does not protect against a
  concurrent writer in the same session worktree; it rejects a Scout result
  when a new status line appears during its run.
- No new distributed lock, worktree, merge ownership, or capacity controller is
  added. Scout uses the existing local runtime leases, write-scope registry
  (for Worker/Labour only), and Worker capacity lane.
