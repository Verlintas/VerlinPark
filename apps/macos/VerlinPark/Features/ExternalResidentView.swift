import SwiftUI

/// Placeholder for the future NovaBAIC Aviiya bridge. No local chat runtime is
/// implemented here on purpose.
struct ExternalResidentView: View {
    private let bridge = UnavailableResidentAgent()

    var body: some View {
        VStack(spacing: 0) {
            header
            VPHairline()

            ScrollView {
                VStack(alignment: .leading, spacing: 26) {
                    hero
                    bridgeSection
                    bridgeGrid
                    footnote
                }
                .padding(.horizontal, 40)
                .padding(.vertical, 34)
                .frame(maxWidth: 820, alignment: .leading)
                .frame(maxWidth: .infinity)
            }
        }
    }

    // MARK: Header

    private var header: some View {
        HStack(spacing: 9) {
            Text("RESIDENT")
                .font(VPTheme.mono(8, weight: .semibold))
                .tracking(1.0)
                .foregroundStyle(VPTheme.textTertiary)

            Rectangle()
                .fill(VPTheme.hairline)
                .frame(width: 1, height: 9)

            Text("NovaBAIC bridge")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)

            Spacer()

            StatusPill(status: bridge.status.rawValue)
        }
        .padding(.horizontal, 24)
        .padding(.vertical, 15)
    }

    // MARK: Hero

    private var hero: some View {
        HStack(alignment: .center, spacing: 28) {
            ResidentOrbit()

            VStack(alignment: .leading, spacing: 10) {
                Text("RESIDENT SURFACE")
                    .font(VPTheme.mono(8.5, weight: .semibold))
                    .tracking(1.4)
                    .foregroundStyle(VPTheme.resident)

                Text("Aviiya lives in\nNovaBAIC.")
                    .font(.system(size: 38, weight: .semibold, design: .serif))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineSpacing(1)

                Text(
                    "VerlinPark reserves this surface for the external NovaBAIC resident bridge. This build does not implement a local Aviiya runtime, memory, or persona."
                )
                .font(VPTheme.font(12))
                .foregroundStyle(VPTheme.textSecondary)
                .lineSpacing(3)
                .frame(maxWidth: 470, alignment: .leading)

                HStack(spacing: 7) {
                    residentChip("external", icon: "link")
                    residentChip("shared memory", icon: "brain")
                    residentChip("shared tools", icon: "wrench.and.screwdriver")
                }
                .padding(.top, 3)
            }
            .vpAppear(index: 0, animation: VPMotion.emphasizedSpring)

            Spacer(minLength: 0)
        }
    }

    // MARK: Bridge explanation

    private var bridgeSection: some View {
        VStack(alignment: .leading, spacing: 11) {
            VPKicker(text: "Reserved seam", trailing: "not implemented")

            Text(
                "The bridge is a contract, not a second assistant. VerlinPark's project org stays independent; identity, memory, tools, and cross-device sessions will be supplied by NovaBAIC."
            )
            .font(VPTheme.font(11.5))
            .foregroundStyle(VPTheme.textSecondary)
            .lineSpacing(3)
            .fixedSize(horizontal: false, vertical: true)
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(
                VPTheme.resident.opacity(0.045),
                in: RoundedRectangle(cornerRadius: 12, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(VPTheme.resident.opacity(0.14), lineWidth: 1)
            }
        }
        .vpAppear(index: 1)
    }

    private var bridgeGrid: some View {
        LazyVGrid(
            columns: [
                GridItem(.flexible(), spacing: 10),
                GridItem(.flexible(), spacing: 10),
            ],
            spacing: 10
        ) {
            bridgeCard(
                icon: "person.crop.circle",
                title: "Identity",
                detail: "The Aviiya persona is owned by NovaBAIC, never by this app.",
                index: 0
            )
            bridgeCard(
                icon: "brain",
                title: "Memory",
                detail: "Shared resident memory across devices and surfaces.",
                index: 1
            )
            bridgeCard(
                icon: "wrench.and.screwdriver",
                title: "Tools",
                detail: "A shared tool catalog exposed through the bridge contract.",
                index: 2
            )
            bridgeCard(
                icon: "bubble.left.and.bubble.right",
                title: "Sessions",
                detail: "Conversation continuity without a local chat runtime.",
                index: 3
            )
        }
        .vpAppear(index: 2)
    }

    private var footnote: some View {
        HStack(spacing: 10) {
            Image(systemName: "lock.shield")
                .font(.system(size: 9, weight: .semibold))
                .foregroundStyle(VPTheme.textTertiary)
            Text("No local persona, memory, or chat loop is present in this build.")
                .font(VPTheme.mono(8))
                .foregroundStyle(VPTheme.textTertiary)

            Spacer()

            Link(destination: URL(string: "https://github.com/Verlintas/NovaBAIC")!) {
                HStack(spacing: 5) {
                    Text("NovaBAIC")
                    Image(systemName: "arrow.up.right")
                        .font(.system(size: 7, weight: .bold))
                }
                .font(VPTheme.mono(8, weight: .medium))
                .foregroundStyle(VPTheme.textSecondary)
                .padding(.horizontal, 9)
                .padding(.vertical, 5)
                .background(VPTheme.surface, in: Capsule())
                .overlay {
                    Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1)
                }
            }
            .buttonStyle(VPPressableButtonStyle(scale: 0.96))
        }
        .padding(.top, 4)
        .vpAppear(index: 3)
    }

    // MARK: Components

    private func residentChip(_ title: String, icon: String) -> some View {
        Label(title, systemImage: icon)
            .font(VPTheme.mono(8))
            .foregroundStyle(VPTheme.textTertiary)
            .padding(.horizontal, 7)
            .padding(.vertical, 3)
            .background(VPTheme.surface, in: Capsule())
            .overlay {
                Capsule().strokeBorder(VPTheme.hairline, lineWidth: 1)
            }
    }

    private func bridgeCard(
        icon: String,
        title: String,
        detail: String,
        index: Int
    ) -> some View {
        HStack(alignment: .top, spacing: 10) {
            ZStack {
                RoundedRectangle(cornerRadius: 8, style: .continuous)
                    .fill(VPTheme.resident.opacity(0.09))
                    .frame(width: 30, height: 30)
                Image(systemName: icon)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(VPTheme.resident)
            }

            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: 6) {
                    Text(title)
                        .font(VPTheme.font(11.5, weight: .semibold))
                        .foregroundStyle(VPTheme.textPrimary)
                    Text("FUTURE")
                        .font(VPTheme.mono(6.5, weight: .semibold))
                        .tracking(0.7)
                        .foregroundStyle(VPTheme.textTertiary)
                        .padding(.horizontal, 4)
                        .padding(.vertical, 1)
                        .background(VPTheme.surfaceRaised, in: Capsule())
                }
                Text(detail)
                    .font(VPTheme.font(9.5))
                    .foregroundStyle(VPTheme.textSecondary)
                    .fixedSize(horizontal: false, vertical: true)
            }

            Spacer(minLength: 0)
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(VPTheme.surface, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 12, style: .continuous)
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
        }
        .vpHoverLift(scale: 1.008, lift: 2, glow: VPTheme.resident)
        .vpAppear(index: index + 3)
    }
}

