# ADR-0193: Task contracts and argument-level intent authorization

- **Status:** Accepted
- **Date:** 2026-11-12
- **Depends on:** ADR-0188 (content-addressed instruction store and prompt
  layout v3), ADR-0190 (frozen tool bundles), ADR-0191 (cache epochs and
  compaction)
- **Scope:** C1 of [CONTRACTED_EXECUTION_PLAN.md](../CONTRACTED_EXECUTION_PLAN.md)
  only. Effect receipts, external gates, version pinning, reliability metrics,
  and project-scoped agent identities are explicitly not part of this ADR.

## Context

VerlinPark already checks *which tool* may run and applies a risk-based
approval broker, a Seatbelt write sandbox, a path jail, task write scopes,
Verifier review, and a deterministic Integrator. The remaining gap is
**argument-level intent**: a tool can be allowed, the path can be inside the
worktree, and the arguments can still contradict the task the Leader actually
declared.

Examples:

- a Worker declares `write_scope = ["crates/vp-core"]` and writes
  `docs/secret-plans.md`;
- a research task has no egress permission and calls `fetch_url` to an
  arbitrary host;
- a coding shell command runs `git push`, `sudo`, or writes outside the task
  scope;
- an MCP server silently changes a tool's input schema or description between
  runs, so the model calls a different tool than the one that was reviewed.

The runtime also needed the model to see the same authority the runtime
enforces. Compaction can discard conversation history; a constraint that exists
only in a summary is soft governance and decays.

## Decision

### 1. Canonical `TaskContract`

`vp-core::contract::TaskContract` is the canonical authority object:

```text
contract_id     deterministic from trusted-source hash
project_id, session_id, task_id, plan_revision
role, profile
allowed_tools   role/profile policy surface, names only
allowed_paths   normalized worktree-relative write scopes
forbidden_paths project policy denials
egress          deny | allow | allowlist(hosts)
mutation_budget optional max_writes / max_bytes / max_commits
acceptance      Leader-authored plan task acceptance strings
source_hash     hash of trusted inputs only
hash            sha256(canonical contract body)
created_at
unrestricted_scope
```

Derivation is a pure function of trusted inputs:

1. the stored, Leader-authored `PlanTask` (`write_scope`, `acceptance`,
   `kind`, `profile`);
2. the session goal;
3. the role/profile policy surface (`role_tool_surface`, not merely the
   advertised bundle);
4. the project `ContractPolicy` and `ShellPolicy.network`.

Tool output, project sources, retrieved chunks, MCP descriptions, shell output,
and model prose are never inputs. A task-scoped retry/re-verification that
outlives the stored plan uses the runtime-built `AgentSpec.write_scope` only
when that scope is non-empty. If it is empty, no contract is fabricated: the
existing role policy, sandbox, worktree, Verifier, and Integrator gates still
apply. A missing plan never becomes an unrestricted session contract.

Empty `write_scope` normalizes to `allowed_paths = ["."]` with
`unrestricted_scope = true`. This preserves today's single-run barrier without
inventing a narrower scope the Leader never declared.

`source_hash` and `hash` use canonical JSON with recursively sorted keys. The
canonical body excludes `created_at` and the hash itself, so deriving the same
trusted inputs twice is byte-identical. `contract_id` is deterministic from
`source_hash`. The first successful derivation pins `created_at`; later
identical derivations reuse the stored row.

### 2. Enforcement and escalation

`authorize(contract, policy, call)` returns one of:

- `Allow` — the call is inside the contract;
- `Escalate { code, reason, rule }` — a human/template decision is required
  through the existing approval broker;
- `Deny { code, reason, rule }` — the runtime does not execute the call and
  does not prompt.

Mapping is deliberately conservative:

