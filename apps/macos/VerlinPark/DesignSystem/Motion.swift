import SwiftUI

/// Shared motion vocabulary for VerlinPark.
///
/// Motion is intentionally small and composable: a handful of durations,
/// springs, hover/press treatments, and transition helpers. Views opt into a
/// treatment instead of inventing one-off animations so the app feels coherent
/// and reduce-motion behavior stays centralized.
enum VPMotion {
    // Durations
    static let instant: Double = 0.10
    static let quick: Double = 0.18
    static let standard: Double = 0.30
    static let emphasized: Double = 0.46
    static let slow: Double = 0.72

    // Curves and springs
    static let hover = Animation.easeOut(duration: quick)
    static let press = Animation.spring(duration: 0.18, bounce: 0.10)
    static let snap = Animation.spring(duration: 0.28, bounce: 0.16)
    static let smooth = Animation.spring(duration: 0.42, bounce: 0.04)
    static let gentle = Animation.spring(duration: 0.62, bounce: 0.01)
    static let emphasizedSpring = Animation.spring(duration: 0.52, bounce: 0.20)
    static let badge = Animation.spring(duration: 0.34, bounce: 0.30)
    static let shimmer = Animation.linear(duration: 1.65).repeatForever(autoreverses: false)
    static let pulse = Animation.easeInOut(duration: 1.35).repeatForever(autoreverses: true)
    static let caret = Animation.easeInOut(duration: 0.72).repeatForever(autoreverses: true)
    static let float = Animation.easeInOut(duration: 2.6).repeatForever(autoreverses: true)
    static let aurora = Animation.easeInOut(duration: 18).repeatForever(autoreverses: true)
    static let spin = Animation.linear(duration: 0.9).repeatForever(autoreverses: false)

    /// Staggered reveal delay, capped so a long list never takes too long.
    static func stagger(_ index: Int, step: Double = 0.045, cap: Double = 0.36) -> Double {
        min(Double(max(0, index)) * step, cap)
    }

    /// Returns nil when Reduce Motion is enabled.
    static func resolved(_ animation: Animation, reduceMotion: Bool) -> Animation? {
        reduceMotion ? nil : animation
    }

    static func delayed(_ animation: Animation, index: Int, reduceMotion: Bool) -> Animation? {
        guard !reduceMotion else { return nil }
        return animation.delay(stagger(index))
    }
}

extension AnyTransition {
    /// A restrained upward reveal for rows, cards, and chat entries.
    static var vpRise: AnyTransition {
        .asymmetric(
            insertion: .opacity
                .combined(with: .offset(y: 10))
                .combined(with: .scale(scale: 0.985, anchor: .top)),
            removal: .opacity.combined(with: .scale(scale: 0.985, anchor: .top))
        )
    }

    static var vpScaleFade: AnyTransition {
        .opacity.combined(with: .scale(scale: 0.965))
    }

    static var vpContent: AnyTransition {
        .opacity.combined(with: .offset(y: 6))
    }

    static var vpTop: AnyTransition {
        .move(edge: .top).combined(with: .opacity)
    }

    static var vpSidebar: AnyTransition {
        .asymmetric(
            insertion: .move(edge: .leading).combined(with: .opacity),
            removal: .opacity
        )
    }
}

// MARK: - Appearance and interaction modifiers

private struct VPAppearModifier: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let index: Int
    let animation: Animation
    @State private var appeared = false

    func body(content: Content) -> some View {
        content
            .opacity(appeared || reduceMotion ? 1 : 0)
            .offset(y: appeared || reduceMotion ? 0 : 10)
            .scaleEffect(appeared || reduceMotion ? 1 : 0.985)
            .animation(
                VPMotion.delayed(animation, index: index, reduceMotion: reduceMotion),
                value: appeared
            )
            .onAppear { appeared = true }
    }
}

extension View {
    /// Staggered entrance used by cards, rows, and page sections.
    func vpAppear(index: Int = 0, animation: Animation = VPMotion.smooth) -> some View {
        modifier(VPAppearModifier(index: index, animation: animation))
    }

    /// Soft hover elevation for interactive cards. Preserves the card's own
    /// background and border; only adds scale, lift, and a tinted shadow.
    func vpHoverLift(
        scale: CGFloat = 1.004,
        lift: CGFloat = 1,
        glow: Color = .white
    ) -> some View {
        modifier(VPHoverLiftModifier(scale: scale, lift: lift, glow: glow))
    }

