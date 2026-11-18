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
    var onResolveApproval: ((String, Bool) -> Void)?
    /// High-risk approvals must be reviewed in the app, where risk, tool, and
    /// target are visible. This callback selects the Inbox instead of
    /// resolving the request from a notification action.
    var onReviewApproval: ((String) -> Void)?
    private var requestedAuthorization = false
    nonisolated static let pendingApprovalReviewKey = "verlinpark.pendingApprovalReview"

    nonisolated static func approvalID(from noticeID: String) -> String? {
        guard noticeID.hasPrefix("approval-") else { return nil }
        let id = String(noticeID.dropFirst("approval-".count))
        return id.isEmpty ? nil : id
    }

    func requestAuthorization() {
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        let approve = UNNotificationAction(
            identifier: "verlinpark.approve",
            title: "Approve",
            options: []
        )
        let deny = UNNotificationAction(
            identifier: "verlinpark.deny",
            title: "Deny",
            options: [.destructive]
        )
        let review = UNNotificationAction(
            identifier: "verlinpark.review",
            title: "Review in VerlinPark",
            options: [.foreground]
        )
        center.setNotificationCategories([
            UNNotificationCategory(
                identifier: NotificationApprovalPolicy.inlineCategory,
                actions: [approve, deny],
                intentIdentifiers: [],
                options: []
            ),
            // No Approve/Deny action here: the request only opens the app.
            UNNotificationCategory(
                identifier: NotificationApprovalPolicy.reviewCategory,
                actions: [review],
                intentIdentifiers: [],
                options: []
            ),
        ])
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
        if let approvalID = Self.approvalID(from: notice.id) {
            let risk = notice.approvalRisk
            content.categoryIdentifier = NotificationApprovalPolicy.categoryIdentifier(risk: risk)
            content.userInfo["approval_id"] = approvalID
            content.userInfo["approval_risk"] = risk ?? ""
        }
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
        let userInfo = response.notification.request.content.userInfo
        let sessionID = userInfo["session_id"] as? String
        let approvalID = userInfo["approval_id"] as? String
        let risk = userInfo["approval_risk"] as? String
        let action = response.actionIdentifier
        Task { @MainActor in
            if action == "verlinpark.approve" || action == "verlinpark.deny",
               let approvalID
            {
                // Re-check at action time so a stale or misregistered category
                // can never approve a high-risk request blindly.
                guard NotificationApprovalPolicy.allowsInlineResolution(risk: risk) else {
                    NSApp.activate(ignoringOtherApps: true)
                    self.routeToReview(approvalID)
                    return
                }
                self.onResolveApproval?(approvalID, action == "verlinpark.approve")
                return
            }
            NSApp.activate(ignoringOtherApps: true)
            if action == "verlinpark.review", let approvalID {
                self.routeToReview(approvalID)
                return
            }
            if let approvalID,
               !NotificationApprovalPolicy.allowsInlineResolution(risk: risk)
            {
                self.routeToReview(approvalID)
                return
            }
            if let sessionID {
                self.onOpenSession?(sessionID)
            }
        }
        completionHandler()
    }

    /// A notification action can arrive before `AppModel.start` wires the
    /// callback (for example when the action launches the app). Persist the
    /// request so the first connected app session opens the Inbox.
    private func routeToReview(_ approvalID: String) {
        if let onReviewApproval {
            onReviewApproval(approvalID)
        } else {
            UserDefaults.standard.set(approvalID, forKey: Self.pendingApprovalReviewKey)
        }
    }
}
