# AGENTS.md

This file is for coding agents working in this repository. Read it before changing code.

## Read first

- [docs/PLAN.md](docs/PLAN.md) — product direction, scope, milestones
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — runtime/protocol invariants
- [docs/DECISIONS.md](docs/DECISIONS.md) — accepted ADRs
- [docs/PROGRESS.md](docs/PROGRESS.md) — what is actually implemented

## Current milestone

**M1 Leader runtime.** The daemon, SQLite/event log, CLI, provider SSE adapters (including streamed tool calls), provider settings via Keychain, and a single-Leader agent loop with read tools plus write/shell approvals exist. The four-tier agent org, capacity controller, computer lease, per-task worktrees, budgets, and multi-agent scheduler are **not** implemented yet. Do not pretend they are.

## Invariants

1. The Rust daemon owns state; the SwiftUI app is a client.
2. Every state change is an event; append events before updating projections.
3. Do not add hidden model routing. Models and budgets are user-configured.
4. Do not add an agent write path outside its worktree.
5. Do not log API keys or full secrets. Keychain is for secrets.
6. Keep the JSON-RPC protocol backward compatible within major version 1.
7. Prefer deterministic Rust for scheduling/permissions/merge; LLM only inside a task.
8. The project org is four tiers: Leader → Manager → Worker → Labour. Auto-expansion is budget-gated and hard-capped.
9. `ComputerLease` serializes GUI/browser only; shell and worktree writes remain parallel.
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
