# VerlinPark 项目重审

> 审计日期：2026-10-29  
> 审计基线：`cbef2e6`（`main`，Manager roundtable 已合入）  
> 范围：Rust runtime、SwiftUI app、协议/存储、Git 集成、权限与预算、文档与仓库卫生  
> 结论先行：**M2 的功能闭环已经成立，但仍是一个强原型，不是可靠的多 Agent 平台。** P0 约束、merge queue、确定性 Integrator、queue retry、本地 landing、GitHub PR flow、pause/resume 与 shell 写入沙箱已经补上；下一步是完整 event sourcing、完整 shell 读取/网络隔离与其他 forge PR adapter。

## 0. 2026-10-30 跟进

第一轮硬化已完成（同一 `main` 线，提交见仓库历史）：

- **P0-1 已修复**：TeamPlan 自动派发按 `write_scope` 选择可并行批次；空 scope / 重叠 scope 的任务串行执行。Worker 使用可释放的 active slot，Manager 继续按 run 计数；`available(Worker)` 限制 batch 大小。
- **P0-2 已修复**：provider 请求前做 token reservation，按保守输入估算 + 动态 output budget 预留；provider 返回 usage 后结算，缺少 usage 的本地端点会以 `estimated: true` 的估算用量持久化。并发批次不再共用同一个 pre-flight 窗口。
- **P1-4 已修复**：`fetch_url` 在连接前解析并固定已校验的 IP，阻止解析到 loopback/private/link-local/文档/benchmark 等地址；补充了 IPv4-mapped IPv6 与特殊段检查。
- **P1-3 部分修复**：shell 现在通过 macOS Seatbelt 拒绝 worktree/Git 元数据/scratch 之外的写入，并保护 common hooks/config；sandbox 不可用时 fail closed。常见 credential/state 路径已 read-deny，timeout/cancel 会清理整个 process group，项目级 policy 可 deny network、追加 read-deny，并可 opt-in `restrict_home` 拒绝 HOME 内容，或 `read_jail` 进一步拒绝 HOME parent（通常 `/Users`）和 `/Volumes`（worktree/scratch/Git/toolchain/系统路径例外）；System 路径、`/private/tmp`、`/private/var`、network allow 下的外发、进程执行策略和 Docker 等 daemon 介导写入仍未隔离，Verifier 的 shell 现在运行在 isolated detached clone 中，不能修改 Worker worktree/branch；tracked 源码修改仍有 post-run guard。
- **P1-9 第一版**：project-level `dispatch_tasks` 模式让全局 scheduler 每次只领取一个 ready plan task，session 结束后回到 idle，多个 session 可以公平推进；`task.dispatch` / CLI / Manage Dash 可手动触发；limited 模式不把 cancel/budget abort 扩散到整份 plan。仍缺 default-on dispatcher 和 durable cross-process session lease。
- **Context compaction 第一版 + model-aware 修订**：按 global/per-role context window 计算 hard limit 和 75% summary threshold；每个 top-level turn 前检查；summary + retained tail 为主，失败/超限时滚动裁剪；原消息仍可搜索/回放。已有 session checkpoint/fork；仍没有 compaction checkpoint/resume。
- **P1-8 部分修复**：provider stream 在**尚无任何输出**时最多 3 次 attempts（含首次）；partial failure/cancel 会保留 `partial: true` message，并按 provider usage 或估算结算 token reservation；partial turn 不会 replay。`agent.probe` 现在报告 chunks/first-token/done；env-gated live OpenAI/Anthropic/local E2E 已加入。仍不能从 mid-stream 断点续传。
- **P1-7 已实现（第一版）**：ACP external-agent bridge。外部 stdio agent 注册表、Leader/Manager `delegate_acp`、Worker slot + global capacity/write lease、task worktree、Seatbelt 写沙箱 + scratch HOME、MCP 传递（stdio 总是传递，HTTP/SSE 按 agent capability）、permission→approval、timeout/cancel、Verifier/Integrator 全链路、`acp.agent.probe`、CLI/Settings 已具备；ACP 本身仍缺 HTTP/SSE transport、session resume、`fs/*`/`terminal/*` capability、auth flow、usage ledger 映射，且外部二进制仍是受信任程序。
- **P1-6 已修复（第一版）**：Verifier-approved commit 进入持久 merge queue，确定性 Integrator 执行 merge、diff check 和可选项目检查，失败回滚，冲突记录；conflict/failed 可 rebase+retry，冲突可显式调用 resolver agent 解决并重新验证；项目级 opt-in policy 可在 session idle 时自动触发，带 attempt/token cap；本地 landing 和 `gh` PR flow 可用。仍缺默认开启的静默自动冲突解决、LLM Integrator、其他 forge PR adapter。
- **队列恢复**：daemon 重启把 queued/merging/checking entry 标为 `interrupted` 并补事件；interrupted 可人工 rebase+retry。
- **P1-5 部分修复**：session status、usage、agent status、team plan、todos、messages、merge queue、recovery 使用 event+projection 原子事务；`session.replay` / `vp replay --repair` 可检查并修复覆盖的关键投影；agent roster 已通过 `agent.snapshot` 覆盖；`session.export` / `--rebuild` / `session.import` 已实现；导入 session 默认是只读 archive、保留 approvals；`session.rehydrate` 可显式重建 session worktree，并按 branch 分组 remap imported agent task-worktrees。
- **仍未解决**：完整 shell read jail/网络隔离、完整 event sourcing、静默自动冲突解决、其他 forge PR、shell scratch 回收、部分 merge 状态的人工检查、外部 ACP agent 的网络/凭证边界、mid-stream 断点续传。

当前测试：Rust 316（core 256 / protocol 2 / store 44 / providers 14；另有 3 个 ignored live tests），Swift 89。CI 以最新提交为准。

### 0.1 2026-10-31 跟进

