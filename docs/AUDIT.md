# VerlinPark 项目重审

> 审计日期：2026-10-29  
> 审计基线：`cbef2e6`（`main`，Manager roundtable 已合入）  
> 范围：Rust runtime、SwiftUI app、协议/存储、Git 集成、权限与预算、文档与仓库卫生  
> 结论先行：**M2 的功能闭环已经成立，但仍是一个强原型，不是可靠的多 Agent 平台。** P0 约束、merge queue、确定性 Integrator、queue retry、本地 landing、GitHub PR flow、pause/resume 与 shell 写入沙箱已经补上；下一步是完整 event sourcing、完整 shell 读取/网络隔离与非 GitHub PR adapter。

## 0. 2026-10-30 跟进

第一轮硬化已完成（同一 `main` 线，提交见仓库历史）：

- **P0-1 已修复**：TeamPlan 自动派发按 `write_scope` 选择可并行批次；空 scope / 重叠 scope 的任务串行执行。Worker 使用可释放的 active slot，Manager 继续按 run 计数；`available(Worker)` 限制 batch 大小。
- **P0-2 已修复**：provider 请求前做 token reservation，按保守输入估算 + 动态 output budget 预留；provider 返回 usage 后结算，缺少 usage 的本地端点会以 `estimated: true` 的估算用量持久化。并发批次不再共用同一个 pre-flight 窗口。
- **P1-4 已修复**：`fetch_url` 在连接前解析并固定已校验的 IP，阻止解析到 loopback/private/link-local/文档/benchmark 等地址；补充了 IPv4-mapped IPv6 与特殊段检查。
- **P1-3 部分修复**：shell 现在通过 macOS Seatbelt 拒绝 worktree/Git 元数据/scratch 之外的写入，并保护 common hooks/config；sandbox 不可用时 fail closed。常见 credential/state 路径已 read-deny，timeout/cancel 会清理整个 process group，项目级 policy 可 deny network 或追加 read-deny（只能收窄）；任意读取、network allow 下的外发、进程执行策略和 Docker 等 daemon 介导写入仍未隔离，Verifier 仍可修改自己的 worktree。
- **P1-6 已修复（第一版）**：Verifier-approved commit 进入持久 merge queue，确定性 Integrator 执行 merge、diff check 和可选项目检查，失败回滚，冲突记录；conflict/failed 可 rebase+retry，冲突可显式调用 resolver agent 解决并重新验证；项目级 opt-in policy 可在 session idle 时自动触发，带 attempt/token cap；本地 landing 和 `gh` PR flow 可用。仍缺默认开启的静默自动冲突解决、LLM Integrator、非 GitHub PR adapter。
- **队列恢复**：daemon 重启把 queued/merging/checking entry 标为 `interrupted` 并补事件；interrupted 可人工 rebase+retry。
- **P1-5 部分修复**：session status、usage、agent status、team plan、todos、messages、merge queue、recovery 使用 event+projection 原子事务；`session.replay` / `vp replay --repair` 可检查并修复覆盖的关键投影；agent roster 已通过 `agent.snapshot` 覆盖；`session.export` / `--rebuild` / `session.import` 已实现；导入 session 默认是只读 archive、保留 approvals；`session.rehydrate` 可显式重建 session worktree，并按 branch 分组 remap imported agent task-worktrees。
- **仍未解决**：完整 shell read jail/网络隔离、完整 event sourcing、静默自动冲突解决、非 GitHub PR、worktree/scratch 清理、部分 merge 状态的人工检查、P2 batch abort 状态一致性。

当前测试：Rust 95（core 77 / store 13 / providers 5），Swift 9。CI 以最新提交为准。

---

## 1. 总体判断

### 1.1 已经跑通的部分

