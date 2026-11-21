# VerlinPark 全量可靠性复审报告

- 日期：2026-11-19
- 基线：`d239dca`（`Fix live provider compatibility and budget preflight`）
- 复审方式：10 个并行只读 subagents，覆盖 Rust runtime、store、daemon、CLI、providers/bench、SwiftUI、安全与发布链路；未修改源码。
- 原始分报告：`docs/reviews/2026-11-19/01-...md` 至 `10-...md`
- 结论：**当前版本不应作为“可直接信任的生产开发环境”发布。** 架构与门控骨架是真实存在的，但仍有多个可复现的 High 级授权、跨会话所有权、状态机、恢复和 provider 完成状态缺陷；先修复 P0，再继续扩大功能面。

## 1. 复审覆盖

| # | 范围 | 报告 |
|---|---|---|
| 01 | vp-core agent runtime / run loop / delegation / merge / budget | `01-core-runtime.md` |
| 02 | context / prompt / cache / memory / sources / retrieval | `02-context-memory-cache.md` |
| 03 | plans / tasks / scheduler / scaling / programs / identities / leases / gates / effects / contracts | `03-orchestration-state.md` |
| 04 | replay / repair / journal / remote / A2A / progress / backups | `04-replay-remote-backup.md` |
| 05 | vp-store SQLite schema / projections / migrations / transactions | `05-store-sqlite.md` |
| 06 | vp-daemon RPC / HTTP / WebSocket / auth / lifecycle / sweepers | `06-daemon-protocol.md` |
| 07 | vp-cli args / RPC / output / destructive actions / errors | `07-cli.md` |
| 08 | vp-providers streaming / retry / usage / cache + vp-bench honesty | `08-providers-bench.md` |
| 09 | macOS SwiftUI app concurrency / state / secrets / UX reliability | `09-swift-app.md` |
| 10 | cross-cutting security, CI, release, tests, docs claims | `10-security-tests-ci.md` |

共计 **109** 条记录级 finding：约 **29 条 High-impact**、**42 条 Medium**、**38 条 Low/Low-Medium**。没有发现“Critical/可直接远程执行”的单点漏洞，但多个 High 问题组合后会影响授权、数据隔离和恢复正确性。

## 2. Top blockers（必须先修）

### P0-1. 并行只读 tool batch 绕过项目审批模板和 intent 审计

**来源：** 01 R1 / 10 S1  
**证据：** `crates/vp-core/src/agent/run_loop/body.rs:1620-1682`  
**问题：** 一个 assistant turn 有 2+ 个 parallel-safe 只读调用时，代码使用 stateless `permissions::decide()`，没有调用 `Engine::evaluate_tool_permission`/`record_intent_decision`。项目 deny template（例如 `read_file` over `secrets/**`）对单个调用生效，对并行 batch 不生效。

**影响：** “deny always wins” 被绕过；`approval.template.matched` 和 C1 `intent.decision` 审计缺失。  
**修复：** 并行预检复用 sequential authorization/approval 路径；任何 Ask/Deny/contract escalation 回退到 sequential。  
**测试：** 两个 `read_file` 的并行 batch + path-scoped deny template，断言两个都 denied 且审计事件存在。

### P0-2. 跨会话 artifact/source/MCP task ID 没有所有权检查

**来源：** 10 S2  
**证据：** `agent/tools/integrations.rs`、`agent/tools/artifacts.rs`、`archive_media.rs`、`mcp/tasks.rs`、`engine/sources/retrieval.rs`  
**问题：** `read_artifact`、`publish_artifact`、`read_source`、`archive_list/extract`、`media_info/transcode`、MCP task get/result/cancel 只按全局 ID 查找，不校验当前 session/project ownership。

**影响：** 已知 ID 即可跨 session 读/改 artifact、向受害者 session 追加事件、跨项目读取 source、取消别人的 MCP task。UUID 不可猜不是授权。  
**修复：** store/engine 层增加 session/project scoped lookup；工具调用传递当前 session/project；外部 ID 一律按 `NOT_FOUND`/`PERMISSION_DENIED` 失败。  
**测试：** 两个 session/两个 project 的跨边界读写、MCP task 操作、archive/media 派生 artifact；断言无受害者事件。

