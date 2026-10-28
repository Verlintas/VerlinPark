# VerlinPark

**A native macOS agentic development environment for agent teams.**

VerlinPark runs a hierarchy of coding agents in isolated Git worktrees, lets them collaborate through typed messages and shared artifacts, and makes every plan, tool call, diff, token, and permission decision inspectable and replayable.

> **Status: M2 four-tier delegation (in progress).** The daemon, CLI, SQLite event log, provider adapters, and native SwiftUI shell run end-to-end. A single Leader can stream responses, work on its own Git worktree, inspect a repository, call tools, ask for approval before writes or shell commands, maintain a persisted plan, show a live worktree diff, commit or discard the reviewed result, and hard-stop on a per-session token budget. The four-tier scheduler, per-task worktrees, budgets, and the full permission broker are still ahead.

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
├── Managers (3–5)        discuss, produce outlines, assign work
│   ├── Workers (2–10)    decompose tasks, own worktrees, hire Labour
│   │   └── Labour (3–40) write code, run subtasks
```

Auto-expansion is budget-gated: start with a minimal team, grow only when the task queue and budget justify it, and freeze at 80% of the session budget.

Plans are proposed by the Lead, validated by code, and executed by a DAG scheduler. Agents can talk to each other, but only through typed messages and artifact references — not by forwarding entire chat histories.

### 2. Glass Box — everything is inspectable

VerlinPark records the full session as an append-only event log:

- the plan and every revision
- every agent message, including who asked whom for what
- every LLM request and response
- every tool call, approval, and diff
- tokens, cost, latency, retries, and budget state
- every Git worktree, commit, and merge

The UI can reconstruct a session, replay it, export it, and show exactly why a change landed.

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
  CORE --> DB[("SQLite + append-only event log")]
  CORE --> GIT["Git worktrees<br/>branches + merge queue"]
  CORE --> PTY["PTY / shell processes"]
  CORE --> LLM["Model providers<br/>cloud + local"]
  CORE --> MCP["MCP servers"]
  CORE --> ACP["ACP agents"]
  CORE --> ART["Artifact store<br/>plans, patches, reports"]
```

The daemon owns sessions and keeps running when the app closes. The SwiftUI app is a client. The CLI is another client. The protocol is versioned and replayable.

## Planned v0.1

The first demo is a complete multi-agent feature delivery on a benchmark repository:

1. Open a repository and create a session with a goal.
2. Pick a four-tier org template. Assign models and budgets manually.
3. The Lead produces a plan; the runtime validates the DAG, write scopes, fan-out, and budgets.
4. Departments execute tasks in parallel, each Builder in its own worktree.
5. Scouts produce context packets; Verifiers run tests and review; the Integrator merges into an integration branch.
6. The UI shows the Park Map, every message, every tool call, every diff, and the cost ledger.
7. The user reviews and lands the integration branch.

v0.1 is not a full IDE, not a model hub, and not a document workspace. See [docs/PLAN.md](docs/PLAN.md) for scope, milestones, and non-goals.

## Documentation

- [Development plan](docs/PLAN.md) — product thesis, scope, milestones, risks
- [Architecture](docs/ARCHITECTURE.md) — runtime, protocol, data model, security
- [Research](docs/RESEARCH.md) — competitive analysis and sources
- [Decisions](docs/DECISIONS.md) — locked architectural decisions (ADRs)
- [Progress](docs/PROGRESS.md) — what actually works today
- [Third-party notices](THIRD_PARTY_NOTICES.md) — bundled FlagKit assets (MIT)
- [Pride flag sources](docs/PRIDE_FLAGS.md) — Wikimedia Commons file/author/license table

## Build and run (M0)

Requirements: macOS 15+, Xcode 27+, Rust stable, [XcodeGen](https://github.com/yonaskolb/XcodeGen), [`just`](https://github.com/casey/just).

```bash
just bootstrap
just test
just run
```

Configure a model in **Settings → Providers** (any OpenAI-compatible endpoint, including LM Studio/Ollama/llama.cpp, or Anthropic). The API key is stored in Keychain and sent only to the local daemon. For headless runs, set `VERLINPARK_BASE_URL` and `VERLINPARK_MODEL` (plus `VERLINPARK_API_KEY` if needed).

`just run` builds `verlinpadd` and the SwiftUI app, then opens it. The app launches the daemon on demand from `target/debug/verlinpadd` (debug builds). To run the daemon separately:

```bash
just daemon
just cli ping
just cli project-add /path/to/repo --name demo
just cli session-new <project-id> "Add team invitations"
just cli events <session-id>
just cli prompt <session-id> "Now add tests"
just cli diff <session-id>
just cli commit <session-id> -m "Add tests"
just cli discard <session-id>
just cli cancel <session-id>
just cli todos <session-id>
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

当前处于 **M2 四层委派**：daemon、CLI、SQLite 事件日志、Provider 适配、Glass Park 三栏界面、可配置品牌 Mark、248 地区旗 + 20 Pride 旗、Collab/Preview、Manage Dash 已经跑通；Leader 可以委派给 Manager → Worker → Labour，维护可见的 todo 计划，在 Preview 中查看 worktree diff，提交或丢弃审阅后的结果，并在 session token 预算耗尽时硬停。Worker 拥有独立 task worktree，成功后串行合并到 session 分支；session diff 从 base commit 起算，已合并的改动仍可审阅。Leader 必须给每个 Manager 分配部门角色（Discovery / Implementation / Quality 或自定义）；Manager 可以用 `delegate_many` 并行运行写作用域不重叠的 Worker。集成仍串行，独立 Verifier gate 和 merge queue 仍在后续里程碑。Aviiya 是 NovaBAIC 的外部常驻 Agent，本仓库只保留桥接占位。详见 [docs/PROGRESS.md](docs/PROGRESS.md)。

## 许可

GPL-3.0-only。见 [LICENSE](LICENSE)。
