# ADR-0005: Typed Messages + Artifacts

- 状态：Accepted
- 日期：2026-10-21

## 背景

让 Agent 自由群聊会导致上下文污染、传话失真、token 爆炸。大产物（文件、日志、patch）塞进消息会迅速撑爆上下文。

## 决策

- Agent 之间使用 typed message envelope。
- 消息只包含摘要 + `artifact_ref`；大内容写 artifact store。
- 默认分层路由；允许同部门受限点对点；跨部门需显式许可。
- 不共享隐藏 reasoning/chain-of-thought。
- 所有消息持久化、可审计、有 TTL/幂等 key/大小上限。

## 理由

- Anthropic 的实践表明：subagent 把大产出写文件、只回传引用，能减少“传话游戏”。
- 结构化消息便于调度、重放、UI 渲染和权限控制。
- 私有上下文 + 显式共享可以降低污染。

## 后果

- 需要 artifact store、消息 router、TTL/背压。
- Agent prompt 必须教它“写 artifact，而不是贴全文”。
- UI 需要能展开 artifact 引用。