### P0-3. Durable journal 的 completed-write 重放会消耗证据并可二次执行

**来源：** 01 R2  
**证据：** `agent/run_loop/tool_calls.rs:89-106`、`agent/dispatch.rs:524-655`、`store/runs.rs:951-1003`  
**问题：** `check_duplicate_write` 在检查时消费 `run_tool_calls.consumed_by_run_id`，但 replay 分支不写 successor completed row。若 replay 后再次失败，下一次 resume 找不到未消费记录，会对 `edit_file`/`apply_patch`/`file_manage` 再次执行。

**影响：** 重复副作用，直接破坏 journal 的“consume-once”承诺。  
**修复：** 消费与 replay 完成/事件写入同一事务；失败时保留可重放证据；用显式 replay row 记录 successor。  
**测试：** 两次连续 resume + post-replay failure，断言文件内容不变且第二次 resume 不执行 write。

### P0-4. Plan batch abort 会丢弃已完成 sibling 的结果

**来源：** 03 F1  
**证据：** `agent/plan/execute.rs:534-573`、`store/plans/transitions.rs:133-150`  
**问题：** join 后一旦某个 result 设置 abort，后续 sibling 不再检查真实 result，直接写成 `cancelled/blocked`；`finish_plan_task` 无当前状态谓词。

**影响：** 已 Verifier/已 merge 的 sibling 被覆盖，merge queue 与 task projection 不一致，后续可能重跑并生成重复 commit。  
**修复：** 先处理所有实际 result，只对确实没有 result 的 task 合成 abort 状态；store 状态迁移加 terminal guard。  
**测试：** 两 task batch，T1 gate defer/abort，T2 completed/merged；断言 T2 保留 completed/merged。

### P0-5. Plan re-submission 重置任务 projection，skip 依赖活锁

**来源：** 03 F2-F4  
**证据：** `agent/run_loop/tool_calls.rs:433-462`、`engine/plans/revisions.rs:209-236`、`scheduler.rs:113-128,186-195`、`orchestration/controller.rs:120-148,259-284`  
**问题：** 第二次 `submit_team_plan` 直接 `set_team_plan`，产生新 `team_plan.submitted` seq，`sync_plan_tasks` 只回放新 seq 之后的事件，completed/attempts 丢失；控制器把 `skipped` 当作满足依赖，scheduler 只接受 `completed`，导致下游 `blocked → pending → blocked` 活锁。

**影响：** 已完成工作丢失、预算重复花费；skip 后的 DAG 可能永远不运行。  
**修复：** re-submit 必须走 revision path + `ensure_revision_safe`；scheduler 与 controller 统一 `satisfied` 定义；加 revision/skip 一致性测试。

### P0-6. Plan revision replay/repair 可能把 plan 写坏，gate deferral journal 不终态

**来源：** 04 F1-F3  
**证据：** `engine/runs/replay.rs:689-710`、`engine/runs/repair.rs:31-47`、`engine/runs/support.rs:113-134`、`run_journal.rs:588-598`、`store/runs.rs:426-432`  
**问题：** `team_plan.revised` wrapper 被当作 plan 比较/修复；task DAG 只从最后一个 `team_plan.submitted` 重建；`deferred` 不在允许的 run 终态中，gate wait 会留下 `running` journal row。

**影响：** 修复后 `get_team_plan` 可能返回 None；revision-added tasks 被 drift repair 删除；重启后 deferred run 被错误恢复。  
**修复：** 明确 revised payload schema；从 submitted+revised 合并重建 DAG；`deferred` 成为终态或显式 deferred status；补 replay/repair 测试。

### P0-7. Store manifest 截断与 session create 非原子

**来源：** 05 S1/S2  
**证据：** `store/events.rs` manifest query（oldest 100k）、`engine/sessions.rs` / `store/sessions.rs`  
**问题：** `projects.jsonl` 读取受 oldest 100k 限制，`db-rebuild` 把截断结果当权威；session row 与 `session.created` 分两个事务提交，崩溃会留下 rebuild 跳过的 session 和 orphan worktree。

