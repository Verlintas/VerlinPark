@testable import VerlinPark
import XCTest

final class EffectReceiptsTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testSessionEffectsDecodesDoneAndUnknownWithoutContent() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "policy": {
            "enabled": true,
            "unknown_blocks_merge": true,
            "stale_intent_grace_secs": 120,
            "shell_mode": "external_only"
          },
          "counts": {
            "total": 2,
            "intent": 0,
            "done": 1,
            "failed": 0,
            "unknown": 1,
            "blocking": 1
          },
          "blocked": true,
          "block_reason": "EFFECT_UNKNOWN: 1 unknown effect receipt(s) block integration",
          "truncated": false,
          "receipts": [
            {
              "id": "effect-1",
              "session_id": "ses-1",
              "task_id": "task-1",
              "attempt": 1,
              "tool": "write_file",
              "target": {"kind": "workspace_write", "key": "src/lib.rs"},
              "state": "done",
              "evidence": {
                "probe": "workspace_sha256",
                "outcome": "observed",
                "sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
              }
            },
            {
              "id": "effect-2",
              "session_id": "ses-1",
              "attempt": 1,
              "tool": "remote_write",
              "target": {"kind": "remote_write", "key": "out/app.txt"},
              "state": "unknown",
              "evidence": {
                "probe": "remote_helper_hash",
                "outcome": "unverifiable",
                "reason": "probe_incomplete",
                "caveat": "helper evidence, not remote exactly-once"
              }
            }
          ]
        }
        """.data(using: .utf8))

        let report = try decoder.decode(SessionEffectsReport.self, from: json)
        XCTAssertEqual(report.sessionID, "ses-1")
        XCTAssertTrue(report.blocked)
        XCTAssertEqual(report.counts.blocking, 1)
        XCTAssertEqual(report.receipts.count, 2)
        XCTAssertEqual(report.receipts[0].target.label, "workspace_write:src/lib.rs")
        XCTAssertEqual(report.receipts[0].stateLabel, "DONE")
        XCTAssertFalse(report.receipts[0].blocksMerge)
        XCTAssertEqual(report.receipts[0].evidence.anchor, "aaaaaaaaaaaaaaaa")
        XCTAssertEqual(report.receipts[1].target.label, "remote_write:out/app.txt")
        XCTAssertTrue(report.receipts[1].blocksMerge)
        XCTAssertEqual(report.receipts[1].evidence.reason, "probe_incomplete")
        XCTAssertEqual(report.blockingReceipts.count, 1)
        XCTAssertEqual(report.unresolved.count, 1)
        XCTAssertTrue(report.policy.unknownBlocksMerge)
        // No file content, command text, or provider key is part of the model.
        XCTAssertFalse(String(describing: report).contains("content"))
    }

    func testLegacyEffectReportDefaultsAreSafe() throws {
        let json = try XCTUnwrap("""
        {"session_id": "ses-legacy"}
        """.data(using: .utf8))
        let report = try decoder.decode(SessionEffectsReport.self, from: json)
        XCTAssertEqual(report.sessionID, "ses-legacy")
        XCTAssertTrue(report.receipts.isEmpty)
        XCTAssertEqual(report.counts.total, 0)
        XCTAssertFalse(report.blocked)
        XCTAssertNil(report.blockReason)
        XCTAssertFalse(report.truncated)
        XCTAssertTrue(report.policy.enabled)
        XCTAssertTrue(report.policy.unknownBlocksMerge)
        XCTAssertEqual(report.policy.staleIntentGraceSecs, 120)
        XCTAssertEqual(report.policy.shellMode, "external_only")
    }

    func testReceiptDecodesWithMissingEvidenceAndAttempt() throws {
        let json = try XCTUnwrap("""
        {
          "id": "effect-1",
          "session_id": "ses-1",
          "tool": "shell",
          "target": {"kind": "shell", "key": "cmd-abc"},
          "state": "intent",
          "evidence": {"probe": "no_postcondition_probe", "outcome": "unverifiable"}
        }
        """.data(using: .utf8))
        let receipt = try decoder.decode(EffectReceiptView.self, from: json)
        XCTAssertEqual(receipt.attempt, 0)
        XCTAssertNil(receipt.taskID)
        XCTAssertNil(receipt.evidence.sha256)
        XCTAssertNil(receipt.evidence.caveat)
        XCTAssertFalse(receipt.evidence.replayed)
        XCTAssertTrue(receipt.blocksMerge)
        XCTAssertEqual(receipt.stateLabel, "INTENT")
    }

    func testReconcileResponseAndPolicyEnvelopeDecode() throws {
        let reconcile = try XCTUnwrap("""
        {
          "receipt": {
            "id": "effect-1",
            "state": "done",
            "target": {"kind": "remote_write", "key": "out/app.txt"},
            "evidence": {"sha256": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}
          },
          "changed": true
        }
        """.data(using: .utf8))
        let response = try decoder.decode(EffectReconcileResponse.self, from: reconcile)
        XCTAssertTrue(response.changed)
        XCTAssertEqual(response.receipt?.state, "done")
        XCTAssertEqual(response.receipt?.evidence.anchor?.count, 16)

        let envelope = try XCTUnwrap("""
        {"project_id": "prj-1", "policy": {"unknown_blocks_merge": false}}
        """.data(using: .utf8))
        let policy = try decoder.decode(EffectPolicyEnvelope.self, from: envelope).policy
        XCTAssertTrue(policy.enabled)
        XCTAssertFalse(policy.unknownBlocksMerge)
        XCTAssertEqual(policy.staleIntentGraceSecs, 120)
    }
}
