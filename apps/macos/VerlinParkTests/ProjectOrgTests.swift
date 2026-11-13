@testable import VerlinPark
import XCTest

final class ProjectOrgTests: XCTestCase {
    private let decoder = JSONDecoder()

    private func snapshotJSON() -> Data {
        Data(
            """
            {
              "project_id": "prj-1",
              "generated_at": "2026-11-13T00:00:00Z",
              "enabled": true,
              "policy": {
                "enabled": true,
                "max_managers": 4,
                "max_workers": 10,
                "hire_requires_approval": true,
                "retirement_requires_approval": true
              },
              "retirement_policy": {
                "enabled": true,
                "lookback_days": 30,
                "min_completed_tasks": 3,
                "max_consecutive_failures": 3,
                "max_verifier_rejections": 2,
                "max_unsubstantiated_handoffs": 2,
                "max_false_consensus": 2,
                "cooldown_secs": 86400,
                "require_approval": true
              },
              "counts": {
                "identities": 4,
                "leaders": 1,
                "managers": 1,
                "workers": 1,
                "active": 2,
                "idle": 1,
                "paused": 1,
                "retired": 1,
                "imported": 1,
                "active_assignments": 1,
                "active_workspaces": 1,
                "unread_inbox": 3,
                "memory_count": 2,
                "pending_hire_requests": 1,
                "pending_retirement_proposals": 1,
                "retirement_candidates": 1
              },
              "leader": {
                "identity": {
                  "id": "leader-1",
                  "project_id": "prj-1",
                  "role": "leader",
                  "title": "Leader",
                  "status": "active",
                  "generation": 1,
                  "created_at": "2026-11-13T00:00:00Z",
                  "updated_at": "2026-11-13T00:00:00Z",
                  "origin": "local"
                },
                "unread_inbox": 0,
                "memory_count": 0,
                "performance": {"completed_tasks": 1, "failed_tasks": 0, "evidence_count": 1},
                "retirement": {},
                "reports": [{
                  "identity": {
                    "id": "manager-1",
                    "project_id": "prj-1",
                    "role": "manager",
                    "department": "Platform",
                    "component": "platform",
                    "title": "Platform Manager",
                    "status": "paused",
                    "generation": 3,
                  "created_at": "2026-11-13T00:00:00Z",
                  "updated_at": "2026-11-13T00:00:00Z",
                    "origin": "local"
                  },
                  "active_assignment": {
                    "session_id": "ses-1",
                    "role": "manager",
                    "assigned_at": "2026-11-13T00:00:00Z",
                    "lease_active": true,
                    "assignment_count": 2,
                    "session_count": 1
                  },
                  "workspace": {
                    "id": "ws-1",
                    "identity_id": "manager-1",
                    "component": "platform",
                    "branch": "verlinpark/components/platform",
                    "status": "active",
                    "lease_active": true,
                    "workspace_count": 1
                  },
                  "unread_inbox": 3,
                  "memory_count": 2,
                  "performance": {"completed_tasks": 2, "failed_tasks": 1, "verifier_rejections": 1},
                  "retirement": {
                    "candidate": true,
                    "severity": 1001,
                    "reasons": [{"code": "consecutive_failures", "count": 3, "threshold": 2}],
                    "pending_proposals": 1,
                    "latest_proposal_id": "pro-1",
                    "latest_proposal_status": "pending"
                  },
                  "reports": [{
                    "identity": {
                      "id": "worker-1",
                      "project_id": "prj-1",
                      "role": "worker",
                      "department": "Platform",
                      "component": "auth",
                      "title": "Auth Worker",
                      "parent_identity_id": "manager-1",
                      "status": "active",
                      "generation": 2,
                  "created_at": "2026-11-13T00:00:00Z",
                  "updated_at": "2026-11-13T00:00:00Z",
                      "origin": "local"
                    },
                    "unread_inbox": 0,
                    "memory_count": 0,
                    "performance": {},
                    "retirement": {},
                    "reports": []
                  }]
                }]
              },
              "unassigned": [{
                "identity": {
                  "id": "imported-1",
                  "project_id": "prj-1",
                  "role": "worker",
                  "title": "Imported Worker",
                  "status": "retired",
                  "generation": 1,
                  "created_at": "2026-11-13T00:00:00Z",
                  "updated_at": "2026-11-13T00:00:00Z",
                  "origin": "imported"
                },
                "unread_inbox": 0,
                "memory_count": 0,
                "performance": {},
                "retirement": {},
                "reports": []
              }],
              "departments": [{
                "name": "Platform",
                "manager_count": 1,
                "worker_count": 1,
                "active_count": 2,
                "active_workers": 1,
                "unread_inbox": 3,
                "manager_ids": ["manager-1"],
                "worker_ids": ["worker-1"]
              }],
              "active_assignments": [{
                "session_id": "ses-1",
                "project_id": "prj-1",
                "identity_id": "manager-1",
                "role": "manager",
                "status": "active",
                "assigned_at": "2026-11-13T00:00:00Z",
                "heartbeat_at": "2026-11-13T00:05:00Z",
                "lease_active": true
              }],
              "workspace_summaries": [],
              "pending_hire_requests": [{
                "id": "hire-1",
                "project_id": "prj-1",
                "identity_id": "reserved-1",
                "role": "worker",
                "title": "New Worker",
                "status": "pending",
                "requested_at": "2026-11-13T00:00:00Z",
                "requested_by": "operator",
                "approval_risk": "host_state",
                "origin": "local"
              }],
              "pending_retirement_proposals": [{
                "id": "pro-1",
                "project_id": "prj-1",
                "identity_id": "manager-1",
                "status": "pending",
                "reason_code": "consecutive_failures",
                "reasons": [{"code": "consecutive_failures", "count": 3, "threshold": 2}],
                "proposed_at": "2026-11-13T00:00:00Z",
                "proposed_by": "operator",
                "origin": "local"
              }],
              "truncated": false,
              "warnings": ["1 imported identities are read-only archive evidence"]
            }
            """.utf8
        )
    }

