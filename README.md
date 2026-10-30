# VerlinPark

**A native macOS agentic development environment for agent teams.**

VerlinPark runs a hierarchy of coding agents in isolated Git worktrees, lets Managers compare plans before work starts, and makes every plan, tool call, diff, token, and permission decision inspectable and traceable. A session-scoped typed peer mailbox and text artifact store are available to agents, the Preview, RPC, and the CLI.

> **Status: M2 four-tier delegation + M3 merge queue/conflict-resolve/landing/PR/pause-resume/shell-sandbox.** The daemon, CLI, SQLite event log, provider adapters, and native SwiftUI shell run end-to-end. A session Leader can plan and delegate through Manager → Worker → Labour, convene a read-only Manager roundtable, auto-dispatch a validated TeamPlan in dependency batches, isolate Workers in Git worktrees, gate merges on a Verifier verdict, and pass approved commits through a persisted merge queue and a deterministic Integrator (merge, diff check, optional project check, rollback on failure). Conflict/failed/interrupted entries can be rebased and retried, or a conflict can be explicitly auto-resolved by a resolver agent and re-verified, with an opt-in project policy for automatic resolution after a session goes idle; a clean checkout can be explicitly landed into the default branch, or pushed and opened as a GitHub PR via `gh`. Sessions have approvals, cooperative pause/resume, cancellation, a live diff, an event trace, and a session-scoped typed mailbox/text artifact store; role models and USD prices are user-configured, and sessions support token, USD, and wall-clock budgets. A daemon-wide session semaphore queues excess runs (`queued`) and cross-session Worker write scopes wait instead of racing. Binary/GC artifact storage, default-on conflict resolution without a project opt-in, a full global Task-DAG scheduler, per-role token/task budget trees, and full shell read/network confinement are **not** implemented yet; shell writes, common credential reads, and process groups are constrained by the runtime sandbox, and projects can deny shell network access or add read-deny paths — see [PROGRESS](docs/PROGRESS.md) and [AUDIT](docs/AUDIT.md).

---

## Why another ADE?

Coding agents are powerful, but the workflow around them is still held together with terminal tabs, copy-pasted prompts, and hope.

- Multiple agents edit the same checkout and step on each other.
- Context is trapped in one chat window; subagents become a game of telephone.
- Token spend and tool activity are invisible until the bill arrives.
- Every agent vendor wants to own the workflow.
- Local models are treated as a second-class option.

VerlinPark is built around a different default: **agent teams should have an org chart, isolated workspaces, and a paper trail.**

It is not a chat window bolted onto an editor. It is a native control plane for planning, running, reviewing, and landing agent work.

## The three pillars

### 1. Park — a workspace for agent teams

Every task gets a Git worktree and a branch. Every manager owns a workstream. Every agent gets a role, a model, a budget, and a permission policy.

The default organization is a four-tier structure:

```text
Leader (1)
├── Managers (4 default, max 5)        discuss, produce outlines, assign work
│   ├── Workers (6 default, max 10)    execute tasks, own worktrees, hire Labour
│   │   └── Labour (12 default, max 40) assist with subtasks
```

Auto-expansion is a planned budget-gated feature; the current runtime uses fixed, configurable caps and creates Managers lazily when a department has ready work.

Plans are proposed by the Leader, validated by code, and executed in dependency batches. Managers can exchange positions in a bounded roundtable; agents exchange durable typed messages and text artifacts through the session mailbox.

### 2. Glass Box — everything is inspectable

VerlinPark records the full session as an append-only event log:

- the plan and every revision
- every agent message, including who asked whom for what
- every LLM request and response
- every tool call, approval, and diff
- tokens, cost, latency, retries, and budget state
- every Git worktree, commit, merge queue entry, and Integrator result

The UI can inspect a session and show exactly why a change was proposed. `vp replay` checks critical projections against the event stream, `vp replay --repair/--rebuild` rebuilds the covered fields, and `vp export` writes a portable session bundle; `vp import` restores it as a read-only archive (including approvals), and `vp rehydrate` explicitly recreates a worktree to make it runnable. See [AUDIT](docs/AUDIT.md).

### 3. Open Bench — native, local-first, and extensible

