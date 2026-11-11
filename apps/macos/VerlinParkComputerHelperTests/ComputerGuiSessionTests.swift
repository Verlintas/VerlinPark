import XCTest

final class ComputerGuiSessionTests: XCTestCase {
    func testElementReferencesCarryTheSnapshotGeneration() {
        let ref = ElementRef.make(
            pid: 42,
            windowIndex: 0,
            path: ["window:0", "0:AXButton:go"],
            generation: 7
        )
        XCTAssertTrue(ref.hasPrefix("ax-00000007-"))
        XCTAssertEqual(ElementRef.generation(from: ref), 7)
        XCTAssertTrue(ElementRef.isValid(ref))
        XCTAssertEqual(ref.count, 28)

        let legacy = "ax-0123456789abcdef"
        XCTAssertTrue(ElementRef.isValid(legacy))
        XCTAssertNil(ElementRef.generation(from: legacy))

        let unrelated = ElementRef.make(
            pid: 42,
            windowIndex: 0,
            path: ["window:0", "0:AXButton:go"],
            generation: 8
        )
        XCTAssertNotEqual(ref, unrelated)
        XCTAssertEqual(ElementRef.generation(from: unrelated), 8)

        XCTAssertFalse(ElementRef.isValid("ax-0000000-0123456789abcdef"))
        XCTAssertFalse(ElementRef.isValid("ax-00000007-0123456789abcdeg"))
        XCTAssertFalse(ElementRef.isValid("ax-00000007-01234567"))
        XCTAssertFalse(ElementRef.isValid("AX-00000007-0123456789abcdef"))
        XCTAssertFalse(ElementRef.isValid("ax-0123456"))
    }

    func testSnapshotGenerationCounterRejectsOlderRefs() {
        var counter = SnapshotGenerationCounter()
        let first = ElementRef.make(pid: 1, windowIndex: 0, path: ["a"], generation: 1)
        let second = ElementRef.make(pid: 1, windowIndex: 0, path: ["b"], generation: 1)
        XCTAssertEqual(counter.next(refs: [first]), 1)
        XCTAssertTrue(counter.isCurrent(first))
        XCTAssertFalse(counter.isCurrent(second))

        let next = ElementRef.make(pid: 1, windowIndex: 0, path: ["a"], generation: 2)
        XCTAssertEqual(counter.next(refs: [next]), 2)
        XCTAssertFalse(counter.isCurrent(first), "a ref from generation 1 is stale at generation 2")
        XCTAssertTrue(counter.isCurrent(next))
    }

    func testStaleRefFailureCarriesGenerationAndSnapshotHint() throws {
        let failure = ComputerFailure.staleRef(
            "ax-00000003-0123456789abcdef",
            generation: 7
        )
        XCTAssertEqual(failure.code, "STALE_REF")
        XCTAssertTrue(failure.message.contains("generation 7"))
        let details = try XCTUnwrap(failure.details)
        XCTAssertEqual(details["snapshot_generation"] as? UInt64, 7)
        XCTAssertEqual(details["ref_generation"] as? UInt64, 3)
        XCTAssertEqual(details["action"] as? String, "snapshot")
        XCTAssertEqual(details["auto_retry"] as? Bool, false)
        XCTAssertTrue((details["hint"] as? String)?.contains("fresh snapshot") == true)
    }

