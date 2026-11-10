# Agent Orchestration and Prompt-Cache Engineering

> **Status:** Research notes, not an implementation plan of record.<br>
> **Date:** 2026-11-10<br>
> **Scope:** Prompt/KV caching, context engineering, multi-agent topology, and
> concrete implications for VerlinPark.<br>
> **Sources:** Public provider docs, engineering posts, papers, and repository
> documentation available on 2026-11-10; provider pricing and cache rules change,
> so verify before implementation.

---

## 0. Executive summary

1. **Prompt caching is a prefix problem, not a “cache everything” switch.**
   The cached unit is the exact token sequence from the beginning of the request.
   One changed byte in the stable prefix invalidates everything after it.
   Stable content must come first; volatile content must come last.

2. **The highest-leverage cache metric is input cache hit rate.**
   Manus reports an input:output ratio around 100:1 and calls KV-cache hit rate
   the single most important production metric. OpenAI describes prompt caching
   as turning an agent loop from quadratic to linear in sampling cost.

3. **Tool definitions are part of the prefix.**
   Anthropic’s cache order is `tools` → `system` → `messages`. Reordering tools,
   renaming a parameter, changing a description, adding a skill filter, or
   changing the model invalidates the cache from that point. Manus’s rule is
   “mask, don’t remove”: keep the schema stable and constrain execution.

4. **Multi-agent is a cost decision before it is an architecture decision.**
   Anthropic’s orchestrator-worker research system improved its internal eval by
   ~90.2% over a single agent, but used ~15× the tokens of a chat and ~3.75× a
   single agent. It is optimized for breadth-first research, not tightly coupled
   coding.

5. **The strongest team patterns are boring and inspectable.**
   Claude Code agent teams use a shared task list, a mailbox, independent
   contexts, and worktrees. Magentic-One uses a task ledger plus a progress
   ledger and replans after repeated stalls. OpenHands uses an event stream and a
   runtime boundary. Aider uses a tree-sitter repo map plus graph ranking.
   SWE-agent shows that interface guardrails (lint-on-edit) change outcomes.

6. **VerlinPark already has the hard deterministic half.**
   Worktrees, a four-tier hierarchy, persisted task DAG, event-sourced
   projections, Verifier gates, merge queue, capacity, leases, and scheduler are
   real. The missing pieces are **cache-aware prompt construction**, **cache
   telemetry**, **progress/stall ledgers**, **team-shape heuristics**, and
   **tree-sitter/PageRank repo intelligence**.

---

## 1. What is actually cached

There are three different caches that are often conflated:

| Cache | Scope | Key | What it saves |
|---|---|---|---|
| KV cache | One inference request | Token positions | Decoding recomputation inside a request |
| Prefix / prompt cache | Across requests | Exact token prefix | Prefill (input processing) for repeated prompt prefixes |
| Semantic cache | Application layer | Similar meaning / embedding | Entire model call for near-duplicate requests |

This document is about **prefix/prompt caching**. It is exact-prefix, not
semantic. A timestamp, random request ID, reordered tool, or changed system
prompt before the cache boundary makes the following tokens uncached.

### Provider snapshot

| Provider | Mode | Minimum / granularity | Explicit markers | Typical read price | Notes |
|---|---|---|---|---|---|
| Anthropic Claude | Explicit (`cache_control`) or automatic | Model-dependent; often 1k–4k tokens | Up to 4 breakpoints; `ttl: "5m"` (default) or `"1h"` | ~0.1× input | Writes ~1.25× (5m) or ~2× (1h); order is tools → system → messages; usage fields include cache creation/read tokens |
| OpenAI | Automatic | 1,024 tokens; 128-token increments | No marker needed | ~0.5×–0.9× depending on model | Usage exposes `prompt_tokens_details.cached_tokens`; exact prefix required |
| Google Gemini | Explicit cache object (and newer implicit modes) | Large prompts; cache object + TTL | Cache resource name | ~0.1× on 2.5+ (2.0 ~0.25×) | Default TTL around 60 minutes; storage billed per token-hour |
| vLLM / SGLang / local | Automatic prefix cache | Implementation-dependent | Usually none | No billing; lower TTFT | vLLM uses hash-based automatic prefix caching; SGLang uses RadixAttention |
| DeepSeek / OpenRouter / others | Provider-dependent | Provider-dependent | Provider-dependent | Provider-dependent | Parse provider-specific cached-token fields; do not assume OpenAI field names |

