import CoreGraphics
import Foundation
import XCTest

final class ComputerCoreTests: XCTestCase {
    func testParsesAWellFormedRequest() throws {
        let data = Data(
            #"{"id":"7","action":"Snapshot","params":{"max_depth":4}}"#.utf8
        )
        let request = try ComputerRequest.parse(data: data)
        XCTAssertEqual(request.id, "7")
        XCTAssertEqual(request.action, .snapshot)
        XCTAssertEqual(request.params["max_depth"] as? Int, 4)
    }

    func testParsesARequestWithoutParams() throws {
        let request = try ComputerRequest.parse(
            data: Data(#"{"id":"a","action":"permissions"}"#.utf8)
        )
        XCTAssertEqual(request.action, .permissions)
        XCTAssertTrue(request.params.isEmpty)
    }

    func testRejectsMalformedRequests() {
        XCTAssertThrowsError(try ComputerRequest.parse(data: Data("[]".utf8)))
        XCTAssertThrowsError(try ComputerRequest.parse(data: Data(#"{"id":"1"}"#.utf8)))
        XCTAssertThrowsError(
            try ComputerRequest.parse(data: Data(#"{"id":"1","action":"explode"}"#.utf8))
        )
        XCTAssertThrowsError(
            try ComputerRequest.parse(data: Data(#"{"id":"1","action":"click","params":[]}"#.utf8))
        )
    }

    func testRejectsAnOversizedRequest() {
        let padding = String(repeating: "a", count: ComputerProtocol.maxRequestBytes + 1)
        let data = Data(#"{"id":"1","action":"snapshot","params":{"x":"\#(padding)"}}"#.utf8)
        XCTAssertThrowsError(try ComputerRequest.parse(data: data)) { error in
            XCTAssertEqual((error as? ComputerFailure)?.code, "PROTOCOL_ERROR")
        }
    }

    func testResponseEnvelopesAreBoundedAndTagged() {
        let ok = ResponseEnvelope.ok(id: "3", result: ["value": 1])
        XCTAssertEqual(ok["id"] as? String, "3")
        XCTAssertEqual(ok["ok"] as? Bool, true)
        let error = ResponseEnvelope.error(id: "4", code: "X", message: "bad")
        XCTAssertEqual(error["ok"] as? Bool, false)
        XCTAssertEqual((error["error"] as? [String: Any])?["code"] as? String, "X")
        let ready = ResponseEnvelope.ready(
            pid: 12,
            scratch: "/tmp/scratch",
            permissions: ComputerPermissions(
                accessibility: false,
                screenRecording: true,
                detail: "missing"
            )
        )
        XCTAssertEqual(ready["event"] as? String, "ready")
        XCTAssertEqual(ready["version"] as? String, "1")
        XCTAssertEqual(ready["accessibility_trusted"] as? Bool, false)
        XCTAssertEqual(ready["screen_recording"] as? Bool, true)
    }

    func testSanitizerBoundsStringsAndDepth() {
        let long = String(repeating: "z", count: ComputerProtocol.maxSnapshotTextChars * 20)
        let sanitized = JSONSanitizer.sanitize(["text": long]) as? [String: Any]
        let text = sanitized?["text"] as? String ?? ""
        XCTAssertLessThanOrEqual(text.count, ComputerProtocol.maxSnapshotTextChars * 8)
        let nested = JSONSanitizer.sanitize(["a": ["b": ["c": ["d": ["e": ["f": 1]]]]]]) as? [String: Any]
        XCTAssertNotNil(nested?["a"])
    }

    func testElementRefsAreStableAndNotPointerShaped() {
        let first = ElementRef.make(pid: 42, windowIndex: 1, path: ["window:1", "0:AXButton:id"])
        let second = ElementRef.make(pid: 42, windowIndex: 1, path: ["window:1", "0:AXButton:id"])
        let other = ElementRef.make(pid: 42, windowIndex: 2, path: ["window:1", "0:AXButton:id"])
        XCTAssertEqual(first, second)
        XCTAssertNotEqual(first, other)
        XCTAssertTrue(ElementRef.isValid(first))
        XCTAssertFalse(ElementRef.isValid("e12"))
        XCTAssertFalse(ElementRef.isValid("ax-"))
        XCTAssertFalse(ElementRef.isValid("ax-zzzzzzzz"))
        XCTAssertFalse(ElementRef.isValid("ax-0123456"))
    }

    func testSecureFieldsAreRedactedBeforeBounding() {
        XCTAssertTrue(
            SensitiveValuePolicy.isSecureField(role: "AXTextField", subrole: "AXSecureTextField")
        )
        XCTAssertEqual(
            SensitiveValuePolicy.redactedValue(
                role: "AXTextField",
                subrole: "AXSecureTextField",
                raw: "hunter2"
            ),
            "***"
        )
        XCTAssertNil(
            SensitiveValuePolicy.redactedValue(role: "AXTextField", subrole: nil, raw: "")
        )
        let long = String(repeating: "a", count: 1000)
        let bounded = SensitiveValuePolicy.redactedValue(
            role: "AXStaticText",
            subrole: nil,
            raw: long
        )
        XCTAssertEqual(bounded?.count, ComputerProtocol.maxSnapshotValueChars)
    }

    func testSnapshotBudgetBoundsElementsAndDepth() {
        let leaf = SnapshotNode(ref: "ax-1111111111111111", role: "AXButton", title: "Go")
        let middle = SnapshotNode(
            ref: "ax-2222222222222222",
            role: "AXGroup",
            title: "Group",
            children: [leaf]
        )
        let root = SnapshotNode(
            ref: "ax-3333333333333333",
            role: "AXWindow",
            title: "Window",
            children: [middle]
        )
        let limits = SnapshotLimits(maxDepth: 1, maxElements: 3, maxBytes: 131_072)
        let budget = SnapshotBudget(limits: limits)
        let bounded = SnapshotBudget.bounded(root, depth: 0, budget: budget)
        XCTAssertNotNil(bounded)
        XCTAssertEqual(budget.elements, 2)
        XCTAssertTrue(bounded?.children.first?.children.isEmpty ?? false)
        XCTAssertTrue(bounded?.children.first?.truncated ?? false)
    }

    func testSnapshotBudgetRejectsBeyondTheElementCap() {
        let limits = SnapshotLimits(maxDepth: 10, maxElements: 1, maxBytes: 131_072)
        let budget = SnapshotBudget(limits: limits)
        let root = SnapshotNode(
            ref: nil,
            role: "AXWindow",
            title: "Window",
            children: [SnapshotNode(ref: nil, role: "AXButton", title: "Go")]
        )
        let bounded = SnapshotBudget.bounded(root, depth: 0, budget: budget)
        XCTAssertEqual(budget.elements, 1)
        XCTAssertTrue(bounded?.truncated ?? false)
    }

    func testSnapshotLimitsClampToHardBounds() {
        let limits = SnapshotLimits(maxDepth: 1000, maxElements: 1_000_000, maxBytes: 1_000_000)
        XCTAssertEqual(limits.maxDepth, ComputerProtocol.maxSnapshotDepth)
        XCTAssertEqual(limits.maxElements, ComputerProtocol.maxSnapshotElements)
        XCTAssertEqual(limits.maxBytes, ComputerProtocol.maxSnapshotBytes)
        let tooSmall = SnapshotLimits(maxDepth: 0, maxElements: 0, maxBytes: 0)
        XCTAssertEqual(tooSmall.maxDepth, 1)
        XCTAssertEqual(tooSmall.maxElements, 1)
        XCTAssertEqual(tooSmall.maxBytes, 1024)
    }

    func testNormalizedPointValidationAndResolution() throws {
        let point = try NormalizedPoint(x: 0.5, y: 0.5)
        let rect = ComputerRect(x: 100, y: 200, width: 400, height: 200)
        XCTAssertEqual(try point.resolve(in: rect), CGPoint(x: 300, y: 300))
        XCTAssertThrowsError(try NormalizedPoint(x: -0.1, y: 0.5))
        XCTAssertThrowsError(try NormalizedPoint(x: 0.5, y: 1.1))
        XCTAssertThrowsError(try NormalizedPoint(x: .nan, y: 0.5))
        XCTAssertThrowsError(try NormalizedPoint(x: .infinity, y: 0.5))
        XCTAssertThrowsError(try point.resolve(in: ComputerRect(x: 0, y: 0, width: 0, height: 10)))
    }

    func testClickSpecRequiresExactlyOneTarget() throws {
        let byRef = try ClickSpec.parse(["ref": "ax-0123456789abcdef"])
        XCTAssertEqual(byRef.ref, "ax-0123456789abcdef")
        XCTAssertNil(byRef.point)
        XCTAssertEqual(byRef.button, "left")
        XCTAssertEqual(byRef.clickCount, 1)

        let byPoint = try ClickSpec.parse(["x": 0.25, "y": 0.75, "button": "right", "click_count": 2])
        XCTAssertEqual(byPoint.point?.x, 0.25)
        XCTAssertEqual(byPoint.point?.y, 0.75)
        XCTAssertEqual(byPoint.button, "right")
        XCTAssertEqual(byPoint.clickCount, 2)

        XCTAssertThrowsError(try ClickSpec.parse([:]))
        XCTAssertThrowsError(
            try ClickSpec.parse(["ref": "ax-0123456789abcdef", "x": 0.1, "y": 0.2])
        )
        XCTAssertThrowsError(try ClickSpec.parse(["x": 0.1]))
        XCTAssertThrowsError(
            try ClickSpec.parse(["ref": "ax-0123456789abcdef", "button": "middle"])
        )
        let clamped = try ClickSpec.parse(["ref": "ax-0123456789abcdef", "click_count": 9])
        XCTAssertEqual(clamped.clickCount, ComputerProtocol.maxClickCount)
    }

    func testClickSpecAcceptsAWindowTarget() throws {
        let spec = try ClickSpec.parse([
            "x": 0.1,
            "y": 0.2,
            "bundle_id": "com.example.App",
            "window_id": 44,
        ])
        XCTAssertEqual(spec.bundleID, "com.example.App")
        XCTAssertEqual(spec.windowID, 44)
    }

    func testTypeSpecBoundsTextAndRejectsNul() throws {
        let spec = try TypeSpec.parse(["text": "hello", "ref": "ax-0123456789abcdef"])
        XCTAssertEqual(spec.text, "hello")
        XCTAssertEqual(spec.ref, "ax-0123456789abcdef")
        XCTAssertThrowsError(try TypeSpec.parse([:]))
        XCTAssertThrowsError(try TypeSpec.parse(["text": "bad\u{0}"]))
        XCTAssertThrowsError(
            try TypeSpec.parse([
                "text": String(repeating: "a", count: ComputerProtocol.maxTypeBytes + 1),
            ])
        )
    }

    func testKeySpecWhitelistAndDestructiveCombinations() throws {
        let escape = try KeySpec(key: "Escape", modifiers: [])
        XCTAssertEqual(escape.keyCode, 53)
        XCTAssertFalse(escape.isDestructive)

        let quit = try KeySpec(key: "q", modifiers: ["cmd"])
        XCTAssertTrue(quit.isDestructive)
        XCTAssertTrue(quit.eventFlags.contains(.maskCommand))

        let emptyTrash = try KeySpec(key: "delete", modifiers: ["command", "shift"])
        XCTAssertTrue(emptyTrash.isDestructive)

        let forceQuit = try KeySpec(key: "escape", modifiers: ["cmd", "option"])
        XCTAssertTrue(forceQuit.isDestructive)

        let lock = try KeySpec(key: "q", modifiers: ["ctrl", "cmd"])
        XCTAssertTrue(lock.isDestructive)

        let plainQ = try KeySpec(key: "q", modifiers: [])
        XCTAssertFalse(plainQ.isDestructive)

        XCTAssertThrowsError(try KeySpec(key: "volumeup", modifiers: []))
        XCTAssertThrowsError(try KeySpec(key: "", modifiers: []))
        XCTAssertThrowsError(try KeySpec(key: "q", modifiers: ["hyper"]))
        XCTAssertThrowsError(
            try KeySpec(key: "q", modifiers: ["cmd", "shift", "option", "control", "fn"])
        )
    }

    func testScrollSpecBoundsAndTargets() throws {
        let spec = try ScrollSpec.parse(["dy": 5, "unit": "pixel"])
        XCTAssertEqual(spec.dy, 5)
        XCTAssertEqual(spec.unit, "pixel")
        XCTAssertThrowsError(try ScrollSpec.parse([:]))
        XCTAssertThrowsError(try ScrollSpec.parse(["dx": 0, "dy": 0]))
        XCTAssertThrowsError(
            try ScrollSpec.parse(["dy": ComputerProtocol.maxScrollDelta + 1])
        )
        XCTAssertThrowsError(try ScrollSpec.parse(["dy": 1, "unit": "page"]))
    }

    func testDragSpecValidatesCornersAndDurations() throws {
        let spec = try DragSpec.parse([
            "from_x": 0.1,
            "from_y": 0.2,
            "to_x": 0.8,
            "to_y": 0.9,
            "duration_ms": 500,
            "steps": 20,
        ])
        XCTAssertEqual(spec.from.x, 0.1)
        XCTAssertEqual(spec.to.y, 0.9)
        XCTAssertEqual(spec.durationMilliseconds, 500)
        XCTAssertEqual(spec.steps, 20)
        XCTAssertThrowsError(try DragSpec.parse(["from_x": 0.1, "from_y": 0.2]))
        XCTAssertThrowsError(
            try DragSpec.parse([
                "from_x": 0.1, "from_y": 0.2, "to_x": 1.5, "to_y": 0.9,
            ])
        )
    }

    func testWaitSpecConditionsAndTimeouts() throws {
        let ref = try WaitSpec.parse(["condition": "ref", "ref": "ax-0123456789abcdef"])
        XCTAssertEqual(ref.condition, .ref("ax-0123456789abcdef"))
        XCTAssertEqual(ref.timeoutMilliseconds, ComputerProtocol.defaultWaitMilliseconds)

        let element = try WaitSpec.parse([
            "condition": "element", "role": "AXButton", "timeout_ms": 10,
        ])
        XCTAssertEqual(element.timeoutMilliseconds, ComputerProtocol.minWaitMilliseconds)

        let window = try WaitSpec.parse(["condition": "window", "title": "Save"])
        XCTAssertEqual(window.condition, .window(title: "Save"))

        let front = try WaitSpec.parse([
            "condition": "app_frontmost", "bundle_id": "com.example.App",
        ])
        XCTAssertEqual(front.condition, .appFrontmost(bundleID: "com.example.App", pid: nil))

        XCTAssertThrowsError(try WaitSpec.parse(["condition": "element"]))
        XCTAssertThrowsError(try WaitSpec.parse(["condition": "window"]))
        XCTAssertThrowsError(try WaitSpec.parse(["condition": "app_frontmost"]))
        XCTAssertThrowsError(try WaitSpec.parse(["condition": "ref", "ref": "nope"]))
    }

    func testFocusAndScreenshotSpecs() throws {
        let focus = try FocusSpec.parse([
            "bundle_id": "com.example.App", "window_title": "Main",
        ])
        XCTAssertEqual(focus.bundleID, "com.example.App")
        XCTAssertEqual(focus.windowTitle, "Main")
        XCTAssertThrowsError(try FocusSpec.parse(["window_title": "Main"]))

        let screenshot = try ScreenshotSpec.parse([
            "target": "screen", "max_width": 10000, "max_height": 10,
        ])
        XCTAssertEqual(screenshot.target, .screen)
        XCTAssertEqual(screenshot.maxWidth, ComputerProtocol.maxScreenshotDimension)
        XCTAssertEqual(screenshot.maxHeight, ComputerProtocol.minScreenshotDimension)
        XCTAssertThrowsError(try ScreenshotSpec.parse(["target": "desktop"]))
    }

    func testScreenshotFittingPreservesAspectRatio() {
        let fitted = ScreenshotSpec.fittedSize(
            width: 3840,
            height: 2160,
            maxWidth: 1600,
            maxHeight: 1200
        )
        XCTAssertEqual(fitted.width, 1600)
        XCTAssertEqual(fitted.height, 900)
        let tall = ScreenshotSpec.fittedSize(
            width: 1000,
            height: 4000,
            maxWidth: 1600,
            maxHeight: 1200
        )
        XCTAssertEqual(tall.width, 300)
        XCTAssertEqual(tall.height, 1200)
        let unchanged = ScreenshotSpec.fittedSize(
            width: 100,
            height: 100,
            maxWidth: 1600,
            maxHeight: 1200
        )
        XCTAssertEqual(unchanged.width, 100)
        XCTAssertEqual(unchanged.height, 100)
    }

    func testScreenshotOutputPathIsConfinedToScratch() throws {
        let scratch = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("vp-computer-tests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: scratch) }
        let output = try ScreenshotOutput.validatedRelativePath(
            "screenshots/test.png",
            scratch: scratch
        )
        XCTAssertTrue(output.path.hasPrefix(scratch.path))
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("/tmp/test.png", scratch: scratch)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("../test.png", scratch: scratch)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots/../test.png", scratch: scratch)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots/test.jpg", scratch: scratch)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("other/test.png", scratch: scratch)
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots/a b.png", scratch: scratch)
        )
    }

    func testApplicationAndWindowEncodingUsesSnakeCaseKeys() throws {
        let app = ComputerApp(
            bundleID: "com.example.App",
            name: "Example",
            pid: 55,
            frontmost: true,
            hidden: false,
            activationPolicy: "regular"
        )
        let data = try JSONEncoder().encode(app)
        let text = String(decoding: data, as: UTF8.self)
        XCTAssertTrue(text.contains("\"bundle_id\""))
        XCTAssertTrue(text.contains("\"activation_policy\""))

        let window = ComputerWindow(
            id: 9,
            pid: 55,
            app: "Example",
            title: "Main",
            bounds: ComputerRect(x: 1, y: 2, width: 3, height: 4),
            layer: 0,
            focused: true,
            onScreen: true
        )
        let windowText = try String(decoding: JSONEncoder().encode(window), as: UTF8.self)
        XCTAssertTrue(windowText.contains("\"on_screen\""))
    }

    func testParamReaderRejectsWrongTypesAndBoundsStrings() throws {
        XCTAssertEqual(try ParamReader.requiredString(["a": "x"], "a"), "x")
        XCTAssertThrowsError(try ParamReader.requiredString(["a": "  "], "a"))
        XCTAssertThrowsError(try ParamReader.requiredString(["a": 1], "a"))
        XCTAssertThrowsError(
            try ParamReader.optionalString(["a": String(repeating: "x", count: 10)], "a", max: 5)
        )
        XCTAssertEqual(try ParamReader.optionalInt(["a": 4], "a"), 4)
        XCTAssertThrowsError(try ParamReader.optionalInt(["a": true], "a"))
        XCTAssertEqual(try ParamReader.optionalDouble(["a": 1.5], "a"), 1.5)
        XCTAssertEqual(try ParamReader.optionalBool(["a": true], "a"), true)
        XCTAssertThrowsError(try ParamReader.optionalBool(["a": 1], "a"))
        XCTAssertEqual(
            try ParamReader.optionalStringArray(["a": ["cmd", "shift"]], "a", maxItems: 2),
            ["cmd", "shift"]
        )
        XCTAssertThrowsError(
            try ParamReader.optionalStringArray(["a": ["cmd", "shift", "option"]], "a", maxItems: 2)
        )
    }

    func testActionClassification() throws {
        XCTAssertTrue(try ComputerAction(raw: "snapshot").isReadOnlyInspection)
        XCTAssertTrue(try ComputerAction(raw: "wait").isReadOnlyInspection)
        XCTAssertFalse(try ComputerAction(raw: "click").isReadOnlyInspection)
        XCTAssertFalse(try ComputerAction(raw: "key").isReadOnlyInspection)
        XCTAssertThrowsError(try ComputerAction(raw: "shell"))
    }
}
