import CoreGraphics
import Foundation
import XCTest

final class ComputerSnapshotTests: XCTestCase {
    func testHundredsOfSiblingNodesAreBoundedByTheBudget() {
        let children = (0 ..< 1500).map { index in
            SnapshotNode(
                ref: ElementRef.make(pid: 1, windowIndex: 0, path: ["\(index):AXButton"]),
                role: "AXButton",
                title: "Button \(index)"
            )
        }
        let root = SnapshotNode(
            ref: "ax-0123456789abcdef",
            role: "AXWindow",
            title: "Main",
            children: children
        )
        let limits = SnapshotLimits(maxDepth: 4, maxElements: 100, maxBytes: 64 * 1024)
        let budget = SnapshotBudget(limits: limits)
        let bounded = SnapshotBudget.bounded(root, depth: 0, budget: budget)
        XCTAssertNotNil(bounded)
        XCTAssertLessThanOrEqual(budget.elements, 100)
        XCTAssertTrue(bounded?.truncated ?? false)
        XCTAssertLessThanOrEqual(bounded?.children.count ?? 0, 99)
    }

    func testDeepTreesStopAtTheDepthLimit() {
        var node = SnapshotNode(ref: nil, role: "AXStaticText", title: "leaf")
        for depth in (0 ..< 30).reversed() {
            node = SnapshotNode(
                ref: nil,
                role: "AXGroup",
                title: "group-\(depth)",
                children: [node]
            )
        }
        let limits = SnapshotLimits(maxDepth: 3, maxElements: 100, maxBytes: 64 * 1024)
        let budget = SnapshotBudget(limits: limits)
        let bounded = SnapshotBudget.bounded(node, depth: 0, budget: budget)
        XCTAssertNotNil(bounded)
        XCTAssertLessThanOrEqual(budget.elements, 4)
        var cursor = bounded
        var depth = 0
        while let child = cursor?.children.first {
            cursor = child
            depth += 1
            XCTAssertLessThanOrEqual(depth, 3)
        }
        XCTAssertTrue(cursor?.truncated ?? false)
    }

    func testRefsAreUniqueAcrossManySiblingPaths() {
        var refs = Set<String>()
        for index in 0 ..< 2000 {
            let ref = ElementRef.make(
                pid: 7,
                windowIndex: 2,
                path: ["\(index):AXButton:action"]
            )
            XCTAssertTrue(ElementRef.isValid(ref))
            refs.insert(ref)
        }
        XCTAssertGreaterThanOrEqual(refs.count, 1990)
    }

    func testSecureTextAreaAndTextFieldNeverExposeValues() {
        for (role, subrole) in [
            ("AXTextField", "AXSecureTextField"),
            ("AXTextArea", "AXSecureTextArea"),
            ("AXSecureTextField", nil),
            ("AXSecureTextArea", nil),
        ] {
            XCTAssertEqual(
                SensitiveValuePolicy.redactedValue(
                    role: role,
                    subrole: subrole,
                    raw: "super-secret"
                ),
                "***"
            )
        }
    }

    func testSnapshotDictionaryOmitsEmptyOptionalValues() {
        let node = SnapshotNode(
            ref: nil,
            role: "AXWindow",
            subrole: nil,
            identifier: nil,
            title: "Main",
            label: nil,
            value: nil,
            enabled: nil,
            focused: nil,
            selected: nil,
            frame: nil
        )
        let dictionary = node.dictionary()
        XCTAssertNil(dictionary["subrole"])
        XCTAssertNil(dictionary["identifier"])
        XCTAssertNil(dictionary["label"])
        XCTAssertNil(dictionary["value"])
        XCTAssertNil(dictionary["enabled"])
        XCTAssertNil(dictionary["focused"])
        XCTAssertNil(dictionary["selected"])
        XCTAssertNil(dictionary["frame"])
        XCTAssertEqual(dictionary["truncated"] as? Bool, false)
    }

    func testScreenshotNestedDirectoriesStayInsideScreenshots() throws {
        let scratch = URL(fileURLWithPath: NSTemporaryDirectory())
            .appendingPathComponent("vp-snapshot-path-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: scratch, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: scratch) }
        let nested = try ScreenshotOutput.validatedRelativePath(
            "screenshots/run-1/window.png",
            scratch: scratch
        )
        XCTAssertTrue(nested.path.hasPrefix(scratch.path))
        XCTAssertTrue(nested.path.hasSuffix("screenshots/run-1/window.png"))
    }

    func testSnapshotFramesAreCarriedAsNumbers() {
        let node = SnapshotNode(
            ref: nil,
            role: "AXWindow",
            title: "Main",
            frame: ComputerRect(x: -10.5, y: 20.25, width: 800, height: 600)
        )
        let frame = node.dictionary()["frame"] as? [String: Double]
        XCTAssertEqual(frame?["x"], -10.5)
        XCTAssertEqual(frame?["y"], 20.25)
        XCTAssertEqual(frame?["width"], 800)
        XCTAssertEqual(frame?["height"], 600)
    }

    func testScreenGuardRejectsNonFiniteAndEmptyScreenList() {
        XCTAssertFalse(
            ScreenGuard.isOnScreen(
                CGPoint(x: 1, y: 1),
                screens: []
            )
        )
        XCTAssertFalse(
            ScreenGuard.isOnScreen(
                CGPoint(x: CGFloat.infinity, y: 1),
                screens: [ComputerRect(x: 0, y: 0, width: 10, height: 10)]
            )
        )
        XCTAssertThrowsError(
            try ScreenGuard.requireOnScreen(
                CGPoint(x: 1, y: 1),
                screens: [],
                label: "pointer"
            )
        )
    }

    func testSnapshotBudgetDoesNotAdmitBeyondByteLimit() {
        let limits = SnapshotLimits(maxDepth: 5, maxElements: 100, maxBytes: 1024)
        let budget = SnapshotBudget(limits: limits)
        XCTAssertTrue(budget.admit(depth: 0, bytes: 512))
        XCTAssertFalse(budget.admit(depth: 1, bytes: 513))
        XCTAssertTrue(budget.truncated)
        XCTAssertEqual(budget.elements, 1)
    }

    func testEstimatedBytesCountsRefAndFields() {
        let without = SnapshotNode(ref: nil, role: "AXButton", title: "")
        let withRef = SnapshotNode(
            ref: "ax-0123456789abcdef",
            role: "AXButton",
            title: ""
        )
        XCTAssertGreaterThan(withRef.estimatedBytes, without.estimatedBytes)
        XCTAssertGreaterThanOrEqual(withRef.estimatedBytes, 96)
    }
}
