# VerlinPark 竞品与多 Agent 研究

> 版本：0.1（Draft）  
> 日期：2026-10-21  
> 目的：为 VerlinPark 的产品定位、Agent 组织设计和技术选型提供依据。

---

## 1. 方法与范围

本轮调研覆盖三类对象：

1. **直接竞品/邻近产品**：LM Studio Bionic、DeepSeek Harness、Zed、Conductor、Claude Code、Cursor/Codex（作为交互参照）。
2. **基础设施/协议**：LM Studio Runtime、MLX Swift、Agent Client Protocol（ACP）、Model Context Protocol（MCP）。
3. **多 Agent 设计资料**：Anthropic 多 Agent 研究系统复盘、OpenAI Agents SDK、CrewAI hierarchical process。

说明：产品迭代很快，以下事实以 2026-10-21 可访问的公开文档为准；未亲自验证的功能不写入结论。

---

## 2. 执行摘要

### 2.1 市场空白

现有产品各自占住一块：

- **Bionic**：本地/云端模型的通用 Agent 工作流，项目/会话/技能/后台会话做得完整，但不是硬核开发者的多 Agent 控制台。
- **DeepSeek Harness**：插件化 Harness + Trace，架构开放，但 Web/桌面形态和预览期生态还在早期。
- **Zed**：原生编辑器 + ACP 外部 Agent + 并行 Threads + worktree，开发者工作流强，但编辑器优先。
- **Conductor**：Mac 上的并行 Agent 编排，一任务一 workspace，review/merge/archive 路径清晰，但没有自有 harness/model runtime。
- **Claude Code**：终端 Agent 的标杆，subagent/权限/worktree 设计成熟，但绑定 Anthropic 生态。

**没有一个产品同时做好：macOS 原生 + 本地优先 + 自有 harness + 三层 Agent 组织 + worktree 隔离 + 全链路 trace + 开放协议。**

### 2.2 最重要的设计约束

Anthropic 的多 Agent 复盘给了一个冷静的提醒：

- 多 Agent 在研究型任务上收益巨大（+90.2%），但 token 约 **15x**。
- **大多数 coding 任务真正可并行的部分少**，LLM 实时协调多 Agent 的能力还不够。
- 有效模式是 orchestrator-worker；subagent 把大产物写文件，只回传引用。
- 同步等待 subagent 会成为瓶颈；异步协调更难。

**对 VerlinPark 的含义：**

- 三层部门制必须由 **代码强制边界**，不能靠 Agent 自觉。
- 并行只用于独立任务；有依赖就串行。
- 消息必须结构化 + artifact 引用，不能自由群聊。
- 必须手动预算 + 硬停；不能假设多 Agent 一定更快。
- 先证明单 Agent 纵向切片，再证明多 Agent 收益。

---

## 3. 产品快照

### 3.1 LM Studio Bionic

**定位：** “The agent for your computer.” 面向工作与代码的 Agent，支持本地或前沿开源模型。

**关键能力（来自官方文档）：**

- Project / Session / Tab 模型；Session 可 Pin/Rename/Archive/Delete；后台会话继续运行；支持 side-by-side。
- 一个 Session 可以使用：
  - LM Studio Secure Cloud 的前沿开源模型（ZDR）
  - 本地模型（LM Studio runtime，MLX + llama.cpp）
  - 通过 LM Link 的另一台设备上的远程模型
- Coding 项目：选择本地目录，Agent 可以搜索仓库、编辑文件、使用 Git、运行 shell；UI 展示 diff/command output；支持 sub-sessions 并行调查。
- Skills：教 Agent 可重复的专门工作流。
- 语音输入：本地实时转写。
- 文档工作：研究、写作、分析、生成文件。

**值得借鉴：**

- Project/Session/Tab 的信息架构。
- 本地/云端/远程的统一模型抽象。
- 后台会话 + 完成通知。
- Skills 作为可复用工作流。
- Coding 的 inspect → change → test 工作流。
- diff review 的产品化。

**不照搬：**

- 通用文档/PPT/语音不是我们的主战场。
- 不做“模型路由黑盒”；VerlinPark 手动配置。
- 不做消费级宽场景，聚焦硬核开发。

**来源：**

- https://lmstudio.ai/
- https://lmstudio.ai/docs/bionic
- https://lmstudio.ai/docs/bionic/quick-start
- https://lmstudio.ai/docs/bionic/projects-and-sessions
- https://lmstudio.ai/docs/bionic/agent/code-project

---

### 3.2 DeepSeek Harness (dsh)

