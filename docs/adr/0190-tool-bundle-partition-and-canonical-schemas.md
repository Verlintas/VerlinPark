# ADR-0190: Tool bundle partition, freeze, and canonical schemas

- **Status:** Accepted
- **Date:** 2026-11-12
- **Depends on:** ADR-0134 (scoped shell sandbox), ADR-0137 (role tool
  policy), ADR-0170 (tool error contract), ADR-0188 (instruction store and
  prompt layout v3)
- **Supersedes:** the role-wide tool advertisement described in ADR-0137

## Context

The stable tool-schema prefix is the largest fixed block in an agent prompt.
Before this change every Leader received the whole role-wide registry plus
every special tool (72 schemas, roughly 12–13k tokens), and every Worker
received 64 schemas (roughly 11.8k tokens). Most of those schemas cannot be
used by the task at hand:

- a one-file typo fix does not need delegation, team planning, artifacts,
  source retrieval, document readers, media tools, remote workspaces, or GUI
  control;
- a Verifier must not be tempted by write, edit, commit, merge, or delegation
  tools;
- a research task needs retrieval and web tools, not shell and workspace
  writes;
- a Leader needs orchestration, but a coding profile is the only Leader that
  should receive browser and native computer schemas.

The cache-lossless plan (L3, §3.4) makes the schema prefix part of the stable
cache epoch. The prefix is only useful if it is small, deterministic, frozen
for the run, and identical across sessions with the same inputs. Two sessions
with the same role/profile/task kind must produce byte-identical tool JSON and
the same `tools_hash`.

This ADR defines the partition matrix, the task-kind mapping, the freeze
rules, the canonical serialization, the size targets, the fallback, and the
security boundary. It does not change the prompt layout (ADR-0188 owns that),
the append-only conversation rules (ADR-0189), compaction/epoch policy
(ADR-0191), or cache telemetry aggregation (the L5 workstream).

## Decision

### 1. Bundles are explicit allowlists

`ToolBundleKind` has six values:

| Bundle | Purpose | Built-in target |
|---|---|---:|
| `leader` | Leader/Manager orchestration, planning, files, shell, Git/code, memory, artifacts, questions, team, task control | ≤ 48 |
| `coding` | Worker/Labour implementation, tests, code intelligence, coordination | ≤ 40 |
| `research` | Read-only exploration, web/source retrieval, memory, code mapping | none |
| `verification` | Read-only inspection, shell/tests, diagnostics, verdict | none |
| `minimal` | Trivial one-file read/write/shell task | ≤ 12 |
| `full` | Legacy fallback when no deterministic bundle applies | none |

Each partitioned bundle is an explicit `&'static [&'static str]` allowlist.
There is no "all tools minus a few" subtraction. A name that is not in the
allowlist is not advertised, even if the role policy would allow it.

The static catalog in `vp-core/src/tool_bundle.rs` assigns every bundled tool
a category, a `workspace_write` flag, and a `delegation` flag. The audit
rejects duplicate names, unknown names, target overruns, and write/delegation
leaks into read-only role bundles. The matrix is test-gated; adding a tool to
the registry does not add it to a bundle automatically.

### 2. Bundle matrix

The requested name sets are role/profile adjusted. All lists are sorted and
deduplicated before freezing.

**Leader (46 built-ins, 48 with a coding profile):** read/list/grep,
edit/write/apply-patch, find/info/read-files, shell/process, Git,
repo-map/find-symbol/find-references/code-diagnostics/build-context/LSP,
fetch-url/web-search, recall/remember, list/search/read sources,
send-message/read-mailbox/publish-artifact/read-artifact,
ask-user/coordinate/team-status/use-skill/MCP,
todo-write/delegate/delegate-ACP/submit-team-plan/run-team-plan/deliberate-team-plan/scout,
task-control, budget-status, read-tool-output, tool-help, tool-search.
A coding profile adds `browser` and `computer`, exactly reaching the 48-tool
target. A generic Leader does not receive GUI schemas.

**Manager:** the Leader request list is filtered to the Manager role policy
and read-only pruning. Writes, shell, process, delegation, scout, browser, and
computer are not requested. Managers keep read/search/Git/code intelligence,
mailbox, team status, task control, budget, output paging, and tool discovery.

**Coding (33 base, 37 for a coding Worker, 36 for coding Labour):**
read/list/grep, edit/write/apply-patch, find/info/read-files, shell/process/
wait-for, Git, repo-map/find-symbol/find-references/code-diagnostics/
dependency-graph/build-context/LSP, run-project-task, fetch-url,
recall/remember, send-message/read-mailbox/publish-artifact/read-artifact,
coordinate/use-skill/MCP, read-tool-output/tool-help/tool-search.
`delegate` is appended only for a Worker, never for Labour. A coding profile
adds `browser` and `computer`. The result stays under 40.