    func testSensitivePolicyMatchesBuiltInsAndProjectExtensions() {
        let deny = SensitiveTargetPolicy(mode: .deny)
        XCTAssertTrue(deny.matches(
            bundleID: "com.apple.systempreferences",
            app: "System Settings",
            title: "Privacy & Security"
        ))
        XCTAssertTrue(deny.matches(
            bundleID: "com.1password.1password",
            app: "1Password",
            title: "Vault"
        ))
        XCTAssertTrue(deny.matches(
            bundleID: nil,
            app: "System Settings",
            title: nil
        ))
        XCTAssertFalse(deny.matches(
            bundleID: "com.apple.Safari",
            app: "Safari",
            title: "Example"
        ))
        let extended = SensitiveTargetPolicy(
            mode: .deny,
            bundles: [" COM.Example.Vault ", "com.example.vault", ""],
            titles: ["My Vault", ""]
        )
        XCTAssertEqual(extended.effectiveBundles.filter { $0 == "com.example.vault" }.count, 1)
        XCTAssertTrue(extended.matches(bundleID: "com.example.vault", app: nil, title: nil))
        XCTAssertTrue(extended.matches(bundleID: nil, app: "My Vault", title: nil))
    }

    func testSensitivePolicyDenialAndApprovalModes() {
        let target = ("com.apple.keychainaccess", "Keychain Access", "Passwords")
        let denied = SensitiveTargetPolicy(mode: .deny).refusal(
            bundleID: target.0,
            app: target.1,
            title: target.2
        )
        XCTAssertEqual(denied?.code, "SENSITIVE_TARGET")
        XCTAssertEqual(denied?.details?["requires_approval"] as? Bool, false)

        let needsApproval = SensitiveTargetPolicy(mode: .requireApproval).refusal(
            bundleID: target.0,
            app: target.1,
            title: target.2
        )
        XCTAssertEqual(needsApproval?.code, "SENSITIVE_TARGET")
        XCTAssertEqual(needsApproval?.details?["requires_approval"] as? Bool, true)
        XCTAssertTrue(
            (needsApproval?.details?["hint"] as? String)?.contains("bundle_id") == true
        )

        let acknowledged = SensitiveTargetPolicy(
            mode: .requireApproval,
            acknowledged: true
        ).refusal(bundleID: target.0, app: target.1, title: target.2)
        XCTAssertNil(acknowledged)

        let ordinary = SensitiveTargetPolicy(mode: .deny).refusal(
            bundleID: "com.example.App",
            app: "Example",
            title: "Main"
        )
        XCTAssertNil(ordinary)
    }

    func testSensitivePolicyParsesAndBoundsRequestParameters() throws {
        let params: [String: Any] = [
            "sensitive_mode": "require_approval",
            "sensitive_bundles": [" com.example.Vault ", "com.example.Vault"],
            "sensitive_titles": ["Vault"],
            "sensitive_ack": true,
        ]
        let policy = try SensitiveTargetPolicy.parse(params)
        XCTAssertEqual(policy.mode, .requireApproval)
        XCTAssertEqual(policy.bundles, ["com.example.vault"])
        XCTAssertEqual(policy.titles, ["vault"])
        XCTAssertTrue(policy.acknowledged)

        let missing = try SensitiveTargetPolicy.parse([:])
        XCTAssertEqual(missing.mode, .deny)
        XCTAssertFalse(missing.acknowledged)
        XCTAssertEqual(missing.effectiveBundles.count, SensitiveTargetPolicy.builtInBundles.count)

        let unknownMode = try SensitiveTargetPolicy.parse(["sensitive_mode": "future-mode"])
        XCTAssertEqual(unknownMode.mode, .deny, "unknown modes fail closed")

        var many: [String] = []
        for index in 0 ..< 65 {
            many.append("bundle-\(index)")
        }
        XCTAssertThrowsError(
            try SensitiveTargetPolicy.parse(["sensitive_bundles": many])
        )
    }

    func testResponseEnvelopeIncludesSanitizedErrorDetails() {
        let envelope = ResponseEnvelope.error(
            id: "7",
            code: "STALE_REF",
            message: "gone",
            details: [
                "snapshot_generation": 4,
                "hint": "take a fresh snapshot",
                "nested": ["screen": "must not leak bytes", "count": 2],
            ]
        )
        XCTAssertEqual(envelope["ok"] as? Bool, false)
        let error = envelope["error"] as? [String: Any]
        XCTAssertEqual(error?["code"] as? String, "STALE_REF")
        XCTAssertEqual(error?["message"] as? String, "gone")
        let details = error?["details"] as? [String: Any]
        XCTAssertEqual(details?["snapshot_generation"] as? Int, 4)
        XCTAssertEqual(details?["hint"] as? String, "take a fresh snapshot")
        let nested = details?["nested"] as? [String: Any]
        XCTAssertEqual(nested?["count"] as? Int, 2)
    }

