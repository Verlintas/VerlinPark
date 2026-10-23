import AppKit
@testable import VerlinPark
import XCTest

final class BrandConfigTests: XCTestCase {
    func testDecodesLegacyBrandConfigWithExtraAssistantName() throws {
        let json = """
        {
          "gradientStartHex": "#1E7A53",
          "displayName": "VerlinPark",
          "flag": { "pride": { "_0": "transgender" } },
          "gradientEndHex": "#0B1F16",
          "gradientAngle": 135,
          "solidHex": "#1F6F4A",
          "style": "flag",
          "assistantName": "Aviiya"
        }
        """
        let config = try JSONDecoder().decode(BrandConfig.self, from: Data(json.utf8))
        XCTAssertEqual(config.displayName, "VerlinPark")
        XCTAssertEqual(config.style, .flag)
        XCTAssertEqual(config.flag, .pride(.transgender))
    }

    func testBrandConfigRoundTrip() throws {
        var config = BrandConfig()
        config.displayName = "VerlinHome"
        config.style = .solid
        config.solidHex = "#112233"
        let data = try JSONEncoder().encode(config)
        let decoded = try JSONDecoder().decode(BrandConfig.self, from: data)
        XCTAssertEqual(decoded, config)
    }

    func testRegionFlagSelectionMigratesLegacyCountryKey() throws {
        let legacy = #"{"country":{"_0":"TW"}}"#
        let decoded = try JSONDecoder().decode(FlagSelection.self, from: Data(legacy.utf8))
        XCTAssertEqual(decoded, .region("TW"))

        let encoded = try JSONEncoder().encode(decoded)
        let text = String(decoding: encoded, as: UTF8.self)
        XCTAssertTrue(text.contains("region"))
        XCTAssertFalse(text.contains("country"))
    }

    func testRegionsCatalogIsNonEmpty() {
        XCTAssertGreaterThan(FlagCatalog.regions.count, 200)
    }

    func testPrideCatalogIsComplete() {
        XCTAssertEqual(FlagCatalog.pride.count, PrideFlag.allCases.count)
        XCTAssertGreaterThanOrEqual(FlagCatalog.pride.count, 12)
        XCTAssertTrue(FlagCatalog.pride.contains { $0.selection == .pride(.progress) })
    }

    func testResidentBridgeIsUnavailable() {
        let bridge = UnavailableResidentAgent()
        XCTAssertEqual(bridge.displayName, "Aviiya")
        XCTAssertEqual(bridge.status, .unavailable)
    }

    func testFlagAssetsAreBundled() {
        XCTAssertNotNil(NSImage(named: "US"))
        for flag in PrideFlag.allCases {
            XCTAssertNotNil(flag.image, "missing pride flag asset: \(flag.assetName)")
            XCTAssertNotNil(flag.sourceURL)
        }
    }
}
