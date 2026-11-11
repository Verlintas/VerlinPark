// VerlinPark computer helper core.
//
// This file is compiled into both the packaged `VerlinParkComputerHelper`
// command-line tool and its unit-test target. It deliberately contains only
// pure validation, bounding, and encoding helpers: no Accessibility calls, no
// CGEvent posting, and no ScreenCaptureKit. That split lets the safety rules be
// tested without granting macOS GUI permissions or running a GUI session.
//
// The helper is a trusted local process. It is not a sandbox boundary. Its job
// is to make the daemon-owned protocol narrow, bounded, and explicit so a
// misbehaving agent cannot turn a GUI action into arbitrary process/file/network
// access. The Rust daemon remains the approval and lease authority.

import CoreGraphics
import Foundation

public enum ComputerProtocol {
    public static let version = "1"
    public static let maxRequestBytes = 1_048_576
    public static let maxResponseBytes = 2_097_152

    public static let defaultSnapshotDepth = 10
    public static let maxSnapshotDepth = 20
    public static let defaultSnapshotElements = 400
    public static let maxSnapshotElements = 1200
    public static let defaultSnapshotBytes = 131_072
    public static let maxSnapshotBytes = 262_144
    public static let maxSnapshotTitleChars = 200
    public static let maxSnapshotValueChars = 200
    public static let maxSnapshotTextChars = 400
    public static let maxChildrenPerNode = 200

    public static let maxTypeBytes = 10000
    public static let maxRefLength = 64
    public static let maxStringParam = 2000
    public static let maxApps = 200
    public static let maxWindows = 100
    public static let maxModifiers = 4
    public static let maxClickCount = 2
    public static let maxScrollDelta = 1000
    public static let maxDragDurationMilliseconds = 2000
    public static let maxDragSteps = 60

    public static let defaultWaitMilliseconds = 5000
    public static let minWaitMilliseconds = 50
    public static let maxWaitMilliseconds = 30000
    public static let defaultWaitIntervalMilliseconds = 100
    public static let minWaitIntervalMilliseconds = 50
    public static let maxWaitIntervalMilliseconds = 1000

    public static let defaultScreenshotWidth = 1600
    public static let defaultScreenshotHeight = 1200
    public static let minScreenshotDimension = 320
    public static let maxScreenshotDimension = 2560
    public static let maxScreenshotBytes = 5 * 1024 * 1024
}

public struct ComputerFailure: Error, CustomStringConvertible {
    public let code: String
    public let message: String
    public let details: [String: Any]?

    public init(_ code: String, _ message: String, details: [String: Any]? = nil) {
        self.code = code
        self.message = message
        self.details = details
    }

    public var description: String {
        "\(code): \(message)"
    }

    public static func invalid(_ message: String) -> ComputerFailure {
        ComputerFailure("INVALID_ARGUMENT", message)
    }

    public static func permission(_ message: String) -> ComputerFailure {
        ComputerFailure("PERMISSION_DENIED", message)
    }

    public static func timeout(_ message: String) -> ComputerFailure {
        ComputerFailure("TIMEOUT", message)
    }

    /// A stale ref carries the helper's current generation and an explicit
    /// snapshot hint. The daemon surfaces this as a structured error and never
    /// retries the input action automatically.
    public static func staleRef(
        _ ref: String,
        generation: UInt64,
        hint: String = "Take a fresh snapshot and use the new ref. The action was not retried."
    ) -> ComputerFailure {
        ComputerFailure(
            "STALE_REF",
            "element reference \(ref) is not in the latest snapshot (generation \(generation))",
            details: [
                "snapshot_generation": generation,
                "ref_generation": ElementRef.generation(from: ref) ?? 0,
                "action": "snapshot",
                "hint": hint,
                "auto_retry": false,
            ]
        )
    }

    public static func sensitiveTarget(
        _ bundleID: String?,
        app: String?,
        title: String?,
        requiresApproval: Bool
    ) -> ComputerFailure {
        var details: [String: Any] = [
            "requires_approval": requiresApproval,
            "auto_retry": false,
            "hint": requiresApproval
                ? "Re-issue the action with an explicit bundle_id or window_id so the daemon approval layer can classify the target."
                :
                "The project GUI policy denies this sensitive target. Change the policy or ask the operator.",
        ]
        if let bundleID, !bundleID.isEmpty {
            details["bundle_id"] = String(bundleID.prefix(200))
        }
        if let app, !app.isEmpty {
            details["app"] = String(app.prefix(200))
        }
        if let title, !title.isEmpty {
            details["window_title"] = String(title.prefix(200))
        }
        return ComputerFailure(
            "SENSITIVE_TARGET",
            "the target is a sensitive GUI surface and the helper refused the action",
            details: details
        )
    }
}

public enum ParamReader {
    public static func optionalString(
        _ params: [String: Any],
        _ key: String,
        max: Int = ComputerProtocol.maxStringParam
    ) throws -> String? {
        guard let raw = params[key], !(raw is NSNull) else { return nil }
        guard let value = raw as? String else {
            throw ComputerFailure.invalid("\(key) must be a string")
        }
        if value.utf8.count > max {
            throw ComputerFailure.invalid("\(key) exceeds \(max) bytes")
        }
        return value
    }

    public static func optionalNonEmptyString(
        _ params: [String: Any],
        _ key: String,
        max: Int = ComputerProtocol.maxStringParam
    ) throws -> String? {
        guard let value = try optionalString(params, key, max: max) else { return nil }
        return value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? nil : value
    }

    public static func requiredString(
        _ params: [String: Any],
        _ key: String,
        max: Int = ComputerProtocol.maxStringParam
    ) throws -> String {
        guard let value = try optionalString(params, key, max: max),
              !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else {
            throw ComputerFailure.invalid("\(key) is required")
        }
        return value
    }

    public static func optionalInt(_ params: [String: Any], _ key: String) throws -> Int? {
        guard let raw = params[key], !(raw is NSNull) else { return nil }
        if raw is Bool {
            throw ComputerFailure.invalid("\(key) must be an integer")
        }
        if let value = raw as? Int {
            return value
        }
        if let number = raw as? NSNumber, number.doubleValue.isFinite,
           number.doubleValue.rounded() == number.doubleValue
        {
            return number.intValue
        }
        throw ComputerFailure.invalid("\(key) must be an integer")
    }

