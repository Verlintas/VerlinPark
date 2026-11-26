# VerlinPark Status

> 最后更新：2026-11-26
> 当前主线：C1–C12 代码落地（C11 为 loopback + card-only，尚无 inbound 执行桥）；M3.1–M3.6 结构治理完成；CUSTOS observe-only 基础已实现（ADR-0235）；下一阶段为生产化与分布式边界。

## 1. 一句话

VerlinPark 已经是一个可运行的 macOS 原生多 Agent 开发区：Rust daemon 拥有状态与调度，SwiftUI 是客户端，四层组织、worktree、Verifier、merge queue、预算、审批、事件回放和长期运行能力均已实现。当前最大的风险不是功能缺失，而是发布链路、跨主机语义和 UI 自动化。

## 2. 代码与验证

| 项 | 当前值 |
|---|---:|
| Rust crates | 11（新增 `vp-custos`） |
| ADR | 235 |
| 最近全量 Rust 测试 | 4,777 passed / 0 failed / 6 ignored |
| Swift | 673 passed，`xcodebuild test` 通过 |
| Live provider | DeepSeek `deepseek-flash` 流式/provider-test/完整 Leader 写文件通过；小前缀 cache 97.1–97.4% |
| 当前分支 | `m3-round`（本轮审查前不合并） |

## 3. 已完成路线

- Contracted Execution C1–C5：contracts/intent、effect receipts、external gates、runtime pinning、reliability/OTel。
- C6–C10：failure attribution、bounded fan-out、tool search v2/delta snapshots、MCP tasks/elicitation、Code Mode。
- C11：loopback-only A2A facade（card-only inbound，无本地执行权限）。
- C12：advisory progress ledger（bounded、event-backed、永不自动重排）。
- S1–S8：project-scoped identities、assignment/lease/inbox、identity prompts、Worker workspaces、memory、hiring/retirement、org chart、team templates。
- Hardening 1–3：identity/lease/effect/gate/template/evidence 等生产修复。
- E1–E8、R1–R4、C1–C4、D1–D4、Q1–Q4、F1–F5、H1–H5、I1–I6、L1–L5：效率、长任务、checkpoint/ACP/ANN/backup、cache、scaling、source/remote/budget、运行硬化、gap closure。
- M3.1：`vp-store/src/store.rs` 34,939 行 → 6,611 行 parent + 33 个 domain modules；SQL/API 行为不变，390 store tests 通过。
- M3.2：`vp-core/src/agent.rs` 29,496 行 → 2,738 行 parent + 14 个 modules；`vp-core/src/lib.rs` 24,074 行 → 479 行 parent + 23 个 `engine/` modules；行为/API 不变。
- M3.3：`run_loop`/`service`/`service_run`/`runs`/`plans`/`sources` 完成二级拆分；parent 最高 138 行，所有模块 < 3k；module-governance 改为递归扫描。
- M3.4：`vp-cli/src/main.rs` 27,823 行 → 1,160 行 parent + `cli`/`dispatch`/`render`/`rpc`/`tests` modules；CLI 行为不变。
- M3.5：`vp-store` 8 个大模块、`vp-core` plan/memory/service-merge、`vp-daemon/main.rs` 完成二级拆分；新增 store/CLI/daemon module-governance 测试。
- M3.6：`vp-cli/render.rs`、`vp-cli/dispatch.rs`、`vp-daemon/dispatch.rs` 按输出域/命令族/路由族拆分；行为与协议不变。
- CUSTOS observe-only：`vp-custos` crate、`custosd`/`custos`、0600 Unix socket、只读 SQLite probe、四进程 heartbeat/launchd 安装器、只读进程 shadow 采样、本地 standard/forensic 诊断包、hash vault（显式 init/verify/repair，可选 local Ed25519）+ hash-chained audit + 单向 shadow notice、普通 SwiftUI 只读窗口（仅 wordmark 为字符艺术）；notice、signed/automatic self-heal 与 L2–L4 仍未实现。

## 4. 本轮新增

