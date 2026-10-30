# VerlinPark 进度

> 最后更新：2026-10-31
> 当前阶段：M2 四层 runtime + M2.5 硬化 + Merge Queue/Integrator/Retry/Conflict Resolve + Opt-in Auto-Resolve + Global Session Queue/Scope Locks + Per-role Models/USD/Time Budgets + Landing/PR + Session 协作式 Pause/Resume + Shell 沙箱/Policy 已跑通
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
| Verifier gate | ✅ | 独立 Verifier；`submit_verdict`；approve 才合并。只读是 prompt 约束；shell 写入被限制在其 worktree，但仍可修改该 worktree；审批卡会提示 |
| Merge queue | ✅ | Verifier-approved commit 入持久队列；`merge_queue.list` / `vp queue` / Preview → Queue；状态 queued/merging/checking/merged/conflict/failed/interrupted |
| Integrator gate | ✅ | 确定性 merge：`--no-ff` + `git diff --check` + 可选 `VERLINPARK_INTEGRATION_COMMAND`；失败 reset 到 merge 前 commit；冲突 abort 并保留 Worker 分支 |
| Queue retry | ✅ | `merge_queue.retry` / `vp retry` / Queue UI：conflict/failed 先 rebase 到当前 session branch，再重新 merge；attempt +1 |
| Conflict resolution | ✅ | `merge_queue.resolve` / `vp resolve` / Queue UI Auto-resolve：resolver agent 在 Worker worktree 解决 rebase 冲突；runtime 检查 markers、完成 rebase、独立 Verifier 复核、再走确定性 Integrator；失败回到 conflict |
| Auto-resolve policy | ✅ | 项目级 `IntegrationPolicy`：默认关闭；`max_auto_attempts` 0–5；`max_resolve_tokens` 1000–10M；daemon watcher 在 session idle / 启动时扫描 conflict，持久 attempt 计数 + per-resolution token cap；RPC/CLI/Manage Dash |
| Landing | ✅ | `session.land` / `vp land` / Preview → Land：clean + default branch 检查后 `--no-ff` 合入项目默认分支；不 push、不建 PR |
| PR flow | ✅ | `session.pr` / `vp pr` / Preview → Open PR：preflight origin + `gh auth status`，push session branch，`gh pr create`；gh 失败会明确报告 branch 已 push |
| Queue recovery | ✅ | daemon 重启把 queued/merging/checking entry 标为 interrupted 并补事件；interrupted 可 rebase+retry |
| Diff / commit | ✅ | base..worktree diff、显式 commit、discard、session committed 事件 |
| Shell 写入沙箱 | ✅ | macOS Seatbelt（`sandbox-exec`）：只允许写 worktree、对应 Git 元数据、per-agent scratch 和 `/dev/null`；拒绝 common hooks/config 与 worktree `config.worktree`；`.git` 指向校验；scratch 承载 TMPDIR/构建缓存；env allowlist 不继承 provider/daemon secrets；常见 credential/state 路径 read-deny；timeout/cancel 杀整个 process group；项目级 `ShellPolicy` 可拒绝网络并追加 read-deny；sandbox 不可用时 fail closed |
| 预算 / 取消 / 暂停 | ✅ | token reservation + provider usage 结算；无 usage 端点使用 `estimated: true` 估算；Stop、CLI cancel、崩溃恢复；`session.pause` / `session.resume` / `vp pause` / `vp resume` 在 turn/tool 边界协作式暂停并继续同一 run |
| 审批 | ✅ | 写/Shell 审批；事件带发起 Agent；UI Approve/Deny |
| 可观测性 | ✅ | agent/tool/approval/plan_task/manager.roundtable/merge_queue/integrator/landing/PR 事件；Collab roster；Preview Plan/Changes/Queue/Trace |
| Event consistency | ✅ | session status / usage / agent status / team plan / todos / messages / merge queue / agent roster / recovery 使用 event+projection 原子事务；`session.replay` / `--repair` / `--rebuild` 可校验并重建覆盖投影；`session.export` / `session.import` 支持 bundle 迁移 |
| Import archive | ✅ | `vp import <bundle>` 保留 session/project/事件/approvals/投影；导入 session 为只读 archive，不创建 worktree |
| Rehydrate | ✅ | `vp rehydrate <session> [--worktree path]`：only imported；校验 project/base commit；创建/attach session worktree；按 branch 分组重建 imported agent task worktrees；状态回到 idle；返回 warnings/remapped count |
| 容量 | ✅ | 默认 4 Managers / 6 Workers / 12 Labour；runtime caps 5/10/40；plan ≤100 tasks；`delegate_many` ≤10；Worker/Labour 用 active slot，plan batch 按可用容量选择 |
| Global scheduling | ✅ | daemon-wide session semaphore：`VERLINPARK_GLOBAL_SESSION_CAP` 默认 4（1–32）；超出时 session 状态 `queued`，`agent.queued`，可 cancel；跨 session Worker/Labour write-scope 锁按 project 串行等待，同 session 共享，空 scope 视为 wildcard |
| Provider | ✅ | OpenAI-compatible + Anthropic（Anthropic 工具调用仅 mock 验证） |
| Models / budgets | ✅ | 分角色 model override + input/output USD/1M price；session token / USD / wall-clock budget；cost 持久化 + `agent.cost`；time 在 turn/tool 边界检查；Providers/New Session/CLI/UI |
| CLI | ✅ | ping/info/projects/shell-policy/shell-policy-set/integration-policy/integration-policy-set/session-new/prompt/events/approvals/approve/deny/diff/commit/discard/cancel/pause/resume/todos/agents/team-plan/queue/retry/resolve/land/pr/replay [--repair|--rebuild]/export/import/rehydrate |

