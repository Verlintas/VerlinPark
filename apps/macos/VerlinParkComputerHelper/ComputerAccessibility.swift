// Accessibility helpers for the VerlinPark computer helper.
//
// The helper speaks to the macOS Accessibility API as the logged-in user. It
// never runs shell commands and never writes files except the bounded PNG
// output handled by ComputerScreen.swift. Accessibility is a host-level trust
// boundary: the daemon approval policy decides whether an agent may ask for a
// host-state action, but once approved the helper has the user's authority.

import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

/// AX change observer used by `computer wait`. Notifications are best-effort:
/// AppKit does not promise a notification for every visual change, so the wait
/// loop always re-checks the condition and treats the polling interval as the
/// fallback. The observer is created only when a target pid is known.
final class AccessibilityChangeObserver {
    private var observer: AXObserver?
    private var source: CFRunLoopSource?
    private var changed = false
    private let pid: Int32

    init?(pid: Int32) {
        self.pid = pid
        var observer: AXObserver?
        let status = AXObserverCreate(pid, accessibilityObserverCallback, &observer)
        guard status == .success, let observer else {
            return nil
        }
        self.observer = observer
        let app = AXUIElementCreateApplication(pid)
        let context = Unmanaged.passUnretained(self).toOpaque()
        let notifications: [String] = [
            kAXFocusedWindowChangedNotification,
            kAXWindowCreatedNotification,
            kAXFocusedUIElementChangedNotification,
            kAXSelectedChildrenChangedNotification,
            kAXValueChangedNotification,
            kAXTitleChangedNotification,
            kAXUIElementDestroyedNotification,
        ]
        for notification in notifications {
            AXObserverAddNotification(observer, app, notification as CFString, context)
        }
        source = AXObserverGetRunLoopSource(observer)
        if let source {
            CFRunLoopAddSource(CFRunLoopGetCurrent(), source, .defaultMode)
        }
    }

    func service(seconds: TimeInterval) {
        let deadline = Date().addingTimeInterval(seconds)
        repeat {
            CFRunLoopRunInMode(.defaultMode, max(0.01, deadline.timeIntervalSinceNow), true)
            if changed {
                changed = false
                return
            }
        } while Date() < deadline
    }

    func markChanged() {
        changed = true
        CFRunLoopStop(CFRunLoopGetCurrent())
    }

    deinit {
        if let source {
            CFRunLoopRemoveSource(CFRunLoopGetCurrent(), source, .defaultMode)
        }
        _ = pid
    }
}

private func accessibilityObserverCallback(
    _: AXObserver,
    _: AXUIElement,
    _: CFString,
    _ refcon: UnsafeMutableRawPointer?
) {
    guard let refcon else { return }
    let watcher = Unmanaged<AccessibilityChangeObserver>
        .fromOpaque(refcon)
        .takeUnretainedValue()
    watcher.markChanged()
}

final class AccessibilityController {
    private var refs: [String: AXUIElement] = [:]
    private var lastSnapshotPid: Int32?
    private(set) var snapshotGeneration: UInt64 = 0

    // MARK: - Permissions

    static func permissions() -> ComputerPermissions {
        let accessibility = AXIsProcessTrusted()
        let screen = CGPreflightScreenCaptureAccess()
        var details: [String] = []
        if !accessibility {
            details.append(
                "Accessibility is not granted to VerlinParkComputerHelper; enable it in System Settings > Privacy & Security > Accessibility"
            )
        }
        if !screen {
            details.append(
                "Screen Recording is not granted to VerlinParkComputerHelper; enable it in System Settings > Privacy & Security > Screen Recording"
            )
        }
        return ComputerPermissions(
            accessibility: accessibility,
            screenRecording: screen,
            detail: details.isEmpty ? nil : details.joined(separator: " ")
        )
    }

    static func requireAccessibility() throws {
        guard AXIsProcessTrusted() else {
            throw ComputerFailure.permission(
                "Accessibility permission is required for this action. Grant it to VerlinParkComputerHelper in System Settings > Privacy & Security > Accessibility."
            )
        }
    }

    static func requireScreenRecording() throws {
        guard CGPreflightScreenCaptureAccess() else {
            throw ComputerFailure.permission(
                "Screen Recording permission is required for screenshots. Grant it to VerlinParkComputerHelper in System Settings > Privacy & Security > Screen Recording."
            )
        }
    }

    // MARK: - Applications

