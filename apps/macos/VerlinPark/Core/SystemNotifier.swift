import AppKit
import UserNotifications

enum SystemNotificationPreference {
    static let key = "verlinpark.systemNotifications"
}

/// Local macOS notifications for the same events that feed the in-app Rail.
/// Notifications are posted only while VerlinPark is in the background, so the
/// foreground UX stays in the app and does not duplicate the Rail.
@MainActor
final class SystemNotifier: NSObject, UNUserNotificationCenterDelegate {
    static let shared = SystemNotifier()

    var onOpenSession: ((String) -> Void)?
    private var requestedAuthorization = false

    func requestAuthorization() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        guard !requestedAuthorization else { return }
        requestedAuthorization = true
        center.requestAuthorization(options: [.alert, .sound]) { _, _ in }
    }

    func notify(notice: AppNotice, sessionGoal: String?) {
        let enabled =
            UserDefaults.standard.object(forKey: SystemNotificationPreference.key) as? Bool
                ?? true
        guard enabled, !NSApp.isActive else { return }
        requestAuthorization()
        let content = UNMutableNotificationContent()
        content.title = notice.title
        if let sessionGoal, !sessionGoal.isEmpty {
            content.subtitle = String(sessionGoal.prefix(120))
        }
        content.body = notice.detail
        content.sound = .default
        content.userInfo = ["session_id": notice.sessionId]
        let request = UNNotificationRequest(
            identifier: "verlinpark-\(notice.id)",
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().add(request)
    }

    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter,
        willPresent _: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([])
    }

    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        let sessionID = response.notification.request.content.userInfo["session_id"] as? String
        Task { @MainActor in
            NSApp.activate(ignoringOtherApps: true)
            if let sessionID {
                self.onOpenSession?(sessionID)
            }
        }
        completionHandler()
    }
}
