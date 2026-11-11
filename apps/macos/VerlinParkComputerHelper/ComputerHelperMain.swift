// VerlinPark native macOS computer helper.
//
// A daemon-owned command-line tool. It speaks newline-delimited JSON on stdin
// and stdout, uses the user's Accessibility and Screen Recording permissions,
// and only performs the bounded actions defined in ComputerCore.swift.
//
// This helper is NOT sandboxed. Accessibility, CGEvent, and ScreenCaptureKit are
// host-level APIs and cannot be confined by the Seatbelt profile used for the
// browser helper. The daemon therefore treats every computer action as a
// host-state operation, applies the normal approval policy before dispatch, and
// serializes the surface with the `computer` ComputerLease. See ADR-0158.

import AppKit
import ApplicationServices
import CoreGraphics
import Darwin
import Foundation

private let protocolVersion = ComputerProtocol.version

private final class LineWriter {
    private let lock = NSLock()
    private let handle = FileHandle.standardOutput

    func send(_ object: [String: Any]) {
        guard JSONSerialization.isValidJSONObject(object),
              var data = try? JSONSerialization.data(withJSONObject: object, options: [])
        else {
            return
        }
        if data.count > ComputerProtocol.maxResponseBytes {
            let id = (object["id"] as? String) ?? "?"
            let fallback: [String: Any] = [
                "id": id,
                "ok": false,
                "error": [
                    "code": "RESPONSE_TOO_LARGE",
                    "message": "helper response exceeded \(ComputerProtocol.maxResponseBytes) bytes",
                ],
            ]
            guard let replacement = try? JSONSerialization.data(withJSONObject: fallback) else {
                return
            }
            data = replacement
        }
        data.append(0x0A)
        lock.lock()
        defer { lock.unlock() }
        do {
            try handle.write(contentsOf: data)
        } catch {
            exit(0)
        }
    }
}

private final class ComputerController {
    private let writer: LineWriter
    private let scratch: URL
    private let accessibility = AccessibilityController()
    private let input: InputController
    private let screen: ScreenController

    init(writer: LineWriter, scratch: URL) {
        self.writer = writer
        self.scratch = scratch
        input = InputController(accessibility: accessibility)
        screen = ScreenController(scratch: scratch)
    }

    func ready() {
        writer.send(
            ResponseEnvelope.ready(
                pid: ProcessInfo.processInfo.processIdentifier,
                scratch: scratch.path,
                permissions: AccessibilityController.permissions()
            )
        )
    }

    func protocolOverflow() {
        writer.send(
            ResponseEnvelope.error(
                id: "?",
                code: "PROTOCOL_ERROR",
                message: "request line exceeded \(ComputerProtocol.maxRequestBytes) bytes"
            )
        )
    }

    func handle(_ data: Data) {
        let request: ComputerRequest
        do {
            request = try ComputerRequest.parse(data: data)
        } catch let failure as ComputerFailure {
            writer.send(ResponseEnvelope.error(id: "?", code: failure.code, message: failure.message))
            return
        } catch {
            writer.send(
                ResponseEnvelope.error(id: "?", code: "PROTOCOL_ERROR", message: "\(error)")
            )
            return
        }
        do {
            let result = try dispatch(request)
            writer.send(ResponseEnvelope.ok(id: request.id, result: result))
        } catch let failure as ComputerFailure {
            writer.send(
                ResponseEnvelope.error(
                    id: request.id,
                    code: failure.code,
                    message: failure.message,
                    details: failure.details
                )
            )
        } catch {
            writer.send(
                ResponseEnvelope.error(id: request.id, code: "INTERNAL", message: "\(error)")
            )
        }
    }

