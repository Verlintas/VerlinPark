import SwiftUI

struct NotificationCenterView: View {
    @Environment(AppModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(alignment: .firstTextBaseline) {
                VStack(alignment: .leading, spacing: 3) {
                    Text("Notifications")
                        .font(.title3.weight(.semibold))
                    Text("Approvals, failed tasks, integration failures, and stopped sessions.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Text("\(model.unreadNoticeCount) unread")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.tertiary)
            }

            HStack {
                Button("Mark all read") {
                    model.markAllNoticesRead()
                }
                .buttonStyle(.bordered)
                .controlSize(.small)
                .disabled(model.unreadNoticeCount == 0)
                Button("Clear") {
                    model.clearNotices()
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
                    Image(systemName: "bell.slash")
                        .font(.system(size: 22, weight: .light))
                        .foregroundStyle(.tertiary)
                    Text("No notifications yet.")
                        .font(.system(size: 10, design: .monospaced))
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            } else {
                ScrollView {
                    LazyVStack(spacing: 6) {
                        ForEach(model.notices) { notice in
                            Button {
                                Task {
                                    await model.openNotice(notice)
                                    dismiss()
                                }
                            } label: {
                                NotificationRow(notice: notice)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
            }
        }
        .padding(20)
        .frame(width: 650, height: 500)
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
            Circle()
                .fill(notice.isRead ? Color.secondary.opacity(0.25) : color)
                .frame(width: 7, height: 7)
                .padding(.top, 5)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(notice.title)
                        .font(.system(size: 11, weight: .medium))
                    Spacer()
                    Text(notice.createdAt)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
                }
                Text(notice.detail)
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .lineLimit(2)
                if let directory = model.sessionDirectory[notice.sessionId] {
                    Text(directory.goal)
                        .font(.system(size: 8, design: .monospaced))
                        .foregroundStyle(.tertiary)
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
            notice.isRead ? Color.white.opacity(0.02) : Color.white.opacity(0.045),
            in: RoundedRectangle(cornerRadius: 9)
        )
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
        case "question": .cyan
        case "approval": .orange
        case "task": .red
        case "integration": .purple
        default: .red
        }
    }
}
