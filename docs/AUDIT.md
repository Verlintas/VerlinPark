# VerlinPark 项目重审

> 审计日期：2026-10-29  
> 审计基线：`cbef2e6`（`main`，Manager roundtable 已合入）  
> 范围：Rust runtime、SwiftUI app、协议/存储、Git 集成、权限与预算、文档与仓库卫生  
> 结论先行：**M2 的功能闭环已经成立，但仍是一个强原型，不是可靠的多 Agent 平台。** 当前最需要的不是继续加组织层功能，而是把已经承诺的约束真正执行，并补上 merge queue / Integrator 闭环。

---

## 1. 总体判断

### 1.1 已经跑通的部分

| 能力 | 评价 |
|---|---|
| Daemon / CLI / app 三端 | 真实可运行；JSON-RPC、WebSocket、SQLite、JSONL 事件日志齐全 |
| Session worktree | 每个 session 独立分支，project root 只读；diff 从 base commit 计算 |
| 四层组织 | Leader → Manager → Worker → Labour 严格链路；Manager 部门 title；真实 agent roster |
| Worker 隔离 | 每任务 worktree/branch；成功后 commit，再串行合并回 session 分支 |
| Verifier gate | 独立 Verifier；`submit_verdict`；approve 才合并（只读是 prompt 约束，shell 未沙箱） |
| TeamPlan | 结构化部门/task DAG/acceptance/write_scope；runtime 校验、持久化、自动派发 |
| Manager 圆桌 | 1–3 轮、并发、peer positions 跨轮共享、只读、事件可追踪 |
| 审批 / 预算 / 取消 | 写与 shell 审批；session token budget；Stop / CLI cancel；崩溃后标记失败 |
| 原生 UI | Glass Park 三栏、品牌/旗帜、Collab、Preview、Plan board、Trace |
| CI | Rust fmt/clippy/test + Swift format/build/test；当前 CI 绿 |

### 1.2 评分

| 维度 | 评分 | 说明 |
|---|---|---|
| 功能完整度（M2） | B | 主链路真实可跑；缺 merge queue、Integrator、artifact、mailbox、landing |
| 正确性/一致性 | C+ | 计划派发绕过部分 caps/scope 校验；预算不是严格硬停；事件溯源只做了一半 |
| 隔离与安全 | C+ | 路径 jail 和 loopback token 做得不错；shell 无沙箱，fetch_url 有 DNS SSRF 缺口 |
| 可观测性 | B | 事件很丰富，Collab/Plan/Trace 可用；圆桌只出现在原始 Trace，缺少面向人的卡片 |
| 可维护性 | C | `agent.rs` 5,060 行、`StudioView.swift` 1,145 行；文档状态与实现漂移 |
| 测试 | B− | 45 Rust + 9 Swift 全绿；但关键并发/安全/合并冲突路径缺回归测试 |
| 发布准备 | D | unsigned debug build；无公证、无更新、无 release 流程 |

**一句话：架构骨架是对的，执行层还有几个必须补的洞，产品叙事需要从“已经有 agent team”收敛到“先把一条可合并的交付链做硬”。**

---

## 2. 必须优先处理的问题

### P0-1：自动派发绕过 org caps 和 plan scope 重叠校验

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

### P0-2：session token budget 不是真正的 hard stop

**证据**

- `run_loop` 在每次 LLM 请求前检查 `budget.exhausted()`，请求结束后才 `record_usage` + `add_usage`。
- 单次请求本身可能超过剩余预算；并行 Worker / Manager roundtable 会同时通过请求前检查，然后各自完成一次调用。
- 因此实际超调量至少是一轮 `max_tokens`，并行时可能是 `并发数 × max_tokens`。

**影响**

- “hard-stop on a per-session token budget”是 README 和产品叙事的核心卖点；当前实现更接近“下一轮不再开始”。
- 对高并发 plan，账单可能明显超过用户设定值。对一个强调“用户控制成本”的自托管平台，这是信任问题。