    public static func optionalDouble(_ params: [String: Any], _ key: String) throws -> Double? {
        guard let raw = params[key], !(raw is NSNull) else { return nil }
        if raw is Bool {
            throw ComputerFailure.invalid("\(key) must be a number")
        }
        if let value = raw as? Double {
            return value
        }
        if let number = raw as? NSNumber {
            return number.doubleValue
        }
        throw ComputerFailure.invalid("\(key) must be a number")
    }

    public static func optionalBool(_ params: [String: Any], _ key: String) throws -> Bool? {
        guard let raw = params[key], !(raw is NSNull) else { return nil }
        guard let value = raw as? Bool else {
            throw ComputerFailure.invalid("\(key) must be a boolean")
        }
        return value
    }

    public static func optionalStringArray(
        _ params: [String: Any],
        _ key: String,
        maxItems: Int,
        maxItemBytes: Int = 32
    ) throws -> [String]? {
        guard let raw = params[key], !(raw is NSNull) else { return nil }
        guard let values = raw as? [Any] else {
            throw ComputerFailure.invalid("\(key) must be an array of strings")
        }
        guard values.count <= maxItems else {
            throw ComputerFailure.invalid("\(key) accepts at most \(maxItems) items")
        }
        var result: [String] = []
        for value in values {
            guard let text = value as? String, text.utf8.count <= maxItemBytes else {
                throw ComputerFailure.invalid("\(key) contains an invalid item")
            }
            result.append(text)
        }
        return result
    }
}

public enum ComputerAction: String, CaseIterable {
    case permissions
    case apps
    case windows
    case displays
    case focus
    case snapshot
    case screenshot
    case click
    case type
    case key
    case scroll
    case drag
    case wait
    case close

    public init(raw: String) throws {
        let normalized = raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard let action = ComputerAction(rawValue: normalized) else {
            throw ComputerFailure.invalid("unknown computer action: \(raw)")
        }
        self = action
    }

    /// Actions that inspect the Accessibility tree or host application list.
    public var isReadOnlyInspection: Bool {
        switch self {
        case .permissions, .apps, .windows, .displays, .snapshot, .wait, .close:
            true
        default:
            false
        }
    }

    /// Every action that reads or drives the AX tree needs Accessibility.
    /// Screenshots use ScreenCaptureKit and are intentionally excluded here;
    /// they have their own Screen Recording preflight.
    public var requiresAccessibility: Bool {
        switch self {
        case .permissions, .screenshot, .displays, .close:
            false
        case .apps, .windows, .focus, .snapshot, .click, .type, .key, .scroll, .drag, .wait:
            true
        }
    }

    public var requiresScreenRecording: Bool {
        self == .screenshot
    }

    /// Actions that can change another application's state. This mirrors the
    /// daemon's risk map but is not the approval decision; the helper only uses
    /// it for diagnostics and defensive ordering.
    public var mutatesHostUI: Bool {
        switch self {
        case .focus, .click, .type, .key, .scroll, .drag:
            true
        case .permissions, .apps, .windows, .displays, .snapshot, .screenshot, .wait, .close:
            false
        }
    }
}

public struct ComputerRequest {
    public let id: String
    public let action: ComputerAction
    public let params: [String: Any]

    public static func parse(data: Data) throws -> ComputerRequest {
        if data.isEmpty {
            throw ComputerFailure("PROTOCOL_ERROR", "empty request")
        }
        guard data.count <= ComputerProtocol.maxRequestBytes else {
            throw ComputerFailure(
                "PROTOCOL_ERROR",
                "request exceeds \(ComputerProtocol.maxRequestBytes) bytes"
            )
        }
        let object: Any
        do {
            object = try JSONSerialization.jsonObject(with: data, options: [])
        } catch {
            throw ComputerFailure("PROTOCOL_ERROR", "request is not valid JSON")
        }
        guard let dictionary = object as? [String: Any] else {
            throw ComputerFailure("PROTOCOL_ERROR", "request must be a JSON object")
        }
        let id = (dictionary["id"] as? String) ?? "?"
        guard id.utf8.count <= 64 else {
            throw ComputerFailure("PROTOCOL_ERROR", "request id is too long")
        }
        guard let rawAction = dictionary["action"] as? String else {
            throw ComputerFailure("PROTOCOL_ERROR", "action is required")
        }
        guard let rawParams = dictionary["params"], !(rawParams is NSNull) else {
            let action = try ComputerAction(raw: rawAction)
            return ComputerRequest(id: id, action: action, params: [:])
        }
        if rawParams is [Any] {
            throw ComputerFailure("PROTOCOL_ERROR", "params must be a JSON object")
        }
        guard let params = rawParams as? [String: Any] else {
            throw ComputerFailure("PROTOCOL_ERROR", "params must be a JSON object")
        }
        let action = try ComputerAction(raw: rawAction)
        return ComputerRequest(id: id, action: action, params: params)
    }
}

public enum ResponseEnvelope {
    public static func ready(
        pid: Int32,
        scratch: String,
        permissions: ComputerPermissions
    ) -> [String: Any] {
        [
            "event": "ready",
            "version": ComputerProtocol.version,
            "pid": pid,
            "scratch": scratch,
            "accessibility_trusted": permissions.accessibility,
            "screen_recording": permissions.screenRecording,
        ]
    }

    public static func ok(id: String, result: Any) -> [String: Any] {
        [
            "id": id,
            "ok": true,
            "result": JSONSanitizer.sanitize(result),
        ]
    }

    public static func error(
        id: String,
        code: String,
        message: String,
        details: [String: Any]? = nil
    ) -> [String: Any] {
        var error: [String: Any] = [
            "code": String(code.prefix(64)),
            "message": String(message.prefix(4000)),
        ]
        if let details {
            error["details"] = JSONSanitizer.sanitize(details)
        }
        return [
            "id": id,
            "ok": false,
            "error": error,
        ]
    }
}

public enum JSONSanitizer {
    public static func sanitize(_ value: Any, depth: Int = 0) -> Any {
        if depth > 24 {
            return "<depth-limit>"
        }
        switch value {
        case let text as String:
            return String(text.prefix(ComputerProtocol.maxSnapshotTextChars * 8))
        case let number as NSNumber:
            if number.doubleValue.isFinite {
                return number
            }
            return String(describing: number)
        case let dictionary as [String: Any]:
            var result: [String: Any] = [:]
            for (key, child) in dictionary.prefix(2000) {
                result[String(key.prefix(200))] = sanitize(child, depth: depth + 1)
            }
            return result
        case let array as [Any]:
            return array.prefix(2000).map { sanitize($0, depth: depth + 1) }
        case let dictionary as NSDictionary:
            var result: [String: Any] = [:]
            for (key, child) in dictionary {
                guard let key = key as? String else { continue }
                result[String(key.prefix(200))] = sanitize(child, depth: depth + 1)
            }
            return result
        case let array as NSArray:
            return array.prefix(2000).map { sanitize($0, depth: depth + 1) }
        case is NSNull:
            return NSNull()
        default:
            return String(describing: value).prefix(ComputerProtocol.maxSnapshotTextChars * 8)
        }
    }
}