| 能力 | 评价 |
|---|---|
| Daemon / CLI / app 三端 | 真实可运行；JSON-RPC、WebSocket、SQLite、JSONL 事件日志齐全 |
| Session worktree | 每个 session 独立分支，project root 只读；diff 从 base commit 计算 |
| 四层组织 | Leader → Manager → Worker → Labour 严格链路；Manager 部门 title；真实 agent roster |
| Worker 隔离 | 每任务 worktree/branch；成功后 commit，再串行合并回 session 分支 |
| Verifier gate | 独立 Verifier；`submit_verdict`；approve 才合并（只读是 prompt 约束；shell 写入限于其 worktree，但仍可修改该 worktree） |
| TeamPlan | 结构化部门/task DAG/acceptance/write_scope；runtime 校验、持久化、自动派发 |
| Manager 圆桌 | 1–3 轮、并发、peer positions 跨轮共享、只读、事件可追踪 |
| 审批 / 预算 / 取消 / 暂停 | 写与 shell 审批；shell 写入 Seatbelt 隔离；session token budget；Stop / CLI cancel；session 级协作式 pause/resume；崩溃后标记失败 |
| 原生 UI | Glass Park 三栏、品牌/旗帜、Collab、Preview、Plan board、Trace |
| CI | Rust fmt/clippy/test + Swift format/build/test；当前 CI 绿 |

### 1.2 评分

| 维度 | 评分 | 说明 |
|---|---|---|
| 功能完整度（M3 第三切片） | B | 主链路真实可跑；merge queue、Integrator、rebase+retry、显式/opt-in 自动冲突解决、本地 landing、`gh` PR 已有；缺 artifact、mailbox、默认自动策略 |
| 正确性/一致性 | B | P0 的 plan scope/caps 与预算 reservation 已修复；关键状态事件+投影已原子化，export/import/rehydrate/rebuild 已覆盖关键投影；从零数据库重建仍未完成 |
| 隔离与安全 | B− | 路径 jail、loopback token、fetch_url DNS 固定、shell 写入 Seatbelt、credential read-deny、process group 清理、项目级 network/read-deny policy 已具备；任意读取/network allow 下的外发/daemon 介导写入仍未隔离 |
| 可观测性 | B | 事件很丰富，Collab/Plan/Trace 可用；圆桌只出现在原始 Trace，缺少面向人的卡片 |
| 可维护性 | C | `agent.rs` 5,060 行、`StudioView.swift` 1,145 行；文档状态与实现漂移 |
| 测试 | B− | 95 Rust + 9 Swift 全绿；并发/scope/SSRF/shell sandbox/policy/queue retry/auto-resolve/global admission/global Worker-Labour capacity/models/session+task+department budgets/usage ledger replay/mailbox retention/binary artifacts/GC/landing/PR/recovery/replay 已有回归，真实 Provider 路径仍缺 |
| 发布准备 | D | unsigned debug build；无公证、无更新、无 release 流程 |

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

### P1-3（部分修复 2026-10-30）：Shell 写入已隔离，但读取/网络/daemon 介导写入和 Verifier 只读仍未解决

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
- 在宣传“agent team 自动合并”之前，Verifier 只读仍必须保持 prompt-level 的诚实表述。

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
- team plan / todos / messages / recovery 已原子化；`session.replay` 校验关键投影，`--repair`/`--rebuild` 可修复/重建覆盖字段（含 agent roster），`session.export`/`import`/`rehydrate` 已形成 archive 迁移路径；从零数据库导入仍未完成。

**后续**

1. 把 team plan / todos / messages / recovery 搬到同一套 event+projection 事务。
2. 继续做跨环境 approvals 导入与 session/project 创建元数据的从零 rebuild；当前 export/import/rehydrate 覆盖事件与关键投影。
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
- 修复后：持久 queue、确定性 merge、diff check、可选 integration command、失败回滚、冲突记录、rebase+retry、显式 agent-assisted conflict resolution、opt-in auto-resolve policy、本地 landing、显式 push/PR。仍缺默认开启的自动冲突解决、LLM Integrator、非 GitHub PR。

**后续**

- queue retry、显式 auto-resolve、项目级 opt-in 自动触发与 GitHub PR flow 已支持；下一步是默认策略设计与非 GitHub PR adapter。
- 可选 LLM Integrator 只做语义 review/冲突建议，不替代确定性 merge 和 rollback。
- 增加跨 entry 的 blocking/backpressure 和 post-merge 集成分支，避免后续 entry 基于旧 base。

---

## 3. 次级问题与债务

### P2-7：batch abort 后同批 task 状态可能悬挂

`execute_team_plan` 在某个 task 的 `finish_worker` 返回 `BudgetExceeded` 或 `Cancelled` 时，只标记当前 task 和“不在本 batch 的 remaining”，同一 batch 中尚未处理的兄弟 task 可能仍留在 `running` 状态。UI 的 Plan board 会显示一直运行。建议在 abort 时统一结算整个 batch，并 emit 对应的 `plan_task.finished`。

