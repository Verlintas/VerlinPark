@testable import VerlinPark
import XCTest

final class FailureDiagnosisTests: XCTestCase {
    private let decoder = JSONDecoder()

    func testReportDecodesRootFindingCascadeAndGraphSummary() throws {
        let data = Data(
            """
            {
              "schema_version": 1,
              "session_id": "ses-1",
              "generated_at": "2026-11-13T00:00:00Z",
              "status": "attributed",
              "target": {"kind": "task", "id": "t2", "task_id": "t2", "status": "failed", "at_seq": 9},
              "earliest": {
                "finding_id": "handoff:t2:8",
                "reason_code": "unsubstantiated_handoff",
                "severity": "root",
                "node_id": "task:t2",
                "task_id": "t2",
                "at_seq": 8,
                "at": "2026-11-13T00:00:08Z",
                "summary": "the Worker result does not cite an upstream commit or artifact hash it used",
                "agents": [
                  {"agent_id": "worker-2", "agent_name": "Worker 2", "role": "worker", "task_id": "t2", "responsibility": "root"}
                ],
                "evidence": [
                  {"kind": "hash", "reference": "upstream", "hash": "aaaaaaaaaaaaaaaa", "event_seq": 4, "event_kind": "plan_task.finished"}
                ],
                "flags": ["handoff", "worker"]
              },
              "findings": [],
              "responsible_agents": [],
              "cascade": [
                {"node_id": "task:t1", "kind": "task", "task_id": "t1", "status": "completed", "at_seq": 4, "relation": "root"},
                {"node_id": "task:t2", "kind": "task", "task_id": "t2", "status": "failed", "at_seq": 9, "relation": "failure"}
              ],
              "graph": {"session_id": "ses-1", "roots": ["task:t1"], "nodes": [], "edges": [], "truncated": false},
              "graph_summary": {
                "nodes": 8, "edges": 7, "tasks": 2, "agents": 2, "artifacts": 1, "commits": 1,
                "tool_calls": 1, "test_evidence": 1, "verifications": 1, "merges": 1,
                "max_depth": 1, "depth_limited": false, "truncated": false,
                "scanned_events": 10, "total_events": 10, "window_start_seq": 1, "window_end_seq": 10,
                "digest": "bbbbbbbbbbbb"
              },
              "other_failures": 0,
              "assignments": [
                {"identity_id": "agt-1", "role": "worker", "status": "active", "assigned_at": "2026-11-13T00:00:00Z"}
              ],
              "advisory": true,
              "truncated": false,
              "warnings": [],
              "recorded_event_seq": 11
            }
            """.utf8
        )
        let report = try decoder.decode(SessionDiagnosisReport.self, from: data)
        XCTAssertTrue(report.advisory)
        XCTAssertTrue(report.isAttributed)
        XCTAssertTrue(report.hasRootFinding)
        XCTAssertEqual(report.earliest?.reasonCode, "unsubstantiated_handoff")
        XCTAssertEqual(report.earliest?.agents.first?.displayName, "Worker 2")
        XCTAssertEqual(report.cascade.count, 2)
        XCTAssertEqual(report.graphSummary.commits, 1)
        XCTAssertEqual(report.graphSummary.maxDepth, 1)
        XCTAssertEqual(report.assignments.first?.identityID, "agt-1")
        XCTAssertEqual(report.recordedEventSeq, 11)
        // The client model has no action or content fields.
        let encoded = String(describing: report)
        XCTAssertFalse(encoded.contains("prompt"))
        XCTAssertFalse(encoded.contains("toolOutput"))
    }

    func testLegacyReportDefaultsAreSafe() throws {
        let data = Data("{\"session_id\":\"ses-1\"}".utf8)
        let report = try decoder.decode(SessionDiagnosisReport.self, from: data)
        XCTAssertEqual(report.status, "no_failure")
        XCTAssertTrue(report.advisory)
        XCTAssertFalse(report.hasRootFinding)
        XCTAssertFalse(report.truncated)
        XCTAssertTrue(report.findings.isEmpty)
        XCTAssertTrue(report.cascade.isEmpty)
        XCTAssertEqual(report.graphSummary.nodes, 0)
        XCTAssertNil(report.earliest)
        XCTAssertNil(report.recordedEventSeq)
    }

    func testAttributionsLedgerDecodesBoundedRows() throws {
        let data = Data(
            """
            {
              "session_id": "ses-1",
              "total": 1,
              "truncated": false,
              "attributions": [{
                "event_seq": 11,
                "recorded_at": "2026-11-13T00:00:00Z",
                "attribution": {
                  "schema_version": 1,
                  "attribution_id": "fattr-1",
                  "session_id": "ses-1",
                  "status": "attributed",
                  "reason_code": "cascade_upstream",
                  "earliest_node_id": "failure:task:t1:4",
                  "earliest_seq": 4,
                  "responsible_agents": [],
                  "findings": [],
                  "cascade": [],
                  "graph_digest": "cccccccccccc",
                  "graph_nodes": 4,
                  "graph_edges": 3,
                  "truncated": false,
                  "advisory": true,
                  "created_at": "2026-11-13T00:00:00Z"
                }
              }]
            }
            """.utf8
        )
        let report = try decoder.decode(SessionAttributionsReport.self, from: data)
        XCTAssertEqual(report.total, 1)
        XCTAssertEqual(report.attributions.first?.eventSeq, 11)
        XCTAssertEqual(report.attributions.first?.attribution.reasonCode, "cascade_upstream")
        XCTAssertEqual(report.attributions.first?.attribution.advisory, true)
    }

    func testGraphNodesAndEdgesDecodeWithoutContentFields() throws {
        let data = Data(
            """
            {
              "session_id": "ses-1",
              "roots": ["task:t1"],
              "nodes": [{
                "id": "task:t1", "kind": "task", "label": "task t1", "task_id": "t1",
                "status": "completed", "at_seq": 1, "at": "2026-11-13T00:00:01Z",
                "evidence": [{"kind": "commit", "reference": "abc123", "hash": "abc123def456", "event_seq": 2}]
              }],
              "edges": [{"from": "task:t2", "to": "task:t1", "kind": "depends_on", "at_seq": 1}],
              "truncated": true
            }
            """.utf8
        )
        let graph = try decoder.decode(ProvenanceGraphView.self, from: data)
        XCTAssertEqual(graph.nodes.count, 1)
        XCTAssertEqual(graph.edges.first?.kind, "depends_on")
        XCTAssertTrue(graph.truncated)
        XCTAssertEqual(graph.nodes.first?.evidence.first?.hash, "abc123def456")
    }
}
