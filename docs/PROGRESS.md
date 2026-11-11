# VerlinPark 进度

> 最后更新：2026-11-11
> 当前阶段：M2 四层 runtime + M2.5 硬化 + Merge Queue/Integrator/Retry/Conflict Resolve + Opt-in Auto-Resolve + Global Session Queue/Scope Locks + Global Worker/Labour Capacity + Per-role Models/USD/Time Budgets + Per-call Usage Ledger + Task/Department Budgets + Mailbox Retention + Content-Addressed Artifacts + Typed Mailbox/Binary Artifacts/GC + Landing/PR + Session 协作式 Pause/Resume + Shell 沙箱/Policy + Black Crystal Glass UI + EN/CN-S localization + Workspace Shell 硬化 + 多智能体编排控制器/重试策略/持久协调投影/计划修订/会话租约 + 默认开启的 standalone task inbox/autopilot + 懒加载文件树/内容搜索 + 项目知识源/引用 + 项目 skill packs + artifact versions/idempotent publication + deterministic agent tooling (atomic patch/project task/git/code intelligence/process/local HTTP) + optional LSP/web search/PDF/image/office-document/data readers/runtime introspection + sandboxed WKWebView browser/ComputerLease + cache-aware long-task telemetry/stable prompt prefix + operator steering/task progress ledger + policy-gated stall recovery/deferred continuations + evidence-backed project memory v2/long-task operator console + memory conflict review/health/verifier capture + daemon Prometheus metrics/recurring continuations/Gitea-Forgejo PR/rich artifact preview/backup-restore + efficiency playbook E1-E4/cache telemetry/read-only Scout/idempotent operations + durable run journal/crash resume/remote SSH workspaces/project programs/native macOS computer control + tool-output paging/compaction budget/cross-process lease fencing/hybrid memory/cost-per-solved-task bench 已跑通
> 审计与优先级：[AUDIT.md](AUDIT.md)<br>
> Agent 工具路线：[TOOLING_ROADMAP.md](TOOLING_ROADMAP.md)<br>
> 文档索引：[README.md](README.md)

---

## 2026-11-11 Long-task round 2: context economy, fencing, hybrid memory, benchmark

这一轮处理长任务的上下文成本、跨进程所有权、可选语义记忆和可度量基准。四条线
全部保持 deterministic Rust、事件先行、不绕过 approval / Seatbelt / path jail /
write scope / Verifier / merge / capacity / budget gate。计划与验收见
[CONTEXT_OWNERSHIP_MEMORY_BENCH_PLAN.md](CONTEXT_OWNERSHIP_MEMORY_BENCH_PLAN.md)，
决策见 ADR-0159–0163。

### C1 tool-output paging + compaction budget

- 新 `ToolOutputPolicy`（enabled、spill_threshold_bytes 16 KiB、preview_bytes
  4 KiB、max_total_bytes_per_turn 64 KiB、retention 7 天、max_entries 2000、
  可选 per-tool 阈值）；RPC `tool_output.policy.get/set`，CLI
  `vp tool-output-policy*`。
- 完整 tool output 写入 daemon data dir 的 `tool-outputs/<sha256>.txt`（原子
  写、0600、拒绝 symlink/non-regular），SQLite 只存 metadata；同内容共享 blob。
- 单个结果超过阈值或单 turn 聚合超预算时，确定性 spill：先强制 per-tool，再按
  largest-first/原始顺序；provider-facing 文本带
  `[output truncated: ... use read_tool_output handle=...]`，UTF-8 安全。
- 新只读工具 `read_tool_output`：`offset`/`limit`/`max_bytes`/`from_end`/
  字面量 `pattern`；handle 只对当前 session 有效；返回值带行号和 continuation
  hint。Leader/Manager/Verifier/Scout/Worker/Labour 可用。
- 事件 `tool.output_spilled` / `tool.output_truncated`；RPC
  `tool_output.status/list/prune/audit`，CLI `vp tool-outputs` /
  `tool-output-status` / `tool-output-prune` / `tool-output-audit`；prune
  保护未过期 active session 行，audit 检查缺失/大小/hash/孤儿 blob。
- compaction：新增 `compact_keep_tokens`（默认 15k）与
  `compact_prepare_ratio`（默认 0.6）；tail 同时满足 keep messages/tokens，
  只从 user/assistant 边界切。summary 带 deterministic
  `ContextStructuredState`（goal、plan revision、active task/acceptance/write
  scope、recent completed tasks），rolling trim fallback 仍保留。
- prepared summary 在阈值前生成，按 provider/model/prefix hash/plan
  revision/cut cursor 校验复用；失效则 `fallback_to_sync`。swap 仍在阈值处
  同步执行，不创建第二个 run，不绕过 budget；prepared summary 只存在于当前
  run 内存，不跨 restart/fork。
- `session.context` 与 `context.compaction` 增加 compaction count/latency/
  tokens before/after/background reuse/fallback；manage dash 与 Stage
  context 显示。tool output 是 operational state，不进 replay/bundle。

### C2 cross-process lease fencing / continuation ownership

- daemon 持久化 instance identity（`store_meta`），lease owner 由
  instance/pid/nonce 组成；`runtime_leases` 增加 owner_instance、lease_token、
  fencing_token、renewed_at、released_at；`runtime_lease_fences` 保存跨
  release/expiry 的 monotonic generation。
- acquire/renew/release/assert 使用 `BEGIN IMMEDIATE` 与条件更新；stale owner
  得到 typed `RuntimeLeaseError::{Busy,Lost,Internal}`，token 不进入 RPC/CLI/
  events/log。
- `RuntimeLeaseGuard` 携带 token/generation，heartbeat 失败会标记 lost；
  Integrator 在 `git merge` 前、后和 finalize 前 assert fence；stale 在触碰
  分支前中止，post-merge 丢失回滚到 pre-merge HEAD。
- `session_continuations` 增加 lease_owner/lease_token/lease_expires_at/
  delivery_attempt；claim 原子化，delivered/retry/failed ack 必须 owner+token+
  unexpired；过期 claim 可 reclaim，stale owner 不能写终态。reconciler 用
  `continuation-delivery:<id>` receipt 防止重复启动。
- RPC `runtime.leases.history`、`session.continuation.status/reclaim`；CLI
  `vp lease-history` / `continuation-status` / `continuation-reclaim`。限制：
  同一 SQLite data dir 的进程协调，不是跨主机共识；外部副作用不是 exactly-once。

### C3 optional hybrid memory

- `MemoryPolicy.embedding`（显式 opt-in）：endpoint/model/dimensions(1536)/
  batch_size/timeout/candidates/min_similarity/api_key_env；endpoint 校验
  scheme/host/no-userinfo/no-query/fragment，key 只存环境变量名。
- 派生表 `memory_embeddings` / `memory_embedding_jobs` /
  `memory_embedding_usage`；memory 写入/导入只入队，不因 embedding 失败而失败；
  reconciler 有 batch、5 次 attempt、指数 backoff、4xx 非重试、partial batch。
- 向量 `VPEM` 小端 f32 编码，校验维度/有限值/zero-norm；不通过任何 RPC/tool/UI
  返回原始向量。内容 hash 变化、model/dimension 变化会重建覆盖。
- `recall` 在有配置且健康时做 lexical FTS + cosine candidates，deterministic
  RRF 后叠加既有 task/scope/recency/status/pinned 权重；失败/禁用回退 lexical
  并报告 `semantic_unavailable`。`MemoryMatch` 增加 lexical_score/
  semantic_score/source 可选字段。
- RPC `memory.embeddings.status/rebuild/test`；CLI `memory-embeddings*`；
  Manage Dash memory health 显示 model/coverage/last error，可 probe/rebuild。
- 限制：SQLite 全量 vector scan，无 ANN/分布式索引；memory body/tags 会发送到
  显式配置的 endpoint；embedding 用量单独记账，不自动计入 session/task budget。

### C4 cost-per-solved-task benchmark

- 新 crate `vp-bench` + `vp-bench` binary + `vp bench` CLI：
  `list` / `run` / `report` / `compare`。
- 9 个 deterministic scenarios：baseline、plan/Worker、Scout、paging off/on、
  compaction off/on、worktree parallelism 1/4；有 deterministic acceptance，
  失败/Verifier 拒绝/merge 失败都算 unsolved，不伪造 solved。
- 指标：solved、acceptance、Verifier pass、merge、wall clock、input/output/
  cache tokens、USD cost、tool output bytes/spills、compactions、retries、
  human interventions；派生 cost_per_solved_task / tokens_per_solved_task。
- 报告 JSON/Markdown/CSV，包含 commit/host/provider/pricing/caveat；8 MiB 上限、
  原子写；`compare` 拒绝 fixture/acceptance/model/pricing/schema 不匹配，除非
  `--allow-mismatch`。真实 provider 需要三重显式开关，默认 fake provider。
- 限制：fake usage 是 request-size 模拟，不评价模型质量；单机顺序重复；不是
  universal ranking；不绕过任何 Verifier/merge/approval/sandbox gate。

### 验证与集成

- 合并顺序：C4 `b0adba1` → C2 `26256ed` → C3 `93a991a` → C1 `bef841a`；
  计划 `beaefae`。
- `git diff --stat a5f57c8..HEAD`：**38,436 insertions / 2,153 deletions**。
- Rust：**1,330 passed，6 ignored**；Swift：**195 app + 116 computer-helper**。
- `just lint`、`xcodebuild test`、`just bundle` 通过；bundle 包含 verlinpadd、
  browser helper、computer helper、remote agent。
- C1 的 paging 指标在 C4 的 bench 报告中现在可记录；C4 的 paging-on scenario
  不再标记 unavailable。
- 仍然不要声称：跨主机 consensus、provider-call exactly-once、完整无损
  compaction、ANN/分布式向量索引、embedding 自动计入 session 预算、单次 benchmark
  的普适排名。

---

## 2026-11-11 Long-task runtime R1-R4: durable journal, remote, programs, native computer

这一轮把长任务 runtime 的四个结构缺口做成可操作、可恢复、可审计的模块；全部
保持 deterministic Rust 调度、事件先行、不绕过 approval / Seatbelt /
write scope / Verifier / merge queue / capacity / budget gate。计划与验收记录
见 [LONG_TASK_RUNTIME_PLAN.md](LONG_TASK_RUNTIME_PLAN.md)，决策见 ADR-0155–0158。

### R1 durable run journal / crash resume

- 新表：`session_runs`、`run_steps`、`run_tool_calls`、`run_checkpoints`；
  项目级 `SessionRunPolicy { auto_resume(default false), max_attempts(1..=3),
  max_age_hours(1..=168) }`。
- 事件：`session.run.started/checkpointed/step_intent/step_completed/
  tool_intent/tool_completed/interrupted/resumed/failed/completed/
  cancel_requested`；run 状态/attempt/lineage/budget/worktree/plan revision
  由事件重建，tool outcome 明确是 operational audit state。
- provider 请求前写 checkpoint 与 `model_request` intent；工具执行前写
  `tool_intent`（tool + canonical fingerprint），完成后写 `tool_completed`。
  崩溃窗口按“已完成 / 未提交 / 未知”分类，不假装 exactly-once。
- daemon 启动把 running run 标 `interrupted`，仅在项目 policy opt-in 时
  auto-resume；manual `session.run.resume` 始终存在。
- resume 前置条件：session idle/failed/cancelled、run interrupted/failed、
  attempt/age 未超、provider/model 匹配、budget 未耗尽、plan revision 未变、
  worktree HEAD 等于记录值或是其干净的 descendant；不 reset 用户改动。
- resume 从事件重建消息，注入 bounded journal note；已完成的
  `write_file`/`edit_file`/`apply_patch`/`file_manage` 按 name + fingerprint
  硬阻断并返回 journaled result（consume-once，失败/中断释放 claim）；
  read-only 工具可重跑；shell/process/local_http/browser 的未知结果如实
  标 unknown 并提示先验证。
- RPC：`session.runs`、`session.run.detail/resume/cancel`、
  `session.run.policy.get/set`；CLI `vp runs` / `run-*`。
- 边界：没有 provider-call exactly-once、没有 mid-stream resume；task-scoped
  Worker run 走 plan-task retry；imported/rehydrated archive 不可 resume；
  wall-clock budget 只按 attempt 检查，token/USD 才做 preflight。

### R2 remote workspaces over SSH/stdio

- 新 crate `vp-remote`：bounded NDJSON JSON-RPC 2.0，`StdioTransport` /
  `SshTransport`，显式 argv（无本地 shell），握手校验 protocol version、
  root、auth token；超时、取消、frame 上限、进程组清理。
- 新 helper `vp-remote-agent`：只依赖 POSIX + Git，不开监听端口；token
  来自 `--token-file`（0600）或 `--token-env`，在 SSH 场景下若远端没有该
  环境变量会回退到 `~/.verlinpark/remote-token`（0600）。
- 操作：`stat`、`list_dir`、`read_file`（offset/limit/UTF-8/base64）、
  `write_file`（expected SHA + 临时文件 + fsync + rename）、`mkdir`、
  `remove`、`rename`、`exec`（cwd jail、env allowlist、timeout、output cap、
  cancel）、`git_status/diff/log/rev_parse/worktree_add/list/remove`、
  `ledger_stats`、`cancel`、`shutdown`。
- canonical root path jail：拒绝绝对路径、`..`、控制字符、Git metadata、
  symlink escape；写目标拒绝 symlink，原子 rename；operation ledger 支持
  operation id 幂等/冲突/审计。
- 项目 `ExecutionHost`（SQLite + manifest）选择 `local` / `ssh`；新增显式
  `remote_read/write/list/exec/git/worktree` 工具，本地工具绝不静默改路由。
  远程写/exec 仍走 approval broker；`network=deny` 或无法强制的读限制直接
  拒绝 remote exec（远端不是 Seatbelt 沙箱）。
- RPC：`host.list/add/update/remove/probe`、`remote.status`、
  `remote.worktree_list`、`remote.ledger`；helper 打包进 app/release。
- 边界：helper 需预装；SSH 主机/用户是信任基础设施；没有 remote daemon、
  跨主机 scheduler、远程 merge queue；远程 worktree 不会自动进入本地
  merge queue。

### R3 project programs / milestones

- 新表：`programs`、`program_milestones`；追加式
  `program_milestone_sessions` 记录每次派发，重试不清除历史，program /
  milestone 预算因此跨 attempt 累计，retry 只获得剩余额度。
- 确定性 controller：按 priority、aging、dependencies、max_parallel 选
  ready milestone；先 atomic claim 再创建普通 session + one-task TeamPlan；
  completion 需要 session idle、T1 completed、integration evidence clean、
  acceptance 非空；失败按 exponential backoff 重试，耗尽后 blocked；依赖
  失败按 block/cancel/retry policy 处理；pause/resume/cancel 协作式。
- 事件：`program.created/updated/paused/resumed/completed/failed/deleted`、
  `program.milestone.created/updated/started/verified/completed/failed/
  blocked/retried/skipped/cancelled`；program ledger 进入 manifest /
  bundle / `db-rebuild`。
- RPC：`program.list/create/update/pause/resume/cancel/delete/status`、
  `program.milestone.*`、`program.tick`、`program.replay`；CLI
  `vp programs` / `program-*`；Manage Dash Programs 卡（里程碑、依赖、
  attempts、预算、pause/resume/cancel/retry/skip、详情和事件）。
- 边界：没有 LLM program planner，没有跨项目 program，没有 distributed
  scheduler/fencing；program budget 不是全局 pre-flight reservation。

### R4 native macOS computer control

- 新打包 helper `VerlinParkComputerHelper`（Accessibility + CGEvent +
  ScreenCaptureKit），JSON-lines 协议、1 MiB 请求 / 2 MiB 响应上限、父进程
  watchdog、scratch HOME、进程组清理。
- 动作：`permissions`、`apps`、`windows`、`focus`、bounded `snapshot`
  （稳定 `ax-…` ref、secure field 脱敏）、`screenshot`（window/screen PNG
  到 daemon artifact）、`click`（ref 或 normalized point）、`type`、`key`、
  `scroll`、`drag`、`wait`。
- Rust `vp-core/src/computer.rs` 持有 helper 生命周期、frame/超时/空闲
  sweep、`ComputerLease` 串行化、daemon 侧二次参数校验；Agent 工具
  `computer` 只给 Leader 和 coding Worker/Labour。
- 风险映射：read/permissions/apps/windows/snapshot/wait/screenshot →
  External；focus/click/type/key/scroll/drag → HostState；cmd+q、
  cmd+shift+delete 等 → Destructive；approval modes 仍然生效。
- 边界：不是 sandbox；helper 以登录用户 TCC/CGEvent 权限运行；AX tree
  因 app 而异，ref 只在 snapshot 生命周期内有效；GUI/TCC 只做手工 smoke，
  不做 CI GUI 覆盖；没有 menu-bar walker、AppleScript、clipboard、URL
  opener 或 shell 路径。

### 验证与集成

- 合并顺序：R3 (`5208270`) → R2 (`191ec56`) → R3 review fixes
  (`6298fc0`, `315ecf7`) → R4 (`0d75274`) → R1 (`a504e10`)；R2 token
  修复 `7e4a7ba`。
- `git diff --stat c615f72..HEAD`：**42,176 insertions / 1,715 deletions**。
- Rust：932 passed，5 ignored（2 core + 3 live）；Swift：170 app +
  116 computer-helper passed；`cargo fmt --check`、`cargo clippy -D warnings`
  通过，`just lint` / `just test` / `just bundle` 在收尾时重跑。
- 仍然不要声称：provider-call exactly-once、mid-stream resume、remote
  daemon/cross-host scheduler、distributed program/merge fencing、native GUI
  sandboxing、precise preemption beyond the documented boundaries。

---

## 2026-11-11 Efficiency playbook E1-E4: cache telemetry, Scout, idempotency, dashboard

这一轮把 OpenCode + Orca 的研究转成 VerlinPark 内部可执行的 E1-E4，全部保持
deterministic Rust、无隐藏模型路由、不削弱 approval/sandbox/Verifier/merge/
capacity/budget gate。

### E1 cache telemetry

- `UsageTotals` / `UsageBucket` / `ProjectUsageSession` / `DaemonMetrics`
  增加派生 `cache_hit_ratio`（不持久化）。
- 公式：`uncached = input - cache_read - cache_write`，
  `ratio = cache_read / (uncached + cache_read + cache_write)`，clamp 0..1；
  input 为 provider total 时等价于 `cache_read / input`。
- 新 RPC `session.cache` → input/output/cache read/write、ratio、prefix hash、
  `preamble_tokens_estimate`、last call。
- 新事件 `context.cache`；`context.measured` / `context.prompt_prefix` 扩展
  cache ratio 与 preamble estimate。
- Prometheus：`verlinpark_cache_hit_ratio`、
  `verlinpark_preamble_tokens_estimate`（无数据时省略）。
- CLI `vp cache <session>`；usage CSV 增加 ratio 列。
- 不做自动 warming、不推断 provider TTL、不产生额外 provider call。

### E2 read-only Scout

- 新结构角色 `scout`，只读工具集：
  read/list/read_files/find_files/file_info/grep/repo_map/find_symbol/
  find_references/dependency_graph/affected_tests/code_diagnostics/
  build_context/git-read/list_sources/search_sources/read_source/recall/
  team_status。
- 没有 shell/write/edit/apply_patch/process/delegate/MCP/browser/coordinate。
- Leader/Manager 可调用 `scout` 工具：
  `{objective, files?, max_steps?}`，返回 bounded summary + usage + evidence。
- 每个 session run 最多 3 个 Scout，占 Worker capacity lane；共享 session
  budget / RunControl；parent cancel 会取消 Scout。
- 无 task worktree、无 commit、无 Verifier、无 merge_queue；post-run
  `git status --porcelain --untracked-files=all` 守卫拒绝新变化。
- 事件：`agent.spawned(read_only: true)`、`agent.started`、role scout 的
  turn/tool/message、`agent.finished`、`scout.completed`。