**定位：** 开源 Agent Harness，“Everything is a Plugin”，基于 Cordis。

**关键能力（来自官网/仓库/文档）：**

- 桌面端 + Web UI；`npx @deepseek-ai/dsh web` 启动 Web UI，默认 `127.0.0.1:3080`。
- 插件是一段 TypeScript 模块，导出 `apply(ctx)`；通过 `ctx` 注册能力；`inject` 声明依赖；`ctx.effect()` 管理清理；支持 function/object/class 三种形态。
- `cordis.yml` 作为 profile/patch 配置；插件可热加载/组合。
- Workspace → Session；Agent 可以读写文件、运行命令、委派工作、维护计划；权限策略可要求审批。
- Trace 视图：时间线、工具调用记录、执行详情。
- MCP 记忆、插件市场、Python SDK、GitHub 评审会话、定时提醒。
- 开源 MIT；developer preview，明确会有 breaking changes。

**值得借鉴：**

- “一切皆插件”的生命周期/依赖/清理设计（但不必在 v0.1 全做）。
- Trace 作为一等公民。
- Provider/模型配置与 Workspace/Session 模型。
- Headless 启动 + 多客户端。
- 插件生态的发现/安装路径。

**不照搬：**

- Web/Electron 技术栈；VerlinPark 要原生。
- 预览期全插件化的复杂度；v0.1 先内部 registry。
- 通用工作场景（PPT/表格）不是重点。

**来源：**

- https://www.deepseek.com/harness/
- https://github.com/deepseek-ai/deepseek-harness
- https://deepseek-harness.github.io/deepseek-harness/guide/quickstart
- https://deepseek-harness.github.io/deepseek-harness/develop/basic/

---

### 3.3 Zed

**定位：** 高性能原生编辑器，Agent Panel 是核心 AI 体验。

**关键能力（来自官方文档）：**

- Agent Panel + Threads Sidebar；多 Thread 并行，每个 Thread 独立 Agent/上下文/历史。
- External Agents：通过 **ACP** 接入 Claude Code、Codex、Gemini CLI 等；每个 Thread 可以用不同 Agent。
- Worktree Isolation：Thread 可以在独立 Git worktree 中运行；新建 worktree 默认 detached HEAD；支持 `create_worktree` hook。
- Checkpoints：每次模型编辑后可 Restore Checkpoint。
- Review Changes：多文件 diff、逐 hunk accept/reject、单文件 inline review。
- Terminal Threads：Agent Panel 里跑 CLI/TUI。
- MCP servers、Agent Profiles、Tool Permissions、Skills、自动 compaction、token usage。
- 原生性能、键盘优先、命令面板。

**值得借鉴：**

- ACP 作为外部 Agent 标准；v0.2 接。
- worktree 隔离 + 恢复。
- 逐 hunk review（v0.2）。
- Threads Sidebar 的信息架构。
- Checkpoint/rollback。
- 原生性能与键盘 UX。

**不照搬：**

- 编辑器优先；VerlinPark 是 Agent 控制台，不抢 Zed/VS Code 的编辑体验。
- 不做完整 LSP/调试器（v1）。

**来源：**

- https://zed.dev/docs/ai/agent-panel
- https://zed.dev/docs/ai/parallel-agents

---

### 3.4 Conductor

**定位：** “Run parallel coding agents on your Mac.” 支持 Claude Code、Codex、Cursor、OpenCode。

**关键能力（来自官方文档）：**

- 每个 task 一个 isolated workspace：独立 branch、files、terminal、diff、review path。
- 并行跑多个 Agent。
- 工作流：创建 workspace → Agent 工作 → review diff → open PR / merge → archive。
- Checks、Agent modes、Conductor API。

**值得借鉴：**

- 一任务一 workspace 的完整生命周期。
- review → merge → archive 的收尾路径。
- Mac 原生 + 多 Agent 并行的产品表达。
- “Agent 是工人，任务是单位”的心智模型。

**不照搬：**

- 纯编排层，没有自有 harness/model runtime；VerlinPark 要自有 harness。
- 不做单一 Agent 供应商绑定；用 ACP/自有 runtime 混合。

**来源：**

- https://conductor.build/
- https://conductor.build/docs

---

### 3.5 Claude Code

**定位：** 终端里的 coding agent harness。

**关键能力（来自官方文档）：**

