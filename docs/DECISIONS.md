# VerlinPark 决策记录

> 最后更新：2026-10-21  
> 格式：Architecture Decision Record（ADR）  
> 规则：已接受的决策不删除；如果改变，新增一条“Supersedes”记录。

---

## 决策总览

| ADR | 决策 | 状态 | 影响 |
|---|---|---|---|
| [0001](adr/0001-swiftui-rust-core.md) | SwiftUI + Rust Core | Accepted | 技术栈/构建/IPC |
| [0002](adr/0002-three-level-departments.md) | 三层部门制 | Accepted | 产品/调度/UX |
| [0003](adr/0003-manual-model-budget.md) | 手动模型/预算配置 | Accepted | 产品/成本/UI |
| [0004](adr/0004-worktree-per-task.md) | 每任务 worktree + 集成分支 | Accepted | Git/合并/隔离 |
| [0005](adr/0005-typed-messages-artifacts.md) | typed message + artifact | Accepted | Agent 通信/存储 |
| [0006](adr/0006-event-sourcing.md) | 事件溯源 + SQLite 投影 | Accepted | 存储/回放/恢复 |
| [0007](adr/0007-websocket-jsonrpc.md) | WebSocket JSON-RPC over loopback | Accepted | IPC/安全 |
| [0008](adr/0008-gpl3-direct-distribution.md) | GPL-3.0-only + 直接分发 | Accepted | 许可/发布 |
| [0009](adr/0009-api-first-mlx-later.md) | API 优先，MLX 后置 | Accepted | 模型/路线图 |
| [0010](adr/0010-v01-scope.md) | v0.1 范围 | Accepted | 范围/里程碑 |

---

## 待定决策

以下问题在 M0 前需要确认，或先采用默认值：

| 问题 | 默认/倾向 | 何时决定 |
|---|---|---|
| UI 视觉方向 | Park After Dark（深色工业 + 绿/黄铜） | M0 前 |
| 最低 macOS | 15+，Apple Silicon only | M0 |
| UI 语言 | 英文 UI，中文文档 | M0 |
| Hero demo 仓库 | 自建 `fixtures/tiny-webapp` | M1 前 |
| Apple Developer 账号/公证 | 有则 v0.1 公证；没有则 ad-hoc | M5 |
| CLI 范围 | `run/status/attach/diff/logs` | M3 |
| 遥测 | 默认关闭，永不默认开启 | M0 |
| 插件许可边界 | 独立进程/IPC；v0.3 前定 | v0.3 |
| 官网/docs site | 本仓库 docs 先够用 | v0.2 |
| 自动更新 | Sparkle，v0.2 | v0.2 |

---

## 如何新增 ADR

1. 复制 [docs/adr/0000-template.md](adr/0000-template.md) 或任一现有 ADR。
2. 编号递增；文件名 `NNNN-short-title.md`。
3. 包含：状态、日期、背景、决策、理由、后果、备选。
4. 更新本文件总览。
5. 如果推翻旧决策，写 `Supersedes ADR-XXXX`，旧 ADR 状态改为 `Superseded`。

---

## 决策原则

- 优先可逆决策；不可逆决策要更慢、更多证据。
- 优先确定性代码，而不是 LLM 自由发挥。
- 优先本地/隐私，而不是云便利。
- 优先用户显式控制，而不是自动魔法。
- 优先开放协议，而不是私有耦合。
- 任何新功能都要问：它是否服务于“多 Agent 团队 + 玻璃盒 + 可合并”？
