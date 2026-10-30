# VerlinPark 决策记录

> 最后更新：2026-10-30
> 格式：Architecture Decision Record（ADR）  
> 规则：已接受的决策不删除；如果改变，新增一条“Supersedes”记录。

---

## 决策总览

| ADR | 决策 | 状态 | 影响 |
|---|---|---|---|
| [0001](adr/0001-swiftui-rust-core.md) | SwiftUI + Rust Core | Accepted | 技术栈/构建/IPC |
| [0002](adr/0002-three-level-departments.md) | 三层部门制 | Superseded by 0011 | 产品/调度/UX |
| [0003](adr/0003-manual-model-budget.md) | 手动模型/预算配置 | Accepted | 产品/成本/UI |
| [0004](adr/0004-worktree-per-task.md) | 每任务 worktree + 集成分支 | Accepted | Git/合并/隔离 |
| [0005](adr/0005-typed-messages-artifacts.md) | typed message + artifact | Accepted | Agent 通信/存储 |
| [0006](adr/0006-event-sourcing.md) | 事件溯源 + SQLite 投影 | Accepted | 存储/回放/恢复 |
| [0007](adr/0007-websocket-jsonrpc.md) | WebSocket JSON-RPC over loopback | Accepted | IPC/安全 |
| [0008](adr/0008-gpl3-direct-distribution.md) | GPL-3.0-only + 直接分发 | Accepted | 许可/发布 |
| [0009](adr/0009-api-first-mlx-later.md) | API 优先，MLX 后置 | Accepted | 模型/路线图 |
| [0010](adr/0010-v01-scope.md) | v0.1 范围 | Accepted | 范围/里程碑 |
| [0011](adr/0011-four-level-org.md) | 四层组织 + 预算门控自动扩编 | Accepted | 产品/调度/UX |
| [0012](adr/0012-computer-lease.md) | Computer Lease 只串行 GUI/浏览器 | Accepted | 执行/并发 |
| [0013](adr/0013-glass-park-brand.md) | Glass Park UI + 可配置品牌 | Accepted | UI/品牌 |
| [0014](adr/0014-external-aviiya.md) | Aviiya 是 NovaBAIC 外部常驻 Agent | Accepted | 集成边界 |
| [0015](adr/0015-m1-leader-loop.md) | M1 单 Leader loop、审批与 Provider 配置 | Accepted | Agent runtime/安全/UI |
| [0016](adr/0016-session-worktree.md) | 每个 session 一个 Git worktree | Accepted | 隔离/Git/工具路径 |
| [0017](adr/0017-session-commit.md) | 从 Review 显式提交 worktree | Accepted | 审阅/Git/UX |
| [0018](adr/0018-budget-cancel.md) | Session token 预算 + 协作式取消 | Accepted | 成本/控制/可靠性 |
| [0019](adr/0019-four-tier-delegation.md) | 四层递归委派（串行、同 worktree） | Accepted | 组织/Agent runtime/隔离 |
| [0020](adr/0020-task-worktrees.md) | Worker 任务 worktree + 串行集成 | Accepted | 隔离/Git/审阅 |
| [0021](adr/0021-parallel-workers-scopes.md) | 并行 Worker + write scope 校验 | Accepted | 调度/并发/安全 |
| [0022](adr/0022-manager-titles.md) | Manager 必须有部门角色 title | Accepted | 组织/委派/UI |
| [0023](adr/0023-verifier-gate.md) | Verifier gate：审查通过才合并 Worker 分支 | Accepted | 质量/集成/安全 |
| [0024](adr/0024-team-plan-profiles.md) | Leader 定义 TeamPlan + AgentProfile/research | Accepted | 组织/平台化/工具 |
| [0025](adr/0025-team-plan-dispatch.md) | TeamPlan 自动派发 + 放宽固定 caps | Accepted; caps superseded by 0026 | 调度/效率/预算 |
| [0026](adr/0026-org-cap-right-sizing.md) | 默认小团队 + Manager 懒创建 | Accepted | 容量/成本/UX |
| [0027](adr/0027-manager-roundtable.md) | Manager 圆桌商讨（1–3 轮、只读） | Accepted | 组织协作/计划质量 |
| [0028](adr/0028-merge-queue-integrator.md) | 持久 merge queue + 确定性 Integrator gate | Accepted | 集成/质量/回滚 |
| [0029](adr/0029-local-landing.md) | 显式本地 landing；PR 后置 | Accepted | 交付/安全/UX |
| [0030](adr/0030-push-pr-flow.md) | 显式 push + GitHub PR（gh） | Accepted | 交付/集成/安全 |
| [0031](adr/0031-queue-recovery.md) | daemon 重启恢复 in-flight merge queue entry | Accepted | 可靠性/恢复/审计 |

---

## 待定决策

以下问题在 M0 前需要确认，或先采用默认值：

| 问题 | 默认/倾向 | 何时决定 |
|---|---|---|
| Aviiya / NovaBAIC 桥接协议 | 不在 VerlinPark 实现 Aviiya；等 NovaBAIC 协议 | NovaBAIC 稳定后 |
| 四层扩编默认值 | 已定：1 Leader / 4 Managers / 6 Workers / 12 Labour；runtime caps 5 / 10 / 40；自动扩编仍未实现 | 已定（ADR-0026） |
| 最低 macOS | 15+，Apple Silicon only | M0 |
| UI 语言 | 英文 UI，中文文档 | M0 |
| Hero demo 仓库 | 自建 `fixtures/tiny-webapp` | M1 前 |
| Apple Developer 账号/公证 | 有则 v0.1 公证；没有则 ad-hoc | M4 |
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