public struct ComputerRect: Codable, Equatable {
    public var x: Double
    public var y: Double
    public var width: Double
    public var height: Double

    public init(x: Double, y: Double, width: Double, height: Double) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }

    public init(_ rect: CGRect) {
        x = rect.origin.x
        y = rect.origin.y
        width = rect.size.width
        height = rect.size.height
    }

    public var cgRect: CGRect {
        CGRect(x: x, y: y, width: width, height: height)
    }

    public var isFinite: Bool {
        x.isFinite && y.isFinite && width.isFinite && height.isFinite
    }

    public var isUsable: Bool {
        isFinite && width > 0 && height > 0
    }

    public func contains(_ point: CGPoint, tolerance: Double = 0.5) -> Bool {
        point.x >= x - tolerance
            && point.y >= y - tolerance
            && point.x <= x + width + tolerance
            && point.y <= y + height + tolerance
    }
}

public struct NormalizedPoint: Equatable {
    public let x: Double
    public let y: Double

    public init(x: Double, y: Double) throws {
        guard x.isFinite, y.isFinite else {
            throw ComputerFailure.invalid("normalized point must be finite")
        }
        guard (0 ... 1).contains(x), (0 ... 1).contains(y) else {
            throw ComputerFailure.invalid("normalized point must be between 0 and 1")
        }
        self.x = x
        self.y = y
    }

    public func resolve(in rect: ComputerRect) throws -> CGPoint {
        guard rect.isUsable else {
            throw ComputerFailure.invalid("target window has no usable frame")
        }
        let point = CGPoint(
            x: rect.x + x * rect.width,
            y: rect.y + y * rect.height
        )
        guard point.x.isFinite, point.y.isFinite else {
            throw ComputerFailure.invalid("resolved point is not finite")
        }
        guard rect.contains(point) else {
            throw ComputerFailure.invalid("resolved point is outside the target window")
        }
        return point
    }
}

/// Keeps synthetic input on a display that the user can actually see. A window
/// may extend beyond a display, and an AX frame may be stale or report a
/// non-finite rectangle; posting an event at such a point would be surprising
/// and could hit another app's window on a different Space.
public enum ScreenGuard {
    public static func isOnScreen(
        _ point: CGPoint,
        screens: [ComputerRect],
        tolerance: Double = 1.0
    ) -> Bool {
        guard point.x.isFinite, point.y.isFinite else { return false }
        return screens.contains { $0.contains(point, tolerance: tolerance) }
    }

    public static func requireOnScreen(
        _ point: CGPoint,
        screens: [ComputerRect],
        label: String = "point"
    ) throws {
        guard isOnScreen(point, screens: screens) else {
            throw ComputerFailure.invalid("\(label) is off-screen")
        }
    }
}

public enum ScreenshotTarget: String {
    case window
    case screen
}

public enum ScreenshotOutput {
    public static func defaultRelativePath() -> String {
        "screenshots/computer-\(UUID().uuidString.lowercased()).png"
    }

    /// The helper may only write a PNG below `<scratch>/screenshots`. The
    /// daemon supplies a generated name; the helper still re-validates it so a
    /// malformed or hostile request cannot choose an arbitrary path.
    public static func validatedRelativePath(_ raw: String, scratch: URL) throws -> URL {
        let value = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !value.isEmpty, value.utf8.count <= 200 else {
            throw ComputerFailure.invalid("screenshot output path is empty or too long")
        }
        guard !value.hasPrefix("/"), !value.hasPrefix("~") else {
            throw ComputerFailure.invalid("screenshot output path must be relative")
        }
        guard value.hasPrefix("screenshots/"), value.lowercased().hasSuffix(".png") else {
            throw ComputerFailure.invalid("screenshot output must be a PNG under screenshots/")
        }
        let allowed = CharacterSet.alphanumerics
            .union(CharacterSet(charactersIn: "-_./"))
        guard value.unicodeScalars.allSatisfy({ allowed.contains($0) }) else {
            throw ComputerFailure.invalid("screenshot output path contains unsupported characters")
        }
        let components = value.split(separator: "/", omittingEmptySubsequences: false)
        guard components.allSatisfy({ !$0.isEmpty && $0 != "." && $0 != ".." }) else {
            throw ComputerFailure.invalid("screenshot output path contains an invalid component")
        }
        let root = scratch.standardizedFileURL.resolvingSymlinksInPath()
        let candidate = root.appendingPathComponent(value).standardizedFileURL
        let parent = candidate.deletingLastPathComponent().resolvingSymlinksInPath()
        let expected = root
            .appendingPathComponent("screenshots", isDirectory: true)
            .standardizedFileURL
        guard parent.path == expected.path || parent.path.hasPrefix(expected.path + "/") else {
            throw ComputerFailure.invalid("screenshot output path escapes the helper scratch")
        }
        return parent.appendingPathComponent(candidate.lastPathComponent)
    }
}

public struct ScreenshotSpec {
    public let target: ScreenshotTarget
    public let windowID: UInt32?
    public let displayIndex: Int?
    public let displayID: UInt32?
    public let maxWidth: Int
    public let maxHeight: Int
    public let output: String