- Provider adapter 在 stream 建立前对 429/5xx/connection 错误做最多 3 次重试，支持 `Retry-After`、指数退避和 15s connect timeout；stream 中途断开仍不续传。
- 新增 `session.search` / `vp search` / Stage `Cmd-F`：跨 session 搜索 message content 与 raw event payload，可按 project 过滤；后续已升级为 FTS5 主索引 + LIKE fallback（ADR-0096）。
- 新增 `session.transcript` / `vp transcript` / Export Transcript：Markdown 记录 plan、conversation 和 tool calls，超长内容截断。
- Preview 支持文本（64KB）与二进制 hex/ASCII（1KB）artifact 预览；Manage Dash 增加 Doctor 和 worktree cleanup 卡片；project pool 80% 也发 `agent.budget_warning`。
- 修复 merge 回滚测试中 queue 投影先于 agent 状态可见导致的竞态。
- 修复 P2-7：plan batch 中止时同批未处理 task 现在会结算为 `cancelled`/`blocked` 并发出 `plan_task.finished`，不再留下 running 投影；新增取消 batch 回归。
- P2-10 已修复：Collab 新增 Manager roundtable 卡片，按轮次显示 title/profile/position。
- 新增 `session.usage_csv` / `vp usage --csv` / Preview Export CSV。
- 新增 Homebrew cask 生成脚本和 release asset（`just cask`）；release workflow 现在可选发布到 tap。
- 新增 `project.usage` / `vp project-usage` / Manage Dash 按 session 用量汇总。
- 新增 Manage Dash 跨 project pending approvals 卡片，可直接 Approve/Deny。
- 新增 Preview Changes 文件选择（unified diff parser + 4 个 Swift 测试）。
- 新增 opt-in worktree retention policy、`keep_failed` 和 daemon sweep（`VERLINPARK_WORKTREE_SWEEP_SECS`）。
- 新增 resumable TeamPlan：`session.plan_resume` / `vp resume-plan` / Preview Resume plan，跳过 completed task，复用 task worktree/commit；见 ADR-0070。
- 新增 provider stream idle timeout（`VERLINPARK_PROVIDER_IDLE_SECS`，默认 300s）和 OpenAI-compatible HTTP adapter 全链路 E2E；见 ADR-0071。
- 新增 `vp db-rebuild`：从 project manifest + session event mirrors 重建 SQLite 和关键投影；见 ADR-0072。
- 新增 Rail 通知中心：approval、task failure、integration failure、session stop，带 unread/Dock badge 和跳转 session；通知历史持久化到 UserDefaults。
- queued session 跨 daemon 重启保留并自动 re-enqueue；新增 store/agent 回归。
- 新增 Anthropic HTTP adapter 全链路 E2E；worktree cleanup 同步清理 session scratch。
- 新增 explicit project memory：`remember`/`recall` tools、`memory.list/create`、CLI/Manage Dash、replay/repair 与 bundle。
- 新增 blocking user questions：`ask_user`、`waiting_input`、Chat 卡片、`question.*` RPC/CLI、replay/repair/bundle；Manage Dash inbox，timeout 由 `VERLINPARK_QUESTION_TIMEOUT_SECS` 配置。
- 新增 opt-in shell home read jail（`ShellPolicy.restrict_home`），HOME 内容默认拒绝、toolchain allowlist、read-deny 最后生效。
- 新增 MCP stdio client：server config、tools list/call、agent `mcp` tool、Settings/CLI、approval 与超时；后续切片加入 per-server persistent process、idle 回收、config 失效、process group 清理、cursor 分页，以及 resources/templates/prompts list/read/get；见 ADR-0077。
- 新增 MCP HTTP/SSE transport：`McpTransport`、Streamable HTTP（JSON/SSE 响应、`Mcp-Session-Id`、`MCP-Protocol-Version`、DELETE）、legacy HTTP+SSE（endpoint event、same-origin POST）、same-origin redirects、`${ENV_VAR}` header indirection、8MiB caps、server-initiated -32601、transport-aware reconnect/close、ACP capability filter、`mcp.server.probe` 与 Settings/CLI add/edit/probe；见 ADR-0113。
- 新增 memory retention + FTS5：`memory.delete` / `memory.prune`、`memory.deleted` 回放/修复、FTS5 外部内容索引与 LIKE fallback、Manage Dash 删除/30 天 prune；后续切片加入 opt-in per-project retention policy 与 daemon sweep；见 ADR-0078。
- 新增 priority-aware session admission 与 Worker/Labour capacity：session priority -100..100，高优先级先准入，同优先级 session/project round-robin；见 ADR-0079。
- 新增 macOS 本地系统通知：后台发送 approval/task/integration/session 通知，点击跳转 session，Settings 可开关和查看权限；approval 通知支持 Approve/Deny actions。
- Settings 增加 Team capacity 编辑器（Managers/Workers/Labour Stepper）；测试代码移到 `agent/tests.rs`、`tests.rs`、`store/tests.rs`，无行为变更。
- 新增 read-only Git history viewer：`git.log` / `git.show`、Preview History tab、`vp history` / `vp show`、commit hash 校验与 patch cap。
- 新增 opt-in budget-gated plan Worker auto-expansion：20% token 余额门槛、hard cap 10、plan 结束恢复 base cap、`plan.auto_expand` 事件。
- PR flow 增加 GitLab MR adapter：origin 自动检测、`glab` preflight/push/MR、`VERLINPARK_PR_PROVIDER` override，与 GitHub 共用 clean/ahead/event 路径。
- Session priority 可在创建后修改：`session.priority.set`、`session.priority` 事件、repair/rebuild 恢复、CLI/header menu。
- 新增 daemon-owned PTY Terminal：portable-pty、base64 bytes、resize、process-group、SwiftTerm 1.10.1、Preview Terminal tab、`vp terminal`。
- MCP 工具现在会以 `mcp_<server>_<tool>` 动态 schema 注入 Leader/Worker/Labour；Manager/Verifier 保持只读，不注入动态工具。
- 新增 daemon-owned line-oriented Console：session worktree Seatbelt sandbox、streaming chunks、signals、Preview tab 与 `vp console`；不是 PTY。
- 新增 workspace file browser：tree/file 路径 jail、Preview Files tab、`vp files` / `vp file`、binary/large preview；后续切片加入 expected_sha256 冲突检测的文本编辑、atomic write 与 `workspace.file_saved` 事件。
- 新增 isolated Verifier checkout：Verifier 在 Worker commit 的 detached clone 中运行，clone 结束后删除；见 ADR-0092。
- 新增 project-level auto-resume policy：`TaskSchedulerPolicy`、manifest、RPC/CLI/Manage Dash、per-project cooldown；见 ADR-0091。
- 新增 running task cooperative cancel/pause/resume：`task.cancel` / `task.pause` / `task.resume`、Worker turn/tool 边界检查，操作一个 task 不终止 session；见 ADR-0088。
- 新增可选 Developer ID 签名/notarization、cask tap 发布和 arm64 release runner；见 ADR-0090。
- 新增 opt-in user/volume read jail：`ShellPolicy.read_jail`、HOME parent + `/Volumes` deny、toolchain/worktree allowlist、Manage Dash/CLI，Agent/Console/PTY 统一；见 ADR-0089。
- 新增持久 task-DAG 队列与全局 write leases：`plan_tasks`/`write_leases`、plan lifecycle rebuild、priority+aging ready order、`task.queue`/`vp tasks`/Manage Dash、recovery 将 in-flight task 标为 interrupted 并清 lease；task 行已进入 replay/repair/bundle/db-rebuild；非 active session 可通过 `task.skip` / `task.retry` 和 Manage Dash 按钮跳过/重试 task，事件为 `plan_task.skipped` / `plan_task.retried`；opt-in `VERLINPARK_TASK_SCHEDULER` 可按 effective priority/round-robin resume 待办 session plan；见 ADR-0088。

### 0.2 2026-11-08 权限策略、转录面与 Collab 收敛

- **审批从“每次写/Shell 都问”改为风险分级**：`ApprovalMode` 三档 `smart` / `strict` / `unattended`，默认 `smart`。`smart` 放行工作区文件写入、常规构建/测试和外部只读/网络动作，只在 host-state/destructive 操作前询问；`strict` 恢复旧行为；`unattended` 不等待人工并拒绝 host-state/destructive。Settings → General 与 `VERLINPARK_APPROVAL_MODE` 可配置，daemon 仍是唯一决策者。
- **确定性分类器**：`crates/vp-core/src/permissions.rs` 不调用模型。它拆 Shell 段、跟踪 `cd`、扫描 `sh -c`/`eval`/解释器 `-c`/`find -exec`/`xargs`、双引号内命令替换和重定向；区分 workspace write、external、host-state、destructive；永久拒绝 `sudo`/`su`/`doas`、根目录删除、裸设备写入、`chroot` 和 fork bomb。`xargs` 无显式目标时按 stdin 目标处理为 host-state。ACP permission 走同一分类器；未知 ACP 请求为 host-state。
- **审批记录可审计**：`Approval` 增加可选 `approval_risk`/`approval_reason`，`approvals` 表增量迁移、bundle/replay/repair 保留；`permission.allowed` / `permission.denied` 事件记录自动放行和拒绝，不再只靠 tool 输出猜原因。Stage 与 Manage Dash 展示 risk/reason。
- **工具调用不再是卡片**：`TranscriptProjection` 把事件流投影为稳定行；tool call 是单行工具名 + 参数摘要 + agent + 耗时/状态，点击才展开平铺的输入/输出；running tool 立即出现，`tool.finished` 原地完成。审批行也改为扁平、少见的决策条，显示 risk/reason 后才给 Approve/Deny。
- **Collab 只保留编排状态**：`CollabSnapshot` 按真实 `parent_id` 将 Worker 归到 Manager 下，只渲染 Leader、Manager/Worker 组、未归属 Worker、Verifier、Labour 和一条计划进度；任务列表、roundtable 文本、Leader todo 明细、computer-lease 设计说明全部移出 Collab。
- **审计修复**：`write_file`/`edit_file` 拒绝 `.git` 元数据写入；approval schema 增量迁移保留旧行；移除已不再使用的 `AgentTool::requires_approval` 静态标志，避免与真实策略产生第二套事实；修复 `eval`/`sh -c` 递归时把当前目录误当 worktree root 的问题；`xargs` 无显式目标时不再自动放行。
- **仍未解决**：分类器不是形式化证明。任意解释器、Makefile、构建脚本仍可在 sandbox 内执行未枚举动作；shell 写入之外的任意读取、network allow 下的外发、daemon 介导写入仍未完全隔离。外部 ACP agent 仍是受信任二进制，不提供网络/凭证沙箱。`smart` 只覆盖已识别的 host-state 模式，新增工具必须显式分类；未知工具默认 `external`，在 `strict` 会询问。
- **测试**：权限分类矩阵、Shell 递归/重定向/`cd`/`xargs`、审批持久化与迁移、smart/strict/unattended 决策、smart 模式下写入不询问、blocked Shell 不询问、MCP/ACP risk 断言；Swift transcript running/finished/approval、tool summary、Collab 分组/计数、approval mode 兼容。见 ADR-0119。

