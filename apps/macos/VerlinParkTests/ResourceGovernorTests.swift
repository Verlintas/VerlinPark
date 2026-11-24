@testable import VerlinPark
import XCTest

final class ResourceGovernorTests: XCTestCase {
    func testResourceSnapshotDecodesBoundedDimensionsAndReadiness() throws {
        let json = try XCTUnwrap("""
        {
          "session_id": "ses-1",
          "generated_at": "2026-11-24T00:00:00Z",
          "profile": "balanced",
          "dimensions": [
            {"key":"money","label":"Provider cost","unit":"usd","used":0.0,
             "remaining":null,"soft_target":null,"hard_cap":null,"forecast_p50":null,
             "forecast_p90":null,"state":"ok","source":"unconfigured",
             "note":"prices are not configured"},
            {"key":"attention","label":"Operator attention","unit":"interrupts",
             "used":5.0,"remaining":15.0,"soft_target":16.0,"hard_cap":20.0,
             "forecast_p50":null,"forecast_p90":null,"state":"ok","source":"approvals",
             "note":"5 weighted interrupts"}
          ],
          "bottleneck": "attention",
          "attention": {"pending":1,"approved":2,"denied":0,"expired":0,
                        "interrupts":5,"by_risk":[{"key":"host_state","count":1,"weight":0}]},
          "readiness": {"session_id":"ses-1","level":"committed","shippable":false,
                        "mergeable":false,"latest_commit":"abc123","latest_test":null,
                        "verifier":null,"merge":null,"changed_paths":[],
                        "remaining_work":["T1: pending"],"blockers":[],
                        "generated_at":"2026-11-24T00:00:00Z"},
          "forecast": null,
          "warnings": ["R1 is observe-only."],
          "sample_calls": 7,
          "pricing_source": "unconfigured"
        }
        """.data(using: .utf8))
        let snapshot = try JSONDecoder().decode(ResourceSnapshotView.self, from: json)
        XCTAssertEqual(snapshot.profile, "balanced")
        XCTAssertEqual(snapshot.sampleCalls, 7)
        XCTAssertEqual(snapshot.pricingSource, "unconfigured")
        XCTAssertEqual(snapshot.dimension("money")?.state, "ok")
        XCTAssertEqual(snapshot.dimension("attention")?.hardCap, 20)
        XCTAssertEqual(snapshot.attention.pending, 1)
        XCTAssertEqual(snapshot.attention.interrupts, 5)
        XCTAssertEqual(snapshot.readiness?.level, "committed")
        XCTAssertEqual(snapshot.readiness?.remainingWork.count, 1)
        XCTAssertFalse(snapshot.readiness?.shippable ?? true)
        XCTAssertEqual(snapshot.bottleneck, "attention")
        XCTAssertTrue(snapshot.needsAttention)
    }

    func testResourceSnapshotDefaultsMissingOptionalCollections() throws {
        let json = try XCTUnwrap("""
        {"session_id":"ses-2","generated_at":"2026-11-24T00:00:00Z","profile":"balanced",
         "dimensions":[],"attention":{"pending":0},"warnings":[],"sample_calls":0,
         "pricing_source":"unconfigured"}
        """.data(using: .utf8))
        let snapshot = try JSONDecoder().decode(ResourceSnapshotView.self, from: json)
        XCTAssertTrue(snapshot.dimensions.isEmpty)
        XCTAssertNil(snapshot.readiness)
        XCTAssertEqual(snapshot.attention.interrupts, 0)
        XCTAssertTrue(snapshot.warnings.isEmpty)
        XCTAssertFalse(snapshot.needsAttention)
    }

    func testRunProfileWireMappingIsConservative() {
        XCTAssertEqual(RunProfile.fromWire("quality"), .quality)
        XCTAssertEqual(RunProfile.fromWire("FAST"), .fast)
        XCTAssertEqual(RunProfile.fromWire("unattended"), .unattended)
        XCTAssertEqual(RunProfile.fromWire("not-a-profile"), .balanced)
        XCTAssertEqual(RunProfile.allCases.count, 5)
    }

    func testRunContractDecodesCapsAndStopPolicy() throws {
        let json = try XCTUnwrap("""
        {"session_id":"ses-1","project_id":"prj-1","profile":"quality",
         "source":"derived","goal":"ship it","acceptance":["tests pass"],
         "risk_ceiling":"approval-managed","autonomy":"operator-configured",
         "hard_caps":{"token":2000000,"cost_usd":2.5,"wall_clock_secs":1800},
         "soft_targets":{"cost_usd":2.0,"time_secs":1440,"attention":16},
         "completion_reserve_ratio":0.2,"attention_cap":20,
         "stop_policy":{"require_evidence":true,"no_progress_turns":6,
                        "marginal_value_ratio":0.2,"partial_delivery":true},
         "warnings":[],"generated_at":"2026-11-24T00:00:00Z"}
        """.data(using: .utf8))
        let contract = try JSONDecoder().decode(RunContractView.self, from: json)
        XCTAssertEqual(contract.profile, "quality")
        XCTAssertEqual(contract.hardCaps.token, 2_000_000)
        XCTAssertEqual(contract.hardCaps.costUSD, 2.5)
        XCTAssertEqual(contract.softTargets.attention, 16)
        XCTAssertEqual(contract.attentionCap, 20)
        XCTAssertEqual(contract.stopPolicy.noProgressTurns, 6)
        XCTAssertEqual(contract.acceptance, ["tests pass"])
    }
}
