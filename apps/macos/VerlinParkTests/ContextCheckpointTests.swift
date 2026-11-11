@testable import VerlinPark
import XCTest

final class ContextCheckpointTests: XCTestCase {
    func testContextCheckpointListDecodesAdditiveFields() throws {
        let json = """
        {
          "session_id": "ses-1",
          "latest_id": "ctx-2",
          "limit": 50,
          "policy": {"enabled": false, "max_checkpoints": 20, "max_age_days": 30},
          "checkpoints": [
            {
              "id": "ctx-1",
              "session_id": "ses-1",
              "project_id": "prj-1",
              "label": "auto summary #1",
              "mode": "summary",
              "summary": "Earlier conversation summary state",
              "through_message_id": "msg-9",
              "message_cursor": 9,
              "plan_revision": 2,
              "provider": "queue",
              "model": "test",
              "prompt_prefix_hash": "abc",
              "worktree_head": "deadbeef",
              "tokens_before": 90000,
              "tokens_after": 12000,
              "structured_state": {"goal": "ship D1", "plan_revision": 2},
              "source_compaction_seq": 42,
              "automatic": true,
              "created_at": "2026-11-11T00:00:00Z"
            },
            {
              "id": "ctx-2",
              "session_id": "ses-1",
              "project_id": "prj-1",
              "label": "manual",
              "mode": "manual",
              "message_cursor": 0,
              "plan_revision": 2,
              "provider": "queue",
              "model": "test",
              "tokens_before": 12000,
              "tokens_after": 12000,
              "created_at": "2026-11-11T00:01:00Z"
            }
          ]
        }
        """
        let result = try JSONDecoder().decode(
            ContextCheckpointListResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.sessionID, "ses-1")
        XCTAssertEqual(result.latestID, "ctx-2")
        XCTAssertEqual(result.checkpoints.count, 2)
        XCTAssertEqual(result.checkpoints[0].throughMessageID, "msg-9")
        XCTAssertEqual(result.checkpoints[0].tokenDelta, 78000)
        XCTAssertTrue(result.checkpoints[0].automatic)
        XCTAssertEqual(result.checkpoints[1].summary, nil)
        XCTAssertEqual(result.checkpoints[1].mode, "manual")
        XCTAssertEqual(result.checkpoints[1].messageCursor, 0)
        XCTAssertFalse(result.policy.enabled)
    }