- 明确边界：不是安全沙箱；`code_diagnostics` 可能跑项目 check task；没有
  网络研究/MCP/嵌套 Scout。

### E3 idempotent session operations

- `session.prompt` / `session.defer` / `session.steer` 增加可选
  `operation_id`。
- 新表 `session_operations`：
  `(session_id, operation_id)`、method、fingerprint、status、result/error。
- 同 ID + 同 payload + completed → replay；pending → in-progress；
  different fingerprint → conflict；failed → safe retry；unknown → fail closed。
- daemon 启动把残留 pending 标 unknown。
- CLI `--operation-id`。
- `/provider` 调用不是 exactly-once；receipt 是 operational state，不是
  replay projection。

### E4 efficiency dashboard

- Manage Dash → Usage & Budget 新增 `EfficiencyCard`：
  cache hit ratio、cached/uncached input、output、cache write、cost、
  cost/session、context usage、compaction count、preamble estimate。
- Stage context popover 增加 cache hit pill + prefix hash。
- `EfficiencyMetrics` 纯函数负责 ratio/clamp/format/compaction 统计。
- 所有值来自真实 RPC；`session.cache` 不可用时从 `session.usage` 派生。
- Collab graph 增加 `scout` 节点颜色/图标/布局。

### 测试

- Rust：**618 passed，4 ignored**
  - core 490 + 1 GUI ignored
  - protocol 14
  - store 89
  - providers 18 + 3 live ignored
  - daemon 4
  - cli 3
- Swift：**163 passed**
- ADR-0151（idempotent operations）、0152（Scout）、0153（cache telemetry）、
  0154（efficiency dashboard）已记录。

### 边界

- cache ratio 是 observational cache share，不是账单折扣；cache read 仍计费。
- preamble 是 system+tools 的启发式估算，不是 provider tokenizer。
- Scout 不是安全沙箱；只依赖工具 schema + 执行时 backstop + post-run guard。
- idempotency 不覆盖 `session.create`，也没有跨 daemon fencing 或 receipt GC。
- efficiency dashboard 不推断 provider 价格，不声称 cost per solved task。

---

## 2026-11-11 Daemon metrics, recurring continuations, richer previews, and backup

这一轮并行补了五个运维/长任务缺口，全部保持 deterministic Rust / 无隐藏路由。

### Daemon metrics

- `GET /metrics`：Prometheus text，要求与 `/rpc` 相同的
  `Authorization: Bearer <discovery token>`。
- `daemon.metrics` RPC 返回结构化快照 + 渲染文本；`vp metrics [--prometheus]`。
- 指标：
  - sessions / plan tasks / merge queue / intake / continuations / recoveries /
    memories / conflicts 按状态
  - runtime leases 按 kind
  - tokens input/output、cache read/write、USD total、projects、uptime
- 无 session id、task id、路径、模型名或密钥标签；每类最多 64 个分组。
- 不改变 `/health`，也不改任何调度/审批行为。

### Recurring session continuations

- `SessionContinuation` 增加 `series_id`、`parent_continuation_id`、
  `recurrence_every_secs`、`recurrence_max_runs`、`run_count`。
- `session.defer` 增加 `every_secs`、`max_runs`；CLI
  `vp defer ... --every-secs N --max-runs K`。
- 第一次仍由 watch/deadline 触发；成功后在同一事务内：
  - 当前 row → `delivered`，`run_count += 1`
  - 写 `session.continuation.delivered`
  - 若未达 max runs，创建 child：
    - 继承 series/body/recurrence
    - 清空 watch task，只保留 `deadline_at = now + every_secs`
    - status `pending`
    - 写 `session.continuation.scheduled`
- child 不会因为原始 watch task 已 settled 而立刻重跑。
- `every_secs` 60..=2,592,000；有 interval 时 `max_runs` 默认 10、范围 2..=100；
  无 interval 时只能 one-shot。
- 这是 delivery-relative interval，不是 cron；没有 catch-up、jitter、
  series-wide cancel 或跨进程 lease。
- 失败/取消/落地/导入/达到 max runs 都不创建 child；`delivering` crash 仍
  fail closed。

### Gitea / Forgejo PR

- 新增 `PrCli::Gitea`，通过 `tea`：
  - `VERLINPARK_PR_PROVIDER=gitea|forgejo`
  - `VERLINPARK_TEA_BIN`（默认 `tea`）
  - preflight `tea login list`
  - `tea pr create --head ... --base ... --title ... --description ...`
- GitHub/GitLab 行为不变；不自动猜测任意 self-hosted hostname。
- 不读取/存储 forge token；失败不会伪造成功。

### Rich artifact previews

- 分类：image / pdf / video / audio / text / binary / archive / other。
- Image 支持更大的 zoom/scroll 视图、尺寸、Quick Look。
- PDF 使用 PDFKit，支持 page count 和前后翻页。
- Video/audio 使用 AVKit，本地文件播放。
- Binary 从 1 KB hex 升级为 64 KB 分页 hex（offset + ASCII），
  只读取 daemon 给出的 artifact path；inline 数据在 app cache 物化。
- Text 支持 wrap 切换、Save As；统一支持 Reveal in Finder、Copy SHA-256。
- 不做 archive 解压、媒体转码、远程/云 URL 或 artifact 执行。
- 临时文件只落在 app cache，dismiss/deinit 清理并做启动 sweeper。

### Backup / restore

- `vp backup --output <zip> [--include-artifacts]`
- `vp restore --input <zip> --data-dir <dir> [--force]`
- SQLite 用只读连接 + `VACUUM INTO` 做一致性快照，不复制 WAL/SHM。
- 归档包含 `backup.json`（版本、时间、大小、SHA-256）、数据库快照、
  `projects.jsonl`、per-session event mirrors；可选 artifact blobs。
- 排除 discovery、instance lock、logs、worktrees、scratch、secrets。
- restore 先校验 manifest/路径/类型/重复/symlink/大小/checksum，再解压到
  兄弟临时目录并原子替换；失败保留旧数据。
- 非空目标/已有数据库需要 `--force`；daemon 运行中不自动停止。
- 没有加密/签名、增量备份、worktree/Git 备份或 live-daemon quiesce。

### 其他

- lease 到期比较改为 SQLite `julianday()`，修复 RFC3339 fractional seconds
  导致的过期 lease 卡住问题（上一轮已记录，本轮保持）。
- 测试：Rust **590 passed，4 ignored**；Swift **153 passed**。
- ADR-0146（metrics）、0147（recurring continuations）、0148（Gitea/Forgejo PR）、
  0149（rich preview）、0150（backup/restore）已记录。

---

## 2026-11-11 Memory quality: conflict review, health, and verdict capture

这一轮把 memory 从“可检索、可验证”推进到“可审查、可沉淀”。仍然没有
embedding、LLM 判断或隐藏路由；冲突检测只做同 scope、不同 body 的确定性
review signal，是否解决由 operator 显式决定。

### Same-scope conflict review

- 两条 active memory 共享同一 evidence reference 且 body 不同时，产生
  potential conflict：
  - `decision` / `convention` / `fact` 按 exact `path` evidence 分组
  - `verification` / `task_result` 按 exact `task` evidence 分组
- stale / superseded / archived 不参与；检测上限 5,000 条 memory、每组 50
  对、总共 500 对，排序确定。
- conflict id 是 project + scope + sorted memory ids + normalized body hashes
  的 SHA-256 前 32 位；内容不变时 resolved 状态保持，内容变化才产生新 row。
- 检测只插入 `open` row 并写 `memory.conflict.detected`，**从不修改 memory**。
- 显式 resolution：
  - `keep_a` / `keep_b`：loser → superseded，`superseded_by = winner`
  - `archive_both`：两条 → archived
  - `keep_both`：不改 memory
  - conflict → resolved，写 `memory.conflict.resolved`
  - memory mutation、conflict row、events 在同一事务提交
- 默认每小时扫描（`VERLINPARK_MEMORY_CONFLICT_SECS`，0 关闭），只检测、
  不自动解决。
- `memory.conflicts` / `memory.conflict.scan` / `memory.conflict.resolve` /
  `memory.health`；CLI `vp memory-conflicts` / `memory-conflict-scan` /
  `memory-conflict-resolve` / `memory-health`。
- Manage Dash → Storage & Memory 新增 Memory Health 卡片：active/stale/pinned、
  evidence coverage、verification due、open conflicts、by-kind，以及逐条
  Keep A / Keep B / Keep Both / Archive Both。

### Verifier and integration capture

- Verifier approved/rejected 自动写 `verification` memory：
  - source `verifier`
  - evidence task/session/commit
  - TTL `default_verify_days`（fallback 365）
  - body 包含 approved、verdict、commit；approved 额外包含真实 integration 结果
- rejected 永远写 `approved: false`，不会出现 merged 声明。
- 冲突解决在 re-verify 通过并成功 requeue 后写 `integration` memory：
  - source `orchestrator`
  - evidence task/session/commit
  - 后续 Integrator 失败会如实写 `integration failed: ...`
- 两种 capture 都是 best-effort：失败只记 warning，不改变 Verifier / merge /
  approval / sandbox / write scope / capacity / budget 结果。
- 重试后重新验证会 supersede 旧结论，event history 保留旧 claim。
- 新 ranking 权重：`verification` 0.24、`integration` 0.22、
  `verifier` source 0.18；exact task 0.60 仍是主导结构信号。

### Lease comparison fix

- RFC3339 可能包含 fractional seconds；用字符串直接比较到期时间会把
  `...00.500Z` 排在 `...00Z` 之后，导致过期 lease 卡住。
- runtime/computer lease 的到期比较改为 SQLite `julianday()`，并新增
  fractional-second 回归测试。

### 测试

- Rust：**562 passed，4 ignored**
  - core 455 passed + 1 browser ignored
  - protocol 7
  - store 82
  - providers 18 + 3 live ignored
- Swift：**136 passed**
- `just lint`、`just test`、`just bundle` 通过。
- ADR-0144（conflicts/health）和 ADR-0145（verdict/integration capture）已记录。

### 边界

- 只检测同 scope 冲突；不同 scope 的矛盾不在本轮范围。
- 不是 semantic contradiction detection，不做 LLM/embedding 判断。
- URL evidence 不抓取、不验证。
- open conflict 不会自动消失，必须 operator 显式解决。
- capture 不写失败诊断，只复述 Verifier 自己的 summary。
- memory 仍 project-scoped。

---

## 2026-11-11 Evidence-backed project memory v2 and long-task operator console

这一轮没有照搬 Agent Swarm 的 chat memory + embedding + LLM 自评。VerlinPark 的
持久真相在事件、task result、commit 和 Verifier 结论里，所以 memory 被做成
「带 provenance 的 claim」，检索和生命周期都保持 deterministic Rust。

### Evidence-backed memory

- `MemoryItem` 新增：
  - `status`（active/stale/superseded/archived）
  - `source`（agent/task_result/operator/orchestrator/import）
  - `evidence`（commit/path/task/session/event/url）
  - `supersedes` / `superseded_by`
  - `pinned`
  - `use_count` / `last_used_at`
  - `last_verified_at` / `verify_after` / `stale_reason`
- 旧 events、bundle、客户端通过 `serde(default)` 继续解码。
- 检索是 lexical + structural + behavioral：
  - FTS5 bm25（或 LIKE fallback）× 0.60
  - exact task evidence/tag +0.60
  - path prefix overlap，每项 +0.15，上限 0.45
  - department +0.20
  - kind/source 权重
  - 30 天 half-life recency
  - observed usage，`ln(1+use_count)`
  - pinned +0.50
  - stale ×0.25，且只有显式 `include_stale` 才参与
- `MemoryMatch` 返回 score 和 reasons（`fts`、`task:T3`、`path:src/api`、
  `department:eng`、`pinned` 等），可解释、可测试；没有 embedding、provider
  call 或 hidden routing。
- 去重/替代按 project + normalized kind/title：
  - 完全相同 body/evidence/source → 幂等返回，不写新 event
  - 同 key 不同内容 → 一个事务内插入新 active row，旧 row 标 `superseded`，
    写 `memory.superseded` + `memory.created`
  - `dedupe=false` 是显式逃生口；`auto_supersede` 可项目级关闭
- 任务完成时自动写 `task_result` memory，evidence 包含 task、session、
  最新 merged commit，以及最多 20 个 changed path；重试后再次完成会替代旧结果，
  不再因为 title 已存在而跳过。
- `memory.brief` 用 session goal + task title/acceptance/department/write scope
  检索，按 project `brief_max_chars`（默认 4000）做字符预算；被注入的 memory
  每个 session 只记一次 `memory.used`。
- `memory.verify` 只做本地证据检查：
  - commit：`git cat-file -e` + `git merge-base --is-ancestor`
  - path：项目根内存在
  - task/session/event：存在性检查
  - url/unsupported：明确返回 `unchecked`，不会假装已 fact-check
- `memory.sweep` 只把过期 active、非 pinned 行标 stale，不删除、不自动恢复；
  daemon sweep 由项目 `MemoryPolicy.sweep_stale` opt-in，
  `VERLINPARK_MEMORY_STALE_SECS` 默认 3600，0 关闭。
- `memory.prune` 现在明确跳过 pinned；`keep_latest` 只计算 unpinned 行。
- Events：`memory.created` / `updated` / `verified` / `stale` / `superseded` /
  `used` / `deleted`，每个都携带完整 `MemoryItem`，replay/repair 为
  last-write-wins；bundle 和 db-rebuild 覆盖新字段。
- RPC/CLI：`memory.search`、`memory.brief`、`memory.update`、
  `memory.verify`、`memory.sweep`、`memory.stats`；
  `vp memory-search`、`vp memory-brief`、`vp memory-update`、
  `vp memory-verify`、`vp memory-sweep`、`vp memory-stats`。
- 没有新增 agent-facing tool；`remember`/`recall` 仍以 source=agent 工作。

### Long-task operator console

- Manage Dash → Team & Tasks 新增：
  - `RecoveryCard`：stall recovery intent/status/attempts/grace/error，只读
  - `ContinuationCard`：watch task/mode/deadline/wake reason/outcomes，可取消/新建
  - `SteeringCard`：pending/delivered/cancelled，可取消 pending
  - `TaskStallCard`：progress + stall 观测，只读
- Stage：
  - running/paused 时显示 `Steer`
  - 会话菜单增加 `Defer Continuation…`
  - 顶部显示 continuation/recovery 提示
- 所有动作走真实 RPC，错误内联；steering 明确说明只在下一个 provider
  boundary 交付，continuation 明确说明不会立即运行。
- Manifest 增加证据型 memory 的 pin/verify/sweep UI，并把
  `auto_supersede`、verify TTL、brief 字符预算、stale sweep 暴露到
  Memory retention 卡片；保存 policy 时完整回传 v2 字段，避免静默重置。

### 边界

- 没有 semantic recall / embedding；CJK 走 LIKE fallback。
- verify 不是 fact-check；URL evidence 不抓取。
- memory 仍是 project-scoped，不跨项目。
- `memory_usage` 是 operational 表，不进入 bundle；`use_count` 会保留。
- brief 是字符预算，不是 tokenizer 精确计数。
- continuation 是 operator-only、非 recurring；recovery 是 same-task/department，
  不是 same-agent identity。
- 没有 recovery cancel RPC，因此 UI 不提供该动作。

### 测试

- Rust：**539 passed，4 ignored**
  - core 439 passed + 1 browser ignored
  - protocol 6
  - store 76
  - providers 18 + 3 live ignored
- Swift：**132 passed**
- `just lint`、`just test`、`just bundle` 通过。
- ADR-0142（evidence-backed memory）和 ADR-0143（long-task console）已记录。

---

## 2026-11-11 Policy-gated stall recovery and deferred continuations

这一轮把“卡死可见”推进到“可选地自动恢复”，并补上“结束后再唤醒”的
continuation 原语。二者都默认保守：stall recovery 默认 `observe`，
continuation 必须由 operator 显式创建。

### Policy-gated stall recovery

- `OrchestrationPolicy` 增加：
  - `stall_recovery`: `observe`（默认）/ `escalate` / `retry`
  - `stall_min_observations`: 默认 2，范围 1..=10
  - `stall_max_attempts`: 默认 1，范围 0..=5（0 禁止自动重试）
  - `stall_grace_secs`: 默认 300，范围 0..=86400
- `observe` 完全保持 ADR-0139 的观测行为；旧 project policy 通过
  `serde(default)` 解码为 observe。
- `escalate`：达到阈值后写一条 critical coordination escalation，不取消任务。
- `retry`：达到阈值且任务仍 running/claimed 时，先持久化 recovery intent，
  再走现有协作式 cancel；独立的 `VERLINPARK_STALL_RECOVERY_TICK_SECS`
  （默认 30，0 关闭）等待 task terminal + session idle/failed/cancelled/draft，
  然后复用 `retry_plan_task` + `resume_plan` 一次。
- 事件：
  `orchestration.recovery_requested` / `waiting_cancel` / `retried` /
  `escalated` / `failed` / `unnecessary`。
- `orchestration_recoveries` 是 event-backed projection，支持 replay/repair、
  bundle/import、db-rebuild。
- max attempts、grace、budget exhausted、retry 失败都会 fail + escalate，
  不无限循环；completed/skipped 标记 unnecessary。
- 这是 **same-task/department** recovery，不是 immutable same-agent pin；
  重试时会创建新的 Worker。
- CLI/RPC：`vp recoveries <session>`、`orchestration.recoveries`，
  policy 字段通过现有 `orchestration.policy.get/set` 和 CLI flags 暴露。

### Deferred session continuations

- 新增 `session_continuations` projection：
  id、session/project、body、watch_task_ids、watch_mode（all/any）、
  deadline、status（pending/ready/delivering/delivered/cancelled/failed）、
  attempts、wake_reason、outcomes、last_error 等。
- 事件：
  `session.continuation.created` / `ready` / `delivering` / `retrying` /
  `delivered` / `cancelled` / `failed`。
- RPC/CLI：
  `session.defer` / `session.continuations` / `session.continuation.cancel`
  `vp defer <session> "prompt" [--watch ...] [--watch-mode all|any] [--after-secs|--at]`
  `vp continuations <session>`、`vp continuation-cancel <id>`。
- 语义：
  - `all` 等所有 watch task terminal；`any` 第一个 terminal；
  - terminal 不等于 success，`failed/cancelled/skipped/interrupted` 都算 settled；
  - deadline 是 fallback；watch 和 deadline 同时满足时 watch 有具体 outcome；
  - plan revision 后消失的 task 视为 `missing` settled，避免永久挂起；
  - 只有 session 处于 idle/failed/cancelled 才调用正常
    `AgentService::start`；running/queued/paused/waiting_input 保持 ready；
  - `ready -> delivering -> delivered`；start 失败按 attempt/backoff 重试，
    达到 cap 后 failed；
  - daemon 重启时残留 `delivering` 行标记 failed，fail-closed，不重复启动；
  - prompt 是普通 user message，前面带 bounded 2,000 字符 context note。
- operator-only；没有 agent-facing tool；不创建 plan task，不替代 plan DAG。

### 边界

- stall retry 默认关闭；自动 cancel 是协作式的，无法中断不回到 turn/tool
  boundary 的 provider stream。
- recovery 不保证 same-agent identity、conversation 或 worktree state。
- 本轮 continuation 不是 recurring workflow，也不是 cross-process lease；后续
  round 2 已加入 recurring series、same-host lease fencing 与 Manage Dash
  管理面，跨主机仍不支持。
- continuation 启动失败会消耗 attempt budget；provider 未配置时最终 failed。
- 本轮没有 SwiftUI/Manage Dash continuation 管理面；后续 round 2 已补齐。

### 测试

- Rust：**526 passed，4 ignored**
  - core 432 passed + 1 browser ignored
  - protocol 4
  - store 72
  - providers 18 + 3 live ignored
- Swift：**111 passed**
- `just lint`、`just test`、`just bundle` 通过。
- ADR-0140（stall recovery）和 ADR-0141（deferred continuations）已记录。