- Native SwiftUI/AppKit UI, not a web app in a shell.
- Glass Park shell: project rail, chat stage, Collab/Preview studio, Manage Dash.
- Knockout wordmark: solid, gradient, or a region/pride flag showing through the letters; 248 regions and 20 pride flags.
- A Rust core daemon that can run headless and be driven by a CLI.
- Cloud APIs first; any OpenAI-compatible endpoint (LM Studio, Ollama, llama.cpp) works as a local provider.
- Embedded MLX runtime planned for a later milestone.
- MCP for tools, ACP for external agents, skills and hooks for project-specific behavior.
- GPL-3.0, open protocols, no vendor lock-in.

> **Aviiya is external.** The resident assistant surface is reserved for a future
> [NovaBAIC](https://github.com/Verlintas/NovaBAIC) bridge. VerlinPark does not implement a
> local Aviiya persona or memory.

## Architecture at a glance

```mermaid
flowchart LR
  U[User] --> APP["VerlinPark.app<br/>SwiftUI + AppKit"]
  CLI["vp CLI"] --> CORE
  APP <-->|"JSON-RPC over WebSocket<br/>127.0.0.1 + token"| CORE["verlinpadd<br/>Rust agent runtime"]
  CORE --> DB[("SQLite + event/audit log")]
  CORE --> GIT["Git worktrees<br/>merge queue + Integrator"]
  CORE --> PTY["shell processes<br/>(no PTY yet)"]
  CORE --> LLM["Model providers<br/>cloud + local"]
  CORE --> MCP["MCP servers<br/>(planned)"]
  CORE --> ACP["ACP agents<br/>(planned)"]
  CORE --> ART["Artifact store<br/>(planned)"]
```

The daemon owns sessions and keeps running when the app closes. The SwiftUI app is a client. The CLI is another client. The protocol is versioned; full replay is not implemented yet.

## Toward v0.1

The first end-to-end demo is a complete multi-agent feature delivery on a benchmark repository:

1. Open a repository and create a session with a goal.
2. Pick a four-tier org preset. Assign models and budgets manually.
3. The Leader produces a plan; the runtime validates the DAG, scope syntax, fan-out, and budget.
4. Departments execute tasks in parallel, each Worker in its own worktree.
5. A read-only Verifier runs tests and submits a verdict.
6. The Integrator merges approved work into the session branch; the UI shows the plan, messages, tool calls, diffs, queue state, and token usage.
7. The user reviews and lands the integration branch.

**Today:** steps 1–5, the merge queue, and the deterministic Integrator work with a base model, per-role model overrides, and session token/USD/time budgets; the Manager roundtable, task worktrees, and Verifier gate are real. Plan batches only group non-overlapping write scopes (unscoped tasks serialize), and the token budget uses pre-flight reservations. **Still planned:** per-role token/task budget trees, optional LLM Integrator, default-on auto-resolution, a cost ledger view, and binary/GC artifact storage. Opt-in auto-resolution with attempt/token caps is available. Local landing and `gh` PR flow are available. v0.1 is not a full IDE, not a model hub, and not a document workspace. See [docs/PLAN.md](docs/PLAN.md) for scope, milestones, and non-goals.

> Known gaps at this milestone: shell writes are confined to the worktree, Git metadata, and runtime scratch; common credential/state paths are read-denied and shell process groups are killed on timeout/cancel; projects can deny shell network access or add read-deny paths, but arbitrary reads, network under allow, and daemon-mediated writes are not confined; Verifier read-only remains prompt-level; the event log can verify/repair/rebuild critical projections including the agent roster and export a bundle; import is a read-only archive until `vp rehydrate` explicitly recreates the session worktree and remaps imported agent task-worktrees by branch; conflict resolution is an explicit agent-assisted action or an opt-in per-project policy; default-on auto-resolution is not implemented, and the PR flow requires `gh` and currently targets GitHub. See [docs/AUDIT.md](docs/AUDIT.md).

## Documentation

Start with the [documentation index](docs/README.md). The short version:

- [Progress](docs/PROGRESS.md) — what actually works today (source of truth)
- [Audit](docs/AUDIT.md) — known gaps, evidence, and recommended order
- [Architecture](docs/ARCHITECTURE.md) — target runtime, protocol, data model, security
- [Development plan](docs/PLAN.md) — product thesis, scope, milestones, risks
- [Decisions](docs/DECISIONS.md) — locked architectural decisions (ADRs)
- [Research](docs/RESEARCH.md) — competitive analysis and sources (historical)
- [Third-party notices](THIRD_PARTY_NOTICES.md) — bundled FlagKit assets (MIT)
- [Pride flag sources](docs/PRIDE_FLAGS.md) — Wikimedia Commons file/author/license table

## Build and run

Requirements: macOS 15+, Xcode 27+, Rust stable, [XcodeGen](https://github.com/yonaskolb/XcodeGen), [`just`](https://github.com/casey/just).

```bash
just bootstrap
just test
just run
```

Configure a model in **Settings → Providers** (any OpenAI-compatible endpoint, including LM Studio/Ollama/llama.cpp, or Anthropic). The API key is stored in Keychain and sent only to the local daemon. For headless runs, set `VERLINPARK_BASE_URL` and `VERLINPARK_MODEL` (plus `VERLINPARK_API_KEY` if needed). Set `VERLINPARK_INTEGRATION_COMMAND` to add a project-specific post-merge check (for example `cargo test`); without it the Integrator still runs `git diff --check`. `VERLINPARK_GLOBAL_SESSION_CAP` bounds concurrent session runs across the daemon (default 4, range 1–32); excess prompts queue as `queued`. PR flow requires an `origin` remote and an authenticated GitHub CLI (`gh`); local landing does not.

`just run` builds `verlinpadd` and the SwiftUI app, then opens it. The app launches the daemon on demand from `target/debug/verlinpadd` (debug builds). To run the daemon separately:

```bash
just daemon
just cli ping
just cli project-add /path/to/repo --name demo
just cli shell-policy <project-id>
just cli shell-policy-set <project-id> --network deny --read-deny ~/.secrets
just cli integration-policy <project-id>
just cli integration-policy-set <project-id> --auto-resolve --max-auto-attempts 1 --max-resolve-tokens 50000
just cli session-new <project-id> "Add team invitations" --cost-budget-usd 5 --time-budget-secs 1800
just cli events <session-id>
just cli prompt <session-id> "Now add tests"
just cli diff <session-id>
just cli commit <session-id> -m "Add tests"
just cli discard <session-id>
just cli pause <session-id>
just cli resume <session-id>
just cli cancel <session-id>
just cli todos <session-id>
just cli mailbox <session-id>
just cli read-message <message-id>
just cli artifacts <session-id>
just cli artifact <artifact-id>
just cli queue <session-id>
just cli retry <queue-entry-id>
just cli resolve <queue-entry-id>     # explicit agent-assisted conflict resolution
just cli land <session-id>
just cli pr <session-id>
just cli replay <session-id>          # --repair / --rebuild covered projections
just cli export <session-id> -o session.json
just cli import session.json
just cli rehydrate <session-id> --worktree /path/to/worktree
just cli approvals
just cli approve <approval-id>
```

Local state lives in `~/Library/Application Support/VerlinPark/`. The daemon binds to `127.0.0.1` with a random port and a per-run token in a `0600` discovery file. It does not expose a remote API.


## Principles

1. **Native, not wrapped.** If it feels like a browser, we have failed.
2. **Agent teams, not chat tabs.** Organization is a first-class product surface.
3. **Glass box, not black box.** No invisible tool calls, cost, or state changes.
4. **Local-first, cloud-optional.** Your repository and traces stay on your machine by default.
5. **Manual control over magic.** The runtime advises; the user decides models, budgets, and permissions.
6. **Worktree-native.** Isolation is the default, not a feature flag.
7. **Open protocols, open source.** MCP, ACP, Git, and GPL-3.0.

## Non-goals for v1

- A general-purpose document / slides / voice assistant
- A complete IDE with debugger and full LSP stack
- A model marketplace or hosted inference service
- Windows or Linux desktop clients
- Team cloud sync or enterprise administration
- Autonomous merge to `main` without human approval

## License

VerlinPark is licensed under the **GNU General Public License v3.0 only** (`GPL-3.0-only`). See [LICENSE](LICENSE).

GPL-3.0 is incompatible with the current Mac App Store terms; the plan is direct distribution via a signed and notarized DMG and Homebrew. See [docs/PLAN.md](docs/PLAN.md#13-发布与许可).

## Inspiration

VerlinPark learns from, and is not affiliated with:

- [LM Studio Bionic](https://lmstudio.ai/) — local-first agent work with projects, sessions, skills, and model routing
- [DeepSeek Harness](https://www.deepseek.com/harness/) — everything-is-a-plugin harness architecture and trace-first developer experience
- [Zed](https://zed.dev/) — native performance, ACP external agents, parallel threads, worktree isolation
- [Conductor](https://conductor.build/) — one workspace per task, diff review, merge, archive
- [Claude Code](https://code.claude.com/) — subagents, permission modes, worktree isolation
- [LM Studio](https://lmstudio.ai/) and [MLX Swift](https://github.com/ml-explore/mlx-swift) — local inference on Apple silicon

---

## 中文简介

**VerlinPark 是 macOS 原生的 Agent 开发区（ADE）。**

它把每个任务放进独立的 Git worktree，让一个四层部门制的 Agent 团队并行工作；Agent 之间通过结构化消息和共享工件协作；所有计划、工具调用、diff、token 和权限决策都可审计、可回放、可合并。

三个支柱：

- **Park（工作区）**：项目 → 会话 → 部门 → 任务 → worktree，一任务一分支，天然并行。
- **Glass Box（玻璃盒）**：完整事件日志、Trace、成本账本、Checkpoint、逐 hunk review，不做黑盒。
- **Open Bench（开放工作台）**：SwiftUI 原生 UI + Rust Core，本地/云端模型，MCP/ACP，CLI，GPL-3.0。

当前处于 **M2 四层委派 + M3 Merge Queue / Conflict Resolve / 分角色模型与预算 / Landing / PR / Pause-Resume / Shell Sandbox + Policy**：daemon、CLI、SQLite 事件日志、Provider 适配、Glass Park 三栏界面、可配置品牌 Mark、248 地区旗 + 20 Pride 旗、Collab/Preview、Manage Dash 已经可用；Leader 可以委派给 Manager → Worker → Labour，召集 1–3 轮只读 Manager 圆桌，提交并自动派发 TeamPlan，维护可见的 todo 计划，在 Preview 中查看 worktree diff，提交或丢弃审阅后的结果。Worker 拥有独立 task worktree；Verifier 审查通过后进入持久 merge queue，由确定性 Integrator 串行合并、做 diff check 和可选项目检查，失败则回滚；session diff 从 base commit 起算。用户可以把 session branch 本地 land，或显式 push 并用 `gh` 开 PR；Queue 冲突可显式调用 resolver agent 解决，重新验证后再由确定性 Integrator 合并。Agent 之间可以通过 session-scoped typed mailbox 交换消息，并把 ≤100KB 的文本 artifact 发布到 Preview → Mailbox。Leader 必须给每个 Manager 分配部门角色；`delegate_many` 会校验不重叠写作用域；research profile 可读取公网 URL。当前限制：pause 是 turn/tool 边界的协作式暂停，不会中断进行中的 provider stream 或 shell；PR 依赖本机 `gh` 且只支持 GitHub；冲突解决默认需要显式触发，自动触发必须由项目 policy 开启并有 attempt/token cap；global session cap 只做进程内 admission，scope locks 只覆盖 Worker/Labour；USD 预算依赖用户手填 per-role 价格，时间预算只在 turn/tool 边界检查；shell 写入已由 macOS Seatbelt 限制在 worktree、Git 元数据和 runtime scratch，常见 credential/state 路径 read-deny，timeout/cancel 会清理整个 process group（任意读取、网络和 daemon 介导写入仍未隔离，Verifier 只读仍是 prompt 约束）；事件日志已能校验/重建关键投影和 agent roster，并支持 session bundle export/import 与显式 rehydrate；跨环境 approvals 已随 bundle 恢复；imported agent task-worktree 会按 branch 分组重建，缺失 branch 会 warning 跳过。Aviiya 是 NovaBAIC 的外部常驻 Agent，本仓库只保留桥接占位。详见 [docs/PROGRESS.md](docs/PROGRESS.md) 和 [docs/AUDIT.md](docs/AUDIT.md)。

## 许可

GPL-3.0-only。见 [LICENSE](LICENSE)。
