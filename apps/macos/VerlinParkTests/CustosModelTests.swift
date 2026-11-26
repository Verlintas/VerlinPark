@testable import VerlinPark
import XCTest

final class CustosModelTests: XCTestCase {
    @MainActor
    func testUnavailableProviderReportsSignalLostWithoutState() async {
        let model = CustosModel(provider: FailingCustosProvider(), pollInterval: .seconds(60))
        await model.refreshOnce()

        XCTAssertEqual(model.availability, .signalLost)
        XCTAssertEqual(model.state, .signalLost)
        XCTAssertEqual(model.stateWord, "SIGNAL LOST")
        XCTAssertEqual(model.protectionLabel, "SIGNAL LOST")
        XCTAssertEqual(model.menuBarSymbol, "diamond")
        XCTAssertNil(model.status)
        XCTAssertEqual(model.dimensionRows.count, 9)
        XCTAssertEqual(model.dimensionRows.map(\.state), Array(repeating: .unknown, count: 9))
        XCTAssertEqual(model.countItems.count, 11)
        XCTAssertTrue(model.countItems.allSatisfy { $0.value == "--" })
        XCTAssertEqual(model.roleLabel, "--")
        XCTAssertNotNil(model.lastError)
    }

    @MainActor
    func testSuccessfulProviderBecomesLiveAndExposesStatus() async throws {
        let status = try JSONDecoder().decode(
            CustosStatus.self,
            from: Data(CustosProtocolTests.statusJSON.utf8)
        )
        let model = CustosModel(
            provider: StaticCustosProvider(status: status),
            pollInterval: .seconds(60)
        )
        await model.refreshOnce()

        XCTAssertEqual(model.availability, .live)
        XCTAssertEqual(model.state, .degraded)
        XCTAssertEqual(model.stateWord, "DEGRADED")
        XCTAssertEqual(model.protectionLabel, "PROTECTION ACTIVE")
        XCTAssertEqual(model.menuBarSymbol, "diamond.fill")
        XCTAssertEqual(model.roleLabel, "CORE")
        XCTAssertEqual(model.status?.counts.sessionsTotal, 8)
        XCTAssertEqual(model.dimensionRows[2].state, .degraded)
        XCTAssertEqual(model.generatedLabel, "2026-11-26 12:00:00")
        XCTAssertNil(model.lastError)
    }

    @MainActor
    func testDiagnosticsProviderStoresLocalBundleResult() async {
        let diagnostics = CustosDiagnosticsResult(
            schema: "verlinpark.custos_diagnostics/v1",
            reportID: "cdr_test",
            generatedAt: "2026-11-26T12:00:00Z",
            directory: "/tmp/custos",
            standardPath: "/tmp/custos/standard.zip",
            forensicPath: "/tmp/custos/forensic.zip",
            bytes: ["standard": 10, "forensic": 20]
        )
        let model = CustosModel(
            provider: StaticDiagnosticsProvider(diagnostics: diagnostics),
            pollInterval: .seconds(60)
        )
        await model.generateDiagnostics()
        XCTAssertEqual(model.diagnostics, diagnostics)
        XCTAssertNil(model.diagnosticsError)
        XCTAssertFalse(model.isGeneratingDiagnostics)
    }

    @MainActor
    func testFailureClearsPreviouslyLiveStatus() async throws {
        let status = try JSONDecoder().decode(
            CustosStatus.self,
            from: Data(CustosProtocolTests.statusJSON.utf8)
        )
        let model = CustosModel(
            provider: SwitchingCustosProvider(status: status),
            pollInterval: .seconds(60)
        )

        await model.refreshOnce()
        XCTAssertEqual(model.availability, .live)
        XCTAssertNotNil(model.status)

        await model.refreshOnce()
        XCTAssertEqual(model.availability, .signalLost)
        XCTAssertNil(model.status)
        XCTAssertEqual(model.state, .signalLost)
    }
}

private struct FailingCustosProvider: CustosStatusProviding {
    func fetchStatus() async throws -> CustosStatus {
        throw CustosSocketError.unavailable
    }
}

private struct StaticDiagnosticsProvider: CustosStatusProviding, CustosDiagnosticsProviding {
    let diagnostics: CustosDiagnosticsResult

    func fetchStatus() async throws -> CustosStatus {
        throw CustosSocketError.unavailable
    }

    func generateDiagnostics() async throws -> CustosDiagnosticsResult {
        diagnostics
    }
}

private struct StaticCustosProvider: CustosStatusProviding {
    let status: CustosStatus

    func fetchStatus() async throws -> CustosStatus {
        status
    }
}

private actor SwitchingCustosProvider: CustosStatusProviding {
    private let status: CustosStatus
    private var calls = 0

    init(status: CustosStatus) {
        self.status = status
    }

    func fetchStatus() async throws -> CustosStatus {
        calls += 1
        if calls == 1 {
            return status
        }
        throw CustosSocketError.unavailable
    }
}
