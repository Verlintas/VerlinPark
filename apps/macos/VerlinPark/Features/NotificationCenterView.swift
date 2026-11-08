import SwiftUI

struct NotificationCenterView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("INBOX")
                        .font(VPTheme.mono(8, weight: .semibold))
                        .tracking(1.2)
                        .foregroundStyle(VPTheme.accent)
                    Text("Notifications")
                        .font(VPTheme.font(20, weight: .semibold, rounded: true))
                    Text("Approvals, failed tasks, integration failures, and stopped sessions.")
                        .font(VPTheme.mono(8.5))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                Spacer()
                VPCountBadge(count: model.unreadNoticeCount, tint: VPTheme.accent, compact: false)
                    .contentTransition(.numericText())
                    .animation(VPMotion.snap, value: model.unreadNoticeCount)
            }

            HStack {
                Button("Mark all read") {
                    withAnimation(VPMotion.snap) {
                        model.markAllNoticesRead()
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.unreadNoticeCount == 0)
                Button("Clear") {
                    withAnimation(VPMotion.snap) {
                        model.clearNotices()
                    }
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.notices.isEmpty)
                Spacer()
                Button("Close") {
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
            }

            if model.notices.isEmpty {
                VStack(spacing: 6) {
                    VPFloatingSymbol(name: "bell.slash", size: 22, tint: .secondary)
                    Text("No notifications yet.")
                        .font(VPTheme.mono(9))
                        .foregroundStyle(VPTheme.textSecondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 6) {
                        ForEach(Array(model.notices.enumerated()), id: \.element.id) { index, notice in
                            Button {
                                Task {
                                    await model.openNotice(notice)
                                    dismiss()
                                }
                            } label: {
                                NotificationRow(notice: notice)
                            }
                            .buttonStyle(VPPressableButtonStyle(scale: 0.99))
                            .transition(.vpRise)
                            .vpAppear(index: index)
                        }
                    }
                    .animation(VPMotion.smooth, value: model.notices.count)
                }
            }
        }
        .padding(22)
        .frame(width: 680, height: 540)
        .background(VPTheme.canvas)
        .tint(VPTheme.accent)
        .onAppear {
            Task { await model.refreshSessionDirectory() }
        }
    }
}

private struct NotificationRow: View {
    @Environment(AppModel.self) private var model
    let notice: AppNotice

    var body: some View {
        HStack(alignment: .top, spacing: 9) {
            VPStatusDot(
                color: notice.isRead ? .secondary.opacity(0.35) : color,
                pulsing: !notice.isRead,
                size: 7
            )
            .padding(.top, 5)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(notice.title)
                        .font(VPTheme.font(11, weight: .medium))
                        .foregroundStyle(VPTheme.textPrimary)
                    Spacer()
                    Text(VPRelativeTime.label(notice.createdAt))
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                }
                Text(notice.detail)
                    .font(VPTheme.mono(9))
                    .foregroundStyle(VPTheme.textSecondary)
                    .lineLimit(2)
                if let directory = model.sessionDirectory[notice.sessionId] {
                    Text(directory.goal)
                        .font(VPTheme.mono(8))
                        .foregroundStyle(VPTheme.textTertiary)
                        .lineLimit(1)
                }
            }
            Image(systemName: icon)
                .font(.system(size: 11))
                .foregroundStyle(notice.isRead ? Color.secondary : color)
                .frame(width: 18)
        }
        .padding(9)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(
            notice.isRead ? VPTheme.surface.opacity(0.45) : VPTheme.surface,
            in: RoundedRectangle(cornerRadius: 10, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(
                    notice.isRead ? VPTheme.hairline : color.opacity(0.16),
                    lineWidth: 1
                )
        }
        .vpHoverLift(scale: 1.005, lift: 1, glow: notice.isRead ? .white : color)
    }

    private var icon: String {
        switch notice.kind {
        case "question": "questionmark.bubble"
        case "approval": "checkmark.shield"
        case "task": "list.bullet.rectangle"
        case "integration": "arrow.triangle.merge"
        default: "exclamationmark.triangle"
        }
    }

    private var color: Color {
        switch notice.kind {
        case "question": VPTheme.info
        case "approval": VPTheme.warning
        case "task": VPTheme.danger
        case "integration": VPTheme.plan
        default: VPTheme.danger
        }
    }
}