### P2-8：worktree / branch 生命周期没有清理

`worktree::remove` 只在 session 创建失败时调用；task worktree 和 `verlinpark/<session>--<task>` 分支在成功/失败/取消后都保留。长时间使用会积累磁盘和 refs。需要 session 归档/清理策略，以及“保留失败现场供审阅”的显式选项。

### P2-9：Dashboard / Settings / Studio 有过期文案（审计时）

审计时发现：

- Dashboard 写 “0 active”“Managers 3–5 · Workers 2–10 · Labour 3–40”“M1 · Leader chat…”“org graph is a design preview until the agent runtime lands”。
- Settings/Studio 写 “Parallel workers, per-task worktrees, and auto-expansion are not implemented yet”。
- Studio 的 tier range 写 Workers 1–10、Verifiers 1–10、Labour 1–20，与 runtime caps（10 / 100 / 40）不一致。

这些不是功能 bug，但会让用户/贡献者误判当前能力，属于“玻璃盒”产品的可信度问题。本次已做最小文案修正（Dashboard、Org sheet、Computer lease、tier ranges、预算提示）；更完整的 UI 状态化应随后处理。

### P2-10：Manager 圆桌在 UI 中不可见

`manager.roundtable` 事件只出现在 Preview → Trace 的原始 JSON 行里；Collab 没有圆桌卡片，Chat 也不显示。用户看不到“Manager 们讨论了什么、分歧在哪、最后怎么收敛”。建议在 Collab 增加 Roundtable 卡片：轮次、Manager title、position 摘要、peer 关系；点击展开完整内容。

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

已有 95 个 Rust 测试 + 9 个 Swift 测试，覆盖 Leader loop、delegation、task worktree、parallel scope、Verifier、TeamPlan、roundtable、session/task/department 预算、恢复、pause/resume、shell 沙箱/policy、process group 清理、agent-assisted conflict resolution、opt-in auto-resolve policy、global session admission、cross-session scope locks、global Worker/Labour capacity、per-role models/USD/time budgets、per-call usage ledger 与 replay/repair、mailbox retention、text/binary artifacts 与 GC。仍缺：

- plan 自动派发的 scope overlap / cap 回归
- 并发预算超调上界
- `fetch_url` 的 DNS/私网/IPv6 SSRF
- 非 GitHub PR adapter / 默认自动冲突解决策略 / 真正的 post-merge 集成分支
- WebSocket 断线重连
- 真实 Anthropic 工具调用（目前只有 mock/SSE 解析测试）
- UI 的 event → plan status / roundtable 渲染
- worktree/branch 清理

### P2-13：产品叙事大于实现

审计时 README 仍说 agent “collaborate through typed messages and shared artifacts”，架构图把 MCP、ACP、artifact store、merge queue 画成已有组件；本次已把 typed mailbox/artifact、MCP/ACP 标为 planned，并补上 merge queue/Integrator 的真实实现。后续仍需在每次能力变化时维护 **current / planned** 分离，否则第一次认真试用就会掉信任。

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

- 用户可以看见 Managers、Workers、Plan board、圆桌、merge queue，并可本地 landing 或 push/PR；仍缺默认自动冲突解决策略与非 GitHub PR。
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
2. replay/export、worktree 清理、断线重连。

**M4 — 平台化**
1. artifact 压缩/内容级 replay/rebuild 与自动 mailbox retention（content-addressed dedupe 已实现）。
2. cross-session shared budget pools（session 内 task/department 已有原子 reservation；跨 session 共享池未实现）。
3. 持久化 global Task-DAG scheduler / 公平队列 / 跨进程 capacity；auto-expansion。
4. MCP/ACP、终端、hunk review、非 GitHub PR adapter。

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
cargo test --workspace     # 95 passed after the 10-31 artifact-blob slice
just lint                  # fmt + clippy -D warnings + swiftformat lint
just test                  # Rust + xcodebuild test (9 Swift tests)
```

审计基线 CI：GitHub Actions run `40902407039`（commit `cbef2e6`）成功。
硬化后 CI 以最新提交为准。