---

## 2026-11-11 Long-task cache telemetry, operator steering, and progress ledger

这一轮补上超长任务最缺的三件事：token 成本可解释、运行中可纠偏、卡死可见。
三项都保持现有 approval、Seatbelt、path jail、write scope、Verifier、merge queue
和 budget gate 不变。

### Provider cache usage

- `StreamEvent::Usage` 增加 `cache_read_tokens` / `cache_write_tokens`。
- 语义固定为：`input_tokens` 是 provider 报告的总输入（uncached + cache read +
  cache write）；cache 字段是 total 的子集，只用于定价和命中率，不会重复加到
  session tokens。
- OpenAI-compatible：`input_tokens = prompt_tokens`，读取
  `prompt_tokens_details.cached_tokens`（fallback `prompt_cache_hit_tokens`）和
  provider 暴露的 cache write 字段。
- Anthropic：`input_tokens + cache_read_input_tokens +
  cache_creation_input_tokens` 组成 total；三个字段分别保留。
- 不一致的 provider 报告会被 clamp，保证 cache read/write 不超过 total。
- `usage_ledger` 增加 cache 列；`UsageEntry`、`UsageBucket`、`UsageTotals`、
  project usage、CSV、bundle/import、replay/repair、`db-rebuild` 全部携带。
- `RoleModelConfig` 增加 `cache_read_usd_per_million` /
  `cache_write_usd_per_million`；未配置时回退到 full input price，绝不自动
  推断折扣。
- 成本公式：uncached input × input + cache read × cache-read price + cache
  write × cache-write price + output × output。预算 reservation 仍按 full input
  预留，结算按 provider 报告的实际 mix。
- Preview Cost 显示 cache read/write 和 derived hit rate
  `cache_read_tokens / input_tokens`；Settings 暴露 per-role cache prices。

### Stable prompt prefix

- `role_system_prompt` 拆成稳定前缀和 run context：
  - 稳定前缀：role charter、security/output contract、稳定项目约定；
  - run context：agent/department/parent、project root、session goal、
    worktree、branch、profile、write scope、objective；
  - goal-dependent memory 放在 run context 之后。
- `context.prompt_prefix` 每个 agent run 发一次，包含 version、model、
  `tool_bundle_hash`、`stable_system_hash`、`prefix_hash` / `cache_generation`、
  tool count 和 stable prefix chars；hash 输入不含 secrets、API keys、
  worktree path 或 session id。
- skill 激活不再过滤 advertised tool schema；完整 role tool bundle 在 run
  内冻结，执行时仍用 `skill_allows_tool` 拒绝并返回精确错误。
- dynamic MCP tools 按 exposed name 排序后截断到 32 个，并在 run 开始时
  收集一次。
- Anthropic：`ChatRequest.system_stable_prefix` 精确匹配时，把 system 拆成
  稳定 cached block + 非缓存 run-context block，并在最后一个 tool 上加
  `cache_control: {"type":"ephemeral"}`。OpenAI-compatible 忽略该 marker。
- 限制：Anthropic 不会自动加 beta header；兼容 proxy 忽略 cache_control 时
  内容仍然有效；OpenAI 仍走自动 prefix cache；compaction 仍会重置
  conversation head 的 cache generation。

### Operator steering

- 新增 SQLite `steering_messages`，字段：id、session_id、agent_id、可选
  task_id、body、status（pending/delivered/cancelled）、created_at、
  delivered_at。
- RPC：`session.steer`、`session.steering`、`session.steering.cancel`。
- CLI：`vp steer <session> "text" [--agent <id>] [--task <id>]`、
  `vp steering <session>`、`vp steer-cancel <id>`。
- 默认目标为 session 中 running/waiting_input 的 Leader；显式 agent 必须属于
  该 session 且处于 running/waiting_input；task scoped steering 要求该 agent
  正在执行该 plan task。
- 在下一个 provider call 之前、pause/cancel/budget gate 和 compaction 之后
  投递；以带 `[Operator steering #<id>]` 的 user turn 进入 conversation，
  同时写 `steering.delivered`（top-level 还写 `message.created`）。
- run 结束时仍 pending 的 steering 会被取消并写 `steering.cancelled`；daemon
  恢复也会取消 pending 行，避免泄漏到下一次 run。
- 这是 operator-only RPC/CLI，不新增 agent-facing steering tool，不做隐式广播。
- 限制：cooperative delivery，只在下一个 turn/tool 边界生效；不能中断已经
  在进行的 provider stream/tool call。

### Task progress / stall ledger

- 新增 `plan_task_progress` 伴生表：
  `last_progress_at`、`stall_count`、`last_turn`、`last_tool`、`updated_at`。
- 仅对 task-scoped run 在 turn 开始和 tool 开始时 touch，不按 token/chunk 写。
- daemon sweep：`VERLINPARK_TASK_STALL_SECS` 默认 1800 秒，0 关闭；
  只观察 `running`/`claimed` plan tasks；超过阈值时递增 `stall_count`、
  刷新 observation window，并发出一个 `orchestration.stall` 事件；事件会
  broadcast 到 live subscribers。
- 事件包含 task id、plan seq、department、title、status、attempts、
  `age_secs`、`last_turn`、`last_tool`、`stalled` 等。
- 可见面：`session.stalls` / alias `task.stalls`、`vp task-stalls <session>`、
  `task_detail`、`task.queue`、`orchestration.snapshot`、`orchestration.overview`
  的 `stalled_tasks`。
- 本轮只做观测，不自动 cancel/retry/pause/改 budget/write scope；自动恢复和
  same-agent pin 留给后续轮次。
- `plan_task_progress` 是 operational telemetry，不是 replay projection；
  `session.replay` 不比较它，daemon restart 可能丢失最近 observation window。

### 测试

- Rust：**505 passed，4 ignored**
  - core 412 passed + 1 browser ignored
  - protocol 3
  - store 72
  - providers 18 + 3 live ignored
- Swift：**111 passed**
- `just lint`、`just test`、`just bundle` 通过。
- ADR-0138（cache usage / stable prefix）和 ADR-0139（steering / progress
  ledger）已记录边界。

---

## 2026-11-10 Browser helper and daemon-wide ComputerLease

这一轮把 browser 和 ComputerLease 从“设计缺口”变成可运行实现，同时明确
它不是无边界浏览器，也不是 distributed lease。

### ComputerLease

- 新增 SQLite `computer_leases`，一行一个交互 surface；当前 surface 是
  `browser`。
- 记录 lease id、surface、owner、session、agent id/name、acquired/heartbeat/
  expires 时间。
- TTL 120 秒，心跳每 TTL/3；过期行在 acquire 事务内删除并写
  `computer.lease.expired`。
- acquire 冲突 fail-fast，返回当前 holder；没有隐藏队列。
- 审计事件：`computer.lease.acquired` / `denied` / `released` / `expired`。
  它们是 daemon 运维事件，不是 session replay projection。
- daemon recovery 清理残留 lease；`computer.leases`、
  `computer.leases.sweep`、`computer.leases.release` 三个 RPC 和
  `vp computer [--sweep|--release <id>]` 提供状态、过期清理和运维释放。
- 与 `write_leases` 分离：`write_leases` 串行 worktree 写作用域；
  `computer_leases` 串行视觉/交互 surface。

### Browser helper

- 新增 Swift/WKWebView `VerlinParkBrowserHelper` 可执行文件，由 daemon 通过
  独立 Seatbelt profile 启动：
  - 允许普通读取和 localhost 网络；
  - 写入只允许 helper scratch、`/dev/null`、`/dev/zero`；
  - 显式 deny worktree 写入；
  - deny Apple event；
  - 没有 daemon 的 sandbox marker 时 helper 拒绝启动，避免意外 unsandboxed
    运行。
- 非持久 WebKit data store，使用 scratch `HOME`/`TMPDIR`/cache；不共享用户
  cookie、local storage 或网站数据。
- 导航限制：`localhost` / `127.0.0.1` / `[::1]` 的 HTTP(S)，以及当前
  worktree 内的 `file:` 页面；拒绝 URL credentials、外部 host、`data:` URL
  和其它 scheme。helper 侧重复校验。
- 阻止 download、新窗口、file picker、JavaScript dialog；confirm/prompt
  被拒绝或取消。
- 动作：`open`、`snapshot`、`click`、`type`、`press`、`select`、`wait_for`、
  `screenshot`、`console`、`evaluate`、`close`。
- `snapshot` 是 DOM 派生的 accessibility-shaped tree，带稳定 `eN` 引用、
  role/name/text/value/checked/disabled、URL、title、viewport；不是 native
  macOS AX，也不默认返回完整 HTML。
- `screenshot` 为 viewport-only PNG；helper 先写 scratch，daemon 再经 path
  jail、symlink-parent 检查、16 MiB cap、atomic write 和 SHA-256 写入
  worktree，默认 `.verlinpark/browser/`。
- `evaluate` 是唯一 JavaScript 执行动作，风险级别 `HostState`：`smart` /
  `strict` 询问，`unattended` 拒绝。
- 角色：Leader 和 coding Worker/Labour；Manager、Verifier、research、
  verification、generic 不可用。
- 生命周期：每个 `(session, worktree)` 一个 helper；close、idle sweep 和
  session cleanup 显式释放 lease；daemon exit 停止 helper，残留 lease 由 TTL
  过期或下次 daemon recovery 清理；`VERLINPARK_BROWSER_IDLE_SECS` 默认 600。
  helper 崩溃/超时后下一次动作重建。
- 打包：Xcode target `VerlinParkBrowserHelper`；`just bundle` 和
  `just package` 把 helper 复制到 daemon 旁边；`VERLINPARK_BROWSER_HELPER`
  可覆盖路径。缺失 helper 返回显式
  `BROWSER_HELPER_UNAVAILABLE`，不是 stub。
- `tool_search` 现在从该角色实际收到的完整 tool bundle 搜索，包含
  `browser` 等 special-cased 工具。

### 边界

- macOS-only，需要已登录的 GUI session；不是 headless CI browser。
- helper 的 Seatbelt profile 只约束 helper 自身写入；WebKit 的 XPC 服务是
  Apple 签名的系统服务。**page JavaScript 仍可向外部 host 发起网络请求**，
  browser 不是 network isolation boundary。
- `snapshot` 是 DOM-derived，不是 native AX；synthetic input 不覆盖所有
  浏览器默认行为。
- screenshot 只截 viewport；`full_page` 参数被接受但不生效。
- ComputerLease 是单 daemon SQLite lease，不是 distributed fencing token；
  冲突 fail-fast，无优先级/aging 队列或公平调度；manual acquire 不暴露。
- helper 必须随 app/daemon 构建；source-only checkout 未构建 Xcode target 时
  会得到明确 unavailable 错误。

### 测试

- 新增 lease contention/ownership/expiry/sweep/recovery 和 browser URL/
  permission/helper lifecycle 测试。
- ignored 的真实 helper 集成测试：
  `cargo test -p vp-core browser_helper_opens_local_page_and_saves_screenshot -- --ignored`
  （需要已构建 helper 和 GUI session）；本轮在主工作区已验证通过。
- 当前 Rust 测试 490（core 402 + 1 ignored / protocol 2 / store 72 /
  providers 14；另有 3 个 live ignored），Swift 110；`just lint`、
  `just test`、`just bundle` 通过。

---

## 2026-11-10 Office documents, OCR confidence, PDF metadata

### `read_document`

- 支持 DOCX/ODT：按文档顺序提取标题、段落、表格。
- 支持 PPTX/ODP：按 presentation relationship 顺序逐页提取文本，支持
  `pages=1-3,5` 过滤。
- 支持 XLSX/ODS：sharedStrings、文本/数字/布尔/错误 cell、sheet 名称，
  支持 `sheets` 过滤和 `max_rows`/`max_cols` 上限。
- ZIP 安全：
  - 128 MiB 容器上限
  - 64 MiB 单 entry 解压上限
  - 256 MiB 总声明解压上限
  - 10,000 entries
  - 拒绝重复/加密/NUL/反斜杠/绝对路径/`..`/越界 entry
  - 实际读取再次用 `Read::take` 限界，不把 archive member 解压到磁盘
- XML 安全：
  - `quick-xml` tokenizer
  - 拒绝 DTD
  - XML 嵌套深度 512
  - 只解析 predefined/numeric entities；unknown entity 原样保留并 warning
  - 支持 UTF-8 和 BOM UTF-16，其它编码显式拒绝
  - 输出 200k chars、block/cell/sheet/slide 多级 cap
- 结构化输出包含 `format`、`entries`、`archive_entries`、`chars`、
  `extracted_chars`、`truncated`、`warnings`、`tables`/`slides`/`sheets`；
  `pages` 对非演示文稿显式 warning，不假装有页码。

### Media improvements

- `read_image` 的 tesseract OCR 改为 TSV 解析，返回 text、word count、
  mean/min/max confidence；缺失 tesseract 仍显式 warning。
- `read_pdf` 用 `lopdf` 读取 metadata 和真实 page count，返回
  `page_count_source`（`pdf-metadata` / `text-boundaries` / `unknown`）；
  page range 仍只在真实 form-feed 页边界存在时生效，否则 warning。
- 修复共享 `truncate` helper 的 UTF-8 边界截断，避免截断多字节字符 panic。

### 边界

- Office 提取保留可读的段落/表格/slide/sheet 结构，不重建页面布局、栏、
  脚注、批注、浮动文本框、speaker notes、动画、图片、chart、宏、公式或
  日期/数字格式。
- 加密文档拒绝；超大文档需要外部转换。
- Office 输出仍受 40 KB tool-content cap 限制；structured data 受 32 KiB
  event cap 限制。
- 这一轮不包含 browser/ComputerLease；同一日期后续切片已实现，见上节。

---

## 2026-11-10 Optional LSP, provider search, document readers, runtime tools

这一轮补齐上一轮留下的高价值工具缺口，同时把“不能可靠实现的东西”明确
标成未实现，而不是交付半成品。

### LSP（可选，不自动安装）

- 新增 `lsp_query`：definition / references / hover / symbols /
  workspace_symbols / diagnostics。
- 发现 `rust-analyzer`、`sourcekit-lsp`、`typescript-language-server`、
  `pyright-langserver`/`pylsp`、`gopls`、`clangd`、`jdtls`、
  `kotlin-language-server`；缺少 server 时返回 `LSP_UNAVAILABLE`。
- server 在 shell sandbox 下启动，按 worktree/language 缓存；支持
  `workspace/configuration`、`workspace/workspaceFolders` 等 server 请求；
  idle 15 分钟或 worktree 删除时终止。
- document 在每次查询前 didOpen/didChange 同步；pull diagnostics 不支持时
  回退到 `textDocument/publishDiagnostics` 缓存。
- 位置使用 1-based line/column，内部转换为 LSP 的 0-based UTF-16 position。

### Web search（显式 provider）

- 新增 `web_search`：SearXNG、Brave、Tavily，按环境变量显式配置，不做隐藏
  routing，不伪造结果。
- `VERLINPARK_SEARCH_PROVIDER`、`VERLINPARK_SEARCH_ENDPOINT`、
  `VERLINPARK_SEARCH_API_KEY`、`VERLINPARK_SEARCH_LANGUAGE`、
  `VERLINPARK_SEARCH_SAFE`、`VERLINPARK_SEARCH_TIMEOUT_SECS`。
- 结果归一化为 title/url/snippet/source/published/rank/citation_id；TTL 5
  分钟缓存；snippet 只作为线索，阅读正文仍走 `fetch_url`。
- shell policy 拒绝 network 时直接 `NETWORK_DENIED`。

### PDF / image / structured data

- `read_pdf`：有界 PDF 文本提取，64 MiB 输入、2 MiB 文本、可选 page range；
  有 form-feed 页边界时按页输出，没有时明确 warning，不假装精确页数。
- `read_image`：sandboxed `sips` 读取宽高/格式/哈希；`ocr=true` 且本机有
  `tesseract` 时才 OCR，缺失时 warning + metadata，不伪造 OCR 文本。
- `data_query`：JSON/YAML/TOML/CSV，确定性 path 子集（key/index/wildcard/
  recursive descent），支持 offset/limit/columns/pretty；不执行任意代码。

### Runtime introspection / control

- `budget_status`：session/task/department/project 的 token/USD/time 已用、
  剩余、reserved、unlimited 状态。
- `task_detail`：acceptance/deps/write_scope/attempts/status/budgets/usage/
  changed paths/current task。
- `task_control`：Leader/Manager 对 TeamPlan task 执行 event-backed
  retry/skip/cancel/pause/resume。
- `tool_search`：搜索 built-in 与 MCP 工具，MCP 结果返回 server/tool 对，
  可通过 generic `mcp action=call` 调用；Manager/Verifier 只能 inspect。
- `publish_artifact_file`：把 worktree 里的截图/日志/报告发布为 content-
  addressed artifact（5 MiB 上限，复用现有 retention GC）。

### Browser spike（历史；已被 WKWebView helper 取代）

尝试了 headless Chrome + CDP（open/snapshot/click/type/press/select/wait/
screenshot/console/evaluate）。结论是：Chrome 的 network service 和嵌套
sandbox 在 daemon 的 Seatbelt 写沙箱内无法可靠加载本地 HTTP 页面；关闭
Chrome 自身 sandbox 并扩大 temp/crashpad 写权限仍只能加载 data URL。
直接 unsandboxed 运行会违反 worktree 写边界，因此这一轮没有注册 browser
工具。后续切片改用 daemon 启动的 WKWebView helper，并配套 ComputerLease；
见本文顶部的 Browser helper 章节和 ADR-0137。

### 测试

- 新增 LSP framing/UTF-16/symbol mapping、SearXNG mock、PDF page selection、
  image metadata（sandboxed sips）、data query path/CSV、runtime budget/
  tool_search/artifact 事件测试。
- 当前 Rust 测试 490（core 402 + 1 ignored / protocol 2 / store 72 /
  providers 14；另有 3 个 live ignored），Swift 110；`just lint`、
  `just test`、`just bundle` 通过。

剩余：native macOS computer、优先级/aging 的 ComputerLease 队列、跨进程
distributed lease，以及 browser 的 network-isolation 和 native AX 增强。
见 [TOOLING_ROADMAP.md](TOOLING_ROADMAP.md)。

---

## 2026-11-10 Deterministic agent tooling wave

这一轮把 Agent 的“手和眼睛”从 7 个基础工具扩展成一套端侧确定性的工程工具。
目标不是增加工具数量，而是让模型少猜命令、少拼 shell、少做 read/grep/parse
循环；所有写入仍然受 worktree jail、Seatbelt、approval、Verifier 和预算约束。

### 结构化结果与安全写入

- `ToolOutput` 增加可选 `structured` JSON；provider 仍收到有界文本，事件流
  增加结构化结果；事件持久化上限 32 KiB，超出时降级为摘要。
- `write_file` / `edit_file` 改为同目录临时文件 + fsync + rename 原子写；
  支持 `expected_sha256`、`dry_run`、bounded diff，歧义替换默认拒绝。
- 新增 `apply_patch`：解析 unified diff，先校验全部路径/hunk/hash 再写；
  任一文件失败会回滚已写文件；支持 create/delete/dry-run/expected hash。
- 新增 `file_manage`：mkdir/move/copy/delete，dry-run、hash 校验、递归删除
  显式确认；所有路径仍走 canonical worktree jail。
- 新增 `find_files`、`file_info`、`read_files`：递归 glob、hash/MIME/行数/
  symlink/Git 状态、批量区间读取；避免 shell `find`/`cat` 拼接。

### 项目识别与任务执行

- 新增 `project_profile`：检测 Rust/Cargo、Swift/SwiftPM/Xcode、Node/npm/
  pnpm/yarn/bun、Python/uv/poetry/pytest/ruff/mypy、Go、CMake、Gradle/Maven；
  justfile/Makefile recipe 可覆盖默认命令；每条命令带 source/confidence。
