import Foundation

enum NotificationInboxFilter: String, CaseIterable, Identifiable, Sendable {
    case all
    case unread
    case approvals
    case questions
    case tasks
    case integration
    case sessions

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .all: L10n.t("All")
        case .unread: L10n.t("Unread")
        case .approvals: L10n.t("Approvals")
        case .questions: L10n.t("Questions")
        case .tasks: L10n.t("Tasks")
        case .integration: L10n.t("Integration")
        case .sessions: L10n.t("Sessions")
        }
    }

    var icon: String? {
        switch self {
        case .all: nil
        case .unread: "circle.fill"
        case .approvals: "checkmark.shield"
        case .questions: "questionmark.bubble"
        case .tasks: "list.bullet.rectangle"
        case .integration: "arrow.triangle.merge"
        case .sessions: "bubble.left.and.exclamationmark.bubble.right"
        }
    }

    func includes(_ notice: AppNotice) -> Bool {
        switch self {
        case .all: true
        case .unread: !notice.isRead
        case .approvals: notice.kind == "approval"
        case .questions: notice.kind == "question"
        case .tasks: notice.kind == "task"
        case .integration: notice.kind == "integration"
        case .sessions: notice.kind == "session"
        }
    }
}

struct NotificationDayGroup: Identifiable, Equatable, Sendable {
    let id: String
    let title: String
    let notices: [AppNotice]
}

struct NotificationInboxModel: Equatable, Sendable {
    var filter: NotificationInboxFilter = .all
    var query = ""
    var projectID: String?

    func visible(
        in notices: [AppNotice],
        directory: [String: SessionDirectoryEntry]
    ) -> [AppNotice] {
        scoped(in: notices, directory: directory).filter { filter.includes($0) }
    }

    func counts(
        in notices: [AppNotice],
        directory: [String: SessionDirectoryEntry]
    ) -> [NotificationInboxFilter: Int] {
        let scoped = scoped(in: notices, directory: directory)
        return Dictionary(
            uniqueKeysWithValues: NotificationInboxFilter.allCases.map { filter in
                (filter, scoped.count(where: { filter.includes($0) }))
            }
        )
    }

    private func scoped(
        in notices: [AppNotice],
        directory: [String: SessionDirectoryEntry]
    ) -> [AppNotice] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        return notices
            .filter { notice in
                if let projectID, notice.projectId != projectID {
                    return false
                }
                guard !trimmed.isEmpty else { return true }
                let goal = directory[notice.sessionId]?.goal ?? ""
                return notice.title.localizedCaseInsensitiveContains(trimmed)
                    || notice.detail.localizedCaseInsensitiveContains(trimmed)
                    || goal.localizedCaseInsensitiveContains(trimmed)
            }
            .sorted { lhs, rhs in
                if lhs.createdAt == rhs.createdAt {
                    return lhs.id > rhs.id
                }
                return lhs.createdAt > rhs.createdAt
            }
    }

    func groups(
        from notices: [AppNotice],
        calendar: Calendar = .current,
        now: Date = Date()
    ) -> [NotificationDayGroup] {
        var buckets: [NotificationBucket: [AppNotice]] = [:]
        for notice in notices {
            let date = Self.parse(notice.createdAt)
            let bucket = Self.bucket(for: date, calendar: calendar, now: now)
            buckets[bucket, default: []].append(notice)
        }
        return NotificationBucket.ordered.compactMap { bucket in
            guard let notices = buckets[bucket], !notices.isEmpty else { return nil }
            return NotificationDayGroup(
                id: bucket.rawValue,
                title: bucket.title,
                notices: notices
            )
        }
    }

    private enum NotificationBucket: String, CaseIterable {
        case today
        case yesterday
        case week
        case earlier

        static let ordered: [NotificationBucket] = [.today, .yesterday, .week, .earlier]

        var title: String {
            switch self {
            case .today: L10n.t("Today")
            case .yesterday: L10n.t("Yesterday")
            case .week: L10n.t("Earlier this week")
            case .earlier: L10n.t("Earlier")
            }
        }
    }

    private static func bucket(
        for date: Date,
        calendar: Calendar,
        now: Date
    ) -> NotificationBucket {
        if calendar.isDate(date, inSameDayAs: now) {
            return .today
        }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now),
           calendar.isDate(date, inSameDayAs: yesterday)
        {
            return .yesterday
        }
        let weekAgo = calendar.date(byAdding: .day, value: -7, to: now) ?? now
        return date >= weekAgo ? .week : .earlier
    }

    private static func parse(_ iso: String) -> Date {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: iso) {
            return date
        }
        return ISO8601DateFormatter().date(from: iso) ?? Date.distantPast
    }
}