**Research (33 requested for a research Worker):** read/list/grep,
find/info/read-files, Git, repo-map/find-symbol/find-references/
dependency-graph/build-context/affected-tests, fetch-url/web-search,
list/search/read sources, recall/remember, PDF/image/document readers,
archive-list, media-info, data-query, use-skill, read-tool-output/tool-help/
tool-search, team-status, send-message/read-mailbox. No workspace write,
shell, process, or delegation. Scout additionally requests
`code_diagnostics` because its role policy allows it; a research Worker does
not. Scout's role policy then narrows the resolved set to 20 built-ins:
fetch/web, document readers, data-query, remember, use-skill, mailbox, and
tool discovery are unavailable to Scout, while code diagnostics, sources,
recall, team status, and output paging remain.

**Verification (38):** read/list/grep, find/info/read-files, Git, repo-map/
find-symbol/find-references/dependency-graph/build-context/LSP/
code-diagnostics/affected-tests, shell/process/wait-for/local-http/
run-project-task/project-profile, PDF/image/document readers, archive-list,
media-info, data-query, read-tool-output/tool-help/tool-search,
send-message/read-mailbox/team-status/recall/MCP/budget-status/task-detail,
and submit-verdict. There is no write, edit, apply-patch, file-manage, commit,
merge, plan, todo, or delegation schema.

**Minimal (6):** read-file, list-dir, write-file, edit-file, shell,
read-tool-output. This is the trivial-task path and deliberately has no MCP
schemas.

**Full:** everything the role policy produced for the run, exactly as before
this ADR. It is selected only when a Worker/Labour has no task kind, an
unknown task kind, an unknown role, or an operator explicitly previews it.

### 3. Selection from role, profile, and task kind

Selection is deterministic and happens once per run:

```text
leader, manager                  -> leader
verifier                         -> verification
scout                            -> research
worker, labour + known kind      -> task_kind_bundle(kind)
worker, labour + no kind         -> full (fallback)
worker, labour + unknown kind    -> full (fallback)
unknown role                     -> full (fallback)
```

Task-kind aliases (case, spaces, and dashes are normalized):

| Bundle | Kinds |
|---|---|
| `coding` | implementation, implement, implementing, coding, code, edit, fix, bugfix, bug-fix, refactor, feature, test, testing, build, integration, integrate |
| `verification` | verification, verify, review, audit, validate, validation, check, quality, quality-gate |
| `research` | research, investigate, investigation, discovery, explore, exploration, spike, analysis, analyze, design, docs, documentation |
| `minimal` | minimal, trivial, typo, format, formatting, rename, comment(s), lint, one-file, onefile, small, chore |

The mapping is a deliberate allowlist. An unknown non-empty kind falls back
to `full`; it is never guessed from a title, objective, department, or model
output. The selected bundle name, reason, role, profile, task kind, and
fallback flag are persisted in prompt-prefix telemetry and exposed by
`session.tool_bundle`.

For a plan task, the persisted `PlanTask.kind` is copied into the Worker
`AgentSpec` before the run starts. Retries reuse the same kind and therefore
the same bundle. The Leader, Manager, and Verifier never inherit a task kind
as a bundle override: role determines their surface family.

### 4. Freeze rules

The provider prefix is frozen for the whole run:

1. Build the role-policy schema list once with `tool_schemas`.
2. Apply the named bundle allowlist, sorted and deduplicated.
3. Collect dynamic MCP tools once, sorted by exposed name / server id / tool
   name, capped at 32, and append them once.
4. Drop a dynamic tool that would shadow a built-in or an earlier dynamic
   name.
5. Freeze the resulting vector and reuse it for every provider call in the
   run, including tool-result turns, retries inside the run, and compaction
   summary calls that share the same spec.

There is no per-turn schema add/remove. `use_skill` activation is enforced at
tool execution time and never mutates the advertised schema. `tool_search`
can still describe tools outside the frozen bundle; that is a discovery and
recovery surface, not a schema mutation.

### 5. Canonical schema and hashes

`canonical_tools_json` defines the prefix identity:

- tools are sorted by name;
- duplicate names keep the first occurrence after sorting;
- each object serializes `name`, `description`, `parameters` in that order;
- parameter objects are recursively key-sorted;
- arrays keep their order;
- no session id, agent id, worktree, branch, model, timestamp, or other run
  data appears in the schema.