    private func detailJSON(readOnly: Bool = false) -> Data {
        Data(
            """
            {
              "project_id": "prj-1",
              "generated_at": "2026-11-13T00:00:00Z",
              "identity": {
                "id": "worker-1",
                "project_id": "prj-1",
                "role": "worker",
                "department": "Platform",
                "component": "auth",
                "title": "Auth Worker",
                "profile": "coding",
                "status": "active",
                "generation": 4,
                  "created_at": "2026-11-13T00:00:00Z",
                  "updated_at": "2026-11-13T00:00:00Z",
                "origin": "local"
              },
              "policy": {
                "enabled": true,
                "max_managers": 4,
                "max_workers": 10,
                "hire_requires_approval": true,
                "retirement_requires_approval": true
              },
              "retirement_policy": {"enabled": false},
              "node": {
                "identity": {
                  "id": "worker-1",
                  "project_id": "prj-1",
                  "role": "worker",
                  "title": "Auth Worker",
                  "status": "active",
                  "generation": 4,
                  "created_at": "2026-11-13T00:00:00Z",
                  "updated_at": "2026-11-13T00:00:00Z",
                  "origin": "local"
                },
                "active_assignment": {
                  "session_id": "ses-1",
                  "role": "worker",
                  "assigned_at": "2026-11-13T00:00:00Z",
                  "lease_active": true,
                  "assignment_count": 1,
                  "session_count": 1
                },
                "unread_inbox": 1,
                "memory_count": 1,
                "performance": {"completed_tasks": 4},
                "retirement": {},
                "reports": []
              },
              "assignments": [{
                "session_id": "ses-1",
                "project_id": "prj-1",
                "identity_id": "worker-1",
                "role": "worker",
                "status": "active",
                "assigned_at": "2026-11-13T00:00:00Z",
                "heartbeat_at": "2026-11-13T00:05:00Z",
                "lease_active": true
              }],
              "inbox": [{
                "id": "msg-1",
                "kind": "task",
                "status": "unread",
                "from_label": "operator",
                "created_at": "2026-11-13T00:00:00Z"
              }],
              "inbox_counts": {"unread": 1, "read": 2, "done": 3, "expired": 0},
              "memories": [{
                "id": "mem-1",
                "project_id": "prj-1",
                "scope": "agent",
                "content": "Auth owns JWT validation.",
                "source": "agent",
                "status": "active",
                "pinned": true
              }],
              "memory_total": 3,
              "performance": {
                "project_id": "prj-1",
                "identity_id": "worker-1",
                "role": "worker",
                "status": "active",
                "lookback_days": 30,
                "completed_tasks": 4,
                "failed_tasks": 1,
                "consecutive_failures": 0,
                "retries": 2,
                "verifier_approvals": 3,
                "verifier_rejections": 1,
                "input_tokens": 100,
                "output_tokens": 20,
                "cost_usd": 0.25,
                "reason_counts": [{"code": "false_consensus", "count": 1}],
                "evidence": [{"kind": "task", "reference": "T1", "reason": "task_failed"}]
              },
              "retirement_candidates": [],
              "retirement_proposals": [],
              "hire_requests": [],
              "workspaces": [{
                "id": "ws-1",
                "identity_id": "worker-1",
                "component": "auth",
                "branch": "verlinpark/components/auth",
                "status": "active",
                "lease_active": true,
                "workspace_count": 1
              }],
              "parent": {
                "id": "manager-1",
                "project_id": "prj-1",
                "role": "manager",
                "status": "active",
                "generation": 1,
                  "created_at": "2026-11-13T00:00:00Z",
                  "updated_at": "2026-11-13T00:00:00Z",
                "origin": "local"
              },
              "successor": null,
              "reports": [],
              "employment_events": [{
                "id": "evt-1",
                "project_id": "prj-1",
                "seq": 2,
                "identity_id": "worker-1",
                "kind": "agent.transfer.approved",
                "payload": {"approved": true},
                "created_at": "2026-11-13T00:00:00Z"
              }],
              "read_only": \(readOnly),
              "read_only_reason": \(readOnly ? "\"imported identities are read-only archive evidence\"" : "null"),
              "truncated": true,
              "warnings": ["one or more detail lists reached their bound"]
            }
            """.utf8
        )
    }