### 0.3 2026-11-09 全量审查台账

- 全量审查覆盖 Rust runtime/security、store/protocol/daemon/CLI/providers、SwiftUI client、测试、文档和 invariants；116 个原始 finding 合并为 65 个 root-cause 条目，保留全部原始 ID。
- 完整表：[docs/audit/2026-11-09-findings.md](audit/2026-11-09-findings.md) 与 [CSV](audit/2026-11-09-findings.csv)。本切片修复了 P0 Git sandbox/任务 ID 碰撞，以及 merge queue、MCP、recovery、rebuild cost、bundle import、provider EOF/redaction、bounded tool output、sandboxed integration command、protocol auth negotiation、UI event pagination/selection/approval、write-lease resolver 等主要 P1/P2 问题；未完成项在表中标为 Partial/Deferred。
- 当前测试：Rust 316（core 256 / protocol 2 / store 44 / providers 14；3 ignored live），Swift 89；`just lint`、`cargo test --workspace`、`xcodebuild test` 通过。

---

## 1. 总体判断

### 1.1 已经跑通的部分

| 能力 | 评价 |
|---|---|
| Daemon / CLI / app 三端 | 真实可运行；JSON-RPC、WebSocket、SQLite、JSONL 事件日志齐全 |
| Session worktree | 每个 session 独立分支，project root 只读；diff 从 base commit 计算 |
| 四层组织 | Leader → Manager → Worker → Labour 严格链路；Manager 部门 title；真实 agent roster |
| Worker 隔离 | 每任务 worktree/branch；成功后 commit，再串行合并回 session 分支 |
| Verifier gate | 独立 Verifier；`submit_verdict`；approve 才合并。无 write/edit 工具，post-run tracked-file guard 会 reset 并拒绝修改；shell 仍可写 untracked/ignored 文件、读任意路径，不是完整沙箱 |
| TeamPlan | 结构化部门/task DAG/acceptance/write_scope；runtime 校验、持久化、自动派发 |
| Manager 圆桌 | 1–3 轮、并发、peer positions 跨轮共享、只读、事件可追踪 |
| 审批 / 预算 / 取消 / 暂停 | 写与 shell 审批；shell 写入 Seatbelt 隔离；session token budget；Stop / CLI cancel；session 级协作式 pause/resume；崩溃后标记失败 |
| 原生 UI | Glass Park 三栏、品牌/旗帜、Collab、Preview、Plan board、Trace |
| CI | Rust fmt/clippy/test + Swift format/build/test；当前 CI 绿 |

### 1.2 评分

| 维度 | 评分 | 说明 |
|---|---|---|
| 功能完整度（M3） | B | 主链路真实可跑；merge queue、Integrator、rebase+retry、显式/opt-in 自动冲突解决、本地 landing、`gh` PR、mailbox/artifacts/replay、search/transcript、持久 task-DAG 队列/global write leases、opt-in per-task dispatch 已有；缺 default-on auto-resolution、default-on autonomous dispatcher、durable cross-process session leases |
| 正确性/一致性 | B+ | P0 的 plan scope/caps 与预算 reservation 已修复；关键状态事件+投影已原子化，export/import/rehydrate/rebuild 已覆盖关键投影（含 memory/question）；`vp db-rebuild` 可从 manifest + mirrors 从零重建，但 imported archive/blobs/worktree 需要原文件存在 |
| 隔离与安全 | B− | 路径 jail、loopback token、fetch_url DNS 固定、shell 写入 Seatbelt、credential read-deny、process group 清理、项目级 network/read-deny policy 已具备；任意读取/network allow 下的外发/daemon 介导写入仍未隔离 |
| 可观测性 | B | 事件很丰富，Collab/Plan/Trace、Console、Terminal、Files、History、Doctor、cleanup、roundtable、跨 session approvals、project usage、按文件 diff、Rail 通知中心（历史持久化）可用；系统通知已支持（后台本地通知），更早的 round 受已加载 event window 限制 |
| 可维护性 | C− | `agent.rs` 约 9.4k 行（测试移到 `agent/tests.rs`）、`vp-core/lib.rs` 约 5.4k 行（测试移到 `tests.rs`）、`store.rs` 约 6.3k 行（测试移到 `store/tests.rs`）、`StudioView.swift` 约 2.4k 行；核心逻辑模块边界仍需治理 |
| 测试 | B− | 316 Rust + 89 Swift 全绿（+3 ignored live）；并发/scope/SSRF/shell sandbox/policy/home read jail/queue retry/auto-resolve/global admission/global Worker-Labour capacity/models/session+task+department budgets/usage ledger replay/mailbox retention/binary artifacts/GC/landing/PR/recovery/replay/provider retry/idle timeout/search/transcript/preview/plan batch abort/plan resume/project memory delete-prune-FTS-retention-policy/mid-run questions/MCP stdio persistent-pagination-resources-prompts-dynamic-tools/sandboxed console/PTY terminal/task-DAG queue/global write leases/project-level auto-resume policy/per-task-dispatch/task queue replay-bundle-rebuild/task skip-retry-running-cancel-pause-resume/user-volume read jail/isolated-verifier-checkout/ACP-external-agent/provider-stream-retry-partial-settlement/long-stream/live-harness/usage CSV/project usage/worktree retention/diff parser/OpenAI+Anthropic HTTP adapter E2E/queued recovery/db rebuild 已有回归；真实外部 Provider 由 env-gated live harness 覆盖但 CI 不运行 |
| 发布准备 | C+ | `just package` 默认 ad-hoc DMG + checksum；Developer ID/公证已明确暂缓，不要求 Apple 开发者账号；可选 tap 发布；仍无 Sparkle 更新器，首次打开可能需 Gatekeeper 确认 |

**一句话：架构骨架是对的，执行层还有几个必须补的洞，产品叙事需要从“已经有 agent team”收敛到“先把一条可合并的交付链做硬”。**

---

## 2. 必须优先处理的问题

### P0-1（已修复 2026-10-30）：自动派发绕过 org caps 和 plan scope 重叠校验

**证据**

