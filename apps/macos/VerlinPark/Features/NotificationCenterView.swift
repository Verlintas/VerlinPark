import SwiftUI

struct NotificationCenterView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss
    @State private var inbox = NotificationInboxModel()
    @State private var confirmClearRead = false
    @FocusState private var searchFocused: Bool

    private var groups: [NotificationDayGroup] {
        inbox.groups(from: visibleNotices)
    }

    private var visibleNotices: [AppNotice] {
        inbox.visible(in: model.notices, directory: model.sessionDirectory)
    }

    private var counts: [NotificationInboxFilter: Int] {
        inbox.counts(in: model.notices, directory: model.sessionDirectory)
    }

    private var selectedProjectName: String {
        guard let id = inbox.projectID,
              let project = model.projects.first(where: { $0.id == id })
        else { return L10n.t("All projects") }
        return project.name
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            header
            VPHairline()
            toolbar
            VPHairline()
            filterBar
            VPHairline()

            if visibleNotices.isEmpty {
                emptyState
            } else {
                ScrollView {
                    LazyVStack(alignment: .leading, spacing: 14) {
                        ForEach(groups) { group in
                            VStack(alignment: .leading, spacing: 6) {
                                HStack(spacing: 8) {
                                    Text(group.title.uppercased())
                                        .font(VPTheme.mono(8, weight: .semibold))
                                        .tracking(0.8)
                                        .foregroundStyle(VPTheme.textTertiary)
                                    Rectangle()
                                        .fill(VPTheme.hairline)
                                        .frame(height: 1)
                                    Text("\(group.notices.count)")
                                        .font(VPTheme.mono(8))
                                        .foregroundStyle(VPTheme.textTertiary)
                                }
                                ForEach(Array(group.notices.enumerated()),
                                        id: \.element.id)
                                { index, notice in
                                    notificationRow(notice)
                                        .vpAppear(index: index)
                                }
                            }
                        }
                    }
                    .padding(16)
                }
            }
        }
        .padding(22)
        .frame(width: 780, height: 620)
        .background { VPCrystalBackdrop() }
        .tint(VPTheme.accent)
        .onAppear {
            Task { await model.refreshSessionDirectory() }
        }
        .confirmationDialog(
            L10n.t("Clear read notifications?"),
            isPresented: $confirmClearRead
        ) {
            Button(L10n.t("Clear read"), role: .destructive) {
                withAnimation(VPMotion.snap) {
                    model.clearReadNotices()
                }
            }
            Button(L10n.t("Cancel"), role: .cancel) {}
        } message: {
            Text(L10n.t("Unread notifications are kept."))
        }
    }

    // MARK: Header

    private var header: some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 3) {
                Text("INBOX")
                    .font(VPTheme.mono(8, weight: .semibold))
                    .tracking(1.2)
                    .foregroundStyle(VPTheme.accent)
                Text(L10n.t("Notifications"))
                    .font(VPTheme.font(20, weight: .semibold, rounded: true))
                Text(L10n.t("Approvals, failed tasks, integration failures, and stopped sessions."))
                    .font(VPTheme.mono(8.5))
                    .foregroundStyle(VPTheme.textSecondary)
            }

            Spacer()

            VStack(alignment: .trailing, spacing: 7) {
                VPCountBadge(
                    count: model.unreadNoticeCount,
                    tint: VPTheme.accent,
                    compact: false
                )
                .contentTransition(.numericText())
                .animation(VPMotion.snap, value: model.unreadNoticeCount)

                Menu {
                    Button(L10n.t("All projects")) {
                        inbox.projectID = nil
                    }
                    Divider()
                    ForEach(model.projects) { project in
                        Button(project.name) {
                            inbox.projectID = project.id
                        }
                    }
                } label: {
                    HStack(spacing: 5) {
                        Image(systemName: "folder")
                            .font(.system(size: 8, weight: .semibold))
                        Text(selectedProjectName)
                            .font(VPTheme.mono(8))
                            .lineLimit(1)
                        Image(systemName: "chevron.down")
                            .font(.system(size: 6, weight: .bold))
                    }
                    .foregroundStyle(VPTheme.textSecondary)
                    .padding(.horizontal, 8)
                    .padding(.vertical, 4)
                    .background(VPTheme.surface, in: Capsule())
                    .overlay {
                        Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1)
                    }
                }
                .menuStyle(.borderlessButton)
                .fixedSize()
            }
        }
        .padding(.bottom, 14)
    }

    // MARK: Toolbar

    private var toolbar: some View {
        HStack(spacing: 8) {
            HStack(spacing: 7) {
                Image(systemName: "magnifyingglass")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                TextField(L10n.t("Search notifications"), text: $inbox.query)
                    .textFieldStyle(.plain)
                    .font(VPTheme.mono(9))
                    .focused($searchFocused)
                if !inbox.query.isEmpty {
                    Button {
                        inbox.query = ""
                    } label: {
                        Image(systemName: "xmark.circle.fill")
                            .font(.system(size: 9))
                            .foregroundStyle(VPTheme.textTertiary)
                    }
                    .buttonStyle(.plain)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
            .vpFieldSurface(radius: 8, focused: searchFocused)
            .frame(maxWidth: 260)

            Spacer()

            Button(L10n.t("Mark all read")) {
                withAnimation(VPMotion.snap) {
                    model.markAllNoticesRead()
                }
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(model.unreadNoticeCount == 0)

            Button(L10n.t("Clear read")) {
                confirmClearRead = true
            }
            .buttonStyle(.bordered)
            .controlSize(.small)
            .disabled(!model.notices.contains(where: \.isRead))

            Button(L10n.t("Close")) {
                dismiss()
            }
            .keyboardShortcut(.cancelAction)
        }
        .padding(.vertical, 10)
    }

    // MARK: Filters

    private var filterBar: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: 4) {
                ForEach(NotificationInboxFilter.allCases) { filter in
                    filterChip(filter)
                }
            }
            .padding(.vertical, 8)
        }
    }

    private func filterChip(_ filter: NotificationInboxFilter) -> some View {
        let selected = inbox.filter == filter
        let count = counts[filter] ?? 0
        return Button {
            withAnimation(VPMotion.snap) {
                inbox.filter = filter
            }
        } label: {
            HStack(spacing: 5) {
                if let icon = filter.icon {
                    Image(systemName: icon)
                        .font(.system(size: 7, weight: .semibold))
                }
                Text(filter.title)
                    .font(VPTheme.mono(8, weight: .medium))
                if count > 0 {
                    Text("\(count)")
                        .font(VPTheme.mono(7, weight: .semibold))
                        .foregroundStyle(selected ? VPTheme.accent : VPTheme.textTertiary)
                }
            }
            .foregroundStyle(selected ? VPTheme.textPrimary : VPTheme.textSecondary)
            .padding(.horizontal, 8)
            .padding(.vertical, 5)
            .background {
                if selected {
                    Capsule()
                        .fill(VPTheme.accent.opacity(0.10))
                        .overlay {
                            Capsule().strokeBorder(VPTheme.accent.opacity(0.24), lineWidth: 1)
                        }
                } else {
                    Capsule().fill(VPTheme.surface.opacity(0.45))
                }
            }
        }
        .buttonStyle(VPPressableButtonStyle(scale: 0.97))
    }

    // MARK: Rows and empty state

    private func notificationRow(_ notice: AppNotice) -> some View {
        let project = model.projects.first { $0.id == notice.projectId }
        let directory = model.sessionDirectory[notice.sessionId]
        return HStack(alignment: .top, spacing: 10) {
            VPStatusDot(
                color: notice.isRead ? VPTheme.textTertiary.opacity(0.35) : notificationColor(notice.kind),
                pulsing: !notice.isRead,
                size: 7
            )
            .padding(.top, 6)

            ZStack {
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(notificationColor(notice.kind).opacity(notice.isRead ? 0.06 : 0.13))
                    .frame(width: 26, height: 26)
                Image(systemName: notificationIcon(notice.kind))
                    .font(.system(size: 10, weight: .semibold))
                    .foregroundStyle(notificationColor(notice.kind).opacity(notice.isRead ? 0.55 : 1))
            }

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(notice.title)
                        .font(VPTheme.font(11, weight: notice.isRead ? .regular : .semibold))
                        .foregroundStyle(notice.isRead ? VPTheme.textSecondary : VPTheme.textPrimary)
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    if let project {
                        Text(project.name)
                            .font(VPTheme.mono(7))
                            .foregroundStyle(VPTheme.textTertiary)
                            .lineLimit(1)
                    }
                    Text(VPRelativeTime.label(notice.createdAt))
                        .font(VPTheme.mono(7.5))
                        .foregroundStyle(VPTheme.textTertiary)
                }

                Text(notice.detail)
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(2)

                if let goal = directory?.goal {
                    HStack(spacing: 4) {
                        Image(systemName: "bubble.left")
                            .font(.system(size: 7))
                        Text(goal)
                            .lineLimit(1)
                    }
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
                }
            }

            Menu {
                if !notice.isRead {
                    Button(L10n.t("Mark read")) {
                        model.markNoticeRead(notice.id)
                    }
                }
                Button(L10n.t("Open")) {
                    Task {
                        await model.openNotice(notice)
                        dismiss()
                    }
                }
                Divider()
                Button(L10n.t("Remove"), role: .destructive) {
                    withAnimation(VPMotion.snap) {
                        model.removeNotice(notice.id)
                    }
                }
            } label: {
                Image(systemName: "ellipsis")
                    .font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(VPTheme.textTertiary)
                    .frame(width: 22, height: 22)
                    .background(VPTheme.surface.opacity(0.5), in: Circle())
            }
            .menuStyle(.borderlessButton)
            .fixedSize()
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            notice.isRead ? VPTheme.surface.opacity(0.45) : VPTheme.surface,
            in: RoundedRectangle(cornerRadius: 10, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(
                    notice.isRead ? VPTheme.hairline : notificationColor(notice.kind).opacity(0.22),
                    lineWidth: 1
                )
        }
        .contentShape(Rectangle())
        .onTapGesture {
            Task {
                await model.openNotice(notice)
                dismiss()
            }
        }
        .vpHoverLift(
            scale: 1.003,
            lift: 1,
            glow: notice.isRead ? .white : notificationColor(notice.kind)
        )
    }

    private var emptyState: some View {
        let emptyInbox = model.notices.isEmpty
        return VStack(spacing: 9) {
            VPFloatingSymbol(name: "bell.slash", size: 24, tint: .secondary)
            Text(L10n.t(emptyInbox ? "No notifications yet." : "No matching notifications"))
                .font(VPTheme.font(12, weight: .medium))
                .foregroundStyle(VPTheme.textSecondary)
            Text(
                L10n.t(
                    emptyInbox
                        ? "Approvals, questions, task failures, and integration conflicts will appear here."
                        : "Try a different filter, project, or search."
                )
            )
            .font(VPTheme.mono(8.5))
            .foregroundStyle(VPTheme.textTertiary)
            if !emptyInbox,
               inbox.filter != .all || !inbox.query.isEmpty || inbox.projectID != nil
            {
                Button(L10n.t("Clear filters")) {
                    withAnimation(VPMotion.snap) {
                        inbox = NotificationInboxModel()
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .padding(.top, 3)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private func notificationIcon(_ kind: String) -> String {
        switch kind {
        case "question": "questionmark.bubble"
        case "approval": "checkmark.shield"
        case "task": "list.bullet.rectangle"
        case "integration": "arrow.triangle.merge"
        default: "exclamationmark.triangle"
        }
    }

    private func notificationColor(_ kind: String) -> Color {
        switch kind {
        case "question": VPTheme.info
        case "approval": VPTheme.warning
        case "task": VPTheme.danger
        case "integration": VPTheme.plan
        default: VPTheme.danger
        }
    }
}
