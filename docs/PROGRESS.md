# VerlinPark 进度

> 最后更新：2026-10-25  
> 当前里程碑：M2 四层 Agent runtime 第一切片进行中；M0 / M0.5 / M1 已完成

---

## M2 进行中（四层委派）

| 项 | 状态 | 说明 |
|---|---|---|
| 四层委派 | ✅ | Leader → Manager → Worker → Labour 严格链路；`delegate` 工具；嵌套同步执行；越级/越权被拒绝 |
| 角色工具策略 | ✅ | Manager 只读（read/list/grep）；Worker/Labour 可读写/Shell；Leader 全部工具 |
| Agent roster | ✅ | `agents` 表持久化 role/name/title/parent/status/model；`agent.list` RPC；Collab 显示真实 Managers/Workers/Labour |
| Manager 角色 | ✅ | Leader 委派 Manager 时必须给 `title`（Discovery / Implementation / Quality 等）；title 持久化、显示在 Collab/审批卡/CLI |
| 委派上限 | ✅ | 按 run 限制 1–5 Managers / 1–10 Workers / 1–40 Labour；由 Org Preset 传给 `agent.configure` |
| 任务 worktree | ✅ | Worker 独立 worktree/branch；Labour 继承 Worker worktree；Leader/Manager 用 session worktree |
| 串行集成 | ✅ | Worker 完成后内部 commit + `--no-ff` 合并到 session 分支；冲突 abort、标记 Worker failed、保留分支 |
| Base diff | ✅ | session 记录 base_commit；diff 对比 base..worktree，已合并/已提交的 Worker 改动仍可审阅 |
| 并行 Worker | ✅ | Manager `delegate_many` 一次启动 1–5 个 Worker；独立 task worktree；`join_all` 并发执行，完成后按任务顺序串行集成 |
| Write scope | ✅ | 并行任务必须声明不重叠 `write_scope`；集成前检查 tracked + untracked 变更，越界 Worker 失败且不合并 |
| Verifier gate | ✅ | Worker 内部 commit 后启动独立 Verifier（只读 + shell）；`submit_verdict` 结构化结论；approve 才 merge，reject 标记 changes_requested 并保留分支 |
| TeamPlan | ✅ | Leader `submit_team_plan` 定义 departments/managers/tasks；runtime 校验 caps、唯一 title、profile、DAG、acceptance；计划持久化并在 Collab 显示 |
| Agent Profile | ✅ | 结构角色与领域 profile 分离：coding / research / verification / generic；profile 决定工具集与 prompt |
| Research 工具 | ✅ | `fetch_url`：仅公网 HTTP(S)、阻止 localhost/private IP、不跟随重定向、超时与输出截断 |
| TeamPlan 自动派发 | ✅ | `run_team_plan` 按依赖批次并发执行 ready tasks；自动创建 Manager/Worker/task worktree；Verifier gate + 集成；失败阻塞依赖；任务/计划事件可审计 |
| 容量放宽 | ✅ | 默认 8 Managers / 32 Workers / 128 Labour；可配置到 32/128/512；plan 最多 500 tasks；Verifier cap 1000；token 预算仍是主硬停 |
| 审批归属 | ✅ | 审批事件带 `agent_id/agent_name/agent_role`；审批卡显示发起 Agent |
| 边界 | ✅ | 串行、同一 session worktree；per-task worktree/并行/merge queue 尚未实现 |

## M1 完成情况（单 Leader Agent runtime）