    func applications(includeBackground: Bool) throws -> [[String: Any]] {
        try Self.requireAccessibility()
        let frontmost = NSWorkspace.shared.frontmostApplication
        let apps = NSWorkspace.shared.runningApplications
            .filter { app in
                if app.processIdentifier <= 0 {
                    return false
                }
                if includeBackground {
                    return true
                }
                return app.activationPolicy == .regular
            }
            .sorted { left, right in
                if left.processIdentifier == frontmost?.processIdentifier {
                    return true
                }
                if right.processIdentifier == frontmost?.processIdentifier {
                    return false
                }
                let leftName = left.localizedName ?? left.bundleIdentifier ?? ""
                let rightName = right.localizedName ?? right.bundleIdentifier ?? ""
                return leftName.localizedCaseInsensitiveCompare(rightName) == .orderedAscending
            }
            .prefix(ComputerProtocol.maxApps)
            .map { app in
                ComputerApp(
                    bundleID: app.bundleIdentifier,
                    name: app.localizedName ?? app.bundleIdentifier ?? "pid \(app.processIdentifier)",
                    pid: app.processIdentifier,
                    frontmost: app.processIdentifier == frontmost?.processIdentifier,
                    hidden: app.isHidden,
                    activationPolicy: Self.activationPolicyName(app.activationPolicy)
                )
            }
        return apps.map(Self.dictionary)
    }

    func frontmostApplication() throws -> NSRunningApplication {
        guard let app = NSWorkspace.shared.frontmostApplication, app.processIdentifier > 0 else {
            throw ComputerFailure("APP_NOT_FOUND", "no frontmost application is available")
        }
        return app
    }

    func runningApplication(bundleID: String?, pid: Int32?) throws -> NSRunningApplication {
        if let bundleID, !bundleID.isEmpty {
            let matches = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
            if let pid {
                guard let app = matches.first(where: { $0.processIdentifier == pid }) else {
                    throw ComputerFailure(
                        "APP_NOT_FOUND",
                        "no running application with bundle id \(bundleID) and pid \(pid)"
                    )
                }
                return app
            }
            guard let app = matches.first else {
                throw ComputerFailure("APP_NOT_FOUND", "no running application with bundle id \(bundleID)")
            }
            return app
        }
        if let pid {
            guard let app = NSRunningApplication(processIdentifier: pid) else {
                throw ComputerFailure("APP_NOT_FOUND", "no running application with pid \(pid)")
            }
            return app
        }
        return try frontmostApplication()
    }

    // MARK: - Windows

    func windows(bundleID: String?, pid explicitPid: Int32?,
                 includeOffscreen: Bool) throws -> [[String: Any]]
    {
        try Self.requireAccessibility()
        let app: NSRunningApplication = if explicitPid == nil, bundleID == nil {
            try frontmostApplication()
        } else {
            try runningApplication(bundleID: bundleID, pid: explicitPid)
        }
        let pid = app.processIdentifier
        let frontmost = NSWorkspace.shared.frontmostApplication?.processIdentifier
        let focused = focusedWindowDescriptor(pid: pid)
        let infos = Self.cgWindows(includeOffscreen: includeOffscreen)
            .filter { ($0.pid == pid) && (includeOffscreen || $0.onScreen) }
            .prefix(ComputerProtocol.maxWindows)
            .map { info in
                var focusedHere = false
                if frontmost == pid, let focused {
                    focusedHere = Self.framesMatch(info.bounds.cgRect, focused.bounds.cgRect)
                        && (focused.title.isEmpty || info.title.isEmpty || info.title == focused.title)
                }
                var result = info
                result.focused = focusedHere
                return result
            }
        return infos.map(Self.dictionary)
    }

    // MARK: - Focus

    func focus(_ spec: FocusSpec) throws -> [String: Any] {
        try Self.requireAccessibility()
        let app = try runningApplication(bundleID: spec.bundleID, pid: spec.pid)
        let pid = app.processIdentifier
        let activated = app.activate(options: [.activateAllWindows])
        let axApp = AXUIElementCreateApplication(pid)
        let windows = axWindows(axApp)
        guard !windows.isEmpty else {
            throw ComputerFailure(
                "WINDOW_NOT_FOUND",
                "application \(app.localizedName ?? "pid \(pid)") has no accessible windows"
            )
        }
        let window = try selectWindow(
            windows,
            pid: pid,
            windowID: spec.windowID,
            title: spec.windowTitle
        )
        AXUIElementPerformAction(window, kAXRaiseAction as CFString)
        AXUIElementSetAttributeValue(window, kAXMainAttribute as CFString, kCFBooleanTrue)
        AXUIElementSetAttributeValue(window, kAXFocusedAttribute as CFString, kCFBooleanTrue)
        let frame = frame(of: window) ?? Self.cgWindow(id: spec.windowID ?? 0)?.bounds
        let title = stringAttribute(window, kAXTitleAttribute) ?? spec.windowTitle ?? ""
        return [
            "focused": true,
            "activated": activated,
            "bundle_id": app.bundleIdentifier ?? "",
            "pid": pid,
            "window_title": title,
            "window": frame.map(Self.dictionary) ?? NSNull(),
        ]
    }

