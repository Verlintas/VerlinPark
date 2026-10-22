# ADR-0002: 三层部门制

- 状态：Superseded by ADR-0011
- 日期：2026-10-21
- 备注：2026-10-23 新版设计改为四层 Leader → Manager → Worker → Labour，并引入预算门控自动扩编。本文保留作为历史记录。

## 背景

多 Agent 可以提升并行度，但协调成本和 token 消耗会快速上升。Anthropic 的实测显示多 Agent 研究任务可提升 90%+，但 token 约 15x；coding 任务真正可并行的部分更少。

候选：

1. 两层：Lead + 专家
2. 三层：Lead → Department Lead → Worker
3. 动态图 / Swarm

## 决策

默认采用 **三层部门制**，并施加硬限制：

- 深度固定 3 层
- 部门 ≤ 4
- 每部门 Worker ≤ 5
- 活跃 Agent ≤ 12
- 全局并发 task ≤ 6

## 理由

- 用户明确选择三层。
- 部门层可以承载领域知识（API/UI/质量），减少 Lead 的上下文负担。
- 硬限制防止组织膨胀；LLM 只提出计划，runtime 校验。

## 后果

- 需要 Plan schema、DAG 校验、部门级预算/权限。
- 协调开销高于两层；必须用 artifact/typed message 控制。
- 不追求自由 swarm；扩编需 Lead 修改计划 + runtime 重校验。