    func testOrgSnapshotDecodesHierarchyCountsAndPendingActions() throws {
        let snapshot = try decoder.decode(AgentOrgSnapshot.self, from: snapshotJSON())
        XCTAssertEqual(snapshot.projectId, "prj-1")
        XCTAssertTrue(snapshot.enabled)
        XCTAssertEqual(snapshot.counts.liveIdentities, 3)
        XCTAssertEqual(snapshot.counts.pendingOrgActions, 2)
        XCTAssertEqual(snapshot.leader?.identity.id, "leader-1")
        XCTAssertEqual(snapshot.leader?.reports.count, 1)
        XCTAssertEqual(snapshot.leader?.reports.first?.reports.first?.identity.id, "worker-1")
        XCTAssertEqual(snapshot.departments.first?.workerIds, ["worker-1"])
        XCTAssertEqual(snapshot.pendingHireRequests.first?.status, "pending")
        XCTAssertEqual(snapshot.pendingRetirementProposals.first?.identityId, "manager-1")
        XCTAssertEqual(snapshot.activeAssignments.first?.identityId, "manager-1")
        XCTAssertEqual(snapshot.workspaceSummaries.count, 0)
        XCTAssertFalse(snapshot.isEmpty)
        XCTAssertEqual(snapshot.node(identityID: "worker-1")?.statusLabel, "active")
        XCTAssertEqual(snapshot.node(identityID: "imported-1")?.statusLabel, "imported")
        XCTAssertEqual(snapshot.parentChoices.count, 2)
        XCTAssertEqual(snapshot.managerChoices.first?.id, "manager-1")
        XCTAssertEqual(snapshot.leaderChoices.first?.id, "leader-1")
        XCTAssertEqual(snapshot.warnings.count, 1)
    }

