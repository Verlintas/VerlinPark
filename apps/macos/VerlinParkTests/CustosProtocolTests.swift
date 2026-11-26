import Foundation
@testable import VerlinPark
import XCTest

final class CustosProtocolTests: XCTestCase {
    func testStatusJSONDecodesOnlyRenderedFields() throws {
        let status = try JSONDecoder().decode(CustosStatus.self, from: Data(Self.statusJSON.utf8))

        XCTAssertEqual(status.state, .degraded)
        XCTAssertTrue(status.observeOnly)
        XCTAssertEqual(status.role, "core")
        XCTAssertEqual(status.charterHash, "sha256:0123456789abcdef0123456789abcdef")
        XCTAssertEqual(status.dimensions.count, 7)
        XCTAssertEqual(status.counts.sessionsTotal, 8)
        XCTAssertEqual(status.counts.sessionsRunning, 2)
        XCTAssertEqual(status.counts.sessionsPaused, 1)
        XCTAssertEqual(status.counts.sessionsFailed, 0)
        XCTAssertEqual(status.counts.tasksPending, 4)
        XCTAssertEqual(status.counts.mergesQueued, 1)
        XCTAssertEqual(status.counts.mergesConflict, 0)
        XCTAssertEqual(status.counts.effectsUnknown, 2)
        XCTAssertEqual(status.counts.approvalsPending, 3)
        XCTAssertEqual(status.counts.leasesExpired, 1)
        XCTAssertEqual(status.counts.eventAgeSecs, 3)
        XCTAssertEqual(status.ring.mode, "single_role")
        XCTAssertEqual(status.ring.fresh, 1)
        XCTAssertEqual(status.ring.stale, 0)
        XCTAssertEqual(status.ring.missing, 3)
        XCTAssertFalse(status.ring.quorum)
        XCTAssertEqual(status.generatedAt, "2026-11-26T12:00:00Z")
    }

    func testStatusPresentationKeepsNineCanonicalDimensions() throws {
        let status = try JSONDecoder().decode(CustosStatus.self, from: Data(Self.statusJSON.utf8))
        XCTAssertEqual(
            status.dimensionRows.map(\.key),
            ["runtime", "data", "leases", "queues", "effects", "signal", "ring", "processes", "vault"]
        )
        XCTAssertEqual(status.dimensionRows.map(\.label), [
            "RUNTIME", "DATA", "LEASES", "QUEUES", "EFFECTS", "SIGNAL", "RING", "PROCESSES", "VAULT",
        ])
        XCTAssertEqual(status.dimensionRows[2].state, .degraded)
        XCTAssertEqual(status.dimensionRows[3].state, .watch)
        XCTAssertEqual(status.countItems.count, 11)
        XCTAssertEqual(status.countItems.last?.value, "3s")
        XCTAssertEqual(status.ringLine, "SINGLE_ROLE  /  FRESH 1  /  STALE 0  /  MISSING 3  /  QUORUM NO")
        XCTAssertEqual(status.generatedLabel, "2026-11-26 12:00:00")
        XCTAssertEqual(status.shortCharter, "sha256:0123456789abcdef...")
        XCTAssertTrue(status.sentence.contains("One dimension is degraded"))
    }

    func testUnknownStateFailsClosedToUnknownPresentation() throws {
        let json = Self.statusJSON.replacingOccurrences(
            of: #""state": "degraded""#,
            with: #""state": "mystery""#
        )
        let status = try JSONDecoder().decode(CustosStatus.self, from: Data(json.utf8))
        XCTAssertEqual(status.state, .unknown)
        XCTAssertEqual(status.state.word, "UNKNOWN")
    }

    func testRPCEnvelopeDecodesStatusResultAndUsesReadOnlyRequest() throws {
        let json = """
        {"schema":"verlinpark.custos_rpc/v1","id":1,"ok":true,"result":\(Self.statusJSON)}
        """
        let envelope = try JSONDecoder().decode(CustosRPCEnvelope.self, from: Data(json.utf8))
        XCTAssertTrue(envelope.ok)
        XCTAssertEqual(envelope.id, 1)
        XCTAssertEqual(envelope.schema, "verlinpark.custos_rpc/v1")
        XCTAssertEqual(envelope.result?.state, .degraded)
        XCTAssertNil(envelope.error)

        XCTAssertEqual(
            CustosUnixSocketClient.statusRequestLine,
            "{\"id\":1,\"method\":\"status\",\"params\":{}}\n"
        )
        XCTAssertFalse(CustosUnixSocketClient.statusRequestLine.contains("act"))
        XCTAssertFalse(CustosUnixSocketClient.statusRequestLine.contains("pause"))
        XCTAssertFalse(CustosUnixSocketClient.statusRequestLine.contains("resume"))
    }