    // MARK: - Snapshot

    func snapshot(
        bundleID: String?,
        pid explicitPid: Int32?,
        windowID: UInt32?,
        limits: SnapshotLimits
    ) throws -> [String: Any] {
        try Self.requireAccessibility()
        let app = try runningApplication(bundleID: bundleID, pid: explicitPid)
        let pid = app.processIdentifier
        let axApp = AXUIElementCreateApplication(pid)
        let windows = axWindows(axApp)
        guard !windows.isEmpty else {
            throw ComputerFailure(
                "WINDOW_NOT_FOUND",
                "application \(app.localizedName ?? "pid \(pid)") has no accessible windows"
            )
        }
        let selected = try selectSnapshotWindows(
            windows,
            pid: pid,
            windowID: windowID
        )
        let budget = SnapshotBudget(limits: limits)
        var nextRefs: [String: AXUIElement] = [:]
        var visited: Set<CFHashCode> = []
        let generation = snapshotGeneration + 1
        let appRef = ElementRef.make(
            pid: pid,
            windowIndex: 0,
            path: ["app"],
            generation: generation
        )
        let appNode = SnapshotNode(
            ref: appRef,
            role: "AXApplication",
            identifier: app.bundleIdentifier,
            title: SensitiveValuePolicy.boundedTitle(app.localizedName ?? app.bundleIdentifier),
            children: [],
            truncated: false
        )
        guard budget.admit(depth: 0, bytes: appNode.estimatedBytes) else {
            throw ComputerFailure(
                "SNAPSHOT_TOO_LARGE",
                "snapshot budget is too small for the application root"
            )
        }
        nextRefs[appRef] = axApp

        var windowNodes: [SnapshotNode] = []
        var selectedTitle = ""
        var selectedID: UInt32?
        for (index, window) in selected.enumerated() {
            guard let node = buildNode(
                window,
                pid: pid,
                windowIndex: index,
                path: ["window:\(index)"],
                depth: 1,
                budget: budget,
                limits: limits,
                visited: &visited,
                refs: &nextRefs,
                generation: generation
            ) else {
                appNode.truncated = true
                continue
            }
            windowNodes.append(node)
            if selectedTitle.isEmpty {
                selectedTitle = stringAttribute(window, kAXTitleAttribute) ?? ""
                selectedID = Self.cgWindowID(pid: pid, frame: frame(of: window))
            }
        }
        appNode.children = windowNodes
        guard !windowNodes.isEmpty else {
            throw ComputerFailure("SNAPSHOT_TOO_LARGE", "snapshot budget did not admit any window")
        }
        if selected.count > 1 {
            appNode.truncated = true
        }

        let payload: [String: Any] = [
            "bundle_id": app.bundleIdentifier ?? "",
            "app": app.localizedName ?? app.bundleIdentifier ?? "",
            "pid": pid,
            "window_title": selectedTitle,
            "window_id": selectedID.map { Int($0) } ?? NSNull(),
            "tree": appNode.dictionary(),
            "node_count": budget.elements,
            "truncated": budget.truncated || appNode.truncated,
            "bytes": 0,
        ]
        var bytes = 0
        if let data = try? JSONSerialization.data(withJSONObject: payload) {
            bytes = data.count
        }
        if bytes > limits.maxBytes {
            throw ComputerFailure(
                "SNAPSHOT_TOO_LARGE",
                "snapshot exceeded \(limits.maxBytes) bytes; request a smaller max_depth or max_elements"
            )
        }
        refs = nextRefs
        snapshotGeneration = generation
        lastSnapshotPid = pid
        var result = payload
        result["bytes"] = bytes
        result["generation"] = Int(Date().timeIntervalSince1970 * 1000)
        result["snapshot_generation"] = generation
        result["ref_format"] = "ax-g<generation>-<hash>"
        return result
    }

    private func selectSnapshotWindows(
        _ windows: [AXUIElement],
        pid: Int32,
        windowID: UInt32?
    ) throws -> [AXUIElement] {
        if let windowID {
            let info = Self.cgWindow(id: windowID)
            guard info != nil else {
                throw ComputerFailure("WINDOW_NOT_FOUND", "window id \(windowID) is not present")
            }
            guard let match = windows.first(where: { window in
                guard let info else { return false }
                if let frame = frame(of: window), Self.framesMatch(frame.cgRect, info.bounds.cgRect) {
                    return true
                }
                if !info.title.isEmpty,
                   stringAttribute(window, kAXTitleAttribute) == info.title
                {
                    return true
                }
                return false
            }) else {
                throw ComputerFailure("WINDOW_NOT_FOUND", "window id \(windowID) has no Accessibility window")
            }
            return [match]
        }
        if let focused = axElement(attribute(AXUIElementCreateApplication(pid), kAXFocusedWindowAttribute)) {
            return [focused]
        }
        return Array(windows.prefix(1))
    }

