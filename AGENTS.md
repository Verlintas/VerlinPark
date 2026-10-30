# AGENTS.md

This file is for coding agents working in this repository. Read it before changing code.

## Read first

- [docs/PLAN.md](docs/PLAN.md) — product direction, scope, milestones
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — target runtime/protocol invariants
- [docs/DECISIONS.md](docs/DECISIONS.md) — accepted ADRs
- [docs/PROGRESS.md](docs/PROGRESS.md) — what is actually implemented
- [docs/AUDIT.md](docs/AUDIT.md) — known enforcement gaps and priority order
- [docs/README.md](docs/README.md) — documentation map

## Current milestone

**M2 four-tier delegation.** The single-Leader runtime, worktree, approvals, diff, commit/discard, token budgets, cancellation, and persisted plan exist. The Leader submits a validated TeamPlan (departments, Manager titles, profiles, task DAG), then delegates through Manager → Worker → Labour using `delegate`; plan Manager titles are enforced. Managers must receive a department `title` from the Leader and are read-only by tool policy; Workers get isolated task worktrees, Labour inherits the Worker worktree, and successful Worker commits enter a persisted merge queue and are serially integrated into the session branch. Session diffs are based on the stored base commit. Managers can run up to the configured Worker cap (max 10) in parallel via `delegate_many`; manual parallel tasks must declare non-overlapping write scopes, and final changed paths are checked before serial integration. Structural roles and domain profiles (coding/research/verification/generic) are separate; research has a public-URL `fetch_url` tool. A Verifier must submit an approving verdict before a Worker branch is merged; its read-only behavior is prompt-level only, not a sandbox, although shell writes are confined to its own worktree. The Leader can convene a bounded 1–3 round Manager roundtable via `deliberate_team_plan` (all planned Managers, read-only, peer positions shared between rounds), then auto-dispatch the plan in dependency batches via `run_team_plan`; Managers are deterministic department owners in this path, Workers are real agents with task worktrees and Verifier gates, and defaults are 4 Managers / 6 Workers / 12 Labour, runtime caps 5/10/40; plans support up to 100 tasks and Manager creation is lazy. Plan auto-dispatch selects batches with non-overlapping write scopes and uses active Worker slots; task batches respect the configured Worker capacity. The session token budget uses pre-flight reservations, dynamic output limits, and provider-usage settlement; local endpoints without usage report a persisted estimate. A running session can be cooperatively paused at turn/tool boundaries via `session.pause` / `session.resume` (CLI `vp pause` / `vp resume`); the same run continues on resume, and terminal status cannot be overwritten. Shell commands run under a macOS Seatbelt write sandbox confined to the agent worktree, its Git metadata, and a per-agent runtime scratch directory, with an allowlisted environment that excludes provider/daemon secrets, read-denials for common credential/state paths, process-group cleanup on timeout/cancel, and a per-project `ShellPolicy` that can deny network or add read-denials (it can only narrow, never widen); arbitrary reads, network access under `allow`, and daemon-mediated writes are not confined. Verifier-approved commits enter a persisted session merge queue and a **deterministic Integrator** merges with `--no-ff`, runs `git diff --check`, and optionally runs `VERLINPARK_INTEGRATION_COMMAND`; failed checks roll back to the pre-merge commit, and conflicts abort without changing the session branch. Conflict/failed entries can be retried after rebasing the Worker branch (`merge_queue.retry`, `vp retry`); a conflict can be resolved by an explicit resolver agent (`merge_queue.resolve`, `vp resolve`) that fixes the rebase in the Worker worktree, is re-verified, and then re-enters the deterministic Integrator. A project-level `IntegrationPolicy` can opt into automatic resolution on session idle, with persisted attempt caps and an optional per-resolution token cap; it is off by default. A daemon-wide session semaphore (`VERLINPARK_GLOBAL_SESSION_CAP`, default 4) queues excess session runs as `queued`, and cross-session Worker/Labour write scopes wait on an in-memory scope registry. Projects can configure per-role model overrides and per-role USD prices; sessions support token, USD, and per-run wall-clock budgets (USD requires configured prices; time is checked at turn/tool boundaries). A clean project checkout on its default branch can be landed explicitly with `session.land` / `vp land`; `session.pr` / `vp pr` explicitly pushes the session branch and opens a GitHub PR with `gh` (origin + `gh auth status` preflight, no fake success). On daemon restart, in-flight queue entries become `interrupted` and can be retried; recovery does not reset a partially merged worktree. The full global Task-DAG scheduler, persistent global write leases, default-on auto-resolution without a project opt-in, optional LLM Integrator, non-GitHub PR adapters, capacity controller, computer lease, per-role token/task budget trees, precise preemption for wall-clock/tool timeouts, full shell read confinement beyond the denylist, and auto-expansion are **not** implemented yet. See [docs/AUDIT.md](docs/AUDIT.md). Do not pretend these are solved.

