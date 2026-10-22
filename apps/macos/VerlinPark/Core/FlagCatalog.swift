import AppKit
import SwiftUI

/// CC0 pride flag artwork from Yellow-Dog-Man/PrideFlags.
/// `rawValue` is persisted in brand settings; do not rename cases.
enum PrideFlag: String, Codable, CaseIterable, Identifiable, Hashable {
    case rainbow
    case progress
    case intersexProgress
    case transgender
    case bisexual
    case lesbian
    case nonbinary
    case pansexual
    case asexual
    case aromantic
    case aroace
    case genderfluid
    case genderqueer
    case intersex
    case agender
    case demisexual
    case polyamory
    case sapphic
    case queer
    case twoSpirit

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .rainbow: "Rainbow"
        case .progress: "Progress Pride"
        case .intersexProgress: "Intersex-Inclusive Progress"
        case .transgender: "Transgender"
        case .bisexual: "Bisexual"
        case .lesbian: "Lesbian"
        case .nonbinary: "Non-binary"
        case .pansexual: "Pansexual"
        case .asexual: "Asexual"
        case .aromantic: "Aromantic"
        case .aroace: "Aroace"
        case .genderfluid: "Genderfluid"
        case .genderqueer: "Genderqueer"
        case .intersex: "Intersex"
        case .agender: "Agender"
        case .demisexual: "Demisexual"
        case .polyamory: "Polyamory"
        case .sapphic: "Sapphic"
        case .queer: "Queer"
        case .twoSpirit: "Two-Spirit"
        }
    }

    var assetName: String {
        switch self {
        case .rainbow: "PrideFlag_Traditional"
        case .progress: "PrideFlag_Progress"
        case .intersexProgress: "PrideFlag_Intersex-InclusiveProgress"
        case .transgender: "PrideFlag_Transgender"
        case .bisexual: "PrideFlag_Bisexual"
        case .lesbian: "PrideFlag_Lesbian"
        case .nonbinary: "PrideFlag_Nonbinary"
        case .pansexual: "PrideFlag_Pansexual"
        case .asexual: "PrideFlag_Asexual"
        case .aromantic: "PrideFlag_Aromantic"
        case .aroace: "PrideFlag_Aroace"
        case .genderfluid: "PrideFlag_Genderfluid"
        case .genderqueer: "PrideFlag_Genderqueer"
        case .intersex: "PrideFlag_Intersex"
        case .agender: "PrideFlag_Agender"
        case .demisexual: "PrideFlag_Demisexual"
        case .polyamory: "PrideFlag_Polyamory"
        case .sapphic: "PrideFlag_Sapphic"
        case .queer: "PrideFlag_Queer"
        case .twoSpirit: "PrideFlag_TwoSpirit"
        }
    }

    var image: NSImage? {
        if let url = Bundle.main.url(forResource: assetName, withExtension: "png") {
            return NSImage(contentsOf: url)
        }
        return NSImage(named: assetName)
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

struct PrideFlagImage: View {
    let flag: PrideFlag

    var body: some View {
        if let image = flag.image {
            Image(nsImage: image)
                .resizable()
                .scaledToFill()
        } else {
            Color.gray.opacity(0.35)
        }
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
            PrideFlagImage(flag: flag)
        }
    }
}