    /// Row highlight that works inside lazy stacks without owning the row's
    /// selected background.
    func vpHoverHighlight(radius: CGFloat = 9, tint: Color = .white) -> some View {
        modifier(VPHoverHighlightModifier(radius: radius, tint: tint))
    }

    /// Animated gradient border; useful for pending approvals, focus, and
    /// active cards.
    func vpGlowBorder(_ color: Color, radius: CGFloat = 12, active: Bool = true) -> some View {
        modifier(VPGlowBorderModifier(color: color, radius: radius, active: active))
    }

    /// Focus treatment for text fields and composers.
    func vpFocusRing(_ focused: Bool, color: Color = .green, radius: CGFloat = 12) -> some View {
        modifier(VPFocusRingModifier(focused: focused, color: color, radius: radius))
    }

    func vpShimmer() -> some View {
        modifier(VPShimmerModifier())
    }

    func vpAnimation(_ animation: Animation, value: some Equatable) -> some View {
        modifier(VPAnimationModifier(animation: animation, value: value))
    }
}

private struct VPHoverLiftModifier: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let scale: CGFloat
    let lift: CGFloat
    let glow: Color
    @State private var hovering = false

    func body(content: Content) -> some View {
        content
            .scaleEffect(hovering && !reduceMotion ? scale : 1)
            .offset(y: hovering && !reduceMotion ? -lift : 0)
            .shadow(
                color: hovering && !reduceMotion ? glow.opacity(0.12) : .clear,
                radius: hovering ? 14 : 0,
                y: hovering ? 7 : 0
            )
            .animation(VPMotion.resolved(VPMotion.hover, reduceMotion: reduceMotion), value: hovering)
            .onHover { hovering = $0 }
    }
}

private struct VPHoverHighlightModifier: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let radius: CGFloat
    let tint: Color
    @State private var hovering = false

    func body(content: Content) -> some View {
        content
            .background(
                tint.opacity(hovering ? 0.055 : 0),
                in: RoundedRectangle(cornerRadius: radius, style: .continuous)
            )
            .animation(VPMotion.resolved(VPMotion.hover, reduceMotion: reduceMotion), value: hovering)
            .onHover { hovering = $0 }
    }
}

private struct VPGlowBorderModifier: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let color: Color
    let radius: CGFloat
    let active: Bool
    @State private var pulse = false

    func body(content: Content) -> some View {
        content
            .overlay {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(
                        AngularGradient(
                            colors: [
                                color.opacity(active ? 0.55 : 0.12),
                                color.opacity(active ? 0.16 : 0.04),
                                color.opacity(active ? 0.36 : 0.08),
                                color.opacity(active ? 0.55 : 0.12),
                            ],
                            center: .center
                        ),
                        lineWidth: active ? 1.2 : 0.8
                    )
                    .opacity(active ? (pulse || reduceMotion ? 1 : 0.55) : 1)
                    .onAppear {
                        guard active, !reduceMotion else { return }
                        withAnimation(VPMotion.pulse) { pulse = true }
                    }
                    .onChange(of: active) { _, newValue in
                        guard !reduceMotion else {
                            pulse = false
                            return
                        }
                        if newValue {
                            pulse = false
                            withAnimation(VPMotion.pulse) { pulse = true }
                        } else {
                            pulse = false
                        }
                    }
            }
    }
}

private struct VPFocusRingModifier: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let focused: Bool
    let color: Color
    let radius: CGFloat

    func body(content: Content) -> some View {
        content
            .overlay {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .strokeBorder(
                        color.opacity(focused ? 0.55 : 0.08),
                        lineWidth: focused ? 1.4 : 1
                    )
                    .shadow(
                        color: focused ? color.opacity(0.20) : .clear,
                        radius: focused ? 12 : 0
                    )
            }
            .animation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion), value: focused)
    }
}

private struct VPShimmerModifier: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var phase: CGFloat = -1

    func body(content: Content) -> some View {
        content
            .overlay {
                GeometryReader { proxy in
                    LinearGradient(
                        colors: [.clear, .white.opacity(0.16), .clear],
                        startPoint: .leading,
                        endPoint: .trailing
                    )
                    .frame(width: max(40, proxy.size.width * 0.55))
                    .offset(x: phase * proxy.size.width * 1.8)
                }
                .clipShape(RoundedRectangle(cornerRadius: 8, style: .continuous))
                .allowsHitTesting(false)
            }
            .onAppear {
                guard !reduceMotion else { return }
                withAnimation(VPMotion.shimmer) { phase = 1.4 }
            }
    }
}