### Collab Agent Inspector and Direct Channel (2026-11-22)

- Clicking a live Agent node opens a bounded inspector with Overview/Context/Conversation/Messages/Activity.
- `agent.inspect` exposes agent-scoped task/context/usage/timeline/steering/inbox data without raw prompts, tool payloads, or secrets.
- `agent_event_index` is a derived SQLite index rebuilt from events.
- `agent.message.send` routes running/waiting Agents to steering, idle identity Agents to the project inbox, and idle ephemeral Agents to typed `AGENT_NOT_ACTIVE`; it never fabricates instant delivery or approval.
- Rust 4,534 / 0 failed / 6 ignored; Swift 474 app + 132 helper; fmt/clippy/swiftformat/xcodebuild pass.

### Live DeepSeek validation

- 真实 provider 验证：stream、`provider-test`、4-turn live cache-bench、完整 Leader → `write_file` → idle。
- 完整流程：3 calls / 35,614 input / 188 output / 34,688 cache-read，cache hit **97.1–97.4%**；文件内容精确。
- 修复空 `tool_calls` 兼容性和预算估算过度保守；20k/60k/120k budget 的行为均已实测（后两者不是伪造成功）。
- 小前缀 smoke 不宣称 99.84%；200k 长前缀 live acceptance 仍未运行。

### M3.6 dispatch and render split

- `vp-cli/render.rs` 7,999 → 33 行 parent + 18 个输出域 modules；`print_human` 保持原 first-match 顺序。
- `vp-cli/dispatch.rs` 6,918 → 482 行 parent + 23 个命令族 modules；425 个 `Command` variants 穷举路由，arm 内容保持。
- `vp-daemon/dispatch.rs` 5,442 → 339 行 parent + 11 个路由族 modules；469 个 top-level match arms 保持，`task.stalls → session` alias 保留。
- 新增 ADR-0228；CLI/daemon 行为、RPC method/params、输出、认证、路由不变。

### M3.5 store, core tail, and daemon split

- `vp-store`: employment 4,798 → 549 + 8 modules；backup 2,592 → 8 + 4；workspaces 2,402 → 1,174 + 5；memory 2,367 → 9 + 5；sources 2,339 → 11 + 7；plans 2,092 → 10 + 6；identity_memory 2,083 → 979 + 3；coordination 2,041 → 919 + 3。
- `vp-core`: plan 2,495 → 61 + 6；engine/memory 1,955 → 23 + 7；service/merge 1,894 → 5 + 3。当前最大 core module 为 `agent/run_loop/body.rs` 1,954。
- `vp-daemon/main.rs` 8,216 → 231 + dispatch/http/sweepers/errors/server/backup_status/params/tests；RPC/认证/路由不变。
- 新增 `vp-store`/`vp-cli`/`vp-daemon` 的 module-governance 测试；store parent < 8k、module < 3k。
- 新增 ADR-0226/0227。

### M3.4 CLI entrypoint split

- `vp-cli/src/main.rs` 27,823 → 1,160 行。
- `cli.rs` 4,049 行：clap model 与参数枚举。
- `dispatch.rs` 6,918 行：命令 match，`run_command(command, discovery, json)`。
- `render.rs` 7,999 行：JSON/human/CSV 输出。
- `rpc.rs` 180 行：RPC/discovery helpers。
- `tests.rs` 7,594 行：原 29 个测试模块 / 145 tests，不改写断言。
- 命令行参数、输出、exit code、RPC method/params、discovery 行为不变。
- 同一轮继续拆分 `agent/tools.rs`（2,911 → 14 行 parent + 4 modules）与 `agent/context.rs`（2,765 → 36 行 parent + 5 modules）；当前最大 core module 为 `agent/run_loop/body.rs` 1,954 行。

### M3.3 second-level module split