    func testOrgNodeAttentionAndRetirementPresentation() throws {
        let snapshot = try decoder.decode(AgentOrgSnapshot.self, from: snapshotJSON())
        let manager = try XCTUnwrap(snapshot.node(identityID: "manager-1"))
        XCTAssertTrue(manager.needsAttention)
        XCTAssertTrue(manager.isLiveAssignment)
        XCTAssertEqual(manager.performance.taskSummary, "2✓ 1×")
        XCTAssertTrue(manager.retirement.candidate)
        XCTAssertTrue(manager.retirement.hasPendingProposal)
        XCTAssertEqual(manager.retirement.reasonSummary, "consecutive_failures 3/2")
        let imported = try XCTUnwrap(snapshot.node(identityID: "imported-1"))
        XCTAssertTrue(imported.identity.isImported)
        XCTAssertTrue(imported.needsAttention)
        XCTAssertEqual(imported.statusLabel, "imported")
    }

    func testLegacySnapshotDefaultsToEmptyDisabledState() throws {
        let json = try XCTUnwrap("{\"project_id\":\"prj-1\"}".data(using: .utf8))
        let snapshot = try decoder.decode(AgentOrgSnapshot.self, from: json)
        XCTAssertFalse(snapshot.enabled)
        XCTAssertTrue(snapshot.isEmpty)
        XCTAssertEqual(snapshot.counts.identities, 0)
        XCTAssertTrue(snapshot.unassigned.isEmpty)
        XCTAssertTrue(snapshot.activeAssignments.isEmpty)
        XCTAssertTrue(snapshot.pendingHireRequests.isEmpty)
        XCTAssertEqual(snapshot.policy, .disabledDefault)
        XCTAssertEqual(snapshot.retirementPolicy, .disabledDefault)
    }

    func testIdentityDetailDecodesBoundedTabsAndRedactsInboxByDefault() throws {
        let detail = try decoder.decode(AgentIdentityDetail.self, from: detailJSON())
        XCTAssertEqual(detail.identity.generation, 4)
        XCTAssertEqual(detail.inbox.count, 1)
        XCTAssertNil(detail.inbox.first?.summary)
        XCTAssertEqual(detail.inboxCounts.unread, 1)
        XCTAssertEqual(detail.memoryTotal, 3)
        XCTAssertEqual(detail.memories.first?.scope, "agent")
        XCTAssertEqual(detail.performance.completedTasks, 4)
        XCTAssertEqual(detail.workspaces.first?.branch, "verlinpark/components/auth")
        XCTAssertEqual(detail.parentIdentity?.id, "manager-1")
        XCTAssertEqual(detail.employmentEvents.first?.kind, "agent.transfer.approved")
        XCTAssertTrue(detail.canPromote)
        XCTAssertTrue(detail.canTransfer)
        XCTAssertTrue(detail.canRetire)
        XCTAssertFalse(detail.readOnly)
        XCTAssertTrue(detail.truncated)
        XCTAssertEqual(detail.warnings.count, 1)
        XCTAssertNil(detail.pendingRetirementProposal)
    }

    func testReadOnlyIdentityCannotBeMutatedFromTheConsole() throws {
        let detail = try decoder.decode(
            AgentIdentityDetail.self,
            from: detailJSON(readOnly: true)
        )
        XCTAssertTrue(detail.readOnly)
        XCTAssertNotNil(detail.readOnlyReason)
        XCTAssertFalse(detail.canPromote)
        XCTAssertFalse(detail.canTransfer)
        XCTAssertFalse(detail.canRetire)
    }