Anthropic’s documented order matters: **tool definitions first, then system, then
messages**. A tool-list change invalidates the system and message cache. A
system change invalidates the message cache. A messages-only append preserves
the prior prefix.

---

## 2. Cache engineering rules

These rules are the practical core of the research. They apply to any agent
runtime, including VerlinPark.

### 2.1 Stable prefix, volatile suffix

Put content in this order:

```text
1. Tool definitions (stable per role/profile, deterministic order)
2. Stable role charter and security/output contract
3. Project conventions / AGENTS.md / stable skill index
4. Stable team brief (goal, constraints, success criteria)
5. Run-specific context (session id, worktree, branch, task objective)
6. Conversation messages (append-only)
7. Latest tool result / recitation / current instruction
```

Items 1–4 are cache candidates. Item 5 changes per agent/task but should still
be a single stable block for the life of that agent. Items 6–7 are the volatile
tail.

### 2.2 Append-only context

Never rewrite previous actions, observations, tool results, or system blocks
unless you are intentionally starting a new cache generation (for example after
compaction). Every rewrite of an earlier message invalidates all following
cache entries.

### 2.3 Do not change tools mid-run

The tool list is part of the cached prefix. Two common mistakes:

- filtering tools when a skill activates;
- enumerating MCP tools in nondeterministic order.

Better: keep the role’s full stable tool schema, sort tools deterministically,
and enforce skill/tool policy at execution time. If the model calls a disallowed
tool, return a precise policy error. This costs one turn but preserves the
prefix. “Mask, don’t remove” is the same idea at the decoding layer.

### 2.4 Deterministic serialization

Canonicalize:

- tool order;
- JSON object key order;
- schema formatting;
- system block order;
- MCP tool order;
- message role/content shapes.

A semantically identical prompt with different key order is a different cache
key. serde_json’s default map ordering is deterministic in this repository, but
explicit sorting of MCP tools and team entries is still required.

### 2.5 Never put volatile data in the stable prefix

Do not put these before a cache breakpoint:

- timestamps or current time;
- session IDs, task IDs, request IDs;
- worktree paths or branch names;
- per-user names or secrets;
- live environment state;
- random example selection;
- per-request retrieved snippets.

Move them to the last system block or the first user message after the cache
boundary.

### 2.6 Cache break-even and TTL

Anthropic’s 5-minute cache write costs more than a normal input; it pays back
after roughly one read at the 0.1× read rate. A 1-hour cache write costs more,
so it pays back after roughly three reads. A 60% hit rate at a 5-minute TTL can
cost more than a 25% hit rate at a 1-hour TTL if requests are bursty. Measure
the actual break-even, not just hit rate.

For sporadic agent traffic, a longer TTL can be correct. For a single short
session, a 5-minute TTL is usually enough. Do not cache if the prefix is below
the provider minimum or will not be reused.

### 2.7 Cache-aware compaction

Compaction intentionally replaces the conversation prefix, so it inevitably
starts a new cache generation. The goal is not “never compact”; it is:

- compact as late as safely possible;
- keep the stable tool/system prefix unchanged;
- create one new summary block rather than repeatedly rewriting;
- use provider-native compaction (for example OpenAI’s compact endpoint) when
  available;
- measure cache hit rate before and after compaction;
- avoid compacting on every turn due to a noisy token estimate.

### 2.8 Measure cache usage, not just tokens

Track per request:

```text
input_tokens
cached_input_tokens
cache_creation_input_tokens  # Anthropic
output_tokens
cache_hit_rate = cached_input_tokens / (input_tokens + cached_input_tokens)
cost_uncached + cost_cached + cost_cache_write
time_to_first_token
```

Also track unique system-prompt hashes and tool-bundle hashes per day. A high
number of unique prefixes usually means cache churn, not model diversity.

---

## 3. VerlinPark cache audit

This is the important part: where the current code already helps, and where it
silently destroys cache.

### 3.1 Provider adapters do not expose cache usage

- `crates/vp-providers/src/lib.rs::openai_usage` reads only
  `/usage/prompt_tokens` and `/usage/completion_tokens`. It ignores
  `prompt_tokens_details.cached_tokens` and provider-specific cached-token
  fields.