    public static func parse(_ params: [String: Any]) throws -> ScreenshotSpec {
        let targetRaw = try (ParamReader.optionalString(params, "target", max: 20)) ?? "window"
        guard let target = ScreenshotTarget(rawValue: targetRaw.lowercased()) else {
            throw ComputerFailure.invalid("screenshot target must be window or screen")
        }
        let windowID: UInt32?
        if let value = try ParamReader.optionalInt(params, "window_id") {
            guard value > 0, value <= Int(UInt32.max) else {
                throw ComputerFailure.invalid("window_id is out of range")
            }
            windowID = UInt32(value)
        } else {
            windowID = nil
        }
        if target == .window, windowID == nil {
            // The helper falls back to the frontmost window; this is explicit
            // rather than a silent screen capture.
        }
        let displayIndex: Int?
        if let value = try ParamReader.optionalInt(params, "display_index") {
            guard (0 ... 15).contains(value) else {
                throw ComputerFailure.invalid("display_index must be between 0 and 15")
            }
            displayIndex = value
        } else {
            displayIndex = nil
        }
        let displayID: UInt32?
        if let value = try ParamReader.optionalInt(params, "display_id") {
            guard value > 0, value <= Int(UInt32.max) else {
                throw ComputerFailure.invalid("display_id is out of range")
            }
            displayID = UInt32(value)
        } else {
            displayID = nil
        }
        let maxWidth = try ((ParamReader.optionalInt(params, "max_width"))
            ?? ComputerProtocol.defaultScreenshotWidth)
            .clamped(to: ComputerProtocol.minScreenshotDimension ... ComputerProtocol.maxScreenshotDimension)
        let maxHeight = try ((ParamReader.optionalInt(params, "max_height"))
            ?? ComputerProtocol.defaultScreenshotHeight)
            .clamped(to: ComputerProtocol.minScreenshotDimension ... ComputerProtocol.maxScreenshotDimension)
        let output = try (ParamReader.optionalString(params, "output", max: 200))
            ?? ScreenshotOutput.defaultRelativePath()
        return ScreenshotSpec(
            target: target,
            windowID: windowID,
            displayIndex: displayIndex,
            displayID: displayID,
            maxWidth: maxWidth,
            maxHeight: maxHeight,
            output: output
        )
    }

    public static func fittedSize(
        width: Int,
        height: Int,
        maxWidth: Int,
        maxHeight: Int
    ) -> (width: Int, height: Int) {
        guard width > 0, height > 0 else { return (1, 1) }
        let widthScale = Double(max(1, maxWidth)) / Double(width)
        let heightScale = Double(max(1, maxHeight)) / Double(height)
        let scale = min(1.0, min(widthScale, heightScale))
        let fittedWidth = max(1, Int((Double(width) * scale).rounded(.down)))
        let fittedHeight = max(1, Int((Double(height) * scale).rounded(.down)))
        return (fittedWidth, fittedHeight)
    }
}

public struct SnapshotLimits: Equatable, Sendable {
    public let maxDepth: Int
    public let maxElements: Int
    public let maxBytes: Int

    public init(maxDepth: Int, maxElements: Int, maxBytes: Int) {
        self.maxDepth = maxDepth.clamped(to: 1 ... ComputerProtocol.maxSnapshotDepth)
        self.maxElements = maxElements.clamped(to: 1 ... ComputerProtocol.maxSnapshotElements)
        self.maxBytes = maxBytes.clamped(to: 1024 ... ComputerProtocol.maxSnapshotBytes)
    }

    public static func parse(_ params: [String: Any]) throws -> SnapshotLimits {
        let depth = try (ParamReader.optionalInt(params, "max_depth"))
            ?? ComputerProtocol.defaultSnapshotDepth
        let elements = try (ParamReader.optionalInt(params, "max_elements"))
            ?? ComputerProtocol.defaultSnapshotElements
        let bytes = try (ParamReader.optionalInt(params, "max_bytes"))
            ?? ComputerProtocol.defaultSnapshotBytes
        return SnapshotLimits(maxDepth: depth, maxElements: elements, maxBytes: bytes)
    }

    public static let `default` = SnapshotLimits(
        maxDepth: ComputerProtocol.defaultSnapshotDepth,
        maxElements: ComputerProtocol.defaultSnapshotElements,
        maxBytes: ComputerProtocol.defaultSnapshotBytes
    )
}

public final class SnapshotNode {
    public let ref: String?
    public let role: String
    public let subrole: String?
    public let identifier: String?
    public let title: String
    public let label: String?
    public let value: String?
    public let enabled: Bool?
    public let focused: Bool?
    public let selected: Bool?
    public let frame: ComputerRect?
    public var children: [SnapshotNode]
    public var truncated: Bool

    public init(
        ref: String?,
        role: String,
        subrole: String? = nil,
        identifier: String? = nil,
        title: String = "",
        label: String? = nil,
        value: String? = nil,
        enabled: Bool? = nil,
        focused: Bool? = nil,
        selected: Bool? = nil,
        frame: ComputerRect? = nil,
        children: [SnapshotNode] = [],
        truncated: Bool = false
    ) {
        self.ref = ref
        self.role = role
        self.subrole = subrole
        self.identifier = identifier
        self.title = title
        self.label = label
        self.value = value
        self.enabled = enabled
        self.focused = focused
        self.selected = selected
        self.frame = frame
        self.children = children
        self.truncated = truncated
    }

    public var estimatedBytes: Int {
        (ref?.utf8.count ?? 0)
            + role.utf8.count
            + (subrole?.utf8.count ?? 0)
            + (identifier?.utf8.count ?? 0)
            + title.utf8.count
            + (label?.utf8.count ?? 0)
            + (value?.utf8.count ?? 0)
            + 96
    }

    public func shallowCopy() -> SnapshotNode {
        SnapshotNode(
            ref: ref,
            role: role,
            subrole: subrole,
            identifier: identifier,
            title: title,
            label: label,
            value: value,
            enabled: enabled,
            focused: focused,
            selected: selected,
            frame: frame,
            children: [],
            truncated: truncated
        )
    }

    public func dictionary() -> [String: Any] {
        var result: [String: Any] = [
            "role": role,
            "title": title,
            "truncated": truncated,
        ]
        if let ref {
            result["ref"] = ref
        }
        if let subrole {
            result["subrole"] = subrole
        }
        if let identifier {
            result["identifier"] = identifier
        }
        if let label {
            result["label"] = label
        }
        if let value {
            result["value"] = value
        }
        if let enabled {
            result["enabled"] = enabled
        }
        if let focused {
            result["focused"] = focused
        }
        if let selected {
            result["selected"] = selected
        }
        if let frame {
            result["frame"] = [
                "x": frame.x,
                "y": frame.y,
                "width": frame.width,
                "height": frame.height,
            ]
        }
        if !children.isEmpty {
            result["children"] = children.map { $0.dictionary() }
        }
        return result
    }
}

public final class SnapshotBudget {
    public let limits: SnapshotLimits
    public private(set) var elements = 0
    public private(set) var estimatedBytes = 0
    public private(set) var truncated = false

    public init(limits: SnapshotLimits) {
        self.limits = limits
    }

