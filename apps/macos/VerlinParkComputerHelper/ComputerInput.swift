// CGEvent input for the VerlinPark computer helper.
//
// Input is posted through the HID event tap at global screen coordinates. It
// is intentionally limited to bounded click/type/key/scroll/drag operations;
// there is no AppleScript, shell, URL opening, or process launching here.

import AppKit
import ApplicationServices
import CoreGraphics
import Foundation

final class InputController {
    private let accessibility: AccessibilityController
    private let source = CGEventSource(stateID: .hidSystemState)

    init(accessibility: AccessibilityController) {
        self.accessibility = accessibility
    }

    func click(_ spec: ClickSpec) throws -> [String: Any] {
        try AccessibilityController.requireAccessibility()
        let point: CGPoint
        var target: ComputerRect?
        var elementFrame: ComputerRect?
        if let ref = spec.ref {
            elementFrame = try accessibility.frame(ofRef: ref)
            guard let frame = elementFrame, frame.isUsable else {
                throw ComputerFailure("ELEMENT_NOT_ACTIONABLE", "element \(ref) has no clickable frame")
            }
            point = CGPoint(x: frame.x + frame.width / 2, y: frame.y + frame.height / 2)
            if let element = try? accessibility.resolveRef(ref) {
                accessibility.focusElement(element)
            }
        } else {
            guard let normalized = spec.point else {
                throw ComputerFailure.invalid("click requires ref or normalized x/y")
            }
            target = try accessibility.prepareTarget(
                bundleID: spec.bundleID,
                pid: spec.pid,
                windowID: spec.windowID
            )
            point = try normalized.resolve(in: target ?? ComputerRect(x: 0, y: 0, width: 0, height: 0))
        }
        guard point.x.isFinite, point.y.isFinite else {
            throw ComputerFailure.invalid("click point is not finite")
        }
        try ScreenGuard.requireOnScreen(
            point,
            screens: NSScreen.screens.map { ComputerRect($0.frame) },
            label: "click point"
        )
        let downType: CGEventType = spec.button == "right" ? .rightMouseDown : .leftMouseDown
        let upType: CGEventType = spec.button == "right" ? .rightMouseUp : .leftMouseUp
        let button: CGMouseButton = spec.button == "right" ? .right : .left
        postMouse(.mouseMoved, point: point, button: button)
        guard let down = CGEvent(
            mouseEventSource: source,
            mouseType: downType,
            mouseCursorPosition: point,
            mouseButton: button
        ), let up = CGEvent(
            mouseEventSource: source,
            mouseType: upType,
            mouseCursorPosition: point,
            mouseButton: button
        ) else {
            throw ComputerFailure("INPUT_FAILED", "could not create mouse events")
        }
        for click in 1 ... spec.clickCount {
            down.setIntegerValueField(.mouseEventClickState, value: Int64(click))
            up.setIntegerValueField(.mouseEventClickState, value: Int64(click))
            down.post(tap: .cghidEventTap)
            usleep(20000)
            up.post(tap: .cghidEventTap)
            if click < spec.clickCount {
                usleep(40000)
            }
        }
        var result: [String: Any] = [
            "clicked": true,
            "button": spec.button,
            "click_count": spec.clickCount,
            "x": Double(point.x),
            "y": Double(point.y),
        ]
        if let ref = spec.ref {
            result["ref"] = ref
        }
        if let target {
            result["target_window"] = AccessibilityController.dictionary(target)
        }
        if let elementFrame {
            result["element_frame"] = AccessibilityController.dictionary(elementFrame)
        }
        return result
    }