- `crates/vp-providers/src/anthropic.rs` sends `system` as one string and tools
  without `cache_control`. It parses only input/output tokens from
  `message_start`/`message_delta`; it ignores
  `cache_creation_input_tokens` and `cache_read_input_tokens`.
- `usage_cost` prices every input token at the full input price. There is no
  cache-read or cache-write price in `RoleModelConfig`.
- The usage ledger (`usage_ledger`) stores only input/output/estimated/cost, so
  cache performance cannot be replayed, audited, or displayed.

### 3.2 The system prompt contains volatile data early

`crates/vp-core/src/agent.rs::role_system_prompt` builds:

```text
Project: '...' at /absolute/project/root.
Session goal: '...'.
Worktree: /absolute/session/worktree.
Branch: ...
Profile: ...
Write scope: ...
Then role instructions...
Then objective for Manager/Worker/Verifier/Labour.
```

The project root, session goal, worktree, branch, write scope, and task
objective change per session/task. They are in the system prompt before the
role instructions. This prevents cache reuse:

- across sessions in the same project;
- across Workers in the same team (each worktree differs);
- across Managers of the same session (different objective/title);
- across repeated runs of the same role on similar tasks.

Within a single agent loop the system is stable, so intra-run hits are possible.
Across the team, the prefix is mostly different.

### 3.3 Skill activation changes the tool list

`run_loop` filters the tool vector by the active skill on every provider call:

```rust
tools: tools.iter()
    .filter(|tool| active_skill.as_ref().is_none_or(|skill| skill_allows_tool(skill, &tool.name)))
    .cloned()
    .collect()
```

Activating `use_skill` changes the tool schema prefix mid-run. The execution
gate already checks `skill_allows_tool` before dispatch, so schema filtering is
not the only enforcement point. Removing it from the schema path (or keeping a
stable per-role schema and returning a policy error on disallowed calls) would
preserve cache.

### 3.4 MCP tool order is server-driven

`collect_dynamic_mcp_tools` iterates servers from storage and tools from the MCP
server. The order is not explicitly sorted by exposed name. A server returning
tools in a different order changes the tool prefix and invalidates the cache.
Sort by exposed name (or by server name + tool name) before building schemas.

### 3.5 Compaction rewrites the conversation head

`compact_context_if_needed` drains the front of the conversation and inserts a
summary at index 0. That is a deliberate cache reset. It is correct for context
safety, but should be treated as a new cache generation and measured. Current
estimates use heuristic token counts; a premature compaction can pay a cache
write for no safety gain.

### 3.6 Dynamic MCP tools and role changes

`tool_schemas` is stable per role/profile, and `ToolRegistry::default_tools` has
a deterministic order. Good. But:

- dynamic MCP tools are appended per run;
- role-specific special tools are appended conditionally;
- model changes invalidate the cache;
- approval mode, sandbox policy, and working directory changes may alter the
  system prompt and thus invalidate it.

Codex handles configuration changes by **appending a new developer/user message**
with the same format instead of rewriting the earlier prompt. VerlinPark should
copy that pattern for approval mode and shell policy changes.

---

## 4. Proposed cache-aware prompt architecture

### 4.1 Request layout

```text
tools:
  sorted(role/profile tools, including a stable superset for active skills)
  sorted(MCP tools, if enabled and stable for the run)

system blocks:
  [S1] stable role charter:
       identity, security, tool-use contract, output contract,
       citation rules, no-hidden-routing rules
  [S2] stable project context:
       project conventions, AGENTS/CONTRIBUTING summary,
       stable skill catalog (names/descriptions only),
       stable source catalog (titles/ids only)
  [S3] run context (one block per agent, stable for its life):
       session id/goal, worktree, branch, task id/title/acceptance,
       write scope, team brief, current plan snapshot

messages:
  [M0] user task prompt (already persisted)
  [M1..Mn] append-only conversation and tool results
  [Mn+1] optional recitation of current objective/progress (volatile tail)
```

For Anthropic, place `cache_control` on:

1. the last tool definition;
2. the end of S1;
3. the end of S2;
4. optionally the end of S3 or the latest stable message.

The fourth breakpoint is a tradeoff. S3 is stable for the life of one agent, so
marking it helps that agent’s later turns. It does **not** help a different agent
whose S3 differs. If cross-agent reuse is more important than intra-agent reuse,
leave S3 unmarked; each agent still reads S1+S2 from the shared cache and writes
its own tail. Choose based on the team’s shape, not by marking everything.

