import CoreGraphics
import Foundation
import XCTest

final class ComputerValidationTests: XCTestCase {
    func testOptionalNonEmptyStringTreatsWhitespaceAsAbsent() throws {
        XCTAssertNil(try ParamReader.optionalNonEmptyString(["a": "   "], "a"))
        XCTAssertEqual(try ParamReader.optionalNonEmptyString(["a": " x "], "a"), " x ")
        XCTAssertThrowsError(
            try ParamReader.optionalNonEmptyString(["a": 3], "a")
        )
    }

    func testFocusRejectsAnEmptyBundleInsteadOfFallingBackToFrontmost() {
        XCTAssertThrowsError(try FocusSpec.parse(["bundle_id": "  "]))
        XCTAssertThrowsError(try WaitSpec.parse([
            "condition": "app_frontmost", "bundle_id": "  ",
        ]))
    }

    func testClickAndDragTreatEmptyBundleAsUnspecified() throws {
        let click = try ClickSpec.parse(["x": 0.5, "y": 0.5, "bundle_id": ""])
        XCTAssertNil(click.bundleID)
        let drag = try DragSpec.parse([
            "from_x": 0.1,
            "from_y": 0.1,
            "to_x": 0.2,
            "to_y": 0.2,
            "bundle_id": " ",
        ])
        XCTAssertNil(drag.bundleID)
    }

    func testSnapshotLimitsParseAndClampFromParams() throws {
        let limits = try SnapshotLimits.parse([
            "max_depth": 0,
            "max_elements": 99999,
            "max_bytes": 0,
        ])
        XCTAssertEqual(limits.maxDepth, 1)
        XCTAssertEqual(limits.maxElements, ComputerProtocol.maxSnapshotElements)
        XCTAssertEqual(limits.maxBytes, 1024)
    }

    func testSnapshotNodeDictionaryNestsChildrenAndOmitsNilFields() {
        let child = SnapshotNode(
            ref: nil,
            role: "AXButton",
            title: "Go",
            children: []
        )
        let root = SnapshotNode(
            ref: "ax-0123456789abcdef",
            role: "AXWindow",
            title: "Main",
            children: [child]
        )
        let dictionary = root.dictionary()
        let children = dictionary["children"] as? [[String: Any]]
        XCTAssertEqual(children?.count, 1)
        XCTAssertEqual(children?[0]["title"] as? String, "Go")
        XCTAssertNil(children?[0]["ref"])
        XCTAssertNil(dictionary["value"])
        XCTAssertNil(dictionary["subrole"])
    }

    func testScreenshotDefaultNamesAreUniqueAndConfined() {
        let first = ScreenshotOutput.defaultRelativePath()
        let second = ScreenshotOutput.defaultRelativePath()
        XCTAssertNotEqual(first, second)
        XCTAssertTrue(first.hasPrefix("screenshots/computer-"))
        XCTAssertTrue(first.hasSuffix(".png"))
        XCTAssertFalse(first.contains(".."))
    }