**影响：** 大项目 rebuild 静默丢最新事件；崩溃边界产生不可重建状态。  
**修复：** manifest 读取按完整/有界分页或按最新窗口并显式拒绝截断；session create + event 同事务或补偿回滚。  
**测试：** >100k 事件 manifest、session create 注入失败、db-rebuild 一致性。

### P0-8. Provider finish_reason/stop_reason 被丢弃；空 turn/截断 tool JSON 被当成功

**来源：** 08 PR-1  
**证据：** `vp-providers/src/openai.rs:219-260`、`anthropic.rs:462-496`、`body.rs:1189-1205,1524-1548,1642`  
**问题：** 适配器不报告 `length/max_tokens/content_filter/refusal`；core 对空文本/空 tool call 返回 `Final("")`；tool arguments JSON 解析失败静默变成 `{}`。

**影响：** 截断内容被持久化为成功回答；截断 tool call 可能以空参数执行；reasoning-only turn 无答案但成功。  
**修复：** 增加 provider-neutral finish reason/error；core 对非正常完成 fail closed；tool arguments 非法 JSON 拒绝执行。  
**测试：** OpenAI length、Anthropic max_tokens、reasoning-only、truncated tool JSON。

### P0-9. CLI 部分 policy 更新会静默重置/启用安全设置

**来源：** 07 F1/F2/F8/F9  
**证据：** `cli.rs:1626-1639`、`dispatch/runtime.rs:171-190,399-419`、`dispatch/approval.rs:137-176`、`dispatch/memory.rs:377-383,500-512`  
**问题：** `shell-policy-set` 用新 policy 替换整个对象；approval-template update 用 `enabled=!disabled`；preemption/memory partial update 重启用默认值。

**影响：** 只想改一个字段，可能打开 network、清空 read_deny、重新启用已禁用 auto-approval、重置 retention/limits。  
**修复：** 所有 policy update 先读当前值，只覆盖显式参数；`--disabled` 在 create/update 都生效；加端到端 CLI 参数测试。

### P0-10. Swift reconnect/stream/key/RPC 生命周期不可靠

**来源：** 09 S1-S4/S7  
**证据：** `CoreClient.swift:51-107,4627-4641`、`AppModel.swift:950-1012,5131-5169,9885-9986`、`SystemNotifier.swift`  
**问题：** reconnect 不 backfill approvals/questions；旧 AsyncStream termination 可清掉新 continuation；所有 RPC 30s timeout、无 cancellation/operation id；移除 Keychain key 不撤销 daemon key，缺失 key 会用空 key 重配；通知 Approve 对所有风险无差别可点。

**影响：** 连接显示正常但事件停止；长时间 land/PR 被误报失败并可能重复；密钥撤销/重连行为违反用户预期；Destructive/HostState 审批可盲批。  
**修复：** 订阅 generation、事件回填、分操作 timeout/cancel/operation id、provider credential clear RPC、通知按风险限制并展示详情。

## 3. 其他必须纳入修复计划的问题

### 3.1 Runtime / state machine

- R3/R4：batch setup 失败泄漏 claims/agents/worktrees/leases；merge queue 非 git error 可留 `merging/checking`。
- R5/R6/R7：ask_user 覆盖 paused；Verdict 非穷尽；no-change Worker 用 summary 子串判定 completed。
- R8/R9：warm_on_resume 不在 journal；journal/status 写失败只 warn。
- 03 F5：Verifier 后、Integrator 前的 cancel 仍可能 merge。
- 03 F6/F7：revision check-then-write race；expired gate 仍可 resolve。
- 03 F8/F9/F10：startup 清理 foreign write/GUI leases；task worktree 无 retention；并行 program milestone 各自拿满预算。

### 3.2 Store / persistence

