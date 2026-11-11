import CoreGraphics
import Foundation
import XCTest

final class ComputerCoreEdgeTests: XCTestCase {
    func testAllProtocolActionsRoundTrip() throws {
        for action in ComputerAction.allCases {
            let parsed = try ComputerAction(raw: action.rawValue.uppercased())
            XCTAssertEqual(parsed, action)
        }
    }

    func testRequestIdAndParamsEdgeCases() throws {
        let request = try ComputerRequest.parse(
            data: Data(#"{"id":"abc","action":"apps","params":{"include_background":true}}"#.utf8)
        )
        XCTAssertEqual(request.params["include_background"] as? Bool, true)
        let noParams = try ComputerRequest.parse(
            data: Data(#"{"id":"abc","action":"apps","params":null}"#.utf8)
        )
        XCTAssertTrue(noParams.params.isEmpty)
        let longId = String(repeating: "x", count: 65)
        XCTAssertThrowsError(
            try ComputerRequest.parse(
                data: Data(#"{"id":"\#(longId)","action":"apps"}"#.utf8)
            )
        )
    }

    func testResponseErrorCapsCodeAndMessage() {
        let error = ResponseEnvelope.error(
            id: "1",
            code: String(repeating: "c", count: 200),
            message: String(repeating: "m", count: 10000)
        )
        let payload = error["error"] as? [String: Any]
        XCTAssertEqual((payload?["code"] as? String)?.count, 64)
        XCTAssertEqual((payload?["message"] as? String)?.count, 4000)
    }

    func testSanitizerHandlesArraysNullsAndNonFiniteNumbers() {
        let value: [String: Any] = [
            "array": [1, 2, 3],
            "null": NSNull(),
            "infinity": Double.infinity,
        ]
        let sanitized = JSONSanitizer.sanitize(value) as? [String: Any]
        XCTAssertEqual((sanitized?["array"] as? [Any])?.count, 3)
        XCTAssertTrue(sanitized?["null"] is NSNull)
        XCTAssertNotNil(sanitized?["infinity"] as? String)
    }

    func testSanitizerCapsLargeArrays() {
        let array = Array(0 ..< 5000)
        let sanitized = JSONSanitizer.sanitize(["items": array]) as? [String: Any]
        XCTAssertEqual((sanitized?["items"] as? [Any])?.count, 2000)
    }

    func testComputerRectHelpers() {
        let rect = ComputerRect(x: 10, y: 20, width: 100, height: 50)
        XCTAssertTrue(rect.isFinite)
        XCTAssertTrue(rect.isUsable)
        XCTAssertTrue(rect.contains(CGPoint(x: 10, y: 20)))
        XCTAssertTrue(rect.contains(CGPoint(x: 110, y: 70)))
        XCTAssertFalse(rect.contains(CGPoint(x: 111, y: 70)))
        XCTAssertFalse(ComputerRect(x: 0, y: 0, width: 0, height: 10).isUsable)
        XCTAssertFalse(ComputerRect(x: .nan, y: 0, width: 1, height: 1).isFinite)
    }

    func testNormalizedCornersResolveExactly() throws {
        let rect = ComputerRect(x: -100, y: 50, width: 200, height: 100)
        XCTAssertEqual(try NormalizedPoint(x: 0, y: 0).resolve(in: rect), CGPoint(x: -100, y: 50))
        XCTAssertEqual(try NormalizedPoint(x: 1, y: 1).resolve(in: rect), CGPoint(x: 100, y: 150))
        XCTAssertThrowsError(try NormalizedPoint(x: 1.0001, y: 0.5))
    }

    func testUnknownModifiersAndAllAliases() throws {
        XCTAssertEqual(try Modifier.parse("cmd"), .command)
        XCTAssertEqual(try Modifier.parse("COMMAND"), .command)
        XCTAssertEqual(try Modifier.parse(" meta "), .command)
        XCTAssertEqual(try Modifier.parse("super"), .command)
        XCTAssertEqual(try Modifier.parse("alt"), .option)
        XCTAssertEqual(try Modifier.parse("opt"), .option)
        XCTAssertEqual(try Modifier.parse("ctrl"), .control)
        XCTAssertEqual(try Modifier.parse("shift"), .shift)
        XCTAssertThrowsError(try Modifier.parse("fn"))
    }

    func testKeyFlagsCombine() throws {
        let spec = try KeySpec(key: "a", modifiers: ["cmd", "shift", "alt", "ctrl"])
        XCTAssertTrue(spec.eventFlags.contains(.maskCommand))
        XCTAssertTrue(spec.eventFlags.contains(.maskShift))
        XCTAssertTrue(spec.eventFlags.contains(.maskAlternate))
        XCTAssertTrue(spec.eventFlags.contains(.maskControl))
        XCTAssertEqual(spec.modifiers.count, 4)
    }

    func testKeyNamesCoverNavigationAndFunctionKeys() throws {
        for key in [
            "Return",
            "Enter",
            "Tab",
            "Space",
            "Delete",
            "ForwardDelete",
            "Escape",
            "Left",
            "Right",
            "Up",
            "Down",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "F1",
            "F12",
            "0",
            "9",
            "z",
            "/",
            "`",
        ] {
            XCTAssertNoThrow(try KeySpec(key: key, modifiers: []), key)
        }
        XCTAssertThrowsError(try KeySpec(key: "MediaPlayPause", modifiers: []))
    }

    func testDestructiveShortcutVariants() throws {
        XCTAssertTrue(try KeySpec(key: "q", modifiers: ["command"]).isDestructive)
        XCTAssertTrue(try KeySpec(key: "Q", modifiers: ["cmd"]).isDestructive)
        XCTAssertTrue(try KeySpec(key: "backspace", modifiers: ["cmd", "shift"]).isDestructive)
        XCTAssertTrue(try KeySpec(key: "forwarddelete", modifiers: ["cmd", "shift"]).isDestructive)
        XCTAssertTrue(try KeySpec(key: "escape", modifiers: ["cmd", "alt"]).isDestructive)
        XCTAssertTrue(try KeySpec(key: "q", modifiers: ["ctrl", "cmd"]).isDestructive)
        XCTAssertFalse(try KeySpec(key: "a", modifiers: ["cmd"]).isDestructive)
        XCTAssertFalse(try KeySpec(key: "delete", modifiers: ["cmd"]).isDestructive)
    }

    func testClickSpecIncludesPidAndWindowTarget() throws {
        let spec = try ClickSpec.parse([
            "x": 0.2,
            "y": 0.8,
            "bundle_id": "com.example.App",
            "pid": 123,
            "window_id": 456,
        ])
        XCTAssertEqual(spec.pid, 123)
        XCTAssertEqual(spec.windowID, 456)
        XCTAssertEqual(spec.bundleID, "com.example.App")
    }

    func testClickSpecRejectsInvalidWindowId() {
        XCTAssertThrowsError(
            try ClickSpec.parse(["ref": "ax-0123456789abcdef", "window_id": 0])
        )
    }

    func testDragSpecIncludesTargetAndClamps() throws {
        let spec = try DragSpec.parse([
            "from_x": 0,
            "from_y": 0,
            "to_x": 1,
            "to_y": 1,
            "duration_ms": 9999,
            "steps": 999,
            "bundle_id": "com.example.App",
            "pid": 5,
            "window_id": 6,
        ])
        XCTAssertEqual(spec.durationMilliseconds, ComputerProtocol.maxDragDurationMilliseconds)
        XCTAssertEqual(spec.steps, ComputerProtocol.maxDragSteps)
        XCTAssertEqual(spec.pid, 5)
        XCTAssertEqual(spec.windowID, 6)
    }

    func testWaitConditionsCoverElementFields() throws {
        let role = try WaitSpec.parse(["condition": "element", "role": "AXButton"])
        XCTAssertEqual(role.condition, .element(role: "AXButton", title: nil, identifier: nil))
        let title = try WaitSpec.parse(["condition": "element", "title": "Save"])
        XCTAssertEqual(title.condition, .element(role: nil, title: "Save", identifier: nil))
        let identifier = try WaitSpec.parse(["condition": "element", "identifier": "save"])
        XCTAssertEqual(identifier.condition, .element(role: nil, title: nil, identifier: "save"))
        XCTAssertThrowsError(try WaitSpec.parse(["condition": "element", "role": ""]))
    }

    func testWaitTimeoutsClampAtBothEnds() throws {
        let high = try WaitSpec.parse([
            "condition": "window", "title": "x", "timeout_ms": 99999, "interval_ms": 99999,
        ])
        XCTAssertEqual(high.timeoutMilliseconds, ComputerProtocol.maxWaitMilliseconds)
        XCTAssertEqual(high.intervalMilliseconds, ComputerProtocol.maxWaitIntervalMilliseconds)
        let low = try WaitSpec.parse([
            "condition": "window", "title": "x", "timeout_ms": 1, "interval_ms": 1,
        ])
        XCTAssertEqual(low.timeoutMilliseconds, ComputerProtocol.minWaitMilliseconds)
        XCTAssertEqual(low.intervalMilliseconds, ComputerProtocol.minWaitIntervalMilliseconds)
    }

    func testFocusSpecPidOnlyAndWindowIdBounds() throws {
        let spec = try FocusSpec.parse(["pid": 42])
        XCTAssertEqual(spec.pid, 42)
        XCTAssertNil(spec.bundleID)
        XCTAssertThrowsError(try FocusSpec.parse(["pid": 42, "window_id": 0]))
        let full = try FocusSpec.parse(["pid": 42, "window_id": 7])
        XCTAssertEqual(full.windowID, 7)
    }

    func testScreenshotSpecDefaultsAndBounds() throws {
        let spec = try ScreenshotSpec.parse([:])
        XCTAssertEqual(spec.target, .window)
        XCTAssertNil(spec.windowID)
        XCTAssertEqual(spec.maxWidth, ComputerProtocol.defaultScreenshotWidth)
        XCTAssertEqual(spec.maxHeight, ComputerProtocol.defaultScreenshotHeight)
        XCTAssertTrue(spec.output.hasPrefix("screenshots/"))
        XCTAssertTrue(spec.output.hasSuffix(".png"))
        XCTAssertThrowsError(try ScreenshotSpec.parse(["window_id": 0]))
    }

    func testScreenshotOutputRejectsSymlinkEscape() throws {
        let root = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("vp-computer-symlink-\(UUID().uuidString)", isDirectory: true)
        let outside = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("vp-computer-outside-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        try FileManager.default.createDirectory(at: outside, withIntermediateDirectories: true)
        defer {
            try? FileManager.default.removeItem(at: root)
            try? FileManager.default.removeItem(at: outside)
        }
        try FileManager.default.createSymbolicLink(
            at: root.appendingPathComponent("screenshots"),
            withDestinationURL: outside
        )
        XCTAssertThrowsError(
            try ScreenshotOutput.validatedRelativePath("screenshots/a.png", scratch: root)
        )
    }

    func testSnapshotDictionaryContainsExpectedFieldsAndTruncation() {
        let node = SnapshotNode(
            ref: "ax-0123456789abcdef",
            role: "AXButton",
            subrole: "AXButtonSubrole",
            identifier: "save",
            title: "Save",
            label: "Save document",
            value: "Ready",
            enabled: true,
            focused: false,
            selected: true,
            frame: ComputerRect(x: 1, y: 2, width: 3, height: 4),
            children: [],
            truncated: true
        )
        let dictionary = node.dictionary()
        XCTAssertEqual(dictionary["ref"] as? String, "ax-0123456789abcdef")
        XCTAssertEqual(dictionary["role"] as? String, "AXButton")
        XCTAssertEqual(dictionary["enabled"] as? Bool, true)
        XCTAssertEqual(dictionary["focused"] as? Bool, false)
        XCTAssertEqual(dictionary["truncated"] as? Bool, true)
        let frame = dictionary["frame"] as? [String: Double]
        XCTAssertEqual(frame?["width"], 3)
    }

    func testSnapshotBudgetByteLimitTruncates() {
        let limits = SnapshotLimits(maxDepth: 10, maxElements: 100, maxBytes: 1024)
        let budget = SnapshotBudget(limits: limits)
        let node = SnapshotNode(
            ref: nil,
            role: "AXWindow",
            title: String(repeating: "t", count: 600),
            children: [
                SnapshotNode(ref: nil, role: "AXButton", title: String(repeating: "b", count: 600)),
            ]
        )
        let bounded = SnapshotBudget.bounded(node, depth: 0, budget: budget)
        XCTAssertNotNil(bounded)
        XCTAssertTrue(bounded?.truncated ?? false)
        XCTAssertLessThanOrEqual(budget.estimatedBytes, limits.maxBytes)
    }

    func testSecureFieldPolicyRecognizesBothRoleAndSubrole() {
        XCTAssertTrue(
            SensitiveValuePolicy.isSecureField(role: "AXSecureTextField", subrole: nil)
        )
        XCTAssertTrue(
            SensitiveValuePolicy.isSecureField(role: "AXTextField", subrole: "AXSecureTextArea")
        )
        XCTAssertFalse(SensitiveValuePolicy.isSecureField(role: "AXTextField", subrole: "SearchField"))
        XCTAssertEqual(
            SensitiveValuePolicy.redactedValue(
                role: "AXSecureTextArea",
                subrole: nil,
                raw: "secret"
            ),
            "***"
        )
    }

    func testElementRefsChangeByRoleAndIdentifier() {
        let base = ElementRef.make(pid: 1, windowIndex: 0, path: ["0:AXButton:save"])
        let role = ElementRef.make(pid: 1, windowIndex: 0, path: ["0:AXTextField:save"])
        let identifier = ElementRef.make(pid: 1, windowIndex: 0, path: ["0:AXButton:cancel"])
        let pid = ElementRef.make(pid: 2, windowIndex: 0, path: ["0:AXButton:save"])
        XCTAssertNotEqual(base, role)
        XCTAssertNotEqual(base, identifier)
        XCTAssertNotEqual(base, pid)
    }

    func testAppAndWindowCodableRoundTrip() throws {
        let app = ComputerApp(
            bundleID: nil,
            name: "No Bundle",
            pid: 3,
            frontmost: false,
            hidden: true,
            activationPolicy: "accessory"
        )
        let decoded = try JSONDecoder().decode(
            ComputerApp.self,
            from: JSONEncoder().encode(app)
        )
        XCTAssertEqual(decoded, app)
        let window = ComputerWindow(
            id: UInt32.max,
            pid: 3,
            app: "No Bundle",
            title: "Tiny",
            bounds: ComputerRect(x: 0, y: 0, width: 1, height: 1),
            layer: -5,
            focused: false,
            onScreen: false
        )
        let decodedWindow = try JSONDecoder().decode(
            ComputerWindow.self,
            from: JSONEncoder().encode(window)
        )
        XCTAssertEqual(decodedWindow, window)
    }

    func testPermissionsCodableOmitsNothingButUsesSnakeCase() throws {
        let permissions = ComputerPermissions(
            accessibility: false,
            screenRecording: true,
            detail: nil
        )
        let text = try String(decoding: JSONEncoder().encode(permissions), as: UTF8.self)
        XCTAssertTrue(text.contains("\"screen_recording\""))
        XCTAssertTrue(text.contains("\"accessibility\""))
    }

    func testParameterReaderBoundsArraysAndItemSizes() throws {
        XCTAssertThrowsError(
            try ParamReader.optionalStringArray(
                ["modifiers": [String(repeating: "x", count: 40)]],
                "modifiers",
                maxItems: 1,
                maxItemBytes: 32
            )
        )
        XCTAssertThrowsError(
            try ParamReader.optionalStringArray(
                ["modifiers": ["a", "b"]],
                "modifiers",
                maxItems: 1
            )
        )
        XCTAssertThrowsError(
            try ParamReader.optionalStringArray(
                ["modifiers": [1]],
                "modifiers",
                maxItems: 1
            )
        )
    }

    func testScreenGuardRejectsPointsOutsideAllDisplays() throws {
        let screens = [
            ComputerRect(x: 0, y: 0, width: 1440, height: 900),
            ComputerRect(x: 1440, y: 0, width: 1920, height: 1080),
        ]
        XCTAssertTrue(ScreenGuard.isOnScreen(CGPoint(x: 10, y: 10), screens: screens))
        XCTAssertTrue(ScreenGuard.isOnScreen(CGPoint(x: 2000, y: 500), screens: screens))
        XCTAssertFalse(ScreenGuard.isOnScreen(CGPoint(x: -5, y: 10), screens: screens))
        XCTAssertFalse(ScreenGuard.isOnScreen(CGPoint(x: 4000, y: 10), screens: screens))
        XCTAssertFalse(ScreenGuard.isOnScreen(CGPoint(x: CGFloat.nan, y: 10), screens: screens))
        XCTAssertThrowsError(
            try ScreenGuard.requireOnScreen(CGPoint(x: 9999, y: 9), screens: screens)
        )
        XCTAssertNoThrow(
            try ScreenGuard.requireOnScreen(CGPoint(x: 9, y: 9), screens: screens)
        )
    }

    func testSnapshotBudgetAdmitsRespectsBytesExactly() {
        let limits = SnapshotLimits(maxDepth: 2, maxElements: 3, maxBytes: 1024)
        let budget = SnapshotBudget(limits: limits)
        XCTAssertTrue(budget.admit(depth: 0, bytes: 400))
        XCTAssertTrue(budget.admit(depth: 1, bytes: 400))
        XCTAssertFalse(budget.admit(depth: 1, bytes: 300))
        XCTAssertTrue(budget.truncated)
        XCTAssertEqual(budget.elements, 2)
        XCTAssertEqual(budget.estimatedBytes, 800)
    }

    func testSnapshotNodeEstimatedBytesTracksFields() {
        let bare = SnapshotNode(ref: nil, role: "AXButton")
        let full = SnapshotNode(
            ref: "ax-0123456789abcdef",
            role: "AXTextField",
            subrole: "SearchField",
            identifier: "search",
            title: "Search",
            label: "Search field",
            value: "term"
        )
        XCTAssertGreaterThan(full.estimatedBytes, bare.estimatedBytes)
        XCTAssertGreaterThanOrEqual(bare.estimatedBytes, 96)
    }

    func testScreenshotFittingHandlesDegenerateSizes() {
        XCTAssertEqual(
            ScreenshotSpec.fittedSize(width: 0, height: 0, maxWidth: 100, maxHeight: 100).width,
            1
        )
        XCTAssertEqual(
            ScreenshotSpec.fittedSize(width: 10000, height: 10000, maxWidth: 320, maxHeight: 320).width,
            320
        )
        XCTAssertEqual(
            ScreenshotSpec.fittedSize(width: 100, height: 10000, maxWidth: 320, maxHeight: 320).height,
            320
        )
    }

    func testSnapshotLimitsDefaultsAreSafe() {
        let limits = SnapshotLimits.default
        XCTAssertEqual(limits.maxDepth, ComputerProtocol.defaultSnapshotDepth)
        XCTAssertEqual(limits.maxElements, ComputerProtocol.defaultSnapshotElements)
        XCTAssertEqual(limits.maxBytes, ComputerProtocol.defaultSnapshotBytes)
        XCTAssertLessThanOrEqual(limits.maxElements, ComputerProtocol.maxSnapshotElements)
    }
}