- Subagents：独立上下文窗口、自定义 system prompt、工具/权限、模型；适合把探索/日志/文件内容隔离出主线程。
- Built-in subagents：Explore（只读）、Plan（只读）、General-purpose。
- `isolation: worktree`：subagent 在临时 worktree 中运行。
- Agent teams：协调多个 session；cross-session messaging；background agents。
- Permission modes：default/acceptEdits/auto/dontAsk/bypassPermissions/plan/manual。
- Skills、hooks、MCP、plugins、checkpoints、compaction。
- 明确提示：subagent 的请求计入同一 usage limit；模型选择/成本控制很重要。

**值得借鉴：**

- 角色化 subagent + 工具/权限收窄。
- worktree 隔离。
- 权限模式分级。
- “subagent 只回摘要，保持主上下文干净”。
- Agent teams 的协调/消息思路。

**不照搬：**

- 终端 TUI 形态；VerlinPark 要原生 GUI + 可视化组织。
- 绑定 Anthropic；VerlinPark 多 Provider。
- 自由 subagent 委派；VerlinPark 用代码校验的 DAG。

**来源：**

- https://code.claude.com/docs/en/sub-agents
- https://code.claude.com/docs/en/agent-teams（如可访问）
- https://code.claude.com/docs/en/cross-session-messaging（如可访问）

---

### 3.6 LM Studio Runtime

**定位：** 本地 LLM 运行时（MLX + llama.cpp），提供模型下载、聊天、OpenAI-compatible server。

**值得借鉴：**

- 本地模型的一键下载/加载/卸载。
- OpenAI-compatible endpoint，让上层无需关心推理细节。
- Apple silicon 上的 MLX 性能。
- 模型能力/上下文/量化信息展示。

**对 VerlinPark：**

- v0.1 直接接 LM Studio/Ollama/llama.cpp 的 endpoint。
- v0.3 用 MLX Swift 做 `verlin-mlx` helper，不重造完整运行时。

**来源：**

- https://lmstudio.ai/
- https://github.com/ml-explore/mlx-swift

---

### 3.7 Agent Client Protocol (ACP)

**定位：** 编辑器/IDE 与 coding agent 之间的标准协议，类似 LSP。

**关键事实：**

- 本地 Agent 作为子进程，通过 JSON-RPC over stdio；远程 Agent 通过 HTTP/WebSocket（进行中）。
- 复用 MCP 的 JSON 表示；自定义 diff、terminal、permission、plan、session 等类型。
- v1 已稳定；v2 在 draft；Rust/TypeScript SDK 1.0。
- 支持 session setup/list/delete、prompt turn、tool calls、elicitation、file system、cancellation、terminals、plan、modes、config options、slash commands、extensibility。
- 生态：Zed 等客户端；Claude Code/Codex/Gemini CLI 等通过 ACP adapter 接入。

**对 VerlinPark：**

- v0.2 做 ACP client，把外部 Agent 作为 Worker 或独立 Session。
- v0.3 做 ACP server，让 Zed 等客户端把 VerlinPark 当 Agent 用。
- 不自研 Agent-editor 协议；复用 ACP。

**来源：**

- https://agentclientprotocol.com/
- https://agentclientprotocol.com/llms.txt

---

### 3.8 多 Agent 研究：Anthropic

**核心数据/结论：**

- 多 Agent（Opus Lead + Sonnet subagents）在内部 research eval 上比单 Agent Opus 高 **90.2%**。
- 多 Agent 消耗约 **15x** 普通 chat 的 token；token 使用量解释 80% 的性能方差。
- 适合：breadth-first、可并行、信息量超过单上下文、工具多的任务。
- 不适合：需要所有 Agent 共享同一上下文、依赖很多的任务；**大多数 coding 任务真正可并行的部分少**。
- 架构：orchestrator-worker；Lead 规划、spawn 子 Agent、汇总。
- 关键工程经验：
  - 教 orchestrator 如何委派：目标、输出格式、工具/来源、边界。
  - 按任务复杂度缩放 effort。
  - 工具描述质量至关重要。
  - 并行 tool calls / 并行 subagents 大幅提速。
  - 子 Agent 把大产出写文件系统，只回传引用，减少“传话游戏”。
  - 同步等待 subagent 是瓶颈；异步协调更难但更高效。
  - 生产需要持久化执行、checkpoint、trace、rainbow deployment。
- 失败模式：spawn 过多、重复劳动、过度更新、上下文溢出、错误级联。

**对 VerlinPark：**

- 三层组织 + 硬限制 + DAG 校验。
- 任务描述必须包含 objective/acceptance/write scope/budget。
- 大产物 artifact 化。
- 先做同步调度（简单），v0.2 再评估异步/steering。
- 从一开始做 trace/checkpoint/恢复。

**来源：**

- https://www.anthropic.com/engineering/multi-agent-research-system

