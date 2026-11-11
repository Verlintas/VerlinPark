import CoreGraphics
import Foundation
import XCTest

final class ComputerProtocolTests: XCTestCase {
    func testEveryActionParsesFromJSON() throws {
        for action in ComputerAction.allCases {
            let data = Data(
                #"{"id":"x","action":"\#(action.rawValue)","params":{}}"#.utf8
            )
            let request = try ComputerRequest.parse(data: data)
            XCTAssertEqual(request.action, action)
            XCTAssertEqual(request.id, "x")
        }
    }

    func testActionNamesAreTrimmedAndLowercased() throws {
        let request = try ComputerRequest.parse(
            data: Data(#"{"id":"x","action":"  SCREENSHOT  "}"#.utf8)
        )
        XCTAssertEqual(request.action, .screenshot)
    }

    func testRejectsUnknownAndMissingActions() {
        XCTAssertThrowsError(
            try ComputerRequest.parse(data: Data(#"{"id":"x","action":"run-shell"}"#.utf8))
        )
        XCTAssertThrowsError(
            try ComputerRequest.parse(data: Data(#"{"id":"x","params":{}}"#.utf8))
        )
        XCTAssertThrowsError(
            try ComputerRequest.parse(data: Data(#"{"id":"x","action":42}"#.utf8))
        )
    }

    func testRejectsInvalidJSONAndNonObjectRoot() {
        XCTAssertThrowsError(try ComputerRequest.parse(data: Data("not json".utf8)))
        XCTAssertThrowsError(try ComputerRequest.parse(data: Data("[]".utf8)))
        XCTAssertThrowsError(try ComputerRequest.parse(data: Data("\"string\"".utf8)))
        XCTAssertThrowsError(try ComputerRequest.parse(data: Data()))
    }

    func testRequestBytesCapMatchesTheManagerConstant() {
        let padding = String(repeating: "a", count: ComputerProtocol.maxRequestBytes + 1)
        XCTAssertThrowsError(
            try ComputerRequest.parse(
                data: Data(#"{"id":"x","action":"apps","params":{"x":"\#(padding)"}}"#.utf8)
            )
        )
    }

    func testReadyEventContainsPermissionState() {
        let ready = ResponseEnvelope.ready(
            pid: 1,
            scratch: "/tmp/scratch",
            permissions: ComputerPermissions(
                accessibility: true,
                screenRecording: false,
                detail: nil
            )
        )
        XCTAssertEqual(ready["event"] as? String, "ready")
        XCTAssertEqual(ready["version"] as? String, ComputerProtocol.version)
        XCTAssertEqual(ready["pid"] as? Int32, 1)
        XCTAssertEqual(ready["accessibility_trusted"] as? Bool, true)
        XCTAssertEqual(ready["screen_recording"] as? Bool, false)
    }

    func testSuccessEnvelopeKeepsResultShape() {
        let envelope = ResponseEnvelope.ok(
            id: "42",
            result: ["apps": [], "count": 0]
        )
        XCTAssertEqual(envelope["id"] as? String, "42")
        XCTAssertEqual(envelope["ok"] as? Bool, true)
        let result = envelope["result"] as? [String: Any]
        XCTAssertEqual(result?["count"] as? Int, 0)
    }

    func testErrorEnvelopeIsBounded() {
        let envelope = ResponseEnvelope.error(
            id: "1",
            code: String(repeating: "x", count: 200),
            message: String(repeating: "y", count: 10000)
        )
        let error = envelope["error"] as? [String: Any]
        XCTAssertEqual((error?["code"] as? String)?.count, 64)
        XCTAssertEqual((error?["message"] as? String)?.count, 4000)
    }

    func testProtocolConstantsAreConservative() {
        XCTAssertGreaterThanOrEqual(ComputerProtocol.maxRequestBytes, 64 * 1024)
        XCTAssertGreaterThanOrEqual(ComputerProtocol.maxResponseBytes, ComputerProtocol.maxRequestBytes)
        XCTAssertLessThanOrEqual(ComputerProtocol.maxSnapshotDepth, 20)
        XCTAssertLessThanOrEqual(ComputerProtocol.maxSnapshotElements, 1200)
        XCTAssertLessThanOrEqual(ComputerProtocol.maxSnapshotBytes, 262_144)
        XCTAssertLessThanOrEqual(ComputerProtocol.maxTypeBytes, 10000)
        XCTAssertLessThanOrEqual(ComputerProtocol.maxWaitMilliseconds, 30000)
        XCTAssertLessThanOrEqual(ComputerProtocol.maxScreenshotDimension, 2560)
        XCTAssertLessThanOrEqual(ComputerProtocol.maxScreenshotBytes, 5 * 1024 * 1024)
    }

    func testClickAndDragBytesTravelAsJSONNumbers() throws {
        let click = try ClickSpec.parse(["x": 0.125, "y": 0.875, "click_count": 1])
        XCTAssertEqual(click.point?.x, 0.125)
        XCTAssertEqual(click.point?.y, 0.875)
        let drag = try DragSpec.parse([
            "from_x": 0.0,
            "from_y": 0.0,
            "to_x": 1.0,
            "to_y": 1.0,
        ])
        XCTAssertEqual(drag.from.x, 0)
        XCTAssertEqual(drag.to.y, 1)
    }

    func testWindowIdMustFitUInt32() {
        XCTAssertThrowsError(
            try ScreenshotSpec.parse(["window_id": Int(UInt32.max) + 1])
        )
        XCTAssertNoThrow(
            try ScreenshotSpec.parse(["window_id": Int(UInt32.max)])
        )
    }

    func testParamsReaderTreatsNullAsAbsent() throws {
        XCTAssertNil(try ParamReader.optionalString(["a": NSNull()], "a"))
        XCTAssertNil(try ParamReader.optionalInt(["a": NSNull()], "a"))
        XCTAssertNil(try ParamReader.optionalDouble(["a": NSNull()], "a"))
        XCTAssertNil(try ParamReader.optionalBool(["a": NSNull()], "a"))
        XCTAssertNil(try ParamReader.optionalStringArray(["a": NSNull()], "a", maxItems: 1))
    }

    func testRequestIdBoundaryAndEmptyString() throws {
        let atLimit = String(repeating: "x", count: 64)
        XCTAssertNoThrow(
            try ComputerRequest.parse(
                data: Data(#"{"id":"\#(atLimit)","action":"apps"}"#.utf8)
            )
        )
        XCTAssertThrowsError(
            try ComputerRequest.parse(
                data: Data(#"{"id":"\#(atLimit)x","action":"apps"}"#.utf8)
            )
        )
        let emptyId = try ComputerRequest.parse(
            data: Data(#"{"id":"","action":"apps"}"#.utf8)
        )
        XCTAssertEqual(emptyId.id, "")
    }

    func testNestedParamsRemainDictionaries() throws {
        let request = try ComputerRequest.parse(
            data: Data(#"{"id":"x","action":"click","params":{"target":{"x":0.5,"y":0.5}}}"#.utf8)
        )
        let target = request.params["target"] as? [String: Any]
        XCTAssertEqual(target?["x"] as? Double, 0.5)
    }

    func testEmptyParamsObjectIsAccepted() throws {
        let request = try ComputerRequest.parse(
            data: Data(#"{"id":"x","action":"close","params":{}}"#.utf8)
        )
        XCTAssertTrue(request.params.isEmpty)
        XCTAssertEqual(request.action, .close)
    }

    func testSanitizerCapsDictionaryKeys() {
        let key = String(repeating: "k", count: 400)
        let sanitized = JSONSanitizer.sanitize([key: "value"]) as? [String: Any]
        XCTAssertEqual(sanitized?.keys.first?.count, 200)
    }

    func testActionCapabilitiesDoNotWeakenScreenRecordingToAccessibility() {
        XCTAssertTrue(ComputerAction.screenshot.requiresScreenRecording)
        XCTAssertFalse(ComputerAction.screenshot.requiresAccessibility)
        XCTAssertTrue(ComputerAction.snapshot.requiresAccessibility)
        XCTAssertFalse(ComputerAction.snapshot.requiresScreenRecording)
        XCTAssertFalse(ComputerAction.permissions.requiresScreenRecording)
        XCTAssertFalse(ComputerAction.close.requiresScreenRecording)
    }
}