    func testMinimalDetailFallsBackToIdentityAndEmptyPerformance() throws {
        let json = try XCTUnwrap(
            """
            {
              "identity": {
                "id": "worker-1",
                "project_id": "prj-1",
                "role": "worker",
                "status": "idle",
                "generation": 1,
                  "created_at": "2026-11-13T00:00:00Z",
                  "updated_at": "2026-11-13T00:00:00Z",
                "origin": "local"
              }
            }
            """.data(using: .utf8)
        )
        let detail = try decoder.decode(AgentIdentityDetail.self, from: json)
        XCTAssertEqual(detail.node.identity.id, "worker-1")
        XCTAssertEqual(detail.performance.identityId, "worker-1")
        XCTAssertTrue(detail.assignments.isEmpty)
        XCTAssertEqual(detail.inboxCounts.unread, 0)
        XCTAssertFalse(detail.readOnly)
    }

    func testHireDraftValidationAndApprovalSemantics() {
        var draft = ProjectOrgHireDraft()
        XCTAssertEqual(draft.validationError(), "A title is required for the org chart.")
        draft.title = "Auth Worker"
        XCTAssertNil(draft.validationError())
        draft.role = "leader"
        XCTAssertNotNil(draft.validationError())
        draft.role = "worker"
        draft.profile = ""
        XCTAssertNotNil(draft.validationError())
        draft.profile = "coding"
        draft.approved = false
        XCTAssertTrue(draft.approvalNote.contains("pending"))
        draft.approved = true
        XCTAssertTrue(draft.approvalNote.contains("approved immediately"))
    }

    func testPromoteDraftOnlyAllowsTwoTransitions() {
        var draft = ProjectOrgPromoteDraft()
        draft.currentRole = "worker"
        draft.targetRole = "manager"
        XCTAssertNil(draft.validationError())
        draft.targetRole = "leader"
        XCTAssertNotNil(draft.validationError())
        draft.currentRole = "leader"
        draft.targetRole = "manager"
        XCTAssertNotNil(draft.validationError())
        draft.currentRole = "manager"
        draft.targetRole = "leader"
        XCTAssertNil(draft.validationError())
        XCTAssertTrue(draft.warning.contains("no live Leader"))
    }

    func testTransferPatchOmitsUnchangedClearsEmptyAndSendsChanged() {
        var draft = ProjectOrgTransferDraft()
        XCTAssertEqual(draft.patch(current: "Platform", draft: "Platform"), .unchanged)
        XCTAssertEqual(draft.patch(current: "Platform", draft: "  "), .clear)
        XCTAssertEqual(draft.patch(current: "Platform", draft: "Security"), .value("Security"))
        draft.department = "Security"
        XCTAssertTrue(
            draft.hasChanges(
                currentDepartment: "Platform",
                currentComponent: "auth",
                currentTitle: "Auth",
                currentParent: "manager-1"
            )
        )
        draft.clearParent = true
        XCTAssertTrue(
            draft.hasChanges(
                currentDepartment: "Security",
                currentComponent: "auth",
                currentTitle: "Auth",
                currentParent: "manager-1"
            )
        )
    }

    func testRetireDraftRequiresExplicitApprovalByDefault() {
        var draft = ProjectOrgRetireDraft()
        draft.identityID = "worker-1"
        draft.requiresApproval = true
        XCTAssertNotNil(draft.validationError())
        draft.proposalID = "pro-1"
        XCTAssertNil(draft.validationError())
        draft.proposalID = ""
        draft.approved = true
        XCTAssertNil(draft.validationError())
        XCTAssertTrue(draft.warning.contains("terminal"))
        XCTAssertTrue(draft.warning.contains("irreversible"))
    }