    public func admit(depth: Int, bytes: Int) -> Bool {
        if depth > limits.maxDepth {
            truncated = true
            return false
        }
        if elements >= limits.maxElements {
            truncated = true
            return false
        }
        if estimatedBytes + bytes > limits.maxBytes {
            truncated = true
            return false
        }
        elements += 1
        estimatedBytes += bytes
        return true
    }

    public static func bounded(
        _ node: SnapshotNode,
        depth: Int,
        budget: SnapshotBudget
    ) -> SnapshotNode? {
        guard budget.admit(depth: depth, bytes: node.estimatedBytes) else {
            return nil
        }
        let copy = node.shallowCopy()
        if depth >= budget.limits.maxDepth {
            copy.truncated = true
            copy.children = []
            return copy
        }
        var kept: [SnapshotNode] = []
        for child in node.children {
            if let boundedChild = bounded(child, depth: depth + 1, budget: budget) {
                kept.append(boundedChild)
            } else {
                copy.truncated = true
            }
        }
        copy.children = kept
        return copy
    }
}

public enum SensitiveValuePolicy {
    public static func isSecureField(role: String, subrole: String?) -> Bool {
        if role == "AXSecureTextField" || role == "AXSecureTextArea" {
            return true
        }
        guard let subrole else { return false }
        return subrole == "AXSecureTextField" || subrole == "AXSecureTextArea"
    }

    public static func redactedValue(role: String, subrole: String?, raw: String?) -> String? {
        guard let raw, !raw.isEmpty else { return nil }
        if isSecureField(role: role, subrole: subrole) {
            return "***"
        }
        return String(raw.prefix(ComputerProtocol.maxSnapshotValueChars))
    }

    public static func boundedTitle(_ raw: String?) -> String {
        guard let raw else { return "" }
        return String(raw.prefix(ComputerProtocol.maxSnapshotTitleChars))
    }

    public static func boundedLabel(_ raw: String?) -> String? {
        guard let raw, !raw.isEmpty else { return nil }
        return String(raw.prefix(ComputerProtocol.maxSnapshotTitleChars))
    }
}

public enum ElementRef {
    /// Current refs bind the snapshot generation:
    ///   ax-<8 hex generation>-<16 hex hash>
    /// A ref from an older generation is rejected even when the same element
    /// still exists. Legacy refs without a generation remain parseable so a
    /// warm helper from an older build does not reject every ref, but the
    /// helper still checks membership in the latest snapshot.
    public static func make(
        pid: Int32,
        windowIndex: Int,
        path: [String],
        generation: UInt64
    ) -> String {
        var hash = FNV1a()
        hash.combine("pid=\(pid)")
        hash.combine("window=\(windowIndex)")
        for component in path {
            hash.combine("|")
            hash.combine(component)
        }
        return "ax-" + String(format: "%08llx", generation & 0xFFFF_FFFF)
            + "-" + String(format: "%016llx", hash.value)
    }

    public static func make(pid: Int32, windowIndex: Int, path: [String]) -> String {
        make(pid: pid, windowIndex: windowIndex, path: path, generation: 0)
    }

    public static func isValid(_ ref: String) -> Bool {
        guard ref.hasPrefix("ax-"), ref.utf8.count <= ComputerProtocol.maxRefLength else {
            return false
        }
        let suffix = ref.dropFirst(3)
        if let separator = suffix.firstIndex(of: "-") {
            let generation = suffix[..<separator]
            let hash = suffix[suffix.index(after: separator)...]
            guard generation.count == 8, generation.allSatisfy(\.isHexDigit),
                  (16 ... 32).contains(hash.count),
                  hash.allSatisfy(\.isHexDigit)
            else {
                return false
            }
            return true
        }
        guard (8 ... 32).contains(suffix.count) else { return false }
        return suffix.allSatisfy(\.isHexDigit)
    }

    /// Embedded generation, or nil for a legacy deterministic ref.
    public static func generation(from ref: String) -> UInt64? {
        guard ref.hasPrefix("ax-") else { return nil }
        let suffix = ref.dropFirst(3)
        guard let separator = suffix.firstIndex(of: "-") else { return nil }
        let generation = suffix[..<separator]
        guard generation.count == 8 else { return nil }
        return UInt64(generation, radix: 16)
    }
}

public struct SensitiveTargetPolicy: Equatable {
    public enum Mode: String, Equatable {
        case deny
        case requireApproval = "require_approval"
    }

    public static let builtInBundles: [String] = [
        "com.apple.systempreferences",
        "com.apple.systemsettings",
        "com.apple.securityagent",
        "com.apple.keychainaccess",
        "com.apple.passwords",
        "com.apple.loginwindow",
        "com.1password.1password",
        "com.1password.1password7",
        "com.agilebits.onepassword7",
        "com.bitwarden.desktop",
        "com.lastpass.lastpass",
        "com.dashlane.dashlane",
        "com.keepersecurity.passwordmanager",
        "com.nordpass.nordpass",
    ]

    public static let builtInTitles: [String] = [
        "system settings",
        "system preferences",
        "securityagent",
        "keychain access",
        "passwords",
        "privacy & security",
        "screen recording",
        "full disk access",
        "accessibility",
        "authentication",
        "1password",
        "bitwarden",
        "lastpass",
        "dashlane",
        "nordpass",
    ]

    public let mode: Mode
    public let bundles: [String]
    public let titles: [String]
    public let acknowledged: Bool

    public init(
        mode: Mode,
        bundles: [String] = [],
        titles: [String] = [],
        acknowledged: Bool = false
    ) {
        self.mode = mode
        self.bundles = Self.normalizedBundles(bundles)
        self.titles = Self.normalizedTitles(titles)
        self.acknowledged = acknowledged
    }

    public static func parse(_ params: [String: Any]) throws -> SensitiveTargetPolicy {
        let rawMode = try (ParamReader.optionalString(params, "sensitive_mode", max: 40)) ?? "deny"
        let mode = Mode(rawValue: rawMode.lowercased()) ?? .deny
        let bundles = try ParamReader.optionalStringArray(
            params,
            "sensitive_bundles",
            maxItems: 64,
            maxItemBytes: 200
        ) ?? []
        let titles = try ParamReader.optionalStringArray(
            params,
            "sensitive_titles",
            maxItems: 64,
            maxItemBytes: 200
        ) ?? []
        let acknowledged = try (ParamReader.optionalBool(params, "sensitive_ack")) ?? false
        return SensitiveTargetPolicy(
            mode: mode,
            bundles: bundles,
            titles: titles,
            acknowledged: acknowledged
        )
    }

