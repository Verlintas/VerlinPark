# VerlinPark 进度

> 最后更新：2026-10-30
> 当前阶段：M2 四层 runtime + M2.5 硬化 + 确定性 Merge Queue/Integrator 已跑通
> 审计与优先级：[AUDIT.md](AUDIT.md)  
> 文档索引：[README.md](README.md)

---

## 当前可用的能力

| 能力 | 状态 | 说明 |
|---|---|---|
| Leader Agent | ✅ | 流式对话、工具调用、审批、session 预算、取消、todo 计划 |
| Session worktree | ✅ | 每个 session 独立 Git worktree + 分支；项目根只读 |
| 四层组织 | ✅ | Leader → Manager → Worker → Labour；严格链路，Manager 必须有部门 title |
| Agent profile | ✅ | coding / research / verification / generic；决定工具集与 prompt |
| TeamPlan | ✅ | Leader 定义 departments / Manager titles / profiles / task DAG；runtime 校验结构、依赖和 scope 语法并持久化 |
| Manager 圆桌 | ✅ | `deliberate_team_plan`，1–3 轮，只读，后一轮看到前一轮所有 peer positions |
| 自动派发 | ✅ | `run_team_plan` 按依赖批次执行 ready tasks；同批筛选不重叠 scope；Manager 懒创建；失败阻塞依赖 |
| Task worktree | ✅ | Worker 独立 task worktree/branch；Labour 继承 Worker worktree |
| 并行 Worker | ✅ | `delegate_many` / plan batch 并发；两边都校验 write scope 不重叠；空 scope 或重叠 scope 的 plan task 串行执行 |
| Verifier gate | ✅ | 独立 Verifier；`submit_verdict`；approve 才合并。只读是 prompt 约束，shell 未沙箱；审批卡会提示 |
| Merge queue | ✅ | Verifier-approved commit 入持久队列；`merge_queue.list` / `vp queue` / Preview → Queue；状态 queued/merging/checking/merged/conflict/failed |
| Integrator gate | ✅ | 确定性 merge：`--no-ff` + `git diff --check` + 可选 `VERLINPARK_INTEGRATION_COMMAND`；失败 reset 到 merge 前 commit；冲突 abort 并保留 Worker 分支 |
| Queue retry | ✅ | `merge_queue.retry` / `vp retry` / Queue UI：conflict/failed 先 rebase 到当前 session branch，再重新 merge；attempt +1 |
| Landing | ✅ | `session.land` / `vp land` / Preview → Land：clean + default branch 检查后 `--no-ff` 合入项目默认分支；不 push、不建 PR |
| Diff / commit | ✅ | base..worktree diff、显式 commit、discard、session committed 事件 |
| 预算 / 取消 | ✅ | token reservation + provider usage 结算；无 usage 端点使用 `estimated: true` 估算；Stop、CLI cancel、崩溃恢复 |
| 审批 | ✅ | 写/Shell 审批；事件带发起 Agent；UI Approve/Deny |
| 可观测性 | ✅ | agent/tool/approval/plan_task/manager.roundtable/merge_queue/integrator 事件；Collab roster；Preview Plan/Changes/Queue/Trace |
| 容量 | ✅ | 默认 4 Managers / 6 Workers / 12 Labour；runtime caps 5/10/40；plan ≤100 tasks；`delegate_many` ≤10；Worker/Labour 用 active slot，plan batch 按可用容量选择 |
| Provider | ✅ | OpenAI-compatible + Anthropic（Anthropic 工具调用仅 mock 验证） |
| CLI | ✅ | ping/info/projects/session-new/prompt/events/approvals/approve/deny/diff/commit/discard/cancel/todos/agents/team-plan/queue/retry/land |

## 验证

- `cargo test --workspace`：57 个测试通过（core 41、store 11、providers 5）。
- `xcodebuild test`：9 个 Swift 测试通过。
- `just lint`：fmt + clippy `-D warnings` + swiftformat lint 通过。
- 最近 CI：GitHub Actions success。
- 手工 E2E 已覆盖：Leader/Manager/Worker/Labour 委派、task worktree、并行 Worker、Verifier approve/reject、TeamPlan 自动派发、Manager 圆桌、审批、diff、commit/discard、预算/取消。