- `execute_team_plan` 直接 `create_agent(Worker)` 和 `create_task_worktree`，没有调用 `SessionBudget::reserve(AgentRole::Worker)`；`ensure_plan_manager` 也没有 reserve Manager。手动 `delegate_many` 和单次 `delegate` 才会 reserve。
- 同一个函数把所有 ready task 直接 `take(config.max_workers)` 成一批，没有检查这些 task 的 `write_scope` 是否彼此重叠。
- `validate_team_plan` 只校验每个 scope 的格式，构造了 `scopes` 向量但没有做 pairwise overlap；`finish_worker` 只校验当前 Worker 自己的 changed paths 是否落在自己的 scope 内。
- `delegate_many` 有 `scopes_overlap` 检查，所以问题只出在 **TeamPlan 自动派发路径**。

**影响**

- 一个 100-task plan 可以在生命周期内创建远超 `max_workers` 的 Worker 记录；UI/roster 会看到“超出 cap”的团队。
- 两个 ready task 可以同时写同一个文件。由于集成是串行 `git merge`，结果要么冲突失败，要么 Git 自动合并后在语义上互相覆盖；这直接违背 README/AGENTS 里“parallel tasks must declare non-overlapping write scopes”的承诺。
- 这是当前最值得先修的 correctness bug，不是“以后再加”的功能。

**建议**

1. 在 `validate_team_plan` 或 batch 选择阶段做 pairwise scope overlap 检查；有重叠的 ready task 不要放进同一批。
2. 在 `execute_team_plan` 创建 Worker 前调用 `budget.reserve(AgentRole::Worker)`；Manager 创建同样 reserve。明确 cap 是“总数”还是“并发数”，并在 ADR/UI 统一语义。
3. 加回归测试：重叠 scope 的 plan 不能并行；超 cap 的 plan 被拒绝或降级为串行/排队。

---

### P0-2（已修复 2026-10-30）：session token budget 不是真正的 hard stop

**证据**

- `run_loop` 在每次 LLM 请求前检查 `budget.exhausted()`，请求结束后才 `record_usage` + `add_usage`。
- 单次请求本身可能超过剩余预算；并行 Worker / Manager roundtable 会同时通过请求前检查，然后各自完成一次调用。
- 因此实际超调量至少是一轮 `max_tokens`，并行时可能是 `并发数 × max_tokens`。

**影响**

- “hard-stop on a per-session token budget”是 README 和产品叙事的核心卖点；修复前更接近“下一轮不再开始”。
- 对高并发 plan，账单可能明显超过用户设定值。对一个强调“用户控制成本”的自托管平台，这是信任问题。

**建议**

1. 在发请求前做预算预留（reservation）：按 `max_tokens` / 估算输入 token 原子扣减，调用结束后按实际 usage 结算差额。
2. 剩余预算不足时，动态降低本次 `max_tokens`，或直接拒绝该调用。
3. 加并发测试：N 个 Worker 同时启动时，总 spend 不得超过 limit；至少要把超调上界写进文档，不能继续叫 hard stop。

---

### P1-3（部分修复 2026-10-30 / 2026-11-07）：Shell 写入已隔离，Verifier 已隔离到独立 clone；读取/网络/daemon 介导写入仍未完全解决

**证据**

- `Shell` 仍用 `sh -c` 执行任意命令，但已通过 `/usr/bin/sandbox-exec` 应用 Seatbelt 写入策略：只允许 worktree、worktree/common Git 元数据、per-agent scratch 和 `/dev/null`；common hooks/config 与 `config.worktree` 被拒绝；`sandbox-exec` 不可用时 fail closed。
- 常见 credential/state 路径已 read-deny（SSH/GPG/cloud/gh/kube/docker/Keychain、VerlinPark DB/run/session/log），但不是完整 read jail；自定义 secret 路径仍可能被读取并带进上下文或通过网络发送。
- 网络、进程执行没有策略；Docker socket、GUI/launch service 等 daemon 或外部应用介导的写入可以绕过文件写入沙箱。已 deny Apple Events，但不是完整隔离。
- shell 使用独立 process group；timeout/cancel/drop 会 kill 整组，后台进程不再遗留。
- 项目级 `ShellPolicy` 可设 `network=deny` 或追加 read-deny；它只能收窄，不能关闭写入沙箱、env allowlist 或内置 credential read-deny。
- `AgentRole::Verifier` 的 tool policy 仍允许 `shell`；prompt 里说“read-only”，但 shell 仍可修改 Verifier 自己的 worktree 后给出 approve。
- `kill_on_drop` 只保证直接子进程被清理；shell 已用 process group 补齐 timeout/cancel 清理。

**影响**

- 架构不变量“Agent 不能越界写”对直接文件写入已经成立；对任意读取、网络泄漏、Docker/GUI 介导写入和 Verifier 自审仍不成立。
- 在单用户本地环境里，审批仍是最后一道闸；审批 UI 现在会显示写入沙箱范围，但仍不展示读取/网络/进程副作用。

**建议**

- 短期：保持 Verifier 警告；继续把 read-deny 扩展为更完整的授权模型（目前仍是 denylist）。
- 中期：迁移到 App Sandbox helper/容器/VM；env allowlist、credential read-deny、process group 清理与项目级 network/read-deny policy 已实现，继续明确禁止 Docker socket 等逃逸路径。
- Verifier 现在在 isolated detached clone 中运行，不能再修改 Worker worktree/branch；但 network/credential 外层边界仍不是完整沙箱。

---

### P1-4（已修复 2026-10-30）：`fetch_url` 的 SSRF 防护可被 DNS 解析绕过

**证据**

- 代码只检查 URL host 字符串是否为 `localhost`、`.local`、`.internal`，以及 host 本身就是 IP 时是否 private/loopback/link-local。
- 没有在请求前解析 DNS 并固定到已校验的 IP；`http://attacker.example` 解析到 `127.0.0.1` 或 `169.254.169.254` 时会通过检查。
- redirect 被禁用是好事，但不能防 DNS rebinding / 首次解析即内网。

**影响**

- Research profile 的 Agent 可以让本机 daemon 访问内网服务或云元数据端点，再把响应带回模型上下文。
- 这是研究型平台很容易被忽略、但真实存在的攻击面。

**建议**

1. 使用自定义 resolver：解析 → 校验所有 A/AAAA → 固定 IP 连接，并设置 `Host` header；或使用支持 `resolve` 的客户端。
2. 同时阻断 IPv4-mapped IPv6、`0.0.0.0/8`、`100.64.0.0/10`、`198.18.0.0/15`、multicast、unspecified 等特殊段。
3. 加单元测试：公网域名解析到私网 IP、literal IPv6、重定向、超时、超大响应。

---

### P1-5（部分修复 2026-10-30）：事件溯源只做了一半，事件不是 source of truth

**证据**

- `update_session_status` 先更新 SQLite，再 append event；`record_usage` 同样先写 projection 再写 event。
- `update_agent_status` 只改 `agents` 表，不产生事件。
- `create_agent`、`append_message`、`set_team_plan` 先更新 projection，事件由调用方另行 append；两者不在同一个事务里。
- 因此 daemon 崩溃、事件 mirror 写失败、或调用方忘记 emit 时，事件日志和投影会分叉。

**影响**

- 修复前事件日志是审计流 + UI 增量来源，不是可完整 replay 的 event sourcing。
- 修复后 session status / usage / agent status / merge queue 已同事务写事件；关键投影可由事件重建（有测试）。
- team plan / todos / messages / recovery 已原子化；`session.replay` 校验关键投影，`--repair`/`--rebuild` 可修复/重建覆盖字段（含 agent roster），`session.export`/`import`/`rehydrate` 已形成 archive 迁移路径；`vp db-rebuild` 已可从 project manifest + per-session event mirrors 重建 projects/sessions/events/投影（无 `session.created` 的 imported archive 会跳过，worktree/blobs 需原样存在）。

