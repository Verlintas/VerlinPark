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
            "leader": {},
            "manager": {},
            "worker": {},
            "verifier": {},
            "labour": {}
          }
        }
        """
        let info = try JSONDecoder().decode(AgentInfo.self, from: Data(json.utf8))
        XCTAssertFalse(info.configured)
        XCTAssertNil(info.capacity)
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