| 项 | 状态 | 说明 |
|---|---|---|
| Leader loop | ✅ | `session.create` / `session.prompt` 启动；加载历史消息 → 流式 Provider → 顺序工具 → 最终回答；最大轮数硬停 |
| Session worktree | ✅ | 每个 session 自动创建链接 worktree + `verlinpark/<session-id>` 分支；Agent 只能写 worktree；旧 session 首次运行时自动补建 |
| Provider 工具调用 | ✅ | OpenAI-compatible 按 index 重组流式 `tool_calls`；Anthropic `tool_use` + `input_json_delta` + `tool_result` 映射 |
| 只读工具 | ✅ | `read_file` / `list_dir` / `grep`；worktree 路径 jail；输出 40k 截断 |
| 副作用工具 | ✅ | `edit_file` / `write_file` / `shell`；必须审批；Shell 120s 默认/900s 上限，拒绝已知危险模式 |
| 审批队列 | ✅ | SQLite `approvals` + `approval.requested/resolved` 事件；UI 卡片 Approve/Deny；CLI `approve/deny`；10 分钟超时视为拒绝 |
| Token 预算 | ✅ | session 级 `token_budget` / `tokens_in` / `tokens_out`；每轮 usage 持久化 + `agent.usage`；超限 `agent.budget_exceeded` 并硬停；UI 显示 used / budget |
| 取消运行 | ✅ | `session.cancel` + Stop 按钮 + `vp cancel`；流式、工具、审批等待都可中断；取消后 session 标为 cancelled；不声称 pause/resume |
| 崩溃恢复 | ✅ | daemon 启动时把遗留 `running` session 标为 failed、pending approval 标为 denied，并补事件；不会假装运行还在继续 |
| 错误反馈 | ✅ | App 顶部 ErrorBanner 显示 daemon/项目/会话错误，可关闭 |
| Agent 事件 | ✅ | `agent.started/message_delta/message/turn_finished/failed/unavailable`、`tool.started/finished`、`agent.usage` |
| 对话 UI | ✅ | 用户/Leader 气泡、流式光标、工具卡片、错误卡片、审批卡片、会话状态与模型标识 |
| Leader 计划 | ✅ | `todo_write` 工具持久化 todo（pending / in_progress / completed）；`todo.updated` 事件；Collab 显示真实进度；CLI `vp todos` |
| Diff 审阅 | ✅ | `session.diff` RPC；Preview → Changes 显示 status + 彩色 patch；CLI `vp diff`；200k 上限 |
| Worktree commit | ✅ | Preview → Commit；`session.commit` 在 worktree 内 `git add -A` + commit；`session.committed` 事件；CLI `vp commit`；不做自动提交 |
| 撤销改动 | ✅ | Preview → 垃圾桶；`session.discard` 在 worktree 内 reset --hard + clean -fd（保留 commit）；CLI `vp discard`；二次确认 |
| Provider 设置 | ✅ | Settings → Providers：API、Base URL、Model、API key；Keychain 存 key；App 启动时 `agent.configure` |
| Headless 配置 | ✅ | `VERLINPARK_BASE_URL` / `VERLINPARK_MODEL` / `VERLINPARK_API_KEY` 环境变量兜底 |
| CLI | ✅ | 新增 `prompt` / `approvals` / `approve` / `deny` |
| 测试 | ✅ | Rust 44 个：session/task worktree、串行集成、并行 Worker、write scope 拒绝、Verifier approve/reject、TeamPlan 校验/循环拒绝/自动派发、Manager title 必填、base diff、旧库迁移、中断恢复、commit/discard、edit_file、预算/取消、todo、四层委派、Agent roster、审批归属、工具路径 jail、Leader 工具调用闭环、Provider 配置、OpenAI/Anthropic 流式工具解析；Swift 9 个 |
| 手工 E2E | ✅ | mock SSE：configure → session/worktree → read_file → approval → write_file → final；Preview 显示 worktree diff；项目根保持干净 |

**M2 尚未完成：**

- 全局跨 session 调度器 / merge queue / Integrator；当前 TeamPlan 可按依赖批次自动派发，但 Manager 在自动路径中是确定性部门节点，集成仍串行。
- 完整权限代理（命令分类/路径策略）；当前 Manager 只读、Worker/Labour 可写，写/Shell 仍需审批。
- 上下文压缩、pause/resume、checkpoint（取消与 token 硬停已落地）。
- 只读工具并行、MCP/ACP、逐 hunk review、终端、真实 Collab 节点。
- Capacity Controller、并行 Worker、真实 Computer Lease；当前四层委派是串行同步执行。
- Anthropic 工具调用只做了 mock 测试，尚无真实账号 live 验证。

## M0.5 完成情况

| 项 | 状态 | 说明 |
|---|---|---|
| Glass Park 三栏 | ✅ | Rail（品牌/项目/Manage Dash/Settings/内存）· Stage（对话/会话）· Studio（Collab/Preview） |
| 毛玻璃 | ✅ | 隐藏 titlebar、behind-window blur、material 面板、深色优先 |
| 可配置品牌 | ✅ | displayName + 预设；solid / gradient / flag 镂空 wordmark；持久化 |
| 地区旗 | ✅ | FlagKit v2.5.0，248 个地区（MIT） |
| Pride 旗 | ✅ | 20 面 Wikimedia Commons 标准资产：Rainbow/Progress/Intersex-Inclusive Progress/Trans/Bi/Lesbian/NB/Pan/Ace/Aro/Aroace/Genderfluid/Genderqueer/Intersex/Agender/Demi/Poly/Sapphic/Queer/Two-Spirit |
| 四层组织预览 | ✅ | Leader / Managers 3–5 / Workers 2–10 / Labour 3–40；auto-recruit / auto-hire 占位 |
| Computer Lease 展示 | ✅ | 明确“只串行 GUI/浏览器”，shell/worktree 并行 |
| Org Preset | ✅ | 本地保存扩编上限、自动扩编开关、预算冻结线（runtime 在 M2） |
| Manage Dash | ✅ | 项目/会话/事件、Agent 概览、Computer、内存、品牌、Resident |
| 真实内存 | ✅ | app + daemon RSS，2s 刷新 |
| Aviiya 外置 | ✅ | 不做本地人格/记忆/聊天；resident 入口为 NovaBAIC bridge 占位 |
| 测试 | ✅ | Rust 8 + Swift 品牌/JSON 测试；`xcodebuild test` 通过 |