| Condition | Decision | Risk |
|---|---|---|
| Role/tool outside contract | `Deny CONTRACT_TOOL_DENIED` | existing |
| Forbidden path | `Deny CONTRACT_PATH_FORBIDDEN` | existing |
| Explicit egress `network=deny` | `Deny CONTRACT_EGRESS_DENIED` | HostState |
| Path outside `allowed_paths` | `Escalate CONTRACT_PATH_VIOLATION` | HostState |
| URL/host outside egress/allowlist | `Escalate CONTRACT_EGRESS_VIOLATION` | HostState |
| Mutation budget would be exceeded | `Escalate CONTRACT_MUTATION_BUDGET` | HostState |
| Shell argument contradicts contract, `enforce_shell=true` | `Escalate CONTRACT_SHELL_AMBIGUOUS` | HostState |
| Destructive/privileged shell command | `Escalate CONTRACT_SHELL_AMBIGUOUS` | Destructive |
| MCP pin unverified/drift/removed | `Escalate CONTRACT_MCP_UNTRUSTED` | HostState |
| Unknown/unparseable arguments | `Escalate CONTRACT_AMBIGUOUS` | HostState |

`ContractPolicy` fields:

```text
enabled        default true
mode           advisory | enforce (default enforce)
enforce_shell  default false
forbidden_paths, egress_allowlist
max_writes / max_bytes / max_commits (default None)
```

In `advisory` mode the raw decision is still recorded on `intent.decision` with
`advisory=true` and `enforced=false`, but the call continues with the existing
risk assessment. `enforce_shell=false` does the same for shell argument hints
only; the existing shell classifier still prompts for host-state/destructive
commands and `network=deny` is still a contract denial.

A contract can only narrow. The existing approval broker remains the only
execution path. A contract escalation raises the assessment to `HostState` (or
keeps `Destructive`), so `smart`/`strict` ask and `unattended` denies. Approval
templates never auto-approve Destructive, HostState, remote, MCP, sensitive GUI,
or unknown-risk requests, so the contract cannot become an automatic approval.

Path checks are applied to mutation tools (`write_file`, `edit_file`,
`apply_patch`, `file_manage`, remote writes, custom project tasks, and unknown
workspace-write tools). Read-only reads keep their existing path-jail and role
policy; the task `write_scope` is not a read scope, and treating it as one would
break ordinary cross-module inspection without adding authority safety.
`apply_patch` also checks unified-diff and `*** Add/Update/Delete File:` headers.

The shell check is bounded and syntactic. It detects obvious write targets,
redirections, `git push`, remote tools, network fetch, privilege escalation,
disk/system commands, nested `sh -c`/`eval` one level deep, and shell
expansion. It does not claim to parse shell or prove arbitrary command safety.

Mutation delta is estimated before execution and persisted only after the call
is allowed and before it runs. `intent.mutation` events are projected into
`contract_usage`. A denied call does not consume budget. A crash between the
event and execution is conservative: the attempt is counted.

### 3. MCP schema pins

`mcp_tool_pins(project_id, server_id, tool_name)` stores:

```text
server_name, server_fingerprint
schema_hash, description_hash
observed_schema_hash, observed_description_hash,
observed_server_fingerprint
status: pinned | drift | unverified | removed
approved_at, approved_by, last_seen_at
bounded redacted description_preview
```

Hashes are canonical SHA-256 over tool name, input schema, optional output
schema, and description. `server_fingerprint` commits to transport, command,
args, URL, and the *names* of configured env/header entries. Values are never
stored or hashed.

Semantics:

- **TOFU.** First discovery for a project/server pins every discovered tool
  (`approved_by = "tofu"`) and emits `mcp.tool.pinned` with `source=tofu`.
- **New tool.** A tool added later is `unverified`, not advertised or
  executable, and emits `mcp.tool.unverified`.
- **Drift.** A changed schema, description, or server fingerprint moves the
  pin to `drift`, emits `mcp.tool.drift` with old/new hashes and a bounded
  description diff, and never overwrites the approved hashes.
- **Removed.** A disappeared tool becomes `removed` and emits
  `mcp.tool.removed` (informational).
- **Approve/reject.** `mcp.pin.approve` requires the expected observed hashes;
  mismatch fails closed. Reject leaves the visible status unchanged and emits
  `mcp.tool.rejected`.

Discovery (`collect_dynamic_mcp_tools`) advertises and executes only
`pinned` rows whose observed hashes still match. `tool_search` may list
unverified/drift/removed tools with `trust` and `callable=false`. Generic
`mcp action=call` resolves the same pin trust before the approval broker.
No env values, headers, or secrets enter pins, events, CLI, or Swift.

### 4. Prompt capsule and cache identity

