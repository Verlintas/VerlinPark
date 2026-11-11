import Foundation
@testable import VerlinPark
import XCTest

final class ProtocolCompatibilityTests: XCTestCase {
    func testLegacyEmbeddingPolicyDefaultsToExactScanIndex() throws {
        let policy = try JSONDecoder().decode(
            MemoryEmbeddingPolicy.self,
            from: Data(
                """
                {
                  "enabled": true,
                  "endpoint": "https://example.test/v1",
                  "model": "embed"
                }
                """.utf8
            )
        )
        XCTAssertEqual(policy.index, .scan)
        XCTAssertEqual(policy.ivfNlist, 64)
        XCTAssertEqual(policy.ivfNprobe, 8)
        XCTAssertEqual(policy.ivfMinVectors, 256)
        XCTAssertEqual(policy.ivfRebuildVectors, 4096)
        XCTAssertFalse(policy.usesIVF)
        XCTAssertEqual(policy.jsonObject()["index"]?.stringValue, "scan")
    }

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
            "worker": {"context_window": 8000, "cache_read_usd_per_million": 0.2, "cache_write_usd_per_million": 2.5},
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
        XCTAssertEqual(info.roleModels.worker.cacheReadUsdPerMillion, 0.2)
        XCTAssertEqual(info.roleModels.worker.cacheWriteUsdPerMillion, 2.5)
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

    func testProjectForkForestDecodes() throws {
        let json = """
        {
          "project_id": "project-1",
          "total": 1,
          "checkpoints": 2,
          "dirty_checkpoints": 1,
          "truncated": false,
          "roots": [
            {
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
              "children": []
            }
          ]
        }
        """
        let result = try JSONDecoder().decode(ProjectForkForestResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.total, 1)
        XCTAssertEqual(result.checkpoints, 2)
        XCTAssertEqual(result.dirtyCheckpoints, 1)
        XCTAssertEqual(result.roots.first?.session.id, "root-1")
    }

    func testForkAuditResultDecodes() throws {
        let json = """
        {
          "project_id": "project-1",
          "sessions": 3,
          "checkpoints": 2,
          "issues": [
            {
              "kind": "missing_fork_link",
              "session_id": "parent-1",
              "related_session_id": "child-1",
              "checkpoint_id": "checkpoint-1",
              "commit": "abc123",
              "detail": "missing source event",
              "repairable": true
            }
          ]
        }
        """
        let result = try JSONDecoder().decode(ForkAuditResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.issues.count, 1)
        XCTAssertEqual(result.issues.first?.kind, "missing_fork_link")
        XCTAssertEqual(result.issues.first?.relatedSessionID, "child-1")
        XCTAssertTrue(result.issues.first?.repairable == true)
    }

    func testCheckpointStatsDecodes() throws {
        let json = """
        {
          "session_id": "session-1",
          "checkpoints": 2,
          "dirty": 1,
          "refs": 1,
          "commit_bytes": 512,
          "snapshot_bytes": 4096,
          "unique_blobs": 2
        }
        """
        let result = try JSONDecoder().decode(SessionCheckpointStatsResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.checkpoints, 2)
        XCTAssertEqual(result.dirty, 1)
        XCTAssertEqual(result.snapshotBytes, 4096)
    }

    func testModelPresetDecodes() throws {
        let json = """
        {
          "id": "preset-1",
          "name": "Local 8k",
          "provider": "openai-compatible",
          "model": "local-8k",
          "context_window": 8000,
          "input_usd_per_million": 0.0,
          "output_usd_per_million": 0.0,
          "notes": "local",
          "created_at": "2026-11-08T00:00:00Z"
        }
        """
        let preset = try JSONDecoder().decode(ModelPreset.self, from: Data(json.utf8))
        XCTAssertEqual(preset.name, "Local 8k")
        XCTAssertEqual(preset.contextWindow, 8000)
        XCTAssertEqual(preset.provider, "openai-compatible")
    }

    func testUpdateDownloadResultDecodes() throws {
        let json = """
        {
          "version": "0.2.0",
          "path": "/Users/test/Downloads/VerlinPark-0.2.0.dmg",
          "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          "size": 123456,
          "verified": true
        }
        """
        let result = try JSONDecoder().decode(UpdateDownloadResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.version, "0.2.0")
        XCTAssertTrue(result.verified)
        XCTAssertEqual(result.size, 123_456)
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
        XCTAssertNil(ledger.totals.cacheReadTokens)
    }

    func testUsageCacheFieldsDecodeAndDeriveHitRate() throws {
        let json = """
        {
          "session_id": "session-1",
          "entries": [{
            "id": "usage-1",
            "session_id": "session-1",
            "agent_id": "agent-1",
            "agent_name": "Worker",
            "role": "worker",
            "task_id": "T1",
            "model": "test-model",
            "input_tokens": 1000,
            "output_tokens": 100,
            "cache_read_tokens": 600,
            "cache_write_tokens": 100,
            "estimated": false,
            "cost_usd": 0.25,
            "created_at": "2026-11-11T00:00:00Z"
          }],
          "totals": {
            "calls": 1,
            "input_tokens": 1000,
            "output_tokens": 100,
            "cache_read_tokens": 600,
            "cache_write_tokens": 100,
            "cost_usd": 0.25,
            "by_role": [{
              "key": "worker",
              "calls": 1,
              "input_tokens": 1000,
              "output_tokens": 100,
              "cache_read_tokens": 600,
              "cache_write_tokens": 100,
              "cost_usd": 0.25
            }],
            "by_model": [],
            "by_agent": [],
            "by_task": []
          }
        }
        """
        let ledger = try JSONDecoder().decode(UsageLedgerResult.self, from: Data(json.utf8))
        XCTAssertEqual(ledger.entries.first?.cacheReadTokens, 600)
        XCTAssertEqual(ledger.entries.first?.cacheWriteTokens, 100)
        XCTAssertEqual(ledger.entries.first?.cacheHitRate ?? 0, 0.6, accuracy: 0.0001)
        XCTAssertEqual(ledger.totals.cacheReadTokens, 600)
        XCTAssertEqual(ledger.totals.byRole.first?.cacheReadTokens, 600)
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
        XCTAssertEqual(agents.agents.first?.transport, .stdio)
        XCTAssertNil(agents.agents.first?.endpoint)
        XCTAssertFalse(agents.agents.first?.allowRemote ?? true)
        XCTAssertEqual(agents.agents.first?.connectTimeoutSecs, 30)

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
        XCTAssertEqual(probe.transport, .stdio)
        XCTAssertFalse(probe.remote)
        XCTAssertFalse(probe.loadSession)
        XCTAssertFalse(probe.capabilities.loadSession)
    }