**后续**

1. 把 team plan / todos / messages / recovery 搬到同一套 event+projection 事务。
2. `vp db-rebuild` 已覆盖 project manifest + session event mirrors；下一步是 manifest compaction、imported archive 重建、worktree/blob 校验和旧备份清理。
3. 保持文档明确区分“关键状态已原子化”和“完整 event sourcing 未完成”。

---

### P1-6（已修复，含显式 auto-resolve 2026-10-30）：没有 merge queue / Integrator，串行合并的失败不可恢复

**证据**

- `finish_worker` 在 Verifier approve 后直接 `merge_branch` 到 session worktree；`merge_branch` 冲突时 `merge --abort`，Worker 标记 failed。
- 修复前没有队列、没有 rebase/retry、没有 post-merge integration test、没有 Integrator gate。
- 并行 batch 的 task 都从当前 session branch 创建；后合并的 task 可能基于旧 base，冲突只能失败。

**影响**

- 一个 task 的合并失败会阻塞其 dependents，但没有重新排队/重新验证机制；用户只能手动处理。
- Verifier 审查的是 task branch，不是“合并后的集成结果”；跨 task 的语义冲突不会被发现。
- 修复后：持久 queue、确定性 merge、diff check、可选 integration command、失败回滚、冲突记录、rebase+retry、显式 agent-assisted conflict resolution、opt-in auto-resolve policy、本地 landing、显式 push/PR。仍缺默认开启的自动冲突解决、LLM Integrator、其他 forge PR。

**后续**

- queue retry、显式 auto-resolve、项目级 opt-in 自动触发与 GitHub PR flow 已支持；下一步是默认策略设计与其他 forge PR adapter。
- 可选 LLM Integrator 只做语义 review/冲突建议，不替代确定性 merge 和 rollback。
- 增加跨 entry 的 blocking/backpressure 和 post-merge 集成分支，避免后续 entry 基于旧 base。

---

### P1-7（第一版已实现 2026-11-07）：外部 ACP agent 的可信边界与协议覆盖

**证据**

- `acp_agents` 注册表支持 name/command/args/env/enabled/auto-approve/timeout；`delegate_acp` 仅对 Leader/Manager 开放。
- 启动前占用 Worker slot、daemon capacity permit、global write lease，并从 session branch 创建 task worktree。
- 进程通过 `sandboxed_command` 启动：Seatbelt 拒绝 worktree/Git metadata/scratch 之外的写入，HOME/XDG/TMPDIR 重定向到 scratch；配置的 stdio MCP servers 通过 `session/new` 传给外部 agent。
- `session/update` 映射为 message/thought/tool 事件；`session/request_permission` 映射到 approval center（allow_once/reject_once 优先）；cancel 发 `session/cancel` 并等待 10s；超时/非 end_turn 标记 failed/cancelled。
- `end_turn` 之后走 `finish_worker`：write-scope 检查、commit、isolated Verifier、merge queue、Integrator。
- `acp.agent.probe` 只做 initialize + session/new，不发送 prompt。

**影响**

- 外部 agent 的 branch、Worker 状态、审批、merge queue 与事件和原生 Worker 一致，可审计。
- 外部 agent 仍是用户安装的受信任二进制：默认 shell policy 下可读未 deny 的路径并使用网络；scratch HOME 使 ambient 凭证/状态不可见，但也意味着用户必须显式配置 env。
- 只支持 stdio；不暴露 ACP `fs/*`、`terminal/*` capability；没有 auth/login、session resume；外部 token usage 不进入 usage ledger。
- ACP 委派是同步的 Leader/Manager 工具，不是 plan task kind，plan auto-dispatch 不会调度 ACP Worker；session pause 不会在 ACP 运行中生效，session cancel 才会终止 ACP。

**后续**

- 评估 HTTP/SSE transport 与 `session/resume`。
- 如 ACP 稳定提供 usage 字段，映射到 per-call usage ledger（model 记为 `acp:<name>`）。
- 为外部 agent 增加 per-agent read-deny/network policy，而不是只继承项目 ShellPolicy。
- UI 增加 ACP session 事件时间线（当前复用通用 tool/message 事件）。

---

### P1-8（部分修复 2026-11-07）：provider stream 断线会丢失结算，且真实 Provider E2E 不可复现

**证据**

- 修复前 `run_loop` 在 stream item 返回 `Err` 时直接 `?`，尚未写 usage ledger、未 settle token reservation；partial text 已通过 `agent.message_delta` 发出，但 `agent.failed` 会清掉实时 transcript。
- 修复后 `consume_stream` 在没有任何 text/tool/usage 前对 transient stream error、idle timeout 最多 3 次 attempts；已有输出后不 replay。
- partial failure/cancel 使用 provider usage 或 partial text/tool 的估算记录 usage，settle reservation，并发出 `agent.message {partial:true}`；turn 仍然 failed/cancelled。
- `agent.probe` 增加 `chunks`、`first_token_ms`、`done`；`vp-providers/tests/live.rs` 提供默认 ignored 的真实 OpenAI/Anthropic/local E2E。
- 新增测试覆盖 stream retry、partial failure、cancelled partial、500-chunk long stream、probe 指标；原有 wiremock rate-limit/SSE 测试保留。

**影响**

- 有输出后的断线仍不能从断点续传；只能保留 partial output、结算费用并失败该 turn。这样避免重复文本/工具调用/计费，但长回答断线仍需用户重试。
- 如果 provider 实际收费但没有返回 usage 且没有输出，本地无法检测该费用。
- live tests 依赖用户自己的 key/网络，不在 CI 中运行。

**后续**

- 评估 provider 原生 resume（若 API 提供 event id / response id）而不是 replay prompt。
- 为 live harness 增加长流、限流与断线的用户侧脚本，而不仅是基础 streaming。
- 把 `agent.probe` 的 first-token/chunk 指标显示在 Settings 的 provider 状态里（本轮已接入基础展示）。

---

### P1-9（第一版已实现 2026-11-07）：缺少 per-task 调度，整份 plan 会作为一个调度单元

**证据**

- 旧 scheduler 只调用 `resume_plan`，一次性恢复整个 TeamPlan；虽然 batch 内部并行，但 session 以整份 plan 为单位占用调度窗口。
- 新增 `TaskSchedulerPolicy.dispatch_tasks`（默认 false），DB/manifest/RPC/CLI/Manage Dash 全链路持久化。
- `dispatch_next_task` 复用 plan sync、依赖排序、admission、session budget、Worker capacity、write scope 和 Verifier/Integrator，只把 `execute_team_plan` 限制为 1 个 claimed task；任务结束后 session 回到 idle。
- `execute_team_plan(max_tasks=Some(1))` 在 cancel/budget abort 时只结算当前 task，不把剩余 task 标成 cancelled/blocked；依赖失败导致的 no-ready 仍会把相关 task 标为 blocked。
- `task.dispatch` / `vp task-dispatch` / Manage Dash「Dispatch one task」支持手动 one-task 路径。
- 回归覆盖一次只启动一个 task、第二次调用执行下一个 task、取消当前 dispatched task 不取消未 claim 的剩余 task、policy/manifest round-trip 和 Swift 旧字段兼容。

**影响**

- dispatch 模式下同一 session 同时只跑一个 plan task，牺牲 intra-plan 并行换取跨 session 公平；用户按项目选择模式。
- task claim 是原子行状态更新，但 session 运行归属仍是进程内 `runs` map；两个 daemon 共享数据库时仍可能竞争 session 状态，durable session lease 未实现。
- 调度仍要求已存储的 TeamPlan；没有 plan 的 orphan task 不会被自动创建或调度。

**后续**

