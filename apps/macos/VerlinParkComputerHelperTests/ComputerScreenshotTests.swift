import CoreGraphics
import Foundation
import XCTest

final class ComputerScreenshotTests: XCTestCase {
    private func scratch() throws -> URL {
        let url = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("vp-screenshot-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    func testOutputPathMustStayBelowScreenshots() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("a.png", scratch: root)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots/../../a.png", scratch: root)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("Screenshots/a.png", scratch: root)
        )
        XCTAssertNoThrow(
            try ScreenshotOutput.validatedRelativePath("screenshots/a.png", scratch: root)
        )
    }

    func testOutputPathRejectsUnsupportedCharacters() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots/a b.png", scratch: root)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots/a:b.png", scratch: root)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots/a\\b.png", scratch: root)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots/a\nb.png", scratch: root)
        )
    }

    func testOutputPathRejectsEmptyAndTooLongNames() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        XCTAssertThrowsError(try ScreenshotOutput.validatedRelativePath("", scratch: root))
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath(
                "screenshots/\(String(repeating: "a", count: 250)).png",
                scratch: root
            )
        )
    }

    func testScreenshotSpecOutputDefaultsAndOverrides() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let defaults = try ScreenshotSpec.parse([:])
        XCTAssertTrue(defaults.output.hasPrefix("screenshots/computer-"))
        let overridden = try ScreenshotSpec.parse(["output": "screenshots/custom.png"])
        XCTAssertEqual(overridden.output, "screenshots/custom.png")
        let absolute = try ScreenshotSpec.parse(["output": "/tmp/absolute.png"])
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath(absolute.output, scratch: root)
        )
    }

    func testScreenshotSpecRejectsTooLargeWindowId() {
        XCTAssertThrowsError(
            try ScreenshotSpec.parse(["window_id": Int(UInt32.max) + 1])
        )
        XCTAssertThrowsError(
            try ScreenshotSpec.parse(["window_id": 0])
        )
    }

    func testScreenshotSpecFitsLargeDisplays() throws {
        let spec = try ScreenshotSpec.parse([:])
        let fitted = ScreenshotSpec.fittedSize(
            width: 6016,
            height: 3389,
            maxWidth: spec.maxWidth,
            maxHeight: spec.maxHeight
        )
        XCTAssertLessThanOrEqual(fitted.width, spec.maxWidth)
        XCTAssertLessThanOrEqual(fitted.height, spec.maxHeight)
        XCTAssertGreaterThan(fitted.width, 0)
        XCTAssertGreaterThan(fitted.height, 0)
    }

    func testScreenshotMetadataCodableUsesSnakeCaseWindowID() throws {
        let metadata = ScreenshotMetadata(
            path: "/tmp/a.png",
            width: 10,
            height: 20,
            bytes: 30,
            sha256: "abc",
            target: "window",
            windowID: 42
        )
        let text = try String(decoding: JSONEncoder().encode(metadata), as: UTF8.self)
        XCTAssertTrue(text.contains("\"window_id\""))
        let decoded = try JSONDecoder().decode(
            ScreenshotMetadata.self,
            from: JSONEncoder().encode(metadata)
        )
        XCTAssertEqual(decoded, metadata)
    }

    func testScreenshotTargetScreenDoesNotRequireWindowId() throws {
        let spec = try ScreenshotSpec.parse(["target": "screen"])
        XCTAssertEqual(spec.target, .screen)
        XCTAssertNil(spec.windowID)
    }

    func testFittedSizeNeverExceedsRequestedBounds() {
        for (width, height) in [(100, 100), (4000, 2000), (1, 10000), (10000, 1)] {
            let fitted = ScreenshotSpec.fittedSize(
                width: width,
                height: height,
                maxWidth: 640,
                maxHeight: 480
            )
            XCTAssertLessThanOrEqual(fitted.width, 640)
            XCTAssertLessThanOrEqual(fitted.height, 480)
            XCTAssertGreaterThan(fitted.width, 0)
            XCTAssertGreaterThan(fitted.height, 0)
        }
    }

    func testOutputPathAllowsNestedRunDirectories() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let nested = try ScreenshotOutput.validatedRelativePath(
            "screenshots/run-42/window.png",
            scratch: root
        )
        XCTAssertTrue(nested.path.hasSuffix("screenshots/run-42/window.png"))
    }

    func testDefaultOutputNameIsAConcretePNGPath() {
        let output = ScreenshotOutput.defaultRelativePath()
        XCTAssertEqual(output.split(separator: "/").count, 2)
        XCTAssertEqual(output.split(separator: "/").first, "screenshots")
        XCTAssertFalse(output.contains(" "))
        XCTAssertFalse(output.lowercased().contains("base64"))
    }

    func testScreenshotPermissionIsSeparateFromAccessibility() {
        XCTAssertTrue(ComputerAction.screenshot.requiresScreenRecording)
        XCTAssertFalse(ComputerAction.screenshot.requiresAccessibility)
        XCTAssertFalse(ComputerAction.permissions.requiresScreenRecording)
    }
}