## 已知 enforcement 缺口

第一轮硬化（2026-10-30）已修复 plan scope/caps、token reservation 和 fetch_url 的 DNS 绕过。仍然存在的：

1. **Verifier 只读**：只是 prompt 约束；`shell` 工具没有被沙箱限制。审批卡会提示，但 runtime 不阻止写操作。见 AUDIT P1-3。
2. **事件溯源**：部分状态先写 SQLite 再 append event，部分状态没有事件；事件日志目前是审计流，不是完整 replay 的 source of truth。见 AUDIT P1-5。
3. **batch abort 状态一致性**：同批 task 在预算/取消中止时可能残留 `running` UI 状态；见 AUDIT P2-7。

## 当前限制（明确未实现）

- PR flow 仍未实现；本地 landing 可用，但不会 push 或创建 PR。
- merge queue 目前是 session 内、逐条 drain；retry 是人工触发，没有自动冲突解决或 LLM Integrator。
- 自动派发中的 Manager 是确定性部门节点；LLM 圆桌只在 `deliberate_team_plan` 中运行。
- 没有全局跨 session 调度器、task retry UI、单 task cancel/pause。
- 没有 auto-expansion；Manager/Worker 上限是固定 caps，不是按预算自动扩编。
- 没有 per-role 模型/预算；所有 profile 共用同一模型和 session token budget。
- 没有 typed message bus / artifact store；Agent 之间仍是委派调用 + 事件记录，没有异步 mailbox。
- 没有 context compaction、checkpoint/resume；daemon 重启会把运行中 session 标为 failed。
- 没有终端、逐 hunk review、真实 Collab 节点、MCP/ACP、web search。
- Verifier 的 Shell 不是沙箱；task worktree/branch 暂无自动清理。
- Aviiya 仍是 NovaBAIC 外部桥接占位，不在本仓库实现人格/记忆。
- 没有签名/公证；App 是 unsigned debug build。

## 历史里程碑

- **M0 Foundation**：Rust workspace、daemon + token discovery、HTTP/WebSocket JSON-RPC、SQLite/JSONL 事件、CLI、Provider SSE、SwiftUI shell、CI。
- **M0.5 Glass UI**：三栏 Glass Park、品牌/地区旗/Pride 旗、Manage Dash、Computer Lease 展示、Aviiya external placeholder。
- **M1 Leader runtime**：session worktree、工具、审批、token 预算、取消、diff/commit/discard、Provider 设置、todo 计划。
- **M2 四层 runtime**：Manager titles/profiles、TeamPlan、manager roundtable、自动派发、task worktrees、并行 Worker、write scope、Verifier gate、Task Board。
- **M2.5 硬化**：plan batch scope/cap、token reservation、DNS 固定的 fetch_url、Verifier shell 审批提示。
- **M3 第一切片**：持久 merge queue、确定性 Integrator、diff check、可选 integration command、失败回滚、冲突记录、Queue UI/CLI。
- **M3 第二切片**：queue rebase/retry、显式本地 landing。

## 如何运行

```bash
just bootstrap
just test
just build
just run
```

或单独运行 daemon：

```bash
just daemon
just cli ping
just cli project-add /path/to/repo --name demo
just cli session-new <project-id> "Your goal"
```

本地状态在 `~/Library/Application Support/VerlinPark/`；daemon 只绑定 `127.0.0.1`。

## 已知问题

- App 首次启动 daemon 失败时只在 Welcome/ErrorBanner 显示；日志在 `~/Library/Application Support/VerlinPark/logs/daemon.log`。
- WebSocket 断线重连没有单元测试。
- Anthropic 工具调用没有真实账号 live 验证。
- `agent.rs`、`StudioView.swift` 已经偏大，需要拆分（见 `docs/AUDIT.md`）。