## M0 完成情况

| 项 | 状态 | 说明 |
|---|---|---|
| Cargo workspace | ✅ | `vp-protocol` / `vp-store` / `vp-providers` / `vp-core` / `vp-daemon` / `vp-cli` |
| Daemon IPC | ✅ | HTTP JSON-RPC（CLI）+ WebSocket JSON-RPC（App）；loopback + token |
| Discovery | ✅ | `daemon.json`，0600；随机端口；健康检查 |
| SQLite | ✅ | projects / sessions / events；WAL；迁移在 `Store::open` |
| 事件日志 | ✅ | 每个 session 的 `events.jsonl` 镜像 + SQLite 投影 |
| 事件广播 | ✅ | WebSocket `event.subscribe` / `event.appended`；App 实时刷新 |
| CLI | ✅ | `ping` / `info` / `projects` / `project-add` / `sessions` / `session-new` / `events` |
| Provider | ✅ | OpenAI-compatible + Anthropic SSE，流式解析 + usage；wiremock 测试 |
| SwiftUI shell | ✅ | 三栏布局；项目/会话；事件时间线；连接状态；添加项目；新建会话 |
| App ↔ daemon | ✅ | App 自动启动 `verlinpadd`；App 退出 daemon 继续；重新打开恢复 |
| 测试 | ✅ | Rust 单元/集成 8 个；Swift 2 个；`cargo test` + `xcodebuild test` 通过 |
| CI | ✅ | GitHub Actions：fmt/clippy/test + XcodeGen/xcodebuild |
| 打包 helper | ✅ | `just bundle` 把 `verlinpadd` 放进 `VerlinPark.app/Contents/Helpers` |

## 已验证的手工场景

1. 启动 daemon，`vp ping` 返回 `pong`。
2. 添加当前仓库为 project，创建 session，`events` 能看到 `session.created`。
3. 打开 `VerlinPark.app`，显示 `daemon connected`。
4. App 自动启动 daemon；项目/会话/事件从 SQLite 恢复。
5. 运行中从 CLI 创建第二个 session，App 通过 WebSocket 实时更新会话列表。

## 当前限制

- 没有四层组织的 runtime、Capacity Controller、Computer Lease 实现；右侧是设计预览。
- 没有 worktree 隔离、diff 视图、终端、真实 Collab 节点。
- 没有预算硬停、上下文压缩、运行中恢复/取消。
- Aviiya 是外部 NovaBAIC bridge 占位，当前不可聊天，也不应在 VerlinPark 内实现人格/记忆。
- 没有签名/公证；App 是 unsigned debug build。
- 没有公网/远程能力，也不应该有。

## M1 剩余 / M2 前置

1. 每任务 worktree + 分支；写作用域锁；merge queue 的最小可用版本。
2. 完整权限代理（allow/ask/deny；命令分类；角色/路径策略）。
3. 预算硬停、上下文压缩、取消/暂停、daemon 重启后的运行恢复。
4. `apply_patch` / `git` / `test` 工具；只读工具并行；diff/终端 UI。
5. Agent/Task 状态机与四层组织的 runtime、Capacity Controller。
6. 单 Leader 在 fixture repo 修一个 failing test；可回滚、可恢复。
7. 为 NovaBAIC Aviiya 定义桥接协议（只定义接口，不实现人格/记忆）。

## 如何运行

```bash
just bootstrap
just daemon          # terminal 1
just cli ping        # terminal 2
just build
just run
```

或让 App 自动启动 daemon（debug build 会在 `target/debug/verlinpadd` 查找）。

## 已知问题

- `VerlinPark.app` 是 unsigned debug build；直接 `open` 可运行，但分发前需要 Developer ID + notarize。
- App 首次启动时如果 daemon 启动失败，错误只在 Welcome 页显示；日志在 `~/Library/Application Support/VerlinPark/logs/daemon.log`。
- 没有单元测试覆盖 WebSocket 断线重连；M1 补。