---

### 3.9 OpenAI Agents SDK / CrewAI

**OpenAI Agents SDK：**

- 两种编排：LLM 决策（agents-as-tools / handoffs）vs 代码编排（结构化输出、链式、循环、并行）。
- agents-as-tools：manager 保持控制，专家做有界子任务。
- handoffs：triage 把控制权交给专家。
- 对 VerlinPark：采用“代码编排 + 有界 LLM 决策”的混合；不让 LLM 完全自由 orchestrate。

**CrewAI：**

- Sequential / Hierarchical 两种 process。
- Hierarchical：manager LLM/agent 规划、分配、审核；任务不预分配。
- 对 VerlinPark：三层部门制类似 hierarchical，但增加硬限制、DAG 校验、worktree 隔离和 artifact。

**来源：**

- https://openai.github.io/openai-agents-python/multi_agent/
- https://docs.crewai.com/en/concepts/processes

---

### 3.10 其他参考

- **Cursor / Codex**：后台 Agent、云端任务、diff review、模型体验；作为交互参照，不作为架构蓝本。
- **OpenCode**：开源终端 Agent，client/server、provider 抽象；作为协议/多客户端参照。
- **Vibe Kanban / Crystal**：并行 Agent 看板/工作流；作为任务编排 UX 参照（未深入验证，暂不写结论）。

---

## 4. 竞品矩阵

图例：✅ 强 / ◐ 部分 / — 无或弱 / ? 未验证

| 维度 | Bionic | DSH | Zed | Conductor | Claude Code | VerlinPark（目标） |
|---|---|---|---|---|---|---|
| macOS 原生 | ◐ | —（Web/桌面） | ✅ | ✅ | —（终端） | ✅ |
| 自有 Agent Harness | ✅ | ✅ | ✅（Zed Agent） | —（编排外部） | ✅ | ✅ |
| 多 Provider | ✅ | ✅ | ✅ | ✅（外部 Agent） | —（Anthropic） | ✅ |
| 本地模型 | ✅ | ◐ | ◐ | — | — | ✅（v0.1 接 endpoint） |
| 三层 Agent 组织 | — | ◐（插件可扩展） | —（Threads/Subagents） | —（并行 session） | ◐（agent teams） | ✅ |
| worktree 隔离 | ◐（Bionic coding） | ? | ✅ | ✅ | ✅（subagent） | ✅ |
| 全链路 trace | ◐ | ✅ | ◐ | ◐ | ◐ | ✅ |
| 成本/预算硬控制 | ◐ | ◐ | ◐ | ? | ◐（usage limits） | ✅（手动硬预算） |
| 逐 hunk review | ◐ | ? | ✅ | ✅ | ◐ | v0.2 |
| MCP | ✅ | ✅ | ✅ | ? | ✅ | v0.2 |
| ACP 外部 Agent | — | — | ✅ | —（自有适配） | — | v0.2 |
| 插件/扩展 | ◐（Skills） | ✅（Cordis） | ◐ | — | ✅（plugins） | v0.3 |
| 开源 | — | ✅ MIT | ✅ | — | — | ✅ GPL-3.0 |
| 硬核开发导向 | ◐ | ✅ | ✅ | ✅ | ✅ | ✅ |

> 矩阵是产品定位对比，不是功能逐项审计；部分产品能力可能超出公开文档。

---

## 5. 借鉴 / 拒绝清单

### 5.1 直接借鉴

| 来源 | 借鉴点 | 落地 |
|---|---|---|
| Bionic | Project/Session/Tab、后台会话、Skills、本地/云端模型抽象 | v0.1 项目/会话/后台；v0.2 Skills |
| DSH | 插件生命周期、Trace 一等公民、headless | v0.1 事件/trace；v0.3 插件 |
| Zed | ACP、worktree、并行 threads、checkpoint、hunk review | v0.1 worktree/checkpoint；v0.2 ACP/hunk |
| Conductor | 一任务一 workspace、review/merge/archive | v0.1 核心闭环 |
| Claude Code | subagent 上下文隔离、权限模式、worktree | v0.1 角色/权限/worktree |
| Anthropic | orchestrator-worker、artifact 引用、硬边界、trace | v0.1 组织/DAG/artifact |
| LM Studio | OpenAI-compatible、本地模型体验 | v0.1 endpoint；v0.3 MLX |
| CrewAI | hierarchical manager | v0.1 三层部门 |

### 5.2 明确拒绝