## 验证

- `cargo test --workspace`：78 个测试通过（core 61、store 12、providers 5）。
- `xcodebuild test`：9 个 Swift 测试通过。
- `just lint`：fmt + clippy `-D warnings` + swiftformat lint 通过。
- 最近 CI：GitHub Actions success。
- 手工 E2E 已覆盖：Leader/Manager/Worker/Labour 委派、task worktree、并行 Worker、Verifier approve/reject、TeamPlan 自动派发、Manager 圆桌、审批、diff、commit/discard、预算/取消/暂停恢复、项目 shell policy get/set。

## 已知 enforcement 缺口

第一轮硬化（2026-10-30）已修复 plan scope/caps、token reservation 和 fetch_url 的 DNS 绕过。仍然存在的：

1. **Shell 仍非完整沙箱**：写入已由 Seatbelt 限制在 worktree/Git 元数据/scratch；常见 credential/state 路径已 read-deny，但不是完整 read jail；项目 policy 可 deny network，但默认 allow，Docker 等 daemon 介导写入仍可能；`sandbox-exec` 已废弃。见 AUDIT P1-3。
2. **Verifier 只读**：只是 prompt 约束；shell 写入虽被限制在它自己的 worktree，但仍可修改该 worktree 的源码。见 AUDIT P1-3。
3. **事件溯源**：关键状态已改为原子事件+投影；`session.replay` / `--repair` / `--rebuild` 可发现并重建覆盖的投影（含 agent roster）；`session.export` 可导出 bundle；数据库项目/会话创建元数据的从零导入仍未实现。见 AUDIT P1-5。
4. **batch abort 状态一致性**：同批 task 在预算/取消中止时可能残留 `running` UI 状态；见 AUDIT P2-7。

## 当前限制（明确未实现）