    func testScreenshotPathRejectsDotComponentsAndUppercaseExtension() throws {
        let scratch = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("vp-validation-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: scratch) }
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots/./a.png", scratch: scratch)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots//a.png", scratch: scratch)
        )
        XCTAssertNoThrow(
            try ScreenshotOutput.validatedRelativePath("screenshots/a.PNG", scratch: scratch)
        )
        XCTAssertNoThrow(
            try ScreenshotOutput.validatedRelativePath("screenshots/..png", scratch: scratch)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots/../a.png", scratch: scratch)
        )
    }

    func testScreenshotTargetIsCaseInsensitive() throws {
        XCTAssertEqual(try ScreenshotSpec.parse(["target": "SCREEN"]).target, .screen)
        XCTAssertEqual(try ScreenshotSpec.parse(["target": "Window"]).target, .window)
    }

    func testWaitRefMustBeAValidHash() {
        XCTAssertThrowsError(try WaitSpec.parse(["condition": "ref", "ref": "ax-zz"]))
        XCTAssertThrowsError(try WaitSpec.parse(["condition": "ref", "ref": "e1"]))
        XCTAssertNoThrow(
            try WaitSpec.parse(["condition": "ref", "ref": "ax-0123456789abcdef"])
        )
    }

    func testComputerRequestRejectsNonObjectParams() {
        XCTAssertThrowsError(
            try ComputerRequest.parse(
                data: Data(#"{"id":"1","action":"apps","params":[]}"#.utf8)
            )
        )
        let noId = try? ComputerRequest.parse(data: Data(#"{"action":"apps"}"#.utf8))
        XCTAssertEqual(noId?.id, "?")
    }

    func testResponseEnvelopeSanitizesNestedResults() {
        let envelope = ResponseEnvelope.ok(
            id: "9",
            result: [
                "nested": ["text": String(repeating: "a", count: 10000)],
            ]
        )
        let result = envelope["result"] as? [String: Any]
        let nested = result?["nested"] as? [String: Any]
        let text = nested?["text"] as? String ?? ""
        XCTAssertLessThanOrEqual(text.count, ComputerProtocol.maxSnapshotTextChars * 8)
    }

    func testScreenGuardToleranceEdges() {
        let screen = ComputerRect(x: 0, y: 0, width: 100, height: 100)
        XCTAssertTrue(ScreenGuard.isOnScreen(CGPoint(x: 100.5, y: 50), screens: [screen]))
        XCTAssertFalse(ScreenGuard.isOnScreen(CGPoint(x: 101.5, y: 50), screens: [screen]))
        XCTAssertFalse(ScreenGuard.isOnScreen(CGPoint(x: 50, y: -2), screens: [screen]))
        XCTAssertTrue(ScreenGuard.isOnScreen(CGPoint(x: 50, y: -0.5), screens: [screen]))
    }

    func testComputerRectContainsRespectsTolerance() {
        let rect = ComputerRect(x: 10, y: 10, width: 10, height: 10)
        XCTAssertTrue(rect.contains(CGPoint(x: 9.8, y: 10), tolerance: 0.5))
        XCTAssertFalse(rect.contains(CGPoint(x: 9.0, y: 10), tolerance: 0.5))
    }

    func testKeySpecRejectsTooManyModifiersAfterDeduplication() {
        // Duplicates are still rejected by the raw array cap; the helper does
        // not silently accept an unbounded list from a direct client.
        XCTAssertThrowsError(
            try KeySpec(key: "a", modifiers: ["cmd", "cmd", "cmd", "cmd", "cmd"])
        )
    }

    func testClickButtonCaseAndClickCountClamp() throws {
        let spec = try ClickSpec.parse([
            "ref": "ax-0123456789abcdef",
            "button": "LEFT",
            "click_count": 99,
        ])
        XCTAssertEqual(spec.button, "left")
        XCTAssertEqual(spec.clickCount, 2)
    }

    func testScrollUnitCaseAndReferenceValidation() throws {
        XCTAssertEqual(try ScrollSpec.parse(["dy": 1, "unit": "PIXEL"]).unit, "pixel")
        XCTAssertThrowsError(
            try ScrollSpec.parse(["dy": 1, "ref": "not-a-ref"])
        )
    }

    func testDragCoordinatesAreAllRequired() {
        XCTAssertThrowsError(
            try DragSpec.parse([
                "from_x": 0.1, "from_y": 0.1, "to_x": 0.2,
            ])
        )
        XCTAssertThrowsError(
            try DragSpec.parse([
                "from_x": 0.1, "from_y": 0.1, "to_x": 0.2, "to_y": Double.nan,
            ])
        )
    }

    func testActionCapabilityMapIsExplicitAndDisjoint() {
        XCTAssertFalse(ComputerAction.permissions.requiresAccessibility)
        XCTAssertFalse(ComputerAction.close.requiresAccessibility)
        XCTAssertFalse(ComputerAction.screenshot.requiresAccessibility)
        XCTAssertTrue(ComputerAction.screenshot.requiresScreenRecording)
        XCTAssertFalse(ComputerAction.permissions.requiresScreenRecording)
        for action in ComputerAction.allCases where action != .screenshot {
            XCTAssertFalse(action.requiresScreenRecording, "\(action)")
        }
        for action in [
            ComputerAction.apps, .windows, .focus, .snapshot, .click, .type, .key, .scroll,
            .drag, .wait,
        ] {
            XCTAssertTrue(action.requiresAccessibility, "\(action)")
        }
        for action in [ComputerAction.focus, .click, .type, .key, .scroll, .drag] {
            XCTAssertTrue(action.mutatesHostUI, "\(action)")
        }
        for action in [
            ComputerAction.permissions, .apps, .windows, .snapshot, .screenshot, .wait, .close,
        ] {
            XCTAssertFalse(action.mutatesHostUI, "\(action)")
        }
    }

    func testScreenshotLimitsAndDefaultsAreAppliedTogether() throws {
        let defaults = try ScreenshotSpec.parse([:])
        XCTAssertEqual(defaults.maxWidth, ComputerProtocol.defaultScreenshotWidth)
        XCTAssertEqual(defaults.maxHeight, ComputerProtocol.defaultScreenshotHeight)
        let clamped = try ScreenshotSpec.parse([
            "max_width": 1,
            "max_height": 99999,
        ])
        XCTAssertEqual(clamped.maxWidth, ComputerProtocol.minScreenshotDimension)
        XCTAssertEqual(clamped.maxHeight, ComputerProtocol.maxScreenshotDimension)
    }

    func testOptionalStringArrayRejectsMixedTypesAndNullItems() {
        XCTAssertThrowsError(
            try ParamReader.optionalStringArray(
                ["items": ["ok", NSNull()]],
                "items",
                maxItems: 2
            )
        )
        XCTAssertThrowsError(
            try ParamReader.optionalStringArray(
                ["items": ["ok", 1]],
                "items",
                maxItems: 2
            )
        )
    }

    func testSnapshotBudgetDepthStopMarksNode() {
        let limits = SnapshotLimits(maxDepth: 1, maxElements: 100, maxBytes: 10000)
        let budget = SnapshotBudget(limits: limits)
        let leaf = SnapshotNode(ref: nil, role: "AXButton", title: "leaf")
        let root = SnapshotNode(ref: nil, role: "AXWindow", title: "root", children: [leaf])
        let bounded = SnapshotBudget.bounded(root, depth: 0, budget: budget)
        XCTAssertEqual(bounded?.children.count, 1)
        XCTAssertEqual(bounded?.children.first?.truncated, true)
    }
}