    func testContextCheckpointDetailDecodesPreflightAndSummaryPreview() throws {
        let json = """
        {
          "checkpoint": {
            "id": "ctx-1",
            "session_id": "ses-1",
            "project_id": "prj-1",
            "label": "before",
            "mode": "summary",
            "summary": "line one line two",
            "message_cursor": 3,
            "plan_revision": 1,
            "provider": "queue",
            "model": "test",
            "tokens_before": 5000,
            "tokens_after": 500,
            "created_at": "2026-11-11T00:00:00Z"
          },
          "preflight": {
            "restorable": false,
            "errors": ["plan revision changed"],
            "warnings": [],
            "estimated_tokens": 1200,
            "hard_limit_tokens": 32000,
            "threshold_tokens": 1000,
            "active_messages": 4,
            "plan_revision": 2,
            "checkpoint_plan_revision": 1,
            "provider": "queue",
            "model": "test",
            "worktree_head": "abc",
            "worktree_descendant": true,
            "budget_exhausted": false
          },
          "is_active": false,
          "summary_preview": "line one line two"
        }
        """
        let detail = try JSONDecoder().decode(
            ContextCheckpointDetailResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(detail.checkpoint.id, "ctx-1")
        XCTAssertFalse(detail.preflight.restorable)
        XCTAssertEqual(detail.preflight.errors.first, "plan revision changed")
        XCTAssertTrue(detail.preflight.worktreeDescendant)
        XCTAssertTrue(detail.preflight.currentWorktreeHead == nil)
        XCTAssertEqual(detail.summaryPreview, "line one line two")
    }

    func testRestoreGateRefusesActiveSessionsAndPreflightErrors() {
        let checkpoint = ContextCheckpoint(
            id: "ctx-1",
            sessionID: "ses-1",
            projectID: "prj-1",
            label: "before",
            mode: "summary",
            summary: "summary",
            throughMessageID: nil,
            messageCursor: 0,
            planRevision: 1,
            provider: "queue",
            model: "test",
            promptPrefixHash: "abc",
            worktreeHead: "head",
            tokensBefore: 5000,
            tokensAfter: 500,
            structuredState: nil,
            sourceCompactionSeq: nil,
            automatic: true,
            createdAt: "2026-11-11T00:00:00Z"
        )
        let preflight = ContextRestorePreflight(
            restorable: false,
            errors: ["plan revision changed"],
            warnings: ["estimate is above threshold"],
            estimatedTokens: 1200,
            hardLimitTokens: 32000,
            thresholdTokens: 1000,
            activeMessages: 4,
            planRevision: 2,
            checkpointPlanRevision: 1,
            provider: "queue",
            model: "test",
            worktreeHead: "head",
            currentWorktreeHead: "head",
            worktreeDescendant: true,
            budgetExhausted: false
        )
        let failing = ContextCheckpointDetailResult(
            checkpoint: checkpoint,
            preflight: preflight,
            isActive: false,
            summaryPreview: "summary"
        )
        XCTAssertEqual(
            ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: "idle",
                busy: false,
                detail: failing
            ),
            "plan revision changed"
        )
        XCTAssertEqual(
            ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: "running",
                busy: false,
                detail: nil
            ),
            "Restore is available when the session is idle, failed, or cancelled."
        )
        XCTAssertEqual(
            ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: "imported",
                busy: false,
                detail: nil
            ),
            "Imported archives are read-only."
        )
        XCTAssertEqual(
            ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: "idle",
                busy: true,
                detail: nil
            ),
            "Another checkpoint operation is running."
        )
        XCTAssertNil(
            ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: "idle",
                busy: false,
                detail: nil
            )
        )

        let active = ContextCheckpointDetailResult(
            checkpoint: checkpoint,
            preflight: ContextRestorePreflight(
                restorable: true,
                errors: [],
                warnings: [],
                estimatedTokens: 500,
                hardLimitTokens: 32000,
                thresholdTokens: 1000,
                activeMessages: 2,
                planRevision: 1,
                checkpointPlanRevision: 1,
                provider: "queue",
                model: "test",
                worktreeHead: "head",
                currentWorktreeHead: "head",
                worktreeDescendant: true,
                budgetExhausted: false
            ),
            isActive: true,
            summaryPreview: "summary"
        )
        XCTAssertEqual(
            ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: "idle",
                busy: false,
                detail: active
            ),
            "This boundary is already active."
        )
        XCTAssertTrue(ContextCheckpointGate.needsForce(failing))
        XCTAssertFalse(ContextCheckpointGate.needsForce(active))
    }

    func testCheckpointLabelsStayCompact() {
        let checkpoint = ContextCheckpoint(
            id: "ctx-1",
            sessionID: "ses-1",
            projectID: "prj-1",
            label: "fallback label",
            mode: "summary",
            summary: "a very long summary that should be shortened before it reaches the list",
            throughMessageID: nil,
            messageCursor: 0,
            planRevision: 1,
            provider: "queue",
            model: "test",
            promptPrefixHash: "abc",
            worktreeHead: "head",
            tokensBefore: 90000,
            tokensAfter: 12000,
            structuredState: nil,
            sourceCompactionSeq: nil,
            automatic: false,
            createdAt: "2026-11-11T00:00:00Z"
        )
        XCTAssertEqual(ContextCheckpointGate.tokenDeltaLabel(checkpoint), "−78.0k")
        XCTAssertEqual(ContextCheckpointGate.tokenLabel(999), "999")
        XCTAssertEqual(ContextCheckpointGate.tokenLabel(1500), "1.5k")
        XCTAssertEqual(ContextCheckpointGate.tokenLabel(2_500_000), "2.5M")
        let preview = ContextCheckpointGate.summaryPreview(checkpoint, limit: 20)
        XCTAssertTrue(preview.hasSuffix("…"))
        XCTAssertLessThanOrEqual(preview.count, 21)
    }

    func testSessionContextStatusDecodesCheckpointCounters() throws {
        let json = """
        {
          "session_id": "ses-1",
          "usage": null,
          "last_compaction": null,
          "last_measurement": null,
          "compaction_count": 2,
          "last_compaction_latency_ms": 10,
          "last_compaction_tokens_before": 9000,
          "last_compaction_tokens_after": 3000,
          "compaction_background_reuse": false,
          "compaction_fallback_to_sync": false,
          "checkpoint_count": 3,
          "latest_checkpoint_id": "ctx-3",
          "latest_checkpoint_at": "2026-11-11T00:02:00Z"
        }
        """
        let status = try JSONDecoder().decode(
            SessionContextStatus.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(status.checkpointCount, 3)
        XCTAssertEqual(status.latestCheckpointID, "ctx-3")
        XCTAssertEqual(status.latestCheckpointAt, "2026-11-11T00:02:00Z")

        let legacy = try JSONDecoder().decode(
            SessionContextStatus.self,
            from: Data("""
            {
              "session_id": "ses-1",
              "usage": null,
              "last_compaction": null,
              "last_measurement": null,
              "compaction_count": 0
            }
            """.utf8)
        )
        XCTAssertNil(legacy.checkpointCount)
        XCTAssertNil(legacy.latestCheckpointID)
    }
}

