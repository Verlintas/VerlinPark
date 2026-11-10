# VerlinPark 文档索引

> 最后更新：2026-11-11
> 仓库：<https://github.com/Verlintas/VerlinPark>

文档分成三类，避免把“设计目标”误读成“已经实现”：

| 类型 | 文档 | 用途 |
|---|---|---|
| **Current** | [PROGRESS.md](PROGRESS.md) | 当前真实可用的能力、限制、测试结果；状态以这里为准 |
| **Current** | [AUDIT.md](AUDIT.md) | 最近一次项目重审：P0/P1/P2 问题、证据、建议顺序 |
| **Current** | [audit/2026-11-09-findings.md](audit/2026-11-09-findings.md) | 2026-11-09 全量审查完整台账（[CSV](audit/2026-11-09-findings.csv)）；116 个原始 finding 合并为 65 条并保留全部来源 ID |
| **Design** | [ARCHITECTURE.md](ARCHITECTURE.md) | 目标架构、协议、数据模型、状态机；实现状态见 PROGRESS/AUDIT |
| **Design** | [PLAN.md](PLAN.md) | 长期产品计划、范围、里程碑、风险；保留早期设计章节作为背景 |
| **Current** | [TOOLING_ROADMAP.md](TOOLING_ROADMAP.md) | Agent 工具完整清单、已实现 wave 状态、剩余 media/search/introspection/GUI 缺口与设计预算 |
| **Reference** | [DECISIONS.md](DECISIONS.md) | Accepted ADR 索引；改架构先加 ADR |
| **Reference** | [RESEARCH.md](RESEARCH.md) | 2026-10 的竞品/技术调研；历史背景，不是当前状态 |
| **Reference** | [AGENT_ORCHESTRATION_AND_CACHE_RESEARCH.md](AGENT_ORCHESTRATION_AND_CACHE_RESEARCH.md) | Prompt/KV cache 命中率、Codex/Claude Code/OpenHands/Aider/Magentic-One 等架构对比与 VerlinPark 改进建议 |
| **Reference** | [PRIDE_FLAGS.md](PRIDE_FLAGS.md) | Pride flag 资产来源与许可 |
| **Reference** | [../THIRD_PARTY_NOTICES.md](../THIRD_PARTY_NOTICES.md) | FlagKit 等第三方资产许可 |
| **Contributing** | [../CONTRIBUTING.md](../CONTRIBUTING.md) | 构建、测试、提交约定 |
| **Agent rules** | [../AGENTS.md](../AGENTS.md) | 给 coding agent 的约束与当前里程碑 |

## 推荐阅读顺序

1. [README](../README.md) — 产品是什么、怎么跑。
2. [PROGRESS.md](PROGRESS.md) — 现在到底有什么。
3. [AUDIT.md](AUDIT.md) — 哪些地方还不可靠，下一步优先修什么。
4. [audit/2026-11-09-findings.md](audit/2026-11-09-findings.md) — 完整修复台账与残余风险。
5. [ARCHITECTURE.md](ARCHITECTURE.md) — 目标架构与不变量。
6. [DECISIONS.md](DECISIONS.md) — 已锁定的决策。
7. [PLAN.md](PLAN.md) — 长期路线与早期设计背景。
8. [TOOLING_ROADMAP.md](TOOLING_ROADMAP.md) — Agent 工具缺口与下一阶段工具实现顺序。

## 文档状态规则

- 行为变化：更新 `PROGRESS.md`，必要时加测试。
- 架构/协议/权限/预算语义变化：新增 ADR，并更新 `DECISIONS.md` 索引。
- `PLAN.md` 是长期设计文档，不要求每个实现切片都回写；每季度或大版本前做一次收敛。
- `README.md` 只放稳定的产品叙事和运行方式；细节链接到本目录。
- 任何“已实现”的表述必须有代码、测试或可复现命令支撑；否则写成 planned。
- 中英文可以混用；代码标识符、协议字段、事件名保持英文。

## ADR 快速索引

