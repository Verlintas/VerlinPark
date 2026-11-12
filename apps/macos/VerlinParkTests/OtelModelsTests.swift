import Foundation
@testable import VerlinPark
import XCTest

final class OtelModelsTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testLegacyStatusDefaultsToDisabledLoopback() throws {
        let status = try decoder.decode(
            OtelStatusReport.self,
            from: Data(#"{"project_id":"proj-1"}"#.utf8)
        )
        XCTAssertEqual(status.projectID, "proj-1")
        XCTAssertNil(status.sessionID)
        XCTAssertFalse(status.enabled)
        XCTAssertFalse(status.policy.enabled)
        XCTAssertEqual(status.policy.endpoint, "http://127.0.0.1:4318")
        XCTAssertEqual(status.policy.batchLimit, 200)
        XCTAssertEqual(status.policy.timeoutSecs, 5)
        XCTAssertEqual(status.counts.totalSpans, 0)
        XCTAssertFalse(status.endpointValid)
        XCTAssertFalse(status.endpointLoopback)
        XCTAssertTrue(status.warnings.isEmpty)
    }

    func testStatusDecodesCountsAndExplicitProjectionMetadata() throws {
        let status = try decoder.decode(
            OtelStatusReport.self,
            from: Data(
                """
                {
                  "project_id": "proj-1",
                  "session_id": "ses-1",
                  "policy": {
                    "enabled": true,
                    "endpoint": "http://127.0.0.1:4318",
                    "service_name": "verlinpark",
                    "batch_limit": 50,
                    "timeout_secs": 3
                  },
                  "enabled": true,
                  "endpoint_valid": true,
                  "endpoint_loopback": true,
                  "traces_path": "http://127.0.0.1:4318/v1/traces",
                  "counts": {
                    "session_spans": 1,
                    "agent_spans": 2,
                    "tool_spans": 3,
                    "usage_spans": 4,
                    "total_spans": 10,
                    "events_examined": 20,
                    "events_mapped": 12,
                    "events_skipped": 8,
                    "truncated": false
                  },
                  "last_event_seq": 20,
                  "generated_at": "2026-11-12T00:00:00Z",
                  "warnings": []
                }
                """.utf8
            )
        )
        XCTAssertTrue(status.enabled)
        XCTAssertEqual(status.sessionID, "ses-1")
        XCTAssertEqual(status.counts.totalSpans, 10)
        XCTAssertEqual(status.counts.agentSpans, 2)
        XCTAssertEqual(status.countsLabel, "10 span(s) · 20 event(s)")
        XCTAssertEqual(status.endpointLabel, "http://127.0.0.1:4318 → http://127.0.0.1:4318/v1/traces")
        XCTAssertTrue(status.policy.endpointLooksLoopback)
    }

    func testExportAndTestResultsNeverCarrySessionContentFields() throws {
        let export = try decoder.decode(
            OtelExportResult.self,
            from: Data(
                """
                {
                  "session_id": "ses-1",
                  "exported": true,
                  "status": "exported",
                  "span_count": 4,
                  "available_spans": 4,
                  "truncated": false,
                  "http_status": 200,
                  "duration_ms": 4,
                  "endpoint": "http://127.0.0.1:4318/v1/traces",
                  "request_bytes": 2048,
                  "generated_at": "2026-11-12T00:00:00Z"
                }
                """.utf8
            )
        )
        XCTAssertTrue(export.exported)
        XCTAssertEqual(export.statusLabel, "EXPORTED")
        XCTAssertEqual(export.spanCount, 4)
        XCTAssertNil(export.errorCode)
        XCTAssertFalse(String(describing: export).contains("prompt"))
        XCTAssertFalse(String(describing: export).contains("content"))

        let test = try decoder.decode(
            OtelTestResult.self,
            from: Data(
                """
                {"project_id":"proj-1","ok":false,"http_status":503,"error_code":"OTEL_EXPORT_FAILED"}
                """.utf8
            )
        )
        XCTAssertFalse(test.ok)
        XCTAssertEqual(test.httpStatus, 503)
        XCTAssertEqual(test.errorCode, "OTEL_EXPORT_FAILED")
    }

    func testPolicyEnvelopeDecodesWarningsAndRejectsPublicHint() throws {
        let envelope = try decoder.decode(
            OtelPolicyEnvelope.self,
            from: Data(
                """
                {
                  "project_id": "proj-1",
                  "policy": {"enabled": false, "endpoint": "https://collector.example.com"},
                  "endpoint_valid": false,
                  "endpoint_loopback": false,
                  "warnings": ["export is disabled"]
                }
                """.utf8
            )
        )
        XCTAssertFalse(envelope.policy.enabled)
        XCTAssertFalse(envelope.policy.endpointLooksLoopback)
        XCTAssertEqual(envelope.warnings, ["export is disabled"])
    }

    func testNonLoopbackHostHintDoesNotClaimAuthorization() {
        let policy = OtelPolicy(endpoint: "http://10.0.0.5:4318")
        XCTAssertFalse(policy.endpointLooksLoopback)
        let ipv6 = OtelPolicy(endpoint: "http://[::1]:4318")
        XCTAssertTrue(ipv6.endpointLooksLoopback)
    }
}
