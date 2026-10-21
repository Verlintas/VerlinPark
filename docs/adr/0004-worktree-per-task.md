# ADR-0004: 每任务 worktree + 集成分支

- 状态：Accepted
- 日期：2026-10-21

## 背景

多个 Agent 并行改同一 checkout 会互相踩文件。共享 worktree + 文件锁实现复杂，回滚困难。

## 决策

- 每个 Task 一个 Git worktree + branch。
- Session 有一个 integration branch。
- Task 从 integration 分支创建；完成后进 merge queue。
- Integrator 负责 rebase/merge/集成测试。
- 不允许 Agent 直接 push 到 `main`；不允许 Builder 合并自己的分支。

## 理由

- 隔离彻底，回滚容易。
- 与 Claude Code/Zed/Conductor 的成熟实践一致。
- 合并路径明确，可审计。

## 后果

- 需要 worktree 生命周期管理、repo 级 Git 锁、冲突处理。
- 磁盘占用增加；需要清理策略。
- 写作用域仍要在计划阶段校验，避免无谓冲突。