private struct VPAnimationModifier<Value: Equatable>: ViewModifier {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let animation: Animation
    let value: Value

    func body(content: Content) -> some View {
        content.animation(VPMotion.resolved(animation, reduceMotion: reduceMotion), value: value)
    }
}

// MARK: - Buttons

struct VPPressableButtonStyle: ButtonStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    var scale: CGFloat = 0.97
    var pressedOpacity: Double = 0.85

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .scaleEffect(configuration.isPressed && !reduceMotion ? scale : 1)
            .opacity(configuration.isPressed ? pressedOpacity : 1)
            .animation(
                VPMotion.resolved(VPMotion.press, reduceMotion: reduceMotion),
                value: configuration.isPressed
            )
    }
}

struct VPPrimaryButtonStyle: ButtonStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @Environment(\.controlSize) private var controlSize
    var tint: Color = .green
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        let large = controlSize == .large
        return configuration.label
            .font(.system(size: large ? 13 : 11, weight: .semibold, design: .rounded))
            .foregroundStyle(.white)
            .padding(.horizontal, large ? 17 : 12)
            .padding(.vertical, large ? 9 : 7)
            .background(
                LinearGradient(
                    colors: [
                        tint.opacity(configuration.isPressed ? 0.68 : 0.92),
                        tint.opacity(configuration.isPressed ? 0.48 : 0.68),
                    ],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                ),
                in: RoundedRectangle(cornerRadius: 9, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: 9, style: .continuous)
                    .strokeBorder(.white.opacity(hovering ? 0.24 : 0.12), lineWidth: 1)
            }
            .shadow(
                color: tint.opacity(hovering && !reduceMotion ? 0.26 : 0.10),
                radius: hovering ? 14 : 5,
                y: hovering ? 5 : 2
            )
            .scaleEffect(configuration.isPressed && !reduceMotion ? 0.97 : 1)
            .animation(VPMotion.resolved(VPMotion.hover, reduceMotion: reduceMotion), value: hovering)
            .animation(
                VPMotion.resolved(VPMotion.press, reduceMotion: reduceMotion),
                value: configuration.isPressed
            )
            .onHover { hovering = $0 }
    }
}

struct VPGlassIconButtonStyle: ButtonStyle {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    var size: CGFloat = 26
    var tint: Color = .secondary
    @State private var hovering = false

    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.system(size: 11, weight: .semibold))
            .foregroundStyle(hovering ? Color.primary : tint)
            .frame(width: size, height: size)
            .background(
                .ultraThinMaterial,
                in: Circle()
            )
            .overlay {
                Circle().strokeBorder(.white.opacity(hovering ? 0.16 : 0.06), lineWidth: 1)
            }
            .shadow(color: .black.opacity(hovering ? 0.24 : 0.08), radius: hovering ? 8 : 2, y: 2)
            .scaleEffect(configuration.isPressed && !reduceMotion ? 0.92 : 1)
            .animation(VPMotion.resolved(VPMotion.hover, reduceMotion: reduceMotion), value: hovering)
            .animation(
                VPMotion.resolved(VPMotion.press, reduceMotion: reduceMotion),
                value: configuration.isPressed
            )
            .onHover { hovering = $0 }
    }
}

// MARK: - Segmented control

struct VPSegment<Value: Hashable>: Identifiable {
    let value: Value
    let title: String
    let icon: String?

    init(_ value: Value, title: String, icon: String? = nil) {
        self.value = value
        self.title = title
        self.icon = icon
    }

    var id: Value {
        value
    }
}

struct VPSegmentedControl<Value: Hashable>: View {
    let segments: [VPSegment<Value>]
    @Binding var selection: Value
    var scrollable = false
    var compact = false

    @Namespace private var selectionNamespace
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var hovered: Value?