For OpenAI, do not send S1–S3 as one concatenated system string if you want
cross-agent reuse of S1/S2. Send S1 and S2 as stable system messages and S3 as
a final system message (or first user message). The provider caches the longest
exact prefix; the shared prefix remains reusable.

### 4.2 Prompt-prefix hash and cache generation

Add a deterministic `PromptPrefix` object:

```rust
struct PromptPrefix {
    model: String,
    tool_bundle_hash: String,
    stable_system_hash: String,
    version: u32,
}
```

Emit it with each provider call. When the hash changes, mark the next request as
a new cache generation and expect a cache write. This makes cache churn
inspectable instead of mysterious.

### 4.3 Tool policy without schema churn

Two options:

1. Keep the full role schema and enforce `skill_allows_tool` at dispatch
   (current execution gate already does this). Add a visible note in the system
   prompt: “The active skill may reject some tools.”
2. If hiding is mandatory, apply the filter only once at the start of a run and
   never on `use_skill` changes; a skill change would require a new run. This is
   worse for usability.

Option 1 is recommended. The cost of a rejected call is one turn and a clear
policy error; the cost of invalidating a 20k-token tool prefix is much higher.

### 4.4 Cache-aware team context

For a team, split the context:

- **Team-stable prefix**: project conventions, repository map summary, team goal,
  architecture constraints, success criteria.
- **Role-stable prefix**: role charter and tool schema.
- **Agent-volatile block**: worktree path, task id/title, acceptance, write
  scope, upstream handoff summary.

This lets all Workers in the same role share the stable prefix even though their
worktrees differ. The current `role_system_prompt` mixes all three, so this is
the largest cross-agent cache opportunity.

### 4.5 Cache-aware compaction policy

- Set compaction threshold from the real provider context window, not a fixed
  small default.
- Do not compact on heuristic noise; require a safe margin.
- Before compaction, emit `context.cache_generation` with the new prefix hash.
- After compaction, expect one cache write; do not immediately compact again.
- Keep the stable tool/system prefix unchanged across compaction.
- Prefer provider-native compaction when available (OpenAI Responses API).
- Measure hit rate before/after; if a compaction causes repeated misses, raise
  the threshold or reduce summary size.

### 4.6 Usage ledger and pricing

Add to `usage_ledger` and usage events:

```text
cache_read_tokens
cache_write_tokens
cache_hit_rate
cache_generation
```

Add to role pricing:

```text
input_usd_per_million
cache_read_usd_per_million
cache_write_usd_per_million
output_usd_per_million
```

Use provider-reported cache fields. If a provider does not report them, mark the
entry `cache_estimated: true` and do not invent a hit rate from elapsed time.

---

## 5. What top agent systems do

### 5.1 Anthropic multi-agent research

Architecture:

- Lead agent (Opus) plans and decides how many subagents to spawn.
- 3–5 subagents run in parallel, each with its own context window.
- A separate citation pass synthesizes findings.

Results:

- ~90.2% improvement over single-agent Opus 4 on their internal research eval.
- ~15× tokens versus chat; ~3.75× versus a single agent.
- Token usage alone explained ~80% of performance variance on BrowseComp.

Lesson: this is a **breadth-first research** pattern. It is not a default for
tightly coupled code changes. The cost is real and should be gated by task shape.

### 5.2 Claude Code agent teams

- A lead session decomposes work.
- Teammates are separate sessions with independent context windows.
- They share a task list with dependency tracking and a mailbox.
- Worktrees isolate code changes.
- The lead sets direction; teammates can claim tasks and message each other.

Lesson: shared task state plus direct messaging beats routing every message
through the lead. VerlinPark already has a task queue, mailbox, and coordination
notes; the missing piece is a progress/stall ledger and a clearer team topology.

### 5.3 Magentic-One

- Orchestrator maintains a **task ledger** (facts, guesses, plan).
- Inner loop maintains a **progress ledger** (current progress, assignment,
  completion check, stall check).
- If progress is not being made for enough steps, it updates the task ledger and
  replans.

Lesson: a deterministic stall counter and explicit replanning trigger are worth
more than another agent. VerlinPark has orchestration decisions and retry
backoff, but no first-class progress ledger or stall-driven replan.

