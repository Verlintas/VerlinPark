# ADR-0093: ACP external-agent bridge

- **Status:** Accepted
- **Date:** 2026-11-07

## Context

VerlinPark's four-tier team is composed of agents that use VerlinPark's own
provider configuration. Users also run specialist coding agents (for example
Gemini CLI or Claude Code) that expose the Agent Client Protocol (ACP) over
stdio. Reimplementing those agents inside VerlinPark is out of scope, and
launching them outside the worktree/verification pipeline would bypass the
write isolation and integration invariants.

## Decision

1. **ACP client only.** VerlinPark is the ACP *client*. A project-independent
   registry stores external agents (`acp_agents`): name, command, args, env,
   enabled, auto-approve, and a per-delegation timeout. The registry is managed
   through `acp.agent.*` RPC methods, `vp acp-*` commands, and Settings > ACP.
2. **Delegation creates a Worker.** `delegate_acp` is available to Leaders and
   Managers. It reserves a Worker slot, creates an isolated task worktree from
   the session branch, records a normal Worker agent with model `acp:<name>`,
   and acquires the daemon-wide Worker capacity permit and write lease before
   launching the process.
3. **Process isolation and scratch HOME.** The agent is spawned through the
   same Seatbelt write sandbox as shell work: writes are confined to the task
   worktree, Git metadata, and a per-agent scratch directory. `HOME`, XDG
   config/data/cache, and TMPDIR are redirected into that scratch directory, so
   ambient home credentials and state are not read or written. Credentials must
   be configured explicitly in the agent env map.
4. **Protocol flow.** The bridge performs `initialize`, `session/new` (with the
   configured stdio MCP servers), and `session/prompt`. `session/update`
   notifications are normalized into `agent.message_delta`,
   `agent.thought_delta`, and `tool.*` events. `session/request_permission` is
   answered through the ordinary approval queue: `allow_once` is preferred for
   approvals, `reject_once` for denials, and cancellation is propagated.
5. **Completion uses the normal Worker pipeline.** `end_turn` commits the task
   worktree (including write-scope validation), runs the Verifier on the exact
   commit in its isolated checkout, and enters the persisted merge queue and
   deterministic Integrator. Non-`end_turn` stop reasons, timeouts, protocol
   errors, and cancellation fail or cancel the Worker instead of integrating.
6. **Ephemeral sessions.** ACP sessions are not resumed or persisted across
   daemon restarts; a retry starts a new external session in the existing task
   worktree. Token usage from the external agent is not added to VerlinPark's
   provider usage ledger unless the agent reports it through a future mapping.

## Consequences

- External agents become first-class Workers whose branch, Verifier approval,
  merge queue entry, events, and UI state are inspectable like native Workers.
- The external executable is trusted user software. It runs with the daemon
  user's privileges and, under the default shell policy, may read arbitrary
  non-denied files and use the network. This is not a network/credential
  sandbox beyond the existing shell policy and Seatbelt write confinement.
- The agent must authenticate through environment variables or its own scratch
  state; existing `~/.claude`, `~/.config/gemini`, and similar home state is not
  visible because HOME is redirected.
- `acp.agent.probe` initializes the agent and creates a session without sending a
  prompt, so it never modifies the worktree.
- Only stdio transport is implemented. HTTP/SSE ACP transports and ACP
  `fs/*` or `terminal/*` client capabilities are not advertised.

## Alternatives considered

- **Run ACP agents outside the worktree:** would bypass write leases,
  verification, and integration.
- **Treat ACP output as a provider stream:** ACP is session-oriented and the
  agent performs its own tool calls; a provider adapter would lose permissions
  and tool events.
- **Inherit the real HOME:** many CLI agents would work out of the box, but they
  would expose and mutate ambient credentials/state, contradicting the sandbox
  write boundary.
