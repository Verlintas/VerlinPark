# ADR-0012: Computer Lease 只串行 GUI / 浏览器

- 状态：Accepted
- 日期：2026-10-23

## 背景

新版设计要求“同一时间只有一个 Agent 使用 Computer”，防止 GUI 自动化、浏览器操作互相冲突。但 shell、Git、文件编辑在隔离 worktree 中可以并行。

## 决策

- 全局 `ComputerLease` 只覆盖 **GUI 自动化与浏览器**（以及未来同类 computer-use surface）。
- `shell`、`read/search`、`apply_patch`、`git` 在各自 worktree 中并行，不占用 lease。
- lease 粒度是单次工具调用，不是整个 Agent turn。
- 队列使用优先级 + aging，避免 Labour 饿死；支持超时、心跳、取消、强制回收。
- 所有 acquire/grant/release/deny 写事件日志。
- UI 在 Collab 和 Manage Dash 显示当前 holder 与队列。

## 理由

- GUI/浏览器是真正会争抢鼠标、键盘、窗口焦点的资源。
- shell/文件操作有 worktree 隔离，串行会严重降低多 Agent 效率。
- 工具级 lease 比 Agent 级 lease 更细，减少空占。

## 后果

- 需要 `ComputerLease` 服务、队列、优先级/aging、事件和 UI。
- Orca/OpenCode 等未来 computer backend 也必须走同一个 lease。
- shell 并行仍需要写作用域和 Git 锁，不能把 lease 当作唯一并发控制。