    private func buildNode(
        _ element: AXUIElement,
        pid: Int32,
        windowIndex: Int,
        path: [String],
        depth: Int,
        budget: SnapshotBudget,
        limits: SnapshotLimits,
        visited: inout Set<CFHashCode>,
        refs: inout [String: AXUIElement],
        generation: UInt64
    ) -> SnapshotNode? {
        let hash = CFHash(element)
        if visited.contains(hash) {
            return SnapshotNode(
                ref: nil,
                role: "AXCycle",
                title: "",
                truncated: true
            )
        }
        visited.insert(hash)
        let role = stringAttribute(element, kAXRoleAttribute) ?? "AXUnknown"
        let subrole = stringAttribute(element, kAXSubroleAttribute)
        let identifier = stringAttribute(element, kAXIdentifierAttribute)
        let title = SensitiveValuePolicy.boundedTitle(stringAttribute(element, kAXTitleAttribute))
        let rawDescription = stringAttribute(element, kAXDescriptionAttribute)
        let label = SensitiveValuePolicy.boundedLabel(rawDescription)
        let rawValue = isValueRole(role) ? stringAttribute(element, kAXValueAttribute) : nil
        let value = SensitiveValuePolicy.redactedValue(role: role, subrole: subrole, raw: rawValue)
        let ref = ElementRef.make(
            pid: pid,
            windowIndex: windowIndex,
            path: path,
            generation: generation
        )
        let node = SnapshotNode(
            ref: ref,
            role: String(role.prefix(80)),
            subrole: subrole.map { String($0.prefix(80)) },
            identifier: identifier.map { String($0.prefix(200)) },
            title: title,
            label: label,
            value: value,
            enabled: boolAttribute(element, kAXEnabledAttribute),
            focused: boolAttribute(element, kAXFocusedAttribute),
            selected: boolAttribute(element, kAXSelectedAttribute),
            frame: frame(of: element)
        )
        guard budget.admit(depth: depth, bytes: node.estimatedBytes) else {
            return nil
        }
        refs[ref] = element
        if depth >= limits.maxDepth {
            node.truncated = true
            return node
        }
        let children = axChildren(element, limit: ComputerProtocol.maxChildrenPerNode + 1)
        if children.count > ComputerProtocol.maxChildrenPerNode {
            node.truncated = true
        }
        var kept: [SnapshotNode] = []
        for (index, child) in children.prefix(ComputerProtocol.maxChildrenPerNode).enumerated() {
            let childRole = stringAttribute(child, kAXRoleAttribute) ?? ""
            let childIdentifier = stringAttribute(child, kAXIdentifierAttribute) ?? ""
            let childPath = path + ["\(index):\(childRole.prefix(40)):\(childIdentifier.prefix(80))"]
            if let built = buildNode(
                child,
                pid: pid,
                windowIndex: windowIndex,
                path: childPath,
                depth: depth + 1,
                budget: budget,
                limits: limits,
                visited: &visited,
                refs: &refs,
                generation: generation
            ) {
                kept.append(built)
            } else {
                node.truncated = true
            }
        }
        node.children = kept
        return node
    }

    func resolveRef(_ ref: String) throws -> AXUIElement {
        guard ElementRef.isValid(ref) else {
            throw ComputerFailure.invalid("ref is not a valid element reference")
        }
        if let generation = ElementRef.generation(from: ref),
           generation != snapshotGeneration
        {
            throw ComputerFailure.staleRef(ref, generation: snapshotGeneration)
        }
        guard let element = refs[ref] else {
            throw ComputerFailure.staleRef(ref, generation: snapshotGeneration)
        }
        return element
    }

    // MARK: - Displays

    /// Physical displays visible to AppKit. The index is for `screenshot`
    /// convenience; `id` is the stable CGDirectDisplayID for this boot.
    func displays() -> [[String: Any]] {
        let main = NSScreen.main
        return NSScreen.screens.enumerated().map { index, screen in
            let number = screen.deviceDescription[
                NSDeviceDescriptionKey("NSScreenNumber")
            ] as? NSNumber
            let id = number?.uint32Value ?? 0
            let frame = screen.frame
            return [
                "index": index,
                "id": Int(id),
                "width": Int(frame.width.rounded()),
                "height": Int(frame.height.rounded()),
                "name": screen.localizedName,
                "main": screen == main,
            ]
        }
    }

    // MARK: - Sensitive target preflight

