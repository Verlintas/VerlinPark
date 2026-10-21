# ADR-0009: API 优先，MLX 后置

- 状态：Accepted
- 日期：2026-10-21

## 背景

本地模型是 VerlinPark 的差异化之一，但内置模型下载/量化/推理/显存管理会显著拖慢 v0.1。LM Studio/Ollama/llama.cpp 已经提供 OpenAI-compatible endpoint。

## 决策

- v0.1：支持任意 OpenAI-compatible endpoint（含本地 LM Studio/Ollama/llama.cpp）+ Anthropic。
- v0.2：本地 provider 向导/能力探测。
- v0.3：`verlin-mlx` Swift helper，基于 MLX Swift，暴露本地 OpenAI-compatible API；daemon 当普通 provider。
- 不在 Rust core 内嵌 MLX。

## 理由

- 先把 Agent 体验/多 Agent 调度跑通。
- 复用成熟本地 runtime，避免重复造轮子。
- MLX helper 独立进程，崩溃隔离、易升级。

## 后果

- v0.1 依赖用户已有本地 runtime 才能离线。
- 模型管理体验不如 Bionic 完整；v0.3 补。
- Provider 抽象必须从第一天就支持自定义 base URL。