- 05 S3：orchestration policy 损坏时 fail open（默认启用 retry/escalate）。
- 05 S4：instruction-state `LIKE '<id>#%'` 未转义 `%/_`，可跨 session 删除。
- 05 S5：merge/plan terminal 状态无 guard。
- 05 S6：recover 清理所有 write/computer leases，破坏 two-daemon 语义。
- 05 S7/S8/S9/S10：u64 seq 溢出、effect unknown 子串逃生、DEFERRED 事务、memory LIKE 未转义。

### 3.3 Daemon / protocol

- D1：GUI grace 45s 小于 browser 75s/computer 60s，可能在 action 中途释放 lease。
- D2：WS lag 无 resync/gap 通知。
- D3：context policy/prompt layout 未刷新 manifest。
- D4/D5/D6/D7/D8/D9/D10：pre-auth 64 MiB WS frame、params null 非法、错误码折叠、HTTP 事件订阅假成功、sweeper 被单个 env 关闭、scheduler 先于 bind 启动、session.create 吞错。

### 3.4 Context / memory / cache

- C1：`context.cache.message_prefix_hash` 被 prompt-cache hash 覆盖，resumed first call 产生假 `unknown` break。
- C2：manual compaction/checkpoints 使用 legacy 拼接 prompt 和 synthetic prefix hash。
- C3：same-session warm 被计为 cross-session reuse。
- C4：identity memory brief 先取 128 条最新再排序，可能饿死 pinned/department/project memory。
- C5：agent/memory tail 未计入 context/budget。
- C6：goal/prompt 无界，超 64 KiB session/context 会永久 fail closed。

### 3.5 Provider / benchmark

- PR-2：live cache bench 空回复伪造 `"OK"`。
- PR-3/4：prefix-break negative control 可假通过；`--require-target`/`verify` 不重新校验 shape/provider-reported。
- PR-5：Anthropic 1h TTL 缺 beta header，legacy system path 硬编码 5m marker。
- PR-6/7：Anthropic message_start usage 丢失/重复；parallel tool results 变成多条 user message，不是一条多 block 消息。
- PR-8..21：provider/usage 标签、pricing provenance、merge wait、preemption override、validate 字段、dead timeout、错误事件重复、system/tool 排序、CLI huge allocation 等。

### 3.6 Security / release

- S3：shell read-denial 未覆盖 tool-outputs/processes/verify/sandbox/archive/media/worktrees。
- S4：tool input/output 中的 secrets 进入事件/approval/search/export/backup；注释声称不记录。
- S5：notification approve 无风险 gate。
- S6-S9：declared shell effect probe 可用本地证据结算 external effect；ACP literal headers/cleartext remote；无效 approval mode 静默 smart。
- S10：release workflow 先发布后测试；CLI 未签名；actions 未 pin SHA；无 dependency audit。

## 4. 测试与可靠性缺口

- daemon 没有 HTTP/WS/auth/discovery/instance-lock 测试。
- 没有 cross-session artifact/source/MCP ownership 测试。
- 没有 parallel deny-template 或 parallel intent audit 测试。
- 没有 journal 连续两次 resume、partial batch setup、integrator status-write failure 的故障注入。
- 没有 replay/repair 的 revised-plan/gate-deferred/progress mismatch 覆盖。
- Swift 没有 CoreClient reconnect、AsyncStream termination、Keychain failure、RPC cancellation 测试。
- benchmark 没有 negative-control/verify 篡改测试。
- shell secret-root 测试使用空 roots，未验证真实 denylist。
- 没有 release 签名/公证/unsiged CLI 检查。

## 5. 建议修复顺序

### P0（先修，阻塞任何“可用”声明）

1. 并行只读 batch 复用 sequential authorization/approval（R1/S1）。
2. artifact/source/MCP 对象所有权（S2）。
3. journal duplicate-write replay（R2）。
4. plan batch abort / re-submit / skip / cancel state machine（03 F1-F5）。
5. replay/repair revised-plan + deferred journal（04 F1-F3）。
6. store manifest/session atomicity（05 S1/S2）。
7. provider finish reason/truncated tool JSON（08 PR-1）。
8. CLI policy partial updates（07 F1/F2/F8/F9）。
9. Swift reconnect/stream/key/timeout（09 S1-S4）。