    /// Resolve the app/window that an action would touch before the helper
    /// performs it. The daemon sends the same sensitive list; this method makes
    /// the helper's decision independent of a stale daemon view.
    func targetInfo(
        bundleID: String?,
        pid explicitPid: Int32?,
        windowID: UInt32?,
        ref: String?,
        windowTitle: String? = nil
    ) throws -> [String: Any] {
        var pid = explicitPid
        var title = windowTitle ?? ""
        var resolvedWindowID = windowID
        if let ref {
            let element = try resolveRef(ref)
            var elementPid: pid_t = 0
            AXUIElementGetPid(element, &elementPid)
            if elementPid > 0 {
                pid = elementPid
            }
            title = self.windowTitle(for: element)
        }
        if let windowID, let info = Self.cgWindow(id: windowID) {
            if pid == nil, info.pid > 0 {
                pid = info.pid
            }
            if !info.title.isEmpty {
                title = info.title
            }
            resolvedWindowID = windowID
        }
        let app: NSRunningApplication? = if let bundleID, !bundleID.isEmpty {
            Self.runningApplication(bundleID: bundleID, pid: pid)
        } else if let pid, pid > 0 {
            NSRunningApplication(processIdentifier: pid)
        } else {
            NSWorkspace.shared.frontmostApplication
        }
        if title.isEmpty, let app, let focused = focusedWindowTitle(pid: app.processIdentifier) {
            title = focused
        }
        if pid == nil {
            pid = app?.processIdentifier
        }
        if resolvedWindowID == nil, let pid, pid > 0 {
            resolvedWindowID = Self.cgWindowID(pid: pid, frame: focusedWindowFrame(pid: pid))
        }
        var result: [String: Any] = [
            "pid": Int(pid ?? 0),
            "app": app?.localizedName ?? app?.bundleIdentifier ?? "",
            "bundle_id": app?.bundleIdentifier ?? "",
            "window_title": String(title.prefix(200)),
        ]
        if let resolvedWindowID {
            result["window_id"] = Int(resolvedWindowID)
        }
        return result
    }

    private static func runningApplication(
        bundleID: String,
        pid: Int32?
    ) -> NSRunningApplication? {
        let matches = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
        if let pid {
            return matches.first(where: { $0.processIdentifier == pid })
        }
        return matches.first
    }

    private func windowTitle(for element: AXUIElement) -> String {
        var current: AXUIElement? = element
        var depth = 0
        while let node = current, depth < 32 {
            if let role = stringAttribute(node, kAXRoleAttribute),
               role == "AXWindow" || role == "AXSheet" || role == "AXDialog"
            {
                return stringAttribute(node, kAXTitleAttribute) ?? ""
            }
            current = axElement(attribute(node, kAXParentAttribute))
            depth += 1
        }
        return ""
    }

    private func focusedWindowTitle(pid: Int32) -> String? {
        let app = AXUIElementCreateApplication(pid)
        guard let window = axElement(attribute(app, kAXFocusedWindowAttribute)) else {
            return nil
        }
        return stringAttribute(window, kAXTitleAttribute)
    }

    private func focusedWindowFrame(pid: Int32) -> ComputerRect? {
        let app = AXUIElementCreateApplication(pid)
        guard let window = axElement(attribute(app, kAXFocusedWindowAttribute)) else {
            return nil
        }
        return frame(of: window)
    }

    func frame(ofRef ref: String) throws -> ComputerRect {
        let element = try resolveRef(ref)
        guard let frame = frame(of: element), frame.isUsable else {
            throw ComputerFailure("ELEMENT_NOT_ACTIONABLE", "element \(ref) has no usable frame")
        }
        return frame
    }

    // MARK: - Target windows for normalized coordinates

    func targetWindowRect(bundleID: String?, pid: Int32?, windowID: UInt32?) throws -> ComputerRect {
        let app = try runningApplication(bundleID: bundleID, pid: pid)
        if let windowID, let info = Self.cgWindow(id: windowID) {
            guard info.bounds.isUsable else {
                throw ComputerFailure("WINDOW_NOT_FOUND", "window id \(windowID) has no usable frame")
            }
            return info.bounds
        }
        let axApp = AXUIElementCreateApplication(app.processIdentifier)
        let windows = axWindows(axApp)
        if let focused = axElement(attribute(axApp, kAXFocusedWindowAttribute)) ?? windows.first,
           let frame = frame(of: focused), frame.isUsable
        {
            return frame
        }
        if let info = Self.cgWindows(includeOffscreen: false)
            .first(where: { $0.pid == app.processIdentifier && $0.layer == 0 && $0.bounds.isUsable })
        {
            return info.bounds
        }
        throw ComputerFailure("WINDOW_NOT_FOUND", "application has no usable window")
    }