    public static func normalizedBundles(_ values: [String]) -> [String] {
        var seen: Set<String> = []
        var result: [String] = []
        for value in values {
            let normalized = value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            if !normalized.isEmpty, seen.insert(normalized).inserted {
                result.append(normalized)
            }
        }
        return result
    }

    public static func normalizedTitles(_ values: [String]) -> [String] {
        var seen: Set<String> = []
        var result: [String] = []
        for value in values {
            let normalized = value.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            if !normalized.isEmpty, seen.insert(normalized).inserted {
                result.append(normalized)
            }
        }
        return result
    }

    public var effectiveBundles: [String] {
        var values = Self.builtInBundles
        for bundle in bundles where !values.contains(bundle) {
            values.append(bundle)
        }
        return values
    }

    public var effectiveTitles: [String] {
        var values = Self.builtInTitles
        for title in titles where !values.contains(title) {
            values.append(title)
        }
        return values
    }

    public func matches(bundleID: String?, app: String?, title: String?) -> Bool {
        if let bundleID, !bundleID.isEmpty {
            let normalized = bundleID.lowercased()
            if effectiveBundles.contains(normalized) {
                return true
            }
        }
        let haystacks = [app, title]
            .compactMap { $0?.lowercased() }
            .filter { !$0.isEmpty }
        guard !haystacks.isEmpty else { return false }
        return effectiveTitles.contains { candidate in
            haystacks.contains { $0.contains(candidate) }
        }
    }

    /// nil means the action may proceed. A failure means the helper refuses
    /// before touching the target.
    public func refusal(
        bundleID: String?,
        app: String?,
        title: String?
    ) -> ComputerFailure? {
        guard matches(bundleID: bundleID, app: app, title: title) else {
            return nil
        }
        switch mode {
        case .deny:
            return ComputerFailure.sensitiveTarget(
                bundleID,
                app: app,
                title: title,
                requiresApproval: false
            )
        case .requireApproval:
            guard acknowledged else {
                return ComputerFailure.sensitiveTarget(
                    bundleID,
                    app: app,
                    title: title,
                    requiresApproval: true
                )
            }
            return nil
        }
    }
}

public struct SnapshotGenerationCounter {
    public private(set) var generation: UInt64 = 0
    public private(set) var refs: Set<String> = []

    public init() {}

    @discardableResult
    public mutating func next(refs: Set<String>) -> UInt64 {
        generation += 1
        self.refs = refs
        return generation
    }

    public func isCurrent(_ ref: String) -> Bool {
        guard ElementRef.generation(from: ref) == generation else { return false }
        return refs.contains(ref)
    }
}

private struct FNV1a {
    private(set) var value: UInt64 = 0xCBF2_9CE4_8422_2325

    mutating func combine(_ text: String) {
        for byte in text.utf8 {
            value ^= UInt64(byte)
            value = value &* 0x0000_0100_0000_01B3
        }
    }
}

public enum Modifier: String, CaseIterable {
    case command
    case shift
    case option
    case control

    public var eventFlag: CGEventFlags {
        switch self {
        case .command: .maskCommand
        case .shift: .maskShift
        case .option: .maskAlternate
        case .control: .maskControl
        }
    }

    public static func parse(_ raw: String) throws -> Modifier {
        switch raw.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
        case "cmd", "command", "meta", "super":
            .command
        case "shift":
            .shift
        case "alt", "option", "opt":
            .option
        case "ctrl", "control":
            .control
        default:
            throw ComputerFailure.invalid("unsupported key modifier: \(raw)")
        }
    }
}

public struct KeySpec {
    public let key: String
    public let keyCode: CGKeyCode
    public let modifiers: Set<Modifier>

    public static let keyCodes: [String: CGKeyCode] = [
        "return": 36, "enter": 36, "tab": 48, "space": 49,
        "delete": 51, "backspace": 51, "escape": 53, "esc": 53,
        "forwarddelete": 117, "left": 123, "right": 124, "down": 125, "up": 126,
        "home": 115, "end": 119, "pageup": 116, "pagedown": 121,
        "f1": 122, "f2": 120, "f3": 99, "f4": 118, "f5": 96, "f6": 97,
        "f7": 98, "f8": 100, "f9": 101, "f10": 109, "f11": 103, "f12": 111,
        "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7,
        "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14, "r": 15,
        "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22,
        "5": 23, "=": 24, "9": 25, "7": 26, "-": 27, "8": 28, "0": 29,
        "]": 30, "o": 31, "u": 32, "[": 33, "i": 34, "p": 35, "l": 37,
        "j": 38, "'": 39, "k": 40, ";": 41, "\\": 42, ",": 43, "/": 44,
        "n": 45, "m": 46, ".": 47, "`": 50,
    ]

    public init(key rawKey: String, modifiers rawModifiers: [String] = []) throws {
        let key = rawKey.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        guard !key.isEmpty, key.utf8.count <= 32 else {
            throw ComputerFailure.invalid("key is empty or too long")
        }
        guard let keyCode = KeySpec.keyCodes[key] else {
            throw ComputerFailure.invalid("unsupported key: \(rawKey)")
        }
        guard rawModifiers.count <= ComputerProtocol.maxModifiers else {
            throw ComputerFailure.invalid("too many key modifiers")
        }
        var modifiers = Set<Modifier>()
        for raw in rawModifiers {
            try modifiers.insert(Modifier.parse(raw))
        }
        self.key = key
        self.keyCode = keyCode
        self.modifiers = modifiers
    }

    public var eventFlags: CGEventFlags {
        modifiers.reduce(into: CGEventFlags()) { flags, modifier in
            flags.insert(modifier.eventFlag)
        }
    }

    /// Destructive system/application shortcuts. The daemon classifies these
    /// as `Destructive` before approval; the helper also refuses them unless
    /// the daemon marks the request as approved.
    public var isDestructive: Bool {
        if modifiers.contains(.command), key == "q" {
            return true
        }
        if modifiers.contains(.command), modifiers.contains(.shift),
           key == "delete" || key == "backspace" || key == "forwarddelete"
        {
            return true
        }
        if modifiers.contains(.control), modifiers.contains(.command), key == "q" {
            return true
        }
        if modifiers.contains(.command), modifiers.contains(.option), key == "escape" {
            return true
        }
        return false
    }
}

public struct ClickSpec {
    public let ref: String?
    public let point: NormalizedPoint?
    public let button: String
    public let clickCount: Int
    public let bundleID: String?
    public let pid: Int32?
    public let windowID: UInt32?