- 新增 `run_project_task`：按 profile 运行 build/test/lint/format/typecheck/
  check/custom；支持 package、filter、changed_only、timeout、dry_run；在现有
  shell sandbox 内执行；解析 cargo/swift/xcodebuild/pytest/go/jest 输出为
  `failures[]`（path/line/column/test/message），大输出保留 bounded tail。
- 新增 `affected_tests`：从 changed paths 或 Git base ref 映射测试文件；
  Rust integration tests、Python、Go、Swift、TS/JS 有确定性规则；结果明确
  标记 lexical confidence，找不到时回退完整测试套件。
- 新增只读 `git`：status/diff/log/show/blame/files；不提供 stage/commit/
  branch 写操作，提交与集成分支仍由 runtime/merge queue 拥有。

### 代码智能与上下文

- 新增 bounded lexical code index（缓存 20s，写入/shell 后 eager invalidate）：
  `repo_map`、`find_symbol`、`find_references`、`dependency_graph`、
  `build_context`；所有结果标记 `confidence: "lexical"`，不冒充 LSP。
- `code_diagnostics` 运行检测到的 check/lint 命令并返回结构化诊断；LSP
  backend 尚未实现，显式返回 `LSP_UNAVAILABLE`。
- `build_context` 按 goal 组装 symbol/file/test/profile/git 上下文包，带
  citations 和 token budget；不调用模型、不做 embedding。

### 并行与长驻服务

- 一轮只包含只读 registry 工具时，runtime 以最多 4 个并发执行并保持结果
  顺序；shell、网络、写入、approval、delegation、skill activation 仍串行。
- 新增 `process`：start/list/status/read/write/signal/wait/stop；sandboxed
  stdio、每流 2 MiB ring buffer、每 worktree 最多 8 个、默认 1h idle TTL。
  v1 明确不跨 daemon restart 持久化。
- 新增 `wait_for`：本地 TCP port、localhost URL、process log regex；不再靠
  `sleep` 猜服务是否就绪。
- 新增 `local_http`：仅 localhost/127.0.0.1/[::1]，支持 headers/JSON body/
  local redirects；GET/HEAD 只读，其他方法走 workspace-write risk；secret
  headers 不回声；不注入 ambient credentials。
- `fetch_url` 升级：最多 5 次 redirect，每跳重新做 SSRF/DNS 校验；HTML 走
  readability 文本提取；返回 final_url/title/links/hash；二进制内容显式拒绝。

### 权限与测试

- 新工具加入 role/profile matrix：Manager 保持 code read-only；Verifier 获得
  只读代码智能 + run_project_task/code_diagnostics；coding Worker/Labour 获得
  写、patch、project task、git read、code intel、process、local HTTP；research
  获得只读 code intel + localhost HTTP；generic 保持只读。
- 新增 Rust 单测覆盖 patch 回滚/hash/CRLF/path escape、find/file info/read
  files/file manage、project detection、task parser、git query、code index、
  process lifecycle、local HTTP、并行 read 事件与 structured tool event。
- 上一轮 Rust 测试 451 个（core 365 / protocol 2 / store 70 / providers 14；3 个 live 测试 ignored）；新工具不改变 approval、
  sandbox、write scope、Verifier、capacity、budget 语义。

剩余未实现：web_search、LSP backend、PDF/image/office 读取、
publish_artifact_file、budget_status/task_detail/tool_search、task_control、
data_query，以及 browser/computer GUI 工具。详见
[TOOLING_ROADMAP.md](TOOLING_ROADMAP.md)。

---

## 2026-11-10 Project knowledge sources, citations, skill packs, artifact versions

这一轮把“可检索的项目知识与可复用的工作方式”接入同一个本地 runtime，并给
artifact 增加版本和幂等发布。参考了 SourceWeft 的能力边界和 artifact 安全
设计，但没有复制其实现或引入其运行时。

### Project knowledge sources

- 新增项目级 `sources`：`note` / `file` / `url`，带 collection、labels、
  status、size、SHA-256、revision、chunk/token counts、URI、MIME。
- 原文快照写入 `data_dir/sources/<project>/<source>.json`；SQLite 保存可查询
  投影和 chunk 索引；`vp db-rebuild` 从快照重新 chunk 并恢复 FTS。
- 本地文件只允许项目根内相对路径、canonicalize 后仍在根内、拒绝 `.git`/
  `..`/symlink/非 UTF-8/NUL/大于 2 MiB；URL 复用 DNS 固定、私网阻断、无
  redirect 的受控 fetch，并做 bounded HTML→text。
- 文件夹 import 只处理文本类扩展名，最多 500 个文件，跳过 binary/超大/空
  文件，已有 URI 不重复导入。
- 新增 `source.list/get/add_note/add_file/add_url/import/delete/reindex/
  refresh/search` RPC、`vp sources/source-*` CLI、Preview Sources 面板。
- `source.refresh` 重新读取 file/URL 或从 snapshot 重建 note，revision +1；
  `source.reindex` 从 snapshot 重建 chunks；搜索结果和 UI 均可直接定位 chunk。

### Deterministic retrieval + citations

- `vp-core::sources` 做 heading-aware chunking：Markdown heading 栈、fenced
  code 保护、target/max/overlap、字符边界、chunk 上限 500；chunk 偏移、
  heading path、token 估算与稳定 id。
- `source_chunks_fts` 是 FTS5 external-content 索引；查询先 all terms 再 any
  terms，CJK/短查询/FTS miss 走 bounded LIKE fallback，按字面出现次数和
  heading 命中排序。默认 8、hard cap 50。
- Agent 工具：`search_sources` / `read_source` / `list_sources`（只读）。
  System prompt 注入 bounded source catalog；完整内容按需读取。
- 每次 agent run 一个 `CitationRegistry`；第一次使用的 chunk 分配 `c1..cN`，
  重复使用复用 key。Tool 输出 `[cN]`，system prompt 要求精确引用。
- `agent.message` payload 持久化 citations（source/chunk/heading/excerpt/score/
  origin）；`session.citations` 直接从事件派生，避免第二投影漂移；
  `vp citations` 和 Preview Sources → Citations 可检查。
- Stage assistant bubble 渲染 inline `[cN]` link 和 source strip；citation
  sheet 显示快照 excerpt，即使 source 后续刷新/删除仍可审计。

### Project skill packs

- 项目级 `skills`：name/slug/description/when_to_use/version/instructions/
  tool_allow/tool_deny/profiles/roles/enabled/source/sha256。
- `SKILL.md` import 支持 YAML frontmatter（name/description/when_to_use/version/
  tools/profiles/roles），128 KiB 文件上限；也支持 Settings 手动创建/编辑。
- `skill.list/create/update/import/set_enabled/delete` RPC、`vp skills/skill-*`
  CLI、Settings → Skills 面板；skill 进入 project manifest 与 `db-rebuild`。
- System prompt 只注入匹配 role/profile 的 enabled skill catalog（最多 24 条
  / 4,000 chars）；`use_skill(name)` 才加载完整 instructions。
- 一次只激活一个 skill；激活后下一轮 tool schema 过滤，且每次 tool execution
  立即检查 allow/deny。Deny 永远优先，allow 只能收窄，不能授予角色没有的
  工具；approval/sandbox/budget/write scope 不变。`use_skill` 始终可用以便切换。
- Stage composer 的 wand 菜单可插入 skill 提示；agent 仍通过 `use_skill` 激活。

### Artifact versions + idempotent publication

- `artifacts.version` 从 1 开始，新增 `artifact_versions` 版本行；inline
  artifact 更新支持 `artifact_id` + `expected_version` 乐观并发保护，
  写 `artifact.version_published` 事件并保留旧版本。
- `artifact.restore` 把旧 inline 版本恢复为新版本，写
  `artifact.version_restored` / `restored_from_version`，不重写历史。
- `publish_artifact` 支持 session-scoped `request_key`；重复 key 返回第一次
  提交的 artifact，`artifact_requests` 保存 winner，避免 retry 产生重复。
- 文件 artifact 仍不可变（blob 与内容寻址 GC 边界），这是明确的限制。
- Preview artifact card 显示 `vN`、版本历史 sheet、当前标记和 Restore；
  CLI 增加 `artifact-versions` / `artifact-restore`。

### Collections, bounded diff, citation audit, skill usage

- Source 增加 nullable collection 与 labels；`source.collections` 聚合每个
  collection 的 source/chunk/byte/token/label，`source.update` 一次更新
  title/collection/labels 并同步 durable snapshot；`source.search` 与
  `search_sources` 支持 collection/label filter，FTS 和 literal fallback 使用
  同一套过滤；CJK/mixed-script 查询会把 FTS 与 literal 结果融合，而不是直接
  放弃一侧。
- 新增 `vp-core::textdiff`：CRLF/CR 归一化、bounded Myers O(ND)（edit cap
  512）、输出行 cap（默认 4,000 / 硬上限 20,000）、超限时显式
  delete-all/insert-all fallback；hunk 带 old/new 行号和 context，summary
  计数与 truncation 标记不因渲染截断而丢失。
- `artifact.diff` 比较两个 inline version，或某个 version 与 current；
  文件 artifact 明确拒绝（内容寻址 blob/GC 边界）。`vp artifact-diff` 输出
  unified-style diff，Preview 版本历史增加 Diff sheet，按行显示旧/新行号、
  add/delete/context 配色和 truncated 提示。
- `session.citation_audit` 从 `agent.message` 事件派生 cited/uncited message、
  total citation、distinct source、dangling `[cN]` marker、unused citation；
  `vp citation-audit` 与 Preview Sources → Citations 展示审计条。它是审阅
  辅助，不声称语义事实核查。
- `use_skill` 成功激活时写 `skill.activated` event（name/slug/version/role/
  profile/tool policy/task）；`skill.activations` 从事件派生当前 session 的
  激活历史，Settings → Skills 显示最近激活。
- 修复 context limit 的双重扣减：`context_limits` 只预留 output + 1,024
  safety；`estimated` 已包含 system/tool tokens。手动压缩且摘要失败时 trim
  到 hard limit 的一半，确保显式操作仍创造 headroom。
- Rust 新增 textdiff / collection / scoped search / artifact diff / citation
  audit / skill activation 回归；Swift 新增 collection/activation/diff/audit
  decode 与 UI 状态。

### Unlimited session budget mode

- `Session.unlimited`（默认 false）成为显式、持久、事件驱动的预算模式；创建时
  `unlimited: true` 不允许同时声明 token/cost/time 数值，避免静默忽略。
- `session.budget.set` 可在运行中切换；engine 为每个 session 保存共享
  `Arc<AtomicBool>`，运行中的 `SessionBudget` 在下一个 turn/tool 边界看到变化，
  daemon 重启后以持久字段为准。
- unlimited 绕过 session token/cost/time、TeamPlan task/department token/cost、
  project shared pool、pre-flight reservation 与 budget stop；不绕过 capacity、
  approvals、sandbox、Verifier、context window、max turns、provider timeout、
  pause/cancel。数字预算会保留，关闭 unlimited 后恢复原来的限额。
- 创建事件写 `unlimited` / `budget_mode`；切换写
  `session.budget_mode_changed`，`session.replay`/repair/rebuild 与 bundle
  export/import 都覆盖该字段。
- UI：New Session 的 `Unlimited budget` 开关（开启时禁用三个预算输入并提示
  bypass 边界）、Settings → Providers → Session defaults 默认开关、Stage
  session menu 切换、Stage 用量显示 `unlimited budget`。
- CLI：`vp session-new --unlimited`、`vp session-unlimited <session>
  [--enabled false]`。
- 新增 `SessionBudget` 共享 flag 单测、零 headroom 会话端到端测试、store
  round-trip/event 测试、replay/repair 与 bundle 测试、Swift decode 测试。

### 验证

- 新增 Rust 测试：source chunking/HTML/query、source store/search/snapshot、
  source file traversal/import/refresh、citation event→`session.citations`、
  skill frontmatter/CRUD/catalog/tool narrowing、artifact version/conflict/
  restore/request-key、rebuild 恢复 source/skill。
- 新增 Swift 测试：Source/Skill/Citation decode、transcript citation attach、
  settings section count、artifact/citation UI 兼容。
- 当前测试：Rust 419（core 333 / protocol 2 / store 70 / providers 14；另有
  3 ignored live），Swift 110；以最终 `just test` 输出为准。
- 文档：ADR-0129（sources/citations）、ADR-0130（skills）、ADR-0131
  （artifact versions/idempotency）、ADR-0132（bounded diff/citation audit/
  source collections/context-limit fix）、ADR-0133（unlimited budget mode）。

---

## 2026-11-10 Standalone task inbox + autonomous dispatch + workspace search

这一轮补齐了“没有 TeamPlan 的 orphan task”路径，并把 Preview 文件面从
“打开时遍历整个 worktree”改成按目录懒加载、带内容搜索和行号跳转的可靠浏览器。
所有新增调度都是确定性的，不引入隐藏模型路由。

### Task inbox / autopilot

- 新增项目级 `intake_tasks`：title、instructions、profile、kind、priority、
  write scope、acceptance、intake 依赖、token/USD/time budgets、labels、
  origin、attempts/max attempts、next_attempt_at、linked session/task、
  last_error、result summary。SQLite schema 带增量迁移，并进入 project
  manifest / `vp db-rebuild` 恢复路径。
- 新增 `intake.list/create/update/dispatch/cancel/retry/delete` RPC 与
  `vp intake*` CLI；`intake.update` 只允许非 active task，保存后重新排队并把
  attempts 归零。
- 每个 intake item 派发时合成并复用正式校验的 one-department/one-task
  TeamPlan（任务 id `T1`），然后走正常 session/worktree、Worker、Verifier、
  merge queue、write lease 与预算路径。不是第二套执行引擎。
- `TaskSchedulerPolicy` 改为 default-on：计划 resume、intake include、
  `max_intake_parallel`（1–10，默认 2）、cooldown；显式 DB policy 可关闭，
  `VERLINPARK_TASK_SCHEDULER=0` 是全局 opt-out。手动 Run now 可越过开关和
  并发 cap，但仍受依赖、预算、容量、write lease、Verifier 约束。
- Intake 依赖是项目级 DAG：只允许同项目已有 task，最多 20 个；校验缺失、
  自依赖和 edit 形成的环；只有依赖全部 completed/skipped 才可自动或手动派发；
  依赖被 cancel 或 final failure 时 queued dependents 进入 blocked，retry 后
  重新排队，仍有非终态 dependent 时拒绝删除依赖。
- 调度顺序：task priority + age + orchestration retry boost，再按 enqueue
  time/id，平局优先不同 project 的 round-robin。`scheduler.status` /
  `vp scheduler` / Manage Dash 展示 session cap、plan/intake occupancy 与
  每项目有效 policy，只读、不产生事件。
- `reconcile_intake_tasks` 每个 tick 和启动时把 session/plan task 终态映射回
  intake；失败按 orchestration backoff 写 `next_attempt_at`，绝不越过
  `max_attempts`。daemon restart 把 dispatching/running intake 标为 failed，
  `db-rebuild` 也做同样降级，避免悬挂 running。
- Manage Dash → Team & Tasks 新增 Task Inbox 卡片：创建/编辑、依赖选择、
  run/cancel/retry/delete、打开 session、轮询活动状态；Task Scheduler 卡片
  显示实时 occupancy 与 intake 并发控制。

### Workspace browser / content search

- `workspace.list` 单目录分页（默认 500 / 上限 2,000；20,000 entry 安全
  cap），只返回 dirs-first 的一层；拒绝 absolute/`..`/symlink/`.git`。
  Swift `WorkspaceDirectoryState` 只在展开目录时加载，折叠保留缓存，Refresh
  清空；根目录按 session 加载一次。
- `workspace.grep` 是 bounded literal search：1–200 字符、可选相对目录、
  默认 case-insensitive、跳过 `.git`/symlink/>2 MiB/NUL/非 UTF-8、默认
  200/hard cap 1,000，返回 `files_scanned`/`truncated`。不编译 regex。
- Files panel 增加 Names/Contents 切换、按文件分组的结果、行号/preview；
  点击匹配打开文件并跳到对应行。两个模式都 220ms debounce、取消感知。
- 新增 `WorkspaceCodeEditor`（NSTextView + NSScrollView + 行号 ruler），
  保持 SwiftUI binding、editable/read-only、find、按 reveal token 滚动/选中；
  ruler 只画可见行片段，wrapped line 不重复编号。
- 新增 `VPCloseButton`（粗体 X、可见圆形表面、24–28pt hit target、hover
  放大、accessibility label），替换 Files tab 和 Collab inspector 的小关闭
  按钮；tab strip 仍保留未保存改动确认。

### 验证

- Rust：新增 scheduler/intake/store/agent/grep 测试，覆盖 atomic claim、
  attempts 上限、cancel/delete/reconcile、依赖 gate/cycle、默认 policy、
  manual dispatch、end-to-end Worker+Verifier、scheduler.status 与 grep
  的路径/大小写/二进制/cap 行为。
- Swift：新增 intake/scheduler/grep decode 兼容测试；`xcodebuild test` 通过（107 tests）。
- 当前总量：Rust 381（core 304 / protocol 2 / store 61 / providers 14；另有 3 ignored live），Swift 107。
- 文档：ADR-0127（task inbox/autopilot）与 ADR-0128（lazy browser/search）。

---

## 2026-11-09 多智能体编排硬化切片

这一轮把“多 Agent 协作”从事件展示推进到可配置、可恢复、可审计、可操作的编排层，重点不是增加一个模型，而是让 runtime 对团队状态负责。

### 编排控制器

- 项目级 `OrchestrationPolicy`：自动重试、最大尝试次数、指数退避、重试优先级提升、依赖失败策略（block/cancel/retry）、耗尽后升级、自动解除 blocker。
- `plan_tasks.next_attempt_at` 持久化重试门；`plan_task.retry_scheduled` / `blocked` / `unblocked` / `cancelled` 事件进入 task 投影、replay、repair 和 bundle/db-rebuild。
- 确定性控制器动作：retry / block / unblock / cancel / escalate / resolve_blockers；每个动作写 `orchestration.decision`，不调用模型、不做模型路由。
- 任务完成时立即运行控制器；daemon tick（`VERLINPARK_ORCHESTRATION_TICK_SECS`，默认 20s）补跑重启后到期重试和失败/阻塞 session。
- session token/USD 预算耗尽时抑制重试并升级，避免用用户已耗尽的预算继续循环。
- `orchestration.preview` 返回与 `advance` 相同的动作但不落库；Collab Ops 先预览再显式 Apply。
- `orchestration.overview` 聚合最近 TeamPlan session 的 attention/running/blocked/failed/retrying/merge-conflict 指标，供 Manage Dash Mission Control。
- CLI：`vp orchestrate <session> [--dry-run]`、`vp orchestration-preview`（通过 `orchestration.preview`）、`vp orchestration-overview`、`vp orchestration-policy[-set]`。

### 持久协调与计划修订

- `coordination_notes` / `orchestration_decisions` SQLite 投影：事件 + 投影原子写入；长 session 不再因 5,000 event 窗口丢失 blocker/decision。
- `unblock` 按 note id 或 task id 解析，`resolved_at` 使用事件时间，replay 可逐字段比较。
- 定向 coordination note 会进入目标 agent 的 durable mailbox；Plan Worker brief 包含：
  - 直接发给它的 note；
  - 自己 task 的 note；
  - 最近的共享 handoff/decision/escalation；
  - 未读 mailbox 摘要（读取时标记 read）。
- `team_plan.revision` 返回历史 revision 的完整 plan；`team_plan.restore` 以新 revision 恢复历史 plan；`team_plan.task_edit` 只改一个 task，并复用 active/completed task 保护。
- `SessionReplayReport` 增加 `notes_match` / `decisions_match`；`session.replay --repair` 和 bundle import 从事件重建两张投影。
- Collab Ops 增加 Coordination Inbox、Plan Revision 详情/恢复、Task Editor、blocker resolve/reply、preview/advance 与 policy editor。

### 运行时租约