    func testRequestParsingAcceptsSensitivePolicyFields() throws {
        let data = try JSONSerialization.data(withJSONObject: [
            "id": "1",
            "action": "click",
            "params": [
                "ref": "ax-00000007-0123456789abcdef",
                "sensitive_mode": "deny",
                "sensitive_bundles": ["com.apple.securityagent"],
                "sensitive_titles": ["System Settings"],
                "sensitive_ack": false,
            ],
        ])
        let request = try ComputerRequest.parse(data: data)
        XCTAssertEqual(request.action, .click)
        let policy = try SensitiveTargetPolicy.parse(request.params)
        XCTAssertEqual(policy.mode, .deny)
        XCTAssertFalse(policy.acknowledged)
        XCTAssertEqual(ElementRef.generation(from: "ax-00000007-0123456789abcdef"), 7)
    }

    func testDisplaysActionIsInspectionOnly() throws {
        let action = try ComputerAction(raw: "displays")
        XCTAssertEqual(action, .displays)
        XCTAssertTrue(action.isReadOnlyInspection)
        XCTAssertFalse(action.requiresAccessibility)
        XCTAssertFalse(action.requiresScreenRecording)
        XCTAssertFalse(action.mutatesHostUI)
        XCTAssertThrowsError(try ComputerAction(raw: "display"))
    }

    func testScreenshotDisplaySelectorsAreBounded() throws {
        let byIndex = try ScreenshotSpec.parse([
            "target": "screen",
            "display_index": 2,
            "max_width": 800,
            "max_height": 600,
        ])
        XCTAssertEqual(byIndex.displayIndex, 2)
        XCTAssertNil(byIndex.displayID)
        XCTAssertEqual(byIndex.maxWidth, 800)

        let byID = try ScreenshotSpec.parse([
            "target": "screen",
            "display_id": 4_294_967_000,
        ])
        XCTAssertEqual(byID.displayID, 4_294_967_000)
        XCTAssertNil(byID.displayIndex)

        XCTAssertThrowsError(try ScreenshotSpec.parse(["display_index": 16]))
        XCTAssertThrowsError(try ScreenshotSpec.parse(["display_index": -1]))
        XCTAssertThrowsError(try ScreenshotSpec.parse(["display_id": 0]))
    }

    func testScreenshotMetadataEncodesDisplayID() throws {
        let metadata = ScreenshotMetadata(
            path: "/tmp/s.png",
            width: 10,
            height: 8,
            bytes: 4,
            sha256: "abc",
            target: "screen",
            windowID: nil,
            displayID: 7
        )
        let data = try JSONEncoder().encode(metadata)
        let object = try XCTUnwrap(
            JSONSerialization.jsonObject(with: data) as? [String: Any]
        )
        XCTAssertEqual(object["display_id"] as? Int, 7)
        XCTAssertEqual(object["target"] as? String, "screen")
    }

