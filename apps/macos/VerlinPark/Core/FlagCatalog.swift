import AppKit
import SwiftUI

enum PrideOverlay: String, Codable, Hashable {
    case progressChevron
    case intersexCircle
    case demiTriangle
    case polyPi
}

enum PrideFlag: String, Codable, CaseIterable, Identifiable, Hashable {
    case rainbow
    case progress
    case transgender
    case bisexual
    case lesbian
    case nonbinary
    case pansexual
    case asexual
    case aromantic
    case genderfluid
    case genderqueer
    case intersex
    case agender
    case demisexual
    case polyamory

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .rainbow: "Rainbow"
        case .progress: "Progress Pride"
        case .transgender: "Transgender"
        case .bisexual: "Bisexual"
        case .lesbian: "Lesbian"
        case .nonbinary: "Non-binary"
        case .pansexual: "Pansexual"
        case .asexual: "Asexual"
        case .aromantic: "Aromantic"
        case .genderfluid: "Genderfluid"
        case .genderqueer: "Genderqueer"
        case .intersex: "Intersex"
        case .agender: "Agender"
        case .demisexual: "Demisexual"
        case .polyamory: "Polyamory"
        }
    }

    var stripeHexes: [String] {
        switch self {
        case .rainbow, .progress:
            ["#E40303", "#FF8C00", "#FFED00", "#008026", "#004DFF", "#750787"]
        case .transgender:
            ["#5BCEFA", "#F5A9B8", "#FFFFFF", "#F5A9B8", "#5BCEFA"]
        case .bisexual:
            ["#D60270", "#9B4F96", "#0038A8"]
        case .lesbian:
            ["#D52D00", "#EF7627", "#FF9A56", "#FFFFFF", "#D162A4", "#B55690", "#A30262"]
        case .nonbinary:
            ["#FFF430", "#FFFFFF", "#9C59D1", "#000000"]
        case .pansexual:
            ["#FF1B8D", "#FFDA00", "#1BB3FF"]
        case .asexual:
            ["#000000", "#A3A3A3", "#FFFFFF", "#800080"]
        case .aromantic:
            ["#3DA542", "#A7D379", "#FFFFFF", "#A9A9A9", "#000000"]
        case .genderfluid:
            ["#FF75A2", "#FFFFFF", "#BE18D6", "#000000", "#333EBD"]
        case .genderqueer:
            ["#B57EDC", "#FFFFFF", "#4A8123"]
        case .intersex:
            ["#FFD800"]
        case .agender:
            ["#000000", "#B8B8B8", "#FFFFFF", "#B8F483"]
        case .demisexual:
            ["#000000", "#A3A3A3", "#FFFFFF", "#6E0070"]
        case .polyamory:
            ["#0038A8", "#D60270", "#000000"]
        }
    }

    var overlay: PrideOverlay? {
        switch self {
        case .progress: .progressChevron
        case .intersex: .intersexCircle
        case .demisexual: .demiTriangle
        case .polyamory: .polyPi
        default: nil
        }
    }
}

struct FlagOption: Identifiable, Hashable {
    let id: String
    let title: String
    let selection: FlagSelection
}

enum FlagCatalog {
    static let pride: [FlagOption] = PrideFlag.allCases.map { flag in
        FlagOption(
            id: "pride:\(flag.rawValue)",
            title: flag.title,
            selection: .pride(flag)
        )
    }

    static let countries: [FlagOption] = Locale.Region.isoRegions
        .compactMap { region in
            let code = region.identifier
            guard NSImage(named: code) != nil else { return nil }
            let name = Locale.current.localizedString(forRegionCode: code) ?? code
            return FlagOption(
                id: "country:\(code)",
                title: "\(name) · \(code)",
                selection: .country(code)
            )
        }
        .sorted { $0.title.localizedStandardCompare($1.title) == .orderedAscending }

    static func search(_ query: String, in options: [FlagOption]) -> [FlagOption] {
        let trimmed = query.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return options }
        return options.filter {
            $0.title.localizedStandardContains(trimmed)
                || $0.id.localizedStandardContains(trimmed)
        }
    }
}

struct PrideFlagView: View {
    let flag: PrideFlag

    var body: some View {
        GeometryReader { geo in
            ZStack {
                VStack(spacing: 0) {
                    ForEach(Array(flag.stripeHexes.enumerated()), id: \.offset) { _, hex in
                        Color(hex: hex)
                    }
                }

                if let overlay = flag.overlay {
                    overlayView(overlay, size: geo.size)
                }
            }
        }
        .clipped()
    }

    @ViewBuilder
    private func overlayView(_ overlay: PrideOverlay, size: CGSize) -> some View {
        switch overlay {
        case .progressChevron:
            ZStack(alignment: .leading) {
                chevronBand(color: "#FFFFFF", inset: 0.00, apex: 0.62)
                chevronBand(color: "#F5A9B8", inset: 0.07, apex: 0.54)
                chevronBand(color: "#5BCEFA", inset: 0.14, apex: 0.46)
                chevronBand(color: "#7B4B2A", inset: 0.21, apex: 0.38)
                chevronBand(color: "#000000", inset: 0.28, apex: 0.30)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        case .intersexCircle:
            Circle()
                .stroke(Color(hex: "#7A00A8"), lineWidth: max(2, size.height * 0.08))
                .frame(width: size.height * 0.48, height: size.height * 0.48)
        case .demiTriangle:
            Path { path in
                path.move(to: CGPoint(x: 0, y: size.height))
                path.addLine(to: CGPoint(x: size.width * 0.34, y: size.height / 2))
                path.addLine(to: CGPoint(x: 0, y: 0))
                path.closeSubpath()
            }
            .fill(Color(hex: "#000000"))
        case .polyPi:
            Text("π")
                .font(.system(size: size.height * 0.38, weight: .bold, design: .serif))
                .foregroundStyle(.white)
        }
    }

    private func chevronBand(color: String, inset: CGFloat, apex: CGFloat) -> some View {
        ChevronBand(insetFraction: inset, apexFraction: apex)
            .fill(Color(hex: color))
    }
}

private struct ChevronBand: Shape {
    let insetFraction: CGFloat
    let apexFraction: CGFloat

    func path(in rect: CGRect) -> Path {
        let top = rect.height * insetFraction
        let bottom = rect.height * (1 - insetFraction)
        var path = Path()
        path.move(to: CGPoint(x: rect.minX, y: top))
        path.addLine(to: CGPoint(x: rect.minX + rect.width * apexFraction, y: rect.midY))
        path.addLine(to: CGPoint(x: rect.minX, y: bottom))
        path.closeSubpath()
        return path
    }
}

struct FlagSwatch: View {
    let selection: FlagSelection

    var body: some View {
        switch selection {
        case let .country(code):
            if let image = NSImage(named: code) {
                Image(nsImage: image)
                    .resizable()
                    .scaledToFill()
            } else {
                Color.gray.opacity(0.35)
            }
        case let .pride(flag):
            PrideFlagView(flag: flag)
        }
    }
}