- `runtime_leases`（lease_key/owner/kind/session/heartbeat/expiry）跨进程 SQLite 租约。
- Leader prompt run 和 plan-run（scheduler/resume）都获取 `session:{id}` 租约（TTL 180s，TTL/3 心跳）；释放写 `session.lease.released`，崩溃后 TTL 过期可接管。
- `runtime.leases` / `runtime.leases.sweep` / `vp leases` / `vp leases-sweep` / Manage Dash Runtime leases 卡片。
- 集成阶段额外持有 `integration:{session}` 租约；`process_merge_entry` 在 merge 前记录 `pre_merge_commit`，daemon 重启时对 clean worktree 自动 reset 到该 commit（`merge_queue.recovered`），dirty worktree 只记录 `recovery_skipped`，不覆盖用户改动。Live E2E 已确认 merge queue 返回非空 `pre_merge_commit`，merge 后 `runtime.leases` 清空。
- 现有 data-dir 实例锁仍保留；租约不冒充分布式 fencing。

### 测试与验证

- Rust：新增 orchestration 纯函数/控制器测试、store 投影测试、Engine 控制循环/preview/overview/lease/revision/replay-repair 测试；总计 358 active（core 286，protocol 2，store 56，providers 14；3 ignored live）。
- Swift：新增 snapshot/preview/lease/overview/revision 解码、graph orchestration 状态、policy、task filter 测试；总计 104。
- Live DeepSeek E2E（2026-11-09）：provider probe 成功；单 Leader 写文件成功；真 TeamPlan（1 Manager + 2 Workers + 2 Verifiers）并行完成 T1/T2，两个 commit 经 Verifier 后进入 merge queue 并以 `--diff-check` 合并到 session branch；`orchestration.preview` 对完成 session 返回空动作；对 completed task 的 `task_edit` 被安全拒绝；`coordination.note` blocker/unblock、mailbox、`team_plan.restore`、`replay`/`repair` 均实测通过。

### 仍未实现

- Manager review 不是强制 gate；verifier gate 仍然是合并前唯一必需评审。
- 自动重试只覆盖 failed task；不自动创建新 plan task，也不自动重新规划。
- Runtime lease 不是完整分布式调度/fencing；daemon 仍假设一个 data-dir 一个实例。
- 没有 GUI/browser agent 工具，`ComputerLease` 仍未实现。
- 外部 ACP agent 仍是受信任二进制；不提供 credential/network sandbox。

## 2026-11-09 体验、上下文与权限修订

这一轮处理真实使用反馈，不再只增加编排能力。

### Collab / Preview / Manage Dash
- Collab 顶部 Graph/Ops 改为整行分段控件，不再出现 “G…”/“O…”。
- 图页底部新增实时 Live strip：运行中的 Agent、最近 tool/coordination/task/merge 活动与相对时间。
- Ops 改为 Overview / Board / Inbox / History 四个页签；主命令只保留 Run next、Preview、Coordinate 和 More 菜单，减少首屏卡片与按钮。
- 缩放默认 100%，每次 5%，按 5% 吸附；点击百分比立即回到 100%。宽屏 sheet 同样处理。
- Preview 默认 Showcase（Agent 成果白板），展示 published artifacts；Changes/Files/Plan 为主分类，History/Queue/Mailbox/Cost/Console/Terminal/Trace 收进 More。
- Files 面板改为递归文件夹树 + 浏览器式文件标签页；每个标签独立 draft、dirty、save/reload、关闭确认。文件夹默认折叠（chevron 可展开/收起，支持 Expand all / Collapse all），只渲染当前展开分支，避免一次渲染整个 worktree；筛选时自动展开匹配路径。
- Manage Dash 默认只显示 Overview / Inbox / Team & Tasks / Usage；其余高级段通过 “Advanced sections” 开关显示。

### Provider / 首次使用
- Providers 首屏改为预设优先：DeepSeek / OpenAI / Anthropic / LM Studio / Ollama / Custom；自动填 base URL、model、context window。
- 常用模型族自动推断 context；API key 是远程 provider 唯一必填项。
- Role overrides、team/global capacity、base URL、context override、session defaults 收入 Advanced。
- 新增 first-run Setup Wizard：连接模型 → 选择项目 → 完成；可 Skip，可从 Settings → General 重新打开。
- 新安装默认是 smart approvals、Verifier gate、确定性 Integrator、默认团队 cap 和 200k session budget，不要求先配置高级项。

### 权限与对话
- 修复 `$()` / backtick 命令替换被一律判为 host_state 的误报：内部命令递归分类，常见 `$(cat file)`、`$(git rev-parse HEAD)` 不再触发审批；危险内部命令仍升级。
- 新增项目级 `approval_grants`（tool + path prefix + include_children）；审批卡支持 “This file / This folder and subfolders”，Settings 可查看/删除。
- smart 模式仍只对 host/destructive 请求人工；workspace 写和常规构建/测试不打断。
- Transcript 不再记录 routine tool（read/write/grep/todo/fetch/remember/recall/coordinate/team_status/mailbox/artifact）成功调用；错误仍显示，完整审计仍在事件流。
- ACP permission 也检查 approval grant。

### 上下文与长期记忆
- 角色化 context 策略：Leader/Manager 使用 summary + rolling window；Worker/Labour/Verifier 使用 provider-free rolling window。
- compaction 对所有角色运行，不再只覆盖 top-level；hard limit 压力下保留窗口可降到 2 条消息，summary 保留。
- 手动 compaction 在 summary provider 失败时回退 trim，不再留下 “already running” 状态。
- 完成 plan task 自动写入 `task_result` project memory（按 title 幂等）；system prompt 自动注入相关长期记忆，Plan Worker 额外获得 task-specific memory brief。
- 记忆仍通过 SQLite/event 持久化，受项目 memory retention 策略管理。

### 协作倾向
- Leader prompt 明确：非平凡、多文件、多步骤或需要验证的工作优先提交 TeamPlan 并 run_team_plan；只有真正小任务或用户明确要求不用团队时才直接执行。

## 当前可用的能力