Two hashes are recorded:

- `tool_bundle_hash` / `bundle_hash`: SHA-256 of the format version, selected
  bundle name, role, profile, task kind, and the canonical built-in schemas.
  Dynamic MCP schemas are excluded so the bundle identity is stable even when
  a project adds an MCP server.
- `tools_hash`: SHA-256 of the final canonical list including dynamic MCP
  schemas. This is the existing prompt-prefix tool hash and is what a cache
  epoch binds to.

The same bundle and same schema bytes therefore produce the same
`tool_bundle_hash` and `tools_hash` in another session. A description or
parameter change changes both. A role, profile, task-kind, or format-version
change changes `bundle_hash` even if the selected tool names happen to match.
The canonical fixture hash is locked by a unit test so an accidental field
order or key-order change fails loudly.

The canonical serializer is also used by `PromptSectionHashes::compute`
(`prompt_cache.rs`), so the `tools_hash` on every `context.prompt_prefix` and
`context.cache` event is the bundle hash by construction.

### 6. Telemetry and read-only surfaces

On every run, once:

- `context.prompt_prefix` carries `tool_bundle`, `tool_bundle_hash`,
  `tool_bundle_reason`, `tool_bundle_fallback`, built-in/MCP/total counts,
  target limit, within-target, omitted names, structured warnings, and
  `tool_bundle_frozen=true`;
- `context.cache` carries `tool_bundle` and `tool_bundle_hash` per call;
- `session.cache` exposes `tool_bundle`, `tool_bundle_hash`,
  `tool_bundle_tool_count`, and `tool_bundle_mcp_count` from the latest
  prefix;
- `cache_plan` reports the bundle fields per role and uses the stored plan
  task kind when an agent has one.

Read-only RPCs and CLI:

- `session.tool_bundle` (`vp tool-bundle <session>`) reports the current
  preview per role, the latest persisted observation, exact tool names and
  per-tool schema hashes, categories, write/delegation flags, requested and
  omitted names, warnings, and the static catalog. `--task-kind` previews a
  partition; `--history` returns the bounded newest-first persisted history.
- `tool_bundle.catalog` (`vp tool-bundles`) lists the static matrix.
- `tool_bundle.matrix` (`vp tool-bundles --matrix`) evaluates the real
  registry for every standard role/profile/task-kind combination.
- `tool_bundle.diff` (`vp tool-bundles --diff FROM,TO`) reports added,
  removed, shared, changed, and the two schema hashes.
- `session.tool_bundle.history` is the RPC behind `--history`.
- `agent.info` carries the static catalog for clients that do not have a
  session yet.

None of these surfaces prints schema text, and none calls a provider.

### 7. Size outcomes

With the current registry the actual built-in counts are:

| Role/profile | Bundle | Built-ins | Target |
|---|---|---:|---:|
| Leader generic | leader | 46 | ≤ 48 |
| Leader coding | leader | 48 | ≤ 48 |
| Manager coding (read-only filtered) | leader | 31 | — |
| Worker coding, implementation | coding | 37 | ≤ 40 |
| Labour coding, implementation | coding | 36 | ≤ 40 |
| Research Worker | research | 33 | — |
| Scout | research | 20 | — |
| Verifier | verification | 38 | — |
| Worker coding, trivial | minimal | 6 | ≤ 12 |
| Worker coding, no/unknown kind | full | 64 | — |

The Leader default drops from 72 to 46 schemas; a coding Leader is 48. A
coding Worker drops from 64 to 37. The trivial one-file path drops to 6.
Dynamic MCP schemas are capped separately at 32 and are excluded from the
minimal bundle, so the size targets apply to built-ins and cannot be silently
inflated by a large MCP server.

The reduction is an approximation of token savings, not a promised cache hit.
The provider still bills the first uncached prefix and may evict its cache;
the plan's metric work remains the source of truth for hit ratios.

### 8. Backward compatibility

- Every new protocol field is `#[serde(default)]`; old events decode with
  empty/NULL bundle fields.
- `tool_schemas` keeps its historical full behavior for tests, tool search,
  and callers that do not select a bundle.
- A Worker/Labour with no task kind selects `full`, preserving pre-ADR
  behavior for manual delegation, ACP, conflict resolution, and any old plan
  row without a kind.
- The `full` bundle remains available to operators and is used by the matrix
  preview when no named bundle applies.
- No event kind is renamed, no required field is added, and the JSON-RPC
  major version remains 1.

## Security notes