    public static func parse(_ params: [String: Any]) throws -> ClickSpec {
        let ref = try ParamReader.optionalString(params, "ref", max: ComputerProtocol.maxRefLength)
        if let ref, !ElementRef.isValid(ref) {
            throw ComputerFailure.invalid("ref is not a valid element reference")
        }
        let bundleID = try ParamReader.optionalNonEmptyString(params, "bundle_id", max: 200)
        let pid = try (ParamReader.optionalInt(params, "pid")).map { Int32(clamping: $0) }
        let windowID: UInt32?
        if let value = try ParamReader.optionalInt(params, "window_id") {
            guard value > 0, value <= Int(UInt32.max) else {
                throw ComputerFailure.invalid("window_id is out of range")
            }
            windowID = UInt32(value)
        } else {
            windowID = nil
        }
        let x = try ParamReader.optionalDouble(params, "x")
        let y = try ParamReader.optionalDouble(params, "y")
        var point: NormalizedPoint?
        if x != nil || y != nil {
            guard let x, let y else {
                throw ComputerFailure.invalid("click requires both x and y")
            }
            point = try NormalizedPoint(x: x, y: y)
        }
        if ref != nil, point != nil {
            throw ComputerFailure.invalid("click accepts either ref or a normalized point, not both")
        }
        if ref == nil, point == nil {
            throw ComputerFailure.invalid("click requires ref or normalized x/y")
        }
        let button = try ((ParamReader.optionalString(params, "button", max: 20)) ?? "left").lowercased()
        guard button == "left" || button == "right" else {
            throw ComputerFailure.invalid("click button must be left or right")
        }
        let clickCount = try ((ParamReader.optionalInt(params, "click_count")) ?? 1)
            .clamped(to: 1 ... ComputerProtocol.maxClickCount)
        return ClickSpec(
            ref: ref,
            point: point,
            button: button,
            clickCount: clickCount,
            bundleID: bundleID,
            pid: pid,
            windowID: windowID
        )
    }
}

public struct TypeSpec {
    public let text: String
    public let ref: String?

    public static func parse(_ params: [String: Any]) throws -> TypeSpec {
        let text = try ParamReader.requiredString(
            params,
            "text",
            max: ComputerProtocol.maxTypeBytes
        )
        guard !text.contains("\u{0}") else {
            throw ComputerFailure.invalid("text contains a NUL character")
        }
        let ref = try ParamReader.optionalString(params, "ref", max: ComputerProtocol.maxRefLength)
        if let ref, !ElementRef.isValid(ref) {
            throw ComputerFailure.invalid("ref is not a valid element reference")
        }
        return TypeSpec(text: text, ref: ref)
    }
}

public struct ScrollSpec {
    public let dx: Int
    public let dy: Int
    public let unit: String
    public let ref: String?

    public static func parse(_ params: [String: Any]) throws -> ScrollSpec {
        let dx = try (ParamReader.optionalInt(params, "dx")) ?? 0
        let dy = try (ParamReader.optionalInt(params, "dy")) ?? 0
        guard dx != 0 || dy != 0 else {
            throw ComputerFailure.invalid("scroll requires a non-zero dx or dy")
        }
        guard abs(dx) <= ComputerProtocol.maxScrollDelta,
              abs(dy) <= ComputerProtocol.maxScrollDelta
        else {
            throw ComputerFailure.invalid("scroll delta exceeds \(ComputerProtocol.maxScrollDelta)")
        }
        let unit = try ((ParamReader.optionalString(params, "unit", max: 20)) ?? "line").lowercased()
        guard unit == "line" || unit == "pixel" else {
            throw ComputerFailure.invalid("scroll unit must be line or pixel")
        }
        let ref = try ParamReader.optionalString(params, "ref", max: ComputerProtocol.maxRefLength)
        if let ref, !ElementRef.isValid(ref) {
            throw ComputerFailure.invalid("ref is not a valid element reference")
        }
        return ScrollSpec(dx: dx, dy: dy, unit: unit, ref: ref)
    }
}

public struct DragSpec {
    public let from: NormalizedPoint
    public let to: NormalizedPoint
    public let durationMilliseconds: Int
    public let steps: Int
    public let bundleID: String?
    public let pid: Int32?
    public let windowID: UInt32?

    public static func parse(_ params: [String: Any]) throws -> DragSpec {
        func coordinate(_ name: String) throws -> Double {
            guard let value = try ParamReader.optionalDouble(params, name) else {
                throw ComputerFailure.invalid("drag requires \(name)")
            }
            return value
        }
        let from = try NormalizedPoint(x: coordinate("from_x"), y: coordinate("from_y"))
        let to = try NormalizedPoint(x: coordinate("to_x"), y: coordinate("to_y"))
        let duration = try ((ParamReader.optionalInt(params, "duration_ms")) ?? 300)
            .clamped(to: 0 ... ComputerProtocol.maxDragDurationMilliseconds)
        let steps = try ((ParamReader.optionalInt(params, "steps")) ?? 12)
            .clamped(to: 1 ... ComputerProtocol.maxDragSteps)
        let bundleID = try ParamReader.optionalNonEmptyString(params, "bundle_id", max: 200)
        let pid = try (ParamReader.optionalInt(params, "pid")).map { Int32(clamping: $0) }
        let windowID: UInt32?
        if let value = try ParamReader.optionalInt(params, "window_id") {
            guard value > 0, value <= Int(UInt32.max) else {
                throw ComputerFailure.invalid("window_id is out of range")
            }
            windowID = UInt32(value)
        } else {
            windowID = nil
        }
        return DragSpec(
            from: from,
            to: to,
            durationMilliseconds: duration,
            steps: steps,
            bundleID: bundleID,
            pid: pid,
            windowID: windowID
        )
    }
}

public enum WaitCondition: Equatable {
    case ref(String)
    case element(role: String?, title: String?, identifier: String?)
    case window(title: String)
    case appFrontmost(bundleID: String?, pid: Int32?)