    func testAcpTransportProfilesAreExplicit() {
        XCTAssertEqual(AcpTransport.stdio.rawValue, "stdio")
        XCTAssertEqual(AcpTransport.streamableHTTP.rawValue, "streamable_http")
        XCTAssertEqual(AcpTransport.websocket.rawValue, "websocket")
        XCTAssertFalse(AcpTransport.stdio.isRemote)
        XCTAssertTrue(AcpTransport.streamableHTTP.isRemote)
        XCTAssertTrue(AcpTransport.websocket.isRemote)
        XCTAssertEqual(AcpTransport.streamableHTTP.displayName, "streamable HTTP")
    }

    func testRemoteAcpAgentAndProbeDecode() throws {
        let agentsJSON = """
        {
          "agents": [{
            "id": "agent-remote",
            "name": "remote",
            "command": "",
            "args": [],
            "env": {},
            "enabled": true,
            "auto_approve": false,
            "timeout_secs": 900,
            "created_at": "2026-11-11T00:00:00Z",
            "transport": "streamable_http",
            "endpoint": "https://agent.example.test/acp",
            "auth_header_env": "ACP_TOKEN",
            "headers": {"x-project": "demo", "x-token": "${ACP_TOKEN}"},
            "connect_timeout_secs": 15,
            "allow_remote": true
          }]
        }
        """
        let agents = try JSONDecoder().decode(
            AcpAgentListResult.self,
            from: Data(agentsJSON.utf8)
        )
        let agent = try XCTUnwrap(agents.agents.first)
        XCTAssertEqual(agent.transport, .streamableHTTP)
        XCTAssertTrue(agent.transport.isRemote)
        XCTAssertEqual(agent.endpoint, "https://agent.example.test/acp")
        XCTAssertEqual(agent.authHeaderEnv, "ACP_TOKEN")
        XCTAssertEqual(agent.headers["x-token"], "${ACP_TOKEN}")
        XCTAssertEqual(agent.connectTimeoutSecs, 15)
        XCTAssertTrue(agent.allowRemote)
        XCTAssertEqual(agent.summary, "https://agent.example.test/acp")

        let probeJSON = """
        {
          "agent_id": "agent-remote",
          "agent_name": "remote-acp",
          "version": "1.2.3",
          "protocol_version": 1,
          "session_created": true,
          "transport": "websocket",
          "endpoint": "wss://agent.example.test/acp",
          "remote": true,
          "load_session": true,
          "http_version": "HTTP/2",
          "messages_sent": 4,
          "messages_received": 6,
          "streams_opened": 2,
          "capabilities": {
            "load_session": true,
            "mcp_http": true,
            "mcp_sse": false,
            "prompt_image": true,
            "prompt_audio": false,
            "prompt_embedded_context": true
          }
        }
        """
        let probe = try JSONDecoder().decode(
            AcpProbeResult.self,
            from: Data(probeJSON.utf8)
        )
        XCTAssertEqual(probe.transport, .websocket)
        XCTAssertEqual(probe.endpoint, "wss://agent.example.test/acp")
        XCTAssertTrue(probe.remote)
        XCTAssertTrue(probe.loadSession)
        XCTAssertTrue(probe.capabilities.loadSession)
        XCTAssertTrue(probe.capabilities.mcpHTTP)
        XCTAssertFalse(probe.capabilities.mcpSSE)
        XCTAssertTrue(probe.capabilities.promptEmbeddedContext)
        XCTAssertEqual(probe.httpVersion, "HTTP/2")
        XCTAssertEqual(probe.messagesSent, 4)
        XCTAssertEqual(probe.messagesReceived, 6)
        XCTAssertEqual(probe.streamsOpened, 2)
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

    func testWorkspaceGrepResultsDecode() throws {
        let json = """
        {
          "query": "token",
          "matches": [{
            "path": "src/main.rs",
            "line": 2,
            "column": 9,
            "preview": "let token = find_token();"
          }],
          "files_scanned": 4,
          "truncated": true
        }
        """
        let result = try JSONDecoder().decode(WorkspaceGrepResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.query, "token")
        XCTAssertEqual(result.filesScanned, 4)
        XCTAssertTrue(result.truncated)
        XCTAssertEqual(result.matches.first?.path, "src/main.rs")
        XCTAssertEqual(result.matches.first?.line, 2)
        XCTAssertEqual(result.matches.first?.column, 9)
        XCTAssertEqual(result.matches.first?.id, "src/main.rs:2:9")
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
        XCTAssertTrue(result.policy.includeIntake)
        XCTAssertEqual(result.policy.maxIntakeParallel, 2)

        let perTask = try JSONDecoder().decode(
            TaskSchedulerPolicyResult.self,
            from: Data(
                #"{"project_id":"project-1","policy":{"enabled":true,"cooldown_secs":15,"dispatch_tasks":true,"include_intake":false,"max_intake_parallel":4}}"#
                    .utf8
            )
        )
        XCTAssertTrue(perTask.policy.dispatchTasks)
        XCTAssertFalse(perTask.policy.includeIntake)
        XCTAssertEqual(perTask.policy.maxIntakeParallel, 4)
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

    func testSourceAndSkillModelsDecode() throws {
        let sourceJSON = """
        {
          "id": "source-1",
          "project_id": "project-1",
          "kind": "note",
          "title": "Architecture",
          "uri": null,
          "mime_type": "text/markdown",
          "status": "indexed",
          "size_bytes": 42,
          "sha256": "abc",
          "revision": 1,
          "chunk_count": 2,
          "char_count": 42,
          "token_count": 10,
          "error": null,
          "labels": ["docs"],
          "created_at": "2026-11-10T00:00:00Z",
          "updated_at": "2026-11-10T00:00:00Z",
          "indexed_at": "2026-11-10T00:00:00Z"
        }
        """
        let source = try JSONDecoder().decode(ProjectSource.self, from: Data(sourceJSON.utf8))
        XCTAssertEqual(source.title, "Architecture")
        XCTAssertEqual(source.chunkCount, 2)
        XCTAssertEqual(source.labels, ["docs"])

        let skillJSON = """
        {
          "id": "skill-1",
          "project_id": "project-1",
          "name": "Review",
          "slug": "review",
          "description": "Review a change",
          "when_to_use": "Before merging",
          "version": "1.1.0",
          "instructions": "Read the diff.",
          "tool_allow": ["read_file"],
          "tool_deny": ["shell"],
          "profiles": ["coding"],
          "roles": ["worker"],
          "enabled": true,
          "source": "user",
          "sha256": "def",
          "created_at": "2026-11-10T00:00:00Z",
          "updated_at": "2026-11-10T00:00:00Z"
        }
        """
        let skill = try JSONDecoder().decode(ProjectSkill.self, from: Data(skillJSON.utf8))
        XCTAssertEqual(skill.name, "Review")
        XCTAssertEqual(skill.toolAllow, ["read_file"])
        XCTAssertEqual(skill.toolDeny, ["shell"])

        let hitJSON = """
        {
          "source_id": "source-1",
          "source_title": "Architecture",
          "chunk_id": "chunk-1",
          "chunk_no": 1,
          "heading": "Leases",
          "content": "deterministic leases",
          "score": 1.5
        }
        """
        let hit = try JSONDecoder().decode(SourceSearchHit.self, from: Data(hitJSON.utf8))
        XCTAssertEqual(hit.chunkNo, 1)
        XCTAssertEqual(hit.score, 1.5, accuracy: 0.001)
    }

    func testKnowledgeCollectionActivationDiffAndAuditDecode() throws {
        let collectionJSON = """
        {
          "collections": [
            {"name": "engineering", "sources": 2, "chunks": 9, "bytes": 1200,
             "tokens": 340, "labels": ["runtime", "verified"]},
            {"name": null, "sources": 1, "chunks": 2, "bytes": 100,
             "tokens": 20, "labels": []}
          ],
          "total_sources": 3
        }
        """
        let collections = try JSONDecoder().decode(
            SourceCollectionsResult.self,
            from: Data(collectionJSON.utf8)
        )
        XCTAssertEqual(collections.totalSources, 3)
        XCTAssertEqual(collections.collections.count, 2)
        XCTAssertEqual(collections.collections[0].displayName, "engineering")
        XCTAssertEqual(collections.collections[1].id, "")
        XCTAssertTrue(collections.collections[1].name == nil)

        let activationJSON = """
        {
          "activations": [
            {"name": "Release Review", "slug": "release-review", "version": "1.2.0",
             "role": "worker", "profile": "coding", "tool_allow": ["read_file"],
             "tool_deny": ["shell"], "agent_id": "agent-1", "agent_name": "Worker",
             "task_id": "T1", "activated_at": "2026-11-10T00:00:00Z"}
          ]
        }
        """
        let activations = try JSONDecoder().decode(
            SkillActivationsResult.self,
            from: Data(activationJSON.utf8)
        )
        XCTAssertEqual(activations.activations.count, 1)
        XCTAssertEqual(activations.activations[0].toolAllow, ["read_file"])
        XCTAssertEqual(activations.activations[0].toolDeny, ["shell"])
        XCTAssertEqual(activations.activations[0].role, "worker")

        let diffJSON = """
        {
          "artifact_id": "artifact-1",
          "name": "Report",
          "from_version": 1,
          "to_version": 2,
          "diff": {
            "hunks": [
              {"old_start": 1, "old_lines": 1, "new_start": 1, "new_lines": 2,
               "lines": [
                 {"kind": "delete", "old_line": 1, "new_line": null, "text": "alpha"},
                 {"kind": "add", "old_line": null, "new_line": 1, "text": "beta"},
                 {"kind": "add", "old_line": null, "new_line": 2, "text": "gamma"}
               ]}
            ],
            "additions": 2, "deletions": 1, "unchanged": 0,
            "identical": false, "truncated": false
          }
        }
        """
        let diff = try JSONDecoder().decode(
            ArtifactDiffResult.self,
            from: Data(diffJSON.utf8)
        )
        XCTAssertEqual(diff.fromVersion, 1)
        XCTAssertEqual(diff.toVersion, 2)
        XCTAssertEqual(diff.diff.additions, 2)
        XCTAssertEqual(diff.diff.deletions, 1)
        XCTAssertEqual(diff.diff.hunks[0].lines[1].kind, "add")
        XCTAssertEqual(diff.diff.hunks[0].lines[1].newLine, 1)
        XCTAssertNil(diff.diff.hunks[0].lines[1].oldLine)

        let auditJSON = """
        {
          "audit": {
            "messages": [
              {"message_id": "m1", "agent_name": "Worker",
               "excerpt": "Claim [c1].", "citation_count": 1,
               "referenced_keys": ["c1"], "dangling_keys": [],
               "unused_keys": [], "created_at": "2026-11-10T00:00:00Z"}
            ],
            "messages_with_citations": 1,
            "messages_without_citations": 0,
            "total_citations": 1,
            "unique_sources": 1,
            "dangling_key_count": 0,
            "unused_citation_count": 0
          }
        }
        """
        let audit = try JSONDecoder().decode(
            SessionCitationAuditResult.self,
            from: Data(auditJSON.utf8)
        )
        XCTAssertEqual(audit.audit.messagesWithCitations, 1)
        XCTAssertEqual(audit.audit.messages[0].referencedKeys, ["c1"])
        XCTAssertEqual(audit.audit.danglingKeyCount, 0)
    }

    func testSchedulerStatusDecodes() throws {
        let json = """
        {
          "active_sessions": 2,
          "queued_sessions": 1,
          "paused_sessions": 0,
          "pending_plan_tasks": 3,
          "running_plan_tasks": 1,
          "queued_intake": 4,
          "active_intake": 2,
          "failed_intake": 1,
          "global_session_cap": 4,
          "global_worker_cap": 12,
          "global_labour_cap": 24,
          "projects": [{
            "project_id": "project-1",
            "project_name": "Demo",
            "enabled": true,
            "cooldown_secs": 30,
            "dispatch_tasks": false,
            "include_intake": true,
            "max_intake_parallel": 2,
            "pending_sessions": 1,
            "pending_plan_tasks": 3,
            "running_plan_tasks": 1,
            "queued_intake": 4,
            "active_intake": 2,
            "failed_intake": 1
          }]
        }
        """
        let status = try JSONDecoder().decode(SchedulerStatusResult.self, from: Data(json.utf8))
        XCTAssertEqual(status.activeSessions, 2)
        XCTAssertEqual(status.globalSessionCap, 4)
        XCTAssertEqual(status.globalLabourCap, 24)
        XCTAssertEqual(status.queuedIntake, 4)
        XCTAssertEqual(status.projects.count, 1)
        XCTAssertEqual(status.projects[0].projectName, "Demo")
        XCTAssertEqual(status.projects[0].maxIntakeParallel, 2)
        XCTAssertTrue(status.projects[0].includeIntake)
    }

    func testIntakeResultsDecode() throws {
        let json = """
        {
          "tasks": [{
            "id": "intake-1",
            "project_id": "project-1",
            "title": "Fix the intake queue",
            "instructions": "Implement and verify the queue.",
            "profile": "coding",
            "kind": "coding",
            "priority": 12,
            "write_scope": ["src/intake"],
            "acceptance": ["tests pass"],
            "deps": ["intake-0"],
            "token_budget": 20000,
            "cost_budget_usd": 1.5,
            "time_budget_secs": 600,
            "status": "failed",
            "attempts": 1,
            "max_attempts": 2,
            "next_attempt_at": "2026-11-10T00:00:00Z",
            "session_id": "session-1",
            "task_id": "T1",
            "last_error": "worker failed",
            "result_summary": null,
            "origin": "app",
            "labels": ["nightly"],
            "created_at": "2026-11-09T23:00:00Z",
            "updated_at": "2026-11-10T00:00:00Z"
          }]
        }
        """
        let result = try JSONDecoder().decode(IntakeListResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.tasks.count, 1)
        let task = result.tasks[0]
        XCTAssertEqual(task.id, "intake-1")
        XCTAssertEqual(task.profile, "coding")
        XCTAssertEqual(task.writeScope, ["src/intake"])
        XCTAssertEqual(task.deps, ["intake-0"])
        XCTAssertEqual(task.attempts, 1)
        XCTAssertEqual(task.maxAttempts, 2)
        XCTAssertEqual(task.sessionID, "session-1")
        XCTAssertEqual(task.taskID, "T1")
        XCTAssertFalse(task.isActive)
        XCTAssertTrue(task.isTerminal)
        XCTAssertTrue(task.labels.contains("nightly"))

        let dispatchedJSON = """
        {
          "started": true,
          "session_id": "session-1",
          "task": {
            "id": "intake-1",
            "project_id": "project-1",
            "title": "Fix the intake queue",
            "instructions": "Implement and verify the queue.",
            "profile": "coding",
            "kind": "coding",
            "priority": 12,
            "write_scope": ["src/intake"],
            "acceptance": ["tests pass"],
            "status": "running",
            "attempts": 1,
            "max_attempts": 2,
            "origin": "app",
            "labels": [],
            "created_at": "2026-11-09T23:00:00Z",
            "updated_at": "2026-11-10T00:00:00Z"
          }
        }
        """
        let dispatched = try JSONDecoder().decode(
            IntakeDispatchResult.self,
            from: Data(dispatchedJSON.utf8)
        )
        XCTAssertTrue(dispatched.started)
        XCTAssertEqual(dispatched.sessionID, "session-1")
        XCTAssertTrue(dispatched.task.isActive)
        let action = try JSONDecoder().decode(IntakeDeleteResult.self, from: Data(#"{"deleted":true}"#.utf8))
        XCTAssertTrue(action.deleted)
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
        XCTAssertFalse(session.unlimited)

        let legacy = json.replacingOccurrences(of: #""priority": 25,"#, with: "")
        let oldSession = try JSONDecoder().decode(Session.self, from: Data(legacy.utf8))
        XCTAssertNil(oldSession.priority)
        XCTAssertFalse(oldSession.unlimited)

        let unlimitedJSON = json.replacingOccurrences(
            of: #""time_budget_secs": null,"#,
            with: #""time_budget_secs": null, "unlimited": true,"#
        )
        let unlimited = try JSONDecoder().decode(
            Session.self,
            from: Data(unlimitedJSON.utf8)
        )
        XCTAssertTrue(unlimited.unlimited)

        let updated = try JSONDecoder().decode(
            SessionPriorityResult.self,
            from: Data(#"{"session_id":"session-1","priority":40}"#.utf8)
        )
        XCTAssertEqual(updated.priority, 40)
        let mode = try JSONDecoder().decode(
            SessionUnlimitedResult.self,
            from: Data(#"{"session_id":"session-1","unlimited":true}"#.utf8)
        )
        XCTAssertTrue(mode.unlimited)
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
              "cache_read_tokens": 4,
              "cache_write_tokens": 1,
              "cost_usd": 0.25
            }
          ],
          "totals": {
            "calls": 2,
            "input_tokens": 10,
            "output_tokens": 5,
            "cache_read_tokens": 4,
            "cache_write_tokens": 1,
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
        XCTAssertEqual(result.sessions.first?.cacheReadTokens, 4)
        XCTAssertEqual(result.totals.calls, 2)
        XCTAssertEqual(result.totals.cacheWriteTokens, 1)
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

    func testJSONValueShortDescriptionCoversPayloadShapes() {
        XCTAssertEqual(JSONValue.string("hello").shortDescription, "hello")
        XCTAssertEqual(JSONValue.bool(true).shortDescription, "true")
        XCTAssertEqual(JSONValue.number(3).shortDescription, "3")
        XCTAssertEqual(
            JSONValue.array([.string("a"), .string("b")]).shortDescription,
            "2 items"
        )
        XCTAssertEqual(
            JSONValue.object(["b": .number(1), "a": .number(2)]).shortDescription,
            "a, b"
        )
        XCTAssertEqual(JSONValue.null.shortDescription, "null")
    }

    func testDashboardSectionsAreUniqueAndOrdered() {
        let sections = DashboardSection.allCases
        XCTAssertEqual(sections.count, 9)
        XCTAssertEqual(sections.first, .overview)
        XCTAssertEqual(sections.last, .diagnostics)
        XCTAssertEqual(Set(sections.map(\.title)).count, sections.count)
        XCTAssertTrue(sections.allSatisfy { !$0.detail.isEmpty && !$0.icon.isEmpty })
    }

    func testDashboardResultDecodesCrossProjectSummaryAndAlerts() throws {
        let json = """
        {
          "summary": {
            "generated_at": "2026-11-08T00:00:00Z",
            "project_id": null,
            "projects": 2,
            "sessions": 4,
            "sessions_by_status": [
              {"label": "failed", "count": 1},
              {"label": "idle", "count": 3}
            ],
            "pending_approvals": 2,
            "pending_questions": 1,
            "running_sessions": 0,
            "queued_sessions": 1,
            "tasks": 5,
            "tasks_by_status": [{"label": "blocked", "count": 1}],
            "running_tasks": 0,
            "write_leases": 1,
            "checkpoints": 6,
            "dirty_checkpoints": 2,
            "fork_roots": 3,
            "fork_sessions": 1,
            "project_summaries": [
              {
                "project_id": "project-1",
                "name": "Alpha",
                "sessions": 4,
                "running_sessions": 0,
                "queued_sessions": 1,
                "failed_sessions": 1,
                "pending_approvals": 2,
                "pending_questions": 1,
                "tasks": 5,
                "running_tasks": 0,
                "checkpoints": 6,
                "dirty_checkpoints": 2,
                "fork_roots": 3,
                "fork_sessions": 1
              }
            ]
          },
          "alerts": [
            {
              "id": "approval:1",
              "kind": "approval",
              "severity": "warning",
              "title": "Approval pending: write_file",
              "detail": "alpha · tool call call-1",
              "project_id": "project-1",
              "session_id": "session-1",
              "action": "inbox"
            },
            {
              "id": "task:session-1:T1",
              "kind": "task",
              "severity": "critical",
              "title": "Task failed: Build",
              "detail": "compiler error",
              "project_id": "project-1",
              "session_id": "session-1",
              "action": "team"
            }
          ],
          "alerts_truncated": false
        }
        """
        let result = try JSONDecoder().decode(DashboardResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.summary.projects, 2)
        XCTAssertEqual(result.summary.sessionsByStatus.count, 2)
        XCTAssertEqual(result.summary.dirtyCheckpoints, 2)
        XCTAssertEqual(result.summary.projectSummaries.count, 1)
        XCTAssertEqual(result.summary.projectSummaries[0].name, "Alpha")
        XCTAssertEqual(result.summary.projectSummaries[0].failedSessions, 1)
        XCTAssertNil(result.summary.agent)
        XCTAssertEqual(result.alerts.count, 2)
        XCTAssertEqual(result.alerts[1].severity, "critical")
        XCTAssertEqual(result.alerts[1].action, "team")
        XCTAssertFalse(result.alertsTruncated)
    }

    func testCheckpointRestoreResultDecodes() throws {
        let json = """
        {
          "session_id": "session-1",
          "checkpoint_id": "checkpoint-1",
          "previous_commit": "aaaaaaaaaaaaaaaa",
          "restored_commit": "bbbbbbbbbbbbbbbb"
        }
        """
        let result = try JSONDecoder().decode(
            SessionCheckpointRestoreResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.sessionID, "session-1")
        XCTAssertEqual(result.checkpointID, "checkpoint-1")
        XCTAssertEqual(result.previousCommit, "aaaaaaaaaaaaaaaa")
        XCTAssertEqual(result.restoredCommit, "bbbbbbbbbbbbbbbb")
    }

    func testCheckpointDiffResultDecodesFilesAndDirtyState() throws {
        let json = """
        {
          "session_id": "session-1",
          "checkpoint_id": "checkpoint-1",
          "current_commit": "aaaaaaaaaaaaaaaa",
          "checkpoint_commit": "bbbbbbbbbbbbbbbb",
          "current_branch": "vp/session-1",
          "clean": false,
          "files": [
            {"path": "src/main.rs", "additions": 3, "deletions": 1, "binary": false}
          ],
          "patch": "diff --git a/src/main.rs b/src/main.rs",
          "truncated": false
        }
        """
        let result = try JSONDecoder().decode(
            SessionCheckpointDiffResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.currentBranch, "vp/session-1")
        XCTAssertFalse(result.clean)
        XCTAssertEqual(result.files.count, 1)
        XCTAssertEqual(result.files[0].additions, 3)
        XCTAssertFalse(result.files[0].binary)
        XCTAssertTrue(result.patch.hasPrefix("diff --git"))
    }

    func testMcpServerDecodesHttpTransportAndHeaders() throws {
        let json = """
        {
          "server": {
            "id": "server-1",
            "name": "remote",
            "transport": "http",
            "command": "",
            "args": [],
            "env": {},
            "url": "https://mcp.example.test/acp",
            "headers": {"x-test": "verlinpark"},
            "enabled": true,
            "created_at": "2026-11-08T00:00:00Z"
          }
        }
        """
        let result = try JSONDecoder().decode(McpServerResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.server.transportKind, "http")
        XCTAssertEqual(result.server.url, "https://mcp.example.test/acp")
        XCTAssertEqual(result.server.headerCount, 1)
        XCTAssertEqual(result.server.headers?["x-test"], "verlinpark")
    }

    func testLegacyMcpServerWithoutTransportStillDecodes() throws {
        let json = """
        {
          "id": "server-1",
          "name": "local",
          "command": "mcp-files",
          "args": ["--root", "/tmp"],
          "env": {},
          "enabled": true,
          "created_at": "2026-11-08T00:00:00Z"
        }
        """
        let server = try JSONDecoder().decode(McpServer.self, from: Data(json.utf8))
        XCTAssertEqual(server.transportKind, "stdio")
        XCTAssertNil(server.url)
        XCTAssertNil(server.headers)
        XCTAssertEqual(server.args, ["--root", "/tmp"])
    }

    func testMcpProbeResultDecodesTransportAndCapabilities() throws {
        let json = """
        {
          "server_id": "server-1",
          "name": "remote",
          "transport": "http",
          "server_name": "test-mcp",
          "server_version": "1.2.3",
          "protocol_version": "2025-06-18",
          "tools": 4,
          "resources": 2,
          "prompts": 1,
          "resources_supported": true,
          "prompts_supported": false
        }
        """
        let result = try JSONDecoder().decode(McpProbeResult.self, from: Data(json.utf8))
        XCTAssertEqual(result.transport, "http")
        XCTAssertEqual(result.serverName, "test-mcp")
        XCTAssertEqual(result.tools, 4)
        XCTAssertTrue(result.resourcesSupported)
        XCTAssertFalse(result.promptsSupported)
    }

    func testSimplifiedChineseResourcesResolve() {
        let previous = UserDefaults.standard.string(forKey: LanguageStore.storageKey)
        defer {
            if let previous {
                UserDefaults.standard.set(previous, forKey: LanguageStore.storageKey)
            } else {
                UserDefaults.standard.removeObject(forKey: LanguageStore.storageKey)
            }
        }

        UserDefaults.standard.set("zh-Hans", forKey: LanguageStore.storageKey)
        XCTAssertEqual(L10n.t("Settings"), "设置")
        XCTAssertEqual(L10n.t("General"), "通用")
        XCTAssertEqual(L10n.t("History"), "历史")
        XCTAssertEqual(L10n.format("%lld active", 3), "3 个活跃")
        XCTAssertEqual(AppLanguage.simplifiedChinese.locale.identifier, "zh-Hans")
        XCTAssertEqual(AppLanguage.allCases.count, 2)
    }

    func testMotionTokensAreOrderedAndStaggerIsCapped() {
        XCTAssertLessThan(VPMotion.instant, VPMotion.quick)
        XCTAssertLessThan(VPMotion.quick, VPMotion.standard)
        XCTAssertLessThan(VPMotion.standard, VPMotion.emphasized)
        XCTAssertLessThan(VPMotion.emphasized, VPMotion.slow)
        XCTAssertEqual(VPMotion.stagger(-1), 0)
        XCTAssertEqual(VPMotion.stagger(0), 0)
        XCTAssertEqual(VPMotion.stagger(2), 0.09, accuracy: 0.0001)
        XCTAssertEqual(VPMotion.stagger(100), 0.36, accuracy: 0.0001)
    }

    func testMotionRespectsReduceMotion() {
        XCTAssertNil(VPMotion.resolved(VPMotion.snap, reduceMotion: true))
        XCTAssertNotNil(VPMotion.resolved(VPMotion.snap, reduceMotion: false))
        XCTAssertNil(VPMotion.delayed(VPMotion.smooth, index: 2, reduceMotion: true))
        XCTAssertNotNil(VPMotion.delayed(VPMotion.smooth, index: 2, reduceMotion: false))
    }

    func testSegmentIdentityAndMetadata() {
        let segment = VPSegment("preview", title: "Preview", icon: "rectangle.on.rectangle")
        XCTAssertEqual(segment.id, "preview")
        XCTAssertEqual(segment.title, "Preview")
        XCTAssertEqual(segment.icon, "rectangle.on.rectangle")
    }

    @MainActor
    func testPinnedItemsStoreTogglesPersistsAndPrunes() throws {
        let suite = "VerlinParkTests.PinnedItemsStore"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defaults.removePersistentDomain(forName: suite)
        defer { defaults.removePersistentDomain(forName: suite) }

        let store = PinnedItemsStore(defaults: defaults)
        store.toggleSession("s1")
        store.toggleSession("s2")
        store.toggleProject("p1")
        XCTAssertEqual(store.sessionIDs, ["s2", "s1"])
        XCTAssertTrue(store.isPinnedSession("s1"))
        XCTAssertTrue(store.isPinnedProject("p1"))

        let reloaded = PinnedItemsStore(defaults: defaults)
        XCTAssertEqual(reloaded.sessionIDs, ["s2", "s1"])
        XCTAssertTrue(reloaded.isPinnedProject("p1"))

        reloaded.toggleSession("s2")
        XCTAssertEqual(reloaded.sessionIDs, ["s1"])

        reloaded.prune(validSessionIDs: ["s1"], validProjectIDs: [])
        XCTAssertEqual(reloaded.sessionIDs, ["s1"])
        XCTAssertTrue(reloaded.projectIDs.isEmpty)
    }

    @MainActor
    func testWorkspaceLayoutStoreAppliesPresetsAndPersists() throws {
        let suite = "VerlinParkTests.WorkspaceLayoutStore"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defaults.removePersistentDomain(forName: suite)
        defer { defaults.removePersistentDomain(forName: suite) }

        let store = WorkspaceLayoutStore(defaults: defaults)
        XCTAssertEqual(store.currentPreset, .balanced)

        store.apply(.reading)
        XCTAssertEqual(store.currentPreset, .reading)
        XCTAssertFalse(store.inspectorVisible)

        let reloaded = WorkspaceLayoutStore(defaults: defaults)
        XCTAssertEqual(reloaded.currentPreset, .reading)
        XCTAssertFalse(reloaded.inspectorVisible)

        reloaded.reset()
        XCTAssertEqual(reloaded.currentPreset, .balanced)
        XCTAssertTrue(reloaded.inspectorVisible)
    }

    @MainActor
    func testWorkspaceCommandRouterSequencesAndTitlesAreUnique() {
        let router = WorkspaceCommandRouter()
        XCTAssertEqual(router.sequence, 0)
        XCTAssertNil(router.lastCommand)

        router.send(.search)
        XCTAssertEqual(router.lastCommand, .search)
        XCTAssertEqual(router.sequence, 1)

        router.send(.resetLayout)
        XCTAssertEqual(router.lastCommand, .resetLayout)
        XCTAssertEqual(router.sequence, 2)

        let titles = WorkspaceCommand.allCases.map(\.title)
        XCTAssertEqual(Set(titles).count, titles.count)
        XCTAssertTrue(titles.allSatisfy { !$0.isEmpty })
    }

    func testWorkspaceLayoutKeepsReadableStageAndClampsInspector() {
        let defaults = WorkspaceLayout(
            railWidth: WorkspaceLayout.defaultRailWidth,
            inspectorWidth: WorkspaceLayout.defaultInspectorWidth,
            inspectorVisible: true
        ).resolved(windowWidth: 1320, focusMode: false)
        XCTAssertEqual(defaults.railWidth, 272, accuracy: 0.001)
        XCTAssertEqual(defaults.inspectorWidth, 382, accuracy: 0.001)
        XCTAssertEqual(defaults.stageWidth, 652, accuracy: 0.001)
        XCTAssertTrue(defaults.railVisible)
        XCTAssertTrue(defaults.inspectorVisible)

        let narrow = WorkspaceLayout(
            railWidth: 320,
            inspectorWidth: 560,
            inspectorVisible: true
        ).resolved(windowWidth: 1120, focusMode: false)
        XCTAssertEqual(narrow.railWidth, 266, accuracy: 0.001)
        XCTAssertEqual(narrow.inspectorWidth, 320, accuracy: 0.001)
        XCTAssertEqual(narrow.stageWidth, 520, accuracy: 0.001)

        let hidden = WorkspaceLayout(
            railWidth: 272,
            inspectorWidth: 382,
            inspectorVisible: false
        ).resolved(windowWidth: 1120, focusMode: false)
        XCTAssertFalse(hidden.inspectorVisible)
        XCTAssertEqual(hidden.stageWidth, 841, accuracy: 0.001)
    }

    func testWorkspaceLayoutFocusModeUsesFullStage() {
        let focused = WorkspaceLayout(
            railWidth: 300,
            inspectorWidth: 500,
            inspectorVisible: true
        ).resolved(windowWidth: 1320, focusMode: true)
        XCTAssertFalse(focused.railVisible)
        XCTAssertFalse(focused.inspectorVisible)
        XCTAssertEqual(focused.railWidth, 0, accuracy: 0.001)
        XCTAssertEqual(focused.inspectorWidth, 0, accuracy: 0.001)
        XCTAssertEqual(focused.stageWidth, 1320, accuracy: 0.001)
    }

    func testWindowFramePolicyRestoresOnScreenAndEnforcesMinimumSize() {
        let screen = CGRect(x: 0, y: 0, width: 1440, height: 900)
        let offscreen = WindowFramePolicy.sanitized(
            CGRect(x: 5000, y: 5000, width: 1320, height: 860),
            screens: [screen]
        )
        XCTAssertEqual(offscreen.width, 1320, accuracy: 0.001)
        XCTAssertEqual(offscreen.height, 860, accuracy: 0.001)
        XCTAssertEqual(offscreen.midX, screen.midX, accuracy: 0.001)
        XCTAssertEqual(offscreen.midY, screen.midY, accuracy: 0.001)

        let tiny = WindowFramePolicy.sanitized(
            CGRect(x: 10, y: 10, width: 800, height: 500),
            screens: [screen]
        )
        XCTAssertEqual(tiny.width, WindowFramePolicy.minimumSize.width, accuracy: 0.001)
        XCTAssertEqual(tiny.height, WindowFramePolicy.minimumSize.height, accuracy: 0.001)

        let noScreens = WindowFramePolicy.sanitized(
            CGRect(x: 20, y: 20, width: 400, height: 300),
            screens: []
        )
        XCTAssertEqual(noScreens.width, WindowFramePolicy.minimumSize.width, accuracy: 0.001)
        XCTAssertEqual(noScreens.height, WindowFramePolicy.minimumSize.height, accuracy: 0.001)
    }

    func testCommandPaletteRankerRewardsTitleAndRecency() {
        let candidates = [
            CommandPaletteCandidate(
                id: "action-dashboard",
                title: "Manage Dash",
                subtitle: "Agents, cost, queue",
                keywords: "dashboard manage agents"
            ),
            CommandPaletteCandidate(
                id: "session-dashboard",
                title: "Build dashboard",
                subtitle: "Project session",
                keywords: "session dashboard"
            ),
            CommandPaletteCandidate(
                id: "action-search",
                title: "Search",
                subtitle: "Find messages",
                keywords: "find query"
            ),
        ]

        let ranked = CommandPaletteRanker.rank(candidates, query: "dash", recents: [])
        XCTAssertEqual(ranked.first?.id, "action-dashboard")
        XCTAssertTrue(ranked.contains { $0.id == "session-dashboard" })

        let recent = CommandPaletteRanker.rank(candidates, query: "", recents: ["session-dashboard"])
        XCTAssertEqual(recent.first?.id, "session-dashboard")

        XCTAssertEqual(
            CommandPaletteRanker.highlightRanges("dash", in: "Manage Dash"),
            [7 ..< 11]
        )
        XCTAssertNil(CommandPaletteRanker.score("zzz", candidate: "Manage Dash"))
        XCTAssertNotNil(CommandPaletteRanker.score("ms", candidate: "Manage Dash"))
    }

    func testSearchResultsModelFiltersSourcesCountsAndHighlights() {
        let hits = [
            testSearchHit(id: "a", source: "message", goal: "Build Dashboard", snippet: "dashboard summary"),
            testSearchHit(id: "b", source: "event", goal: "Build event", snippet: "dashboard event"),
            testSearchHit(id: "c", source: "event", goal: "Unrelated", snippet: "no match"),
        ]
        var model = SearchResultsModel()
        XCTAssertEqual(model.visible(hits).map(\.id), ["a", "b", "c"])
        XCTAssertEqual(model.counts(hits)[.all], 3)
        XCTAssertEqual(model.counts(hits)[.messages], 1)
        XCTAssertEqual(model.counts(hits)[.events], 2)

        model.source = .messages
        XCTAssertEqual(model.visible(hits).map(\.id), ["a"])
        model.source = .events
        XCTAssertEqual(model.visible(hits).map(\.id), ["b", "c"])

        XCTAssertEqual(
            SearchResultsModel.ranges(of: "dashboard", in: "Build Dashboard here"),
            [6 ..< 15]
        )
        XCTAssertEqual(SearchResultsModel.ranges(of: "", in: "Build Dashboard"), [])
    }

    private func testSearchHit(id: String, source: String, goal: String, snippet: String) -> SearchHit {
        SearchHit(
            id: id,
            sessionId: "s-\(id)",
            projectId: "p1",
            sessionGoal: goal,
            source: source,
            role: nil,
            kind: nil,
            seq: nil,
            snippet: snippet,
            createdAt: "2026-11-08T10:00:00Z"
        )
    }

    func testNotificationInboxFiltersSearchesCountsAndGroups() throws {
        let now = try XCTUnwrap(ISO8601DateFormatter().date(from: "2026-11-08T12:00:00Z"))
        let today = "2026-11-08T10:00:00Z"
        let yesterday = "2026-11-07T10:00:00Z"
        let earlier = "2026-10-01T10:00:00Z"
        let notices = [
            testNotice(
                id: "a",
                project: "p1",
                kind: "approval",
                title: "Approve write",
                createdAt: today,
                read: false
            ),
            testNotice(
                id: "b",
                project: "p1",
                kind: "task",
                title: "Task failed",
                createdAt: yesterday,
                read: true
            ),
            testNotice(
                id: "c",
                project: "p2",
                kind: "integration",
                title: "Merge conflict",
                createdAt: earlier,
                read: false
            ),
        ]
        let directory: [String: SessionDirectoryEntry] = [
            "s-a": SessionDirectoryEntry(projectId: "p1", goal: "Ship the dashboard"),
            "s-b": SessionDirectoryEntry(projectId: "p1", goal: "Fix the queue"),
            "s-c": SessionDirectoryEntry(projectId: "p2", goal: "Merge the branch"),
        ]

        var inbox = NotificationInboxModel()
        XCTAssertEqual(inbox.visible(in: notices, directory: directory).map(\.id), ["a", "b", "c"])
        inbox.filter = .unread
        XCTAssertEqual(inbox.visible(in: notices, directory: directory).map(\.id), ["a", "c"])
        inbox.query = "dashboard"
        XCTAssertEqual(inbox.visible(in: notices, directory: directory).map(\.id), ["a"])
        inbox.query = ""
        inbox.filter = .all
        inbox.projectID = "p2"
        XCTAssertEqual(inbox.visible(in: notices, directory: directory).map(\.id), ["c"])

        inbox.projectID = nil
        let counts = inbox.counts(in: notices, directory: directory)
        XCTAssertEqual(counts[.all], 3)
        XCTAssertEqual(counts[.unread], 2)
        XCTAssertEqual(counts[.approvals], 1)
        XCTAssertEqual(counts[.tasks], 1)
        XCTAssertEqual(counts[.integration], 1)

        var calendar = Calendar(identifier: .gregorian)
        calendar.timeZone = try XCTUnwrap(TimeZone(secondsFromGMT: 0))
        let groups = inbox.groups(from: notices, calendar: calendar, now: now)
        XCTAssertEqual(groups.map(\.id), ["today", "yesterday", "earlier"])
        XCTAssertEqual(groups[0].notices.map(\.id), ["a"])
        XCTAssertEqual(groups[1].notices.map(\.id), ["b"])
        XCTAssertEqual(groups[2].notices.map(\.id), ["c"])
    }

    private func testNotice(
        id: String,
        project: String,
        kind: String,
        title: String,
        createdAt: String,
        read: Bool
    ) -> AppNotice {
        AppNotice(
            id: id,
            sessionId: "s-\(id)",
            projectId: project,
            kind: kind,
            title: title,
            detail: title,
            createdAt: createdAt,
            isRead: read
        )
    }

    func testRailSessionListFiltersSortsLimitsAndCountsActive() {
        let sessions = [
            testSession(id: "a", project: "p1", status: "idle", createdAt: "2026-11-01T10:00:00Z"),
            testSession(id: "b", project: "p2", status: "running", createdAt: "2026-11-03T10:00:00Z"),
            testSession(id: "c", project: "p1", status: "running", createdAt: "2026-11-04T10:00:00Z"),
            testSession(id: "d", project: "p1", status: "paused", createdAt: "2026-11-02T10:00:00Z"),
        ]

        XCTAssertEqual(
            RailSessionList.sorted(sessions, projectID: "p1").map(\.id),
            ["c", "d", "a"]
        )
        XCTAssertEqual(
            RailSessionList.visible(sessions, projectID: "p1", expanded: false, limit: 2).map(\.id),
            ["c", "d"]
        )
        XCTAssertEqual(
            RailSessionList.visible(sessions, projectID: "p1", expanded: true, limit: 1).map(\.id),
            ["c", "d", "a"]
        )
        XCTAssertEqual(RailSessionList.activeCount(sessions, projectID: "p1"), 2)
        XCTAssertEqual(RailSessionList.activeCount(sessions, projectID: "missing"), 0)
    }

    private func testSession(
        id: String,
        project: String,
        status: String,
        createdAt: String,
        priority: Int = 0
    ) -> Session {
        Session(
            id: id,
            projectId: project,
            goal: "Session \(id)",
            status: status,
            priority: priority,
            worktree: nil,
            branch: nil,
            tokenBudget: nil,
            costBudgetUsd: nil,
            timeBudgetSecs: nil,
            costSpentUsd: 0,
            baseCommit: nil,
            tokensIn: 0,
            tokensOut: 0,
            createdAt: createdAt
        )
    }

    func testRailSessionSortsByStatusAndPriority() {
        let sessions = [
            testSession(id: "a", project: "p1", status: "idle", createdAt: "2026-11-01T10:00:00Z"),
            testSession(
                id: "c",
                project: "p1",
                status: "running",
                createdAt: "2026-11-04T10:00:00Z",
                priority: 50
            ),
            testSession(
                id: "d",
                project: "p1",
                status: "paused",
                createdAt: "2026-11-02T10:00:00Z",
                priority: -10
            ),
            testSession(
                id: "e",
                project: "p1",
                status: "failed",
                createdAt: "2026-11-05T10:00:00Z",
                priority: 5
            ),
        ]
        XCTAssertEqual(
            RailSessionSort.status.sorted(sessions, projectID: "p1").map(\.id),
            ["c", "d", "a", "e"]
        )
        XCTAssertEqual(
            RailSessionSort.priority.sorted(sessions, projectID: "p1").map(\.id),
            ["c", "e", "a", "d"]
        )
        XCTAssertEqual(
            RailSessionSort.recent.sorted(sessions, projectID: "p1").map(\.id),
            ["e", "c", "d", "a"]
        )
    }

    func testSettingsSectionsAreUniqueAndComplete() {
        let sections = SettingsSection.allCases
        XCTAssertEqual(sections.count, 9)
        XCTAssertTrue(sections.contains(.skills))
        XCTAssertEqual(sections.first, .general)
        XCTAssertEqual(sections.last, .about)
        XCTAssertEqual(Set(sections.map(\.title)).count, sections.count)
        XCTAssertTrue(sections.allSatisfy { !$0.detail.isEmpty && !$0.icon.isEmpty })
    }

    func testToastKindsHaveDistinctIconography() {
        XCTAssertEqual(VPToastKind.allCases.count, 4)
        XCTAssertEqual(Set(VPToastKind.allCases.map(\.icon)).count, 4)
        XCTAssertEqual(VPToastKind.success.icon, "checkmark.circle.fill")
        XCTAssertEqual(VPToastKind.error.icon, "xmark.octagon.fill")
    }

    func testKeyboardShortcutCatalogIsNonEmptyAndUnambiguous() {
        XCTAssertFalse(KeyboardShortcutsCatalog.groups.isEmpty)
        let entries = KeyboardShortcutsCatalog.groups.flatMap(\.entries)
        XCTAssertGreaterThan(entries.count, 10)
        XCTAssertTrue(entries.allSatisfy { !$0.title.isEmpty && !$0.key.isEmpty })
        XCTAssertEqual(
            Set(KeyboardShortcutsCatalog.groups.map(\.title)).count,
            KeyboardShortcutsCatalog.groups.count
        )
        XCTAssertEqual(KeyboardShortcutsCatalog.groups.first?.title, L10n.t("Global"))
        XCTAssertEqual(KeyboardShortcutsCatalog.groups.last?.title, L10n.t("Project"))
        XCTAssertEqual(
            KeyboardShortcutsCatalog.groups.map(\.entries.count).reduce(0, +),
            entries.count
        )
        XCTAssertTrue(KeyboardShortcutsCatalog.groups.allSatisfy { !$0.entries.isEmpty })
    }
}
