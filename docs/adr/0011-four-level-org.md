# ADR-0011: 四层部门制与预算门控自动扩编

- 状态：Accepted（Supersedes ADR-0002）
- 日期：2026-10-23

## 背景

新版设计把 Agent 组织从三层扩展为四层：

- Leader（1）
- Manager（3–5）
- Worker（2–10）
- Labour（3–40）

并允许 Manager 自动招募 Worker、Worker 自动雇佣 Labour。原 ADR-0002 的三层/12 Agent 上限不再适用。

## 决策

1. 项目组织固定为四层：Leader → Manager → Worker → Labour。
2. 硬上限：1 / 5 / 10 / 40；任何自动扩编不能突破。
3. **预算门控自动扩编**：
   - 默认从最小团队开始（Leader + 1 Manager + 1 Worker + 1 Labour）。
   - 任务队列/依赖/并行度需要更多 Agent 时，逐级扩编。
   - 达到 Session 预算 80% 时冻结扩编；100% 时暂停并升级。
   - 用户可随时冻结/解冻扩编，或手动退休 Agent。
4. 每个 Agent 的模型和预算仍由用户显式配置；扩编不能偷偷换模型。
5. Worker 拥有自己的 worktree/branch；Labour 在 Worker 的 worktree 内执行子任务，每个子任务一个 checkpoint。
6. 组织可保存为 `OrgPreset`，支持导出/导入。

## 理由

- 四层可以承载“讨论多个 outline → 拆任务 → 执行”的分工。
- 自动扩编符合“auto recruit / auto hire”的设计意图。
- 一次性拉满 56 个 Agent 会烧 token 且难以控制；预算门控是必要的刹车。
- 保持确定性的 runtime 校验，而不是让 LLM 自由扩编。

## 后果

- 需要 Capacity Controller、扩编事件、预算冻结线、空闲退休。
- Collab 图必须支持 40 Labour 的聚合/折叠/虚拟化。
- 计划校验规则、UI 预设、成本报告都要按四层重写。
- 原三层模板 `Feature Crew` 被四层 `Feature Crew` 取代。