- **Role policy is the gate; the bundle is not.** `AgentRole::allows_tool` and
  the special-tool dispatch guards still decide whether a call can execute.
  A tool absent from the bundle but present in role policy can be discovered
  by `tool_search` and called manually. A bundle cannot widen a role policy.
- **Read-only roles are pruned twice.** The request list drops write, shell,
  process, delegation, and GUI tools for Manager/Scout; the Verifier drops
  write/edit/apply-patch/plan/todo/remember/delegation. Role policy and the
  dispatch path remain the authoritative enforcement. Verifier shell remains
  allowed by existing policy because verification needs build/test commands;
  the Verifier's isolated checkout and post-run tracked-file guard are
  unchanged.
- **No new write or network capability.** The partition only removes
  advertised schemas. Seatbelt, approval risk, path jail, write scope,
  Verifier, Integrator, capacity, and budget gates are untouched.
- **Dynamic MCP remains trusted-host-state.** Bundle membership does not
  change MCP approval classification. The minimal bundle excludes MCP
  schemas, but an operator can still use `tool_search`/`mcp` if role policy
  allows it; the bundle is an advertisement, not a sandbox.
- **No hidden routing.** Selection uses only the configured role, profile,
  and persisted plan kind. It never inspects prompt text, objective, model
  output, or provider identity.
- **Hash telemetry contains no prompt or schema text.** It records names,
  counts, categories, and SHA-256 digests only.

## Limits

- The bundle matrix is maintained in code. Adding a tool to the registry does
  not add it to a named bundle until an explicit edit and test update.
- `full` is still large (currently 64 schemas for a coding Worker). A call
  path that cannot determine a task kind pays the legacy prefix.
- Manager request counts are lower than the Leader target because the role
  policy is read-only; the static catalog reports the Leader representative,
  not every filtered role.
- Dynamic MCP tools can add up to 32 schemas to leader/coding/research/full
  runs. The size targets count built-ins only; total prompt size is reported
  separately.
- `tool_search` can show out-of-bundle tools, but the model must still choose
  to call them. There is no automatic tool escalation and no schema mutation
  mid-run.
- The bundle hash proves byte equality of the schema list, not provider cache
  retention. Cross-session cache reuse remains provider-managed and is
  measured by the L4/L5 workstreams.
- This ADR does not add a project-level bundle override, per-tool prompt
  trimming, provider-specific schema dialects, or an LLM bundle selector.

## Rationale and alternatives considered

**Why not keep the role-wide bundle?** It is simple and never withholds a
schema, but it makes the stable prefix large for every task and makes a
one-file change pay for team planning, GUI, remote, media, and document
schemas. The measured Leader prefix was roughly 12–13k tokens and the Worker
prefix roughly 11.8k. The cache-lossless plan identifies that fixed block as
the dominant uncached cost on short sessions. A partition is the smallest
change that reduces the fixed cost without changing prompt semantics,
provider behavior, or the approval gates.

**Why not load tool schemas lazily per turn?** Lazy loading changes the
advertised schema between provider calls, which breaks the stable prefix and
the append-only cache invariant. It also makes tool availability depend on
conversation history instead of a deterministic role/task policy. The bundle
is chosen before the first provider call and never changes during the run.
`tool_search` recovers an out-of-bundle tool without moving the prefix.

**Why not let the model choose a bundle?** Model-selected tools are hidden
routing and are not reproducible across retries. The selection here is a pure
function of persisted role, profile, and plan kind. A model can still ask for
more work through the orchestration tools; it cannot silently change its own
schema set.

**Why not one bundle per department or per project?** Departments already map
to profiles, and profiles already narrow the role policy. Adding a
project-configured allowlist would create an unbounded configuration surface
and make the cache identity depend on mutable project state. The static
matrix is small enough to audit and test.

**Why not always use the minimal bundle?** The minimal bundle cannot plan,
delegate, verify, research, inspect sources, or publish artifacts. It is safe
only for a trivial task whose kind is explicitly persisted. Unknown kinds
fall back to `full`, which is deliberately conservative.

**Why is MCP appended rather than bundled by server?** MCP server/tool sets
are project configuration and can change independently of the built-in
registry. Sorting and appending them once preserves determinism for a run
without pretending they are part of the static matrix. The full `tools_hash`
covers them; `bundle_hash` intentionally does not.

## Rollout and migration

1. Deploy the additive protocol fields and the read-only surfaces first. Old
   clients ignore them; old events decode with empty bundle fields.
2. New runs select a named bundle from the persisted role/profile/task kind.
   Existing sessions are not rewritten. A resumed run uses the current
   selection and records a new `context.prompt_prefix` generation.