- PR flow 依赖本机 `gh` 且只覆盖 GitHub；其他 host/PR provider 还没有 adapter。
- merge queue 目前是 session 内、逐条 drain；显式 retry/auto-resolve 是人工触发；自动触发必须由项目 policy 显式开启，并有 attempt/token cap；没有 LLM Integrator。
- Global session cap 是进程内 semaphore，不是持久队列/公平调度器；scope locks 只覆盖 Worker/Labour，daemon 重启会失败 in-flight runs；Leader/Manager/Verifier 写入不在全局 scope 锁内。
- 如果 daemon 在 git merge 过程中崩溃，session worktree 可能仍需人工检查/abort；recovery 不会猜测部分 merge 的结果。
- 自动派发中的 Manager 是确定性部门节点；LLM 圆桌只在 `deliberate_team_plan` 中运行。
- 没有完整全局 Task-DAG 调度器/公平队列、task retry UI、单 task cancel/pause；daemon-wide session admission 与 Worker/Labour scope locks 已实现。
- 没有 auto-expansion；Manager/Worker 上限是固定 caps，不是按预算自动扩编。
- 没有 per-role token budget / task/department budget tree；USD 预算依赖用户手填价格，时间预算只在 turn/tool 边界检查，不能抢占 in-flight provider/tool。
- 没有 typed message bus / artifact store；Agent 之间仍是委派调用 + 事件记录，没有异步 mailbox。
- 没有 context compaction、checkpoint/resume；暂停状态只在内存中，daemon 重启会把 running/paused session 标为 failed。
- 没有终端、逐 hunk review、真实 Collab 节点、MCP/ACP、web search。
- Shell 只做写入隔离与常见 credential read-deny；项目 policy 只能收窄不能放宽、只对后续 run 生效，且不随 session bundle 导出；不是完整 read jail，network allow 下仍可外发，`sandbox-exec` 是废弃 API；task worktree/branch/shell scratch 暂无自动清理。
- `IntegrationPolicy` 与 auto-resolve attempt 计数是项目/队列元数据，不随 session bundle 导出；自动触发必须有 provider 配置，且仍受 session token budget 与 per-resolution cap 限制。
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
- **M3 第三切片**：显式 push + `gh` PR flow（preflight、no fake success、事件/CLI/UI）。
- **M3 第四切片**：daemon 重启后的 in-flight merge queue recovery。
- **M3 第五切片**：关键状态 event+projection 原子写与事件重建测试。
- **M3 第六切片**：team plan / todos / messages / recovery 原子化，`session.replay` / `vp replay` 一致性检查与 `--repair`。
- **M3 第七切片**：`agent.snapshot` 完整 roster 事件与 agent 表 repair。
- **M3 第八切片**：`session.export` bundle 与 `--rebuild` 强制投影重建。
- **M3 第九切片**：`session.import` bundle archive 导入（只读、不自动创建 worktree）。
- **M3 第十切片**：`session.rehydrate` 将 imported session 的 worktree/branch 重建并恢复可运行状态；import 同时恢复 approvals 投影。
- **M3 第十一切片**：rehydrate 按 branch 分组重建 imported agent task worktrees。
- **M3 第十二切片**：session 级协作式 pause/resume（turn/tool 边界、同一 run 继续、terminal 状态不可覆盖），CLI + app 控制。
- **M3 第十三切片**：macOS Seatbelt shell 写入沙箱（worktree + Git 元数据 + scratch；hooks/config 保护；fail closed）。
- **M3 第十四切片**：shell credential/state read-deny + process group 清理（timeout/cancel 不遗留后台进程）。
- **M3 第十五切片**：项目级 shell policy（network allow/deny、额外 read-deny、RPC/CLI/Manage Dash）。
- **M3 第十六切片**：Queue 冲突的 agent-assisted auto-resolve（rebase、marker 检查、独立 Verifier、重新 Integrator）。
- **M3 第十七切片**：项目级 opt-in auto-resolve policy + persisted attempt cap + per-resolution token cap。
- **M3 第十八切片**：daemon-wide session admission（queued sessions）+ 跨 session Worker/Labour write-scope locks。
- **M3 第十九切片**：分角色模型覆盖 + per-role USD pricing + session USD/wall-clock budgets。

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