    func testWaitDeadlineIsBoundedAndClamped() {
        let start = Date(timeIntervalSinceReferenceDate: 1000)
        let deadline = WaitDeadline(startedAt: start, timeoutMilliseconds: 1500)
        XCTAssertEqual(deadline.timeoutInterval, 1.5)
        XCTAssertEqual(deadline.remaining(at: start), 1.5, accuracy: 0.0001)
        XCTAssertEqual(
            deadline.remaining(at: start.addingTimeInterval(0.5)),
            1.0,
            accuracy: 0.0001
        )
        XCTAssertTrue(deadline.isExpired(at: start.addingTimeInterval(3)))
        XCTAssertEqual(
            deadline.sleepInterval(intervalMilliseconds: 500, at: start),
            0.5,
            accuracy: 0.0001
        )
        XCTAssertEqual(
            deadline.sleepInterval(intervalMilliseconds: 5000, at: start),
            1.5,
            accuracy: 0.0001
        )
        let negative = WaitDeadline(startedAt: start, timeoutMilliseconds: -10)
        XCTAssertEqual(negative.timeoutMilliseconds, 0)
        XCTAssertTrue(negative.isExpired(at: start))
        XCTAssertEqual(negative.elapsedMilliseconds(at: start.addingTimeInterval(1)), 1000)
    }

    func testWaitSpecParsesObservationBounds() throws {
        let spec = try WaitSpec.parse([
            "condition": "window",
            "title": "Main",
            "timeout_ms": 30000,
            "interval_ms": 50,
        ])
        XCTAssertEqual(spec.timeoutMilliseconds, 30000)
        XCTAssertEqual(spec.intervalMilliseconds, 50)
        XCTAssertEqual(spec.condition, .window(title: "Main"))
        let clamped = try WaitSpec.parse([
            "condition": "window",
            "title": "Main",
            "timeout_ms": 1,
            "interval_ms": 9000,
        ])
        XCTAssertEqual(clamped.timeoutMilliseconds, ComputerProtocol.minWaitMilliseconds)
        XCTAssertEqual(clamped.intervalMilliseconds, ComputerProtocol.maxWaitIntervalMilliseconds)
    }

    func testDisplayInfoEncodesTheStableIDAndMainFlag() throws {
        let info = DisplayInfo(
            index: 0,
            id: 69_724_584,
            width: 1728,
            height: 1117,
            name: "Built-in",
            main: true
        )
        let data = try JSONEncoder().encode(info)
        let object = try XCTUnwrap(JSONSerialization.jsonObject(with: data) as? [String: Any])
        XCTAssertEqual(object["index"] as? Int, 0)
        XCTAssertEqual(object["id"] as? Int, 69_724_584)
        XCTAssertEqual(object["width"] as? Int, 1728)
        XCTAssertEqual(object["height"] as? Int, 1117)
        XCTAssertEqual(object["main"] as? Bool, true)
        let decoded = try JSONDecoder().decode(DisplayInfo.self, from: data)
        XCTAssertEqual(decoded, info)
    }

    func testScreenshotSpecDefaultsToAWindowTargetAndBoundedSize() throws {
        let spec = try ScreenshotSpec.parse([:])
        XCTAssertEqual(spec.target, .window)
        XCTAssertNil(spec.windowID)
        XCTAssertNil(spec.displayIndex)
        XCTAssertNil(spec.displayID)
        XCTAssertEqual(spec.maxWidth, ComputerProtocol.defaultScreenshotWidth)
        XCTAssertEqual(spec.maxHeight, ComputerProtocol.defaultScreenshotHeight)
        XCTAssertTrue(spec.output.hasPrefix("screenshots/"))
        XCTAssertTrue(spec.output.hasSuffix(".png"))
    }

    func testWaitConditionEqualityAndAppFrontmostParsing() throws {
        let spec = try WaitSpec.parse([
            "condition": "app_frontmost",
            "bundle_id": "com.example.App",
            "pid": 42,
            "timeout_ms": 500,
            "interval_ms": 100,
        ])
        XCTAssertEqual(
            spec.condition,
            .appFrontmost(bundleID: "com.example.App", pid: 42)
        )
        XCTAssertNotEqual(
            spec.condition,
            .appFrontmost(bundleID: "com.example.Other", pid: 42)
        )
        XCTAssertThrowsError(try WaitSpec.parse(["condition": "app_frontmost"]))
    }
}