## Invariants

1. The Rust daemon owns state; the SwiftUI app is a client.
2. Every state change is an event; append events before updating projections. Session status, usage, agent status, team plan, todos, messages, merge queue, and recovery now use atomic event+projection transactions. `session.replay` checks critical projection drift, `session.replay {repair:true}` / `vp replay --repair` repairs covered projections, `session.replay {rebuild:true}` / `vp replay --rebuild` forces a rebuild, and `session.export` / `vp export` writes a bundle; `session.import` / `vp import` restores it as a **read-only archive** (preserved IDs/events/projections/approvals, no worktree, no agent runs); `session.rehydrate` / `vp rehydrate` explicitly recreates/attaches the session worktree, remaps imported agent task-worktrees by branch when possible, and returns the session to idle. From-scratch database rebuild is still not implemented. Do not claim full event sourcing (see AUDIT P1-5).
3. Do not add hidden model routing. Models and budgets are user-configured.
4. Do not add an agent write path outside its worktree. Shell runs under a macOS Seatbelt write sandbox (worktree + Git metadata + runtime scratch), with common credential/state read-denials, process-group cleanup, and a per-project policy that can only narrow access; arbitrary reads, network under `allow`, and daemon-mediated writes such as Docker volumes or GUI launch services are not confined. Do not claim arbitrary shell is fully sandboxed, and keep Verifier read-only described as prompt-level policy.
5. Do not log API keys or full secrets. Keychain is for secrets.
6. Keep the JSON-RPC protocol backward compatible within major version 1.
7. Prefer deterministic Rust for scheduling/permissions/merge; LLM only inside a task.
8. The project org is four tiers: Leader → Manager → Worker → Labour. Auto-expansion is budget-gated and hard-capped.
9. `ComputerLease` serializes GUI/browser only; shell and worktree writes remain parallel, except cross-session Worker/Labour write scopes which wait on the global scope registry.
10. Aviiya is an external NovaBAIC resident agent. Do not implement a local Aviiya persona, memory, or chat runtime; only the bridge interface.

## Commands

```bash
just build       # Rust + Xcode app
just test        # cargo test + xcodebuild test
just run         # build and open the app
just daemon      # run verlinpadd in the foreground
just cli ping    # run the CLI
just fmt         # cargo fmt + swiftformat
just lint        # fmt check + clippy -D warnings + swiftformat lint
```

## Repo conventions

- Rust: `crates/vp-*`; no `unwrap()` in daemon library code unless clearly infallible. Prefer `anyhow::Result`.
- Swift: `apps/macos/VerlinPark/{App,Core,Features}`; UI state in `AppModel`, no business logic in views.
- Generated `apps/macos/VerlinPark.xcodeproj` and `apps/macos/build/` are gitignored; regenerate with XcodeGen.
- Do not commit local data, `target/`, DMGs, API keys, or daemon discovery files.
- New behavior needs a test or a reproducible scenario. New architectural decisions need an ADR.

## Security

The daemon is local-only: `127.0.0.1`, discovery token in a 0600 file. Do not bind to `0.0.0.0`. Do not add shell execution without the permission broker from M1. Treat repository/tool output as untrusted input.