### 5.4 OpenHands

- Event stream captures actions and observations.
- Agent runtime executes shell/browser/files inside a sandbox.
- The agent loop/event stream lives outside the execution environment.
- Newer architecture separates stateless session, harness, and sandbox.

Lesson: event sourcing plus a runtime boundary is the right foundation.
VerlinPark already has this; keep projections and replay integrity strong.

### 5.5 Aider

- Tree-sitter extracts definitions/references.
- A graph-ranking algorithm selects the most relevant repo symbols within a
  token budget (default ~1k map tokens).
- Architect/editor split: strong reasoning model plans, cheaper model edits.
- Atomic auto-commits keep changes reviewable.

Lesson: the repo map is not “read everything”; it is a ranked, token-budgeted
summary. A tree-sitter + PageRank-style map is the next step beyond VerlinPark’s
lexical index.

### 5.6 SWE-agent

- Agent-Computer Interface (ACI) is a first-class design surface.
- File viewer with line numbers; edit command grounded in the viewer.
- Lint-on-edit blocks syntax errors and shows before/after context.
- Empty command output gets an explicit “success, no output” message.
- Ablation: lint guard improved SWE-bench Lite from 15.0% to 18.0% (a ~20%
  relative improvement) in their setup.

Lesson: a small guardrail in the edit path can outperform a better prompt. Add
optional syntax/lint validation after `apply_patch` and return a precise diff
when it fails.

### 5.7 LangGraph supervisor vs swarm

- Supervisor: one coordinator routes to subagents; the coordinator sees the user.
- Swarm: agents hand off directly; each agent sees the shared conversation;
  fewer supervisor messages, lower token use.
- LangGraph’s benchmark found swarm slightly outperformed a naive supervisor, but
  supervisor is the most general topology. Removing handoff messages from the
  subagent state and choosing better handoff tool names improved supervisor
  performance by ~50% in their benchmark.
- Swarm failure modes: probabilistic drift over 8–10 sequential handoffs; a bad
  context propagates.
- Supervisor failure modes: coordinator bottleneck, more tokens, handoff
  overhead.

Lesson: hierarchy is not automatically better. Use a supervisor for dependency
and conflict control; use direct handoff only for independent work. Hide routing
messages from the worker’s context.

### 5.8 Cursor / Superset / parallel-agent IDEs

- Cursor 2.0: parallel agents via local worktrees, plus cloud background agents.
- Superset: many CLI agents, one worktree per task.
- The core isolation mechanism is the worktree; cloud VMs add stronger isolation
  but increase setup cost.

Lesson: worktrees are the right local isolation. The remaining coordination
problem is not file collisions; it is shared state, progress, and review.

### 5.9 Codex CLI

- Stateless requests preserve Zero Data Retention.
- The old prompt is always an exact prefix of the new prompt.
- Static instructions/examples first; variable content last.
- Tool changes, model changes, sandbox changes, approval changes, and cwd changes
  can break the cache; where possible, Codex appends a new developer/user message
  instead of rewriting earlier content.
- Automatic compaction when the context limit is exceeded.

Lesson: append-only configuration changes and prefix preservation are production
requirements, not micro-optimizations.

---

## 6. Team orchestration lessons for VerlinPark

### 6.1 Keep the deterministic layer, add a ledger

VerlinPark already has:

- a validated TeamPlan and task DAG;
- deterministic batch selection and write scopes;
- persisted task queue, priority, dependency, attempts, and retry backoff;
- Verifier gate, merge queue, Integrator, conflict resolution;
- capacity, leases, scheduler, and replay/repair.

Add:

- **Task ledger**: facts, assumptions, success criteria, plan version.
- **Progress ledger**: last state change per task, current owner, last
  observation, attempts, stall count.
- **Stall detector**: no completion/state change after N turns or M seconds →
  emit `orchestration.stall`, then replan/escalate.
- **Replan action**: revise the plan or reassign a task, not just retry it.

This is a deterministic controller, not a second LLM. The LLM decides content;
the runtime decides when progress has stalled.

### 6.2 Team-shape heuristic

Do not use a team for every task. A deterministic heuristic can recommend:

- **single agent**: one file, one test, no cross-cutting dependency;
- **one Worker + Verifier**: bounded implementation with a clear acceptance test;
- **Manager + 2–4 Workers**: multiple independent files or parallel research
  threads;