`InstructionSectionKey::SessionContract` is a **head** section at ordinal 55,
key `session/contract`. This is a deliberate deviation from the plan draft,
which suggested `tail/contract`:

- compaction must not drop governance constraints;
- an append-only conversation can retain a head section across compaction,
  while a tail insertion can move or be reconstructed inconsistently;
- a contract change is a real authority change and should be a visible,
  priced prefix change rather than silent churn.

The section is content-addressed through the L1 instruction store and is
regenerated (semi-stable), not pinned to the first contract. The capsule is
deterministic, bounded to 8 KiB, contains role/task, allowed/forbidden paths,
egress, acceptance, mutation budget, and the sentence "Runtime gates remain
authoritative". It contains no goal prose, tool output, retrieved content, or
secrets.

`context.prompt_prefix` records `contract_hash`, `contract_chars`,
`contract_capsule`, and `contract_policy_enabled`. `context.compacted` records
the same contract identity. Cache audit emits `contract_capsule_missing` when a
v3 prefix or compaction boundary claims contracts are enabled but carries no
contract hash.

Leader and Manager receive a session-scoped read-only contract (paths are
unrestricted because there is no task write scope, but their role surfaces are
the actual enforcement). Worker and Labour receive the task-scoped contract.
Verifier and Scout receive task/session-scoped read surfaces.

### 5. Persistence and replay

- `project_contract_policies` stores the policy; the project manifest includes
  it for `db-rebuild`.
- `contracts` stores the canonical JSON row; `contract.derived` is authoritative
  and replay/repair rebuilds the projection.
- `contract_usage` is derived from `intent.mutation` events.
- `mcp_tool_pins` is project configuration stored in the manifest and replaced
  on `db-rebuild`. Pin lifecycle events are session-scoped audit records.
- All new protocol fields are `serde(default)`; older events and manifests
  decode with the documented defaults.

### 6. RPC/CLI/UI

- `session.contract`, `project.contract_policy.get/set`,
  `project.mcp_pins`, `mcp.pin.approve`, `mcp.pin.reject`.
- `vp contract`, `vp contract-policy`, `vp contract-policy-set`,
  `vp mcp-pins`, `vp mcp-pin-approve`, `vp mcp-pin-reject`.
- Swift Stage/cache inspector shows contract hash, role, task, path count,
  acceptance count, egress, and enforcement mode. It never renders contract
  text, arguments, or secrets.

## Rollout and migration

- `ContractPolicy` defaults to enabled/enforce for new and existing projects.
  An operator can set `mode=advisory` or `enabled=false` for a project without
  changing any existing approval, sandbox, Verifier, or merge gate.
- Legacy sessions have no `contract.derived` events. Replay preserves their
  projection rather than reporting drift; the next run derives a contract.
- Legacy MCP servers have no pins. First discovery is explicit TOFU and emits
  `mcp.tool.pinned`; operators can review the resulting rows before the next
  run.
- Existing tool bundles, provider routing, budgets, and cache layout v2
  sessions are unchanged. Layout v2 does not receive the contract capsule; a
  session must opt into layout v3 to get the head section.

## Limits and non-goals

- The shell analyzer is bounded and syntactic; it is not a full shell parser
  and does not prove safety. Shell writes are not counted as mutation writes or
  bytes; only `git commit` is counted as a commit. Write-tool estimates use the
  declared content/new-string/patch length.
- Read-only path access is not limited by `write_scope`; the existing path jail
  and role policy still apply.
- A contract is derived per run/role; there is no immutable agent identity,
  cross-session contract, or project org chart. Project-scoped identities remain
  a separate program.
- MCP pin approval is operator action, not a cryptographic server attestation.
  A malicious server can still change behavior for a pinned tool without
  changing its advertised schema; this module detects schema/description
  drift, not runtime semantics.
- No effect receipts, exactly-once claims, external gates, version pinning,
  reliability metrics, Code Mode, or automatic approval are added.
- `contract_usage` counts attempted allowed mutations; it is not a billing
  ledger and does not rewind on a tool-internal failure.
- Deterministic internal repair/integration runs use the Manager role with
  `auto_approve` and are explicitly contract-exempt. They are constructed by
  trusted runtime code, not delegated by a model, and retain the normal
  sandbox, write-scope, Verifier, and Integrator gates. Ordinary Managers are
  not exempt.