- 增加 durable session lease（owner + expiry + renewal），把 `runs` 的归属也持久化。
- 评估 default-on dispatcher 策略和 orphan task 的安全入口。
- 为 per-task 模式增加 UI 的按 session 队列和 next-task 预览。

---

### P2-15（第一版已实现 2026-11-08）：长会话没有上下文压缩

**证据**

- 修复前 top-level session 每次 run 都重新加载全部历史消息；工具输出和跨 run 上下文会持续增大 provider prompt。
- 新增 `AgentConfig.compact_after_tokens`（默认 0 = 从 context window 推导）与 `compact_keep_messages`（默认 8）；新增 global/per-role `context_window`（默认 128k，0 关闭）。
- `context_limits` 用 `window - output - system/tool tokens - 512` 计算 hard limit，summary threshold 为 75%；每个 top-level turn 前检查，不再只在 run 开始时压缩。threshold/hard-limit 比较计入当前 system/tool tokens，避免请求已经超窗才触发压缩。
- `compact_context_if_needed` 在安全 user/assistant 边界生成 summary；保留最近消息和当前 prompt。
- `context.compacted` 事件保存 summary、`through_message_id`、消息/token counts；原消息不删除，search/replay/transcript 仍可见。
- `load_conversation` 从最近 `context.compacted` 的 `through_message_id` 之后加载；summary mode 会把 summary 作为首条 active user message，trim mode 不注入 summary。
- usage 以 `compaction` role 进入 ledger，参与 token reservation/cost；失败发 `context.compact_failed`；若仍超 hard limit，按 user/assistant 边界滚动裁剪并写 `mode: "trim"` event。
- 测试覆盖成功压缩、resume 从 summary、原历史保留、usage ledger、replay 一致性、8k/128k/1M threshold 推导、summary 失败后的 rolling trim、压缩失败不 abort run，以及 `context.usage` 状态和 Swift decode。
- `session.context` / `vp context` 读取最新 `context.usage`、`context.compacted` 和 `context.measured`；Stage header 显示 active context 与 threshold，点击打开 popover，显示 window/hard limit/last compaction/last provider measurement。
- Provider 报告 usage 后写 `context.measured`，记录 heuristic estimate、actual input/output、raw ratio、upward-only EWMA calibration 和 samples；calibration 按 session/model 从事件恢复，clamp 1.0–2.0，并用于 active context estimate、compaction threshold/hard-limit 和 summary transcript budget。
- `session.compact` / `vp compact` / popover 可对 idle 等非运行 session 强制 compaction；复用 summary/trim pipeline 和 usage ledger，运行中/暂停/等待输入/imported 拒绝。手动 compaction 注册 manual run control，阻止并发 prompt；pause 返回 false，cancel 仍可取消 summary stream；失败返回 RPC error 并保留 `context.compact_failed`。
- `session.checkpoint` / `session.checkpoints` / `session.fork` / `vp checkpoint|checkpoints|fork` / Stage Fork Session 支持 checkpoint/fork。Checkpoint 要求预期 branch、非运行 session，允许 dirty worktree（临时 index snapshot tracked + untracked 非 ignored 变更，checkpoint ref 保持可达）；fork 从 checkpoint commit 创建独立 worktree/branch，复制 active conversation（summary + tail）并重建显示事件，source 不变；不复制 plan/todos/merge queue/approvals/usage。
- `session.checkpoint.delete` / `vp checkpoint-delete` 追加 `session.checkpoint.deleted` tombstone 并删除 snapshot ref；`session.checkpoint.prune` / `vp checkpoint-prune` 支持 `older_than_days` / `keep_latest` / dry-run，默认保护最新 N；Manage Dash「Session checkpoints」卡可删除/prune。
- `session.checkpoint.diff` / `vp checkpoint-diff` 返回 current HEAD→checkpoint 的 tracked numstat + patch（200k cap）+ clean/branch/commit；允许 dirty worktree 预览，不含 uncommitted 内容。`session.checkpoint.restore` / `vp checkpoint-restore` 在 session quiescent、clean、branch 匹配且 checkpoint commit 存在时 `reset --hard` 到 checkpoint；记录 `session.checkpoint.restored` previous/restored commit；Manage Dash 的 Preview/Restore 菜单与确认；不恢复 conversation/plan，fork branch 不受影响。
- 项目级 `CheckpointRetentionPolicy`（enabled/older_than_secs/keep_latest，默认关闭、30 天、keep 5）+ manifest/db-rebuild；daemon `VERLINPARK_CHECKPOINT_SWEEP_SECS` 默认 3600（0 关闭）按项目自动 prune，CLI `vp checkpoint-policy*` / Manage Dash 可配置。
- `session.lineage` 返回 parent/siblings/children（含 checkpoint/commit）；`session.compare` 同项目两个 branch tip 的 `git diff --numstat` + patch（200k chars cap）；CLI `vp lineage/compare`，Manage Dash Fork lineage Open/Compare；只比较 committed branch，不改 worktree。
- `session.fork.merge` / `vp fork-merge` 只允许 direct child fork、同项目、双方 quiescent、source clean 且 branch 正确；`--no-ff` merge，冲突 abort 并返回 paths，diff check/可选 integration command 失败 reset hard；成功/冲突/失败写 `session.fork.merge` 事件，source/fork operation lock，fork branch 保留。
- `session.fork.tree` / `vp fork-tree` 返回 depth ≤32、nodes ≤200 的递归树，`truncated` 标记 cap；Manage Dash Tree sheet 递归显示并可 Open。
- `project.fork.forest` / `vp fork-forest` 以 session.created parent metadata 构建 depth 32/nodes 500 forest；`project.fork.audit` 检查 missing parent/link/child/checkpoint commit/ref/stale tombstone ref；`project.fork.repair` 追加 missing fork link、重建/删除 ref；`session.checkpoint.stats` 报告近似 commit/blob bytes；Manage Dash Project fork forest 卡。
- `dashboard.snapshot` / `vp dashboard` 只读汇总跨 project sessions/status/tasks/leases/approvals/questions/checkpoints/forks 与 severity-sorted alerts（critical→warning→info，≤100 + truncated）；支持 project filter；含 per-project breakdown（running/queued/failed、approvals/questions、tasks、checkpoints、forks，按 activity 排序）；daemon 附加 `agent.info` capacity；Manage Dash Overview metrics/pending/attention/badges 使用该快照，未选 project 时显示 Projects breakdown，显式 refresh，不自动跑 Doctor/audit/provider。
- UI motion/interaction system：`VPMotion` 统一 tokens/springs/stagger/reduce-motion；`VPSegmentedControl` matched geometry、Rail/Dashboard/Settings 选中动画、Stage chat/composer/streaming、Studio Collab stagger + animated connectors、10-mode Preview chips、Settings sidebar shell、`⌘K` command palette、`⇧⌘F` focus mode、TopChrome、toast stack；见 ADR-0114。
- Warm graphite atelier UI：`VPTheme` 统一暖石墨表面/hairline/珊瑚强调色/语义色/角色色/项目色；Rail 内联展开 session、活动计数、running pulse、show-more、新 session/inbox/settings；Stage 单 header、880pt transcript、可展开 tool output、focus-ring composer；Inspector 持久化 `⌥⌘I`；Settings/Dashboard/Studio/sheet/command palette/resident surface 统一重做；resident orbital mark；见 ADR-0115。视觉回归仍是手工检查（截图 + Reduce Motion + 1120px 最小窗口），未进入 CI。
- Black crystal glass refinement：cold ice accent、深钢 primary fill、无 serif/coral/sparkles/orbit；`VPCrystalBackdrop` 静态 facet、Rail/Stage/Studio/TopChrome 使用 `VPCrystalSurface` material + tint、半透明 surface token、specular `edgeGradient`、圆角方形玻璃按钮、静态 `cube.transparent` crystal mark；见 ADR-0116。
- Runtime localization + responsive Studio + frosted fields：`LanguageStore` + `zh-Hans.lproj/Localizable.strings`（约 450 键），Settings General 与 TopChrome globe 即时切换 EN/简体中文；Settings Providers 角色模型行改为两层卡片、输入框 frosted；Studio Files/History 在窄 inspector 下改为单栏 + 返回；Diff/状态/角色色提高对比；见 ADR-0117。
- Workspace shell hardening：`WorkspaceLayout` 三栏自适应（rail 240–320 / inspector 320–560 / stage ≥520）、可拖拽分隔条、持久化窗口 frame（屏幕内校验 + 防抖）、焦点模式退出胶囊；`CommandPaletteRanker` fuzzy/word-prefix/subsequence + recents + 高亮；`NotificationInboxModel` filter/search/project/day groups/counts + clear-read/remove；native Workspace command menu/router、pinned items、rail sort、search source filters + highlights、layout presets/sliders、Reduce Transparency fallback；见 ADR-0118。
- LM Studio reasoning-model 实机验证：旧 `agents` schema 增量迁移、reasoning-model probe 预算/超时、session priority 与 cost/time budgets 同时保留、OpenAI-compatible `stream_options.include_usage`（400 fallback）、mailbox recipient 指引；`empty3` 实机完成 provider probe、README read、write approval 与真实 usage；Rust 270 / Swift 83。
- `update.check` 支持 `.dmg`/`.sha256` assets；`update.download` 校验 GitHub URL/redirect、512MB cap、流式 SHA-256、原子 rename 到 `~/Downloads`，不自动安装。
- `model.preset.*` 全局 model presets（provider/model/context/prices/notes，不含密钥）；provider 匹配时应用 role 字段，仍需 Save & Connect；Settings Providers 模态。
- `update.check` / `vp update-check` 查询 GitHub latest release，校验 `owner/repo`，numeric version compare；Settings → About 手动检查并打开 release page，不自动下载/安装。

