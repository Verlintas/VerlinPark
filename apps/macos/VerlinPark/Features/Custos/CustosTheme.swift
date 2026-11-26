import Foundation
import SwiftUI

/// Custos has its own visual language, deliberately separate from the
/// VerlinPark glass system. It is an obsidian instrument: near-black planes,
/// one-pixel ice rules, restrained state color, and monospaced operational
/// labels. No materials, cards, rounded panel chrome, or glass are used here.
enum CustosTheme {
    // MARK: Canvas

    static let void = Color(hex: "010203")
    static let obsidian = Color(hex: "04070A")
    static let panel = Color(hex: "070C10")
    static let panelRaised = Color(hex: "0B1217")
    static let rule = Color(hex: "14232A")
    static let ruleStrong = Color(hex: "28424B")

    // MARK: Ink

    static let ice = Color(hex: "C6EAF2")
    static let iceDim = Color(hex: "86AFBA")
    static let textPrimary = Color(hex: "DCE7EA")
    static let textSecondary = Color(hex: "82969D")
    static let textTertiary = Color(hex: "4C5C62")

    // MARK: State

    static let healthy = Color(hex: "A9E4EE")
    static let watch = Color(hex: "D8B36C")
    static let degraded = Color(hex: "DF9A5B")
    static let critical = Color(hex: "FF6F6F")
    static let quarantined = Color(hex: "5A6266")
    static let lost = Color(hex: "9A7C7C")
    static let unknown = Color(hex: "5D696E")

    // MARK: Type

    static func mono(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight, design: .monospaced)
    }

    static func body(_ size: CGFloat, weight: Font.Weight = .regular) -> Font {
        .system(size: size, weight: weight)
    }

    // MARK: State mapping

    static func stateColor(_ state: CustosState) -> Color {
        switch state {
        case .healthy:
            healthy
        case .watch:
            watch
        case .degraded:
            degraded
        case .critical:
            critical
        case .quarantined:
            quarantined
        case .signalLost:
            lost
        case .unknown:
            unknown
        }
    }

    static func dimensionColor(_ state: CustosDimensionState) -> Color {
        switch state {
        case .ok:
            healthy
        case .watch:
            watch
        case .degraded:
            degraded
        case .critical:
            critical
        case .unknown:
            unknown
        }
    }

    /// Idle glow period in seconds. A missing period means the instrument is
    /// static: quarantined and signal-lost surfaces never breathe.
    static func glowPeriod(_ state: CustosState) -> TimeInterval? {
        switch state {
        case .healthy:
            6
        case .watch:
            4
        case .degraded:
            3
        case .critical:
            2
        case .quarantined, .signalLost, .unknown:
            nil
        }
    }

    /// Soft ±8% brightness breath. Static frames and reduced-motion surfaces
    /// return a constant so the caller has no branch of its own.
    static func glowLevel(_ state: CustosState, at date: Date, reduceMotion: Bool) -> Double {
        guard !reduceMotion, let period = glowPeriod(state) else { return 1 }
        let phase = date.timeIntervalSinceReferenceDate
            .truncatingRemainder(dividingBy: period) / period
        return 0.92 + 0.08 * (0.5 + 0.5 * sin(2 * .pi * phase))
    }
}

/// Flat obsidian field with a restrained state wash. It intentionally has no
/// material and no blur, so Reduce Transparency only removes the wash.
struct CustosBackdrop: View {
    let state: CustosState
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    var body: some View {
        ZStack {
            CustosTheme.void
            if !reduceTransparency {
                RadialGradient(
                    colors: [
                        CustosTheme.stateColor(state).opacity(0.09),
                        CustosTheme.obsidian.opacity(0.55),
                        .clear,
                    ],
                    center: .center,
                    startRadius: 30,
                    endRadius: 520
                )
                LinearGradient(
                    colors: [Color.white.opacity(0.025), .clear, Color.black.opacity(0.22)],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
            }
            Rectangle()
                .strokeBorder(CustosTheme.rule, lineWidth: 1)
        }
        .ignoresSafeArea()
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

/// The graphical Custos sigil: one large rhombus with a faceted interior.
/// It is a drawn shape, not character art.
struct CustosDiamondView: View {
    let state: CustosState
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency

    private var tint: Color {
        CustosTheme.stateColor(state)
    }

    var body: some View {
        GeometryReader { proxy in
            let side = min(proxy.size.width, proxy.size.height)
            let frame = CGRect(
                x: (proxy.size.width - side) / 2,
                y: (proxy.size.height - side) / 2,
                width: side,
                height: side
            )
            ZStack {
                CustosDiamondShape()
                    .fill(
                        LinearGradient(
                            colors: [tint.opacity(0.16), tint.opacity(0.03), .clear],
                            startPoint: .top,
                            endPoint: .bottom
                        )
                    )
                    .frame(width: frame.width, height: frame.height)

                CustosDiamondShape(inset: side * 0.02)
                    .stroke(
                        LinearGradient(
                            colors: [tint.opacity(0.90), tint.opacity(0.22), tint.opacity(0.58)],
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        ),
                        lineWidth: 1.4
                    )
                    .frame(width: frame.width, height: frame.height)
                    .shadow(
                        color: reduceTransparency ? .clear : tint.opacity(0.32),
                        radius: 18
                    )

                CustosDiamondShape(inset: side * 0.17)
                    .stroke(tint.opacity(0.24), lineWidth: 0.8)
                    .frame(width: frame.width, height: frame.height)

                CustosDiamondShape(inset: side * 0.32)
                    .stroke(tint.opacity(0.12), lineWidth: 0.6)
                    .frame(width: frame.width, height: frame.height)

                Rectangle()
                    .fill(tint.opacity(0.18))
                    .frame(width: side * 0.42, height: 0.6)

                Circle()
                    .fill(tint)
                    .frame(width: 4, height: 4)
                    .shadow(
                        color: reduceTransparency ? .clear : tint.opacity(0.65),
                        radius: 7
                    )
            }
            .frame(width: proxy.size.width, height: proxy.size.height)
        }
        .accessibilityHidden(true)
    }
}

private struct CustosDiamondShape: Shape {
    var inset: CGFloat = 0

    func path(in rect: CGRect) -> Path {
        let bounds = rect.insetBy(dx: inset, dy: inset)
        var path = Path()
        path.move(to: CGPoint(x: bounds.midX, y: bounds.minY))
        path.addLine(to: CGPoint(x: bounds.maxX, y: bounds.midY))
        path.addLine(to: CGPoint(x: bounds.midX, y: bounds.maxY))
        path.addLine(to: CGPoint(x: bounds.minX, y: bounds.midY))
        path.closeSubpath()
        return path
    }
}
