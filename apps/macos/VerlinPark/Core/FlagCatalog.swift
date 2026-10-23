import AppKit
import SwiftUI

/// Pride flag artwork from Wikimedia Commons.
/// Per-file attribution and licenses: `docs/PRIDE_FLAGS.md`.
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
        case .rainbow: "PrideFlag_rainbow"
        case .progress: "PrideFlag_Progress"
        case .intersexProgress: "PrideFlag_intersexProgress"
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

    var sourceTitle: String {
        switch self {
        case .rainbow: "Gay Pride Flag.svg"
        case .progress: "LGBTQ+ rainbow flag Quasar \"Progress\" variant.svg"
        case .intersexProgress: "Intersex-inclusive pride flag.svg"
        case .transgender: "Transgender Pride flag.svg"
        case .bisexual: "Bisexual Pride Flag.svg"
        case .lesbian: "Lesbian Pride Flag 2019.svg"
        case .nonbinary: "Non-binary Pride Flag.png"
        case .pansexual: "Pansexual Pride Flag (3-2 aspect ratio).svg"
        case .asexual: "Asexual Pride Flag.svg"
        case .aromantic: "Aromantic Pride Flag.svg"
        case .aroace: "Aroace flag.svg"
        case .genderfluid: "Genderfluid Flag (Square).png"
        case .genderqueer: "Genderqueer Pride Flag.svg"
        case .intersex: "Intersex Pride Flag.svg"
        case .agender: "Agender pride flag.svg"
        case .demisexual: "Demisexual Pride Flag.svg"
        case .polyamory: "Polyamory flag with infinity heart.svg"
        case .sapphic: "Sapphic pride flag.svg"
        case .queer: "Queer pride flag.svg"
        case .twoSpirit: "Two-Spirit Flag.svg"
        }
    }

    var sourceURL: URL? {
        let encoded = sourceTitle.addingPercentEncoding(
            withAllowedCharacters: .urlPathAllowed
        ) ?? sourceTitle
        return URL(string: "https://commons.wikimedia.org/wiki/File:\(encoded)")
    }

    var image: NSImage? {
        if let url = Bundle.main.url(forResource: assetName, withExtension: "png") {
            return NSImage(contentsOf: url)
        }
        return NSImage(named: assetName)
    }
}

enum FlagFillMode: Equatable {
    /// Stretch the flag to the glyph band. Best for pure stripe flags.
    case stretch
    /// Preserve the flag's aspect ratio and crop to the glyph band.
    case cover
}

extension PrideFlag {
    var fillMode: FlagFillMode {
        switch self {
        case .rainbow, .transgender, .bisexual, .lesbian, .nonbinary,
             .pansexual, .asexual, .aromantic, .aroace, .genderfluid,
             .genderqueer, .agender, .queer:
            .stretch
        case .progress, .intersexProgress, .intersex, .demisexual,
             .polyamory, .sapphic, .twoSpirit:
            .cover
        }
    }
}

extension FlagSelection {
    var fillMode: FlagFillMode {
        switch self {
        case .region:
            .cover
        case let .pride(flag):
            flag.fillMode
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

    static let regions: [FlagOption] = Locale.Region.isoRegions
        .compactMap { region in
            let code = region.identifier
            guard NSImage(named: code) != nil else { return nil }
            let name = Locale.current.localizedString(forRegionCode: code) ?? code
            return FlagOption(
                id: "region:\(code)",
                title: "\(name) · \(code)",
                selection: .region(code)
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

/// Fills the glyph band with a flag. Stripe flags are stretched; patterned
/// flags preserve their aspect ratio and are cropped to the band so emblems are
/// not distorted.
struct FlagFill: View {
    let selection: FlagSelection

    var body: some View {
        if let image {
            switch selection.fillMode {
            case .stretch:
                Image(nsImage: image)
                    .resizable()
            case .cover:
                Image(nsImage: image)
                    .resizable()
                    .scaledToFill()
                    .clipped()
            }
        } else {
            Color.gray.opacity(0.35)
        }
    }

    private var image: NSImage? {
        switch selection {
        case let .region(code):
            NSImage(named: code)
        case let .pride(flag):
            flag.image
        }
    }
}

struct FlagSwatch: View {
    let selection: FlagSelection

    var body: some View {
        switch selection {
        case let .region(code):
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
