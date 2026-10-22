# ADR-0014: Aviiya 是 NovaBAIC 的外部常驻 Agent

- 状态：Accepted
- 日期：2026-10-23

## 背景

新版 UI 有 “Chat with Aviiya”，但 Aviiya 不应在 VerlinPark 内重新实现。NovaBAIC 已经拥有 Aviiya 的人格、记忆、工具与跨设备会话；未来两者需要互通。

## 决策

- VerlinPark **不实现本地 Aviiya**：不做人格、记忆、聊天 runtime。
- 在 UI 保留 resident 入口，但显示为 external / not connected。
- 定义 `ResidentAgentBridge` 协议作为未来集成 seam：
  - identity / display name
  - status
  - send message → stream
  - 后续扩展 shared memory、tools、sessions。
- Aviiya 独立于项目四层组织，不属于 Leader/Manager/Worker/Labour。
- 未来通过 NovaBAIC 的公开协议/导出格式接入；在协议确定前不做私有耦合。

## 理由

- 避免两个 Aviiya 人格/记忆分叉。
- 保持 VerlinPark 的项目调度与 NovaBAIC 的常驻助理职责清晰。
- 先把 UI 位置和边界留好，等 NovaBAIC 协议稳定再接。

## 后果

- 当前 resident 页面是占位，不可聊天。
- Settings 的 Resident tab 只展示桥接状态与链接。
- 未来需要定义跨设备身份、记忆同步、工具权限和会话协议。
- 相关仓库：https://github.com/Verlintas/NovaBAIC
