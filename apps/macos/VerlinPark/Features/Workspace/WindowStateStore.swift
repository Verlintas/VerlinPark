import AppKit
import Foundation
import Observation

struct WindowFramePreference: Codable, Equatable, Sendable {
    var x: Double
    var y: Double
    var width: Double
    var height: Double

    init(_ rect: CGRect) {
        x = rect.origin.x
        y = rect.origin.y
        width = rect.size.width
        height = rect.size.height
    }

    var rect: CGRect {
        CGRect(x: x, y: y, width: width, height: height)
    }
}

/// Keeps a restored window on a real screen and above the minimum usable size.
/// Screen frames use the same bottom-left coordinate space as `NSWindow.frame`.
enum WindowFramePolicy {
    static let minimumSize = CGSize(width: 1120, height: 720)

    static func sanitized(_ proposed: CGRect, screens: [CGRect]) -> CGRect {
        guard !screens.isEmpty else {
            return CGRect(
                origin: proposed.origin,
                size: CGSize(
                    width: max(proposed.width, minimumSize.width),
                    height: max(proposed.height, minimumSize.height)
                )
            )
        }

        let best = screens.max { lhs, rhs in
            lhs.intersection(proposed).area < rhs.intersection(proposed).area
        } ?? screens[0]
        let hasIntersection = screens.contains { $0.intersection(proposed).area > 0 }
        let target = hasIntersection ? best : screens[0]

        var width = min(max(proposed.width, minimumSize.width), target.width)
        var height = min(max(proposed.height, minimumSize.height), target.height)
        width = max(1, width)
        height = max(1, height)

        let x: CGFloat
        let y: CGFloat
        if hasIntersection {
            x = min(max(proposed.origin.x, target.minX), target.maxX - width)
            y = min(max(proposed.origin.y, target.minY), target.maxY - height)
        } else {
            x = target.midX - width / 2
            y = target.midY - height / 2
        }

        return CGRect(x: x, y: y, width: width, height: height)
    }
}

private extension CGRect {
    var area: CGFloat {
        guard !isNull, !isInfinite else { return 0 }
        return max(0, width) * max(0, height)
    }
}

@MainActor
@Observable
final class WindowStateStore {
    static let storageKey = "verlinpark.window.frame"

    private(set) var savedFrame: WindowFramePreference?
    private weak var window: NSWindow?
    private var observers: [NSObjectProtocol] = []
    private var persistTask: Task<Void, Never>?
    private var restored = false
    private var screenObserver: NSObjectProtocol?

    init() {
        if let data = UserDefaults.standard.data(forKey: Self.storageKey),
           let decoded = try? JSONDecoder().decode(WindowFramePreference.self, from: data)
        {
            savedFrame = decoded
        }
        screenObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didChangeScreenParametersNotification,
            object: nil,
            queue: .main
        ) { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.handleScreenChange()
            }
        }
    }

    private func handleScreenChange() {
        guard let window else { return }
        let sanitized = WindowFramePolicy.sanitized(
            window.frame,
            screens: NSScreen.screens.map(\.visibleFrame)
        )
        if window.frame != sanitized {
            window.setFrame(sanitized, display: true)
        }
        persistNow()
    }

    func install(_ window: NSWindow) {
        guard self.window !== window else { return }
        self.window = window
        removeObservers()

        if !restored {
            if let savedFrame {
                let screens = NSScreen.screens.map(\.visibleFrame)
                window.setFrame(
                    WindowFramePolicy.sanitized(savedFrame.rect, screens: screens),
                    display: true
                )
            }
            restored = true
        }

        let center = NotificationCenter.default
        for name in [
            NSWindow.didMoveNotification,
            NSWindow.didResizeNotification,
            NSWindow.willCloseNotification,
        ] {
            let token = center.addObserver(
                forName: name,
                object: window,
                queue: .main
            ) { [weak self] _ in
                Task { @MainActor [weak self] in
                    self?.schedulePersist()
                }
            }
            observers.append(token)
        }
    }

    func reset() {
        savedFrame = nil
        UserDefaults.standard.removeObject(forKey: Self.storageKey)
        guard let window else { return }
        let screens = NSScreen.screens.map(\.visibleFrame)
        let target = screens.first ?? CGRect(x: 0, y: 0, width: 1320, height: 860)
        let frame = CGRect(
            x: target.midX - 660,
            y: target.midY - 430,
            width: min(1320, target.width),
            height: min(860, target.height)
        )
        window.setFrame(frame, display: true)
        persistNow()
    }

    func persistNow() {
        persistTask?.cancel()
        persistTask = nil
        guard let window else { return }
        let frame = WindowFramePolicy.sanitized(
            window.frame,
            screens: NSScreen.screens.map(\.visibleFrame)
        )
        let preference = WindowFramePreference(frame)
        savedFrame = preference
        guard let data = try? JSONEncoder().encode(preference) else { return }
        UserDefaults.standard.set(data, forKey: Self.storageKey)
    }

    private func schedulePersist() {
        persistTask?.cancel()
        persistTask = Task { [weak self] in
            try? await Task.sleep(for: .milliseconds(250))
            guard !Task.isCancelled else { return }
            self?.persistNow()
        }
    }

    private func removeObservers() {
        for observer in observers {
            NotificationCenter.default.removeObserver(observer)
        }
        observers.removeAll()
    }
}
