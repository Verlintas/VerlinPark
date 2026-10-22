# VerlinPark 进度

> 最后更新：2026-10-23  
> 当前里程碑：M0.5（Glass UI + 品牌 + 四层设计预览）已完成，M1 待开始

---

## M0.5 完成情况

| 项 | 状态 | 说明 |
|---|---|---|
| Glass Park 三栏 | ✅ | Rail（品牌/项目/Manage Dash/Settings/内存）· Stage（对话/会话）· Studio（Collab/Preview） |
| 毛玻璃 | ✅ | 隐藏 titlebar、behind-window blur、material 面板、深色优先 |
| 可配置品牌 | ✅ | displayName + 预设；solid / gradient / flag；持久化 |
| 国家旗 | ✅ | FlagKit v2.5.0，248 个国家/地区（MIT） |
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

## 当前限制（M1 要解决）

- 没有 Agent loop、工具、权限、上下文、trace 的 Agent 语义；事件目前只有 `session.created`。
- 没有四层组织的 runtime、Capacity Controller、Computer Lease 实现；右侧是设计预览。
- 没有 Provider 设置 UI / Keychain；模型配置尚未落地。
- 没有终端、diff、真实 Collab 节点。
- Aviiya 是外部 NovaBAIC bridge 占位，当前不可聊天，也不应在 VerlinPark 内实现人格/记忆。
- 没有签名/公证；App 是 unsigned debug build。
- 没有公网/远程能力，也不应该有。

## M1 下一步

1. 定义 `Agent` / `Task` / `Manager` / `Worker` / `Labour` domain 与状态机。
2. 实现 Leader 单 Agent loop（Provider → tool calls → final）。
3. 核心工具：read/list/glob/grep/apply_patch/write/shell/git/test。
4. 权限代理（allow/ask/deny；路径 jail；命令分类）。
5. 事件：`agent.*`、`llm.*`、`tool.*`、`approval.*`。
6. UI：Leader 对话、工具调用卡片、审批队列、真实 Collab 节点。
7. 单 Agent 在 fixture repo 修一个 failing test；可回滚、可恢复。
8. 为 NovaBAIC Aviiya 定义桥接协议（只定义接口，不实现人格/记忆）。

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