**建议**

1. 在发请求前做预算预留（reservation）：按 `max_tokens` / 估算输入 token 原子扣减，调用结束后按实际 usage 结算差额。
2. 剩余预算不足时，动态降低本次 `max_tokens`，或直接拒绝该调用。
3. 加并发测试：N 个 Worker 同时启动时，总 spend 不得超过 limit；至少要把超调上界写进文档，不能继续叫 hard stop。

---

### P1-3：Shell 没有沙箱，“只读 Verifier”只是 prompt 约束

**证据**

- `Shell` 用 `sh -lc` 在 worktree 里执行任意命令；只有几个字符串黑名单（`sudo `、`rm -rf /`、`curl | sh` 等），没有进程级/文件系统级隔离。
- `AgentRole::Verifier` 的 tool policy 明确允许 `shell`；prompt 里说“read-only”，但 runtime 不阻止写文件、读 worktree 外文件、访问网络或执行 git 操作。
- Shell 的 `current_dir` 是 worktree，但命令可以 `cd ..`，也可以访问用户 HOME、SSH key、Keychain 以外的本地文件。
- `kill_on_drop` 只保证直接子进程被清理，不保证整个进程组。

**影响**

- 架构不变量“Agent 不能越界写”对 shell 不成立；Verifier 可以改文件后再给出 approve，或者把敏感内容带进上下文。
- 在单用户本地环境里，审批是最后一道闸；但审批 UI 目前只展示命令文本，不展示它将触达的路径/进程/网络副作用。

**建议**

- 短期：明确把“Verifier 只读”降级为 prompt-level，文档不要暗示沙箱；对 Verifier 的 shell 单独收紧（只允许测试/只读命令，或需要显式二次审批）。
- 中期：真正的 permission broker + macOS `sandbox-exec`/容器/seatbelt profile；至少把 cwd、可写路径、网络、进程组限制住。
- 这不是 M2 必须当天解决的，但在宣传“agent team 自动合并”之前必须解决或明确标注。

---

### P1-4：`fetch_url` 的 SSRF 防护可被 DNS 解析绕过

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

### P1-5：事件溯源只做了一半，事件不是 source of truth

**证据**

- `update_session_status` 先更新 SQLite，再 append event；`record_usage` 同样先写 projection 再写 event。
- `update_agent_status` 只改 `agents` 表，不产生事件。
- `create_agent`、`append_message`、`set_team_plan` 先更新 projection，事件由调用方另行 append；两者不在同一个事务里。
- 因此 daemon 崩溃、事件 mirror 写失败、或调用方忘记 emit 时，事件日志和投影会分叉。

**影响**

- 当前“事件日志”是审计流 + UI 增量来源，不是可完整 replay 的 event sourcing；`ARCHITECTURE.md` 的不变量 2 与实际实现不一致。
- 崩溃恢复只能把 running session 标成 failed，不能从事件重建状态。
- 以后做 replay/export/审计时会出现“UI 看到的”和“日志记录的”不一致。

**建议**

1. 明确二选一：要么把事件作为唯一写入口（append event + 同事务 projection），要么把文档改为“事件审计日志 + 投影”而不是 event sourcing。
2. 如果选前者：给 `Store` 加 `append_event_with` 事务 API，所有状态变更走同一个入口；加 replay 测试。
3. 短期至少给 `update_agent_status` 补事件，并统一“先事件后投影”的顺序。

---

### P1-6：没有 merge queue / Integrator，串行合并的失败不可恢复

**证据**

- `finish_worker` 在 Verifier approve 后直接 `merge_branch` 到 session worktree；`merge_branch` 冲突时 `merge --abort`，Worker 标记 failed。
- 没有队列、没有 rebase/retry、没有 post-merge integration test、没有 Integrator agent。
- 并行 batch 的 task 都从当前 session branch 创建；后合并的 task 可能基于旧 base，冲突只能失败。

**影响**

