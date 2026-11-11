import Foundation
@testable import VerlinPark
import XCTest

final class ToolActivityTests: XCTestCase {
    func testToolActivityDecodesSummaryAndEntry() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "entries": [
            {
              "tool_call_id": "call-1",
              "name": "edit_file",
              "seq": 12,
              "is_error": true,
              "error_code": "PATCH_CONFLICT",
              "retryable": true,
              "hint": "Re-read the file and regenerate the patch.",
              "duration_ms": 34,
              "output_bytes": 812,
              "truncated": true,
              "approval_risk": "workspace_write",
              "approval_mode": "smart",
              "approval_wait_ms": 900,
              "created_at": "2026-11-11T00:00:00Z"
            }
          ],
          "total": 9,
          "limit": 50,
          "truncated": true,
          "summary": {
            "calls": 1,
            "errors": 1,
            "retryable_errors": 1,
            "truncated_results": 1,
            "total_duration_ms": 34,
            "max_duration_ms": 34,
            "average_duration_ms": 34,
            "output_bytes": 812,
            "by_tool": [
              {
                "name": "edit_file",
                "calls": 1,
                "errors": 1,
                "total_duration_ms": 34,
                "output_bytes": 812
              }
            ]
          }
        }
        """.data(using: .utf8))

        let result = try JSONDecoder().decode(ToolActivityResult.self, from: json)
        XCTAssertEqual(result.sessionID, "ses-1")
        XCTAssertEqual(result.total, 9)
        XCTAssertTrue(result.truncated)
        XCTAssertEqual(result.entries.count, 1)
        let entry = try XCTUnwrap(result.entries.first)
        XCTAssertEqual(entry.id, "12-call-1")
        XCTAssertEqual(entry.errorCode, "PATCH_CONFLICT")
        XCTAssertTrue(entry.retryable)
        XCTAssertEqual(entry.hint, "Re-read the file and regenerate the patch.")
        XCTAssertEqual(entry.durationMs, 34)
        XCTAssertEqual(entry.outputBytes, 812)
        XCTAssertTrue(entry.truncated)
        XCTAssertEqual(entry.approvalRisk, "workspace_write")
        XCTAssertEqual(entry.approvalMode, "smart")
        XCTAssertEqual(entry.approvalWaitMs, 900)
        XCTAssertEqual(result.summary.byTool.first?.name, "edit_file")
    }

    func testEmptyToolActivityIsStable() throws {
        let json = try XCTUnwrap("""
        {"session_id":"ses-empty","entries":[],"total":0,"limit":50,"truncated":false,
         "summary":{"calls":0,"errors":0,"retryable_errors":0,"truncated_results":0,
         "total_duration_ms":0,"max_duration_ms":0,"average_duration_ms":0,
         "output_bytes":0,"by_tool":[]}}
        """.data(using: .utf8))
        let result = try JSONDecoder().decode(ToolActivityResult.self, from: json)
        XCTAssertTrue(ToolActivityMetrics.dashboardRows(result).isEmpty)
        XCTAssertEqual(
            ToolActivityMetrics.summaryLine(result.summary),
            "0 calls · 0 errors · avg 0ms · max 0ms"
        )
    }

    func testMetricsFormattingAndRetryHint() throws {
        let error = try decodeEntry([
            "tool_call_id": "a",
            "name": "shell",
            "seq": 2,
            "is_error": true,
            "error_code": "COMMAND_FAILED",
            "retryable": false,
            "hint": "Fix the first real error.",
            "duration_ms": 1500,
            "output_bytes": 2048,
        ])
        XCTAssertEqual(ToolActivityMetrics.statusLabel(error), "COMMAND_FAILED")
        XCTAssertEqual(ToolActivityMetrics.retryLabel(error), "not retryable")
        XCTAssertEqual(ToolActivityMetrics.durationLabel(1500), "1.5s")
        XCTAssertEqual(ToolActivityMetrics.byteLabel(2048), "2.0 KB")
        XCTAssertEqual(
            ToolActivityMetrics.errorLine(error),
            "COMMAND_FAILED · not retryable · Fix the first real error."
        )
        XCTAssertFalse(ToolActivityMetrics.isRetryableFailure(error))

        let ok = try decodeEntry([
            "tool_call_id": "b",
            "name": "read_file",
            "seq": 3,
            "is_error": false,
            "duration_ms": 7,
            "output_bytes": 10,
        ])
        XCTAssertEqual(ToolActivityMetrics.statusLabel(ok), "ok")
        XCTAssertNil(ToolActivityMetrics.retryLabel(ok))
        XCTAssertNil(ToolActivityMetrics.errorLine(ok))
        XCTAssertEqual(ToolActivityMetrics.durationLabel(65000), "1m 5s")
        XCTAssertEqual(ToolActivityMetrics.byteLabel(1_048_576), "1.0 MB")
    }

    func testDashboardRowsAreNewestFirstAndBounded() {
        let entries = (0 ..< 12).map { index in
            try? decodeEntry([
                "tool_call_id": "call-\(index)",
                "name": index.isMultiple(of: 2) ? "read_file" : "shell",
                "seq": index,
                "is_error": index == 11,
                "duration_ms": index,
                "output_bytes": index * 10,
            ])
        }.compactMap(\.self)
        let result = ToolActivityResult(
            sessionID: "ses-1",
            entries: entries,
            total: 12,
            limit: 50,
            truncated: false,
            summary: ToolActivitySummary(
                calls: 12,
                errors: 1,
                retryableErrors: 0,
                truncatedResults: 0,
                totalDurationMs: 66,
                maxDurationMs: 11,
                averageDurationMs: 5,
                outputBytes: 660,
                byTool: []
            )
        )
        let rows = ToolActivityMetrics.dashboardRows(result)
        XCTAssertEqual(rows.count, ToolActivityMetrics.dashboardRowLimit)
        XCTAssertEqual(rows.first?.seq, 11)
        XCTAssertEqual(rows.last?.seq, 4)
        XCTAssertNil(ToolActivityMetrics.dashboardRows(nil).first)
    }

    func testInspectorFiltersSearchAndToolSelection() throws {
        let entries = try [
            decodeEntry([
                "tool_call_id": "read",
                "name": "read_file",
                "seq": 1,
                "is_error": false,
                "duration_ms": 5,
                "output_bytes": 100,
            ]),
            decodeEntry([
                "tool_call_id": "edit",
                "name": "edit_file",
                "seq": 2,
                "is_error": true,
                "error_code": "PATCH_CONFLICT",
                "retryable": true,
                "hint": "Re-read the file and regenerate the patch.",
                "duration_ms": 90,
                "output_bytes": 20,
            ]),
            decodeEntry([
                "tool_call_id": "shell",
                "name": "shell",
                "seq": 3,
                "is_error": true,
                "error_code": "COMMAND_FAILED",
                "retryable": false,
                "hint": "Fix the first real error.",
                "duration_ms": 500,
                "output_bytes": 2000,
            ]),
        ]
        let result = ToolActivityResult(
            sessionID: "ses-1",
            entries: entries,
            total: 3,
            limit: 50,
            truncated: false,
            summary: ToolActivityMetrics.summary(for: entries)
        )
        XCTAssertEqual(ToolActivityMetrics.toolNames(result), ["edit_file", "read_file", "shell"])
        XCTAssertEqual(
            ToolActivityMetrics.filtered(result, filter: .errors, search: "", tool: nil)
                .map(\.name),
            ["shell", "edit_file"]
        )
        XCTAssertEqual(
            ToolActivityMetrics.filtered(result, filter: .retryable, search: "", tool: nil)
                .map(\.name),
            ["edit_file"]
        )
        XCTAssertEqual(
            ToolActivityMetrics.filtered(result, filter: .all, search: "patch", tool: nil)
                .map(\.name),
            ["edit_file"]
        )
        XCTAssertEqual(
            ToolActivityMetrics.filtered(result, filter: .all, search: "", tool: "shell")
                .map(\.name),
            ["shell"]
        )
        let summary = ToolActivityMetrics.summary(for: entries)
        XCTAssertEqual(summary.calls, 3)
        XCTAssertEqual(summary.errors, 2)
        XCTAssertEqual(summary.retryableErrors, 1)
        XCTAssertEqual(summary.maxDurationMs, 500)
        XCTAssertEqual(summary.byTool.first?.name, "shell")
        XCTAssertEqual(summary.errorCodes.map(\.code), ["PATCH_CONFLICT", "COMMAND_FAILED"].sorted())
        XCTAssertEqual(summary.calls, 3)
        XCTAssertEqual(summary.truncatedResults, 0)
        XCTAssertEqual(summary.outputBytes, 2120)
    }

    func testSummaryDecodesApprovalAndCodeBucketsAndReencodes() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "entries": [],
          "total": 0,
          "limit": 50,
          "truncated": false,
          "summary": {
            "calls": 4,
            "errors": 2,
            "retryable_errors": 1,
            "truncated_results": 1,
            "total_duration_ms": 900,
            "max_duration_ms": 600,
            "average_duration_ms": 225,
            "output_bytes": 4096,
            "approval_waits": 2,
            "total_approval_wait_ms": 350,
            "max_approval_wait_ms": 300,
            "by_tool": [],
            "error_codes": [
              {"code": "PATCH_CONFLICT", "calls": 1},
              {"code": "COMMAND_FAILED", "calls": 1}
            ]
          }
        }
        """.data(using: .utf8))
        let result = try JSONDecoder().decode(ToolActivityResult.self, from: json)
        XCTAssertEqual(result.summary.approvalWaits, 2)
        XCTAssertEqual(result.summary.totalApprovalWaitMs, 350)
        XCTAssertEqual(result.summary.maxApprovalWaitMs, 300)
        XCTAssertEqual(result.summary.errorCodes.first?.code, "PATCH_CONFLICT")
        let encoded = try JSONEncoder().encode(result)
        let decoded = try JSONDecoder().decode(ToolActivityResult.self, from: encoded)
        XCTAssertEqual(decoded, result)
    }

    func testSummaryDefaultsForLegacyPayloadAndEmptyFiltering() throws {
        let json = try XCTUnwrap("""
        {"calls":2,"errors":0,"retryable_errors":0,"truncated_results":0,
         "total_duration_ms":20,"max_duration_ms":15,"average_duration_ms":10,
         "output_bytes":64,"by_tool":[]}
        """.data(using: .utf8))
        let summary = try JSONDecoder().decode(ToolActivitySummary.self, from: json)
        XCTAssertEqual(summary.calls, 2)
        XCTAssertEqual(summary.approvalWaits, 0)
        XCTAssertEqual(summary.totalApprovalWaitMs, 0)
        XCTAssertEqual(summary.maxApprovalWaitMs, 0)
        XCTAssertTrue(summary.errorCodes.isEmpty)

        let empty = ToolActivityResult(
            sessionID: "ses-1",
            entries: [],
            total: 0,
            limit: 50,
            truncated: false,
            summary: summary
        )
        XCTAssertTrue(
            ToolActivityMetrics.filtered(empty, filter: .all, search: "x", tool: nil).isEmpty
        )
        XCTAssertTrue(ToolActivityMetrics.toolNames(empty).isEmpty)
        XCTAssertNil(ToolActivityMetrics.filtered(empty, filter: .errors, search: "", tool: nil).first)
    }

    func testLegacyEntryDefaultsAreConservative() throws {
        let data = try XCTUnwrap("""
        {"tool_call_id":"legacy","name":"read_file","seq":1,"is_error":true}
        """.data(using: .utf8))
        let entry = try JSONDecoder().decode(ToolActivityEntry.self, from: data)
        XCTAssertEqual(entry.toolCallID, "legacy")
        XCTAssertEqual(entry.errorCode, nil)
        XCTAssertFalse(entry.retryable)
        XCTAssertEqual(entry.durationMs, 0)
        XCTAssertEqual(entry.outputBytes, 0)
        XCTAssertFalse(entry.truncated)
        XCTAssertNil(entry.approvalRisk)
        XCTAssertNil(entry.approvalMode)
        XCTAssertNil(entry.approvalWaitMs)
        XCTAssertEqual(ToolActivityMetrics.statusLabel(entry), "TOOL_FAILED")
        XCTAssertEqual(ToolActivityMetrics.retryLabel(entry), "not retryable")
        XCTAssertEqual(ToolActivityMetrics.errorLine(entry), "TOOL_FAILED · not retryable")
    }

    func testMetricFormattingEdgesAndErrorLineWithHint() throws {
        XCTAssertEqual(ToolActivityMetrics.durationLabel(0), "0ms")
        XCTAssertEqual(ToolActivityMetrics.durationLabel(999), "999ms")
        XCTAssertEqual(ToolActivityMetrics.durationLabel(1000), "1.0s")
        XCTAssertEqual(ToolActivityMetrics.durationLabel(59999), "60.0s")
        XCTAssertEqual(ToolActivityMetrics.durationLabel(60000), "1m 0s")
        XCTAssertEqual(ToolActivityMetrics.durationLabel(3_661_000), "61m 1s")
        XCTAssertEqual(ToolActivityMetrics.byteLabel(0), "0 B")
        XCTAssertEqual(ToolActivityMetrics.byteLabel(1023), "1023 B")
        XCTAssertEqual(ToolActivityMetrics.byteLabel(1024), "1.0 KB")
        XCTAssertEqual(ToolActivityMetrics.byteLabel(1_048_575), "1024.0 KB")
        XCTAssertEqual(ToolActivityMetrics.byteLabel(1_048_576), "1.0 MB")

        let entry = try decodeEntry([
            "tool_call_id": "retry",
            "name": "remote_exec",
            "seq": 9,
            "is_error": true,
            "error_code": "REMOTE_TIMEOUT",
            "retryable": true,
            "hint": "Retry with a bounded operation.",
            "duration_ms": 250,
            "output_bytes": 12,
        ])
        XCTAssertEqual(
            ToolActivityMetrics.errorLine(entry),
            "REMOTE_TIMEOUT · retryable · Retry with a bounded operation."
        )
        XCTAssertTrue(ToolActivityMetrics.isRetryableFailure(entry))
        XCTAssertEqual(
            ToolActivityMetrics.summaryLine(
                ToolActivitySummary(
                    calls: 2,
                    errors: 1,
                    retryableErrors: 1,
                    truncatedResults: 0,
                    totalDurationMs: 400,
                    maxDurationMs: 250,
                    averageDurationMs: 200,
                    outputBytes: 100,
                    byTool: []
                )
            ),
            "2 calls · 1 errors · avg 200ms · max 250ms"
        )
    }

    func testEntryWithNilOptionalsReencodesWithoutInventingValues() throws {
        let data = try XCTUnwrap("""
        {"tool_call_id":"c","name":"read_file","seq":2,"is_error":false,
         "duration_ms":5,"output_bytes":10}
        """.data(using: .utf8))
        let entry = try JSONDecoder().decode(ToolActivityEntry.self, from: data)
        let encoded = try JSONEncoder().encode(entry)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: encoded) as? [String: Any]
        )
        XCTAssertNil(object["error_code"])
        XCTAssertNil(object["hint"])
        XCTAssertNil(object["approval_risk"])
        XCTAssertNil(object["approval_mode"])
        XCTAssertNil(object["approval_wait_ms"])
        XCTAssertEqual(object["retryable"] as? Bool, false)
        XCTAssertEqual(object["truncated"] as? Bool, false)
        XCTAssertEqual(object["created_at"] as? String, "")
        let decoded = try JSONDecoder().decode(ToolActivityEntry.self, from: encoded)
        XCTAssertEqual(decoded, entry)
    }

    func testFilterCombinationsAndSummaryBuckets() throws {
        let entries = try [
            decodeEntry([
                "tool_call_id": "a",
                "name": "shell",
                "seq": 1,
                "is_error": true,
                "error_code": "COMMAND_FAILED",
                "retryable": false,
                "duration_ms": 10,
                "output_bytes": 1,
                "approval_wait_ms": 100,
            ]),
            decodeEntry([
                "tool_call_id": "b",
                "name": "shell",
                "seq": 2,
                "is_error": true,
                "error_code": "COMMAND_FAILED",
                "retryable": true,
                "duration_ms": 20,
                "output_bytes": 2,
                "approval_wait_ms": 200,
            ]),
            decodeEntry([
                "tool_call_id": "c",
                "name": "read_file",
                "seq": 3,
                "is_error": false,
                "duration_ms": 30,
                "output_bytes": 3,
            ]),
        ]
        let result = ToolActivityResult(
            sessionID: "ses-1",
            entries: entries,
            total: 3,
            limit: 50,
            truncated: false,
            summary: ToolActivityMetrics.summary(for: entries)
        )
        XCTAssertEqual(result.summary.errorCodes, [
            ToolActivityCodeStat(code: "COMMAND_FAILED", calls: 2),
        ])
        XCTAssertEqual(result.summary.approvalWaits, 2)
        XCTAssertEqual(result.summary.totalApprovalWaitMs, 300)
        XCTAssertEqual(result.summary.maxApprovalWaitMs, 200)
        XCTAssertEqual(
            ToolActivityMetrics.filtered(result, filter: .retryable, search: "", tool: nil)
                .map(\.toolCallID),
            ["b"]
        )
        XCTAssertEqual(
            ToolActivityMetrics.filtered(result, filter: .errors, search: "shell", tool: "shell")
                .map(\.toolCallID),
            ["b", "a"]
        )
        XCTAssertEqual(
            ToolActivityMetrics.filtered(result, filter: .all, search: "read", tool: "shell")
                .count,
            0
        )
        XCTAssertEqual(result.summary.byTool.first?.name, "shell")
    }

    func testUnknownFutureCodesAreShownVerbatim() throws {
        let entry = try decodeEntry([
            "tool_call_id": "future",
            "name": "future_tool",
            "seq": 1,
            "is_error": true,
            "error_code": "FUTURE_CODE_V2",
            "retryable": true,
            "hint": "A future hint.",
            "duration_ms": 1,
            "output_bytes": 2,
        ])
        XCTAssertEqual(ToolActivityMetrics.statusLabel(entry), "FUTURE_CODE_V2")
        XCTAssertEqual(ToolActivityMetrics.retryLabel(entry), "retryable")
        XCTAssertEqual(
            ToolActivityMetrics.errorLine(entry),
            "FUTURE_CODE_V2 · retryable · A future hint."
        )
        XCTAssertTrue(ToolActivityMetrics.isRetryableFailure(entry))
    }

    func testSummaryLineUsesRoundedAverageAndMax() {
        let summary = ToolActivitySummary(
            calls: 3,
            errors: 0,
            retryableErrors: 0,
            truncatedResults: 0,
            totalDurationMs: 100,
            maxDurationMs: 80,
            averageDurationMs: 33,
            outputBytes: 0,
            byTool: []
        )
        XCTAssertEqual(ToolActivityMetrics.summaryLine(summary), "3 calls · 0 errors · avg 33ms · max 80ms")
        XCTAssertEqual(ToolActivityMetrics.dashboardRows(nil).count, 0)
        XCTAssertEqual(ToolActivityMetrics.toolNames(nil).count, 0)
        XCTAssertTrue(ToolActivityMetrics.filtered(nil, filter: .all, search: "", tool: nil).isEmpty)
    }

    func testEmptySummaryBucketsAreDisplayable() throws {
        let summary = ToolActivitySummary(
            calls: 0,
            errors: 0,
            retryableErrors: 0,
            truncatedResults: 0,
            totalDurationMs: 0,
            maxDurationMs: 0,
            averageDurationMs: 0,
            outputBytes: 0,
            approvalWaits: 0,
            totalApprovalWaitMs: 0,
            maxApprovalWaitMs: 0,
            byTool: [],
            errorCodes: []
        )
        XCTAssertEqual(summary.calls, 0)
        XCTAssertTrue(summary.byTool.isEmpty)
        XCTAssertTrue(summary.errorCodes.isEmpty)
        let encoded = try JSONEncoder().encode(summary)
        let decoded = try JSONDecoder().decode(ToolActivitySummary.self, from: encoded)
        XCTAssertEqual(decoded, summary)
    }

    func testSearchMatchesRiskAndApprovalModeButNotMissingContent() throws {
        let entry = try decodeEntry([
            "tool_call_id": "approval",
            "name": "shell",
            "seq": 5,
            "is_error": false,
            "duration_ms": 10,
            "output_bytes": 20,
            "approval_risk": "host_state",
            "approval_mode": "strict",
            "approval_wait_ms": 400,
        ])
        let result = ToolActivityResult(
            sessionID: "ses-1",
            entries: [entry],
            total: 1,
            limit: 50,
            truncated: false,
            summary: ToolActivityMetrics.summary(for: [entry])
        )
        XCTAssertEqual(
            ToolActivityMetrics.filtered(result, filter: .all, search: "HOST_STATE", tool: nil)
                .count,
            1
        )
        XCTAssertEqual(
            ToolActivityMetrics.filtered(result, filter: .all, search: "strict", tool: nil)
                .count,
            1
        )
        XCTAssertEqual(
            ToolActivityMetrics.filtered(result, filter: .all, search: "stdout", tool: nil)
                .count,
            0
        )
        XCTAssertEqual(
            ToolActivityMetrics.filtered(result, filter: .all, search: "", tool: "missing")
                .count,
            0
        )
    }

    func testResultTruncationAndIdentifiersAreStable() throws {
        let entry = try decodeEntry([
            "tool_call_id": "call-1",
            "name": "read_file",
            "seq": 0,
            "is_error": false,
            "duration_ms": 0,
            "output_bytes": 0,
        ])
        XCTAssertEqual(entry.id, "call-1")
        let result = ToolActivityResult(
            sessionID: "ses-1",
            entries: [entry],
            total: 100,
            limit: 1,
            truncated: true,
            summary: ToolActivityMetrics.summary(for: [entry])
        )
        XCTAssertTrue(result.truncated)
        XCTAssertEqual(result.total, 100)
        XCTAssertEqual(result.limit, 1)
        XCTAssertEqual(ToolActivityMetrics.dashboardRows(result).count, 1)
    }

    func testFilterLabelsAreStable() {
        XCTAssertEqual(ToolActivityFilter.all.label, "All")
        XCTAssertEqual(ToolActivityFilter.errors.label, "Errors")
        XCTAssertEqual(ToolActivityFilter.retryable.label, "Retryable")
        XCTAssertEqual(ToolActivityFilter.all.id, "all")
        XCTAssertEqual(ToolActivityFilter.allCases.count, 3)
    }

    func testTranscriptLegacyToolFinishedDecodesWithoutMetrics() {
        let events = [
            event(
                1,
                "tool.started",
                [
                    "tool_call_id": .string("tool-legacy"),
                    "name": .string("shell"),
                    "input": .object(["command": .string("cargo test")]),
                ]
            ),
            event(
                2,
                "tool.finished",
                [
                    "tool_call_id": .string("tool-legacy"),
                    "name": .string("shell"),
                    "output": .string("ok"),
                    "is_error": .bool(false),
                ]
            ),
        ]
        let entries = TranscriptProjection.build(events: events)
        guard case let .tool(entry) = entries[0] else {
            return XCTFail("expected tool entry")
        }
        XCTAssertNil(entry.errorCode)
        XCTAssertNil(entry.errorHint)
        XCTAssertNil(entry.durationMs)
        XCTAssertNil(entry.outputBytes)
        XCTAssertFalse(entry.retryable)
        XCTAssertFalse(entry.truncated)
        XCTAssertNotNil(entry.duration)
    }

    func testTranscriptFailureCarriesCodeHintAndMetrics() {
        let events = [
            event(
                1,
                "tool.started",
                [
                    "tool_call_id": .string("tool-fail"),
                    "name": .string("edit_file"),
                    "input": .object(["path": .string("src/lib.rs")]),
                ]
            ),
            event(
                2,
                "tool.finished",
                [
                    "tool_call_id": .string("tool-fail"),
                    "name": .string("edit_file"),
                    "output": .string("error[PATCH_CONFLICT] old_string was not found"),
                    "is_error": .bool(true),
                    "error_code": .string("PATCH_CONFLICT"),
                    "retryable": .bool(true),
                    "error_hint": .string("Re-read the file and regenerate the patch."),
                    "duration_ms": .number(42),
                    "output_bytes": .number(512),
                    "truncated": .bool(true),
                ]
            ),
        ]
        let entries = TranscriptProjection.build(events: events)
        guard case let .tool(entry) = entries[0] else {
            return XCTFail("expected tool entry")
        }
        XCTAssertEqual(entry.errorCode, "PATCH_CONFLICT")
        XCTAssertTrue(entry.retryable)
        XCTAssertEqual(entry.errorHint, "Re-read the file and regenerate the patch.")
        XCTAssertEqual(entry.durationMs, 42)
        XCTAssertEqual(entry.duration ?? 0, 0.042, accuracy: 0.0001)
        XCTAssertEqual(entry.outputBytes, 512)
        XCTAssertTrue(entry.truncated)
    }

    private func decodeEntry(_ object: [String: Any]) throws -> ToolActivityEntry {
        let data = try JSONSerialization.data(withJSONObject: object)
        return try JSONDecoder().decode(ToolActivityEntry.self, from: data)
    }

    private func event(
        _ seq: UInt64,
        _ type: String,
        _ payload: [String: JSONValue],
        at: String = "2026-11-11T00:00:00.000Z"
    ) -> Event {
        Event(
            sessionId: "ses-1",
            seq: seq,
            type: type,
            payload: .object(payload),
            createdAt: at
        )
    }
}