    func testDataDirectoryEnvironmentAndFallback() {
        let configured = CustosPaths.dataDirectory(
            environment: ["VERLINPARK_DATA_DIR": "/tmp/vp-custos-test"],
            homeDirectory: "/Users/example"
        )
        XCTAssertEqual(configured.path, "/tmp/vp-custos-test")

        let fallback = CustosPaths.dataDirectory(
            environment: [:],
            homeDirectory: "/Users/example"
        )
        XCTAssertEqual(
            fallback.path,
            "/Users/example/Library/Application Support/VerlinPark"
        )
        XCTAssertEqual(
            CustosPaths.defaultSocketURL(dataDirectory: fallback).path,
            "/Users/example/Library/Application Support/VerlinPark/custos/custos.sock"
        )
    }

    func testDiscoverySocketWinsWhenPresentAndMalformedDiscoveryFallsBack() throws {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("custos-test-\(UUID().uuidString)", isDirectory: true)
        let custosDirectory = directory.appendingPathComponent("custos", isDirectory: true)
        try FileManager.default.createDirectory(
            at: custosDirectory,
            withIntermediateDirectories: true
        )
        defer { try? FileManager.default.removeItem(at: directory) }

        let discoveryURL = custosDirectory.appendingPathComponent("discovery.json")
        let discovery = """
        {
          "schema": "verlinpark.custos_discovery/v1",
          "pid": 1,
          "socket": "/tmp/custos-alt.sock",
          "protocol": 1,
          "version": "test",
          "role": "core",
          "started_at": "now",
          "build_hash": "sha256:test",
          "observe_only": true
        }
        """
        try Data(discovery.utf8).write(to: discoveryURL)
        XCTAssertEqual(CustosPaths.resolvedSocketPath(dataDirectory: directory), "/tmp/custos-alt.sock")

        try Data(#"{"schema":"wrong","socket":"/tmp/other.sock"}"#.utf8).write(to: discoveryURL)
        XCTAssertEqual(
            CustosPaths.resolvedSocketPath(dataDirectory: directory),
            CustosPaths.defaultSocketURL(dataDirectory: directory).path
        )
    }

    static let statusJSON = """
    {
      "schema": "verlinpark.custos_status/v1",
      "generated_at": "2026-11-26T12:00:00Z",
      "state": "degraded",
      "observe_only": true,
      "role": "core",
      "protocol": 1,
      "version": "0.1.0",
      "charter_hash": "sha256:0123456789abcdef0123456789abcdef",
      "charter_status": "draft",
      "dimensions": [
        {"key": "runtime", "state": "ok", "detail": "daemon discovery present"},
        {"key": "data", "state": "ok", "detail": "schema 42 / wal 1024 bytes"},
        {"key": "leases", "state": "degraded", "detail": "4 live / 1 expired"},
        {"key": "queues", "state": "watch", "detail": "4 tasks pending / 1 merges queued / 0 conflicts"},
        {"key": "effects", "state": "ok", "detail": "2 unknown"},
        {"key": "signal", "state": "ok", "detail": "last event 3s ago"},
        {"key": "ring", "state": "ok", "detail": "1 fresh / 0 stale / 3 missing"}
      ],
      "counts": {
        "sessions_total": 8,
        "sessions_running": 2,
        "sessions_paused": 1,
        "sessions_failed": 0,
        "events_total": 1200,
        "event_age_secs": 3,
        "leases_live": 4,
        "leases_expired": 1,
        "runs_running": 2,
        "runs_interrupted": 0,
        "tasks_pending": 4,
        "tasks_running": 1,
        "tasks_blocked": 0,
        "merges_queued": 1,
        "merges_conflict": 0,
        "effects_unknown": 2,
        "approvals_pending": 3,
        "processes_live": 6,
        "db_bytes": 4096,
        "wal_bytes": 1024,
        "disk_free_bytes": 1000000,
        "disk_total_bytes": 2000000
      },
      "ring": {
        "mode": "single_role",
        "fresh": 1,
        "stale": 0,
        "missing": 3,
        "quorum": false,
        "roles": []
      },
      "last_action": null
    }
    """
}