| 能力 | 状态 | 说明 |
|---|---|---|
| Leader Agent | ✅ | 流式对话、工具调用、审批、session 预算、取消、todo 计划 |
| Context usage | ✅ | 每次 top-level provider call 前写 `context.usage`（estimated/window/threshold/hard limit/messages/calibration）；provider 报告 usage 后写 `context.measured`（heuristic estimate vs actual input/output/raw ratio/calibration/samples/model）；upward-only EWMA calibration（1.0–2.0，按 model/session 持久化）用于后续 context 估算、compaction 判断和 summary transcript budget；`session.context` / `vp context` 返回 usage + last compaction + last measurement；Stage header 显示 `ctx used/threshold`，超 threshold 变橙色；点击打开 context popover（window/threshold/hard limit/messages/estimate-vs-actual/最近 compaction/手动 compact）；来源为事件，replay/CLI 可查 |
| Context compaction | ✅ | 按模型 context window 自适应：`VERLINPARK_CONTEXT_WINDOW` 默认 128k（0 关闭），`RoleModelConfig.context_window` 可按角色覆盖；hard limit = window - output - system/tools - 512，summary threshold = 75% hard limit，`VERLINPARK_COMPACT_AFTER_TOKENS` 可显式覆盖；threshold/hard-limit 比较计入 system/tool tokens（与 provider request 一致），并应用 provider usage calibration；每个 top-level turn 前检查；context 估算按 4 ASCII chars/token + 1.5 non-ASCII tokens/char，summary transcript 也受模型窗口约束；summary + retained tail 为主，失败/仍超 hard limit 时按 user/assistant 安全边界滚动裁剪；`context.compacted` 记录 `mode: summary|trim` + `through_message_id`，原消息保留；usage 记为 `compaction`；`session.compact` / `vp compact` / popover 可对 idle/failed/cancelled/landed session 手动强制 compaction（同一 summary/trim pipeline，运行中/暂停/等待输入拒绝）；不做 compaction checkpoint/resume；session checkpoint/fork 见 ADR-0101 |
| Provider 流可靠性 | ✅ | 启动前 transient retry；无任何输出前最多 3 次 stream attempts（含首次）；partial failure/cancel 保留 `partial: true` message 并结算 provider usage 或估算 usage；partial 后不 replay；`agent.probe` 返回 chunks/first-token/done；真实 OpenAI/Anthropic/local live tests 默认 ignored |
| Session worktree | ✅ | 每个 session 独立 Git worktree + 分支；项目根只读 |
| Session checkpoint / fork | ✅ | `session.checkpoint` / `vp checkpoint` 在预期 branch + 非运行 session 上记录 snapshot commit、dirty flag、active conversation 的 summary/start/through message ids、message count、label；dirty worktree 通过临时 index 把 tracked + untracked（非 ignored）变更写成一个 checkpoint commit（`refs/verlinpark/checkpoints/*` 保持可达），不移动 source HEAD/index；`session.fork` / `vp fork` 从 checkpoint commit 创建新 session/worktree/branch，复制 source budgets/priority 和 active conversation（summary + tail），重建 assistant/tool display events，seed Leader，写 `session.forked` lineage；不复制 plan/todos/merge queue/approvals/usage，source 不变；Stage「Fork Session…」可选择当前 worktree snapshot 或历史 checkpoint（dirty checkpoint 会标记）；`session.checkpoint.delete` / `vp checkpoint-delete` 用 tombstone 隐藏并删除 snapshot ref；`session.checkpoint.prune` / `vp checkpoint-prune` 支持 `older_than_days` / `keep_latest` / dry-run；Manage Dash 列出并删除/prune；项目级 `CheckpointRetentionPolicy`（默认关闭、30 天、keep 5，最多 365 天/100 个）+ daemon sweep（`VERLINPARK_CHECKPOINT_SWEEP_SECS` 默认 3600，0 关闭）自动 prune；`session.lineage` / `vp lineage` 返回 parent/siblings/children；`session.compare` / `vp compare` 比较同项目两个 session branch 的 committed diff（numstat + patch cap 200k chars）；`session.fork.merge` / `vp fork-merge` 可将 direct child fork branch `--no-ff` merge 回 clean source，冲突 abort 并返回 paths，diff check/可选 integration command 失败 reset hard；`session.fork.tree` / `vp fork-tree` 返回 depth ≤32、nodes ≤200 的递归 fork tree；`project.fork.forest` / `vp fork-forest` 从 session.created parent metadata 构建 project forest（depth 32 / nodes 500 cap、roots/total/checkpoints）；`project.fork.audit` / `vp fork-audit` 检查 missing parent/link/child/checkpoint commit/ref/tombstone ref，`project.fork.repair` 回填 missing fork link、重建/删除 checkpoint ref；`session.checkpoint.stats` / `vp checkpoint-stats` 报告 checkpoint/ref/commit bytes/unique blob bytes；`session.checkpoint.diff` / `vp checkpoint-diff` 预览 current HEAD 到 checkpoint 的 tracked numstat + capped patch + clean flag；`session.checkpoint.restore` / `vp checkpoint-restore` 在 session idle/clean 且 branch 匹配时把 worktree/branch `reset --hard` 到 checkpoint commit，记录 previous/restored commit，不恢复 conversation/plan；Manage Dash Fork lineage + Project fork forest 卡 + Preview/Restore/Delete 菜单；见 ADR-0101/0102/0103/0104/0105/0106/0108/0111 |
| 四层组织 | ✅ | Leader → Manager → Worker → Labour；严格链路，Manager 必须有部门 title |
| Agent profile | ✅ | coding / research / verification / generic；决定工具集与 prompt；Verifier 在 isolated detached clone 中运行 |
| ACP 外部 Agent | ✅ | `acp.agent.*` / `vp acp-*` / Settings ACP；Leader/Manager 用 `delegate_acp` 委派外部 stdio agent；Worker slot、task worktree、global capacity/write lease、Seatbelt 写沙箱 + scratch HOME、MCP 传递（stdio 总是传递，HTTP/SSE 仅在 agent capability 支持时传递）、permission→approval、timeout/cancel、Verifier/Integrator 全链路；probe 在隔离 scratch cwd 中初始化且不发送 prompt |
| TeamPlan | ✅ | Leader 定义 departments / Manager titles / profiles / task DAG / task priority（-100..100）；runtime 校验结构、依赖、优先级和 scope 语法并持久化 |
| 多智能体编排 | ✅ | 项目级 OrchestrationPolicy（重试/退避/依赖失败/升级/blocker 自动解除）；持久 `next_attempt_at` 重试门；确定性 controller（retry/block/unblock/cancel/escalate/resolve_blockers）；plan task 完成 + daemon tick 双入口；预算耗尽抑制重试；`orchestration.preview/advance/overview`；持久 coordination/decision 投影、定向 mailbox 交付、revision detail/restore、单 task 编辑；见 ADR-0121/0122 |
| Manager 圆桌 | ✅ | `deliberate_team_plan`，1–3 轮，只读，后一轮看到前一轮所有 peer positions；Collab 显示按 round 分组的 Manager 立场卡片 |
| 自动派发 | ✅ | `run_team_plan` 按依赖批次执行 ready tasks；ready task 按 priority desc + id 排序；同批筛选不重叠 scope；Manager 懒创建；失败阻塞依赖；batch abort 时先结算同批全部 task（cancelled/blocked），不留下 running 投影；`session.plan_resume` / `vp resume-plan` / Preview Resume plan 可跳过 completed task，重跑 unfinished task |
| Task worktree | ✅ | Worker 独立 task worktree/branch；Labour 继承 Worker worktree；resume 复用/重新 attach 已有 worktree 和 branch，base 使用 merge-base，已有 commit 可重新进入 Verifier/Integrator |
| 并行 Worker | ✅ | `delegate_many` / plan batch 并发；两边都校验 write scope 不重叠；空 scope 或重叠 scope 的 plan task 串行执行 |
| Verifier gate | ✅ | 独立 Verifier；`submit_verdict`；approve 才合并。Verifier 无 write/edit 工具，运行在 Worker commit 的 isolated detached clone 中；clone 内 tracked diff 仍会被检查并拒绝；审批卡会提示 |
| Merge queue | ✅ | Verifier-approved commit 入持久队列；`merge_queue.list` / `vp queue` / Preview → Queue；状态 queued/merging/checking/merged/conflict/failed/interrupted |
| Integrator gate | ✅ | 确定性 merge：`--no-ff` + `git diff --check` + 可选 `VERLINPARK_INTEGRATION_COMMAND`；失败 reset 到 merge 前 commit；冲突 abort 并保留 Worker 分支 |
| Queue retry | ✅ | `merge_queue.retry` / `vp retry` / Queue UI：conflict/failed 先 rebase 到当前 session branch，再重新 merge；attempt +1 |
| Conflict resolution | ✅ | `merge_queue.resolve` / `vp resolve` / Queue UI Auto-resolve：resolver agent 在 Worker worktree 解决 rebase 冲突；runtime 检查 markers、完成 rebase、独立 Verifier 复核、再走确定性 Integrator；失败回到 conflict |
| Auto-resolve policy | ✅ | 项目级 `IntegrationPolicy`：默认关闭；`max_auto_attempts` 0–5；`max_resolve_tokens` 1000–10M；daemon watcher 在 session idle / 启动时扫描 conflict，持久 attempt 计数 + per-resolution token cap；RPC/CLI/Manage Dash |
| Landing | ✅ | `session.land` / `vp land` / Preview → Land：clean + default branch 检查后 `--no-ff` 合入项目默认分支；不 push、不建 PR |
| PR flow | ✅ | `session.pr` / `vp pr` / Preview → Open PR / MR：preflight origin，GitHub 用 `gh auth status` + `gh pr create`，GitLab 用 `glab auth status` + `glab mr create`（origin 含 gitlab 自动检测，`VERLINPARK_PR_PROVIDER` 可覆盖）；push session branch，CLI 失败会明确报告 branch 已 push |
| Queue recovery | ✅ | daemon 重启把 queued/merging/checking entry 标为 interrupted 并补事件；interrupted 可 rebase+retry |
| Diff / commit | ✅ | base..worktree diff、显式 commit、discard、session committed 事件；Preview Changes 解析 unified diff，按文件选择并显示 +/- 行数 |
| Shell 写入沙箱 | ✅ | macOS Seatbelt（`sandbox-exec`）：只允许写 worktree、对应 Git 元数据、per-agent scratch 和 `/dev/null`；拒绝 common hooks/config 与 worktree `config.worktree`；`.git` 指向校验；scratch 承载 TMPDIR/构建缓存；env allowlist 不继承 provider/daemon secrets；常见 credential/state 路径 read-deny；timeout/cancel 杀整个 process group；项目级 `ShellPolicy` 可拒绝网络、追加 read-deny，并可 opt-in `restrict_home`（HOME 内容默认拒绝，只允许 worktree/scratch/Git 元数据、常见 toolchain/cache 与 `.gitconfig`/SSH config/known_hosts）或 `read_jail`（进一步拒绝 HOME parent 通常是 `/Users` 和 `/Volumes`，保留 worktree/scratch/Git/toolchain/系统路径）；HOME metadata 允许以支持路径遍历；sandbox 不可用时 fail closed；System 路径、`/private/tmp`、`/private/var` 等仍可读，network allow 仍可外发 |
| 预算 / 取消 / 暂停 | ✅ | session token reservation + provider usage 结算；TeamPlan 可声明 department/task token/USD budget 树；daemon-wide `BudgetRegistry` 在 turn 前原子 grant task+department 最紧 output budget（token + USD reservation，RAII 释放），并按 ledger 复查；task/department 超限只 block 该任务，session 超限仍失败；session 和 project pool 用量到 80% 时各发一次 `agent.budget_warning`；无 usage 端点使用 `estimated: true` 估算；Stop、CLI cancel、崩溃恢复；`session.pause` / `session.resume` / `vp pause` / `vp resume` 在 turn/tool 边界协作式暂停并继续同一 run |
| 审批 | ✅ | 写/Shell 审批；事件带发起 Agent；Chat Approve/Deny；Manage Dash “Approvals” 跨 project 汇总 pending、显示 session goal、可直接 Approve/Deny |
| 中途提问 | ✅ | `ask_user` tool（Leader/Manager/Worker/Labour，非 Verifier）+ `question.requested/answered/cancelled/expired` 事件；session `waiting_input`；Chat 选项/自由文本、`question.list`/`question.answer`、`vp questions`/`vp answer`；Manage Dash “Questions” 跨 session inbox；`VERLINPARK_QUESTION_TIMEOUT_SECS` 默认 1800（60–86400）；replay/repair/bundle |
| 可观测性 | ✅ | agent/tool/approval/plan_task/manager.roundtable/merge_queue/integrator/landing/PR/mailbox/artifact 事件；Collab roster；Preview Plan/Changes/Queue/Mailbox/Cost/Trace；`server.doctor` / `vp doctor` 检查 DB、权限、project root、worktree、event mirror、sandbox、provider，Manage Dash 有 Doctor 与 worktree cleanup 卡片；Rail Notifications 聚合 approval/task/integration/session failure，支持 unread badge、Dock badge、Mark all read、open session，并通过 UserDefaults 持久化最多 100 条 |
| Git history | ✅ | Preview → History + `vp history` / `vp show`：read-only `git -C <worktree> log/show`（200 commits、4–40 hex hash 校验、200 KB patch cap、stat+patch、refs/author/date）；不提供 checkout/reset/rebase |
| Workspace files | ✅ | Preview → Files + `vp files` / `vp file` / `vp write-file`：session worktree tree（.gitignore、跳过 .git、5000 entries/depth 20、truncated）+ 单文件读取（相对路径 canonicalize jail、拒绝 `..`/absolute/.git/symlink escape、512 KiB 文本 cap、binary 1 KiB hex preview、sha256/editable）+ 文本编辑（expected_sha256 冲突检测、atomic temp+rename、`workspace.file_saved` 事件、dirty/reload 确认）；binary/truncated/超大只读 |
| Terminal | ✅ | Preview → Terminal + `vp terminal`：daemon-owned PTY（`portable-pty`）、sandbox-exec + `/bin/sh -l`、base64 raw bytes、resize、512 KiB ring + dropped、process-group kill；SwiftTerm 1.10.1 渲染；与 Console 分开；输出不持久化 |
| Console | ✅ | per-session line-oriented `/bin/sh -s`，run 在同一 session worktree 的 Seatbelt write sandbox 内；`console.open/write/read/signal/close` / Preview → Console / `vp console` 一次性执行；stdin/stdout/stderr streaming chunks，256 KiB ring buffer + dropped 计数、64 KiB write cap、200 chunks/read；int/term/kill + process-group cleanup；不是 PTY，不支持 TUI/raw mode；输出不持久化 |
| MCP | ✅ | global MCP server configs（`stdio` / `http` Streamable HTTP / `sse` legacy HTTP+SSE；name/transport/command/args/env/url/headers/enabled）；`mcp.server.list/add/update/remove/probe` / `mcp.tools` / `mcp.call` / `mcp.resources` / `mcp.resource.read` / `mcp.prompts` / `mcp.prompt.get` / `vp mcp-*` / Settings → MCP add/edit/probe/inspect；每 server 一个 persistent session，initialize 后复用、串行调用、idle 300s 回收（`VERLINPARK_MCP_IDLE_SECS`）、config 变更失效；stdio process group 清理，HTTP session id + `MCP-Protocol-Version` + DELETE，legacy SSE endpoint event + same-origin POST；tools/resources/templates/prompts cursor 分页（50 页/2000 items）、读操作可重连一次、call 不重试；60s timeout、8MiB HTTP/SSE body/event cap、2MiB stdio line cap、env allowlist；HTTP 只允许 http/https、same-origin redirect/endpoint、敏感 header 必须 `${ENV_VAR}` 展开且不落库/不日志；server-initiated sampling/roots 以 -32601 拒绝；agent generic `mcp` tool（tool call 需 approval；resources/prompts 只读）；Leader/Worker/Labour 启动时把最多 32 个 MCP tools 作为 `mcp_<server>_<tool>` 动态 schema 注入（Manager/Verifier 不注入，call 需 approval）；binary resource 只报告长度；无 standalone server notification stream/sampling |
| Project memory | ✅ | `MemoryItem`（kind/title/body/tags/session/agent）+ `remember` / `recall` agent tools；recall 跨 project 内所有 session；`memory.list` / `memory.create` / `memory.delete` / `memory.prune` / `memory.policy.get` / `memory.policy.set` / `vp memory` / `vp remember` / `vp memory-delete` / `vp memory-prune` / `vp memory-policy` / `vp memory-policy-set` / Manage Dash Project memory（删除 + 30 天 prune + opt-in retention policy）；`memory.created` / `memory.deleted` 事件参与 replay/repair，bundle 包含 memories；FTS5 外部内容索引 + LIKE fallback；daemon 每小时 sweep enabled policy（`VERLINPARK_MEMORY_SWEEP_SECS`）；无 embedding |
| Mailbox / artifacts | ✅ | session-scoped typed messages（unread/read、sender/receiver、kind、body、JSON payload）+ text artifacts（≤100KB）+ content-addressed zstd binary blobs（≤5MiB，SHA-256/path，跨 artifact 去重，refcount 删除，按需 materialize）+ legacy artifact event backfill；agent tools `send_message`/`read_mailbox`/`publish_artifact`/`list_artifacts`/`read_artifact`；RPC/CLI/Preview Mailbox；artifact 预览支持图片、64KB 文本、1KB hex/ASCII dump；bundle export/import（binary base64）；项目 artifact policy（max bytes/age/count、auto GC）+ manual GC（`bytes_freed` 为实际释放的物理字节）；项目 mailbox policy（max age/count、delete_unread、auto_gc）+ manual/auto oldest-first GC，默认保护 unread |
| Event consistency | ✅ | session status / usage / agent status / team plan / todos / messages / merge queue / agent roster / approvals / usage ledger / mailbox / artifacts / memories / questions / task queue / recovery 使用 event+projection 原子事务；`session.replay` 比较 plan task-DAG 队列（status/attempts/priority/deps/scope/enqueue time）、approvals、usage ledger、mailbox、artifacts、memories 与 questions，`--repair` / `--rebuild` 从 `team_plan.submitted`、`plan_task.*`、`approval.*`、`agent.usage`、`mailbox.*` 和完整 `artifact.published/pruned` 事件重建投影；`session.log_status` 比较 JSONL mirror 与 events 表，`session.restore_log` 从连续 mirror 恢复 events 并 rebuild；artifact/mailbox GC 发出 `artifact.pruned` / `mailbox.pruned`；`session.export` / `session.import` bundle 包含 usage ledger 与 task-DAG 行 |
| Import archive | ✅ | `vp import <bundle>` 保留 session/project/事件/approvals/投影/task-DAG 队列；导入 session 为只读 archive，不创建 worktree |
| Rehydrate | ✅ | `vp rehydrate <session> [--worktree path]`：only imported；校验 project/base commit；创建/attach session worktree；按 branch 分组重建 imported agent task worktrees；状态回到 idle；返回 warnings/remapped count |
| DB rebuild | ✅ | `vp db-rebuild [--dry-run]`：读取 `projects.jsonl` 最新项目/policy/budget 快照 + 每个 session 的 `events.jsonl`，备份旧 DB，重建 projects/sessions/events/projections；worktree/branch/artifact blobs 原样保留；无 `session.created` 的 imported archive 跳过并报告 |
| 容量 | ✅ | 默认 4 Managers / 6 Workers / 12 Labour；runtime caps 5/10/40；plan ≤100 tasks；`delegate_many` ≤10；Worker/Labour 用 active slot，plan batch 按可用容量选择 |
| Global scheduling | ✅ | daemon-wide session admission：`VERLINPARK_GLOBAL_SESSION_CAP` 默认 4（1–32）；session priority -100..100（`session-new --priority` / New Session 选择器 / `session.priority.set` / session header menu / `vp session-priority`，事件 `session.priority` + rebuild 恢复），高优先级先准入，aging 每 `VERLINPARK_PRIORITY_AGING_SECS`（默认 60，0 关闭）提升 1 点至 +100，同优先级按 session/project round-robin；超出时 session 状态 `queued`，`agent.queued`；daemon 重启时 queued session 保留并 re-enqueue（running/paused 仍标 failed）；**task-DAG 持久化**：`plan_tasks` 每 session/plan revision 保存 status/deps/write_scope/priority/attempts/timestamps，`sync_plan_tasks` 从 `team_plan.submitted` + `plan_task.started/finished` 幂等重建，`task.queue` / `vp tasks` / Manage Dash 可查看；ready 按 `deps completed`、effective priority（session + task + aging）+ enqueue/task id 排序，scope 不重叠才同批；**write leases**：`write_leases` SQLite registry 跨 session 串行、同 session 共享、空/wildcard 独占，daemon recovery 清理 stale lease，session 终止时把残留 running task 标为 interrupted 并补事件；**project-level resume scheduler**：`TaskSchedulerPolicy`（enabled + cooldown 1–86400s）持久化、manifest、RPC、CLI、Manage Dash；scheduler loop 常驻，只为 enabled 项目按 effective priority/aging/session round-robin 选择 idle/failed/cancelled 且有 pending task 的 session，调用 `resume_plan`；可选 `dispatch_tasks` 模式改为调用 `dispatch_next_task`，每个 tick 每个 session 只领取并执行一个 ready task，任务结束后 session 回到 idle，让多个 session 公平推进；`task.dispatch` / `vp task-dispatch` / Manage Dash「Dispatch one task」可手动触发同一个 one-task 路径；`VERLINPARK_TASK_SCHEDULER=1` 仅作为无显式 policy 项目的全局默认，显式 policy 优先；`VERLINPARK_TASK_SCHEDULER_SECS` 默认 5；**task skip/retry/cancel/pause/resume**：非 active session（idle/failed/cancelled/draft）下 `task.retry` 把 failed/blocked/interrupted/cancelled/skipped 回退为 pending，`task.skip` 把 queued/blocked task 永久标为 skipped；运行中的 task 可 `task.pause` → `pausing` → Worker 边界 `paused`，`task.resume` 回到 running；也可 `task.cancel` 进入 `cancelling` 并协作式退出，兄弟 task 继续；`plan_task.retried` / `.skipped` / `.cancelling` / `.pausing` / `.paused` / `.resumed` 事件 + replay/repair，skipped 不被 global scheduler 自动重试，依赖它的后续 task 仍 blocked；Manage Dash 提供 Pause/Resume/Stop；Worker/Labour capacity controller：`VERLINPARK_GLOBAL_WORKERS` 默认 12、`VERLINPARK_GLOBAL_LABOUR` 默认 24（1–100），run 级 permit、provider 重配置不重置、`agent.info`/Settings 可见；等待 permit 时写 `agent.capacity_wait` 并标 `queued`，获得后写 `agent.capacity_ready`；capacity 也按 session priority + plan task priority（`session.priority + task.priority` clamp ±100）+ aging + round-robin；plan 可选 budget-gated Worker auto-expansion（ready tasks、20% token 余额、hard cap 10、`plan.auto_expand` 事件、结束后恢复）；admission/capacity 仍是进程内，task 队列/lease 已持久化 |
| Team capacity | ✅ | Settings → Providers → Team capacity：Managers 1–5 / Workers 1–10 / Labour 1–40 Stepper，Apply 写入 UserDefaults 并通过 `agent.configure` 应用；默认 4/6/12，运行时 hard cap 5/10/40；可选 auto-expand plan Workers（预算 ≥20% 时按 ready tasks 扩到最多 10，plan 结束后恢复 base cap，`VERLINPARK_AUTO_EXPAND_WORKERS`） |
| Provider | ✅ | OpenAI-compatible + Anthropic；`agent.probe` / `vp provider-test` / Settings Test connection 发送一次无工具 16-token 请求，检查 endpoint/key/model/stream；429/5xx/connection 在 stream 开始前最多重试 3 次（Retry-After + 指数退避，15s connect timeout）；stream 建立后每个事件最多等待 `VERLINPARK_PROVIDER_IDLE_SECS`（默认 300，10–3600），超时使 run 失败；OpenAI-compatible 与 Anthropic 都有本地 HTTP 全链路 E2E（plan → Worker write → approval → Verifier → Integrator） |
| Models / budgets | ✅ | 分角色 model override + input/output USD/1M price；session token / USD / wall-clock budget；project-wide shared token/USD pool；TeamPlan department/task token/USD budgets；per-call `usage_ledger`（agent/role/task/department/model/tokens/estimated/cost）+ `session.usage` / `vp usage` / Preview → Cost 分组汇总；`session.usage_csv` / `vp usage --csv` / Preview Export CSV（RFC 4180 转义，逐 call 一行）；`project.usage` / `vp project-usage` / Manage Dash 按 session 汇总 tokens/cost/calls；`agent.usage` / `agent.cost`；time 在 turn/tool 边界检查；Providers/New Session/CLI/UI |
| CLI | ✅ | ping/info/projects/shell-policy/shell-policy-set/integration-policy/integration-policy-set/artifact-policy/artifact-policy-set/gc/artifact/backfill-artifacts/mailbox-policy/mailbox-policy-set/mailbox-gc/session-new/prompt/events/search/transcript/approvals/approve/deny/diff/commit/discard/cancel/pause/resume/todos/agents/team-plan/usage/mailbox/read-message/artifacts/artifact/backfill-artifacts/queue/retry/resolve/land/pr/replay [--repair|--rebuild]/log-status/restore-log/export/import/rehydrate/cleanup-worktrees/worktree-policy/worktree-policy-set/project-budget/project-budget-set/project-usage/resume-plan/memory/remember/memory-delete/memory-prune/memory-policy/memory-policy-set/questions/answer/mcp-servers/mcp-add/mcp-remove/mcp-tools/mcp-call/mcp-resources/mcp-read-resource/mcp-prompts/mcp-prompt/console/terminal/tasks/task-retry/task-skip/task-cancel/task-pause/task-resume/task-scheduler/task-scheduler-set/files/file/write-file/history/show/doctor/provider-test/db-rebuild |
| Search | ✅ | `session.search` / `vp search` / Stage `Cmd-F`；跨 session 搜索 message content 与 raw event payload，可按 project 过滤，newest-first，`truncated` 标记；FTS5 外部内容索引（messages/events，insert/update/delete triggers，`store_meta` 迁移重建）+ ASCII prefix 查询，无命中/非 ASCII 回退 LIKE；snippet 截断；无 BM25 排序/embedding |
| Transcript | ✅ | `session.transcript` / `vp transcript` / Session menu Export Transcript…；Markdown 包含 session 元数据、按 department 的 TeamPlan、按时间排序的 conversation 与 tool calls；单 message 8KB、tool args 1KB 截断；不是可导入 bundle |
| Update check | ✅ | `update.check` / `vp update-check` 查询 GitHub latest release；默认 `Verlintas/VerlinPark`，可 `VERLINPARK_UPDATE_REPO` 或 `owner/repo` 覆盖；只比较版本并返回 release name/url/time；`update.download` / `vp update-download` 要求 `.dmg` + `.sha256`，流式下载到 `~/Downloads`、512MB cap、SHA-256 校验、原子 rename，不自动安装；Settings → About 手动检查、下载、Show in Finder；见 ADR-0107 |
| UI motion | ✅ | `VPMotion` tokens / `vpAppear` / `vpRise` / `vpHoverLift` / `vpGlowBorder` / `vpShimmer` / `VPSegmentedControl` / `VPStatusDot` / `VPThinkingIndicator` / `VPStreamingCaret` / `VPAnimatedNumberText` / `VPSkeleton` / `VPToastStack`；Rail/Dashboard/Settings 选中项 matched geometry + hover；Stage 消息入场/streaming caret/composer focus glow；Studio Collab stagger + animated connectors + 10-mode scrollable Preview chips；Manage Dash 全卡 hover/appear；MCP/ACP/搜索/通知/新建会话过渡；⌘K command palette、⇧⌘F focus mode、TopChrome breadcrumb、Settings 8-section sidebar、resizable rail/inspector + layout presets、persisted window frame、native Workspace command menu、fuzzy command palette recents/scope filters、filterable notification inbox、pinned rail items/session sort、search source filters、toast feedback；全程遵循 Reduce Motion；TopChrome 提供全局通知入口，⌘/ 打开快捷键参考，关键操作显示 transient toast；见 ADR-0114 |
| Manage Dash | ✅ | 侧边栏 9 个分区（Overview/Inbox/Team & Tasks/Usage & Budget/Git & Forks/Checkpoints/Storage & Memory/Policies/Diagnostics）+ live badges；1080×740；项目/session picker 自带；Overview 显示 metrics/pending actions/needs attention/runtime/recent activity；`dashboard.snapshot` / `vp dashboard` 跨 project 汇总 sessions/tasks/approvals/questions/checkpoints/forks 与 severity-sorted alerts（≤100, truncated flag），daemon 附带 capacity；Overview 在未选 project 时显示按 activity 排序的 Projects breakdown（running/queued/failed、approvals/questions、tasks、checkpoints、forks，可一键 Open）；分区过滤 + AppStorage 记忆；卡片拆分为 Overview/Team/Git/Policy 文件；Team 新增 Agents roster；见 ADR-0110/0112 |
| Model presets | ✅ | 全局 SQLite `model_presets`；`model.preset.list/create/update/delete` / `vp model-presets` / Settings Providers 模态；只存 name/provider/model/context/prices/notes，不存 API key/base URL；provider 匹配时客户端填充 role model/context/price 字段，仍需 Save & Connect 持久化；无自动 routing |
| Distribution | ✅ | `just package` 生成 Release app + bundled `verlinpadd` + `vp` + docs 的 DMG 和 SHA-256；默认 ad-hoc，**Developer ID/公证已明确暂缓，不作为近期目标**；ad-hoc DMG 是当前支持的发布路径。可选 `VERLINPARK_SIGN_IDENTITY` / `VERLINPARK_NOTARY_PROFILE` 代码保留但不投入；`just cask` / tag workflow 生成 cask，并可用 `HOMEBREW_TAP_REPO` + `HOMEBREW_TAP_TOKEN` 自动提交到 tap（未配置时只留在 GitHub release）；release workflow 使用 arm64 runner；仍无 Sparkle 自动更新 |
| Project budget | ✅ | `project.budget.get/set` / `vp project-budget*` / Manage Dash；跨 session 共享 token/USD pool，使用同一 `BudgetRegistry` `project:<id>` 原子 reservation；project 超限终止当前 run |
| Project usage | ✅ | `project.usage` / `vp project-usage` / Manage Dash：按 session 返回 goal/status/calls/input/output/cost 和项目 totals；与 project budget 使用同一 session 投影 |
| Worktree cleanup | ✅ | `project.cleanup_worktrees` / `vp cleanup-worktrees` / Manage Dash：仅终端 session、默认 7 天、dry-run、dirty 保护、默认保留 branch、`--delete-branches` 显式删除、`--drop-failed`（默认保留失败/取消 worktree）；项目级 `WorktreeRetentionPolicy`（默认关闭、≤365 天、keep failed、keep branches）+ daemon 定时 sweep（`VERLINPARK_WORKTREE_SWEEP_SECS` 默认 3600，0 关闭）；移除 worktree 时同时清 `sandbox/<session>`，worktree 已缺失时也清孤儿 scratch |

## 验证

- `cargo test --workspace`：358 个测试通过（core 286、protocol 2、store 56、providers 14）；另有 3 个 env-gated live provider tests 默认 ignored。
- `xcodebuild test`：94 个 Swift 测试通过（含协议向后兼容解码、diff parser、transcript/collab、Rail session grouping 与 design-system invariants）。
- `just lint`：fmt + clippy `-D warnings` + swiftformat lint 通过。
- Live provider E2E：`OPENAI_API_KEY=... cargo test -p vp-providers --test live -- --ignored --nocapture`；Anthropic 用 `ANTHROPIC_API_KEY` / `ANTHROPIC_MODEL`，本地 endpoint 用 `VERLINPARK_LIVE_BASE_URL` / `VERLINPARK_LIVE_MODEL`。
- 最近 CI：GitHub hosted macOS runner 持续排队/容量受限；本地 lint、Rust/Swift 测试与打包通过。
- 手工 E2E 已覆盖：Leader/Manager/Worker/Labour 委派、task worktree、并行 Worker、Verifier approve/reject、TeamPlan 自动派发、Manager 圆桌、审批、diff、commit/discard、预算/取消/暂停恢复、项目 shell policy get/set；ACP 委派由 fake ACP stdio agent 测试覆盖 initialize → session/new → prompt → permission → Worktree 写入 → Verifier → merge。

## 已知 enforcement 缺口

第一轮硬化（2026-10-30）已修复 plan scope/caps、token reservation 和 fetch_url 的 DNS 绕过。仍然存在的：

1. **Shell 仍非完整沙箱**：写入已由 Seatbelt 限制在 worktree/Git 元数据/scratch；常见 credential/state 路径已 read-deny，但不是完整 read jail；项目 policy 可 deny network，但默认 allow，Docker 等 daemon 介导写入仍可能；`sandbox-exec` 已废弃。见 AUDIT P1-3。
2. **Verifier 隔离**：无 write/edit 工具，运行在 Worker commit 的 isolated detached clone 中，shell/Git 写操作不会影响 Worker worktree/branch；clone 结束删除，tracked guard 仍保留。仍不是完整网络/文件系统沙箱。见 AUDIT P1-3。
3. **事件溯源**：关键状态已改为原子事件+投影；`session.replay` / `--repair` / `--rebuild` 可发现并重建覆盖的投影（含 agent roster）；`session.export` 可导出 bundle；数据库项目/会话创建元数据的从零导入仍未实现。见 AUDIT P1-5。
4. **Provider stream 不是可恢复流**：无输出前会 retry，但一旦有 text/tool output，mid-stream 断线会失败该 turn；不会 replay prompt，也不会从断点续传。partial output 和 usage 会保留，但 stream 本身不可恢复。见 AUDIT P1-8。
5. **ACP 不是完整安全边界**：外部 agent 二进制是用户安装的受信任程序，运行在 Seatbelt 写沙箱和 scratch HOME 中，但仍有默认读权限与网络权限（受 shell policy 约束）；`fs/*`/`terminal/*` capability 不暴露，只支持 stdio transport；token usage 不进入 provider usage ledger。见 AUDIT P1-7。
6. **batch abort 状态一致性**：已修复（同批 task 中止时全部结算为 cancelled/blocked）；见 AUDIT P2-7。