    private func dispatch(_ request: ComputerRequest) throws -> Any {
        if request.action.requiresAccessibility {
            try AccessibilityController.requireAccessibility()
        }
        if request.action.requiresScreenRecording {
            try AccessibilityController.requireScreenRecording()
        }
        let policy = try SensitiveTargetPolicy.parse(request.params)
        let target = try sensitiveTarget(for: request, policy: policy)
        let result: Any
        switch request.action {
        case .permissions:
            let permissions = AccessibilityController.permissions()
            var payload: [String: Any] = [
                "accessibility_trusted": permissions.accessibility,
                "screen_recording": permissions.screenRecording,
                "helper_version": protocolVersion,
                "pid": ProcessInfo.processInfo.processIdentifier,
            ]
            if let detail = permissions.detail {
                payload["detail"] = detail
            }
            result = payload
        case .apps:
            let includeBackground = try (ParamReader.optionalBool(
                request.params,
                "include_background"
            )) ?? false
            let apps = try accessibility.applications(includeBackground: includeBackground)
            result = ["apps": apps, "count": apps.count]
        case .displays:
            let displays = accessibility.displays()
            result = ["displays": displays, "count": displays.count]
        case .windows:
            let bundleID = try ParamReader.optionalString(request.params, "bundle_id", max: 200)
            let pid = try ParamReader.optionalInt(request.params, "pid").map { Int32(clamping: $0) }
            let includeOffscreen = try (ParamReader.optionalBool(
                request.params,
                "include_offscreen"
            )) ?? false
            let windows = try accessibility.windows(
                bundleID: bundleID,
                pid: pid,
                includeOffscreen: includeOffscreen
            )
            result = ["windows": windows, "count": windows.count]
        case .focus:
            result = try accessibility.focus(FocusSpec.parse(request.params))
        case .snapshot:
            let bundleID = try ParamReader.optionalString(request.params, "bundle_id", max: 200)
            let pid = try ParamReader.optionalInt(request.params, "pid").map { Int32(clamping: $0) }
            let windowID: UInt32?
            if let value = try ParamReader.optionalInt(request.params, "window_id") {
                guard value > 0, value <= Int(UInt32.max) else {
                    throw ComputerFailure.invalid("window_id is out of range")
                }
                windowID = UInt32(value)
            } else {
                windowID = nil
            }
            result = try accessibility.snapshot(
                bundleID: bundleID,
                pid: pid,
                windowID: windowID,
                limits: SnapshotLimits.parse(request.params)
            )
        case .screenshot:
            result = try screen.capture(ScreenshotSpec.parse(request.params))
        case .click:
            result = try input.click(ClickSpec.parse(request.params))
        case .type:
            result = try input.typeText(TypeSpec.parse(request.params))
        case .key:
            let rawKey = try ParamReader.requiredString(request.params, "key", max: 32)
            let modifiers = try (ParamReader.optionalStringArray(
                request.params,
                "modifiers",
                maxItems: ComputerProtocol.maxModifiers,
                maxItemBytes: 24
            )) ?? []
            let allowDestructive = try (ParamReader.optionalBool(
                request.params,
                "allow_destructive"
            )) ?? false
            result = try input.key(
                KeySpec(key: rawKey, modifiers: modifiers),
                allowDestructive: allowDestructive
            )
        case .scroll:
            result = try input.scroll(ScrollSpec.parse(request.params))
        case .drag:
            result = try input.drag(DragSpec.parse(request.params))
        case .wait:
            result = try wait(WaitSpec.parse(request.params))
        case .close:
            DispatchQueue.global().asyncAfter(deadline: .now() + 0.05) {
                exit(0)
            }
            result = ["closed": true]
        }
        guard var payload = result as? [String: Any], let target else {
            return result
        }
        payload["target"] = target
        return payload
    }

    /// Resolve the target before any input or capture. `deny` refuses entirely;
    /// `require_approval` allows only when the daemon marked an explicitly
    /// named target (the approval layer could see it).
    private func sensitiveTarget(
        for request: ComputerRequest,
        policy: SensitiveTargetPolicy
    ) throws -> [String: Any]? {
        let action = request.action
        guard action == .snapshot || action == .screenshot || action == .focus
            || action == .click || action == .type || action == .key
            || action == .scroll || action == .drag
        else {
            return nil
        }
        let bundleID = try ParamReader.optionalNonEmptyString(
            request.params,
            "bundle_id",
            max: 200
        )
        let pid = try ParamReader.optionalInt(request.params, "pid").map { Int32(clamping: $0) }
        let windowID: UInt32?
        if let value = try ParamReader.optionalInt(request.params, "window_id") {
            guard value > 0, value <= Int(UInt32.max) else {
                throw ComputerFailure.invalid("window_id is out of range")
            }
            windowID = UInt32(value)
        } else {
            windowID = nil
        }
        let ref = try ParamReader.optionalString(
            request.params,
            "ref",
            max: ComputerProtocol.maxRefLength
        )
        let windowTitle = try ParamReader.optionalNonEmptyString(
            request.params,
            "window_title",
            max: 200
        )
        var info = try accessibility.targetInfo(
            bundleID: bundleID,
            pid: pid,
            windowID: windowID,
            ref: ref,
            windowTitle: windowTitle
        )
        let targetBundle = info["bundle_id"] as? String
        let targetApp = info["app"] as? String
        let targetTitle = info["window_title"] as? String
        let sensitive = policy.matches(
            bundleID: targetBundle,
            app: targetApp,
            title: targetTitle
        )
        info["sensitive"] = sensitive
        if let refusal = policy.refusal(
            bundleID: targetBundle,
            app: targetApp,
            title: targetTitle
        ) {
            throw refusal
        }
        return info
    }

