# ADR-0013: Glass Park UI 与可配置品牌 Mark

- 状态：Accepted
- 日期：2026-10-23

## 背景

原 M0 UI 是常规 IDE 三栏 + 事件列表。新设计要求 chat-first、毛玻璃、左侧项目、中间对话、右侧 Collab/Preview，并且左上角名称可配置为纯色/渐变/旗帜底。

## 决策

- 三栏：Rail（品牌/项目/Manage Dash/Settings/内存）· Stage（对话/会话）· Studio（Collab/Preview）。
- 视觉：隐藏 titlebar、behind-window blur、thin material、细边框、深色优先；内容区保持较高不透明度。
- 品牌：`BrandConfig` 支持 displayName、solid/gradient/flag；提供 VerlinPark/Base/Home/Office/Studio/Lab 预设。
- 国旗：使用 FlagKit（MIT）的 248 个 ISO 国家/地区旗帜资源。
- Pride：20 面 Wikimedia Commons 标准旗帜 PNG，包括 Rainbow、Progress、Intersex-Inclusive Progress、Trans、Bi、Lesbian、Nonbinary、Pan、Ace、Aro、Aroace、Genderfluid、Genderqueer、Intersex、Agender、Demi、Poly、Sapphic、Queer、Two-Spirit。每面旗的作者/许可见 `docs/PRIDE_FLAGS.md`。
- `.app` bundle 名保持 `VerlinPark`；品牌名只影响窗口标题、About、wordmark 和设置。
- 尊重 Reduce Transparency / Increase Contrast；不为动画牺牲可读性。

## 理由

- Chat-first 更符合“用户与 Leader 对话，右侧看组织”的新交互。
- 毛玻璃提供现代 macOS 质感，但阅读区必须保持对比度。
- 旗帜是品牌表达，但资产需要可控、可离线、可授权。

## 后果

- SwiftUI 外壳基本重写；需要 `BrandStore`、`BrandMark`、FlagPicker。
- 需要 FlagKit（MIT）和 Wikimedia Commons 旗帜的逐文件归属说明。
- 未来若支持整包重命名，需要单独的构建/包装方案，不能用运行时字符串改 Dock 名。
