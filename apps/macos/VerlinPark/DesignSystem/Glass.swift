import AppKit
import SwiftUI

struct VisualEffectView: NSViewRepresentable {
    var material: NSVisualEffectView.Material = .underWindowBackground
    var blending: NSVisualEffectView.BlendingMode = .behindWindow
    var state: NSVisualEffectView.State = .active

    func makeNSView(context _: Context) -> NSVisualEffectView {
        let view = NSVisualEffectView()
        view.material = material
        view.blendingMode = blending
        view.state = state
        return view
    }

    func updateNSView(_ view: NSVisualEffectView, context _: Context) {
        view.material = material
        view.blendingMode = blending
        view.state = state
    }
}

struct GlassBackground: View {
    var material: NSVisualEffectView.Material = .underWindowBackground

    var body: some View {
        VisualEffectView(material: material, blending: .behindWindow)
            .ignoresSafeArea()
    }
}

struct BrandBackground: View {
    let config: BrandConfig

    var body: some View {
        switch config.style {
        case .solid:
            Color(hex: config.solidHex)
        case .gradient:
            LinearGradient(
                colors: [
                    Color(hex: config.gradientStartHex),
                    Color(hex: config.gradientEndHex),
                ],
                startPoint: gradientPoints.start,
                endPoint: gradientPoints.end
            )
        case .flag:
            ZStack {
                FlagSwatch(selection: config.flag)
                Color.black.opacity(0.26)
                LinearGradient(
                    colors: [.clear, .black.opacity(0.28)],
                    startPoint: .top,
                    endPoint: .bottom
                )
            }
        }
    }

    private var gradientPoints: (start: UnitPoint, end: UnitPoint) {
        let radians = config.gradientAngle * .pi / 180
        let dx = cos(radians) / 2
        let dy = sin(radians) / 2
        return (
            UnitPoint(x: 0.5 - dx, y: 0.5 - dy),
            UnitPoint(x: 0.5 + dx, y: 0.5 + dy)
        )
    }
}

struct BrandMark: View {
    let config: BrandConfig
    var fontSize: CGFloat = 23

    var body: some View {
        Text(config.effectiveName)
            .font(.system(size: fontSize, weight: .semibold, design: .rounded))
            .foregroundStyle(config.prefersDarkText ? Color.black.opacity(0.82) : Color.white)
            .lineLimit(1)
            .minimumScaleFactor(0.55)
            .padding(.horizontal, fontSize * 0.58)
            .padding(.vertical, fontSize * 0.34)
            .background {
                BrandBackground(config: config)
            }
            .clipShape(RoundedRectangle(cornerRadius: fontSize * 0.54, style: .continuous))
            .overlay {
                RoundedRectangle(cornerRadius: fontSize * 0.54, style: .continuous)
                    .strokeBorder(.white.opacity(0.2), lineWidth: 1)
            }
            .shadow(color: .black.opacity(0.28), radius: 14, y: 7)
            .accessibilityLabel("\(config.effectiveName) brand")
    }
}

struct GlassPanel<Content: View>: View {
    var cornerRadius: CGFloat = 20
    @ViewBuilder var content: Content

    var body: some View {
        content
            .background(
                .ultraThinMaterial,
                in: RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
            )
            .overlay {
                RoundedRectangle(cornerRadius: cornerRadius, style: .continuous)
                    .strokeBorder(.white.opacity(0.08), lineWidth: 1)
            }
    }
}

struct SectionLabel: View {
    let title: String

    var body: some View {
        Text(title.uppercased())
            .font(.system(size: 10, weight: .semibold, design: .monospaced))
            .foregroundStyle(.secondary)
            .tracking(0.8)
    }
}
