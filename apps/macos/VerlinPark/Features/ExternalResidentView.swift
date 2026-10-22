import SwiftUI

/// Placeholder for the future NovaBAIC Aviiya bridge. No local chat runtime is
/// implemented here on purpose.
struct ExternalResidentView: View {
    private let bridge = UnavailableResidentAgent()

    var body: some View {
        VStack(spacing: 0) {
            header
            Divider().opacity(0.35)

            VStack(spacing: 16) {
                ZStack {
                    Circle()
                        .fill(
                            LinearGradient(
                                colors: [.green.opacity(0.55), .teal.opacity(0.35)],
                                startPoint: .topLeading,
                                endPoint: .bottomTrailing
                            )
                        )
                        .frame(width: 64, height: 64)
                    Image(systemName: "sparkles")
                        .font(.system(size: 22, weight: .bold))
                        .foregroundStyle(.white.opacity(0.9))
                }

                Text("Aviiya lives in NovaBAIC")
                    .font(.system(size: 20, weight: .semibold, design: .rounded))
                Text(
                    "The resident surface is reserved for the external Aviiya bridge. This build does not implement a local Aviiya runtime, memory, or persona."
                )
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 420)

                HStack(spacing: 8) {
                    Label("external", systemImage: "link")
                    Label("shared memory · future", systemImage: "brain")
                    Label("shared tools · future", systemImage: "wrench.and.screwdriver")
                }
                .font(.system(size: 9, design: .monospaced))
                .foregroundStyle(.tertiary)

                Link(destination: URL(string: "https://github.com/Verlintas/NovaBAIC")!) {
                    Label("NovaBAIC", systemImage: "arrow.up.right.square")
                }
                .font(.system(size: 11))
                .buttonStyle(.plain)
                .padding(.top, 4)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(32)
        }
    }

    private var header: some View {
        HStack(spacing: 12) {
            ZStack {
                Circle()
                    .fill(
                        LinearGradient(
                            colors: [.green.opacity(0.8), .teal.opacity(0.5)],
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        )
                    )
                    .frame(width: 34, height: 34)
                Image(systemName: "sparkles")
                    .font(.system(size: 13, weight: .bold))
                    .foregroundStyle(.white)
            }

            VStack(alignment: .leading, spacing: 2) {
                HStack(spacing: 7) {
                    Text(bridge.displayName)
                        .font(.system(size: 16, weight: .semibold))
                    Text("EXTERNAL · NOVABAIC")
                        .font(.system(size: 8, weight: .semibold, design: .monospaced))
                        .padding(.horizontal, 5)
                        .padding(.vertical, 2)
                        .background(.white.opacity(0.06), in: Capsule())
                        .foregroundStyle(.secondary)
                }
                Text("resident bridge · not connected in this build")
                    .font(.system(size: 9, design: .monospaced))
                    .foregroundStyle(.secondary)
            }

            Spacer()

            StatusPill(status: bridge.status.rawValue)
        }
        .padding(.horizontal, 22)
        .padding(.vertical, 14)
    }
}
