# VerlinPark 进度

> 最后更新：2026-11-07
> 当前阶段：M2 四层 runtime + M2.5 硬化 + Merge Queue/Integrator/Retry/Conflict Resolve + Opt-in Auto-Resolve + Global Session Queue/Scope Locks + Global Worker/Labour Capacity + Per-role Models/USD/Time Budgets + Per-call Usage Ledger + Task/Department Budgets + Mailbox Retention + Content-Addressed Artifacts + Typed Mailbox/Binary Artifacts/GC + Landing/PR + Session 协作式 Pause/Resume + Shell 沙箱/Policy 已跑通
> 审计与优先级：[AUDIT.md](AUDIT.md)  
> 文档索引：[README.md](README.md)

---

## 当前可用的能力

| 能力 | 状态 | 说明 |
|---|---|---|
| Leader Agent | ✅ | 流式对话、工具调用、审批、session 预算、取消、todo 计划 |
| Session worktree | ✅ | 每个 session 独立 Git worktree + 分支；项目根只读 |
| 四层组织 | ✅ | Leader → Manager → Worker → Labour；严格链路，Manager 必须有部门 title |
| Agent profile | ✅ | coding / research / verification / generic；决定工具集与 prompt |
| TeamPlan | ✅ | Leader 定义 departments / Manager titles / profiles / task DAG / task priority（-100..100）；runtime 校验结构、依赖、优先级和 scope 语法并持久化 |
| Manager 圆桌 | ✅ | `deliberate_team_plan`，1–3 轮，只读，后一轮看到前一轮所有 peer positions；Collab 显示按 round 分组的 Manager 立场卡片 |
| 自动派发 | ✅ | `run_team_plan` 按依赖批次执行 ready tasks；ready task 按 priority desc + id 排序；同批筛选不重叠 scope；Manager 懒创建；失败阻塞依赖；batch abort 时先结算同批全部 task（cancelled/blocked），不留下 running 投影；`session.plan_resume` / `vp resume-plan` / Preview Resume plan 可跳过 completed task，重跑 unfinished task |
| Task worktree | ✅ | Worker 独立 task worktree/branch；Labour 继承 Worker worktree；resume 复用/重新 attach 已有 worktree 和 branch，base 使用 merge-base，已有 commit 可重新进入 Verifier/Integrator |
| 并行 Worker | ✅ | `delegate_many` / plan batch 并发；两边都校验 write scope 不重叠；空 scope 或重叠 scope 的 plan task 串行执行 |
| Verifier gate | ✅ | 独立 Verifier；`submit_verdict`；approve 才合并。Verifier 无 write/edit 工具；shell 写入被限制在其 worktree；运行后检查 tracked diff，发现修改则 reset、标记 failed、拒绝 verdict（untracked/ignored 测试产物允许）；审批卡会提示 |
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
| MCP | ✅ | global stdio MCP server configs（name/command/args/env/enabled）；`mcp.server.*` / `mcp.tools` / `mcp.call` / `mcp.resources` / `mcp.resource.read` / `mcp.prompts` / `mcp.prompt.get` / `vp mcp-*` / Settings → MCP inspect；每 server 一个 persistent process，initialize 后复用、串行调用、idle 300s 回收（`VERLINPARK_MCP_IDLE_SECS`）、config 变更失效、进程组清理；tools/resources/templates/prompts cursor 分页（50 页/2000 items）、读操作可重连一次、call 不重试；60s timeout、2MiB line cap、env allowlist；agent generic `mcp` tool（tool call 需 approval；resources/prompts 只读）；Leader/Worker/Labour 启动时把最多 32 个 MCP tools 作为 `mcp_<server>_<tool>` 动态 schema 注入（Manager/Verifier 不注入，call 需 approval）；binary resource 只报告长度；无 sampling/notifications |
| Project memory | ✅ | `MemoryItem`（kind/title/body/tags/session/agent）+ `remember` / `recall` agent tools；recall 跨 project 内所有 session；`memory.list` / `memory.create` / `memory.delete` / `memory.prune` / `memory.policy.get` / `memory.policy.set` / `vp memory` / `vp remember` / `vp memory-delete` / `vp memory-prune` / `vp memory-policy` / `vp memory-policy-set` / Manage Dash Project memory（删除 + 30 天 prune + opt-in retention policy）；`memory.created` / `memory.deleted` 事件参与 replay/repair，bundle 包含 memories；FTS5 外部内容索引 + LIKE fallback；daemon 每小时 sweep enabled policy（`VERLINPARK_MEMORY_SWEEP_SECS`）；无 embedding |
| Mailbox / artifacts | ✅ | session-scoped typed messages（unread/read、sender/receiver、kind、body、JSON payload）+ text artifacts（≤100KB）+ content-addressed zstd binary blobs（≤5MiB，SHA-256/path，跨 artifact 去重，refcount 删除，按需 materialize）+ legacy artifact event backfill；agent tools `send_message`/`read_mailbox`/`publish_artifact`/`list_artifacts`/`read_artifact`；RPC/CLI/Preview Mailbox；artifact 预览支持图片、64KB 文本、1KB hex/ASCII dump；bundle export/import（binary base64）；项目 artifact policy（max bytes/age/count、auto GC）+ manual GC（`bytes_freed` 为实际释放的物理字节）；项目 mailbox policy（max age/count、delete_unread、auto_gc）+ manual/auto oldest-first GC，默认保护 unread |
| Event consistency | ✅ | session status / usage / agent status / team plan / todos / messages / merge queue / agent roster / approvals / usage ledger / mailbox / artifacts / memories / questions / task queue / recovery 使用 event+projection 原子事务；`session.replay` 比较 plan task-DAG 队列（status/attempts/priority/deps/scope/enqueue time）、approvals、usage ledger、mailbox、artifacts、memories 与 questions，`--repair` / `--rebuild` 从 `team_plan.submitted`、`plan_task.*`、`approval.*`、`agent.usage`、`mailbox.*` 和完整 `artifact.published/pruned` 事件重建投影；`session.log_status` 比较 JSONL mirror 与 events 表，`session.restore_log` 从连续 mirror 恢复 events 并 rebuild；artifact/mailbox GC 发出 `artifact.pruned` / `mailbox.pruned`；`session.export` / `session.import` bundle 包含 usage ledger 与 task-DAG 行 |
| Import archive | ✅ | `vp import <bundle>` 保留 session/project/事件/approvals/投影/task-DAG 队列；导入 session 为只读 archive，不创建 worktree |
| Rehydrate | ✅ | `vp rehydrate <session> [--worktree path]`：only imported；校验 project/base commit；创建/attach session worktree；按 branch 分组重建 imported agent task worktrees；状态回到 idle；返回 warnings/remapped count |
| DB rebuild | ✅ | `vp db-rebuild [--dry-run]`：读取 `projects.jsonl` 最新项目/policy/budget 快照 + 每个 session 的 `events.jsonl`，备份旧 DB，重建 projects/sessions/events/projections；worktree/branch/artifact blobs 原样保留；无 `session.created` 的 imported archive 跳过并报告 |
| 容量 | ✅ | 默认 4 Managers / 6 Workers / 12 Labour；runtime caps 5/10/40；plan ≤100 tasks；`delegate_many` ≤10；Worker/Labour 用 active slot，plan batch 按可用容量选择 |
| Global scheduling | ✅ | daemon-wide session admission：`VERLINPARK_GLOBAL_SESSION_CAP` 默认 4（1–32）；session priority -100..100（`session-new --priority` / New Session 选择器 / `session.priority.set` / session header menu / `vp session-priority`，事件 `session.priority` + rebuild 恢复），高优先级先准入，aging 每 `VERLINPARK_PRIORITY_AGING_SECS`（默认 60，0 关闭）提升 1 点至 +100，同优先级按 session/project round-robin；超出时 session 状态 `queued`，`agent.queued`；daemon 重启时 queued session 保留并 re-enqueue（running/paused 仍标 failed）；**task-DAG 持久化**：`plan_tasks` 每 session/plan revision 保存 status/deps/write_scope/priority/attempts/timestamps，`sync_plan_tasks` 从 `team_plan.submitted` + `plan_task.started/finished` 幂等重建，`task.queue` / `vp tasks` / Manage Dash 可查看；ready 按 `deps completed`、effective priority（session + task + aging）+ enqueue/task id 排序，scope 不重叠才同批；**write leases**：`write_leases` SQLite registry 跨 session 串行、同 session 共享、空/wildcard 独占，daemon recovery 清理 stale lease，session 终止时把残留 running task 标为 interrupted 并补事件；**opt-in global resume scheduler**：`VERLINPARK_TASK_SCHEDULER=1` 时按 effective priority/aging/session round-robin 选择 idle/failed/cancelled 且有 pending task 的 session，调用 `resume_plan`，`VERLINPARK_TASK_SCHEDULER_SECS` 默认 5、`_COOLDOWN_SECS` 默认 60；**task skip/retry**：非 active session（idle/failed/cancelled/draft）下 `task.retry` 把 failed/blocked/interrupted/cancelled/skipped 回退为 pending，`task.skip` 把 queued/blocked task 永久标为 skipped；`plan_task.retried` / `plan_task.skipped` 事件 + replay/repair，skipped 不被 global scheduler 自动重试，依赖它的后续 task 仍 blocked；Manage Dash 提供按钮；Worker/Labour capacity controller：`VERLINPARK_GLOBAL_WORKERS` 默认 12、`VERLINPARK_GLOBAL_LABOUR` 默认 24（1–100），run 级 permit、provider 重配置不重置、`agent.info`/Settings 可见；等待 permit 时写 `agent.capacity_wait` 并标 `queued`，获得后写 `agent.capacity_ready`；capacity 也按 session priority + plan task priority（`session.priority + task.priority` clamp ±100）+ aging + round-robin；plan 可选 budget-gated Worker auto-expansion（ready tasks、20% token 余额、hard cap 10、`plan.auto_expand` 事件、结束后恢复）；admission/capacity 仍是进程内，task 队列/lease 已持久化 |
| Team capacity | ✅ | Settings → Providers → Team capacity：Managers 1–5 / Workers 1–10 / Labour 1–40 Stepper，Apply 写入 UserDefaults 并通过 `agent.configure` 应用；默认 4/6/12，运行时 hard cap 5/10/40；可选 auto-expand plan Workers（预算 ≥20% 时按 ready tasks 扩到最多 10，plan 结束后恢复 base cap，`VERLINPARK_AUTO_EXPAND_WORKERS`） |
| Provider | ✅ | OpenAI-compatible + Anthropic；`agent.probe` / `vp provider-test` / Settings Test connection 发送一次无工具 16-token 请求，检查 endpoint/key/model/stream；429/5xx/connection 在 stream 开始前最多重试 3 次（Retry-After + 指数退避，15s connect timeout）；stream 建立后每个事件最多等待 `VERLINPARK_PROVIDER_IDLE_SECS`（默认 300，10–3600），超时使 run 失败；OpenAI-compatible 与 Anthropic 都有本地 HTTP 全链路 E2E（plan → Worker write → approval → Verifier → Integrator） |
| Models / budgets | ✅ | 分角色 model override + input/output USD/1M price；session token / USD / wall-clock budget；project-wide shared token/USD pool；TeamPlan department/task token/USD budgets；per-call `usage_ledger`（agent/role/task/department/model/tokens/estimated/cost）+ `session.usage` / `vp usage` / Preview → Cost 分组汇总；`session.usage_csv` / `vp usage --csv` / Preview Export CSV（RFC 4180 转义，逐 call 一行）；`project.usage` / `vp project-usage` / Manage Dash 按 session 汇总 tokens/cost/calls；`agent.usage` / `agent.cost`；time 在 turn/tool 边界检查；Providers/New Session/CLI/UI |
| CLI | ✅ | ping/info/projects/shell-policy/shell-policy-set/integration-policy/integration-policy-set/artifact-policy/artifact-policy-set/gc/artifact/backfill-artifacts/mailbox-policy/mailbox-policy-set/mailbox-gc/session-new/prompt/events/search/transcript/approvals/approve/deny/diff/commit/discard/cancel/pause/resume/todos/agents/team-plan/usage/mailbox/read-message/artifacts/artifact/backfill-artifacts/queue/retry/resolve/land/pr/replay [--repair|--rebuild]/log-status/restore-log/export/import/rehydrate/cleanup-worktrees/worktree-policy/worktree-policy-set/project-budget/project-budget-set/project-usage/resume-plan/memory/remember/memory-delete/memory-prune/memory-policy/memory-policy-set/questions/answer/mcp-servers/mcp-add/mcp-remove/mcp-tools/mcp-call/mcp-resources/mcp-read-resource/mcp-prompts/mcp-prompt/console/terminal/tasks/task-retry/task-skip/files/file/write-file/history/show/doctor/provider-test/db-rebuild |
| Search | ✅ | `session.search` / `vp search` / Stage `Cmd-F`；跨 session 搜索 message content 与 raw event payload，可按 project 过滤，newest-first，`truncated` 标记；LIKE 转义 `%`/`_`/`\`，snippet 截断；无 FTS/index |
| Transcript | ✅ | `session.transcript` / `vp transcript` / Session menu Export Transcript…；Markdown 包含 session 元数据、按 department 的 TeamPlan、按时间排序的 conversation 与 tool calls；单 message 8KB、tool args 1KB 截断；不是可导入 bundle |
| Distribution | ✅ | `just package` 生成 Release app + bundled `verlinpadd` + `vp` + docs 的 DMG 和 SHA-256；默认 ad-hoc，`VERLINPARK_SIGN_IDENTITY` 可 Developer ID 签名，`VERLINPARK_NOTARY_PROFILE` 可 notarize+staple app 后再打 DMG；`just cask` / tag workflow 生成 cask，并可用 `HOMEBREW_TAP_REPO` + `HOMEBREW_TAP_TOKEN` 自动提交到 tap（未配置时只留在 GitHub release）；release workflow 使用 arm64 runner；仍无 Sparkle 自动更新 |
| Project budget | ✅ | `project.budget.get/set` / `vp project-budget*` / Manage Dash；跨 session 共享 token/USD pool，使用同一 `BudgetRegistry` `project:<id>` 原子 reservation；project 超限终止当前 run |
| Project usage | ✅ | `project.usage` / `vp project-usage` / Manage Dash：按 session 返回 goal/status/calls/input/output/cost 和项目 totals；与 project budget 使用同一 session 投影 |
| Worktree cleanup | ✅ | `project.cleanup_worktrees` / `vp cleanup-worktrees` / Manage Dash：仅终端 session、默认 7 天、dry-run、dirty 保护、默认保留 branch、`--delete-branches` 显式删除、`--keep-failed`；项目级 `WorktreeRetentionPolicy`（默认关闭、≤365 天、keep failed、keep branches）+ daemon 定时 sweep（`VERLINPARK_WORKTREE_SWEEP_SECS` 默认 3600，0 关闭）；移除 worktree 时同时清 `sandbox/<session>`，worktree 已缺失时也清孤儿 scratch |

## 验证

- `cargo test --workspace`：190 个测试通过（core 148、store 34、providers 8）。
- `xcodebuild test`：38 个 Swift 测试通过（含协议向后兼容解码与 diff parser）。
- `just lint`：fmt + clippy `-D warnings` + swiftformat lint 通过。
- 最近 CI：GitHub hosted macOS runner 持续排队/容量受限；本地 lint、Rust/Swift 测试与打包通过。
- 手工 E2E 已覆盖：Leader/Manager/Worker/Labour 委派、task worktree、并行 Worker、Verifier approve/reject、TeamPlan 自动派发、Manager 圆桌、审批、diff、commit/discard、预算/取消/暂停恢复、项目 shell policy get/set。

## 已知 enforcement 缺口

第一轮硬化（2026-10-30）已修复 plan scope/caps、token reservation 和 fetch_url 的 DNS 绕过。仍然存在的：

1. **Shell 仍非完整沙箱**：写入已由 Seatbelt 限制在 worktree/Git 元数据/scratch；常见 credential/state 路径已 read-deny，但不是完整 read jail；项目 policy 可 deny network，但默认 allow，Docker 等 daemon 介导写入仍可能；`sandbox-exec` 已废弃。见 AUDIT P1-3。
2. **Verifier 只读**：无 write/edit 工具，运行后会检查 tracked diff 并 reset/拒绝；但 shell 仍可创建 untracked/ignored 文件、读取任意路径，不是完整文件系统沙箱。见 AUDIT P1-3。
3. **事件溯源**：关键状态已改为原子事件+投影；`session.replay` / `--repair` / `--rebuild` 可发现并重建覆盖的投影（含 agent roster）；`session.export` 可导出 bundle；数据库项目/会话创建元数据的从零导入仍未实现。见 AUDIT P1-5。
4. **batch abort 状态一致性**：已修复（同批 task 中止时全部结算为 cancelled/blocked）；见 AUDIT P2-7。

## 当前限制（明确未实现）

- PR flow 依赖本机 `gh` 且只覆盖 GitHub；其他 host/PR provider 还没有 adapter。
- merge queue 目前是 session 内、逐条 drain；显式 retry/auto-resolve 是人工触发；自动触发必须由项目 policy 显式开启，并有 attempt/token cap；没有 LLM Integrator。
- Global session admission 是进程内 priority queue，不是持久队列；aging 可防止无限饿死，但等待时间仍受优先级差 × aging interval 影响；持久 write leases 只覆盖 Worker/Labour，daemon 重启仍会失败 in-flight runs 并清理 lease；Leader/Manager/Verifier 写入不在全局 lease 内。
- 如果 daemon 在 git merge 过程中崩溃，session worktree 可能仍需人工检查/abort；recovery 不会猜测部分 merge 的结果。
- 自动派发中的 Manager 是确定性部门节点；LLM 圆桌只在 `deliberate_team_plan` 中运行。
- Task-DAG 队列和 write leases 已持久化，task 行也进入 replay/repair/bundle/db-rebuild；全局 scheduler 是 opt-in，只 resume 已存储的 plan，不会执行没有 active plan 的孤立 task；task skip/retry 已支持，但没有 running task 的单 task cancel/pause（仍需 session pause/cancel）。
- 仅 plan Worker 有 budget-gated auto-expansion（最多 10）；Manager/Labour 不自动扩编，也没有 scale-down。
- 没有 per-role token budget（只有 per-role model/USD price）；task/department/project budget tree 已实现；USD 预算依赖用户手填价格，时间预算只在 turn/tool 边界检查，不能抢占 in-flight provider/tool。
- Mailbox 是 pull-based：消息不会唤醒 idle Agent/创建新 run；mailbox auto GC 只在 send/read 边界触发且无回收站，unread 保护可能使 max_count 无法立即满足；artifact binary 上限 5MiB，已有 content-addressed dedupe、zstd 和内容级 replay/repair；GC oldest-first 无回收站；legacy artifact 事件只有部分元数据，repair 会跳过并警告，binary 依赖 content-addressed blob 存在。
- Global admission/capacity controller 仍是进程内、非持久；task-DAG 队列与 write leases 已写入 SQLite，session priority + aging 也进入 task ready 排序，但没有跨进程 priority queue，也没有 daemon 重启后自动恢复 in-flight permit。
- Task/department/project budget 已有进程内原子 reservation，但不跨 daemon 重启持久化；手动 delegate 任务没有 plan task id 时只受 session/project budget 约束。
- 历史 session 只有在新事件带 `usage_id` 后才会被 replay 比较；repair 可用 legacy event 的 synthetic id 回填 ledger。
- 没有 context compaction、checkpoint/resume；暂停状态只在内存中，daemon 重启会把 running/paused session 标为 failed。
- Terminal 是 daemon-owned PTY，但终端输出/scrollback 不持久化、跨 daemon 重启不恢复；SwiftTerm 固定 1.10.1（新版本引入 Metal toolchain/plugin 要求）；逐 hunk review、真实 Collab 节点、ACP、web search 仍缺；Console 仍是 line-oriented；Files 文本编辑限制 512 KiB、binary/truncated 只读，保存用 expected_sha256 检测并发修改但不做三方合并。
- Search 是 `LIKE` 扫描，无 FTS/index/ranking；非 ASCII 大小写匹配不完整，event 结果是 raw JSON snippet。
- Transcript 不包含完整 event timeline、artifacts 或 approvals，且不可导入；超长内容会截断。
- Provider retry 只覆盖 stream 建立前的错误；stream 中途断开不会自动续传。
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
- MCP 首次连接仍有进程启动延迟；无 sampling/notifications；binary resource 只显示 base64 长度，未解码/物化；env 存在本地 DB 而非 Keychain；MCP server 本身不在 shell sandbox 内，只应添加可信命令；tool call 丢失响应时按失败处理，不自动重试。
- queued session 恢复需要 provider 已配置；未配置时保持 `queued`，配置后由 `agent.configure` 触发 re-enqueue；queued session 的取消仍依赖 active run。
- Shell 只做写入隔离与常见 credential read-deny；项目 policy 只能收窄不能放宽、只对后续 run 生效，且不随 session bundle 导出；不是完整 read jail，network allow 下仍可外发，`sandbox-exec` 是废弃 API；task worktree/branch 有显式 `cleanup-worktrees`，但没有自动/定时清理；shell scratch 仍无统一回收。
- `IntegrationPolicy` 与 auto-resolve attempt 计数是项目/队列元数据，不随 session bundle 导出；自动触发必须有 provider 配置，且仍受 session token budget 与 per-resolution cap 限制。
- Aviiya 仍是 NovaBAIC 外部桥接占位，不在本仓库实现人格/记忆。
- 有可选 Developer ID 签名/公证和可选 tap 发布；没有 Sparkle 更新器，未配置证书时仍是 ad-hoc DMG。

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
- `agent.rs`、`StudioView.swift` 已经偏大，需要拆分（见 `docs/AUDIT.md`）。