- **自由 swarm / 群聊**：上下文污染、token 爆炸、不可调试。
- **自动模型路由**：用户选择手动；不偷偷换模型。
- **Web/Electron 壳**：原生是差异化。
- **App Store 优先**：GPL + shell 权限冲突；直接分发。
- **v0.1 全插件化**：先稳定 runtime，再开放。
- **自动合并 main**：必须人工 land。
- **完整 IDE**：不跟 Zed/VS Code 竞争编辑器。

---

## 6. 对 VerlinPark 的启示

### 6.1 产品

1. **卖“控制力”，不卖“魔法”。** 目标用户愿意配置模型/预算/组织，以换取可预测性和可审计性。
2. **组织可视化是核心界面。** Park Map 不是装饰，是产品差异。
3. **成本必须可见、可硬停。** 多 Agent 15x token 是现实。
4. **Trace 是信任的基础。** 没有 trace 的“多 Agent”只是更贵的黑盒。
5. **合并是闭环。** 不会 merge 的 ADE 只是玩具。
6. **原生性能是门槛。** SwiftUI/AppKit + Rust 是长期投入，但值得。

### 6.2 技术

1. 先打通 **单 Agent 纵向切片**，再上多 Agent。
2. 事件日志 + checkpoint 从第一天做；不要后补。
3. worktree/merge 用 git CLI，别硬造。
4. Provider 只做 2 个适配器，能力矩阵 + mock 测试。
5. IPC 契约用 schema 生成 + 契约测试。
6. 多 Agent 调度先同步，异步/steering 后置。
7. 手动预算的硬执行比“智能省钱”更重要。

### 6.3 风险

- 多 Agent 在 coding 上的收益可能低于预期；必须用 benchmark 决定默认策略。
- 三层组织可能过度设计；保留“单 Agent 模式”作为退路。
- 手动配置可能让 onboarding 变重；用模板/预设/批量填充降低门槛。
- GPL-3.0 限制 MAS；直接分发需要签名/公证投入。

---

## 7. 待研究问题

1. 三层部门制在真实仓库上的并行收益/成本比是多少？需要 5-10 个任务的 benchmark。
2. Verifier 的“一票否决”如何避免过度保守/反复修改？需要 revision 上限和升级策略。
3. 写作用域冲突预测的准确率？路径 glob 是否足够？
4. merge queue 的冲突率？是否需要“集成预演”（在临时 integration worktree 先跑）。
5. 本地模型（LM Studio/Ollama）在 tool calling 上的稳定性？需要能力探测/降级提示。
6. ACP 外部 Agent 作为 Worker 时，如何映射权限/预算/消息？需要适配层设计。
7. GPL-3.0 下插件 SDK 的许可边界？需要法律/社区讨论。
8. macOS 最低版本与 SwiftUI 性能的取舍？需要 15+ vs 26+ 的实测。

---

## 8. 来源列表

### 产品/文档

- LM Studio Bionic — https://lmstudio.ai/
- LM Studio Bionic Docs — https://lmstudio.ai/docs/bionic
- Bionic Quick Start — https://lmstudio.ai/docs/bionic/quick-start
- Bionic Projects & Sessions — https://lmstudio.ai/docs/bionic/projects-and-sessions
- Bionic Coding — https://lmstudio.ai/docs/bionic/agent/code-project
- DeepSeek Harness — https://www.deepseek.com/harness/
- DeepSeek Harness GitHub — https://github.com/deepseek-ai/deepseek-harness
- DSH Quickstart — https://deepseek-harness.github.io/deepseek-harness/guide/quickstart
- DSH Plugin Basics — https://deepseek-harness.github.io/deepseek-harness/develop/basic/
- Zed Agent Panel — https://zed.dev/docs/ai/agent-panel
- Zed Parallel Agents — https://zed.dev/docs/ai/parallel-agents
- Conductor — https://conductor.build/
- Conductor Docs — https://conductor.build/docs
- Claude Code Subagents — https://code.claude.com/docs/en/sub-agents

### 协议/运行时

- Agent Client Protocol — https://agentclientprotocol.com/
- ACP Docs Index — https://agentclientprotocol.com/llms.txt
- Model Context Protocol — https://modelcontextprotocol.io/
- MLX Swift — https://github.com/ml-explore/mlx-swift
- LM Studio — https://lmstudio.ai/

### 多 Agent 研究

- Anthropic, “How we built our multi-agent research system” — https://www.anthropic.com/engineering/multi-agent-research-system
- OpenAI Agents SDK, Agent orchestration — https://openai.github.io/openai-agents-python/multi_agent/
- CrewAI Processes — https://docs.crewai.com/en/concepts/processes