### P1（可用性/安全加固）

- GUI lease grace（06 D1）。
- cross-session shell read denylist（10 S3）。
- notification approval risk gate（10 S5）。
- context cache hash attribution/manual compaction/memory brief（02 C1-C4）。
- benchmark negative controls/verify（08 PR-2/3/4）。
- daemon WS resync/manifest refresh/pre-auth bounds/params null（06 D2-D5）。
- merge queue terminal error cleanup, batch setup rollback, ask/pause, no-change completion（01 R3-R7）。

### P2（后续 hardening）

- 05 S3-S10, 04 F4-F10, 07 F3-F10, 08 PR-5..21, 10 S6-S10, and lower-severity items in each report.

## 6. Fix workstreams（并行边界）

建议修复时按文件/依赖边界拆成 6 个 workstream，避免多人同时改同一文件：

1. **Core authorization/runtime**：`agent/run_loop/body.rs`, `tool_calls.rs`, `dispatch.rs`, `run_journal.rs`, `store/runs.rs`（P0-1/P0-3）。
2. **Plan/replay state machine**：`agent/plan/`, `engine/plans/`, `scheduler.rs`, `engine/runs/`, `store/plans/`（P0-4/5/6）。
3. **Store integrity**：`store/events.rs`, `store/sessions.rs`, `store/cache.rs`, `store/memory/`, `store/backup/`（P0-7/3.2）。
4. **Daemon + CLI policy/protocol**：`vp-daemon/`, `vp-cli/`（P0-9/3.3）。
5. **Providers + bench**：`vp-providers/`, `vp-bench/`（P0-8/3.5）。
6. **Swift app**：`apps/macos/`（P0-10/3.4 UI parts）。

安全 cross-cutting 项目（S2/S3/S5）在 1/3/6 完成后单独加固，避免与基础修复冲突。

## 7. 复审原始报告索引

- [01 core runtime](01-core-runtime.md)
- [02 context/memory/cache](02-context-memory-cache.md)
- [03 orchestration/state](03-orchestration-state.md)
- [04 replay/remote/backup](04-replay-remote-backup.md)
- [05 store/SQLite](05-store-sqlite.md)
- [06 daemon/protocol](06-daemon-protocol.md)
- [07 CLI](07-cli.md)
- [08 providers/bench](08-providers-bench.md)
- [09 Swift/macOS](09-swift-app.md)
- [10 security/tests/CI](10-security-tests-ci.md)

## 8. 修复状态（2026-11-19 hardening round）

修复基于同一工作树、按文件/依赖边界拆成多个并行 workstream；没有 merge/push。
主要决策记录在 `docs/adr/0230-reliability-hardening-round.md`。

### 8.1 P0 状态