| 主题 | ADR |
|---|---|
| 技术栈 / IPC / 分发 | [0001](adr/0001-swiftui-rust-core.md)、[0007](adr/0007-websocket-jsonrpc.md)、[0008](adr/0008-gpl3-direct-distribution.md) |
| 组织模型 | [0011](adr/0011-four-level-org.md)（四层）、[0019](adr/0019-four-tier-delegation.md)（委派）、[0022](adr/0022-manager-titles.md)（Manager title）、[0024](adr/0024-team-plan-profiles.md)（TeamPlan/profile）、[0027](adr/0027-manager-roundtable.md)（圆桌） |
| 隔离 / Git | [0004](adr/0004-worktree-per-task.md)、[0016](adr/0016-session-worktree.md)、[0017](adr/0017-session-commit.md)、[0020](adr/0020-task-worktrees.md)、[0021](adr/0021-parallel-workers-scopes.md) |
| 质量 / 验证 | [0023](adr/0023-verifier-gate.md)（Verifier）、[0025](adr/0025-team-plan-dispatch.md)（自动派发）、[0028](adr/0028-merge-queue-integrator.md)（merge queue/Integrator）、[0029](adr/0029-local-landing.md)（本地 landing）、[0030](adr/0030-push-pr-flow.md)（push/PR）、[0031](adr/0031-queue-recovery.md)（queue recovery）、[0032](adr/0032-atomic-event-projections.md)（event/projection 原子写）、[0033](adr/0033-session-import-archive.md)（bundle 导入/archive）、[0034](adr/0034-cooperative-pause-resume.md)（pause/resume）、[0037](adr/0037-agent-assisted-conflict-resolution.md)（冲突自动解决）、[0038](adr/0038-policy-gated-auto-resolve.md)（opt-in 自动触发/caps）、[0127](adr/0127-task-inbox-autopilot.md)（task inbox/autopilot） |
| Preview / Workspace | [0081](adr/0081-workspace-file-browser.md)（workspace files）、[0082](adr/0082-workspace-text-editing.md)（workspace edit）、[0083](adr/0083-git-history-viewer.md)（git history）、[0128](adr/0128-lazy-workspace-browser-and-content-search.md)（lazy tree/content search） |
| 知识 / 产物 | [0129](adr/0129-project-knowledge-sources-and-citations.md)（知识源/引用）、[0130](adr/0130-project-skill-packs.md)（skill packs）、[0131](adr/0131-artifact-versions-and-idempotent-publication.md)（artifact 版本/幂等）、[0132](adr/0132-bounded-text-diff-and-citation-audit.md)（bounded diff/citation audit）、[0142](adr/0142-evidence-backed-memory.md)（evidence-backed memory v2） |
| 长任务控制 | [0138](adr/0138-cache-usage-and-stable-prefix.md)（cache/prompt prefix）、[0139](adr/0139-steering-and-progress-ledger.md)（steering/progress）、[0140](adr/0140-stall-recovery.md)（stall recovery）、[0141](adr/0141-deferred-session-continuations.md)（continuations）、[0143](adr/0143-long-task-operator-console.md)（macOS console） |
| 安全 / 权限 | [0035](adr/0035-shell-write-sandbox.md)（shell 写入沙箱）、[0036](adr/0036-project-shell-policy.md)（项目级 shell policy） |
| 成本 / 容量 | [0003](adr/0003-manual-model-budget.md)、[0018](adr/0018-budget-cancel.md)、[0026](adr/0026-org-cap-right-sizing.md)、[0039](adr/0039-global-session-cap-scope-locks.md)（global admission/scope locks）、[0040](adr/0040-per-role-models-usd-time-budgets.md)（per-role models/USD/time）、[0133](adr/0133-unlimited-session-budget-mode.md)（unlimited budget mode） |
| 产品 / UI | [0013](adr/0013-glass-park-brand.md)（Glass Park）、[0014](adr/0014-external-aviiya.md)（Aviiya 外置）、[0114](adr/0114-motion-and-interaction-system.md)（motion/UI system）、[0115](adr/0115-warm-graphite-atelier-ui.md)（warm graphite atelier）、[0116](adr/0116-black-crystal-glass-refinement.md)（black crystal glass）、[0117](adr/0117-runtime-localization.md)（runtime localization）、[0118](adr/0118-workspace-shell-and-command-inbox.md)（workspace shell/command inbox）、[0120](adr/0120-dynamic-team-graph.md)（dynamic team graph） |
| 通信 / 存储 | [0005](adr/0005-typed-messages-artifacts.md)（typed message/artifact，设计）、[0006](adr/0006-event-sourcing.md)（事件溯源，部分实现）、[0041](adr/0041-typed-mailbox-artifact-store.md)（mailbox/artifact 实现）、[0042](adr/0042-binary-artifacts-gc-retention.md)（binary/GC）、[0043](adr/0043-global-worker-labour-capacity.md)（global capacity）、[0044](adr/0044-per-call-usage-ledger.md)（usage ledger）、[0045](adr/0045-task-department-budgets.md)（budget tree）、[0046](adr/0046-mailbox-retention-gc.md)（mailbox retention）、[0047](adr/0047-budget-reservations.md)（budget reservations）、[0048](adr/0048-artifact-replay.md)（artifact replay）、[0049](adr/0049-verifier-tracked-file-guard.md)（verifier guard）、[0050](adr/0050-approval-events.md)（approval replay）、[0051](adr/0051-event-mirror-restore.md)（event mirror restore）、[0052](adr/0052-artifact-backfill.md)（artifact backfill）、[0053](adr/0053-fair-capacity-queue.md)（fair capacity）、[0054](adr/0054-artifact-compression.md)（artifact compression）、[0055](adr/0055-direct-dmg-packaging.md)（DMG packaging）、[0056](adr/0056-worktree-cleanup.md)（worktree cleanup）、[0057](adr/0057-project-budget-pool.md)（project budget）、[0058](adr/0058-doctor-diagnostics.md)（doctor）、[0059](adr/0059-provider-probe.md)（provider probe）、[0060](adr/0060-plan-task-priority.md)（plan priority）、[0061](adr/0061-provider-retry.md)（provider retry）、[0062](adr/0062-session-search.md)（search）、[0063](adr/0063-markdown-transcript.md)（transcript）、[0064](adr/0064-artifact-preview.md)（artifact preview）、[0065](adr/0065-plan-batch-abort.md)（batch abort）、[0066](adr/0066-usage-csv.md)（usage CSV）、[0067](adr/0067-homebrew-cask.md)（Homebrew cask）、[0068](adr/0068-project-usage-report.md)（project usage）、[0069](adr/0069-worktree-retention-policy.md)（worktree retention）、[0070](adr/0070-resumable-team-plan.md)（plan resume）、[0071](adr/0071-provider-idle-timeout.md)（provider idle timeout）、[0072](adr/0072-database-rebuild.md)（DB rebuild）、[0073](adr/0073-queued-session-recovery.md)（queued recovery）、[0074](adr/0074-project-memory.md)（project memory）、[0075](adr/0075-blocking-user-questions.md)（user questions）、[0076](adr/0076-home-read-jail.md)（home read jail）、[0077](adr/0077-mcp-stdio-client.md)（MCP stdio）、[0078](adr/0078-memory-retention-and-fts.md)（memory retention/FTS）、[0079](adr/0079-priority-admission.md)（priority admission）、[0080](adr/0080-sandboxed-console.md)（sandboxed console）、[0081](adr/0081-workspace-file-browser.md)（workspace files）、[0082](adr/0082-workspace-text-editing.md)（workspace edit）、[0083](adr/0083-git-history-viewer.md)（git history）、[0084](adr/0084-worker-auto-expansion.md)（worker auto-expansion）、[0085](adr/0085-gitlab-merge-request.md)（GitLab MR）、[0086](adr/0086-mutable-session-priority.md)（mutable priority）、[0087](adr/0087-pty-terminal.md)（PTY terminal）、[0088](adr/0088-persistent-task-dag-and-leases.md)（task DAG/leases）、[0089](adr/0089-user-read-jail.md)（user/volume read jail）、[0090](adr/0090-optional-signing-notarization-tap.md)（signing/notary/tap）、[0091](adr/0091-project-task-scheduler-policy.md)（project scheduler policy）、[0092](adr/0092-isolated-verifier-checkout.md)（Verifier checkout）、[0093](adr/0093-acp-external-agent-bridge.md)（ACP 外部 Agent）、[0094](adr/0094-provider-stream-resilience.md)（provider stream resilience）、[0095](adr/0095-per-task-dispatch.md)（per-task dispatch）、[0096](adr/0096-session-search-fts5.md)（search FTS5）、[0097](adr/0097-context-compaction.md)（context compaction）、[0098](adr/0098-model-aware-context-budget.md)（model-aware context budget）、[0099](adr/0099-manual-context-compaction.md)（manual compaction）、[0100](adr/0100-context-estimate-calibration.md)（context calibration）、[0101](adr/0101-session-checkpoints-and-forks.md)（checkpoint/fork）、[0102](adr/0102-checkpoint-retention-and-deletion.md)（checkpoint retention）、[0103](adr/0103-automatic-checkpoint-retention.md)（automatic checkpoint retention）、[0104](adr/0104-fork-lineage-and-comparison.md)（fork lineage/compare）、[0105](adr/0105-fork-merge-back.md)（fork merge-back）、[0106](adr/0106-recursive-fork-tree.md)（fork tree）、[0107](adr/0107-release-update-check.md)（update check）、[0108](adr/0108-project-fork-forest-and-audit.md)（fork forest/audit）、[0109](adr/0109-model-preset-catalog.md)（model presets）、[0110](adr/0110-manage-dash-information-architecture.md)（Manage Dash IA）、[0111](adr/0111-checkpoint-restore.md)（checkpoint restore）、[0112](adr/0112-cross-project-dashboard-snapshot.md)（dashboard snapshot）、[0113](adr/0113-mcp-http-and-sse-transports.md)（MCP HTTP/SSE） |

> `0005` 和 `0006` 是 Accepted 的架构方向，但当前实现只覆盖了一部分：text mailbox/artifact store、binary/zstd blob、retention GC 与 artifact replay 已实现；mailbox 仍是 session-scoped、pull-based，没有跨 session 投递；事件日志目前更接近“audit stream + projection”，不是完整 replay 的 event sourcing；`vp db-rebuild` 已可从 `projects.jsonl` + per-session event mirrors 重建覆盖的 projects/sessions/投影。详见 [AUDIT.md](AUDIT.md)。
