# ADR-0008: GPL-3.0-only + 直接分发

- 状态：Accepted
- 日期：2026-10-21

## 背景

用户选择 Public + GPL-3.0。GPL-3.0 与 Apple Mac App Store 的 DRM/条款存在已知冲突（VLC 先例）。

## 决策

- 代码许可：**GPL-3.0-only**。
- 分发：签名 + notarize 的 DMG；Homebrew cask；可选 Sparkle 自动更新。
- v1 **不做 Mac App Store**。
- 未来如需 MAS，另立 ADR，考虑双许可/例外。

## 理由

- 用户明确选择 GPL-3.0。
- 直接分发可以保留 GPL 自由，也适合开发工具（需要 shell/worktree 权限）。
- App Sandbox 会限制 Agent 能力；MAS 不适合本产品形态。

## 后果

- 不能走 MAS 默认分发。
- 需要 Apple Developer ID 做签名/公证。
- 插件许可边界需在 v0.3 前设计（独立进程/IPC）。