**影响**

- 长会话的 provider prompt 和成本下降；summary 可能丢失未被摘要 prompt 判定为重要的细节。
- 压缩本身消耗 token，阈值刚被超过时会多一次 provider round trip。
- 只在 top-level run 开始压缩，不在 run 中压缩；不会修复单个超长 tool result。
- 已有 session checkpoint/fork；仍没有 compaction checkpoint/resume；compaction 是本地 summary 策略，不是 provider-native context management。

**后续**

- 评估 run 中按 tool 边界压缩、per-message artifact 引用和 summary 的 UI 展示。
- 增加 checkpoint/fork 与 provider-native context/caching 适配。
- 把 compaction 阈值/保留条数暴露到 Settings。

---
## 3. 次级问题与债务

### P2-7（已修复 2026-10-31）：batch abort 后同批 task 状态可能悬挂

`execute_team_plan` 在某个 task 的 `finish_worker` 返回 `BudgetExceeded` 或 `Cancelled` 时，只标记当前 task 和“不在本 batch 的 remaining”，同一 batch 中尚未处理的兄弟 task 可能仍留在 `running` 状态。UI 的 Plan board 会显示一直运行。现在 abort 后会先结算整个 batch（cancelled/blocked + `plan_task.finished`），再返回；见 ADR-0065。

### P2-8（已修复 2026-10-31）：worktree / branch 生命周期没有自动清理

`worktree::remove` 原先只在 session 创建失败时调用；task worktree 和 `verlinpark/<session>--<task>` 分支在成功/失败/取消后都保留。现在有显式 `project.cleanup_worktrees` / `vp cleanup-worktrees`，以及项目级 opt-in `WorktreeRetentionPolicy` + daemon sweep（默认 1 小时，`VERLINPARK_WORKTREE_SWEEP_SECS=0` 可关闭）。默认保留失败/取消现场和 branch；dirty worktree 仍跳过。shell scratch 没有统一回收，也没有全局磁盘上限。

### P2-9：Dashboard / Settings / Studio 有过期文案（审计时）

审计时发现：

- Dashboard 写 “0 active”“Managers 3–5 · Workers 2–10 · Labour 3–40”“M1 · Leader chat…”“org graph is a design preview until the agent runtime lands”。
- Settings/Studio 写 “Parallel workers, per-task worktrees, and auto-expansion are not implemented yet”。
- Studio 的 tier range 写 Workers 1–10、Verifiers 1–10、Labour 1–20，与 runtime caps（10 / 100 / 40）不一致。

这些不是功能 bug，但会让用户/贡献者误判当前能力，属于“玻璃盒”产品的可信度问题。本次已做最小文案修正（Dashboard、Org sheet、Computer lease、tier ranges、预算提示）；更完整的 UI 状态化应随后处理。

### P2-10（已修复 2026-10-31）：Manager 圆桌在 UI 中不可见

`manager.roundtable` 事件原先只出现在 Preview → Trace 的原始 JSON 行里。Collab 现在有 Roundtable 卡片：按轮次分组、显示 Manager title/profile/position 摘要，可展开/折叠；完整原始 payload 仍在 Trace。

### P2-11：代码与文档体量

- `crates/vp-core/src/agent.rs` 5,060 行，同时承担角色策略、loop、委派、Verifier、TeamPlan、roundtable、测试。
- `apps/macos/VerlinPark/Features/StudioView.swift` 1,145 行，同时承担 Collab、Plan、Preview、Trace、Org sheet。
- `docs/PLAN.md` 66 KB，包含早期三层组织/旧 UI 章节；当前实现状态只在少数行更新，容易继续漂移。

建议拆分（不是现在必须做）：

```text
crates/vp-core/src/agent/
  mod.rs          # AgentService / config
  roles.rs        # AgentRole / AgentProfile / tool policy
  budget.rs       # SessionBudget / caps
  run.rs          # run_loop / approvals / cancellation
  delegate.rs     # delegate / delegate_many / task worktrees
  verify.rs       # Verifier gate
  plan.rs         # TeamPlan validation / dispatch / roundtable
  prompts.rs      # role prompts
```

Swift 对应拆成 `CollabView`、`PlanBoardView`、`PreviewView`、`OrgPresetSheet`、`TierCard`。

### P2-12：测试覆盖缺口

已有 316 个 Rust 测试 + 89 个 Swift 测试，覆盖 Leader loop、provider probe/retry/idle timeout/stream retry before output/partial usage settlement/context compaction/model-aware context limits/rolling trim/active-request accounting/context usage status/provider-usage calibration/manual context compaction/session checkpoint-fork/checkpoint-retention/checkpoint-policy-sweep/fork-lineage-compare/fork-merge-back/recursive-fork-tree/project-fork-forest-audit/checkpoint-diff-restore/checkpoint-stats/verified-update-download/model-preset-catalog/dashboard-snapshot/motion-interaction-system/UTF-8-safe truncation、delegation、task worktree、parallel scope、Verifier、TeamPlan、roundtable、session/task/department/project 预算、恢复、pause/resume、queued session 恢复、shell 沙箱/policy/home read jail、process group 清理、agent-assisted conflict resolution、opt-in auto-resolve policy、priority-aware aging global session admission、persistent task-DAG queue/global write leases/project-level auto-resume policy/per-task dispatch、cross-session scope locks、priority-aware aging task+session global Worker/Labour capacity/budget-gated plan auto-expansion、per-role models/USD/time budgets、per-call usage ledger 与 replay/repair、mailbox retention、project memory（delete/prune/FTS/replay/retention policy）、mid-run questions、MCP stdio/HTTP/SSE（Streamable HTTP + legacy SSE、session/protocol headers、same-origin/secret-header policy、probe/persistent/pagination/resources/prompts/dynamic tools）、ACP stdio（initialize/session/new/prompt/permission/update/timeout/probe/Worker integration）、sandboxed console、PTY terminal、workspace files/edit、Git history、artifact replay/repair、content-addressed blobs 与 GC、approval replay/repair、event mirror restore、search FTS5-prefix/filter/rebuild、transcript、plan batch abort、plan resume、usage CSV、project usage、worktree retention、diff parser、OpenAI+Anthropic HTTP adapter E2E、db rebuild。仍缺：

