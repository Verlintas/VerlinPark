@testable import VerlinPark
import XCTest

final class AgentIdentityTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testProjectIdentityRosterDecodesReadOnlyFields() throws {
        let json = try XCTUnwrap("""
        {
          "identities": [
            {
              "id": "identity-1",
              "project_id": "prj-1",
              "role": "manager",
              "department": "platform",
              "component": null,
              "title": "Platform Lead",
              "profile": "generic",
              "model": null,
              "parent_identity_id": "leader-1",
              "status": "paused",
              "generation": 3,
              "created_at": "2026-11-12T00:00:00Z",
              "updated_at": "2026-11-12T00:05:00Z",
              "retired_at": null,
              "successor_id": null,
              "origin": "local"
            },
            {
              "id": "identity-2",
              "project_id": "prj-1",
              "role": "worker",
              "status": "retired",
              "generation": 1,
              "created_at": "2026-11-12T00:00:00Z",
              "updated_at": "2026-11-12T00:06:00Z",
              "retired_at": "2026-11-12T00:06:00Z",
              "origin": "imported"
            }
          ]
        }
        """.data(using: .utf8))

        let result = try decoder.decode(AgentIdentitiesResult.self, from: json)
        XCTAssertEqual(result.identities.count, 2)
        let manager = result.identities[0]
        XCTAssertEqual(manager.projectId, "prj-1")
        XCTAssertEqual(manager.displayName, "Platform Lead")
        XCTAssertEqual(manager.generation, 3)
        XCTAssertEqual(manager.status, "paused")
        XCTAssertFalse(manager.isImported)
        XCTAssertNil(manager.component)
        XCTAssertNil(manager.model)
        let worker = result.identities[1]
        XCTAssertEqual(worker.displayName, "Worker")
        XCTAssertTrue(worker.isImported)
        XCTAssertNotNil(worker.retiredAt)
        XCTAssertNil(worker.title)
    }

    func testLegacyAgentRecordWithoutIdentityStillDecodes() throws {
        let json = try XCTUnwrap("""
        {
          "id": "run-1",
          "session_id": "ses-1",
          "role": "leader",
          "name": "Leader",
          "status": "running",
          "model": "test-model",
          "created_at": "2026-11-12T00:00:00Z"
        }
        """.data(using: .utf8))
        let record = try decoder.decode(AgentRecord.self, from: json)
        XCTAssertNil(record.identityId)
        XCTAssertNil(record.parentId)
        XCTAssertNil(record.profile)
    }

    func testIdentityRunLinkDecodesWhenPresent() throws {
        let json = try XCTUnwrap("""
        {
          "id": "run-1",
          "session_id": "ses-1",
          "identity_id": "identity-1",
          "role": "worker",
          "name": "Worker",
          "status": "running",
          "model": "test-model",
          "created_at": "2026-11-12T00:00:00Z"
        }
        """.data(using: .utf8))
        let record = try decoder.decode(AgentRecord.self, from: json)
        XCTAssertEqual(record.identityId, "identity-1")
    }

    func testAgentMemoryRowsDecodeReadOnlyScopeAndEvidence() throws {
        let json = try XCTUnwrap("""
        {
          "memories": [
            {
              "id": "mem-1",
              "project_id": "prj-1",
              "identity_id": "identity-1",
              "department": "Platform",
              "component": "auth",
              "scope": "agent",
              "content": "The auth component owns JWT validation.",
              "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
              "evidence": [
                {"kind": "task", "reference": "T1", "note": null},
                {"kind": "commit", "reference": "abcdef123456", "note": "reviewed"}
              ],
              "source": "agent",
              "status": "active",
              "pinned": true,
              "created_at": "2026-11-13T00:00:00Z",
              "updated_at": "2026-11-13T00:05:00Z",
              "expires_at": null,
              "verified_at": "2026-11-13T00:06:00Z",
              "superseded_by": null,
              "stale_reason": null,
              "origin": "local",
              "expired": false
            },
            {
              "id": "mem-2",
              "project_id": "prj-1",
              "scope": "project",
              "content": "A project fact.",
              "source": "operator",
              "status": "stale",
              "stale_reason": "forgotten",
              "origin": "imported",
              "expired": true
            }
          ],
          "total": 2
        }
        """.data(using: .utf8))
        let result = try decoder.decode(AgentMemoryListResult.self, from: json)
        XCTAssertEqual(result.total, 2)
        XCTAssertEqual(result.memories.count, 2)
        let agent = result.memories[0]
        XCTAssertEqual(agent.scope, "agent")
        XCTAssertEqual(agent.identityId, "identity-1")
        XCTAssertEqual(agent.evidenceLabelParts, ["task T1", "commit abcdef12"])
        XCTAssertTrue(agent.isActive)
        XCTAssertFalse(agent.isImported)
        XCTAssertEqual(agent.statusDetail, "verified 2026-11-13T00:06:00Z")
        let imported = result.memories[1]
        XCTAssertEqual(imported.effectiveStatus, "expired")
        XCTAssertTrue(imported.isImported)
        XCTAssertFalse(imported.isActive)
        XCTAssertEqual(imported.statusDetail, "forgotten")
    }

    func testAgentPerformanceSummaryDecodesReadOnlyEvidence() throws {
        let json = try XCTUnwrap("""
        {
          "project_id": "prj-1",
          "computed_at": "2026-11-13T00:00:00Z",
          "lookback_days": 30,
          "retirement_policy_enabled": true,
          "identities": [{
            "project_id": "prj-1",
            "identity_id": "identity-1",
            "role": "worker",
            "status": "active",
            "lookback_days": 30,
            "completed_tasks": 3,
            "failed_tasks": 2,
            "cancelled_tasks": 1,
            "consecutive_failures": 1,
            "retries": 2,
            "verifier_approvals": 2,
            "verifier_rejections": 1,
            "unsubstantiated_handoffs": 1,
            "false_consensus": 0,
            "diagnosis_findings": 1,
            "assignment_count": 2,
            "session_count": 1,
            "input_tokens": 1200,
            "output_tokens": 300,
            "cost_usd": 0.42,
            "last_active_at": "2026-11-13T00:00:00Z",
            "computed_at": "2026-11-13T00:00:00Z",
            "reason_counts": [{"code": "false_consensus", "count": 1}],
            "evidence": [{"kind": "task", "reference": "T1", "reason": "task_failed", "at": "2026-11-13T00:00:00Z", "note": "status failed"}]
          }],
          "totals": {
            "identities": 1,
            "completed_tasks": 3,
            "failed_tasks": 2,
            "verifier_approvals": 2,
            "verifier_rejections": 1,
            "input_tokens": 1200,
            "output_tokens": 300,
            "cost_usd": 0.42
          }
        }
        """.data(using: .utf8))
        let summary = try decoder.decode(AgentPerformanceSummaryResult.self, from: json)
        XCTAssertTrue(summary.retirementPolicyEnabled)
        XCTAssertEqual(summary.totals.completedTasks, 3)
        XCTAssertEqual(summary.identities.count, 1)
        XCTAssertEqual(summary.identities[0].completedTasks, 3)
        XCTAssertEqual(summary.identities[0].evidenceCount, 1)
        XCTAssertEqual(summary.identities[0].reasonCounts?.first?.code, "false_consensus")
        XCTAssertFalse(String(describing: summary).contains("prompt"))
    }

    func testRetirementCandidatesAndProposalsDecodeWithoutMutationAuthority() throws {
        let candidatesJSON = try XCTUnwrap("""
        {
          "project_id": "prj-1",
          "generated_at": "2026-11-13T00:00:00Z",
          "policy_enabled": true,
          "evaluated": 2,
          "candidates": [{
            "project_id": "prj-1",
            "identity_id": "identity-1",
            "role": "worker",
            "title": "Auth Worker",
            "status": "active",
            "generation": 2,
            "severity": 1001,
            "reasons": [{"code": "consecutive_failures", "count": 3, "threshold": 2}],
            "evidence": [{"kind": "task", "reference": "T1", "reason": "task_failed", "at": null, "note": null}]
          }],
          "limited": false
        }
        """.data(using: .utf8))
        let candidates = try decoder.decode(AgentRetirementCandidatesResult.self, from: candidatesJSON)
        XCTAssertTrue(candidates.policyEnabled)
        XCTAssertEqual(candidates.candidates.count, 1)
        XCTAssertEqual(candidates.candidates[0].reasonSummary, "consecutive_failures 3/2")
        XCTAssertEqual(candidates.candidates[0].severity, 1001)

        let proposalsJSON = try XCTUnwrap("""
        {
          "proposals": [{
            "id": "proposal-1",
            "project_id": "prj-1",
            "identity_id": "identity-1",
            "status": "pending",
            "reason_code": "consecutive_failures",
            "reasons": [{"code": "consecutive_failures", "count": 3, "threshold": 2}],
            "proposed_at": "2026-11-13T00:00:00Z",
            "proposed_by": "operator",
            "origin": "local"
          }]
        }
        """.data(using: .utf8))
        let proposals = try decoder.decode(AgentRetirementProposalListResult.self, from: proposalsJSON)
        XCTAssertEqual(proposals.proposals.first?.status, "pending")
        XCTAssertEqual(proposals.proposals.first?.reasonCode, "consecutive_failures")
        XCTAssertNil(proposals.proposals.first?.successorId)
    }

    func testHireAndRetireResultsDecodeForS7Consumption() throws {
        let hireJSON = try XCTUnwrap("""
        {
          "request": {
            "id": "hire-1",
            "project_id": "prj-1",
            "identity_id": "identity-1",
            "role": "worker",
            "status": "approved",
            "requested_at": "2026-11-13T00:00:00Z",
            "requested_by": "operator",
            "approval_risk": "host_state",
            "origin": "local"
          },
          "identity": {
            "id": "identity-1",
            "project_id": "prj-1",
            "role": "worker",
            "status": "active",
            "generation": 1,
            "created_at": "2026-11-13T00:00:00Z",
            "updated_at": "2026-11-13T00:00:00Z",
            "origin": "local"
          },
          "state": "approved",
          "changed": true
        }
        """.data(using: .utf8))
        let hire = try decoder.decode(AgentHireResult.self, from: hireJSON)
        XCTAssertEqual(hire.state, "approved")
        XCTAssertEqual(hire.request.approvalRisk, "host_state")
        XCTAssertEqual(hire.identity?.role, "worker")

        let retireJSON = try XCTUnwrap("""
        {
          "identity": {
            "id": "identity-1",
            "project_id": "prj-1",
            "role": "worker",
            "status": "retired",
            "generation": 2,
            "created_at": "2026-11-13T00:00:00Z",
            "updated_at": "2026-11-13T00:00:00Z",
            "successor_id": "identity-2",
            "origin": "local"
          },
          "changed": true,
          "proposal_id": "proposal-1",
          "released_assignments": 1,
          "archived_workspaces": 1,
          "employment_event_id": "event-1"
        }
        """.data(using: .utf8))
        let retire = try decoder.decode(AgentRetireResult.self, from: retireJSON)
        XCTAssertEqual(retire.identity.status, "retired")
        XCTAssertEqual(retire.proposalId, "proposal-1")
        XCTAssertEqual(retire.releasedAssignments, 1)
        XCTAssertEqual(retire.archivedWorkspaces, 1)
    }
}
