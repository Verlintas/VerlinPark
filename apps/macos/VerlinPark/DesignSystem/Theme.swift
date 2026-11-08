import AppKit
import SwiftUI

/// VerlinPark's visual language: a warm graphite atelier.
///
/// The workspace is treated like a quiet workbench rather than a neon
/// dashboard. Surfaces are flat and layered with hairlines; saturated color is
/// reserved for identity, state, and the primary action. Typography mixes a
/// rounded UI face, an editorial serif for large moments, and monospaced
/// micro-labels for operational metadata.
enum VPTheme {
    // MARK: Canvas and surfaces

    static let canvas = Color(hex: "0B0C0E")
    static let rail = Color(hex: "101114")
    static let surface = Color(hex: "15161A")
    static let surfaceRaised = Color(hex: "1C1D22")
    static let surfaceHover = Color(hex: "23242A")
    static let surfaceSunken = Color(hex: "0D0E11")
    static let hairline = Color.white.opacity(0.068)
    static let hairlineStrong = Color.white.opacity(0.12)
    static let hairlineAccent = Color(hex: "E4785B").opacity(0.28)

    // MARK: Text

    static let textPrimary = Color(hex: "F2EFE8")
    static let textSecondary = Color(hex: "A8A59C")
    static let textTertiary = Color(hex: "706E67")

    // MARK: Identity and state

    static let accent = Color(hex: "E4785B")
    static let accentSoft = Color(hex: "F2A58E")
    static let accentDeep = Color(hex: "C25B3F")
    static let success = Color(hex: "8FBF9F")
    static let warning = Color(hex: "E5B25E")
    static let info = Color(hex: "8EA2F0")
    static let danger = Color(hex: "E06666")
    static let plan = Color(hex: "B8A0E8")
    static let resident = Color(hex: "7FC3A8")

    static let accentGradient = LinearGradient(
        colors: [accent, accentSoft],
        startPoint: .topLeading,
        endPoint: .bottomTrailing
    )

    static let accentWash = LinearGradient(
        colors: [accent.opacity(0.14), accent.opacity(0.03)],
        startPoint: .topLeading,
        endPoint: .bottomTrailing
    )

    static let avatarPalettes: [(Color, Color)] = [
        (Color(hex: "D97A5F"), Color(hex: "E8B15B")),
        (Color(hex: "7FA58C"), Color(hex: "A9C7B2")),
        (Color(hex: "8686D8"), Color(hex: "B69BE8")),
        (Color(hex: "C98A9B"), Color(hex: "E6B7C2")),
        (Color(hex: "6F9CB8"), Color(hex: "9CC4D8")),
        (Color(hex: "B49B6A"), Color(hex: "D8C39A")),
    ]

    // MARK: Typography

    static func font(
        _ size: CGFloat,
        weight: Font.Weight = .regular,
        rounded: Bool = false
    ) -> Font {
        .system(size: size, weight: weight, design: rounded ? .rounded : .default)
    }

    static func editorial(_ size: CGFloat, weight: Font.Weight = .semibold) -> Font {
        .system(size: size, weight: weight, design: .serif)
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
                    .strokeBorder(VPTheme.hairline, lineWidth: 1)
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
                .strokeBorder(tint.opacity(0.18), lineWidth: 1)
        }
    }

    func vpFieldSurface(radius: CGFloat = 10, focused: Bool = false) -> some View {
        background(
            VPTheme.surfaceSunken,
            in: RoundedRectangle(cornerRadius: radius, style: .continuous)
        )
        .overlay {
            RoundedRectangle(cornerRadius: radius, style: .continuous)
                .strokeBorder(
                    focused ? VPTheme.accent.opacity(0.55) : VPTheme.hairline,
                    lineWidth: focused ? 1.2 : 1
                )
        }
        .shadow(
            color: focused ? VPTheme.accent.opacity(0.14) : .clear,
            radius: focused ? 12 : 0
        )
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
            Text(text.uppercased())
                .font(VPTheme.mono(9, weight: .semibold))
                .tracking(0.9)
                .foregroundStyle(color)
            Spacer(minLength: 0)
            if let trailing {
                Text(trailing)
                    .font(VPTheme.mono(8))
                    .foregroundStyle(VPTheme.textTertiary)
            }
        }
    }
}

struct VPLabel: View {
    let text: String

    var body: some View {
        Text(text)
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
struct VPAmbientBackground: View {
    var body: some View {
        ZStack {
            VPTheme.canvas
            RadialGradient(
                colors: [VPTheme.accent.opacity(0.075), .clear],
                center: .topLeading,
                startRadius: 20,
                endRadius: 680
            )
            RadialGradient(
                colors: [VPTheme.resident.opacity(0.035), .clear],
                center: .bottomTrailing,
                startRadius: 40,
                endRadius: 720
            )
            LinearGradient(
                colors: [.clear, VPTheme.canvas.opacity(0.35), VPTheme.canvas],
                startPoint: .top,
                endPoint: .bottom
            )
        }
        .ignoresSafeArea()
        .allowsHitTesting(false)
        .accessibilityHidden(true)
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
                Image(systemName: "sparkles")
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
            Text((label ?? status).uppercased())
                .font(VPTheme.mono(8, weight: .semibold))
                .tracking(0.5)
                .foregroundStyle(VPTheme.statusColor(status))
        }
        .padding(.horizontal, 7)
        .padding(.vertical, 3)
        .background(
            VPTheme.statusColor(status).opacity(0.10),
            in: Capsule()
        )
        .overlay {
            Capsule().strokeBorder(VPTheme.statusColor(status).opacity(0.18), lineWidth: 1)
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
