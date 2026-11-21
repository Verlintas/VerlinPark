# Agent tool access matrix

- Snapshot: 2026-11-21
- Source of truth: `crates/vp-core/src/agent/schemas.rs` (advertised schema) and
  `crates/vp-core/src/agent.rs` (`AgentRole::allows_tool` / `role_tool_surface`).
- This lists **advertised built-in tools**. Actual execution still passes the
  task contract, approval broker, effect receipt, Seatbelt/path jail, write
  scope, Verifier/Integrator, and budget/capacity gates.
- `all` means the tool is advertised for every profile of that role. Values
  such as `coding, research` mean only those Worker/Labour profiles receive it.
- `—` means not advertised and not in the role policy surface.
- Dynamic MCP tools are not listed individually. The `mcp` bridge exposes only
  tools from configured, enabled, pinned servers; Manager/Verifier can
  inspect/read but cannot call or cancel.
- `run_script` binds only the current read-only tool set by default; with
  `allow_write` it additionally binds `write_file`, `edit_file`, and
  `apply_patch`. It has no shell, network, process, GUI, MCP, or delegation
  bindings.

| Tool | Leader | Manager | Worker | Verifier | Labour | Scout |
|---|---|---|---|---|---|---|
| `affected_tests` | all | all | all | all | all | all |
| `apply_patch` | all | — | coding | — | coding | — |
| `archive_extract` | all | — | coding, research | — | coding, research | — |
| `archive_list` | all | all | all | all | all | all |
| `ask_user` | all | all | all | — | all | — |
| `browser` | all | — | coding | — | coding | — |
| `budget_status` | all | all | all | all | all | — |
| `build_context` | all | all | all | all | all | all |
| `code_diagnostics` | all | — | coding, verification | all | coding, verification | all |
| `computer` | all | — | coding | — | coding | — |
| `coordinate` | all | all | all | — | all | — |
| `data_query` | all | all | all | all | all | — |
| `delegate` | all | all | all | — | — | — |
| `delegate_acp` | all | all | — | — | — | — |
| `delegate_many` | — | all | — | — | — | — |
| `deliberate_team_plan` | all | — | — | — | — | — |
| `dependency_graph` | all | all | all | all | all | all |
| `edit_file` | all | — | coding | — | coding | — |
| `fetch_url` | all | research | coding, research, verification | research | coding, research, verification | — |
| `file_info` | all | all | all | all | all | all |
| `file_manage` | all | — | coding | — | coding | — |
| `find_files` | all | all | all | all | all | all |
| `find_references` | all | all | all | all | all | all |
| `find_symbol` | all | all | all | all | all | all |
| `git` | all | all | all | all | all | all |
| `grep` | all | all | all | all | all | all |
| `list_artifacts` | all | all | all | all | all | — |
| `list_dir` | all | all | all | all | all | all |
| `list_sources` | all | all | all | all | all | all |
| `local_http` | all | — | coding, research, verification | all | coding, research, verification | — |
| `lsp_query` | all | all | all | all | all | — |
| `mcp` | all | all | all | all | all | — |
| `media_info` | all | all | all | all | all | all |
| `media_transcode` | all | — | coding, research | — | coding, research | — |
| `plan_fanout` | all | — | — | — | — | — |
| `process` | all | — | coding, verification | all | coding, verification | — |
| `project_profile` | all | all | all | all | all | — |
| `publish_artifact` | all | all | all | all | all | — |
| `publish_artifact_file` | all | all | all | all | all | — |
| `read_artifact` | all | all | all | all | all | — |
| `read_document` | all | all | all | all | all | — |
| `read_file` | all | all | all | all | all | all |
| `read_files` | all | all | all | all | all | all |
| `read_image` | all | all | all | all | all | — |
| `read_mailbox` | all | all | all | all | all | — |
| `read_pdf` | all | all | all | all | all | — |
| `read_source` | all | all | all | all | all | all |
| `read_tool_output` | all | all | all | all | all | all |
| `recall` | all | all | all | all | all | all |
| `remember` | all | all | all | — | all | — |
| `remote_exec` | all | — | coding, verification | all | coding, verification | — |
| `remote_git` | all | all | all | all | all | all |
| `remote_list` | all | all | all | all | all | all |
| `remote_read` | all | all | all | all | all | all |
| `remote_worktree` | all | — | coding | — | coding | — |
| `remote_write` | all | — | coding, research | — | coding, research | — |
| `repo_map` | all | all | all | all | all | all |
| `report_progress` | all | all | — | — | — | — |
| `run_project_task` | all | — | coding, verification | all | coding, verification | — |
| `run_script` | all | — | all | — | all | — |
| `run_team_plan` | all | — | — | — | — | — |
| `scout` | all | all | — | — | — | — |
| `search_sources` | all | all | all | all | all | all |
| `send_message` | all | all | all | all | all | — |
| `shell` | all | — | coding, verification | all | coding, verification | — |
| `submit_team_plan` | all | — | — | — | — | — |
| `submit_verdict` | — | — | — | all | — | — |
| `task_control` | all | all | — | — | — | — |
| `task_detail` | all | all | all | all | all | — |
| `team_status` | all | all | all | all | all | all |
| `todo_write` | all | — | — | — | — | — |
| `tool_help` | all | all | all | all | all | — |
| `tool_search` | all | all | all | all | all | — |
| `use_skill` | all | all | all | all | all | — |
| `wait_for` | all | — | coding, verification | all | coding, verification | — |
| `wait_for_gate` | all | all | all | — | — | — |
| `web_search` | all | research | research | research | research | — |
| `write_file` | all | — | coding, research | — | coding, research | — |

## Notes

- `delegate` is role-constrained: Leader → Manager, Manager → Worker,
  Worker → Labour. Labour/Verifier/Scout have no delegation tool.
- `delegate_acp` is Leader/Manager only. `delegate_many` and `scout` are
  Manager only; `submit_verdict` is Verifier only; the plan tools are Leader
  only.
- `browser` and `computer` require the packaged helper/permissions and the
  daemon-wide GUI lease. They are advertised only to Leader and coding
  Worker/Labour.
- `fetch_url`, `web_search`, `local_http`, `remote_*`, `media_transcode`,
  `archive_extract`, `lsp_query`, and `read_pdf`/`read_image` may return a
  typed "unavailable" error when the required provider/tool/endpoint is not
  configured. Availability is not a claim that the integration is installed.
- Manager/Verifier read-only means no workspace write/edit/shell; they still
  have orchestration/artifact/memory surfaces where listed.
- Scout is deliberately read-only and never receives shell, write, process,
  delegation, artifact publishing, MCP, browser, or computer tools.