    public static func parse(_ params: [String: Any]) throws -> WaitCondition {
        let condition = try ((ParamReader.optionalString(params, "condition", max: 40)) ?? "ref")
            .lowercased()
        switch condition {
        case "ref":
            let ref = try ParamReader.requiredString(
                params,
                "ref",
                max: ComputerProtocol.maxRefLength
            )
            guard ElementRef.isValid(ref) else {
                throw ComputerFailure.invalid("ref is not a valid element reference")
            }
            return .ref(ref)
        case "element":
            let role = try ParamReader.optionalNonEmptyString(params, "role", max: 80)
            let title = try ParamReader.optionalNonEmptyString(params, "title", max: 200)
            let identifier = try ParamReader.optionalNonEmptyString(
                params,
                "identifier",
                max: 200
            )
            guard role != nil || title != nil || identifier != nil else {
                throw ComputerFailure.invalid("element wait requires role, title, or identifier")
            }
            return .element(role: role, title: title, identifier: identifier)
        case "window":
            return try .window(title: ParamReader.requiredString(params, "title", max: 200))
        case "app_frontmost":
            let bundleID = try ParamReader.optionalNonEmptyString(params, "bundle_id", max: 200)
            let pid = try ParamReader.optionalInt(params, "pid")
            guard bundleID != nil || pid != nil else {
                throw ComputerFailure.invalid("app_frontmost wait requires bundle_id or pid")
            }
            return .appFrontmost(
                bundleID: bundleID,
                pid: pid.map { Int32(clamping: $0) }
            )
        default:
            throw ComputerFailure.invalid("unsupported wait condition: \(condition)")
        }
    }
}

/// Bounded wait arithmetic shared by the helper's AX observer and polling
/// fallback. Keeping it pure makes the timeout/interval contract testable
/// without a GUI session.
public struct WaitDeadline: Equatable {
    public let startedAt: Date
    public let timeoutMilliseconds: Int

    public init(startedAt: Date = Date(), timeoutMilliseconds: Int) {
        self.startedAt = startedAt
        self.timeoutMilliseconds = max(0, timeoutMilliseconds)
    }

    public var timeoutInterval: TimeInterval {
        Double(timeoutMilliseconds) / 1000.0
    }

    public func remaining(at now: Date = Date()) -> TimeInterval {
        max(0, timeoutInterval - now.timeIntervalSince(startedAt))
    }

    public func isExpired(at now: Date = Date()) -> Bool {
        remaining(at: now) <= 0
    }

    public func sleepInterval(
        intervalMilliseconds: Int,
        at now: Date = Date()
    ) -> TimeInterval {
        let interval = Double(max(0, intervalMilliseconds)) / 1000.0
        return min(interval, remaining(at: now))
    }

    public func elapsedMilliseconds(at now: Date = Date()) -> Int {
        max(0, Int(now.timeIntervalSince(startedAt) * 1000))
    }
}

public struct WaitSpec {
    public let condition: WaitCondition
    public let timeoutMilliseconds: Int
    public let intervalMilliseconds: Int

    public static func parse(_ params: [String: Any]) throws -> WaitSpec {
        let timeout = try ((ParamReader.optionalInt(params, "timeout_ms"))
            ?? ComputerProtocol.defaultWaitMilliseconds)
            .clamped(to: ComputerProtocol.minWaitMilliseconds ... ComputerProtocol.maxWaitMilliseconds)
        let interval = try ((ParamReader.optionalInt(params, "interval_ms"))
            ?? ComputerProtocol.defaultWaitIntervalMilliseconds)
            .clamped(to: ComputerProtocol.minWaitIntervalMilliseconds ... ComputerProtocol
                .maxWaitIntervalMilliseconds)
        return try WaitSpec(
            condition: WaitCondition.parse(params),
            timeoutMilliseconds: timeout,
            intervalMilliseconds: interval
        )
    }
}

public struct FocusSpec {
    public let bundleID: String?
    public let pid: Int32?
    public let windowTitle: String?
    public let windowID: UInt32?

    public static func parse(_ params: [String: Any]) throws -> FocusSpec {
        let bundleID = try ParamReader.optionalNonEmptyString(params, "bundle_id", max: 200)
        let pidValue = try ParamReader.optionalInt(params, "pid")
        let pid = pidValue.map { Int32(clamping: $0) }
        guard bundleID != nil || pid != nil else {
            throw ComputerFailure.invalid("focus requires bundle_id or pid")
        }
        let windowTitle = try ParamReader.optionalString(params, "window_title", max: 200)
        let windowID: UInt32?
        if let value = try ParamReader.optionalInt(params, "window_id") {
            guard value > 0, value <= Int(UInt32.max) else {
                throw ComputerFailure.invalid("window_id is out of range")
            }
            windowID = UInt32(value)
        } else {
            windowID = nil
        }
        return FocusSpec(
            bundleID: bundleID,
            pid: pid,
            windowTitle: windowTitle,
            windowID: windowID
        )
    }
}

public struct ComputerApp: Codable, Equatable {
    public var bundleID: String?
    public var name: String
    public var pid: Int32
    public var frontmost: Bool
    public var hidden: Bool
    public var activationPolicy: String

    enum CodingKeys: String, CodingKey {
        case bundleID = "bundle_id"
        case name
        case pid
        case frontmost
        case hidden
        case activationPolicy = "activation_policy"
    }
}

public struct ComputerWindow: Codable, Equatable {
    public var id: UInt32
    public var pid: Int32
    public var app: String
    public var title: String
    public var bounds: ComputerRect
    public var layer: Int
    public var focused: Bool
    public var onScreen: Bool

    enum CodingKeys: String, CodingKey {
        case id
        case pid
        case app
        case title
        case bounds
        case layer
        case focused
        case onScreen = "on_screen"
    }
}

public struct ComputerPermissions: Codable, Equatable {
    public var accessibility: Bool
    public var screenRecording: Bool
    public var detail: String?

    enum CodingKeys: String, CodingKey {
        case accessibility
        case screenRecording = "screen_recording"
        case detail
    }
}

public struct ScreenshotMetadata: Codable, Equatable {
    public var path: String
    public var width: Int
    public var height: Int
    public var bytes: Int
    public var sha256: String
    public var target: String
    public var windowID: UInt32?
    public var displayID: UInt32?

    enum CodingKeys: String, CodingKey {
        case path
        case width
        case height
        case bytes
        case sha256
        case target
        case windowID = "window_id"
        case displayID = "display_id"
    }
}

/// One physical display reported by `computer displays`. The index is stable
/// only for the current helper process/session; `id` is the CGDirectDisplayID
/// used by `screenshot.display_id`.
public struct DisplayInfo: Codable, Equatable {
    public let index: Int
    public let id: UInt32
    public let width: Int
    public let height: Int
    public let name: String
    public let main: Bool
}

public extension Comparable {
    func clamped(to range: ClosedRange<Self>) -> Self {
        min(max(self, range.lowerBound), range.upperBound)
    }
}
