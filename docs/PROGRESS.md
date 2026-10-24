# VerlinPark 进度

> 最后更新：2026-10-24  
> 当前里程碑：M1（单 Leader Agent runtime）进行中；M0.5 已完成

---

## M1 进行中（单 Leader Agent runtime）

| 项 | 状态 | 说明 |
|---|---|---|
| Leader loop | ✅ | `session.create` / `session.prompt` 启动；加载历史消息 → 流式 Provider → 顺序工具 → 最终回答；最大轮数硬停 |
| Provider 工具调用 | ✅ | OpenAI-compatible 按 index 重组流式 `tool_calls`；Anthropic `tool_use` + `input_json_delta` + `tool_result` 映射 |
| 只读工具 | ✅ | `read_file` / `list_dir` / `grep`；项目根路径 jail；输出 40k 截断 |
| 副作用工具 | ✅ | `write_file` / `shell`；必须审批；Shell 120s 默认/900s 上限，拒绝已知危险模式 |
| 审批队列 | ✅ | SQLite `approvals` + `approval.requested/resolved` 事件；UI 卡片 Approve/Deny；CLI `approve/deny`；10 分钟超时视为拒绝 |
| Agent 事件 | ✅ | `agent.started/message_delta/message/turn_finished/failed/unavailable`、`tool.started/finished`、`agent.usage` |
| 对话 UI | ✅ | 用户/Leader 气泡、流式光标、工具卡片、错误卡片、审批卡片、会话状态与模型标识 |
| Provider 设置 | ✅ | Settings → Providers：API、Base URL、Model、API key；Keychain 存 key；App 启动时 `agent.configure` |
| Headless 配置 | ✅ | `VERLINPARK_BASE_URL` / `VERLINPARK_MODEL` / `VERLINPARK_API_KEY` 环境变量兜底 |
| CLI | ✅ | 新增 `prompt` / `approvals` / `approve` / `deny` |
| 测试 | ✅ | Rust 17 个：工具路径 jail、审批持久化/单次解决、Leader 工具调用闭环、Provider 配置、OpenAI/Anthropic 流式工具解析；Swift 9 个：品牌/旗帜/JSON |
| 手工 E2E | ✅ | mock SSE：configure → session → read_file → approval → write_file → final；App 内同样走通 |

**M1 尚未完成：**

- 每任务 worktree / 分支 / merge queue；当前单 Leader 直接写项目根，多 Agent 前必须替换。
- 完整权限代理（角色/命令分类/路径策略/预算）；当前只有路径 jail + 写/Shell 审批 + 简单拒绝列表。
- Token/成本预算硬停、上下文压缩、运行恢复、取消/暂停。
- 只读工具并行、MCP/ACP、diff 视图、终端、真实 Collab 节点。
- 四层组织 runtime、Capacity Controller、真实 Computer Lease。
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