    func prepareTarget(
        bundleID: String?,
        pid: Int32?,
        windowID: UInt32?,
        title: String? = nil
    ) throws -> ComputerRect {
        let app = try runningApplication(bundleID: bundleID, pid: pid)
        _ = app.activate(options: [.activateAllWindows])
        let axApp = AXUIElementCreateApplication(app.processIdentifier)
        if let window = try? selectWindow(
            axWindows(axApp),
            pid: app.processIdentifier,
            windowID: windowID,
            title: title
        ) {
            AXUIElementPerformAction(window, kAXRaiseAction as CFString)
            if let frame = frame(of: window), frame.isUsable {
                return frame
            }
        }
        return try targetWindowRect(
            bundleID: app.bundleIdentifier,
            pid: app.processIdentifier,
            windowID: windowID
        )
    }

    func focusElement(_ element: AXUIElement) {
        AXUIElementSetAttributeValue(element, kAXFocusedAttribute as CFString, kCFBooleanTrue)
    }

    // MARK: - Wait conditions

    func waitMatches(_ condition: WaitCondition) -> Bool {
        switch condition {
        case let .ref(ref):
            return refs[ref] != nil
        case let .element(role, title, identifier):
            return elementExists(role: role, title: title, identifier: identifier)
        case let .window(title):
            return CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID)
                .flatMap { $0 as? [[String: Any]] }?
                .contains(where: { entry in
                    let value = (entry[kCGWindowName as String] as? String) ?? ""
                    return value.localizedCaseInsensitiveContains(title)
                }) ?? false
        case let .appFrontmost(bundleID, pid):
            guard let frontmost = NSWorkspace.shared.frontmostApplication else { return false }
            if let pid, frontmost.processIdentifier != pid {
                return false
            }
            if let bundleID, frontmost.bundleIdentifier != bundleID {
                return false
            }
            return true
        }
    }

    /// Bounded wait with an Accessibility observer where the process can
    /// provide one, and an interval polling fallback otherwise. The observer
    /// only shortens the re-check latency; it is not a guarantee that every
    /// window change produces a notification. The deadline is always
    /// authoritative.
    func waitForCondition(
        _ condition: WaitCondition,
        timeoutMilliseconds: Int,
        intervalMilliseconds: Int
    ) -> (matched: Bool, elapsedMs: Int, observedBy: String) {
        let deadline = WaitDeadline(timeoutMilliseconds: timeoutMilliseconds)
        let observer = observerPid(for: condition).flatMap {
            AccessibilityChangeObserver(pid: $0)
        }
        let observation = observer == nil ? "polling" : "ax_observer"
        while true {
            if waitMatches(condition) {
                return (true, deadline.elapsedMilliseconds(), observation)
            }
            if deadline.isExpired() {
                return (false, deadline.elapsedMilliseconds(), observation)
            }
            let slice = deadline.sleepInterval(intervalMilliseconds: intervalMilliseconds)
            if slice <= 0 {
                return (false, deadline.elapsedMilliseconds(), observation)
            }
            if let observer {
                observer.service(seconds: slice)
            } else {
                Thread.sleep(forTimeInterval: slice)
            }
        }
    }

    private func observerPid(for condition: WaitCondition) -> Int32? {
        switch condition {
        case .ref:
            return lastSnapshotPid ?? NSWorkspace.shared.frontmostApplication?.processIdentifier
        case .element, .window:
            return NSWorkspace.shared.frontmostApplication?.processIdentifier
        case let .appFrontmost(bundleID, pid):
            if let pid {
                return pid
            }
            if let bundleID,
               let app = NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
                   .first
            {
                return app.processIdentifier
            }
            return NSWorkspace.shared.frontmostApplication?.processIdentifier
        }
    }

    func elementExists(role: String?, title: String?, identifier: String?) -> Bool {
        guard let app = NSWorkspace.shared.frontmostApplication else { return false }
        let axApp = AXUIElementCreateApplication(app.processIdentifier)
        var queue: [(AXUIElement, Int)] = axWindows(axApp).map { ($0, 0) }
        var visited: Set<CFHashCode> = []
        var inspected = 0
        while !queue.isEmpty, inspected < 2000 {
            let (element, depth) = queue.removeFirst()
            inspected += 1
            let hash = CFHash(element)
            if visited.contains(hash) {
                continue
            }
            visited.insert(hash)
            if let role, stringAttribute(element, kAXRoleAttribute) != role {
                continue
            }
            if let identifier, stringAttribute(element, kAXIdentifierAttribute) != identifier {
                continue
            }
            if let title {
                let elementTitle = stringAttribute(element, kAXTitleAttribute) ?? ""
                let description = stringAttribute(element, kAXDescriptionAttribute) ?? ""
                let value = stringAttribute(element, kAXValueAttribute) ?? ""
                if !elementTitle.localizedCaseInsensitiveContains(title),
                   !description.localizedCaseInsensitiveContains(title),
                   !value.localizedCaseInsensitiveContains(title)
                {
                    continue
                }
            }
            return true
        }
        return false
    }

    // MARK: - Attribute helpers

    private func stringAttribute(_ element: AXUIElement, _ name: String) -> String? {
        guard let raw = attribute(element, name) else { return nil }
        if let value = raw as? String {
            return value
        }
        if let value = raw as? NSAttributedString {
            return value.string
        }
        return nil
    }

    private func boolAttribute(_ element: AXUIElement, _ name: String) -> Bool? {
        guard let raw = attribute(element, name) else { return nil }
        if CFGetTypeID(raw) == CFBooleanGetTypeID() {
            return (raw as! Bool)
        }
        if let number = raw as? NSNumber {
            return number.boolValue
        }
        return nil
    }

    private func attribute(_ element: AXUIElement, _ name: String) -> CFTypeRef? {
        var value: CFTypeRef?
        let error = AXUIElementCopyAttributeValue(element, name as CFString, &value)
        return error == .success ? value : nil
    }

    private func axWindows(_ app: AXUIElement) -> [AXUIElement] {
        axElements(attribute(app, kAXWindowsAttribute))
    }

    private func axChildren(_ element: AXUIElement, limit: Int) -> [AXUIElement] {
        var raw: CFArray?
        let error = AXUIElementCopyAttributeValues(
            element,
            kAXChildrenAttribute as CFString,
            0,
            limit,
            &raw
        )
        guard error == .success else { return [] }
        return axElements(raw)
    }

    private func axElements(_ raw: CFArray?) -> [AXUIElement] {
        guard let array = raw else { return [] }
        let count = CFArrayGetCount(array)
        var result: [AXUIElement] = []
        result.reserveCapacity(count)
        for index in 0 ..< count {
            guard let pointer = CFArrayGetValueAtIndex(array, index) else { continue }
            let element = Unmanaged<AXUIElement>.fromOpaque(pointer).takeUnretainedValue()
            result.append(element)
        }
        return result
    }

    private func axElements(_ raw: CFTypeRef?) -> [AXUIElement] {
        guard let raw, CFGetTypeID(raw) == CFArrayGetTypeID() else { return [] }
        return axElements(unsafeBitCast(raw, to: CFArray.self))
    }

    private func axElement(_ raw: CFTypeRef?) -> AXUIElement? {
        guard let raw, CFGetTypeID(raw) == AXUIElementGetTypeID() else { return nil }
        return unsafeBitCast(raw, to: AXUIElement.self)
    }

    private func frame(of element: AXUIElement) -> ComputerRect? {
        guard let positionRaw = attribute(element, kAXPositionAttribute),
              let sizeRaw = attribute(element, kAXSizeAttribute),
              CFGetTypeID(positionRaw) == AXValueGetTypeID(),
              CFGetTypeID(sizeRaw) == AXValueGetTypeID()
        else {
            return nil
        }
        let positionValue = unsafeBitCast(positionRaw, to: AXValue.self)
        let sizeValue = unsafeBitCast(sizeRaw, to: AXValue.self)
        var point = CGPoint.zero
        var size = CGSize.zero
        guard AXValueGetValue(positionValue, .cgPoint, &point),
              AXValueGetValue(sizeValue, .cgSize, &size)
        else {
            return nil
        }
        let frame = ComputerRect(
            x: point.x,
            y: point.y,
            width: size.width,
            height: size.height
        )
        guard frame.isFinite else { return nil }
        return frame
    }

    private func selectWindow(
        _ windows: [AXUIElement],
        pid: Int32,
        windowID: UInt32?,
        title: String?
    ) throws -> AXUIElement {
        guard !windows.isEmpty else {
            throw ComputerFailure("WINDOW_NOT_FOUND", "application has no accessible windows")
        }
        if let windowID {
            if let info = Self.cgWindow(id: windowID) {
                if let match = windows.first(where: { window in
                    if let frame = frame(of: window), Self.framesMatch(frame.cgRect, info.bounds.cgRect) {
                        return true
                    }
                    if !info.title.isEmpty,
                       stringAttribute(window, kAXTitleAttribute) == info.title
                    {
                        return true
                    }
                    return false
                }) {
                    return match
                }
            }
            throw ComputerFailure("WINDOW_NOT_FOUND", "window id \(windowID) has no Accessibility window")
        }
        if let title, !title.isEmpty {
            if let match = windows.first(where: {
                (stringAttribute($0, kAXTitleAttribute) ?? "")
                    .localizedCaseInsensitiveContains(title)
            }) {
                return match
            }
            throw ComputerFailure("WINDOW_NOT_FOUND", "no window title contains \(title)")
        }
        let axApp = AXUIElementCreateApplication(pid)
        return axElement(attribute(axApp, kAXFocusedWindowAttribute)) ?? windows[0]
    }

    private func focusedWindowDescriptor(pid: Int32) -> (bounds: ComputerRect, title: String)? {
        let axApp = AXUIElementCreateApplication(pid)
        guard let window = axElement(attribute(axApp, kAXFocusedWindowAttribute)),
              let frame = frame(of: window)
        else {
            return nil
        }
        return (frame, stringAttribute(window, kAXTitleAttribute) ?? "")
    }

    private func isValueRole(_ role: String) -> Bool {
        switch role {
        case "AXTextField", "AXTextArea", "AXComboBox", "AXSlider", "AXStaticText",
             "AXCheckBox", "AXRadioButton", "AXPopUpButton", "AXMenuItem", "AXCell",
             "AXRow", "AXIncrementor", "AXSearchField":
            true
        default:
            false
        }
    }

    // MARK: - CGWindow metadata

    struct CGWindowInfo {
        var id: UInt32
        var pid: Int32
        var app: String
        var title: String
        var bounds: ComputerRect
        var layer: Int
        var onScreen: Bool
        var focused: Bool = false
    }

    static func cgWindow(id: UInt32) -> CGWindowInfo? {
        cgWindows(includeOffscreen: true).first { $0.id == id }
    }

    static func cgWindowID(pid: Int32, frame: ComputerRect?) -> UInt32? {
        guard let frame else { return nil }
        return cgWindows(includeOffscreen: true)
            .first(where: { $0.pid == pid && framesMatch($0.bounds.cgRect, frame.cgRect) })?
            .id
    }

    static func cgWindows(includeOffscreen: Bool) -> [CGWindowInfo] {
        let options: CGWindowListOption = includeOffscreen
            ? [.optionAll]
            : [.optionOnScreenOnly, .excludeDesktopElements]
        guard let raw = CGWindowListCopyWindowInfo(options, kCGNullWindowID),
              let entries = raw as? [[String: Any]]
        else {
            return []
        }
        return entries.compactMap { entry in
            guard let id = (entry[kCGWindowNumber as String] as? NSNumber)?.uint32Value,
                  let pid = (entry[kCGWindowOwnerPID as String] as? NSNumber)?.int32Value
            else {
                return nil
            }
            let title = (entry[kCGWindowName as String] as? String) ?? ""
            let app = (entry[kCGWindowOwnerName as String] as? String) ?? ""
            let layer = (entry[kCGWindowLayer as String] as? NSNumber)?.intValue ?? 0
            let onScreen = (entry[kCGWindowIsOnscreen as String] as? NSNumber)?.boolValue ?? false
            guard let boundsDictionary = entry[kCGWindowBounds as String] as? [String: Any],
                  let rect = CGRect(dictionaryRepresentation: boundsDictionary as CFDictionary)
            else {
                return nil
            }
            let bounds = ComputerRect(rect)
            guard bounds.isFinite else { return nil }
            return CGWindowInfo(
                id: id,
                pid: pid,
                app: app,
                title: String(title.prefix(ComputerProtocol.maxSnapshotTitleChars)),
                bounds: bounds,
                layer: layer,
                onScreen: onScreen
            )
        }
    }

    static func framesMatch(_ left: CGRect, _ right: CGRect, tolerance: CGFloat = 2.0) -> Bool {
        abs(left.origin.x - right.origin.x) <= tolerance
            && abs(left.origin.y - right.origin.y) <= tolerance
            && abs(left.size.width - right.size.width) <= tolerance
            && abs(left.size.height - right.size.height) <= tolerance
    }

    static func dictionary(_ app: ComputerApp) -> [String: Any] {
        [
            "bundle_id": app.bundleID ?? NSNull(),
            "name": app.name,
            "pid": Int(app.pid),
            "frontmost": app.frontmost,
            "hidden": app.hidden,
            "activation_policy": app.activationPolicy,
        ]
    }

    static func dictionary(_ window: ComputerWindow) -> [String: Any] {
        [
            "id": Int(window.id),
            "pid": Int(window.pid),
            "app": window.app,
            "title": window.title,
            "bounds": dictionary(window.bounds),
            "layer": window.layer,
            "focused": window.focused,
            "on_screen": window.onScreen,
        ]
    }

    static func dictionary(_ rect: ComputerRect) -> [String: Any] {
        [
            "x": rect.x,
            "y": rect.y,
            "width": rect.width,
            "height": rect.height,
        ]
    }

    static func dictionary(_ info: CGWindowInfo) -> [String: Any] {
        [
            "id": Int(info.id),
            "pid": Int(info.pid),
            "app": info.app,
            "title": info.title,
            "bounds": dictionary(info.bounds),
            "layer": info.layer,
            "focused": info.focused,
            "on_screen": info.onScreen,
        ]
    }

    private static func activationPolicyName(_ policy: NSApplication.ActivationPolicy) -> String {
        switch policy {
        case .regular: "regular"
        case .accessory: "accessory"
        case .prohibited: "prohibited"
        @unknown default: "unknown"
        }
    }
}