/// A restrained orbital mark for the external resident. The ring and satellite
/// pause under Reduce Motion; the mark itself remains readable.
private struct ResidentOrbit: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var rotating = false
    @State private var breathing = false

    var body: some View {
        ZStack {
            Circle()
                .strokeBorder(VPTheme.hairline, lineWidth: 1)
                .frame(width: 116, height: 116)

            Circle()
                .trim(from: 0.04, to: 0.32)
                .stroke(
                    AngularGradient(
                        colors: [
                            VPTheme.resident.opacity(0.0),
                            VPTheme.resident.opacity(0.75),
                            VPTheme.resident.opacity(0.1),
                        ],
                        center: .center
                    ),
                    style: StrokeStyle(lineWidth: 1.6, lineCap: .round)
                )
                .frame(width: 116, height: 116)
                .rotationEffect(.degrees(rotating ? 360 : 0))
                .animation(
                    reduceMotion
                        ? nil
                        : .linear(duration: 9).repeatForever(autoreverses: false),
                    value: rotating
                )

            Circle()
                .fill(VPTheme.resident)
                .frame(width: 4, height: 4)
                .offset(y: -58)
                .rotationEffect(.degrees(rotating ? 360 : 0))
                .animation(
                    reduceMotion
                        ? nil
                        : .linear(duration: 9).repeatForever(autoreverses: false),
                    value: rotating
                )

            Circle()
                .fill(VPTheme.resident.opacity(0.08))
                .frame(width: 82, height: 82)
                .scaleEffect(breathing || reduceMotion ? 1 : 0.94)
                .animation(
                    reduceMotion
                        ? nil
                        : VPMotion.float,
                    value: breathing
                )

            Image(systemName: "sparkles")
                .font(.system(size: 27, weight: .light))
                .foregroundStyle(
                    LinearGradient(
                        colors: [VPTheme.resident, VPTheme.resident.opacity(0.55)],
                        startPoint: .topLeading,
                        endPoint: .bottomTrailing
                    )
                )
                .symbolRenderingMode(.hierarchical)
        }
        .frame(width: 120, height: 120)
        .onAppear {
            guard !reduceMotion else { return }
            rotating = true
            breathing = true
        }
        .accessibilityHidden(true)
    }
}
