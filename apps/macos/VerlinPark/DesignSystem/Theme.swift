import AppKit
import SwiftUI

/// VerlinPark's visual language: black crystal glass.
///
/// The workspace is treated like a quiet obsidian workbench: translucent
/// graphite planes, specular edges, and a cold ice accent. Color is reserved
/// for identity, state, and the primary action. Typography stays technical:
/// a UI sans for labels and large moments, monospaced micro-labels for
/// operational metadata.
enum VPTheme {
    // MARK: Canvas and surfaces

    static let canvas = Color(hex: "030407")
    static let rail = Color(hex: "070B10F5")
    static let surface = Color(hex: "0F1720CC")
    static let surfaceRaised = Color(hex: "16212BDD")
    static let surfaceHover = Color(hex: "1D2A36EE")
    static let surfaceSunken = Color(hex: "070A0ECC")
    static let hairline = Color.white.opacity(0.075)
    static let hairlineStrong = Color.white.opacity(0.15)
    static let hairlineAccent = Color(hex: "A9C9D6").opacity(0.26)

    // MARK: Text

    static let textPrimary = Color(hex: "EEF2F5")
    static let textSecondary = Color(hex: "A5B0B8")
    static let textTertiary = Color(hex: "6D7982")

    // MARK: Identity and state

    static let accent = Color(hex: "B4DCEB")
    static let accentSoft = Color(hex: "E2F4FA")
    static let accentDeep = Color(hex: "3F6B7F")
    static let success = Color(hex: "74D3A4")
    static let warning = Color(hex: "F2C66D")
    static let info = Color(hex: "9CB5FF")
    static let danger = Color(hex: "FF8585")
    static let plan = Color(hex: "C2A6FF")
    static let resident = Color(hex: "6FE0E6")

    static let accentGradient = LinearGradient(
        colors: [accentSoft, accent],
        startPoint: .topLeading,
        endPoint: .bottomTrailing
    )

    static let primaryGradient = LinearGradient(
        colors: [Color(hex: "4C7285"), Color(hex: "2E4957")],
        startPoint: .top,
        endPoint: .bottom
    )

    static let accentWash = LinearGradient(
        colors: [accent.opacity(0.13), accent.opacity(0.025)],
        startPoint: .topLeading,
        endPoint: .bottomTrailing
    )

    static let edgeGradient = LinearGradient(
        colors: [
            Color.white.opacity(0.24),
            Color.white.opacity(0.045),
            Color.white.opacity(0.12),
        ],
        startPoint: .topLeading,
        endPoint: .bottomTrailing
    )

    static let avatarPalettes: [(Color, Color)] = [
        (Color(hex: "4E8FA8"), Color(hex: "8FD6E8")),
        (Color(hex: "4F8F7B"), Color(hex: "86D8B8")),
        (Color(hex: "6B6FB0"), Color(hex: "B5A6FF")),
        (Color(hex: "A05F77"), Color(hex: "E8A6C2")),
        (Color(hex: "4D7CA8"), Color(hex: "8FC4F0")),
        (Color(hex: "8B7B57"), Color(hex: "D8C08A")),
    ]

    // MARK: Typography

    // MARK: Typography

    static func font(
        _ size: CGFloat,
        weight: Font.Weight = .regular,
        rounded: Bool = false
    ) -> Font {
        .system(size: size, weight: weight, design: rounded ? .rounded : .default)
    }

    static func editorial(_ size: CGFloat, weight: Font.Weight = .semibold) -> Font {
        .system(size: size, weight: weight)
    }