    private func wait(_ spec: WaitSpec) throws -> [String: Any] {
        switch spec.condition {
        case .ref, .element:
            try AccessibilityController.requireAccessibility()
        case .window, .appFrontmost:
            break
        }
        let observation = accessibility.waitForCondition(
            spec.condition,
            timeoutMilliseconds: spec.timeoutMilliseconds,
            intervalMilliseconds: spec.intervalMilliseconds
        )
        guard observation.matched else {
            throw ComputerFailure.timeout(
                "wait condition \(Self.describe(spec.condition)) did not match within \(spec.timeoutMilliseconds) ms"
            )
        }
        return [
            "matched": true,
            "condition": Self.describe(spec.condition),
            "elapsed_ms": observation.elapsedMs,
            "observed_by": observation.observedBy,
            "polling_fallback": true,
        ]
    }

    private static func describe(_ condition: WaitCondition) -> String {
        switch condition {
        case let .ref(ref):
            "ref \(ref)"
        case let .element(role, title, identifier):
            "element role=\(role ?? "*") title=\(title ?? "*") identifier=\(identifier ?? "*")"
        case let .window(title):
            "window title contains \(title)"
        case let .appFrontmost(bundleID, pid):
            "app frontmost bundle=\(bundleID ?? "*") pid=\(pid.map(String.init) ?? "*")"
        }
    }
}

private func canonicalPath(_ path: String) -> String {
    guard let resolved = realpath(path, nil) else { return path }
    defer { free(resolved) }
    return String(cString: resolved)
}

private func argument(_ arguments: [String], _ name: String) -> String? {
    guard let index = arguments.firstIndex(of: name), index + 1 < arguments.count else {
        return nil
    }
    return arguments[index + 1]
}

@main
struct ComputerHelperMain {
    static func main() {
        let environment = ProcessInfo.processInfo.environment
        guard environment["VERLINPARK_COMPUTER_HELPER"] == "1" else {
            FileHandle.standardError.write(
                Data("VerlinParkComputerHelper is daemon-owned and refuses standalone use\n".utf8)
            )
            exit(64)
        }
        let arguments = CommandLine.arguments
        guard let rawScratch = argument(arguments, "--scratch"), !rawScratch.isEmpty else {
            FileHandle.standardError.write(
                Data("VerlinParkComputerHelper requires --scratch\n".utf8)
            )
            exit(64)
        }
        let scratch = URL(
            fileURLWithPath: canonicalPath(rawScratch),
            isDirectory: true
        )
        do {
            try FileManager.default.createDirectory(
                at: scratch,
                withIntermediateDirectories: true
            )
            try FileManager.default.createDirectory(
                at: scratch.appendingPathComponent("screenshots", isDirectory: true),
                withIntermediateDirectories: true
            )
        } catch {
            FileHandle.standardError.write(
                Data("VerlinParkComputerHelper could not create scratch: \(error)\n".utf8)
            )
            exit(70)
        }

        let writer = LineWriter()
        let controller = ComputerController(writer: writer, scratch: scratch)
        controller.ready()

        if let rawParent = argument(arguments, "--parent-pid"),
           let parent = Int32(rawParent), parent > 1
        {
            DispatchQueue.global(qos: .utility).async {
                while true {
                    Thread.sleep(forTimeInterval: 2.0)
                    if kill(parent, 0) != 0 {
                        exit(0)
                    }
                }
            }
        }

        let queue = DispatchQueue(label: "com.verlintas.VerlinParkComputerHelper.actions")
        let input = FileHandle.standardInput
        DispatchQueue.global(qos: .userInitiated).async {
            var buffer = Data()
            while true {
                let chunk = input.availableData
                if chunk.isEmpty {
                    // Drain queued requests before exiting so a short-lived
                    // pipe (manual smoke, daemon close) still gets responses.
                    queue.sync {}
                    exit(0)
                }
                buffer.append(chunk)
                while let newline = buffer.firstIndex(of: 0x0A) {
                    let line = buffer.subdata(in: 0 ..< newline)
                    buffer.removeSubrange(0 ... newline)
                    if line.isEmpty {
                        continue
                    }
                    queue.async {
                        controller.handle(line)
                    }
                }
                if buffer.count > ComputerProtocol.maxRequestBytes {
                    controller.protocolOverflow()
                    buffer.removeAll(keepingCapacity: true)
                }
            }
        }
        dispatchMain()
    }
}