final class ContextCheckpointActionResultTests: XCTestCase {
    func testRestoreAndPruneResultsDecode() throws {
        let restore = try JSONDecoder().decode(
            ContextRestoreResult.self,
            from: Data("""
            {
              "session_id": "ses-1",
              "checkpoint_id": "ctx-1",
              "mode": "summary",
              "through_message_id": "msg-9",
              "active_messages": 4,
              "estimated_tokens": 3200,
              "hard_limit_tokens": 32000,
              "warning": "above threshold",
              "compaction_event_seq": 13,
              "restored_event_seq": 14
            }
            """.utf8)
        )
        XCTAssertEqual(restore.checkpointID, "ctx-1")
        XCTAssertEqual(restore.mode, "summary")
        XCTAssertEqual(restore.throughMessageID, "msg-9")
        XCTAssertEqual(restore.activeMessages, 4)
        XCTAssertEqual(restore.warning, "above threshold")
        XCTAssertEqual(restore.restoredEventSeq, 14)

        let prune = try JSONDecoder().decode(
            ContextCheckpointPruneResult.self,
            from: Data("""
            {
              "session_id": "ses-1",
              "candidates": 3,
              "removed": 2,
              "dry_run": false,
              "checkpoints": [{
                "id": "ctx-1",
                "session_id": "ses-1",
                "project_id": "prj-1"
              }]
            }
            """.utf8)
        )
        XCTAssertEqual(prune.candidates, 3)
        XCTAssertEqual(prune.removed, 2)
        XCTAssertFalse(prune.dryRun)
        XCTAssertEqual(prune.checkpoints.first?.id, "ctx-1")
        XCTAssertEqual(prune.checkpoints.first?.mode, "manual")
        XCTAssertEqual(prune.checkpoints.first?.tokenDelta, 0)

        let delete = try JSONDecoder().decode(
            ContextCheckpointDeleteResult.self,
            from: Data("""
            {"session_id": "ses-1", "checkpoint_id": "ctx-2", "removed": false}
            """.utf8)
        )
        XCTAssertFalse(delete.removed)
        XCTAssertEqual(delete.checkpointID, "ctx-2")
    }

    func testPolicyResultAndPolicyDefaultsDecode() throws {
        let result = try JSONDecoder().decode(
            ContextCheckpointPolicyResult.self,
            from: Data("""
            {
              "project_id": "prj-1",
              "policy": {"enabled": true, "max_checkpoints": 6, "max_age_days": 12}
            }
            """.utf8)
        )
        XCTAssertEqual(result.projectID, "prj-1")
        XCTAssertTrue(result.policy.enabled)
        XCTAssertEqual(result.policy.maxCheckpoints, 6)
        XCTAssertEqual(result.policy.maxAgeDays, 12)

        let defaults = try JSONDecoder().decode(
            ContextCheckpointPolicy.self,
            from: Data("{}".utf8)
        )
        XCTAssertFalse(defaults.enabled)
        XCTAssertEqual(defaults.maxCheckpoints, 20)
        XCTAssertEqual(defaults.maxAgeDays, 30)
    }

    func testGateAllowsRestoreWhenOnlyAWarningExistsButFlagsForce() {
        let checkpoint = ContextCheckpoint(
            id: "ctx-1",
            sessionID: "ses-1",
            projectID: "prj-1",
            label: "before",
            mode: "summary",
            summary: "summary",
            throughMessageID: nil,
            messageCursor: 0,
            planRevision: 1,
            provider: "queue",
            model: "test",
            promptPrefixHash: "abc",
            worktreeHead: "head",
            tokensBefore: 5000,
            tokensAfter: 500,
            structuredState: nil,
            sourceCompactionSeq: nil,
            automatic: true,
            createdAt: "2026-11-11T00:00:00Z"
        )
        let detail = ContextCheckpointDetailResult(
            checkpoint: checkpoint,
            preflight: ContextRestorePreflight(
                restorable: true,
                errors: [],
                warnings: ["above threshold"],
                estimatedTokens: 1200,
                hardLimitTokens: 32000,
                thresholdTokens: 1000,
                activeMessages: 4,
                planRevision: 1,
                checkpointPlanRevision: 1,
                provider: "queue",
                model: "test",
                worktreeHead: "head",
                currentWorktreeHead: "head",
                worktreeDescendant: true,
                budgetExhausted: false
            ),
            isActive: false,
            summaryPreview: "summary"
        )
        XCTAssertNil(
            ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: "idle",
                busy: false,
                detail: detail
            )
        )
        XCTAssertTrue(ContextCheckpointGate.needsForce(detail))
        XCTAssertEqual(
            ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: "idle",
                busy: false,
                detail: detail
            ),
            nil
        )
    }

    func testSummaryPreviewFallsBackToTheLabel() throws {
        let checkpoint = try JSONDecoder().decode(
            ContextCheckpoint.self,
            from: Data("""
            {
              "id": "ctx-1",
              "session_id": "ses-1",
              "project_id": "prj-1",
              "label": "manual fallback label",
              "mode": "manual"
            }
            """.utf8)
        )
        XCTAssertEqual(
            ContextCheckpointGate.summaryPreview(checkpoint),
            "manual fallback label"
        )
        XCTAssertEqual(ContextCheckpointGate.tokenDeltaLabel(checkpoint), "no change")
    }
}