## 当前限制（明确未实现）

- PR flow 依赖本机 `gh` 且只覆盖 GitHub；其他 host/PR provider 还没有 adapter。
- merge queue 目前是 session 内、逐条 drain；显式 retry/auto-resolve 是人工触发；自动触发必须由项目 policy 显式开启，并有 attempt/token cap；没有 LLM Integrator。
- Global session admission 是进程内 priority queue，不是持久队列；aging 可防止无限饿死，但等待时间仍受优先级差 × aging interval 影响；持久 write leases 只覆盖 Worker/Labour，daemon 重启仍会失败 in-flight runs 并清理 lease；Leader/Manager/Verifier 写入不在全局 lease 内。
- 如果 daemon 在 git merge 过程中崩溃，session worktree 可能仍需人工检查/abort；recovery 不会猜测部分 merge 的结果。
- 自动派发中的 Manager 是确定性部门节点；LLM 圆桌只在 `deliberate_team_plan` 中运行。
- Task-DAG 队列和 write leases 已持久化，task 行也进入 replay/repair/bundle/db-rebuild；自动调度是 project-level policy（默认关闭），支持整-plan resume 或每 tick 一个 ready task 的 dispatch 模式，但仍需要已存储的 TeamPlan，不会执行没有 active plan 的孤立 task；task skip/retry/cancel/pause/resume 已支持（running 操作为 turn/tool 边界协作式，不抢占 in-flight provider stream/tool）。
- 仅 plan Worker 有 budget-gated auto-expansion（最多 10）；Manager/Labour 不自动扩编，也没有 scale-down。
- 没有 per-role token budget（只有 per-role model/USD price）；task/department/project budget tree 已实现；USD 预算依赖用户手填价格，时间预算只在 turn/tool 边界检查，不能抢占 in-flight provider/tool。
- Mailbox 是 pull-based：消息不会唤醒 idle Agent/创建新 run；mailbox auto GC 只在 send/read 边界触发且无回收站，unread 保护可能使 max_count 无法立即满足；artifact binary 上限 5MiB，已有 content-addressed dedupe、zstd 和内容级 replay/repair；GC oldest-first 无回收站；legacy artifact 事件只有部分元数据，repair 会跳过并警告，binary 依赖 content-addressed blob 存在。
- Global admission/capacity controller 仍是进程内、非持久；task-DAG 队列与 write leases 已写入 SQLite，session priority + aging 也进入 task ready 排序，但没有 daemon 重启后自动恢复 in-flight permit。Per-task dispatch 的 task claim 是原子行状态更新，但 session 运行归属仍是进程内 `runs` map。当前 daemon 以 `run/daemon.lock` 排他持有 data dir，因此同库多 daemon 不是受支持场景；真正的跨进程 priority/capacity/session lease 属于未来 server 模式，不是当前单 daemon 的运行时缺口。
- ACP 只支持 stdio transport；HTTP/SSE、`session/resume`、ACP `fs/*`/`terminal/*` client capability、auth/login 流程未实现；外部 agent 的 token usage 不写入 usage ledger；scratch HOME 意味着 ambient `~/.claude`、`~/.config/gemini` 等状态不可见，凭据必须显式配置 env；外部 agent 仍是受信任二进制，不是网络/凭证沙箱。ACP 委派是同步的 Leader/Manager 工具，不是 plan task kind，因此 plan auto-dispatch 不会调度 ACP Worker；session pause 不会在 ACP 运行中生效，session cancel 会发送 `session/cancel` 并终止进程组。
- Task/department/project budget 已有进程内原子 reservation，但不跨 daemon 重启持久化；手动 delegate 任务没有 plan task id 时只受 session/project budget 约束。
- 历史 session 只有在新事件带 `usage_id` 后才会被 replay 比较；repair 可用 legacy event 的 synthetic id 回填 ledger。
- Context compaction 在每个 top-level turn 前检查，但只在 user/assistant 边界切割；summary 可能丢失细节，hard-limit 的 rolling trim 更会丢失未进 summary 的消息（事件记录 dropped_messages）；原消息仍保留。`RoleModelConfig.context_window` 需要用户按模型正确配置，否则仍可能过早压缩或 provider overflow；已有 session checkpoint/fork，但没有 compaction checkpoint/resume；暂停状态只在内存中，daemon 重启会把 running/paused session 标为 failed。
- Terminal 是 daemon-owned PTY，但终端输出/scrollback 不持久化、跨 daemon 重启不恢复；SwiftTerm 固定 1.10.1（新版本引入 Metal toolchain/plugin 要求）；逐 hunk review、真实 Collab 节点、web search 仍缺；Console 仍是 line-oriented；Files 文本编辑限制 512 KiB、binary/truncated 只读，保存用 expected_sha256 检测并发修改但不做三方合并。
- Search 主路径是 FTS5 word/prefix 索引，空结果或非 ASCII 查询回退 LIKE；结果是 newest-first，不做 BM25/相关性排序；event 结果是 raw JSON snippet；FTS 有命中时可能漏掉另一行里的纯 substring 匹配。
- Transcript 不包含完整 event timeline、artifacts 或 approvals，且不可导入；超长内容会截断。
- Provider retry 覆盖 stream 建立前的 transient error，以及**尚无任何输出**时的 stream error/idle timeout（最多 3 次）；一旦已有 text/tool output，mid-stream 断开不会续传或 replay，只会保留 partial output、结算 usage 并失败该 turn。
- Artifact preview 是只读预览：文本 64KB、hex 1KB，未做语法高亮或非 UTF-8 编码探测。
- Roundtable 卡片只渲染当前已加载的 event window；更早的 round 需要扩大 Trace/事件加载。
- Usage CSV 一次生成完整字符串，不支持流式导出；Homebrew cask 可发布到 tap，但没有 Sparkle 式自动更新/增量升级。
- `project.usage` 只按 session 汇总，没有按天/model 的时间序列；UI 只展示前 8 个 session。
- 审批中心在打开 Manage Dash 时拉取；有 macOS 本地系统通知（后台时发送，点击回到 session），但没有远程推送；自动 resolve 的审批需要手动刷新。
- Diff 文件选择基于 unified diff 文本；不处理 Git 的 quoted path，也不支持逐 hunk stage。
- Worktree retention sweep 默认 1 小时一次、按 session 创建时间计算，不覆盖 shell scratch，也没有全局磁盘上限。
- Plan resume 默认是显式操作；设置 `VERLINPARK_TASK_SCHEDULER=1` 后，daemon 会按 effective priority 轮询 idle/failed/cancelled session 并 resume pending plan（有 cooldown，仍会消耗 session budget）。只重跑非 completed task，completed task 不重新验证；plan 版本只按最近一次 `team_plan.submitted` 区分；batch abort 只结算，不自动重跑 sibling task。
- Provider idle timeout 只覆盖 stream 事件间隔，不限制总时长；超时不会重放已产生的 partial text/tool calls；外部真实 Provider 的 E2E 仍需 API key。
- `db-rebuild` 依赖 `projects.jsonl`（daemon 至少启动/变更过一次）；旧 session 的 budget/base commit 可能回落默认值；imported archive 需要重新 `vp import`；worktree/artifact blobs 不重建；没有自动清理旧备份。
- 通知历史通过 UserDefaults 持久化，内容包含 session goal/tool/agent；macOS 系统通知在 app 非 active 时发送，前台由 Rail 承担，点击通知打开对应 session；系统通知权限被拒时仅保留 in-app 通知；同一个 session 的失败/取消可能各产生一条通知。
- Project memory 支持手动删除、`older_than_days` / `keep_latest` prune，以及 opt-in per-project retention policy；daemon 每小时（可配置/可关闭）sweep enabled policy；recall 先走 FTS5，无结果/失败时回退 LIKE；CJK 分词不完整，主要靠 fallback；无 embedding/语义检索；删除保留事件历史。
- `ask_user` 等待时占用 session admission slot；超时可配（`VERLINPARK_QUESTION_TIMEOUT_SECS`，默认 1800s）；一个 question 只接受一个答案，不能做多问题访谈；inbox 只在 Manage Dash 拉取，不是持久任务队列。
- `restrict_home` 只限制 HOME 内容；`read_jail` 进一步限制 HOME parent 与 `/Volumes`，但 System 路径、`/private/tmp`、`/private/var`、HOME 外其他路径、network allow 下的外发和 daemon 介导写入仍未隔离；allowlist 是启发式，可能需调整；`sandbox-exec` 已废弃。
- MCP stdio 首次连接仍有进程启动延迟；stdio env 存在本地 DB 而非 Keychain，MCP server 本身不在 shell sandbox 内，只应添加可信命令；HTTP/SSE 敏感 header 必须用 `${ENV_VAR}`，静态 secret 会被拒绝；不打开 standalone server notification stream，不支持 sampling/roots/elicitation；binary resource 只显示 base64 长度，未解码/物化；tool call 丢失响应时按失败处理，不自动重试；legacy SSE endpoint 必须同源。
- queued session 恢复需要 provider 已配置；未配置时保持 `queued`，配置后由 `agent.configure` 触发 re-enqueue；queued session 的取消仍依赖 active run。
- Shell 只做写入隔离与常见 credential read-deny；项目 policy 只能收窄不能放宽、只对后续 run 生效，且不随 session bundle 导出；不是完整 read jail，network allow 下仍可外发，`sandbox-exec` 是废弃 API；task worktree/branch 有显式 `cleanup-worktrees`，但没有自动/定时清理；shell scratch 仍无统一回收。
- `IntegrationPolicy` 与 auto-resolve attempt 计数是项目/队列元数据，不随 session bundle 导出；自动触发必须有 provider 配置，且仍受 session token budget 与 per-resolution cap 限制。
- Aviiya 仍是 NovaBAIC 外部桥接占位，不在本仓库实现人格/记忆。
- 发布路径确定为 ad-hoc DMG + 可选 Homebrew cask/tap；Developer ID/公证暂缓，不要求 Apple 开发者账号；没有 Sparkle 更新器。

## 历史里程碑