- `agent/run_loop.rs` 4,822 → 18 行 parent + 8 modules。
- `agent/service.rs` 3,951 → 10 行 parent + 8 modules。
- `agent/service_run.rs` 3,298 → 13 行 parent + 11 modules。
- `engine/runs.rs` 3,245 → 138 行 parent + 6 modules。
- `engine/plans.rs` 2,738 → 49 行 parent + 7 modules。
- `engine/sources.rs` 2,618 → 9 行 parent + 7 modules。
- `module_governance` 改为递归扫描，module budget 从 6k 收紧到 3k；当时最大模块 `agent/tools.rs` 2,911 行。

### M3.2 core module split

- `vp-core/src/agent.rs` 29,496 行 → 2,738 行 parent + 14 个 `agent/` modules。
- `vp-core/src/lib.rs` 24,074 行 → 479 行 parent + 23 个 `engine/` modules。
- 597 个 `impl Engine` 方法与 51 个 tail helpers 各保留一次；行为/API 不变。
- 新增 `module_governance` 测试：parent < 8k 行、domain module < 6k 行。
- 剩余较大模块：`agent/run_loop.rs` 4,822、`engine/runs.rs` 3,245、`engine/plans.rs` 2,738、`engine/sources.rs` 2,618。

### C11 A2A

- `vp-a2a` crate：bounded Agent Card/task/SSE、JSON-RPC lifecycle、in-memory store、constant-time bearer token、loopback-only URL/bind。
- daemon `a2a.status/tasks/card/start/stop/policy.set`、CLI、`a2a.start` explicit approval。
- Inbound 仍是 card-only rejected：没有 capability contract 就不给本地 tool/file/shell/network/model 权限。
- task list 是 bounded operational memory，重启丢失，不作为 Verifier/merge evidence。

### C12 progress ledger

- `vp-protocol::progress` + `vp-core::progress_ledger`：bounded rounds、严格 r1/r2 序列、deterministic status、`render_brief`、advisory-only。
- `progress_rounds` event/projection、`session.progress(.report)`、`vp progress`、Leader/Manager `report_progress`、replay/repair。
- Swift ProgressCard/AppModel/Dashboard 接入。
- 永不选择/claim/skip/retry/cancel/replan/merge，不改预算/容量。

## 5. 已知边界

| 边界 | 影响 | 下一步 |
|---|---|---|
| A2A card-only | 不能真正接收/恢复外部任务 | capability contract + external gate bridge（需要 partner） |
| A2A in-memory | 重启/停止丢 task list | 若需要，DB projection + replay |
| store 拆分 | 纯移动，无行为变化 | 继续拆 `agent.rs`/`lib.rs` |
| 发布 | 仅 ad-hoc DMG，无签名/公证/Sparkle | 发布链路生产化 |
| 分布式 | 同机 SQLite lease，无跨主机调度/merge ownership | 独立分布式项目 |
| 沙箱 | Shell 写入隔离，读取/网络/daemon 写入未完全隔离 | 读取/网络策略 |
| 事件溯源 | audit stream + projections，不是完整 replay | 补 projection/archive rebuild |
| Code Mode | 仅 registry tools、best-effort schema、非 exactly-once | 保持约束，补边缘测试 |
| Swift UI | 无完整自动化 UI 套件 | 关键窗口 smoke/UI tests |

## 6. 推荐下一阶段

1. **可用性优先**：把本次 live smoke 固化为可重复的 opt-in 测试/脚本，继续验证首次使用、恢复、备份与真实 provider 失败路径；结构拆分降为按需。
2. **发布生产化**：Developer ID、notarization、Sparkle/自动更新、干净 Mac smoke。
3. **C11 partner gated**：只有有真实 partner 时接 inbound execution/gate bridge。
4. **分布式 runtime**：跨主机 lease/fencing、merge ownership、远程环境复现。
5. **持续质量**：live provider/cache benchmark、Swift UI automation、adversarial corpus 扩展。

## 7. 非目标

- 自由 swarm / 群聊。
- 无 partner 的公网 A2A。
- OS-level Code Mode sandbox 宣称。
- exactly-once provider/remote 宣称。
- 用 attribution/replay/progress ledger 自动合并。
- 隐藏模型路由或自动 DAG 重写。