    static func mono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight, design: .monospaced)
    }

    // MARK: Colors by meaning

    static func avatarGradient(seed: String) -> LinearGradient {
        let hash = seed.unicodeScalars.reduce(0) { ($0 &* 31) &+ Int($1.value) }
        let palette = avatarPalettes[abs(hash) % avatarPalettes.count]
        return LinearGradient(
            colors: [palette.0, palette.1],
            startPoint: .topLeading,
            endPoint: .bottomTrailing
        )
    }

    static func statusColor(_ status: String) -> Color {
        switch status.lowercased() {
        case "running", "verifying", "merging", "thinking", "in_progress":
            accent
        case "queued", "paused", "waiting", "waiting_input":
            warning
        case "failed", "cancelled", "offline", "blocked", "interrupted":
            danger
        case "done", "completed", "connected", "landed", "merged":
            success
        case "conflict", "changes_requested":
            warning
        case "approved":
            info
        default:
            textTertiary
        }
    }

    static func roleColor(_ role: String) -> Color {
        switch role.lowercased() {
        case "leader": accent
        case "manager": plan
        case "worker": info
        case "verifier": success
        case "labour": warning
        default: textSecondary
        }
    }

    static func roleIcon(_ role: String) -> String {
        switch role.lowercased() {
        case "leader": "crown"
        case "manager": "person.2"
        case "worker": "person.3"
        case "verifier": "checkmark.shield"
        case "labour": "hammer"
        default: "person"
        }
    }

    static func projectTint(seed: String) -> Color {
        let hash = seed.unicodeScalars.reduce(0) { ($0 &* 31) &+ Int($1.value) }
        let colors: [Color] = [accent, resident, info, plan, warning, Color(hex: "C98A9B")]
        return colors[abs(hash) % colors.count]
    }

    static func relativeTime(_ iso: String) -> String {
        VPRelativeTime.label(iso)
    }
}

// MARK: - Surfaces and chrome

extension View {
    /// Flat layered surface used by panels, cards, and rows.
    func vpSurface(
        radius: CGFloat = 14,
        raised: Bool = false,
        border: Bool = true
    ) -> some View {
        background(
            raised ? VPTheme.surfaceRaised : VPTheme.surface,
            in: RoundedRectangle(cornerRadius: radius, style: .continuous)
        )
        .overlay {
            if border {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(VPTheme.edgeGradient, lineWidth: 1)
            }
        }
    }

    func vpTextPrimary() -> some View {
        foregroundStyle(VPTheme.textPrimary)
    }

    func vpTextSecondary() -> some View {
        foregroundStyle(VPTheme.textSecondary)
    }

    /// A soft inner wash for selected rows and inset controls.
    func vpInset(radius: CGFloat = 10, tint: Color = VPTheme.accent) -> some View {
        background(
            tint.opacity(0.085),
            in: RoundedRectangle(cornerRadius: radius, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: radius, style: .continuous)
                .strokeBorder(tint.opacity(0.22), lineWidth: 1)
        }
    }

    func vpFrostedField(radius: CGFloat = 8) -> some View {
        font(VPTheme.mono(10))
            .foregroundStyle(VPTheme.textPrimary)
            .padding(.horizontal, 9)
            .padding(.vertical, 6)
            .vpFieldSurface(radius: radius)
    }

    func vpFieldSurface(radius: CGFloat = 10, focused: Bool = false) -> some View {
        background {
            ZStack {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(.ultraThinMaterial)
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(VPTheme.surfaceSunken.opacity(0.52))
            }
        }
        .overlay {
            RoundedRectangle(cornerRadius: radius, style: .continuous)
                .strokeBorder(
                    focused
                        ? AnyShapeStyle(VPTheme.accent.opacity(0.55))
                        : AnyShapeStyle(VPTheme.edgeGradient),
                    lineWidth: focused ? 1.2 : 1
                )
        }
        .shadow(
            color: focused ? VPTheme.accent.opacity(0.12) : .black.opacity(0.12),
            radius: focused ? 12 : 0,
            y: focused ? 0 : 1
        )
        .overlay {
            RoundedRectangle(cornerRadius: radius, style: .continuous)
                .strokeBorder(.white.opacity(focused ? 0.02 : 0.045), lineWidth: 1)
                .padding(1)
        }
    }
}

struct VPHairline: View {
    var vertical = false

    var body: some View {
        Rectangle()
            .fill(VPTheme.hairline)
            .frame(width: vertical ? 1 : nil, height: vertical ? nil : 1)
    }
}

