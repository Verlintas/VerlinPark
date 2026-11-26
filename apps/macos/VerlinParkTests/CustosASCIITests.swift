@testable import VerlinPark
import XCTest

final class CustosASCIITests: XCTestCase {
    private let acceptedWordmark = [
        #"  ___         _"#,
        #" / __|_  _ __| |_ ___ ___"#,
        #"| (__| || (_-<  _/ _ (_-<"#,
        #" \___|\_,_/__/\__\___/__/"#,
    ]

    func testAcceptedWordmarkIsExactFourRowASCII() {
        XCTAssertEqual(CustosASCII.wordmark, acceptedWordmark)
        XCTAssertEqual(CustosASCII.wordmark.count, 4)
        XCTAssertEqual(CustosASCII.wordmarkText, acceptedWordmark.joined(separator: "\n"))
        XCTAssertTrue(isPureASCII(CustosASCII.wordmarkText))
        XCTAssertFalse(CustosASCII.wordmarkText.contains("\u{2500}"))
        XCTAssertFalse(CustosASCII.wordmarkText.contains("\u{2502}"))
    }

    func testBootHasEightFramesAndFinalFrameIsExactWordmark() {
        XCTAssertEqual(CustosASCII.bootFrames.count, 8)
        XCTAssertEqual(CustosASCII.bootFrames.last, CustosASCII.wordmark)
        XCTAssertEqual(CustosASCII.bootFrameStrings.count, 8)
        XCTAssertEqual(CustosASCII.bootFrameStrings.last, CustosASCII.wordmarkText)
        XCTAssertTrue(CustosASCII.bootFrames.allSatisfy { $0.count == 4 })
        XCTAssertTrue(CustosASCII.bootFrames.allSatisfy { isPureASCII($0.joined()) })
        XCTAssertTrue(CustosASCII.bootFrames.dropLast().allSatisfy { $0.joined().contains("|") })
    }

    func testCriticalHasSevenFramesNoScanlineAndFinalFrameIsExactWordmark() {
        XCTAssertEqual(CustosASCII.criticalFrames.count, 7)
        XCTAssertEqual(CustosASCII.criticalFrames.last, CustosASCII.wordmark)
        XCTAssertEqual(CustosASCII.criticalFrameStrings.count, 7)
        XCTAssertEqual(CustosASCII.criticalFrameStrings.last, CustosASCII.wordmarkText)
        XCTAssertTrue(CustosASCII.criticalFrames.allSatisfy { $0.count == 4 })
        XCTAssertTrue(CustosASCII.criticalFrames.allSatisfy { isPureASCII($0.joined()) })
        XCTAssertTrue(CustosASCII.criticalFrames[4].joined().contains("#"))
        let criticalText = CustosASCII.criticalFrameStrings.joined(separator: "\n").lowercased()
        XCTAssertFalse(criticalText.contains("scanline"))
    }

    func testIdleIsStaticWordmarkWithNoCursorCarrierOrScanline() {
        let idle = CustosASCII.wordmarkText
        XCTAssertEqual(idle, acceptedWordmark.joined(separator: "\n"))
        XCTAssertFalse(idle.hasSuffix("|"))
        XCTAssertFalse(idle.split(separator: "\n").contains { $0.contains("│") })
    }

    private func isPureASCII(_ value: String) -> Bool {
        value.unicodeScalars.allSatisfy { $0.value < 128 }
    }
}
