# ADR-0001: SwiftUI + Rust Core

- 状态：Accepted
- 日期：2026-10-21

## 背景

VerlinPark 要做 macOS 原生 ADE，需要原生 UI、复杂 Agent 运行时、PTY/进程管理、Git worktree、流式 Provider、事件存储和可脚本化能力。

候选：

1. 全 Swift（SwiftUI + Swift actor/NIO）
2. SwiftUI + Rust Core
3. Tauri/Electron + Web UI

## 决策

采用 **SwiftUI/AppKit 做 UI，Rust 做 daemon/core**，通过本地 WebSocket JSON-RPC 通信。

## 理由

- Rust 在并发、进程/PTY、Git、SQLite、JSON、流式 HTTP 方面生态成熟。
- Core 可 headless 运行，CLI/CI/未来远程客户端复用。
- IPC 强制边界，UI 崩溃不影响 Agent。
- 原生 UI 是产品差异化，不用 Web 壳。

## 后果

- 双语言、双构建链、IPC 契约维护。
- 需要 schema 生成/契约测试、打包签名 helper。
- 第一周必须先打通 IPC，否则后续风险高。

## 备选

- 全 Swift：简单，但进程/Agent 生态弱，headless/跨端受限。
- Tauri/Electron：UI 快，但不是纯原生，启动/内存/系统集成不占优。