- **multi-department plan**: multiple domains, shared interfaces, or a long DAG.

Inputs: task count, write-scope overlap, dependency depth, expected files,
research breadth, verification requirements, budget. Output: recommended
topology and max parallelism. The Leader can override, but the default should be
cheap.

### 6.3 Cache-shared team prefix

All agents in a session should share:

- project conventions and stable repo map summary;
- team goal and constraints;
- role charter and tool schema.

Only the task/worktree/acceptance block should differ. This is the single
largest cost optimization for a team: a 20k-token stable prefix reused by five
Workers saves far more than reducing a few tool outputs.

### 6.4 Do not let coordination messages pollute the stable prefix

Routing messages, handoff boilerplate, and “you are Worker 3” text should be
appended as a single dynamic context block, not injected into every system
prompt. LangGraph found that removing handoff messages from the subagent state
improved supervisor performance; the same principle applies to cache.

### 6.5 Architect/editor split

VerlinPark already supports per-role models. Make the intended split explicit:

- Leader/Manager: stronger reasoning model, planning and review;
- Worker: coding model, focused implementation;
- Verifier: independent model or at least independent context;
- Labour: cheap model for bounded subtasks.

Do not add hidden model routing. The user configures the role models; the
runtime only honors them.

### 6.6 Repo map: lexical now, tree-sitter next

Current `repo_map` is a bounded lexical index. Aider’s result shows that a
ranked symbol map is more useful than a flat symbol dump. Next step:

- tree-sitter parsers for Rust, Swift, TS/JS, Python, Go, Java/Kotlin, C/C++;
- extract definitions, references, imports, and signatures;
- build a file/symbol graph;
- rank by reference centrality (PageRank or a simple weighted degree);
- select within a token budget;
- report confidence and cache age.

This improves `find_symbol`, `find_references`, `dependency_graph`,
`build_context`, and `repo_map` at once.

### 6.7 Edit guardrail

After `apply_patch`, optionally run a fast syntax/lint check on changed files
(for example `cargo check`/`swiftc -parse`/`tsc --noEmit`/`python -m py_compile`
where cheap). If it fails, return the error with file/line and the before/after
context, and do not silently claim success. This is the SWE-agent lint-on-edit
lesson translated to VerlinPark.

### 6.8 Evaluation

Add an internal benchmark suite with fixture repositories:

- one-file edit;
- multi-file refactor;
- failing test repair;
- dependency upgrade;
- frontend local API test;
- research with citations;
- conflicted merge resolution;
- a 10k-file repo for map/search performance.

Metrics:

```text
task success
wall-clock time
tool calls per task
input/output tokens
cache hit rate
cache write tokens
context compactions per task
retries per task
Verifier rejection rate
merge conflict rate
human interventions
```

Do not compare architectures by token cost alone; Anthropic’s data shows token
usage explains much of performance variance, but the goal is success per unit of
human time and risk, not the cheapest run.

---

## 7. Prioritized implementation slices

### P0 — Cache observability and provider support (~1.5–2.5k LOC)

1. Extend `StreamEvent`/usage records with cache read/write tokens.
2. Parse OpenAI `prompt_tokens_details.cached_tokens` and Anthropic
   `cache_read_input_tokens` / `cache_creation_input_tokens`.
3. Add Anthropic `cache_control` breakpoints for tools and system blocks.
4. Add cache fields to `usage_ledger`, `session.usage`, CSV, and Preview Cost.
5. Add per-request `PromptPrefix` hash and cache-generation events.
6. Add a cache hit-rate card/dashboard.

Acceptance: two consecutive identical provider calls report a cache read on the
second call (provider permitting); usage ledger reconciles cached + uncached +
write tokens.

### P1 — Stable prefix refactor (~1.5–2.5k LOC)

1. Split `role_system_prompt` into stable role/project blocks and a volatile run
   block.
2. Stop filtering tool schemas on skill activation; enforce skill policy at
   execution.
3. Sort MCP tools deterministically and freeze the list for the run.
4. Send configuration changes as appended messages, not rewritten prompts.
5. Add cross-agent shared prefix for the same role/profile.
6. Add a cache-aware compaction threshold and generation marker.

Acceptance: Worker A and Worker B in the same team share a measurable cached
prefix; activating a skill no longer changes the tool-bundle hash.