- 并发预算超调上界与预算 abort 的 sibling 结算（当前只测取消 abort）
- 其他 forge PR adapter / 默认自动冲突解决策略 / 真正的 post-merge 集成分支
- WebSocket 断线重连
- 真实外部 Provider E2E 已有 env-gated live harness（OpenAI/Anthropic/local，默认 ignored）；CI 不运行，仍需要用户凭证人工验证。本地 wiremock 全链路已覆盖 OpenAI-compatible 与 Anthropic
- search 已有 FTS5 索引与 prefix 查询；仍缺 BM25/相关性排序和语义搜索
- transcript 的 event timeline 与增量导出
- roundtable 卡片的 UI snapshot / accessibility 回归
- macOS UserNotifications / 前台通知 delegate（历史已持久化到 UserDefaults）
- 单 task 操作仍为协作式：pause/cancel 在 turn/tool 边界生效，不抢占 in-flight provider stream/tool；skip/retry 只允许非 active session，skipped task 不会被 global scheduler 自动重试（这是有意的）
- project-level auto-resume policy 的长时间运行/多 session admission 压力测试

### P2-13：产品叙事大于实现

审计时 README 仍说 agent “collaborate through typed messages and shared artifacts”，架构图把 MCP、ACP、artifact store、merge queue 画成已有组件；本次已把 typed mailbox/artifact、MCP/ACP 标为 planned，并补上 merge queue/Integrator 的真实实现；MCP stdio/HTTP/SSE client 与 ACP client 现已实现。后续仍需在每次能力变化时维护 **current / planned** 分离，否则第一次认真试用就会掉信任。

---

## 4. 产品层重审

### 4.1 真正的差异化

1. **Native macOS + 本地 daemon**：不是 webview，不是把 IDE 包一层。这个方向是对的。
2. **四层组织 + 部门 title**：比“subagent 列表”更接近真实工程组织；Manager 圆桌是合理的低成本协作原语。
3. **worktree 隔离 + Verifier gate**：这是多 Agent 产品最难做对的部分，当前已经有一条真实链路。
4. **事件/Plan/Trace 的玻璃盒**：可审计性是长期护城河，尤其对不信任大厂 harness 的用户。
5. **品牌/旗帜/Glass UI**：有辨识度，但不应挤占 runtime 可靠性。

### 4.2 当前最大的产品风险

**“组织层很热闹，交付链没有闭合。”**

- 用户可以看见 Managers、Workers、Plan board、圆桌、merge queue，并可本地 landing 或 push/PR；仍缺默认自动冲突解决策略与其他 forge PR。
- 并行 task 的 scope/caps 与预算已经收紧；剩下的是 Verifier shell 不是沙箱、事件链不是完整 replay、冲突需要人工处理。
- 这些会让“更少限制、更高效率”的主张反噬：限制少了，但失败模式更多。

### 4.3 建议的 milestone 顺序

**M2.5 — 把承诺变成约束（1–2 天）**
1. 修 P0-1（plan caps + scope overlap）。
2. 修 P0-2（预算 reservation / 超调上界）。
3. 修 P1-4（SSRF）。
4. 明确 Verifier shell 策略并补测试。
5. 清理过期 UI 文案（本次已做一部分）。

**M3 — Merge Queue + Integrator + Landing（已跑通第一版）**
1. queue 状态机、冲突 rebase/retry、集成检查、本地 landing、显式 push/PR 已实现。
2. 确定性 Integrator 事件 + Queue UI 已实现；可选 LLM Integrator 仍待做。
3. 下一步：一条真实 fixture 的端到端 demo：Leader plan → 多 Worker → Verifier → queue → 集成测试 → 人类 review → land/PR。
4. 没有这个 demo，不建议继续加 auto-expansion、per-role token/task budget trees、content-addressed artifact dedupe。

**M3.5 — 事件与恢复**
1. 统一 event/projection 写路径，或把文档改成 audit log。
2. replay/export、worktree 清理（已实现显式清理）、doctor 诊断、断线重连。

**M4 — 平台化**
1. binary artifact preview（content-addressed dedupe、zstd compression、artifact replay/repair、legacy backfill、mailbox auto GC 已实现）。
2. project-wide budget quotas and alerts（project 共享 token/USD pool 已实现；per-project soft warning/alert 未做）。
3. Task-DAG 行、task priority/aging 和 global write leases 已持久化；已有 project-level auto-resume policy（按 effective priority/round-robin resume plan 或 per-task dispatch）、task skip/retry/cancel/pause/resume、replay repair/bundle/db-rebuild。仍缺 default-on autonomous dispatcher、无存储 plan 的 orphan task 调度。跨进程 priority/capacity/session lease 只有在移除当前 `run/daemon.lock` 的排他 data-dir 假设、进入 server 模式后才有意义。
4. ACP 第一版已实现（stdio/Worker/Verifier/merge）；仍缺 HTTP/SSE、session resume、auth flow 与 usage 映射；终端、hunk review、其他 forge PR adapter 仍待做。

---

## 5. 整理与硬化范围

第一轮（审计当天）只做文档与过期文案：

- 新增本审计与 `docs/README.md` 文档索引。
- 更新 README、PROGRESS、PLAN、ARCHITECTURE、DECISIONS、AGENTS 的状态与边界描述。
- 修正 Dashboard / Settings / Studio / NewSessionSheet 中与当前实现冲突的文案和范围数字。

第二轮（2026-10-30）做 runtime 硬化：

- plan batch 的 `write_scope` 选择与 active Worker slot。
- token reservation、动态 output budget、无 usage 端点的估算用量。
- `fetch_url` 的 DNS 解析 + IP 固定 + 特殊地址段检查。
- Verifier shell 审批提示（仍不是沙箱）。
- 新增 plan scope/cap、token reservation、IP 分类的回归测试。
- merge queue、确定性 Integrator、失败回滚和冲突记录。

第三轮（2026-10-30）做交付闭环：

- queue rebase/retry（人工触发）。
- 本地 landing（clean + default branch 检查，`--no-ff`）。
- 显式 push + `gh pr create`（preflight、no fake success）。

第四轮（2026-10-30）做恢复：

- daemon 重启把 queued/merging/checking entry 标为 interrupted，并补事件。
- interrupted entry 可走同一条 rebase+retry 路径。
- daemon 重启后的 in-flight merge queue recovery。

第五轮（2026-10-30）做审计一致性：

- session status / usage / agent status / merge queue 的 event+projection 原子事务。
- team plan / todos / messages / recovery 原子化。
- `session.replay` / `vp replay` 投影一致性检查、`--repair` 与 `--rebuild`。
- `session.export` / `vp export` bundle、`session.import` / `vp import` archive（含 approvals）、`session.rehydrate` / `vp rehydrate`（含 agent task-worktree remap）。
- 事件流重建关键投影的回归测试。

---

## 6. 复现与验证

```bash
cargo test --workspace     # 253 passed after the 11-08 Manage Dash IA slice
just lint                  # fmt + clippy -D warnings + swiftformat lint
just test                  # Rust + xcodebuild test (40 Swift tests)
```

审计基线 CI：GitHub Actions run `40902407039`（commit `cbef2e6`）成功。
硬化后 CI 以最新提交为准。