## 8. Capability matrix

| 能力 | 状态 | 主要入口 | 备注 |
|---|---|---|---|
| Session/worktree | 完成 | `session.*`, worktree | 每 session 独立 branch；diff 基于 base commit |
| 四层组织 | 完成 | TeamPlan/delegate | Leader→Manager→Worker→Labour |
| Verifier | 完成 | `submit_verdict` | 独立 detached clone；无 write/edit |
| Merge queue/Integrator | 完成 | `merge_queue.*` | `--no-ff`、diff check、失败回滚 |
| 预算/容量 | 完成 | `session.budget.*`, `agent.info` | token/USD/time + 全局容量 + reservations |
| 审批 | 完成 | `approval.*` | smart/strict/unattended；模板/去重/attention |
| Shell sandbox | 部分 | shell/process | 写入 Seatbelt；读取/网络/daemon 写入未完全隔离 |
| 事件/回放 | 部分 | `session.replay`, `db-rebuild` | audit + projections；不是完整 event sourcing |
| MCP | 完成 | `mcp.*` | stdio/HTTP/SSE；pins；async tasks；elicitation |
| ACP | 完成 | `delegate_acp` | stdio + HTTP/WS；远程是 trusted external |
| Code Mode | 完成 | `run_script` | QuickJS+oxc；无 ambient capability；不是 OS sandbox |
| A2A | 部分 | `a2a.*` | loopback card-only；无 inbound execution |
| Progress ledger | 完成 | `report_progress`, `session.progress` | advisory only；无自动动作 |
| Identities S1–S8 | 完成 | `agent.*` | org chart/team templates/retirement |
| Programs | 完成 | `program.*` | milestone controller；无 LLM planner |
| Remote workspace | 部分 | `remote_*` | SSH helper；不复现远程环境 |
| Backups | 完成 | `backup.*` | v1 ZIP + v2 encrypted + v3 signed |
| Updates | 部分 | `update.*` | 检查/下载；无 Sparkle 自动安装 |
| UI | 完成（手动 smoke） | SwiftUI | 无完整自动化 UI 套件 |

## 9. Architecture debt

1. `vp-core` 的 `lib.rs`/`agent.rs` 与主要领域模块已完成 M3.2/M3.3/M3.5 拆分；parent 最高 138 行。
2. `vp-cli` 入口、render、dispatch 已完成 M3.4/M3.6 拆分；剩余最大 CLI 文件是 `cli.rs` 4,049（clap model）与 `tests.rs` 7,594（测试）。
3. `vp-daemon/main.rs` 与 dispatch 已完成 M3.5/M3.6 拆分；最大 route family 为 `dispatch/session.rs` 1,122。
4. 当前最大 core modules：`agent/run_loop/body.rs`（1,954）、`agent/run_loop/tool_calls.rs`（1,923）、`engine/checkpoints.rs`（1,558）、`engine/usage.rs`（1,319）、`engine/agents.rs`（1,249）。下一轮可拆 `body`/`tool_calls`/`checkpoints`/`usage`。
5. `store` 已完成 8 个大模块二级拆分；`schema.rs` 2,931 仍是单一有序单元，`team_template`/`programs` 等仍 >1.5k。
6. 事件投影覆盖仍以关键 session 状态为主；plan_task_progress、A2A task list 等是操作状态。
7. 审批/契约/effect 的门控顺序已共享 `authorize_tool_call`/`prepare_tool_effect_gate`/`finish_tool_call`；新增 gate 必须改共享 helper。
8. `module_governance` 覆盖 core/store/CLI/daemon：parent < 8k 行、domain module < 3k（CLI/daemon 使用各自预算）。

## 10. Release checklist

- `cargo fmt --all -- --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`
- `xcodebuild test` against generated project
- Clean Mac install + first-run wizard
- DMG checksum + notarization
- Live provider probe (env key required)
- Backup/restore smoke on a copy of the data directory
- Worktree cleanup / target size check