    var body: some View {
        Group {
            if scrollable {
                ScrollView(.horizontal, showsIndicators: false) {
                    segmentRow
                }
            } else {
                segmentRow
            }
        }
        .padding(3)
        .background(.black.opacity(0.14), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay {
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(.white.opacity(0.06), lineWidth: 1)
        }
        .animation(VPMotion.resolved(VPMotion.snap, reduceMotion: reduceMotion), value: selection)
    }

    private var segmentRow: some View {
        HStack(spacing: 2) {
            ForEach(segments) { segment in
                Button {
                    selection = segment.value
                } label: {
                    HStack(spacing: compact ? 3 : 5) {
                        if let icon = segment.icon {
                            Image(systemName: icon)
                                .font(.system(size: compact ? 8 : 9, weight: .semibold))
                        }
                        Text(segment.title)
                            .font(.system(size: compact ? 9 : 10, weight: .medium, design: .rounded))
                            .lineLimit(1)
                    }
                    .foregroundStyle(selection == segment.value ? Color.primary : Color.secondary)
                    .padding(.horizontal, compact ? 7 : 10)
                    .padding(.vertical, compact ? 4 : 6)
                    .frame(maxWidth: scrollable ? nil : .infinity)
                    .background {
                        if selection == segment.value {
                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                                .fill(.white.opacity(0.12))
                                .matchedGeometryEffect(id: "vp-segment", in: selectionNamespace)
                        } else if hovered == segment.value {
                            RoundedRectangle(cornerRadius: 7, style: .continuous)
                                .fill(.white.opacity(0.045))
                        }
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(VPPressableButtonStyle(scale: 0.96))
                .onHover { hovering in
                    if hovering {
                        hovered = segment.value
                    } else if hovered == segment.value {
                        hovered = nil
                    }
                }
                .accessibilityAddTraits(selection == segment.value ? .isSelected : [])
            }
        }
    }
}

// MARK: - Status and progress

struct VPStatusDot: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let color: Color
    var pulsing = false
    var size: CGFloat = 7
    @State private var animate = false

    var body: some View {
        ZStack {
            if pulsing {
                Circle()
                    .stroke(color.opacity(0.45), lineWidth: 1)
                    .scaleEffect(animate ? 2.3 : 1)
                    .opacity(animate ? 0 : 0.75)
            }
            Circle()
                .fill(color)
                .frame(width: size, height: size)
        }
        .frame(width: size, height: size)
        .onAppear {
            guard pulsing, !reduceMotion else { return }
            withAnimation(VPMotion.pulse) { animate = true }
        }
        .onChange(of: pulsing) { _, newValue in
            guard !reduceMotion else { return }
            if newValue {
                animate = false
                withAnimation(VPMotion.pulse) { animate = true }
            } else {
                animate = false
            }
        }
    }
}

struct VPThinkingIndicator: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let label: String
    var tint: Color = .green
    @State private var animate = false

    var body: some View {
        HStack(spacing: 8) {
            HStack(spacing: 3) {
                ForEach(0 ..< 3, id: \.self) { index in
                    Circle()
                        .fill(tint)
                        .frame(width: 4, height: 4)
                        .scaleEffect(animate ? 1 : 0.55)
                        .opacity(animate ? 1 : 0.35)
                        .animation(
                            reduceMotion
                                ? nil
                                : VPMotion.pulse.delay(Double(index) * 0.14),
                            value: animate
                        )
                }
            }
            Text(label)
                .font(.system(size: 10, design: .monospaced))
                .foregroundStyle(.secondary)
        }
        .onAppear { animate = true }
    }
}

struct VPStreamingCaret: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    var tint: Color = .green
    @State private var visible = true

    var body: some View {
        RoundedRectangle(cornerRadius: 1, style: .continuous)
            .fill(tint)
            .frame(width: 2, height: 12)
            .opacity(visible ? 1 : 0.15)
            .onAppear {
                guard !reduceMotion else { return }
                withAnimation(VPMotion.caret) { visible.toggle() }
            }
    }
}

struct VPGradientProgress: View {
    let value: Double
    var tint: Color = .accentColor
    var height: CGFloat = 6

    var body: some View {
        GeometryReader { proxy in
            ZStack(alignment: .leading) {
                Capsule().fill(.white.opacity(0.07))
                Capsule()
                    .fill(
                        LinearGradient(
                            colors: [tint.opacity(0.55), tint],
                            startPoint: .leading,
                            endPoint: .trailing
                        )
                    )
                    .frame(width: max(0, min(1, value)) * proxy.size.width)
                    .shadow(color: tint.opacity(0.24), radius: 5, y: 1)
            }
        }
        .frame(height: height)
        .animation(VPMotion.smooth, value: value)
    }
}

struct VPAnimatedNumberText: View {
    let value: String
    var size: CGFloat = 20
    var weight: Font.Weight = .semibold
    var design: Font.Design = .rounded

    var body: some View {
        Text(value)
            .font(.system(size: size, weight: weight, design: design))
            .contentTransition(.numericText())
            .animation(VPMotion.snap, value: value)
    }
}

struct VPCountBadge: View {
    let count: Int
    var tint: Color = .orange
    var compact = false

