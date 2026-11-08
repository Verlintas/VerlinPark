import SwiftUI

enum VPToastKind: String, CaseIterable, Equatable {
    case success
    case info
    case warning
    case error

    var icon: String {
        switch self {
        case .success: "checkmark.circle.fill"
        case .info: "info.circle.fill"
        case .warning: "exclamationmark.triangle.fill"
        case .error: "xmark.octagon.fill"
        }
    }

    var tint: Color {
        switch self {
        case .success: .green
        case .info: .cyan
        case .warning: .orange
        case .error: .red
        }
    }
}

struct VPToast: Identifiable, Equatable {
    let id: String
    let title: String
    let detail: String
    let kind: VPToastKind
    let duration: TimeInterval
}

struct VPToastStack: View {
    let toasts: [VPToast]
    let dismiss: (String) -> Void

    var body: some View {
        VStack(alignment: .trailing, spacing: 8) {
            ForEach(toasts.suffix(4)) { toast in
                VPToastCard(toast: toast) {
                    dismiss(toast.id)
                }
                .transition(
                    .move(edge: .trailing)
                        .combined(with: .opacity)
                        .combined(with: .scale(scale: 0.96))
                )
            }
        }
        .frame(maxWidth: 360, alignment: .trailing)
        .animation(VPMotion.smooth, value: toasts.count)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Notifications")
    }
}

private struct VPToastCard: View {
    let toast: VPToast
    let dismiss: () -> Void
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var progress: CGFloat = 1

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            HStack(alignment: .top, spacing: 9) {
                Image(systemName: toast.kind.icon)
                    .font(.system(size: 13, weight: .semibold))
                    .foregroundStyle(toast.kind.tint)
                    .frame(width: 24, height: 24)
                    .background(toast.kind.tint.opacity(0.12), in: RoundedRectangle(cornerRadius: 7))

                VStack(alignment: .leading, spacing: 2) {
                    Text(toast.title)
                        .font(.system(size: 11, weight: .semibold))
                    Text(toast.detail)
                        .font(.system(size: 9, design: .monospaced))
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }

                Spacer(minLength: 6)
                Button(action: dismiss) {
                    Image(systemName: "xmark")
                        .font(.system(size: 8, weight: .bold))
                        .foregroundStyle(.tertiary)
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.88))
                .help("Dismiss")
            }
            .padding(.horizontal, 11)
            .padding(.vertical, 9)

            GeometryReader { proxy in
                Rectangle()
                    .fill(
                        LinearGradient(
                            colors: [toast.kind.tint.opacity(0.15), toast.kind.tint.opacity(0.65)],
                            startPoint: .leading,
                            endPoint: .trailing
                        )
                    )
                    .frame(width: proxy.size.width * progress, alignment: .leading)
            }
            .frame(height: 2)
        }
        .frame(width: 340, alignment: .leading)
        .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(toast.kind.tint.opacity(0.22), lineWidth: 1)
        }
        .shadow(color: .black.opacity(0.30), radius: 18, y: 8)
        .clipShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .onAppear {
            guard !reduceMotion else {
                progress = 0
                return
            }
            withAnimation(.linear(duration: toast.duration)) {
                progress = 0
            }
        }
    }
}
