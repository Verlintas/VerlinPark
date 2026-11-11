import Foundation
@testable import VerlinPark
import XCTest

final class ProcessSupervisorTests: XCTestCase {
    private func processJSON(state: String = "running") -> String {
        """
        {
          "id": "psup_01",
          "session_id": "session-1",
          "name": "server",
          "command": "cargo run --bin server",
          "argv": ["/bin/sh", "-c", "cargo run --bin server"],
          "cwd": "/tmp/work",
          "worktree": "/tmp/work",
          "env_hash": "env",
          "command_hash": "command",
          "state": "\(state)",
          "pid": 4242,
          "pgid": 4242,
          "nonce": "nonce",
          "started_at": "2026-11-11T00:00:00Z",
          "process_started_at": "Wed Nov 11 00:00:00 2026",
          "heartbeat_at": "2026-11-11T00:00:02Z",
          "finished_at": null,
          "exit_code": null,
          "signal": null,
          "output_path": "/data/processes/session-1/psup_01/output.log",
          "output_bytes": 128,
          "output_lines": 4,
          "total_output_bytes": 512,
          "output_rotations": 1,
          "max_output_bytes": 1048576,
          "max_output_lines": 20000,
          "owner_instance": "instance:9",
          "retention_deadline": null,
          "idle_ttl_secs": 3600,
          "last_access_at": "2026-11-11T00:00:02Z",
          "detail": "adopted after daemon restart"
        }
        """
    }

    func testSupervisedProcessDecodesFullWireShape() throws {
        let process = try JSONDecoder().decode(
            SupervisedProcess.self,
            from: Data(processJSON().utf8)
        )
        XCTAssertEqual(process.id, "psup_01")
        XCTAssertEqual(process.sessionId, "session-1")
        XCTAssertEqual(process.state, "running")
        XCTAssertEqual(process.pid, 4242)
        XCTAssertEqual(process.pgid, 4242)
        XCTAssertEqual(process.outputBytes, 128)
        XCTAssertEqual(process.totalOutputBytes, 512)
        XCTAssertEqual(process.outputRotations, 1)
        XCTAssertEqual(process.maxOutputBytes, 1_048_576)
        XCTAssertEqual(process.idleTtlSecs, 3600)
        XCTAssertEqual(process.detail, "adopted after daemon restart")
    }

    func testProcessListDecodesActiveCount() throws {
        let json = """
        {
          "session_id": "session-1",
          "processes": [\(processJSON())],
          "active": 1,
          "limit": 8
        }
        """
        let result = try JSONDecoder().decode(
            ProcessListResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.sessionId, "session-1")
        XCTAssertEqual(result.active, 1)
        XCTAssertEqual(result.limit, 8)
        XCTAssertEqual(result.processes.count, 1)
    }

    func testProcessLogsDecodePagingMetadata() throws {
        let json = """
        {
          "session_id": "session-1",
          "process_id": "psup_01",
          "state": "running",
          "content": "ready\\n",
          "offset": 0,
          "next_offset": 6,
          "total_bytes": 6,
          "eof": true,
          "truncated": false,
          "rotated": true
        }
        """
        let result = try JSONDecoder().decode(
            ProcessLogsResult.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(result.processId, "psup_01")
        XCTAssertEqual(result.content, "ready\n")
        XCTAssertEqual(result.nextOffset, 6)
        XCTAssertTrue(result.eof)
        XCTAssertTrue(result.rotated)
    }

    func testProcessStopDecodesForcedResult() throws {
        let json = """
        {
          "process": \(processJSON(state: "stopped")),
          "forced": true,
          "waited_ms": 1500
        }
        """
        let result = try JSONDecoder().decode(
            ProcessStopResult.self,
            from: Data(json.utf8)
        )
        XCTAssertTrue(result.forced)
        XCTAssertEqual(result.waitedMs, 1500)
        XCTAssertEqual(result.process.state, "stopped")
    }

    func testSupervisedProcessRoundTrips() throws {
        let process = try JSONDecoder().decode(
            SupervisedProcess.self,
            from: Data(processJSON().utf8)
        )
        let encoded = try JSONEncoder().encode(process)
        let decoded = try JSONDecoder().decode(SupervisedProcess.self, from: encoded)
        XCTAssertEqual(decoded, process)
    }
}