| P0 | 状态 | 修复摘要 |
|---|---|---|
| P0-1 并行只读审批绕过 | 已修 | 并行 fast path 先做非消费 preview；只有全部 `Allow` 才并行。执行前逐调用记录真实 contract `intent.decision` 和持久 broker 决策；任何 Ask/Deny/策略变化 fail closed。回归测试覆盖两个 `read_file` + path-scoped deny template。 |
| P0-2 跨会话 artifact/source/MCP task | 已修 | artifact/source/MCP task 解析绑定调用 session/project；外部 id 统一 `NOT_FOUND`/拒绝；archive/media 派生 artifact 不再向受害 session 写事件。shell 默认 read-deny 增加 tool-outputs/processes/verify/sandbox/archive/media/sibling worktrees/tmp/backup-keys。 |
| P0-3 journal 重放二次写 | 已修 | `check_duplicate_write` 改为非消费查找；重放作为当前 attempt 的独立 completed row 记录。连续两次 resume + replay 后失败不会再次执行写。 |
| P0-4 batch abort 覆盖已完成 sibling | 已修 | join 后先处理每个 sibling 的真实结果；仅对无结果任务合成 abort 状态。`finish_plan_task` 增加终态 guard，`plan_task.finished` 只在 store 真正应用时发出。 |
| P0-5 plan 重提交重置 projection | 部分修复 | Leader `submit_team_plan` 改为 create-only；第二次提交返回 `PLAN_REVISION_REQUIRED`，不产生新 `team_plan.submitted`，completed/attempts 保留。revision 写入使用 store 内 `BEGIN IMMEDIATE` 的 expected-revision CAS。Leader 工具面仍没有通用 `team_plan.revise`，需要 operator RPC/CLI 显式 revision。 |
| P0-6 revised plan replay/gate deferral | 已修 | replay/repair 解开 `team_plan.revised` wrapper；DAG 从 submitted + revisions 重建；merge queue replay 只折叠真实 mutation 事件；progress drift 纳入 consistency；`deferred` 成为终态 run 状态；bundle 增加 contracts/contract usage/progress/remote-test evidence。 |
| P0-7 manifest 截断/session 原子性 | 已修 | manifest v2 要求完整 replay stream，显式 truncation/ malformed/ legacy capped 均 fail closed；写入 temp + `sync_all` + atomic rename。session 行与 `session.created` 同事务；fork lineage 同样原子。 |
| P0-8 provider finish reason/截断 | 已修 | OpenAI `length/content_filter/refusal/unknown` 和 Anthropic `max_tokens/refusal/unknown` fail closed，不发出 `Done`，不执行截断 tool call；保留已计费 usage。1h TTL beta header、usage 去重、parallel tool results 单 message 已修。 |
| P0-9 CLI policy 部分更新 | 已修 | shell/preemption/memory policy 读取当前值后只 patch 显式字段；approval template enabled 保持；renderer 不再串台；console/terminal 退出码/JSON/清理/deadline 修复；`--file -` stdin、RPC timeout、typed error、destructive 确认已加。 |
| P0-10 Swift reconnect/stream/key/timeout | 已修 | 事件缓冲 + continuation generation + receive-loop connection epoch；`event.gap` 处理；per-method timeout/cancel/operation id；移除 Keychain key 同时通过 `agent.configure` 清 daemon key；`SESSION_START_FAILED` + session_created 作为部分成功；notification 只允许 routine workspace write 内联审批。 |

### 8.2 同轮修复的 P1/P2

- GUI lease：`GuiCoordinator` 跟踪 in-flight ticket；动作执行中 sweep/其他 session 不能按 grace 回收；移除 daemon 105s 硬编码 grace floor；用户 grace 0–600 原样保存。
- Context/cache：manual compaction/checkpoint 使用真实 layout-aware prefix；warm 跨 session 归属修正；identity memory preview 按 scope 保留配额；goal 32 KiB / prompt 64 KiB 上限；source 导入精确 URI 去重；memory LIKE 转义；instruction blob 不重复写。
- Tool surface：advertised schema 与 role policy 对齐；Manager/Verifier 不再看到 `archive_extract`/`media_transcode`，Scout 重新获得只读的 `archive_list`/`media_info`；Leader policy surface 不再包含 `delegate_many`/`submit_verdict`，Worker policy surface 不再包含 `delegate_acp`，Verifier policy surface 不再包含 `ask_user`/`coordinate`/`remember`。
- Store：orchestration policy 损坏 fail closed；instruction prefix 精确大小写；merge/plan 终态 guard；foreign live lease 保留；bundle seq/payload 边界；effect sweep 结构化标记；memory LIKE 转义。
- Bench：空 live turn 不再伪造 `"OK"`；prefix-break 负控制真实注入；`--require-target`/verify 重算 acceptance 和 provider-reported provenance；pricing/scenario 比较拒绝不匹配。
- Security：shell read roots 扩展；跨会话对象所有权；notification 风险门控；invalid approval mode 仍为既有 gap（未在本轮改变默认 smart 行为）。

### 8.3 验证（最终冻结树）