### P2 — Team ledger and topology (~1.5–2.5k LOC)

1. Add persisted task/progress ledgers and `orchestration.stall` events.
2. Add deterministic stall detection and replan request.
3. Add a team-shape heuristic and expose it in `TeamPlan` suggestions.
4. Keep routing/handoff messages out of the stable prefix.
5. Add direct worker handoff only for explicitly independent tasks.

Acceptance: a stalled plan emits one stall event and either replans or escalates
instead of retrying forever; small tasks do not spawn a team by default.

### P3 — Tree-sitter repo map (~2–3k LOC)

1. Add tree-sitter parsers and symbol/reference extraction.
2. Build a file/symbol graph and rank within a token budget.
3. Feed `repo_map`, `find_symbol`, `find_references`, `dependency_graph`, and
   `build_context`.
4. Keep the lexical index as a fallback and report confidence.

Acceptance: on a 10k-file fixture, the map fits a 2k-token budget, includes the
symbols needed for a known change, and improves tool-call count versus lexical.

### P4 — Edit guardrail and eval (~1–2k LOC)

1. Optional post-patch syntax/lint check.
2. Fixture benchmark and architecture comparison.
3. Cache/cost/latency dashboard for sessions and teams.

---

## 8. Risks and non-goals

- **Cache writes are not free.** Do not cache a prefix that will not be reused.
- **Provider cache rules change.** Do not hardcode one provider’s discount or
  minimum without a capability table.
- **Multi-agent is expensive.** A 15× token multiplier is not a default for
  tightly coupled coding.
- **Cache does not replace context engineering.** A cached bad prompt is still a
  bad prompt.
- **Do not cache secrets.** Cache keys are provider-side content; never put
  credentials in the stable prefix.
- **Do not hide model routing.** Cache-aware layout is not permission for hidden
  model selection.
- **Do not weaken the sandbox or approval boundary for speed.** Cache wins must
  not change authority.
- **Do not rewrite history for cache.** Append; compact only as a deliberate
  generation change.
- **Do not over-orchestrate.** A team is a tool for independent work, not a
  status symbol.

---

## 9. Sources

- Anthropic, “How we built our multi-agent research system”:
  https://www.anthropic.com/research/multi-agent-research-system
- Anthropic, “Prompt caching”:
  https://platform.claude.com/docs/en/build-with-claude/prompt-caching
- Anthropic, “Effective context engineering for AI agents” (context as a finite
  resource; referenced by Manus and others):
  https://www.anthropic.com/engineering/effective-context-engineering-for-ai-agents
- OpenAI, “Prompt caching”:
  https://developers.openai.com/api/docs/guides/prompt-caching
- OpenAI, “Unrolling the Codex agent loop”:
  https://openai.com/index/unrolling-the-codex-agent-loop/
- Google, “Context caching”:
  https://ai.google.dev/gemini-api/docs/caching
- Manus, “Context Engineering for AI Agents: Lessons from Building Manus”:
  https://manus.im/blog/Context-Engineering-for-AI-Agents-Lessons-from-Building-Manus
- Microsoft Research, “Magentic-One”:
  https://www.microsoft.com/en-us/research/articles/magentic-one-a-generalist-multi-agent-system-for-solving-complex-tasks/
- OpenHands architecture:
  https://github.com/OpenHands/openhands
  https://docs.openhands.dev/openhands/usage/agent-canvas/architecture
- Aider, “Building a better repository map with tree-sitter”:
  https://aider.chat/2023/10/22/repomap.html
- SWE-agent, “Agent-Computer Interface”:
  https://github.com/SWE-agent/SWE-agent/blob/main/docs/background/aci.md
  https://arxiv.org/abs/2405.15793
- LangGraph supervisor/swarm:
  https://reference.langchain.com/python/langgraph-supervisor
  https://reference.langchain.com/python/langgraph-swarm
  https://www.langchain.com/blog/benchmarking-multi-agent-architectures
- Claude Code agent teams coverage:
  https://www.mindstudio.ai/blog/claude-code-agent-teams-shared-task-list
  https://joseparreogarcia.substack.com/p/claude-code-agent-teams
- Cursor worktrees:
  https://cursor.com/docs/configuration/worktrees
- “Don’t Break the Cache” (long-horizon agentic prompt caching evaluation):
  https://arxiv.org/html/2601.06007v2