3. Old plan rows with a missing/unknown kind use `full`. No data migration is
   required; the next plan revision can add a kind.
4. Operators can inspect a session with `vp tool-bundle`, compare two
   bundles with `vp tool-bundles --diff`, and evaluate the whole live registry
   with `vp tool-bundles --matrix` before changing a task kind.
5. Rollback is a code rollback: there is no destructive schema migration and
   no stored bundle row that must be rewritten. A rollback returns every run
   to the role-wide list; old telemetry remains readable.
6. If a named bundle withholds a tool that a task unexpectedly needs, the
   operator can change the plan task kind (for example from `trivial` to
   `implementation`) and retry. The retry uses a new bundle and a new prefix;
   the previous run's events remain intact.

## Test plan

The module tests enforce:

- exact requested name sets, sorted and duplicate-free, for every standard
  role/profile combination;
- task-kind alias mapping, case/space/dash normalization, and unknown
  fallback;
- read-only pruning for Manager, Scout, and Verifier, including browser,
  computer, shell, process, delegation, plan, todo, and remember;
- size targets and the five/six-tool minimal set;
- canonical field order, recursive key sorting, array-order preservation,
  unicode, scalars, null/boolean/number parameters, deduplication, and a
  locked fixture hash;
- stable `bundle_hash`/`tools_hash` across input permutations and sessions,
  and a changed hash when the selection, role, profile, description, or
  parameters change;
- MCP name sorting, shadow rejection, cap enforcement, minimal exclusion,
  and no per-turn churn;
- static catalog coverage, metadata completeness, audit cleanliness, and
  category histogram totals.

The agent tests enforce:

- the Leader run advertises ≤48 schemas and the same list on every turn;
- a trivial plan task reaches a real Worker with exactly six schemas, writes
  its file, and is verified with the verification bundle;
- a coding Worker receives the coding bundle without plan/team/verdict
  schemas, and Labour does not receive delegation;
- a Verifier manually requesting `write_file` is denied before approval and
  cannot create the file;
- two sessions with the same Leader bundle produce identical `tools_hash`,
  schema bytes, and persisted `tool_bundle_hash`;
- MCP tools appear after built-ins in name order and stay frozen across
  provider calls;
- `session.tool_bundle`, `.history`, `tool_bundle.catalog`, `.matrix`, and
  `.diff` return bounded, deterministic, schema-free data.

The protocol tests decode legacy payloads with defaults and round-trip every
new field. The CLI tests parse repeated roles, task kinds, matrix/diff flags,
and render the human views without schema text.

## Observability example

A healthy Leader prefix event contains only names, counts, and hashes:

```json
{
  "tool_bundle": "leader",
  "tool_bundle_hash": "…64 hex…",
  "tool_bundle_reason": "role_leader",
  "tool_bundle_fallback": false,
  "tool_bundle_tool_count": 46,
  "tool_bundle_mcp_count": 0,
  "tool_bundle_total_tool_count": 46,
  "tool_bundle_target_limit": 48,
  "tool_bundle_within_target": true,
  "tool_bundle_omitted": [],
  "tool_bundle_warnings": [],
  "tool_bundle_frozen": true,
  "tools_hash": "…64 hex…"
}
```

The matching `context.tool_bundle` manifest adds per-tool `name`, `category`,
`workspace_write`, `delegation`, and `schema_hash` rows. It never contains
description or parameter text. The `session.tool_bundle` surface adds the
current preview, the latest persisted run, category totals, and the static
catalog; `--history` adds bounded past observations and `--diff` adds the
added/removed/shared tool names.

## Consequences

Positive:

- the default Leader prefix loses 26 schemas (72 → 46) and a coding Worker
  loses 27 (64 → 37);
- a trivial task costs six schemas instead of the full role list;
- read-only roles cannot even see write/delegation schemas;
- the tool prefix is canonical, frozen, and provably identical across
  sessions with the same inputs;
- operators can inspect and diff bundles without a provider call.

Negative:

- a Worker with an unknown/missing task kind still pays the full prefix;
- the matrix is a maintained allowlist and must be updated when a tool or
  profile changes;
- a tool can be role-allowed but out of bundle, so the model must use
  `tool_search` or an operator must change the task kind;
- the size target applies to built-ins, not dynamic MCP schemas;
- fewer schemas may change model tool-choice behavior; the prompt tests and
  live smoke remain necessary.

None of these consequences weaken approval, sandbox, path, Verifier, merge,
capacity, or budget enforcement.
