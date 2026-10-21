# ADR-0007: WebSocket JSON-RPC over Loopback

- 状态：Accepted
- 日期：2026-10-21

## 背景

Swift UI 与 Rust daemon 需要本地 IPC。候选：Unix domain socket、localhost HTTP/WebSocket、stdio。

## 决策

- Daemon 绑定 `127.0.0.1` 随机端口，提供 WebSocket JSON-RPC 2.0。
- Discovery file（`0600`）保存 pid/port/token。
- 客户端首条消息 `auth`；token 常量时间比较。
- 事件带 `seq`，支持断线重连/补发。
- 写操作支持 idempotency key。

## 理由

- Swift `URLSessionWebSocketTask` 原生支持，开发/调试成本低。
- JSON-RPC 简单、跨语言、易生成 schema。
- 只绑定 loopback + token + 文件权限，满足本地单用户安全需求。
- 比 stdio 更适合多客户端/CLI/未来远程。

## 后果

- 需要处理端口冲突、token 泄露、慢客户端、重连。
- Unix socket 可作为未来可选 transport；不改变协议语义。