- **M0 Foundation**：Rust workspace、daemon + token discovery、HTTP/WebSocket JSON-RPC、SQLite/JSONL 事件、CLI、Provider SSE、SwiftUI shell、CI。
- **M0.5 Glass UI**：三栏 Glass Park、品牌/地区旗/Pride 旗、Manage Dash、Computer Lease 展示、Aviiya external placeholder。
- **M1 Leader runtime**：session worktree、工具、审批、token 预算、取消、diff/commit/discard、Provider 设置、todo 计划。
- **M2 四层 runtime**：Manager titles/profiles、TeamPlan、manager roundtable、自动派发、task worktrees、并行 Worker、write scope、Verifier gate、Task Board。
- **M2.5 硬化**：plan batch scope/cap、token reservation、DNS 固定的 fetch_url、Verifier shell 审批提示。
- **M3 第一切片**：持久 merge queue、确定性 Integrator、diff check、可选 integration command、失败回滚、冲突记录、Queue UI/CLI。
- **M3 第二切片**：queue rebase/retry、显式本地 landing。
- **M3 第三切片**：显式 push + `gh` PR flow（preflight、no fake success、事件/CLI/UI）。
- **M3 第四切片**：daemon 重启后的 in-flight merge queue recovery。
- **M3 第五切片**：关键状态 event+projection 原子写与事件重建测试。
- **M3 第六切片**：team plan / todos / messages / recovery 原子化，`session.replay` / `vp replay` 一致性检查与 `--repair`。
- **M3 第七切片**：`agent.snapshot` 完整 roster 事件与 agent 表 repair。
- **M3 第八切片**：`session.export` bundle 与 `--rebuild` 强制投影重建。
- **M3 第九切片**：`session.import` bundle archive 导入（只读、不自动创建 worktree）。
- **M3 第十切片**：`session.rehydrate` 将 imported session 的 worktree/branch 重建并恢复可运行状态；import 同时恢复 approvals 投影。
- **M3 第十一切片**：rehydrate 按 branch 分组重建 imported agent task worktrees。
- **M3 第十二切片**：session 级协作式 pause/resume（turn/tool 边界、同一 run 继续、terminal 状态不可覆盖），CLI + app 控制。
- **M3 第十三切片**：macOS Seatbelt shell 写入沙箱（worktree + Git 元数据 + scratch；hooks/config 保护；fail closed）。
- **M3 第十四切片**：shell credential/state read-deny + process group 清理（timeout/cancel 不遗留后台进程）。
- **M3 第十五切片**：项目级 shell policy（network allow/deny、额外 read-deny、RPC/CLI/Manage Dash）。
- **M3 第十六切片**：Queue 冲突的 agent-assisted auto-resolve（rebase、marker 检查、独立 Verifier、重新 Integrator）。
- **M3 第十七切片**：项目级 opt-in auto-resolve policy + persisted attempt cap + per-resolution token cap。
- **M3 第十八切片**：daemon-wide session admission（queued sessions）+ 跨 session Worker/Labour write-scope locks。
- **M3 第十九切片**：分角色模型覆盖 + per-role USD pricing + session USD/wall-clock budgets。
- **M3 第二十切片**：session-scoped typed mailbox + text artifact store + agent tools/UI/CLI/bundle。
- **M3 第二十一切片**：binary file artifacts（≤5MiB）+ SHA-256/path + project retention policy + manual/auto GC + bundle base64。
- **M3 第二十二切片**：daemon-wide Worker/Labour capacity controller（run 级 permit、provider 重配置复用、`agent.info` snapshot、Settings 展示）。
- **M3 第二十三切片**：per-call usage ledger（agent/role/task/model/tokens/estimated/cost）+ `session.usage` / `vp usage` / Preview → Cost 分组汇总。
- **M3 第二十四切片**：TeamPlan department/task token/USD budget 树 + scope-aware 失败语义 + usage ledger replay/repair/export/import + capacity wait 观测。
- **M3 第二十五切片**：项目级 mailbox retention policy + oldest-first GC + unread 保护 + CLI/Manage Dash。
- **M3 第二十六切片**：mailbox replay/repair（sent/read/pruned）+ artifact/mailbox GC audit events。
- **M3 第二十七切片**：daemon-wide plan budget reservation registry（task+department 原子 grant、token/USD reservation、RAII release、session finalize 清理）。
- **M3 第二十八切片**：content-addressed binary artifact blobs + SHA-256 去重 + path refcount 删除 + 物理 bytes_freed。
- **M3 第二十九切片**：artifact replay/repair（full publish metadata、prune apply、legacy 只读比较、projection drift 检测）。
- **M3 第三十切片**：mailbox `auto_gc`（send/read 后触发、unread 保护、CLI/Manage Dash）。
- **M3 第三十一切片**：Verifier tracked-file guard（无 write/edit 工具、post-run tracked diff、reset + 拒绝 verdict）。
- **M3 第三十二切片**：approval event+projection 原子化 + approval replay/repair。
- **M3 第三十三切片**：event mirror status/restore（contiguity 校验、events 表替换、projection rebuild）。
- **M3 第三十四切片**：legacy artifact event backfill（显式、幂等、backfill 后 replay/repair 可用）。
- **M3 第三十五切片**：session-fair Worker/Labour capacity queue（round-robin、cancellation-safe、try_acquire 不插队）。
- **M3 第三十六切片**：zstd artifact blob compression + materialize + bundle 原字节导出/导入。
- **M3 第三十七切片**：direct DMG packaging（release build、bundled daemon、ad-hoc sign、checksum、tag release workflow）。
- **M3 第三十八切片**：terminal worktree cleanup（dry-run、dirty 保护、默认保留 branch）。
- **M3 第三十九切片**：project-wide shared budget pool（跨 session token/USD reservation、abortive project scope、CLI/Manage Dash）。
- **M3 第四十切片**：`server.doctor` / `vp doctor` 本地只读诊断（DB、权限、root、worktree、mirror、sandbox、provider）。
- **M3 第四十一切片**：`agent.probe` / `vp provider-test` / Settings Test connection 真实 provider 连通性探针。
- **M3 第四十二切片**：TeamPlan task priority（-100..100，ready batch priority desc + id，scope/capacity/budget 仍优先）。
- **M3 第四十三切片**：provider transient retry（429/5xx/connection，最多 3 次，Retry-After + 指数退避，connect timeout）。
- **M3 第四十四切片**：session search（messages + raw event payload，project scope，CLI/UI `Cmd-F`，truncated/snippet）。
- **M3 第四十五切片**：Markdown transcript export（plan + conversation + tool calls，CLI/Save panel）。
- **M3 第四十六切片**：artifact text/hex preview（64KB text、1KB hex dump，materialize-on-demand）。
- **M3 第四十七切片**：Manage Dash Doctor/worktree cleanup 卡片、project budget 80% warning 显示、merge test 竞态修复。
- **M3 第四十八切片**：plan batch abort 全量结算（同批 cancelled/blocked 事件，无 running 残留）。
- **M3 第四十九切片**：Collab Manager roundtable 卡片（按 round 分组、title/profile/position）。
- **M3 第五十切片**：usage ledger CSV export（`session.usage_csv` / `vp usage --csv` / Preview Export CSV）。
- **M3 第五十一切片**：Homebrew cask 生成与 release asset（`just cask`、tag workflow）。
- **M3 第五十二切片**：`project.usage` / `vp project-usage` / Manage Dash 按 session 的项目用量汇总。
- **M3 第五十三切片**：Manage Dash 跨 project 审批中心（pending 汇总、session goal、Approve/Deny）。
- **M3 第五十四切片**：Preview Changes 按文件解析 unified diff（路径、+/- 行数、文件选择）。
- **M3 第五十五切片**：opt-in worktree retention policy + daemon sweep + `keep_failed`。
- **M3 第五十六切片**：resumable TeamPlan（skip completed、reattach task worktree、reuse existing commit、deterministic resume）。
- **M3 第五十七切片**：provider stream idle timeout（默认 300s，10–3600 可配，超时失败 run）。
- **M3 第五十八切片**：OpenAI-compatible HTTP adapter 全链路 E2E（wiremock SSE → plan → write → approval → Verifier → Integrator）。
- **M3 第五十九切片**：`vp db-rebuild`：project manifest + session event mirrors 的从零数据库重建（dry-run、旧 DB backup、projection rebuild）。
- **M3 第六十切片**：Rail 通知中心（approval / task failure / integration failure / session stop，unread badge、Dock badge、open session）。
- **M3 第六十一切片**：queued session 跨 daemon 重启恢复（`recover_state` 保留 queued，读 `agent.queued` prompt 自动 re-enqueue）。
- **M3 第六十二切片**：Anthropic HTTP adapter 全链路 E2E（SSE tool use → plan → write → Verifier → Integrator）。
- **M3 第六十三切片**：worktree cleanup 同步清理 session sandbox scratch（含 worktree 已缺失的孤儿 scratch）。
- **M3 第六十四切片**：通知历史 UserDefaults 持久化（最多 100 条，重启恢复 unread/Dock badge）。
- **M3 第六十五切片**：project memory（remember/recall tools、RPC/CLI/Manage Dash、replay/repair、bundle）。
- **M3 第六十六切片**：blocking user questions（ask_user、waiting_input、Chat card、RPC/CLI、replay/repair/bundle、30 分钟超时）。
- **M3 第六十七切片**：opt-in shell home read jail（`restrict_home`，HOME 内容默认拒绝、toolchain allowlist、read-deny 最后生效）。
- **M3 第六十八切片**：Manage Dash question inbox + 可配置 `ask_user` timeout。
- **M3 第六十九切片**：MCP stdio client（server config、list/call、agent tool、Settings/CLI、approval、timeout）。
- **M3 第七十切片**：MCP persistent process manager（复用、idle 回收、config 失效、process group、cursor 分页、安全重连）。
- **M3 第七十一切片**：memory retention + FTS5（delete/prune、事件回放、FTS/LIKE fallback、Manage Dash 删除/prune、`store_meta` 迁移标记）；见 ADR-0078。
- **M3 第七十二切片**：MCP resources/prompts（list/read/get、模板、agent tool、CLI/RPC、Settings inspect、分页与只读重连）。
- **M3 第七十三切片**：opt-in memory retention policy（per-project max items/max age、daemon sweep、CLI/Manage Dash、校验与测试）。
- **M3 第七十四切片**：priority-aware session admission + Worker/Labour capacity（-100..100、priority queue、session/project tie-break、CLI/UI、测试）；见 ADR-0079。
- **M3 第七十五切片**：macOS 本地系统通知（后台发送、点击跳转 session、权限请求、Settings 开关/状态）。
- **M3 第七十六切片**：MCP dynamic tool exposure（Leader/Worker/Labour 启动时注入最多 32 个 schema、sanitize/truncate、approval、isError、Manager/Verifier 排除）。
- **M3 第七十七切片**：sandboxed line-oriented Console（daemon-owned shell、streaming、Preview tab、`vp console`、signals、process-group cleanup）；见 ADR-0080。
- **M3 第七十八切片**：read-only workspace file browser（tree/file RPC、path jail、Preview Files tab、CLI、binary/large preview）；见 ADR-0081。
- **M3 第七十九切片**：workspace text editing（expected_sha256 冲突检测、atomic write、`workspace.file_saved` 事件、dirty/save/reload UI、CLI）；见 ADR-0082。
- **M3 第八十切片**：priority aging（`VERLINPARK_PRIORITY_AGING_SECS`、+100 cap、admission/capacity 测试）。
- **M3 第八十一切片**：approval 通知 Approve/Deny actions（category、action routing、测试）。
- **M3 第八十二切片**：Team capacity Settings 编辑器（Managers/Workers/Labour Stepper + apply）。
- **M3 第八十三切片**：测试模块拆分（`agent/tests.rs`、`lib.rs` 测试移到 `tests.rs`、`store/tests.rs`），无行为变更。
- **M3 第八十四切片**：read-only Git history viewer（`git.log`/`git.show`、History tab、CLI、hash 校验与 patch cap）；见 ADR-0083。
- **M3 第八十五切片**：plan task priority 计入全局 Worker/Labour capacity 排序（session priority + task priority clamp ±100）。
- **M3 第八十六切片**：budget-gated plan Worker auto-expansion（20% token gate、hard cap 10、cap guard、Settings/agent.configure/env、`plan.auto_expand`）；见 ADR-0084。
- **M3 第八十七切片**：GitLab merge-request adapter（origin 检测、`glab` preflight/push/MR、provider override、GitHub/GitLab 共用执行路径）；见 ADR-0085。
- **M3 第八十八切片**：mutable session priority（`session.priority.set`、事件/repair/rebuild、CLI、header menu）；见 ADR-0086。
- **M3 第八十九切片**：PTY terminal（portable-pty、SwiftTerm、Terminal tab、`vp terminal`、resize/process-group）；见 ADR-0087。
- **M3 第九十切片**：持久 task-DAG 队列 + SQLite global write leases + opt-in global resume scheduler（`task.queue`/`vp tasks`/Manage Dash、aging/round-robin、恢复清理）；task 行 replay/repair/bundle/db-rebuild 已补齐；task skip/retry + Manage Dash actions 已完成；见 ADR-0088。
- **M3 第九十一切片**：opt-in user/volume read jail（`ShellPolicy.read_jail`、HOME parent + `/Volumes` deny、toolchain/worktree allowlist、Manage Dash/CLI/RPC、Agent/Console/PTY 统一）；见 ADR-0089。
- **M3 第九十二切片**：可选 Developer ID 签名/notarization + cask tap 发布 + arm64 release runner；见 ADR-0090。
- **M3 第九十三切片**：running task cooperative cancel + pause/resume（`task.cancel` / `task.pause` / `task.resume`、`plan_task.cancelling/pausing/paused/resumed`、Worker 边界检查、兄弟 task 继续、Manage Dash/CLI、replay/recovery）；见 ADR-0088 更新。
- **M3 第九十四切片**：project-level auto-resume policy（`TaskSchedulerPolicy`、manifest、RPC/CLI/Manage Dash、per-project cooldown、scheduler 常驻按 policy 过滤）；见 ADR-0091。
- **M3 第九十五切片**：Verifier isolated checkout（detached clone of worker commit、GIT metadata 隔离、cleanup guard、tracked guard 保留、clone commit 不影响 worker branch）；见 ADR-0092。
- **M3 第九十六切片**：ACP external-agent bridge（agent 注册表、`delegate_acp` Leader/Manager 工具、stdio client、session/update→事件、permission→approval、scratch HOME + Seatbelt、MCP 传递、Verifier/merge 全链路、probe、Settings/CLI）；见 ADR-0093。
- **M3 第九十七切片**：provider stream resilience（无输出前 retry、partial failure/cancel 保留 message + usage settlement、长流测试、probe chunks/first-token/done、env-gated live OpenAI/Anthropic/local E2E）；见 ADR-0094。
- **M3 第九十八切片**：per-task dispatch mode（`TaskSchedulerPolicy.dispatch_tasks`、one-task-per-tick、`task.dispatch`/CLI/Manage Dash、limited plan execution 不扩散 abort、policy/manifest/UI/tests）；见 ADR-0095。
- **M3 第九十九切片**：session search FTS5（messages/events 外部内容索引、triggers、旧库 one-time rebuild、ASCII prefix 查询、空结果/非 ASCII LIKE fallback、跨 project filter、测试）；见 ADR-0096。
- **M3 第一百切片**：context compaction（阈值触发、安全边界切割、summary request/usage/budget、`context.compacted` + `through_message_id`、resume 从 summary、失败继续、测试）；见 ADR-0097。
- **M3 第一百零一切片**：model-aware context budget（global/per-role context window、75% hard-limit threshold、每个 turn 检查、message-ID tracking、summary 后 rolling trim fallback、`mode: trim` event、Settings ctx 字段、测试）；见 ADR-0098。
- **M3 第一百零二切片**：context usage observability（每个 top-level turn 的 `context.usage` event、`session.context`/`vp context`、Stage header ctx pill + tooltip、最近 summary/trim；`context.usage` test 与 Swift decode）；同时修正 compaction 阈值未计入 system/tool tokens 的问题，并把 agent/ACP 的错误截断改为 UTF-8 安全；见 ADR-0098 更新。
- **M3 第一百零三切片**：manual context compaction（`session.compact` / `vp compact` / Stage context popover；force summary/trim 复用同一 pipeline；idle-only + manual run control 阻止并发 prompt、pause 返回 false、cancel 仍生效；失败通过 RPC error + `context.compact_failed` 暴露；Rust/Swift tests）；见 ADR-0099。
- **M3 第一百零四切片**：provider-usage context calibration（`context.measured`、estimate vs actual、raw ratio、upward-only EWMA clamp 1.0–2.0、按 model/session 持久化、用于 usage/threshold/summary budget/manual compaction、Stage popover 显示 last measurement、legacy event 解码兼容；Rust/Swift tests）；见 ADR-0100。
- **M3 第一百零五切片**：session checkpoint/fork（temporary-index dirty worktree snapshot + checkpoint ref、active conversation snapshot、checkpoint 事件/列表、fork 新 worktree/branch、message copy + tool/assistant display reconstruction、lineage events、budgets/priority copy、CLI/Stage UI；Rust/Swift tests）；见 ADR-0101。
- **M3 第一百零六切片**：checkpoint retention（`session.checkpoint.deleted` tombstone、snapshot ref 删除、`session.checkpoint.prune` + dry-run、CLI/Manage Dash 列表/删除/prune；Rust/Swift tests）；见 ADR-0102。
- **M3 第一百零七切片**：automatic checkpoint retention（`CheckpointRetentionPolicy` 项目级持久化 + manifest/db-rebuild、`project.checkpoint_policy.get/set`、CLI/Manage Dash 控件、daemon sweep `VERLINPARK_CHECKPOINT_SWEEP_SECS`、engine sweep；Rust/Swift tests）；见 ADR-0103。
- **M3 第一百零八切片**：fork lineage + branch comparison（`session.lineage` parent/siblings/children、`session.compare` branch tips + numstat + patch cap、CLI `vp lineage/compare`、Manage Dash Fork lineage Open/Compare sheet、source fork event commit metadata + 旧事件 fallback；Rust/Swift tests）；见 ADR-0104。
- **M3 第一百零九切片**：explicit fork merge-back（direct-child 校验、clean source/fork、`--no-ff` merge、冲突 abort + paths、diff check + 可选 integration command、失败 reset hard、source/fork operation locks、`session.fork.merge` 事件、CLI/Manage Dash Merge；Rust/Swift tests）；见 ADR-0105。
- **M3 第一百一十切片**：recursive fork tree（`session.fork.tree`、depth 32 / nodes 200 cap、truncated flag、CLI/Manage Dash Tree sheet、递归 Open；Rust/Swift tests）；见 ADR-0106。
- **M3 第一百一十一切片**：manual release update check（`update.check` GitHub latest release、repo slug validation/env override、numeric version compare、CLI/Settings About、Open Release Page、无自动安装；Rust/Swift tests）；见 ADR-0107。
- **M3 第一百一十二切片**：project fork forest + audit/repair + checkpoint stats（session.created-derived forest roots、missing link/child/parent/checkpoint ref audit、append-only repair、object-size stats、CLI/Manage Dash；Rust/Swift tests）；见 ADR-0108。
- **M3 第一百一十三切片**：verified update download（`.dmg` + `.sha256`、GitHub URL/redirect allowlist、512MB cap、streaming SHA-256、atomic rename、Settings Download/Show in Finder；Rust/Swift tests）；见 ADR-0107 更新。
- **M3 第一百一十四切片**：model preset catalog（global SQLite presets、CRUD RPC/CLI、provider-scoped apply to role fields、no secrets、Settings modal；Rust/Swift tests）；见 ADR-0109。
- **M3 第一百一十五切片**：Manage Dash IA 重构（sidebar 9 sections + badges、project/session pickers、overview metrics/pending/runtime/recent、section filter/persistence、Agents roster、card file split；Swift tests）；见 ADR-0110。
- **M3 第一百一十六切片**：checkpoint diff/restore + cross-project dashboard snapshot（`session.checkpoint.diff` / `vp checkpoint-diff` tracked diff preview；`session.checkpoint.restore` / `vp checkpoint-restore`，idle/clean/branch/commit 校验、reset hard、restored event、CLI/UI Preview/Restore；`dashboard.snapshot` / `vp dashboard` 汇总 sessions/tasks/approvals/questions/checkpoints/forks + alerts、project filter、per-project breakdown、daemon capacity、Manage Dash attention card/badges；Rust/Swift tests）；见 ADR-0111/0112。
- **M3 第一百一十七切片**：MCP HTTP/SSE transports（`McpTransport` Stdio/Http/Sse、Streamable HTTP JSON/SSE + session id + protocol header + DELETE、legacy SSE endpoint/same-origin POST、same-origin redirect、header env indirection、8MiB caps、server-initiated -32601、transport-aware manager close/retry、ACP capability-filtered propagation、`mcp.server.probe` + CLI/Settings add/edit/probe、store migration；Rust/Swift tests）；见 ADR-0113。
- **M3 第一百一十八切片**：UI motion/interaction system（VPMotion tokens/primitives、Rail/Dashboard/Settings matched geometry、Stage chat/composer/streaming、Studio tabs/Preview chips/Collab stagger、Settings sidebar shell、⌘K command palette、⇧⌘F focus mode、TopChrome、toast stack、Reduce Motion；Swift tests）；见 ADR-0114。
- **M3 第一百一十九切片**：warm graphite atelier UI 重构（`VPTheme` 暖石墨表面/珊瑚强调色/语义色/角色色/项目色、Editorial+mono 排版、静态 ambient wash；Rail 内联展开项目 session、活动计数、running pulse、show-more、⌘K/new-session/inbox/settings；Stage 单 header、去重 transcript、880pt 阅读宽度、用户/助手/tool/approval/question 卡片、可展开 tool output、focus-ring composer；Inspector 持久化开关 `⌥⌘I`；Studio/Dashboard/Settings/所有 sheet/command palette/resident 面板统一重做；resident orbital mark、New Session brief+priority+budget grid；`vpStage`/`vpInspector` transitions；Swift tests）；见 ADR-0115。
- **M3 第一百二十切片**：black crystal glass refinement（`VPCrystalBackdrop` 静态多面体暗色背景、`VPCrystalSurface` in-window material + 深色 tint 的 Rail/Stage/Studio/TopChrome、半透明 surface token、specular `edgeGradient` 边框、圆角方形玻璃 icon button；冷冰蓝 accent + 深钢色 primary fill；移除 serif/coral/sparkles/orbit，改成 `cube.transparent` + 静态 crystal slab；New Session/Settings/Dashboard/Search/Notifications/Shortcuts/Fork 使用 crystal backdrop；见 ADR-0116）。
- **M3 第一百二十一切片**：runtime localization + responsive Studio + frosted fields（`AppLanguage`/`LanguageStore`、`L10n.t/format`、zh-Hans.lproj 约 450 键；Settings General 语言卡 + TopChrome globe 菜单，切换即时生效并持久化；Settings/Providers 角色行改为两层响应式卡片、输入框 frosted crystal；Studio Preview Files/History 在 <560pt inspector 宽度切换单栏 + 返回；Diff 色、状态色、role/project 色提高对比；更深 canvas；见 ADR-0117）。
- **M3 第一百二十二切片**：workspace shell + command/inbox 模块（`WorkspaceLayout` 自适应三栏与 stage minimum、可拖拽 `VPSplitHandle` + 持久化宽度 + `WorkspaceLayoutStore` Balanced/Reading/Team presets + ⌥⌘0 重置、`WindowStateStore` 屏幕内窗口 frame 恢复/屏幕变化重定位/防抖保存、焦点模式退出胶囊与 chrome 状态收敛、native Workspace command menu + `WorkspaceCommandRouter`、`CommandPaletteRanker` fuzzy/word-prefix/subsequence + recents + scope filters + `VPHighlightedText`、`NotificationInboxModel` filter/search/project/group/count + clear-read/remove + day groups、`PinnedItemsStore` pinned sessions/projects + rail sort、`SearchResultsModel` message/event source filters + match highlights、Reduce Transparency fallback；Swift tests 83；见 ADR-0118）。
- **M3 第一百二十三切片**：LM Studio / reasoning-model 实机修复（旧 `agents` 表缺 name/title/profile/parent_id/worktree/branch 的增量迁移；`agent.probe` 对 reasoning model 使用 1024–2048 token 预算与 60s 超时；`session.create` 统一走 `create_session_with_options`，priority 同时保留 token/USD/time budgets；OpenAI-compatible stream 请求 `stream_options.include_usage` 并在 400 时回退；`send_message` 说明不得发给 user 并给出清晰错误）。实机用 `empty3` 验证 provider probe、README 读取、write_file 审批、文件内容 `LMSTUDIO_OK`、真实 usage（33/220 probe，session 15088/488，非估算）与 600s time budget；Rust 270 / Swift 83）。
- **M3 第一百二十四切片**：风险审批 + 紧凑转录 + Collab 收敛（`ApprovalMode` smart/strict/unattended，默认 smart；`permissions.rs` 确定性分类 workspace/external/host-state/destructive/blocked，扫描 Shell 分段、`cd`、`sh -c`/`eval`/解释器 `-c`/`find -exec`/`xargs`/重定向/命令替换；`Approval` 持久化 `approval_risk`/`approval_reason` 并进入 replay/repair/bundle；`permission.allowed`/`permission.denied` 审计事件；Settings General 三档选择 + `VERLINPARK_APPROVAL_MODE`；`TranscriptProjection` + `ToolTraceRow` 单行工具调用、原地 running/finished、扁平审批条；`CollabSnapshot` 按 parent_id 分组 Worker，Collab 仅 Leader/Manager/Worker/Verifier/Labour + 计划进度；审计修复 `.git` 文件工具写保护、旧 approvals schema 迁移、移除静态 `requires_approval`、修正 `eval`/`sh -c` 递归 root 与 `xargs` 无显式目标；见 ADR-0119；Rust 300 / Swift 89）。

## 如何运行

```bash
just bootstrap
just test
just build
just run
```

或单独运行 daemon：

```bash
just daemon
just cli ping
just cli project-add /path/to/repo --name demo
just cli session-new <project-id> "Your goal"
```

本地状态在 `~/Library/Application Support/VerlinPark/`；daemon 只绑定 `127.0.0.1`。

## 已知问题

- App 首次启动 daemon 失败时只在 Welcome/ErrorBanner 显示；日志在 `~/Library/Application Support/VerlinPark/logs/daemon.log`。
- WebSocket 断线重连没有单元测试。
- Anthropic 工具调用没有真实账号 live 验证。
- `agent.rs` 与 `AppModel.swift` 仍然偏大，需要拆分（见 `docs/AUDIT.md`）。

- **M3 第一百二十五切片**：全量审查修复与优化收口（2,000+ 行审计修复）。新增 `docs/audit/2026-11-09-findings.md`/`.csv`：116 个原始 finding 合并为 65 条，50 Fixed / 10 Partial / 5 Deferred。修复 P0 Git worktree `.git`/common-ref sandbox 逃逸与 TeamPlan task-id 目录碰撞；Integrator 改为 merge 精确 verified commit、clean/base 检查与 Git 锁；retry 重新 Verifier；MCP call 升为 HostState 且 Manager/Verifier 只读；shell classifier 补 keyword/expansion/git -C/php -r/>|；secret roots 覆盖 DB sidecar/artifacts/manifest；recovery 事件可 replay；event mirror 串行排序；db-rebuild 恢复 cost/token、加 daemon lock 与失败回滚；bundle import 校验 path traversal/cross-session；provider EOF/error/JSON/非 SSE/超时/partial usage/tool-id/redaction；shell/read_file/grep/integration output 有界，integration command 进入 Seatbelt；auto-approved Worker 也取 write lease；UI event 分页、selection generation、delta 不进历史、approval live/pending、event disconnect/timeout、自动重连退避、workspace draft 跨视图保留、ACP env 编辑器、ACP/New Session/composer 成功后才清空、Rail 重试、terminal 2MB cap、idle console/terminal sweep、atomic plan_task claim event、item-specific stale guards、numeric validation、Collab skipped 计入完成、org preset 绑定真实配置、auth protocol negotiation。

- **M3 第一百二十六切片**：Collab 动态团队树 + 协调工具。新增 `CollabGraphModel`/`CollabGraphView`：基于 `AgentRecord.parent_id`、TeamPlan、task.queue、事件窗口和 merge queue 的纯投影，左到右 Leader→Manager→Worker→Verifier/Labour 树，递归布局、Bezier 连线、运行状态高亮、实时 tool/任务活动、计划中的 Manager/Task 虚线节点、完成 ghost 隐藏、多 Leader 崩溃容错、55% 可读缩放 + pan/zoom/fit + 宽屏 sheet、节点详情 inspector；`AgentRecord` 增加可选 `task_id`/`department`（additive schema、agent.updated、replay/bundle 兼容），plan Worker/Manager/Verifier/Labour/resolver 自动写入；新增 `coordinate`（note/blocker/unblock/handoff/escalate/decision/list）与只读 `team_status` 工具，coordination.note 进入节点活动和团队状态；plan Worker objective 注入上游 handoff 结果与最近协调记录，失败依赖的 blocked reason 精确到具体 task/status；Keychain API key 改为按需读取，本地 provider 启动不再触发钥匙串提示；Rust 318 / Swift 94。