- 一个 task 的合并失败会阻塞其 dependents，但没有重新排队/重新验证机制；用户只能手动处理。
- Verifier 审查的是 task branch，不是“合并后的集成结果”；跨 task 的语义冲突不会被发现。
- 这是 M2 之后最应该做的闭环，而不是继续扩大 org。

**建议**

- 先做 deterministic merge queue：queued → rebasing → integration-check → merged/failed，冲突可重试一次并记录原因。
- 再加 Integrator：只做合并、冲突解决、集成测试，不改业务逻辑；其输出仍需 Verifier/人类批准。
- UI 增加 queue 状态和失败原因；CLI 增加 `queue`/`retry`。

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

已有 45 个 Rust 测试 + 9 个 Swift 测试，覆盖 Leader loop、delegation、task worktree、parallel scope、Verifier、TeamPlan、roundtable、预算、恢复。仍缺：

- plan 自动派发的 scope overlap / cap 回归
- 并发预算超调上界
- `fetch_url` 的 DNS/私网/IPv6 SSRF
- merge conflict / queue retry / post-merge integration
- WebSocket 断线重连
- 真实 Anthropic 工具调用（目前只有 mock/SSE 解析测试）
- UI 的 event → plan status / roundtable 渲染
- worktree/branch 清理

### P2-13：产品叙事大于实现

README 开头仍然说 agent “collaborate through typed messages and shared artifacts”，架构图把 MCP、ACP、artifact store、merge queue 画成已有组件；这些大部分是设计目标。对一个“玻璃盒”产品，文档必须区分 **current** / **planned**，否则第一次认真试用就会掉信任。

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

- 用户可以看见 Managers、Workers、Plan board、圆桌，但最后仍然停在 session branch，没有 merge queue、没有 landing、没有 PR。
- 自动派发的并行 task 可能写冲突；预算可能超调；Verifier 不是沙箱。
- 这些会让“更少限制、更高效率”的主张反噬：限制少了，但失败模式更多。

### 4.3 建议的 milestone 顺序

**M2.5 — 把承诺变成约束（1–2 天）**
1. 修 P0-1（plan caps + scope overlap）。
2. 修 P0-2（预算 reservation / 超调上界）。
3. 修 P1-4（SSRF）。
4. 明确 Verifier shell 策略并补测试。
5. 清理过期 UI 文案（本次已做一部分）。

**M3 — Merge Queue + Integrator（主 milestone）**
1. queue 状态机、冲突 rebase/retry、集成检查。
2. Integrator 只读合并角色 + 事件 + UI。
3. 一条真实 fixture 的端到端 demo：Leader plan → 多 Worker → Verifier → queue → 集成测试 → 人类 review → land/PR。
4. 没有这个 demo，不建议继续加 auto-expansion、per-role models、mailbox。

**M3.5 — 事件与恢复**
1. 统一 event/projection 写路径，或把文档改成 audit log。
2. replay/export、worktree 清理、断线重连。

**M4 — 平台化**
1. typed mailbox + artifact store。
2. per-role model / USD/time budget。
3. auto-expansion / capacity controller。
4. MCP/ACP、终端、hunk review、landing/PR。

---

## 5. 本次整理范围

本次只做**文档与明显过期文案**的整理，不改 runtime 行为：

- 新增本审计。
- 新增 `docs/README.md` 文档索引，明确 current / design / historical。
- 更新 README、PROGRESS、PLAN、ARCHITECTURE、DECISIONS、AGENTS 的状态与边界描述。
- 修正 Dashboard / Settings / Studio 中与当前实现冲突的文案和范围数字。
- 不修改 Rust 逻辑；P0/P1 需要单独的代码切片和测试。

---

## 6. 复现与验证

```bash
cargo test --workspace     # 45 passed at audit time
just lint                  # fmt + clippy -D warnings + swiftformat lint
just test                  # Rust + xcodebuild test (9 Swift tests)
```

审计基线 CI：GitHub Actions run `40902407039`（commit `cbef2e6`）成功。
