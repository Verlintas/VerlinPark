import Foundation
@testable import VerlinPark
import XCTest

final class ProtocolCompatibilityTests: XCTestCase {
    func testAgentInfoWithoutCapacityStillDecodes() throws {
        let json = """
        {
          "configured": false,
          "provider": null,
          "model": null,
          "base_url": null,
          "role_models": {
            "leader": {"context_window": 1000000},
            "manager": {},
            "worker": {"context_window": 8000},
            "verifier": {},
            "labour": {}
          },
          "context_window": 128000
        }
        """
        let info = try JSONDecoder().decode(AgentInfo.self, from: Data(json.utf8))
        XCTAssertFalse(info.configured)
        XCTAssertNil(info.capacity)
        XCTAssertEqual(info.contextWindow, 128_000)
        XCTAssertEqual(info.roleModels.leader.contextWindow, 1_000_000)
        XCTAssertEqual(info.roleModels.worker.contextWindow, 8000)
    }

    func testSessionContextStatusDecodes() throws {
        let json = """
        {
          "session_id": "session-1",
          "usage": {
            "estimated_tokens": 91000,
            "context_window": 128000,
            "threshold_tokens": 90000,
            "hard_limit_tokens": 120000,
            "messages": 42,
            "created_at": "2026-11-08T00:00:00Z"
          },
          "last_compaction": {
            "mode": "summary",
            "summarized_messages": 18,
            "dropped_messages": 0,
            "kept_messages": 4,
            "estimated_tokens_before": 110000,
            "estimated_tokens_after": 12000,
            "created_at": "2026-11-08T00:00:01Z"
          }
        }
        """
        let status = try JSONDecoder().decode(
            SessionContextStatus.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(status.sessionID, "session-1")
        XCTAssertEqual(status.usage?.estimatedTokens, 91000)
        XCTAssertEqual(status.usage?.contextWindow, 128_000)
        XCTAssertEqual(status.lastCompaction?.mode, "summary")
        XCTAssertEqual(status.lastCompaction?.summarizedMessages, 18)
        // Legacy events predate calibration and provider measurements.
        XCTAssertNil(status.usage?.calibration)
        XCTAssertNil(status.lastMeasurement)
    }

    func testContextMeasurementDecodes() throws {
        let json = """
        {
          "session_id": "session-1",
          "usage": {
            "estimated_tokens": 91000,
            "context_window": 128000,
            "threshold_tokens": 90000,
            "hard_limit_tokens": 120000,
            "messages": 42,
            "calibration": 1.25,
            "created_at": "2026-11-08T00:00:00Z"
          },
          "last_compaction": null,
          "last_measurement": {
            "estimated_tokens": 80000,
            "actual_input_tokens": 100000,
            "actual_output_tokens": 512,
            "ratio": 1.25,
            "calibration": 1.25,
            "samples": 3,
            "model": "test-model",
            "messages": 42,
            "created_at": "2026-11-08T00:00:02Z"
          }
        }
        """
        let status = try JSONDecoder().decode(
            SessionContextStatus.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(status.usage?.calibration, 1.25)
        XCTAssertEqual(status.lastMeasurement?.estimatedTokens, 80000)
        XCTAssertEqual(status.lastMeasurement?.actualInputTokens, 100_000)
        XCTAssertEqual(status.lastMeasurement?.calibration, 1.25)
        XCTAssertEqual(status.lastMeasurement?.samples, 3)
    }

    func testUpdateCheckResultDecodes() throws {
        let json = """
        {
          "current_version": "0.1.0-dev",
          "latest_version": "0.2.0",
          "update_available": true,
          "repo": "owner/repo",
          "release_name": "VerlinPark 0.2.0",
          "release_url": "https://example.com/release",
          "published_at": "2026-11-08T00:00:00Z"
        }
        """
        let result = try JSONDecoder().decode(UpdateCheckResult.self, from: Data(json.utf8))
        XCTAssertTrue(result.updateAvailable)
        XCTAssertEqual(result.latestVersion, "0.2.0")
        XCTAssertEqual(result.repo, "owner/repo")
    }

    func testSessionForkTreeResultDecodes() throws {
        let json = """
        {
          "session_id": "root-1",
          "total": 2,
          "truncated": false,
          "root": {
            "session": {
              "id": "root-1",
              "project_id": "project-1",
              "goal": "root",
              "status": "idle",
              "priority": 0,
              "worktree": "/tmp/root",
              "branch": "verlinpark/root-1",
              "token_budget": null,
              "cost_budget_usd": null,
              "time_budget_secs": null,
              "cost_spent_usd": 0.0,
              "base_commit": "aaa111",
              "tokens_in": 0,
              "tokens_out": 0,
              "created_at": "2026-11-08T00:00:00Z"
            },
            "checkpoint_id": null,
            "commit": null,
            "children": [
              {
                "session": {
                  "id": "fork-1",
                  "project_id": "project-1",
                  "goal": "fork",
                  "status": "draft",
                  "priority": 0,
                  "worktree": "/tmp/fork",
                  "branch": "verlinpark/fork-1",
                  "token_budget": null,
                  "cost_budget_usd": null,
                  "time_budget_secs": null,
                  "cost_spent_usd": 0.0,
                  "base_commit": "aaa111",
                  "tokens_in": 0,
                  "tokens_out": 0,
                  "created_at": "2026-11-08T00:00:01Z"
                },
                "checkpoint_id": "checkpoint-1",
                "commit": "aaa111",
                "children": []
              }
            ]
          }
        }
        """
        let result = try JSONDecoder().decode(SessionForkTreeResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.total, 2)
        XCTAssertEqual(result.root.children.first?.session.id, "fork-1")
        XCTAssertEqual(result.root.children.first?.checkpointID, "checkpoint-1")
    }

    func testSessionMergeForkResultDecodes() throws {
        let json = """
        {
          "source_session_id": "source-1",
          "fork_session_id": "fork-1",
          "status": "merged",
          "pre_merge_commit": "aaa111",
          "fork_commit": "bbb222",
          "merge_commit": "ccc333",
          "conflicts": [],
          "detail": "diff check passed"
        }
        """
        let result = try JSONDecoder().decode(SessionMergeForkResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.sourceSessionID, "source-1")
        XCTAssertEqual(result.forkSessionID, "fork-1")
        XCTAssertEqual(result.status, "merged")
        XCTAssertEqual(result.mergeCommit, "ccc333")
        XCTAssertTrue(result.conflicts.isEmpty)
    }

    func testSessionCompareResultDecodes() throws {
        let json = """
        {
          "session_id": "left",
          "other_session_id": "right",
          "left_commit": "aaa111",
          "right_commit": "bbb222",
          "files": [
            {"path": "Sources/App.swift", "additions": 12, "deletions": 3, "binary": false}
          ],
          "patch": "diff --git a/Sources/App.swift b/Sources/App.swift\\n",
          "truncated": false
        }
        """
        let result = try JSONDecoder().decode(SessionCompareResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.sessionID, "left")
        XCTAssertEqual(result.otherSessionID, "right")
        XCTAssertEqual(result.files.first?.path, "Sources/App.swift")
        XCTAssertEqual(result.files.first?.additions, 12)
        XCTAssertFalse(result.truncated)
    }

    func testSessionLineageResultDecodes() throws {
        let json = """
        {
          "session_id": "fork-1",
          "parent": {
            "session": {
              "id": "source-1",
              "project_id": "project-1",
              "goal": "source",
              "status": "idle",
              "priority": 0,
              "worktree": "/tmp/source",
              "branch": "verlinpark/source-1",
              "token_budget": null,
              "cost_budget_usd": null,
              "time_budget_secs": null,
              "cost_spent_usd": 0.0,
              "base_commit": "aaa111",
              "tokens_in": 0,
              "tokens_out": 0,
              "created_at": "2026-11-08T00:00:00Z"
            },
            "checkpoint_id": "checkpoint-1",
            "commit": "aaa111"
          },
          "siblings": [],
          "children": []
        }
        """
        let result = try JSONDecoder().decode(SessionLineageResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.sessionID, "fork-1")
        XCTAssertEqual(result.parent?.session.id, "source-1")
        XCTAssertEqual(result.parent?.checkpointID, "checkpoint-1")
        XCTAssertEqual(result.parent?.commit, "aaa111")
        XCTAssertTrue(result.siblings.isEmpty)
    }

    func testSessionForkResultDecodes() throws {
        let json = """
        {
          "source_session_id": "source-1",
          "checkpoint_id": "checkpoint-1",
          "session": {
            "id": "fork-1",
            "project_id": "project-1",
            "goal": "forked task",
            "status": "draft",
            "priority": 5,
            "worktree": "/tmp/worktree",
            "branch": "verlinpark/fork-1",
            "token_budget": 100000,
            "cost_budget_usd": 2.5,
            "time_budget_secs": 3600,
            "cost_spent_usd": 0.0,
            "base_commit": "abc123",
            "tokens_in": 0,
            "tokens_out": 0,
            "created_at": "2026-11-08T00:00:00Z"
          }
        }
        """
        let result = try JSONDecoder().decode(SessionForkResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.sourceSessionID, "source-1")
        XCTAssertEqual(result.checkpointID, "checkpoint-1")
        XCTAssertEqual(result.session.id, "fork-1")
        XCTAssertEqual(result.session.baseCommit, "abc123")
        XCTAssertEqual(result.session.branch, "verlinpark/fork-1")
    }

    func testSessionCheckpointDecodes() throws {
        let json = """
        {
          "id": "checkpoint-1",
          "session_id": "session-1",
          "label": "before fork",
          "commit": "abc123",
          "branch": "verlinpark/session-1",
          "start_message_id": "message-1",
          "through_message_id": "message-4",
          "message_count": 4,
          "summary": "Earlier conversation summary",
          "dirty": true,
          "created_at": "2026-11-08T00:00:00Z"
        }
        """
        let checkpoint = try JSONDecoder().decode(SessionCheckpoint.self, from: Data(json.utf8))
        XCTAssertEqual(checkpoint.id, "checkpoint-1")
        XCTAssertEqual(checkpoint.messageCount, 4)
        XCTAssertEqual(checkpoint.startMessageID, "message-1")
        XCTAssertEqual(checkpoint.summary, "Earlier conversation summary")
        XCTAssertEqual(checkpoint.dirty, true)
    }

    func testCheckpointPolicyResultDecodes() throws {
        let json = """
        {
          "project_id": "project-1",
          "policy": {
            "enabled": true,
            "older_than_secs": 2592000,
            "keep_latest": 5
          }
        }
        """
        let result = try JSONDecoder().decode(CheckpointPolicyResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.projectId, "project-1")
        XCTAssertTrue(result.policy.enabled)
        XCTAssertEqual(result.policy.olderThanSecs, 2_592_000)
        XCTAssertEqual(result.policy.keepLatest, 5)
    }

    func testCheckpointPruneResultDecodes() throws {
        let json = """
        {
          "session_id": "session-1",
          "candidates": 2,
          "removed": 2,
          "dry_run": false,
          "checkpoints": [
            {
              "id": "checkpoint-1",
              "session_id": "session-1",
              "label": "old",
              "commit": "abc123",
              "branch": "verlinpark/session-1",
              "message_count": 2,
              "summary": null,
              "dirty": false,
              "created_at": "2026-11-01T00:00:00Z"
            }
          ]
        }
        """
        let result = try JSONDecoder().decode(SessionCheckpointPruneResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.candidates, 2)
        XCTAssertEqual(result.removed, 2)
        XCTAssertEqual(result.checkpoints.first?.id, "checkpoint-1")
        XCTAssertFalse(result.dryRun)
    }

    func testSessionCompactResultDecodes() throws {
        let json = """
        {
          "session_id": "session-1",
          "compacted": true,
          "compaction": {
            "mode": "summary",
            "summarized_messages": 18,
            "dropped_messages": 0,
            "kept_messages": 4,
            "estimated_tokens_before": 110000,
            "estimated_tokens_after": 12000,
            "created_at": "2026-11-08T00:00:01Z"
          }
        }
        """
        let result = try JSONDecoder().decode(
            SessionCompactResult.self,
            from: Data(json.utf8)
        )
        XCTAssertTrue(result.compacted)
        XCTAssertEqual(result.compaction?.mode, "summary")
        XCTAssertEqual(result.compaction?.estimatedTokensAfter, 12000)
    }

    func testUsageTotalsWithoutDepartmentBucketStillDecodes() throws {
        let json = """
        {
          "session_id": "session-1",
          "entries": [],
          "totals": {
            "calls": 0,
            "input_tokens": 0,
            "output_tokens": 0,
            "cost_usd": 0,
            "by_role": [],
            "by_model": [],
            "by_agent": [],
            "by_task": []
          }
        }
        """
        let ledger = try JSONDecoder().decode(UsageLedgerResult.self, from: Data(json.utf8))
        XCTAssertEqual(ledger.totals.calls, 0)
        XCTAssertNil(ledger.totals.byDepartment)
    }

    func testSearchHitDecodesEventAndMessageShapes() throws {
        let json = """
        {
          "id": "session-1:7",
          "session_id": "session-1",
          "project_id": "project-1",
          "session_goal": "find it",
          "source": "event",
          "kind": "tool.finished",
          "seq": 7,
          "snippet": "needle",
          "created_at": "2026-10-31T00:00:00Z"
        }
        """
        let hit = try JSONDecoder().decode(SearchHit.self, from: Data(json.utf8))
        XCTAssertEqual(hit.source, "event")
        XCTAssertEqual(hit.detail, "tool.finished")
        XCTAssertEqual(hit.seq, 7)
        XCTAssertNil(hit.role)
    }

    func testMcpServerAndToolsDecode() throws {
        let serverJSON = """
        {
          "servers": [{
            "id": "server-1",
            "name": "files",
            "command": "mcp-files",
            "args": ["--root", "/tmp"],
            "env": {"TOKEN": "hidden"},
            "enabled": true,
            "created_at": "2026-10-31T00:00:00Z"
          }]
        }
        """
        let servers = try JSONDecoder().decode(
            McpServerListResult.self,
            from: Data(serverJSON.utf8)
        )
        XCTAssertEqual(servers.servers.first?.name, "files")
        XCTAssertEqual(servers.servers.first?.args, ["--root", "/tmp"])

        let toolsJSON = """
        {
          "server_id": "server-1",
          "tools": [{
            "name": "echo",
            "description": "Echo text",
            "input_schema": {"type": "object"}
          }]
        }
        """
        let tools = try JSONDecoder().decode(
            McpToolListResult.self,
            from: Data(toolsJSON.utf8)
        )
        XCTAssertEqual(tools.tools.first?.name, "echo")
        XCTAssertEqual(tools.tools.first?.description, "Echo text")
    }

    func testAcpAgentsAndProbeDecode() throws {
        let agentsJSON = """
        {
          "agents": [{
            "id": "agent-1",
            "name": "gemini",
            "command": "npx",
            "args": ["-y", "@google/gemini-cli"],
            "env": {"GEMINI_API_KEY": "hidden"},
            "enabled": true,
            "auto_approve": false,
            "timeout_secs": 900,
            "created_at": "2026-11-07T00:00:00Z"
          }]
        }
        """
        let agents = try JSONDecoder().decode(
            AcpAgentListResult.self,
            from: Data(agentsJSON.utf8)
        )
        XCTAssertEqual(agents.agents.first?.name, "gemini")
        XCTAssertEqual(agents.agents.first?.timeoutSecs, 900)
        XCTAssertEqual(agents.agents.first?.autoApprove, false)

        let probeJSON = """
        {
          "agent_id": "agent-1",
          "agent_name": "gemini",
          "version": "0.12.0",
          "protocol_version": 1,
          "session_created": true
        }
        """
        let probe = try JSONDecoder().decode(
            AcpProbeResult.self,
            from: Data(probeJSON.utf8)
        )
        XCTAssertEqual(probe.agentName, "gemini")
        XCTAssertEqual(probe.protocolVersion, 1)
        XCTAssertTrue(probe.sessionCreated)
    }

    func testMcpResourcesAndPromptsDecode() throws {
        let resourcesJSON = """
        {
          "server_id": "server-1",
          "resources": [{
            "uri": "file:///notes.md",
            "name": "notes",
            "description": "Project notes",
            "mime_type": "text/markdown"
          }],
          "templates": [{
            "uri_template": "file:///{path}",
            "name": "file",
            "description": "Read a file",
            "mime_type": "text/plain"
          }]
        }
        """
        let resources = try JSONDecoder().decode(
            McpResourceListResult.self,
            from: Data(resourcesJSON.utf8)
        )
        XCTAssertEqual(resources.resources.first?.uri, "file:///notes.md")
        XCTAssertEqual(resources.resources.first?.mimeType, "text/markdown")
        XCTAssertEqual(resources.templates.first?.uriTemplate, "file:///{path}")

        let promptsJSON = """
        {
          "server_id": "server-1",
          "prompts": [{
            "name": "review",
            "description": "Review code",
            "arguments": [{
              "name": "target",
              "description": "Path",
              "required": true
            }]
          }]
        }
        """
        let prompts = try JSONDecoder().decode(
            McpPromptListResult.self,
            from: Data(promptsJSON.utf8)
        )
        XCTAssertEqual(prompts.prompts.first?.name, "review")
        XCTAssertTrue(prompts.prompts.first?.arguments.first?.required == true)
    }

    func testShellPolicyWithoutRestrictHomeStillDecodes() throws {
        let json = #"{"network":"allow","read_deny":["~/secret"]}"#
        let policy = try JSONDecoder().decode(ShellPolicy.self, from: Data(json.utf8))
        XCTAssertEqual(policy.network, "allow")
        XCTAssertEqual(policy.readDeny, ["~/secret"])
        XCTAssertFalse(policy.restrictHome)
    }

    func testQuestionListDecodes() throws {
        let json = """
        {
          "questions": [{
            "id": "question-1",
            "session_id": "session-1",
            "agent_id": "leader",
            "agent_name": "Leader",
            "question": "Proceed?",
            "options": ["yes", "no"],
            "status": "pending",
            "answer": null,
            "created_at": "2026-10-31T00:00:00Z",
            "answered_at": null
          }]
        }
        """
        let result = try JSONDecoder().decode(QuestionListResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.questions.first?.question, "Proceed?")
        XCTAssertTrue(result.questions.first?.isPending == true)
    }

    func testGitLogAndShowDecode() throws {
        let logJSON = """
        {
          "session_id": "session-1",
          "branch": "verlinpark/session",
          "base_commit": "abc123",
          "commits": [{
            "hash": "deadbeef",
            "short_hash": "deadbee",
            "author": "Test User",
            "email": "test@example.com",
            "author_date": "2026-11-02T00:00:00Z",
            "subject": "Ship history",
            "refs": "HEAD -> verlinpark/session"
          }]
        }
        """
        let log = try JSONDecoder().decode(GitLogResult.self, from: Data(logJSON.utf8))
        XCTAssertEqual(log.branch, "verlinpark/session")
        XCTAssertEqual(log.commits.first?.shortHash, "deadbee")
        XCTAssertEqual(log.commits.first?.subject, "Ship history")

        let show = try JSONDecoder().decode(
            GitShowResult.self,
            from: Data(
                #"{"session_id":"session-1","commit":"deadbeef","output":"diff --git a/x b/x","truncated":false}"#
                    .utf8
            )
        )
        XCTAssertEqual(show.commit, "deadbeef")
        XCTAssertTrue(show.output.contains("diff --git"))
    }

    func testWorkspaceTreeAndFileDecode() throws {
        let treeJSON = """
        {
          "root": "/tmp/worktree",
          "entries": [
            {"path": "src", "name": "src", "kind": "dir", "size": 0},
            {"path": "src/main.rs", "name": "main.rs", "kind": "file", "size": 42}
          ],
          "truncated": false
        }
        """
        let tree = try JSONDecoder().decode(WorkspaceTreeResult.self, from: Data(treeJSON.utf8))
        XCTAssertEqual(tree.entries.count, 2)
        XCTAssertTrue(tree.entries[0].isDirectory)
        XCTAssertEqual(tree.entries[1].path, "src/main.rs")
        XCTAssertEqual(tree.entries[1].size, 42)

        let fileJSON = """
        {
          "path": "src/main.rs",
          "size": 42,
          "truncated": false,
          "binary": false,
          "content": "fn main() {}\\n",
          "sha256": "abc123",
          "editable": true
        }
        """
        let file = try JSONDecoder().decode(WorkspaceFileResult.self, from: Data(fileJSON.utf8))
        XCTAssertEqual(file.path, "src/main.rs")
        XCTAssertFalse(file.binary)
        XCTAssertFalse(file.truncated)
        XCTAssertEqual(file.sha256, "abc123")
        XCTAssertTrue(file.editable)

        let written = try JSONDecoder().decode(
            WorkspaceWriteResult.self,
            from: Data(#"{"path":"src/main.rs","size":14,"sha256":"def456"}"#.utf8)
        )
        XCTAssertEqual(written.size, 14)
        XCTAssertEqual(written.sha256, "def456")
    }

    func testTaskSchedulerPolicyDecodes() throws {
        let result = try JSONDecoder().decode(
            TaskSchedulerPolicyResult.self,
            from: Data(
                #"{"project_id":"project-1","policy":{"enabled":true,"cooldown_secs":15}}"#
                    .utf8
            )
        )
        XCTAssertEqual(result.projectID, "project-1")
        XCTAssertTrue(result.policy.enabled)
        XCTAssertEqual(result.policy.cooldownSecs, 15)
        XCTAssertFalse(result.policy.dispatchTasks)

        let perTask = try JSONDecoder().decode(
            TaskSchedulerPolicyResult.self,
            from: Data(
                #"{"project_id":"project-1","policy":{"enabled":true,"cooldown_secs":15,"dispatch_tasks":true}}"#
                    .utf8
            )
        )
        XCTAssertTrue(perTask.policy.dispatchTasks)
    }

    func testShellPolicyReadsOptionalReadJail() throws {
        let legacy = try JSONDecoder().decode(
            ShellPolicy.self,
            from: Data(#"{"network":"allow","read_deny":[],"restrict_home":true}"#.utf8)
        )
        XCTAssertTrue(legacy.restrictHome)
        XCTAssertFalse(legacy.readJail)

        let full = try JSONDecoder().decode(
            ShellPolicy.self,
            from: Data(
                #"{"network":"deny","read_deny":["~/secrets"],"restrict_home":false,"read_jail":true}"#
                    .utf8
            )
        )
        XCTAssertTrue(full.readJail)
        XCTAssertEqual(full.readDeny, ["~/secrets"])
    }

    func testTaskActionResultDecodes() throws {
        let json = """
        {
          "task": {
            "session_id": "session-1",
            "project_id": "project-1",
            "plan_seq": 4,
            "task_id": "T1",
            "department": "Implementation",
            "title": "Build",
            "priority": 10,
            "status": "skipped",
            "deps": [],
            "write_scope": ["src"],
            "attempts": 1,
            "enqueued_at": "2026-11-06T12:00:00Z",
            "started_at": "2026-11-06T12:01:00Z",
            "finished_at": "2026-11-06T12:02:00Z",
            "last_error": "skipped by user"
          }
        }
        """
        let result = try JSONDecoder().decode(TaskActionResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.task.taskID, "T1")
        XCTAssertEqual(result.task.status, "skipped")
        XCTAssertEqual(result.task.lastError, "skipped by user")
    }

    func testTaskQueueResultsDecode() throws {
        let json = """
        {
          "tasks": [{
            "session_id": "session-1",
            "project_id": "project-1",
            "plan_seq": 4,
            "task_id": "T1",
            "department": "Implementation",
            "title": "Build the scheduler",
            "priority": 10,
            "status": "running",
            "deps": ["T0"],
            "write_scope": ["src/scheduler"],
            "attempts": 1,
            "enqueued_at": "2026-11-06T12:00:00Z",
            "started_at": "2026-11-06T12:01:00Z",
            "finished_at": null,
            "last_error": null
          }],
          "leases": [{
            "project_id": "project-1",
            "session_id": "session-1",
            "agent_id": "agent-1",
            "scopes": ["src/scheduler"],
            "acquired_at": "2026-11-06T12:01:00Z"
          }]
        }
        """
        let result = try JSONDecoder().decode(TaskQueueResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.tasks.count, 1)
        XCTAssertEqual(result.tasks[0].taskID, "T1")
        XCTAssertEqual(result.tasks[0].planSeq, 4)
        XCTAssertEqual(result.tasks[0].attempts, 1)
        XCTAssertEqual(result.tasks[0].writeScope, ["src/scheduler"])
        XCTAssertEqual(result.leases[0].agentID, "agent-1")
    }

    func testTerminalResultsDecode() throws {
        let opened = try JSONDecoder().decode(
            TerminalOpenResult.self,
            from: Data(#"{"terminal_id":"terminal-1","worktree":"/tmp/repo"}"#.utf8)
        )
        XCTAssertEqual(opened.terminalId, "terminal-1")

        let readJSON = """
        {
          "terminal_id": "terminal-1",
          "chunks": [{"seq": 1, "data": "aGVsbG8="}],
          "next_seq": 1,
          "status": "running",
          "exit_code": null,
          "dropped": 0
        }
        """
        let read = try JSONDecoder().decode(TerminalReadResult.self, from: Data(readJSON.utf8))
        XCTAssertEqual(read.chunks.first?.data, "aGVsbG8=")
        XCTAssertEqual(read.status, "running")
        XCTAssertNil(read.exitCode)

        let closed = try JSONDecoder().decode(
            TerminalCloseResult.self,
            from: Data(#"{"closed":true,"exit_code":0}"#.utf8)
        )
        XCTAssertTrue(closed.closed)
        XCTAssertEqual(closed.exitCode, 0)
    }

    func testConsoleResultsDecode() throws {
        let opened = try JSONDecoder().decode(
            ConsoleOpenResult.self,
            from: Data(#"{"console_id":"console-1","worktree":"/tmp/repo"}"#.utf8)
        )
        XCTAssertEqual(opened.consoleId, "console-1")
        XCTAssertEqual(opened.worktree, "/tmp/repo")

        let readJSON = """
        {
          "console_id": "console-1",
          "chunks": [
            {"seq": 1, "stream": "stdout", "text": "hello\\n"}
          ],
          "next_seq": 1,
          "status": "exited",
          "exit_code": 0,
          "dropped": 0
        }
        """
        let read = try JSONDecoder().decode(ConsoleReadResult.self, from: Data(readJSON.utf8))
        XCTAssertEqual(read.chunks.first?.text, "hello\n")
        XCTAssertEqual(read.nextSeq, 1)
        XCTAssertEqual(read.status, "exited")
        XCTAssertEqual(read.exitCode, 0)

        let closed = try JSONDecoder().decode(
            ConsoleCloseResult.self,
            from: Data(#"{"closed":true,"exit_code":0}"#.utf8)
        )
        XCTAssertTrue(closed.closed)
        XCTAssertEqual(closed.exitCode, 0)
    }

    func testIntegerClamping() {
        XCTAssertEqual(99.clamped(to: 1 ... 5), 5)
        XCTAssertEqual((-3).clamped(to: 1 ... 5), 1)
        XCTAssertEqual(3.clamped(to: 1 ... 5), 3)
    }

    func testApprovalNotificationIDExtraction() {
        XCTAssertEqual(
            SystemNotifier.approvalID(from: "approval-abc-123"),
            "abc-123"
        )
        XCTAssertNil(SystemNotifier.approvalID(from: "task-session-task-failed-1"))
        XCTAssertNil(SystemNotifier.approvalID(from: "approval-"))
    }

    func testSessionPriorityDecodes() throws {
        let json = """
        {
          "id": "session-1",
          "project_id": "project-1",
          "goal": "ship it",
          "status": "running",
          "priority": 25,
          "worktree": null,
          "branch": null,
          "token_budget": 100000,
          "cost_budget_usd": null,
          "time_budget_secs": null,
          "cost_spent_usd": 0,
          "base_commit": null,
          "tokens_in": 0,
          "tokens_out": 0,
          "created_at": "2026-11-01T00:00:00Z"
        }
        """
        let session = try JSONDecoder().decode(Session.self, from: Data(json.utf8))
        XCTAssertEqual(session.priority, 25)

        let legacy = json.replacingOccurrences(of: #""priority": 25,"#, with: "")
        let oldSession = try JSONDecoder().decode(Session.self, from: Data(legacy.utf8))
        XCTAssertNil(oldSession.priority)

        let updated = try JSONDecoder().decode(
            SessionPriorityResult.self,
            from: Data(#"{"session_id":"session-1","priority":40}"#.utf8)
        )
        XCTAssertEqual(updated.priority, 40)
    }

    func testMemoryListDecodes() throws {
        let json = """
        {
          "memories": [{
            "id": "memory-1",
            "session_id": "session-1",
            "project_id": "project-1",
            "agent_id": "leader",
            "agent_name": "Leader",
            "kind": "decision",
            "title": "Keep merges deterministic",
            "body": "No LLM in the Integrator.",
            "tags": ["architecture"],
            "created_at": "2026-10-31T00:00:00Z"
          }]
        }
        """
        let result = try JSONDecoder().decode(MemoryListResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.memories.first?.title, "Keep merges deterministic")
        XCTAssertEqual(result.memories.first?.tags, ["architecture"])
    }

    func testMemoryDeleteAndPruneDecode() throws {
        let deleted = try JSONDecoder().decode(
            MemoryDeleteResult.self,
            from: Data(#"{"memory_id":"memory-1","removed":true}"#.utf8)
        )
        XCTAssertEqual(deleted.memoryId, "memory-1")
        XCTAssertTrue(deleted.removed)

        let pruned = try JSONDecoder().decode(
            MemoryPruneResult.self,
            from: Data(#"{"candidates":7,"removed":5,"dry_run":false}"#.utf8)
        )
        XCTAssertEqual(pruned.candidates, 7)
        XCTAssertEqual(pruned.removed, 5)
        XCTAssertFalse(pruned.dryRun)
    }

    func testMemoryPolicyDecode() throws {
        let json = """
        {
          "project_id": "project-1",
          "policy": {
            "enabled": true,
            "max_items": 500,
            "max_age_days": 30
          }
        }
        """
        let result = try JSONDecoder().decode(MemoryPolicyResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.projectId, "project-1")
        XCTAssertTrue(result.policy.enabled)
        XCTAssertEqual(result.policy.maxItems, 500)
        XCTAssertEqual(result.policy.maxAgeDays, 30)

        let unbounded = try JSONDecoder().decode(
            MemoryPolicy.self,
            from: Data(#"{"enabled":false,"max_items":null,"max_age_days":null}"#.utf8)
        )
        XCTAssertFalse(unbounded.enabled)
        XCTAssertNil(unbounded.maxItems)
        XCTAssertNil(unbounded.maxAgeDays)
    }

    func testAppNoticeRoundTrip() throws {
        let notice = AppNotice(
            id: "approval-1",
            sessionId: "session-1",
            projectId: "project-1",
            kind: "approval",
            title: "Approval needed",
            detail: "shell · Worker",
            createdAt: "2026-10-31T00:00:00Z",
            isRead: false
        )
        let data = try JSONEncoder().encode(notice)
        let decoded = try JSONDecoder().decode(AppNotice.self, from: data)
        XCTAssertEqual(decoded, notice)
    }

    func testSessionPlanResumeResultDecodes() throws {
        let json = #"{"session_id":"session-1","started":true}"#
        let result = try JSONDecoder().decode(
            SessionPlanResumeResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.sessionId, "session-1")
        XCTAssertTrue(result.started)
    }

    func testProjectUsageResultDecodes() throws {
        let json = """
        {
          "project_id": "project-1",
          "sessions": [
            {
              "session_id": "session-1",
              "goal": "ship it",
              "status": "idle",
              "calls": 2,
              "input_tokens": 10,
              "output_tokens": 5,
              "cost_usd": 0.25
            }
          ],
          "totals": {
            "calls": 2,
            "input_tokens": 10,
            "output_tokens": 5,
            "cost_usd": 0.25,
            "by_role": [],
            "by_model": [],
            "by_agent": [],
            "by_task": []
          }
        }
        """
        let result = try JSONDecoder().decode(ProjectUsageResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.projectId, "project-1")
        XCTAssertEqual(result.sessions.first?.totalTokens, 15)
        XCTAssertEqual(result.totals.calls, 2)
    }

    func testUsageCsvResultDecodes() throws {
        let json = """
        {
          "session_id": "session-1",
          "csv": "created_at,agent_name\\n2026-10-31T00:00:00Z,Leader\\n"
        }
        """
        let result = try JSONDecoder().decode(UsageCsvResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.sessionId, "session-1")
        XCTAssertTrue(result.csv.hasPrefix("created_at,agent_name"))
    }

    func testPlanTaskWithoutBudgetsStillDecodes() throws {
        let json = """
        {
          "id": "T1",
          "department": "impl",
          "title": "Build",
          "kind": "coding",
          "deps": [],
          "write_scope": ["src"],
          "acceptance": ["done"]
        }
        """
        let task = try JSONDecoder().decode(PlanTask.self, from: Data(json.utf8))
        XCTAssertNil(task.tokenBudget)
        XCTAssertNil(task.costBudgetUsd)
    }
}
