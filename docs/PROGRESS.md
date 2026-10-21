# VerlinPark 进度

> 最后更新：2026-10-21  
> 当前里程碑：M0（walking skeleton）已完成，M1 待开始

---

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
- 没有 worktree、任务 DAG、部门/Agent 组织。
- 没有 Provider 设置 UI / Keychain；模型配置尚未落地。
- 没有终端、diff、Park Map。
- 没有签名/公证；App 是 unsigned debug build。
- 没有公网/远程能力，也不应该有。

## M1 下一步

1. 定义 `Agent` / `Task` / `Department` domain 与状态机。
2. 实现单 Agent loop（Provider → tool calls → final）。
3. 核心工具：read/list/glob/grep/apply_patch/write/shell/git/test。
4. 权限代理（allow/ask/deny；路径 jail；命令分类）。
5. 事件：`agent.*`、`llm.*`、`tool.*`、`approval.*`。
6. UI：Agent 对话、工具调用卡片、审批队列、trace 时间线。
7. 单 Agent 在 fixture repo 修一个 failing test；可回滚、可恢复。

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