    func testErrorSurfacePreservesTypedCodeAndHint() {
        let error = CoreError.rpc(
            code: -32009,
            message: "identity is busy",
            data: .object([
                "code": .string("IDENTITY_BUSY"),
                "hint": .string("release the session assignment first"),
            ])
        )
        let surface = ProjectOrgErrorSurface.from(error)
        XCTAssertEqual(surface.code, "IDENTITY_BUSY")
        XCTAssertEqual(surface.message, "identity is busy")
        XCTAssertEqual(surface.hint, "release the session assignment first")
        XCTAssertTrue(surface.displayText.contains("IDENTITY_BUSY"))
        XCTAssertTrue(surface.displayText.contains("release the session"))
    }

    @MainActor
    func testAppModelPresentsApprovalAwareOrgSheets() throws {
        let snapshot = try decoder.decode(AgentOrgSnapshot.self, from: snapshotJSON())
        let model = AppModel()
        model.selectedProjectID = "prj-1"
        model.projectOrgSnapshot = snapshot

        model.presentProjectOrgHire()
        XCTAssertEqual(model.projectOrgSheet, .hire)
        XCTAssertFalse(model.projectOrgHireDraft.approved, "approval must never be synthesized")
        XCTAssertEqual(model.projectOrgHireDraft.parentIdentityID, "manager-1")
        model.projectOrgHireDraft.role = "manager"
        model.projectOrgHireRoleChanged()
        XCTAssertEqual(model.projectOrgHireDraft.parentIdentityID, "leader-1")
        model.projectOrgHireDraft.role = "worker"
        model.projectOrgHireRoleChanged()
        XCTAssertEqual(model.projectOrgHireDraft.parentIdentityID, "manager-1")

        model.presentProjectOrgPromote(identityID: "worker-1")
        XCTAssertEqual(model.projectOrgSheet, .promote)
        XCTAssertEqual(model.projectOrgPromoteDraft.currentRole, "worker")
        XCTAssertEqual(model.projectOrgPromoteDraft.targetRole, "manager")
        XCTAssertEqual(model.projectOrgActionIdentityID, "worker-1")

        model.presentProjectOrgTransfer(identityID: "worker-1")
        XCTAssertEqual(model.projectOrgSheet, .transfer)
        XCTAssertEqual(model.projectOrgTransferDraft.department, "Platform")
        XCTAssertEqual(model.projectOrgTransferDraft.component, "auth")
        XCTAssertEqual(model.projectOrgTransferDraft.parentIdentityID, "manager-1")

        model.presentProjectOrgRetire(identityID: "manager-1")
        XCTAssertEqual(model.projectOrgSheet, .retire)
        XCTAssertFalse(model.projectOrgRetireDraft.approved)
        XCTAssertTrue(model.projectOrgRetireDraft.requiresApproval)
        XCTAssertEqual(model.projectOrgRetireDraft.identityID, "manager-1")

        model.dismissProjectOrgSheet()
        XCTAssertNil(model.projectOrgSheet)
        XCTAssertNil(model.projectOrgActionIdentityID)
    }

    @MainActor
    func testAppModelShowsDisabledAndEmptyStateCounts() throws {
        let json = try XCTUnwrap("{\"project_id\":\"prj-1\"}".data(using: .utf8))
        let snapshot = try decoder.decode(AgentOrgSnapshot.self, from: json)
        let model = AppModel()
        model.selectedProjectID = "prj-1"
        model.projectOrgSnapshot = snapshot
        XCTAssertEqual(model.projectOrgPendingActions, 0)
        XCTAssertFalse(snapshot.enabled)
        XCTAssertTrue(snapshot.isEmpty)

        model.presentProjectOrgHire()
        XCTAssertFalse(model.projectOrgHireDraft.approved)
        XCTAssertEqual(model.projectOrgHireDraft.parentIdentityID, "")
    }
}