struct VPKicker: View {
    let text: String
    var color: Color = VPTheme.textTertiary
    var trailing: String?

    var body: some View {
        HStack(spacing: 8) {
            Text(L10n.t(text).uppercased())
                .font(VPTheme.mono(9, weight: .semibold))
                .tracking(0.9)
                .foregroundStyle(color)
            Spacer(minLength: 0)
            if let trailing {
                Text(L10n.t(trailing))
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
    }
}

struct VPLabel: View {
    let text: String

    var body: some View {
        Text(L10n.t(text))
            .font(VPTheme.mono(9, weight: .medium))
            .foregroundStyle(VPTheme.textTertiary)
    }
}

struct VPMeta: View {
    let text: String
    var icon: String?

    var body: some View {
        HStack(spacing: 4) {
            if let icon {
                Image(systemName: icon)
                    .font(.system(size: 8, weight: .semibold))
            }
            Text(text)
                .lineLimit(1)
        }
        .font(VPTheme.mono(8))
        .foregroundStyle(VPTheme.textTertiary)
    }
}

struct VPSurfaceCard<Content: View>: View {
    var radius: CGFloat = 14
    var padding: CGFloat = 12
    var raised = false
    @ViewBuilder var content: Content

    var body: some View {
        content
            .padding(padding)
            .vpSurface(radius: radius, raised: raised)
    }
}

struct VPInsetCard<Content: View>: View {
    var radius: CGFloat = 11
    var padding: CGFloat = 10
    @ViewBuilder var content: Content

    var body: some View {
        content
            .padding(padding)
            .background(
                VPTheme.surfaceSunken,
                in: RoundedRectangle(cornerRadius: radius, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(VPTheme.hairline, lineWidth: 1)
            }
    }
}

struct VPKeyCap: View {
    let text: String

    var body: some View {
        Text(text)
            .font(VPTheme.mono(8, weight: .semibold))
            .foregroundStyle(VPTheme.textSecondary)
            .padding(.horizontal, 5)
            .padding(.vertical, 2)
            .background(VPTheme.surfaceRaised, in: RoundedRectangle(cornerRadius: 4, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: 4, style: .continuous)
                    .strokeBorder(VPTheme.hairline, lineWidth: 1)
            }
    }
}

// MARK: - Brand and avatars

/// Static ambient wash. It replaces the previous drifting aurora so the canvas
/// stays calm while the content carries the motion.
struct VPCrystalSurface: View {
    var tone: Color = VPTheme.canvas
    var tintOpacity: Double = 0.52
    var material: Material = .ultraThinMaterial

    var body: some View {
        ZStack {
            Rectangle().fill(material)
            tone.opacity(tintOpacity)
            RadialGradient(
                colors: [.white.opacity(0.032), .clear],
                center: .topLeading,
                startRadius: 0,
                endRadius: 520
            )
            LinearGradient(
                colors: [
                    .white.opacity(0.032),
                    .clear,
                    .white.opacity(0.012),
                ],
                startPoint: .topLeading,
                endPoint: .bottomTrailing
            )
        }
        .ignoresSafeArea()
    }
}

/// Static black-crystal backdrop. Angular, very low-contrast facets and a
/// single specular sweep replace the earlier aurora; nothing animates, so the
/// effect stays calm and cheap.
struct VPCrystalBackdrop: View {
    var body: some View {
        GeometryReader { proxy in
            ZStack {
                VPTheme.canvas

                CrystalFacet()
                    .fill(
                        LinearGradient(
                            colors: [
                                Color(hex: "35515F").opacity(0.20),
                                Color(hex: "1A2530").opacity(0.06),
                                .clear,
                            ],
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        )
                    )
                    .frame(width: proxy.size.width * 0.78, height: proxy.size.height * 1.05)
                    .rotationEffect(.degrees(-17))
                    .offset(x: -proxy.size.width * 0.24, y: -proxy.size.height * 0.22)
                    .blur(radius: 42)

                CrystalFacet()
                    .fill(
                        LinearGradient(
                            colors: [
                                Color(hex: "3A354C").opacity(0.14),
                                Color(hex: "18202A").opacity(0.05),
                                .clear,
                            ],
                            startPoint: .bottomTrailing,
                            endPoint: .topLeading
                        )
                    )
                    .frame(width: proxy.size.width * 0.62, height: proxy.size.height * 0.88)
                    .rotationEffect(.degrees(24))
                    .offset(x: proxy.size.width * 0.30, y: proxy.size.height * 0.26)
                    .blur(radius: 50)

                LinearGradient(
                    colors: [
                        Color.white.opacity(0.035),
                        .clear,
                        Color.black.opacity(0.18),
                    ],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )

                LinearGradient(
                    colors: [.clear, VPTheme.canvas.opacity(0.30), VPTheme.canvas.opacity(0.78)],
                    startPoint: .top,
                    endPoint: .bottom
                )
            }
        }
        .ignoresSafeArea()
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

struct VPAmbientBackground: View {
    var body: some View {
        VPCrystalBackdrop()
    }
}

private struct CrystalFacet: Shape {
    func path(in rect: CGRect) -> Path {
        var path = Path()
        path.move(to: CGPoint(x: rect.midX, y: rect.minY))
        path.addLine(to: CGPoint(x: rect.minX + rect.width * 0.92, y: rect.minY + rect.height * 0.30))
        path.addLine(to: CGPoint(x: rect.minX + rect.width * 0.71, y: rect.maxY))
        path.addLine(to: CGPoint(x: rect.minX + rect.width * 0.29, y: rect.maxY))
        path.addLine(to: CGPoint(x: rect.minX + rect.width * 0.08, y: rect.minY + rect.height * 0.30))
        path.closeSubpath()
        return path
    }
}

struct VPLogoMark: View {
    let config: BrandConfig
    var compact = false

    var body: some View {
        HStack(spacing: 9) {
            ZStack {
                RoundedRectangle(cornerRadius: compact ? 8 : 10, style: .continuous)
                    .fill(VPTheme.accentGradient)
                    .frame(width: compact ? 26 : 30, height: compact ? 26 : 30)
                Image(systemName: "cube.transparent")
                    .font(.system(size: compact ? 11 : 13, weight: .bold))
                    .foregroundStyle(.white)
            }
            VStack(alignment: .leading, spacing: 1) {
                Text(config.effectiveName)
                    .font(VPTheme.font(compact ? 14 : 16, weight: .semibold, rounded: true))
                    .foregroundStyle(VPTheme.textPrimary)
                    .lineLimit(1)
                if !compact {
                    Text("agent studio")
                        .font(VPTheme.mono(8, weight: .medium))
                        .foregroundStyle(VPTheme.textTertiary)
                        .tracking(0.4)
                }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(config.effectiveName) agent studio")
    }
}

struct VPSessionAvatar: View {
    let seed: String
    let initial: String
    var size: CGFloat = 32
    var status: String?
    var showsStatus = true

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            RoundedRectangle(cornerRadius: size * 0.30, style: .continuous)
                .fill(VPTheme.avatarGradient(seed: seed))
                .frame(width: size, height: size)
                .overlay {
                    Text(initial.uppercased())
                        .font(.system(size: size * 0.38, weight: .bold, design: .rounded))
                        .foregroundStyle(.white.opacity(0.92))
                }
            if let status, showsStatus {
                Circle()
                    .fill(VPTheme.statusColor(status))
                    .frame(width: size * 0.28, height: size * 0.28)
                    .overlay {
                        Circle().strokeBorder(VPTheme.rail, lineWidth: 2)
                    }
                    .offset(x: 2, y: 2)
            }
        }
    }
}

struct VPAgentAvatar: View {
    let seed: String
    var role: String = "worker"
    var size: CGFloat = 26
    var active = false

    var body: some View {
        ZStack {
            RoundedRectangle(cornerRadius: size * 0.30, style: .continuous)
                .fill(VPTheme.roleColor(role).opacity(0.16))
                .frame(width: size, height: size)
            Image(systemName: VPTheme.roleIcon(role))
                .font(.system(size: size * 0.38, weight: .semibold))
                .foregroundStyle(VPTheme.roleColor(role))
            if active {
                Circle()
                    .fill(VPTheme.statusColor("running"))
                    .frame(width: size * 0.24, height: size * 0.24)
                    .overlay { Circle().strokeBorder(VPTheme.surface, lineWidth: 1.5) }
                    .offset(x: size * 0.34, y: -size * 0.34)
            }
        }
        .accessibilityLabel(seed)
    }
}

struct VPSectionTitle: View {
    let title: String
    var trailing: String?

    var body: some View {
        VPKicker(text: title, trailing: trailing)
    }
}

// MARK: - Status chips

struct VPStatusChip: View {
    let status: String
    var label: String?

    var body: some View {
        HStack(spacing: 5) {
            Circle()
                .fill(VPTheme.statusColor(status))
                .frame(width: 5, height: 5)
            Text(L10n.t(label ?? status).uppercased())
                .font(VPTheme.mono(8, weight: .semibold))
                .tracking(0.5)
                .foregroundStyle(VPTheme.statusColor(status))
        }
        .padding(.horizontal, 7)
        .padding(.vertical, 3)
        .background(
            VPTheme.statusColor(status).opacity(0.15),
            in: Capsule()
        )
        .overlay {
            Capsule().strokeBorder(VPTheme.statusColor(status).opacity(0.28), lineWidth: 1)
        }
        .accessibilityLabel("Status \(status)")
    }
}

struct VPFlowLayout: Layout {
    var spacing: CGFloat = 6

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache _: inout ()) -> CGSize {
        let maxWidth = proposal.width ?? .infinity
        var x: CGFloat = 0
        var y: CGFloat = 0
        var rowHeight: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x + size.width > maxWidth, x > 0 {
                x = 0
                y += rowHeight + spacing
                rowHeight = 0
            }
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
        return CGSize(width: maxWidth.isFinite ? maxWidth : x, height: y + rowHeight)
    }

    func placeSubviews(
        in bounds: CGRect,
        proposal _: ProposedViewSize,
        subviews: Subviews,
        cache _: inout ()
    ) {
        var x = bounds.minX
        var y = bounds.minY
        var rowHeight: CGFloat = 0
        for subview in subviews {
            let size = subview.sizeThatFits(.unspecified)
            if x + size.width > bounds.maxX, x > bounds.minX {
                x = bounds.minX
                y += rowHeight + spacing
                rowHeight = 0
            }
            subview.place(at: CGPoint(x: x, y: y), proposal: ProposedViewSize(size))
            x += size.width + spacing
            rowHeight = max(rowHeight, size.height)
        }
    }
}

// MARK: - Time

enum VPRelativeTime {
    static func label(_ iso: String) -> String {
        guard let date = parse(iso) else { return String(iso.prefix(16)) }
        let seconds = Date().timeIntervalSince(date)
        if seconds < 0 {
            return "now"
        }
        if seconds < 60 {
            return "now"
        }
        if seconds < 3600 {
            return "\(Int(seconds / 60))m"
        }
        if seconds < 86400 {
            return "\(Int(seconds / 3600))h"
        }
        if seconds < 604_800 {
            return "\(Int(seconds / 86400))d"
        }
        let formatter = DateFormatter()
        formatter.dateFormat = "MMM d"
        return formatter.string(from: date)
    }

    static func absolute(_ iso: String) -> String {
        guard let date = parse(iso) else { return String(iso.prefix(16)) }
        let formatter = DateFormatter()
        formatter.dateFormat = "MMM d, HH:mm"
        return formatter.string(from: date)
    }

    private static func parse(_ iso: String) -> Date? {
        let fractional = ISO8601DateFormatter()
        fractional.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let date = fractional.date(from: iso) {
            return date
        }
        return ISO8601DateFormatter().date(from: iso)
    }
}
