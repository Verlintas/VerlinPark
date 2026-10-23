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

struct BrandMark: View {
    let config: BrandConfig
    var fontSize: CGFloat = 26
    var maxWidth: CGFloat = 220

    var body: some View {
        ZStack(alignment: .leading) {
            Text(config.effectiveName)
                .font(.system(size: fontSize, weight: .bold, design: .rounded))
                .lineLimit(1)
                .minimumScaleFactor(0.42)
                .opacity(0)

            GeometryReader { proxy in
                let band = flagBand(for: proxy.size)
                fill
                    .frame(
                        width: proxy.size.width,
                        height: config.style == .flag ? band.height : proxy.size.height
                    )
                    .offset(y: config.style == .flag ? band.top : 0)
                    .mask {
                        Text(config.effectiveName)
                            .font(.system(size: fontSize, weight: .bold, design: .rounded))
                            .lineLimit(1)
                            .minimumScaleFactor(0.42)
                            .frame(
                                width: proxy.size.width,
                                height: proxy.size.height,
                                alignment: .leading
                            )
                    }
                    .shadow(color: .black.opacity(0.38), radius: 2, y: 1)
            }
        }
        .frame(maxWidth: maxWidth, alignment: .leading)
        .frame(height: fontSize * 1.2, alignment: .leading)
        .padding(.vertical, fontSize * 0.08)
        .accessibilityLabel("\(config.effectiveName) brand")
    }

    @ViewBuilder
    private var fill: some View {
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
            FlagFill(selection: config.flag)
        }
    }

    /// Vertical band occupied by uppercase glyphs (cap height above the
    /// baseline). Aligning the flag to this band shows the full flag through
    /// the letters instead of only the middle stripes.
    /// Vertical band occupied by the visible glyphs. Using the ascender (not
    /// just cap height) keeps lowercase ascenders such as l/k filled; using the
    /// baseline (or descender when needed) maps the bottom flag stripe to the
    /// bottom of the letters.
    private func flagBand(for size: CGSize) -> (height: CGFloat, top: CGFloat) {
        let font = NSFont.systemFont(ofSize: fontSize, weight: .bold)
        let lineHeight = font.ascender - font.descender + font.leading
        let extra = max(0, (size.height - lineHeight) / 2)
        let baseline = font.ascender + extra
        let hasDescender = config.effectiveName.rangeOfCharacter(
            from: CharacterSet(charactersIn: "gjpqy")
        ) != nil
        let bottom = hasDescender ? baseline - font.descender : baseline
        let top = baseline - font.ascender
        return (max(1, bottom - top), top)
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
