# VerlinPark 文档索引

> 最后更新：2026-10-31
> 仓库：<https://github.com/Verlintas/VerlinPark>

文档分成三类，避免把“设计目标”误读成“已经实现”：

| 类型 | 文档 | 用途 |
|---|---|---|
| **Current** | [PROGRESS.md](PROGRESS.md) | 当前真实可用的能力、限制、测试结果；状态以这里为准 |
| **Current** | [AUDIT.md](AUDIT.md) | 最近一次项目重审：P0/P1/P2 问题、证据、建议顺序 |
| **Design** | [ARCHITECTURE.md](ARCHITECTURE.md) | 目标架构、协议、数据模型、状态机；实现状态见 PROGRESS/AUDIT |
| **Design** | [PLAN.md](PLAN.md) | 长期产品计划、范围、里程碑、风险；保留早期设计章节作为背景 |
| **Reference** | [DECISIONS.md](DECISIONS.md) | Accepted ADR 索引；改架构先加 ADR |
| **Reference** | [RESEARCH.md](RESEARCH.md) | 2026-10 的竞品/技术调研；历史背景，不是当前状态 |
| **Reference** | [PRIDE_FLAGS.md](PRIDE_FLAGS.md) | Pride flag 资产来源与许可 |
| **Reference** | [../THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md) | FlagKit 等第三方资产许可 |
| **Contributing** | [../CONTRIBUTING.md](../CONTRIBUTING.md) | 构建、测试、提交约定 |
| **Agent rules** | [../AGENTS.md](../AGENTS.md) | 给 coding agent 的约束与当前里程碑 |

## 推荐阅读顺序

1. [README](../README.md) — 产品是什么、怎么跑。
2. [PROGRESS.md](PROGRESS.md) — 现在到底有什么。
3. [AUDIT.md](AUDIT.md) — 哪些地方还不可靠，下一步优先修什么。
4. [ARCHITECTURE.md](ARCHITECTURE.md) — 目标架构与不变量。
5. [DECISIONS.md](DECISIONS.md) — 已锁定的决策。
6. [PLAN.md](PLAN.md) — 长期路线与早期设计背景。

## 文档状态规则

- 行为变化：更新 `PROGRESS.md`，必要时加测试。
- 架构/协议/权限/预算语义变化：新增 ADR，并更新 `DECISIONS.md` 索引。
- `PLAN.md` 是长期设计文档，不要求每个实现切片都回写；每季度或大版本前做一次收敛。
- `README.md` 只放稳定的产品叙事和运行方式；细节链接到本目录。
- 任何“已实现”的表述必须有代码、测试或可复现命令支撑；否则写成 planned。
- 中英文可以混用；代码标识符、协议字段、事件名保持英文。

## ADR 快速索引

| 主题 | ADR |
|---|---|
| 技术栈 / IPC / 分发 | [0001](adr/0001-swiftui-rust-core.md)、[0007](adr/0007-websocket-jsonrpc.md)、[0008](adr/0008-gpl3-direct-distribution.md) |
| 组织模型 | [0011](adr/0011-four-level-org.md)（四层）、[0019](adr/0019-four-tier-delegation.md)（委派）、[0022](adr/0022-manager-titles.md)（Manager title）、[0024](adr/0024-team-plan-profiles.md)（TeamPlan/profile）、[0027](adr/0027-manager-roundtable.md)（圆桌） |
| 隔离 / Git | [0004](adr/0004-worktree-per-task.md)、[0016](adr/0016-session-worktree.md)、[0017](adr/0017-session-commit.md)、[0020](adr/0020-task-worktrees.md)、[0021](adr/0021-parallel-workers-scopes.md) |
| 质量 / 验证 | [0023](adr/0023-verifier-gate.md)（Verifier）、[0025](adr/0025-team-plan-dispatch.md)（自动派发）、[0028](adr/0028-merge-queue-integrator.md)（merge queue/Integrator）、[0029](adr/0029-local-landing.md)（本地 landing）、[0030](adr/0030-push-pr-flow.md)（push/PR）、[0031](adr/0031-queue-recovery.md)（queue recovery）、[0032](adr/0032-atomic-event-projections.md)（event/projection 原子写）、[0033](adr/0033-session-import-archive.md)（bundle 导入/archive）、[0034](adr/0034-cooperative-pause-resume.md)（pause/resume）、[0037](adr/0037-agent-assisted-conflict-resolution.md)（冲突自动解决）、[0038](adr/0038-policy-gated-auto-resolve.md)（opt-in 自动触发/caps） |
| 安全 / 权限 | [0035](adr/0035-shell-write-sandbox.md)（shell 写入沙箱）、[0036](adr/0036-project-shell-policy.md)（项目级 shell policy） |
| 成本 / 容量 | [0003](adr/0003-manual-model-budget.md)、[0018](adr/0018-budget-cancel.md)、[0026](adr/0026-org-cap-right-sizing.md)、[0039](adr/0039-global-session-cap-scope-locks.md)（global admission/scope locks）、[0040](adr/0040-per-role-models-usd-time-budgets.md)（per-role models/USD/time） |
| 产品 / UI | [0013](adr/0013-glass-park-brand.md)（Glass Park）、[0014](adr/0014-external-aviiya.md)（Aviiya 外置） |
| 通信 / 存储 | [0005](adr/0005-typed-messages-artifacts.md)（typed message/artifact，设计）、[0006](adr/0006-event-sourcing.md)（事件溯源，部分实现）、[0041](adr/0041-typed-mailbox-artifact-store.md)（mailbox/artifact 实现）、[0042](adr/0042-binary-artifacts-gc-retention.md)（binary/GC）、[0043](adr/0043-global-worker-labour-capacity.md)（global capacity）、[0044](adr/0044-per-call-usage-ledger.md)（usage ledger）、[0045](adr/0045-task-department-budgets.md)（budget tree） |

> `0005` 和 `0006` 是 Accepted 的架构方向，但当前实现只覆盖了一部分：text mailbox/artifact store 已实现，binary/GC/cross-session 未实现；事件日志目前更接近“audit stream + projection”，不是完整 replay 的 event sourcing。详见 [AUDIT.md](AUDIT.md)。