final class ContextCheckpointGateSafetyTests: XCTestCase {
    private func detail(
        errors: [String] = [],
        warnings: [String] = [],
        isActive: Bool = false,
        restorable: Bool = true,
        budgetExhausted: Bool = false
    ) -> ContextCheckpointDetailResult {
        ContextCheckpointDetailResult(
            checkpoint: ContextCheckpoint(
                id: "ctx-1",
                sessionID: "ses-1",
                projectID: "prj-1",
                label: "gate",
                mode: "manual",
                summary: nil,
                throughMessageID: nil,
                messageCursor: 0,
                planRevision: 0,
                provider: "queue",
                model: "test",
                promptPrefixHash: "prefix",
                worktreeHead: "head",
                tokensBefore: 0,
                tokensAfter: 0,
                structuredState: nil,
                sourceCompactionSeq: nil,
                automatic: false,
                createdAt: "2026-11-11T00:00:00Z"
            ),
            preflight: ContextRestorePreflight(
                restorable: restorable,
                errors: errors,
                warnings: warnings,
                estimatedTokens: 500,
                hardLimitTokens: 32000,
                thresholdTokens: 1000,
                activeMessages: 2,
                planRevision: 0,
                checkpointPlanRevision: 0,
                provider: "queue",
                model: "test",
                worktreeHead: "head",
                currentWorktreeHead: "head",
                worktreeDescendant: true,
                budgetExhausted: budgetExhausted
            ),
            isActive: isActive,
            summaryPreview: "gate"
        )
    }

    func testBudgetAndWorktreeErrorsAreSurfacedBeforeRestore() {
        XCTAssertEqual(
            ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: "idle",
                busy: false,
                detail: detail(
                    errors: ["session budget is exhausted"],
                    budgetExhausted: true
                )
            ),
            "session budget is exhausted"
        )
        XCTAssertEqual(
            ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: "idle",
                busy: false,
                detail: detail(errors: ["session worktree has uncommitted changes"])
            ),
            "session worktree has uncommitted changes"
        )
    }

    func testQueuedAndPausedSessionsAreGated() {
        for status in ["queued", "paused", "waiting_input"] {
            XCTAssertEqual(
                ContextCheckpointGate.restoreDisabledReason(
                    sessionStatus: status,
                    busy: false,
                    detail: nil
                ),
                "Restore is available when the session is idle, failed, or cancelled."
            )
        }
        for status in ["idle", "failed", "cancelled"] {
            XCTAssertNil(
                ContextCheckpointGate.restoreDisabledReason(
                    sessionStatus: status,
                    busy: false,
                    detail: nil
                )
            )
        }
    }

    func testWarningOnlyDetailAllowsRestoreButRequestsForce() {
        let warningOnly = detail(warnings: ["estimate above threshold"])
        XCTAssertNil(
            ContextCheckpointGate.restoreDisabledReason(
                sessionStatus: "idle",
                busy: false,
                detail: warningOnly
            )
        )
        XCTAssertTrue(ContextCheckpointGate.needsForce(warningOnly))
        XCTAssertFalse(ContextCheckpointGate.needsForce(detail()))
    }

    func testTokenFormattingHandlesBoundaries() {
        XCTAssertEqual(ContextCheckpointGate.tokenLabel(0), "0")
        XCTAssertEqual(ContextCheckpointGate.tokenLabel(999), "999")
        XCTAssertEqual(ContextCheckpointGate.tokenLabel(1000), "1.0k")
        XCTAssertEqual(ContextCheckpointGate.tokenLabel(999_999), "1000.0k")
        XCTAssertEqual(ContextCheckpointGate.tokenLabel(1_000_000), "1.0M")
    }
}