    var body: some View {
        Text("\(min(count, 99))\(count > 99 ? "+" : "")")
            .font(.system(size: compact ? 8 : 9, weight: .semibold, design: .monospaced))
            .foregroundStyle(tint)
            .padding(.horizontal, compact ? 4 : 5)
            .padding(.vertical, 1)
            .background(tint.opacity(0.16), in: Capsule())
            .contentTransition(.numericText())
            .animation(VPMotion.badge, value: count)
    }
}

// MARK: - Loading and empty states

struct VPSkeleton: View {
    var width: CGFloat?
    var height: CGFloat = 10
    var radius: CGFloat = 5

    var body: some View {
        RoundedRectangle(cornerRadius: radius, style: .continuous)
            .fill(.white.opacity(0.055))
            .frame(width: width, height: height)
            .overlay {
                RoundedRectangle(cornerRadius: radius, style: .continuous)
                    .fill(.clear)
                    .vpShimmer()
            }
    }
}

struct VPFloatingSymbol: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    let name: String
    var size: CGFloat = 30
    var tint: Color = .secondary
    @State private var floating = false

    var body: some View {
        Image(systemName: name)
            .font(.system(size: size, weight: .light))
            .foregroundStyle(
                LinearGradient(
                    colors: [tint.opacity(0.9), tint.opacity(0.45)],
                    startPoint: .topLeading,
                    endPoint: .bottomTrailing
                )
            )
            .symbolRenderingMode(.hierarchical)
            .offset(y: floating && !reduceMotion ? -3 : 2)
            .animation(
                VPMotion.resolved(VPMotion.float, reduceMotion: reduceMotion),
                value: floating
            )
            .onAppear { floating = true }
    }
}

// MARK: - Backdrop

struct VPAuroraBackground: View {
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var drift = false

    var body: some View {
        GeometryReader { proxy in
            ZStack {
                Circle()
                    .fill(Color.green.opacity(0.10))
                    .frame(width: proxy.size.width * 0.58, height: proxy.size.width * 0.58)
                    .blur(radius: 100)
                    .offset(
                        x: drift ? proxy.size.width * 0.18 : -proxy.size.width * 0.16,
                        y: drift ? -proxy.size.height * 0.10 : proxy.size.height * 0.08
                    )
                Circle()
                    .fill(Color.teal.opacity(0.08))
                    .frame(width: proxy.size.width * 0.46, height: proxy.size.width * 0.46)
                    .blur(radius: 90)
                    .offset(
                        x: drift ? -proxy.size.width * 0.20 : proxy.size.width * 0.12,
                        y: drift ? proxy.size.height * 0.16 : -proxy.size.height * 0.06
                    )
                Circle()
                    .fill(Color.purple.opacity(0.06))
                    .frame(width: proxy.size.width * 0.38, height: proxy.size.width * 0.38)
                    .blur(radius: 80)
                    .offset(
                        x: drift ? proxy.size.width * 0.05 : -proxy.size.width * 0.04,
                        y: drift ? proxy.size.height * 0.22 : -proxy.size.height * 0.18
                    )
            }
            .animation(
                VPMotion.resolved(VPMotion.aurora, reduceMotion: reduceMotion),
                value: drift
            )
            .onAppear { drift = true }
        }
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}

// MARK: - Dividers

struct VPGradientDivider: View {
    var tint: Color = .white
    var body: some View {
        Rectangle()
            .fill(
                LinearGradient(
                    colors: [.clear, tint.opacity(0.14), tint.opacity(0.06), .clear],
                    startPoint: .leading,
                    endPoint: .trailing
                )
            )
            .frame(height: 1)
    }
}

struct VPGradientRule: View {
    var vertical = false
    var tint: Color = .white

    var body: some View {
        Rectangle()
            .fill(
                LinearGradient(
                    colors: [.clear, tint.opacity(0.12), tint.opacity(0.05), .clear],
                    startPoint: vertical ? .top : .leading,
                    endPoint: vertical ? .bottom : .trailing
                )
            )
    }
}

struct VPSkeletonRows: View {
    var rows = 3
    var height: CGFloat = 12

    var body: some View {
        VStack(alignment: .leading, spacing: 9) {
            ForEach(0 ..< max(1, rows), id: \.self) { index in
                VPSkeleton(height: height)
                    .frame(maxWidth: index == rows - 1 ? 220 : .infinity)
                    .vpAppear(index: index, animation: VPMotion.hover)
            }
        }
        .padding(14)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }
}