    func typeText(_ spec: TypeSpec) throws -> [String: Any] {
        try AccessibilityController.requireAccessibility()
        if let ref = spec.ref {
            let element = try accessibility.resolveRef(ref)
            accessibility.focusElement(element)
            usleep(20000)
        }
        let utf16 = Array(spec.text.utf16)
        var index = 0
        var chunks = 0
        while index < utf16.count {
            var end = min(index + 64, utf16.count)
            // Do not split a surrogate pair across two synthetic key events.
            if end < utf16.count, end > index,
               UTF16.isLeadSurrogate(utf16[end - 1]),
               UTF16.isTrailSurrogate(utf16[end])
            {
                end -= 1
            }
            let chunk = Array(utf16[index ..< end])
            guard let down = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: true),
                  let up = CGEvent(keyboardEventSource: source, virtualKey: 0, keyDown: false)
            else {
                throw ComputerFailure("INPUT_FAILED", "could not create keyboard events")
            }
            chunk.withUnsafeBufferPointer { buffer in
                if let base = buffer.baseAddress {
                    down.keyboardSetUnicodeString(stringLength: chunk.count, unicodeString: base)
                    up.keyboardSetUnicodeString(stringLength: chunk.count, unicodeString: base)
                }
            }
            down.post(tap: .cghidEventTap)
            up.post(tap: .cghidEventTap)
            chunks += 1
            index = end
            if index < utf16.count {
                usleep(5000)
            }
        }
        return [
            "typed": true,
            "characters": spec.text.count,
            "utf16_units": utf16.count,
            "chunks": chunks,
            "ref": spec.ref ?? NSNull(),
        ]
    }

    func key(_ spec: KeySpec, allowDestructive: Bool) throws -> [String: Any] {
        try AccessibilityController.requireAccessibility()
        if spec.isDestructive, !allowDestructive {
            throw ComputerFailure(
                "DESTRUCTIVE_KEY_BLOCKED",
                "destructive key combination requires daemon approval"
            )
        }
        guard let down = CGEvent(keyboardEventSource: source, virtualKey: spec.keyCode, keyDown: true),
              let up = CGEvent(keyboardEventSource: source, virtualKey: spec.keyCode, keyDown: false)
        else {
            throw ComputerFailure("INPUT_FAILED", "could not create key events")
        }
        down.flags = spec.eventFlags
        up.flags = spec.eventFlags
        down.post(tap: .cghidEventTap)
        usleep(10000)
        up.post(tap: .cghidEventTap)
        return [
            "key": spec.key,
            "key_code": Int(spec.keyCode),
            "modifiers": spec.modifiers.map(\.rawValue).sorted(),
            "destructive": spec.isDestructive,
        ]
    }

    func scroll(_ spec: ScrollSpec) throws -> [String: Any] {
        try AccessibilityController.requireAccessibility()
        var point: CGPoint?
        if let ref = spec.ref {
            let frame = try accessibility.frame(ofRef: ref)
            guard frame.isUsable else {
                throw ComputerFailure("ELEMENT_NOT_ACTIONABLE", "element \(ref) has no scrollable frame")
            }
            point = CGPoint(x: frame.x + frame.width / 2, y: frame.y + frame.height / 2)
            if let element = try? accessibility.resolveRef(ref) {
                accessibility.focusElement(element)
            }
        } else {
            let frame = try accessibility.prepareTarget(bundleID: nil, pid: nil, windowID: nil)
            point = CGPoint(x: frame.x + frame.width / 2, y: frame.y + frame.height / 2)
        }
        if let point {
            postMouse(.mouseMoved, point: point, button: .left)
            usleep(10000)
        }
        let units: CGScrollEventUnit = spec.unit == "pixel" ? .pixel : .line
        guard let event = CGEvent(
            scrollWheelEvent2Source: source,
            units: units,
            wheelCount: 2,
            wheel1: Int32(-spec.dy),
            wheel2: Int32(spec.dx),
            wheel3: 0
        ) else {
            throw ComputerFailure("INPUT_FAILED", "could not create scroll event")
        }
        if let point {
            event.location = point
        }
        event.post(tap: .cghidEventTap)
        return [
            "scrolled": true,
            "dx": spec.dx,
            "dy": spec.dy,
            "unit": spec.unit,
            "ref": spec.ref ?? NSNull(),
        ]
    }

    func drag(_ spec: DragSpec) throws -> [String: Any] {
        try AccessibilityController.requireAccessibility()
        let frame = try accessibility.prepareTarget(
            bundleID: spec.bundleID,
            pid: spec.pid,
            windowID: spec.windowID
        )
        let from = try spec.from.resolve(in: frame)
        let to = try spec.to.resolve(in: frame)
        let screens = NSScreen.screens.map { ComputerRect($0.frame) }
        try ScreenGuard.requireOnScreen(from, screens: screens, label: "drag start")
        try ScreenGuard.requireOnScreen(to, screens: screens, label: "drag end")
        postMouse(.mouseMoved, point: from, button: .left)
        usleep(20000)
        guard let down = CGEvent(
            mouseEventSource: source,
            mouseType: .leftMouseDown,
            mouseCursorPosition: from,
            mouseButton: .left
        ) else {
            throw ComputerFailure("INPUT_FAILED", "could not create drag events")
        }
        down.setIntegerValueField(.mouseEventClickState, value: 1)
        down.post(tap: .cghidEventTap)
        let steps = max(1, spec.steps)
        let delay = spec.durationMilliseconds > 0
            ? spec.durationMilliseconds * 1000 / steps
            : 0
        for step in 1 ... steps {
            let fraction = Double(step) / Double(steps)
            let point = CGPoint(
                x: from.x + (to.x - from.x) * fraction,
                y: from.y + (to.y - from.y) * fraction
            )
            postMouse(.leftMouseDragged, point: point, button: .left)
            if delay > 0 {
                usleep(useconds_t(delay))
            }
        }
        guard let up = CGEvent(
            mouseEventSource: source,
            mouseType: .leftMouseUp,
            mouseCursorPosition: to,
            mouseButton: .left
        ) else {
            throw ComputerFailure("INPUT_FAILED", "could not create drag release")
        }
        up.setIntegerValueField(.mouseEventClickState, value: 1)
        up.post(tap: .cghidEventTap)
        return [
            "dragged": true,
            "from": ["x": Double(from.x), "y": Double(from.y)],
            "to": ["x": Double(to.x), "y": Double(to.y)],
            "steps": steps,
            "duration_ms": spec.durationMilliseconds,
            "target_window": AccessibilityController.dictionary(frame),
        ]
    }

    private func postMouse(_ type: CGEventType, point: CGPoint, button: CGMouseButton) {
        guard let event = CGEvent(
            mouseEventSource: source,
            mouseType: type,
            mouseCursorPosition: point,
            mouseButton: button
        ) else {
            return
        }
        event.post(tap: .cghidEventTap)
    }
}