- `cargo fmt --all -- --check`：通过。
- `cargo check --workspace --all-targets`：通过。
- `cargo clippy --workspace --all-targets -- -D warnings`：通过。
- `cargo test -p vp-core --lib`：**2,480 passed / 0 failed / 2 ignored**。
- `cargo test -p vp-store --lib`：**418 passed / 0 failed**。
- `cargo test -p vp-cli`：**162 unit + 10 backup integration + 1 governance passed**。
- `cargo test -p vp-daemon`：**42 unit + 1 governance passed**。
- `cargo test -p vp-providers`：**159 lib + 27 append_only passed**，3 个 live/network 测试 ignored。
- `cargo test -p vp-bench --lib`：**146 passed**；`--test harness`：**11 passed / 1 ignored**。
- `vp-protocol` / `vp-remote` / `vp-a2a` / `vp-remote-agent` 测试全部通过。
- `xcodebuild ... test`：**430 app tests + 132 helper tests passed**。
- Live DeepSeek（`deepseek-flash`，`https://api.deepseek.com`，key 仅通过环境变量传入，未写入仓库/日志/事件）最终树验证：
  - provider stream：53 chunks，65 input / 281 output，正常 `DONE`。
  - small live cache-bench：95.5471% hit ratio，`provider_reported`，但 prefix ~5.7k，明确标为 `unrealistic`，不冒充 200k long-prefix acceptance。
  - full Leader → `write_file` E2E：4 calls，48,072 input / 318 output / 46,976 cache-read，**97.7201% cache hit**，session 正常 `idle`，文件内容精确，`tail_delta` 为最后 miss reason。
- 额外定向回归：parallel deny template、invalid tool args、empty provider turn、两次 resume、journal replay、plan resubmission/revision CAS、batch abort sibling、skip dependency、gate expiry、merge boundary cancel、deferred run、replay/repair revised plan、archive round-trip、provider finish reasons、bench tamper/negative controls 全部通过。
- Tool schema/policy 对齐回归：Manager/Verifier 不 advertised `archive_extract`/`media_transcode`；Scout 恢复只读 `archive_list`/`media_info`；Leader/Worker/Verifier policy surface 的 role-specific 漂移已清除。
- 宿主机曾出现 `syspolicyd` 代码签名队列导致新二进制启动延迟；以上为最终冻结树上实际执行的完整结果。

### 8.4 明确剩余 gap

- Leader 工具面没有通用 `team_plan.revise`；第二次 submit 会被拒绝，需 operator RPC/CLI revision 或 `plan_fanout`。
- `fanout_plan` 的 expected revision 仍来自 bounded recent-event window；store CAS 会 fail closed，但 typed `FANOUT_CONFLICT` 映射未完成。
- `finish_plan_task` 的终态保护仍是单一 SQL guard，不是完整的 expected-status API；merge queue 的 `pre_merge_commit` 不作为 replay 权威字段。
- task worktree retention 只做了 bounded/opt-in sweep，不是完整 retention/GC 子系统。
- GUI in-flight marker 是单 daemon、in-memory、单 ticket；same-session reentry 会覆盖旧 ticket，放弃的 ticket 由 lease TTL 120s 兜底。
- provider abnormal finish 使用 `ProviderError::Stream`；零输出异常仍可能消耗现有 bounded stream retry 预算。
- Swift 没有全局 event seq；跨 session 只能做 projection reload，per-session tail backfill 有界。
- 未处理本轮低优先级项：daemon HTTP/WS 集成测试客户端、全量 secret redaction、release 签名/公证、更多 archive 格式、cross-host scheduling。
- 宿主机 `syspolicyd`/代码签名队列导致部分测试只能编译验证；最终提交会记录实际执行的测试集合。

## 9. 结论

这次复审不是“风格挑刺”：多个问题是可复现的正确性缺陷，尤其是并行审批绕过、跨会话对象所有权、journal 重放、plan batch/revision、replay/repair、provider truncation、CLI policy reset 和 Swift reconnect。修复轮已关闭主要 P0 路径，并把剩余风险收缩到明确的 operator-revision、fanout CAS、task-worktree retention、GUI 单 ticket 和测试环境限制。仍不应把项目描述为“所有边界都已生产级证明”；但上述 P0 不再是可以静默绕过或静默丢数据的路径。
