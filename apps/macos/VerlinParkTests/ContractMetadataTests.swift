@testable import VerlinPark
import XCTest

final class ContractMetadataTests: XCTestCase {
    func testSessionContractReportDecodesBoundedMetadataWithoutText() throws {
        let json = """
        {
          "session_id": "ses-1",
          "project_id": "prj-1",
          "policy": {"enabled": true, "mode": "enforce", "enforce_shell": false},
          "contract": {
            "contract_id": "ctr_abc",
            "hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "role": "worker",
            "task_id": "T1",
            "plan_revision": 3,
            "allowed_paths_count": 2,
            "forbidden_paths_count": 1,
            "allowed_paths_preview": ["crates/vp-core", "docs"],
            "egress": "allowlist(example.com)",
            "unrestricted_scope": false,
            "acceptance_count": 2,
            "allowed_tools_count": 18,
            "max_writes": 10,
            "max_bytes": 4096,
            "max_commits": 1,
            "writes_used": 2,
            "bytes_used": 120,
            "commits_used": 0,
            "enforcement_mode": "enforce"
          },
          "recent_decisions": [
            {"seq": 4, "decision": "escalate", "tool": "write_file", "rule": "path.allowed_scope", "risk": "host_state", "reason": "outside task scope"}
          ]
        }
        """
        let report = try JSONDecoder().decode(
            SessionContractReport.self,
            from: Data(json.utf8)
        )
        XCTAssertEqual(report.sessionID, "ses-1")
        XCTAssertEqual(report.policy.mode, "enforce")
        XCTAssertEqual(report.decisionCount, 1)
        let contract = try XCTUnwrap(report.contract)
        XCTAssertEqual(contract.taskID, "T1")
        XCTAssertEqual(contract.role, "worker")
        XCTAssertEqual(contract.allowedPathsCount, 2)
        XCTAssertEqual(contract.acceptanceCount, 2)
        XCTAssertEqual(contract.egress, "allowlist(example.com)")
        XCTAssertEqual(contract.enforcementMode, "enforce")
        XCTAssertEqual(contract.writesUsed, 2)
        XCTAssertEqual(contract.shortHash.count, 12)
        XCTAssertEqual(contract.scopeLabel, "crates/vp-core, docs")
    }

    func testSessionContractReportAcceptsLegacyMissingContractAndPolicyDefaults() throws {
        let json = """
        {"session_id": "ses-2", "project_id": "prj-1"}
        """
        let report = try JSONDecoder().decode(
            SessionContractReport.self,
            from: Data(json.utf8)
        )
        XCTAssertNil(report.contract)
        XCTAssertTrue(report.policy.enabled)
        XCTAssertEqual(report.policy.mode, "enforce")
        XCTAssertEqual(report.decisionCount, 0)
    }
}
