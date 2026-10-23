import AppKit
import SwiftUI

enum BrandStyle: String, Codable, CaseIterable, Identifiable {
    case solid
    case gradient
    case flag

    var id: String {
        rawValue
    }

    var title: String {
        switch self {
        case .solid: "Solid"
        case .gradient: "Gradient"
        case .flag: "Flag"
        }
    }

    var icon: String {
        switch self {
        case .solid: "circle.fill"
        case .gradient: "circle.lefthalf.filled"
        case .flag: "flag.fill"
        }
    }
}

enum FlagSelection: Codable, Hashable {
    case country(String)
    case pride(PrideFlag)

    var sourceURL: URL? {
        guard case let .pride(flag) = self else { return nil }
        return flag.sourceURL
    }
}

struct BrandConfig: Codable, Equatable {
    var displayName: String = "VerlinPark"
    var style: BrandStyle = .gradient
    var solidHex: String = "#1F6F4A"
    var gradientStartHex: String = "#1E7A53"
    var gradientEndHex: String = "#0B1F16"
    var gradientAngle: Double = 135
    var flag: FlagSelection = .pride(.progress)

    static let presets = [
        "VerlinPark",
        "VerlinBase",
        "VerlinHome",
        "VerlinOffice",
        "VerlinStudio",
        "VerlinLab",
    ]

    var effectiveName: String {
        let trimmed = displayName.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? "VerlinPark" : trimmed
    }
}

@MainActor
@Observable
final class BrandStore {
    private static let storageKey = "verlinpark.brand.v1"

    var config: BrandConfig {
        didSet { save() }
    }

    init() {
        if let data = UserDefaults.standard.data(forKey: Self.storageKey),
           let decoded = try? JSONDecoder().decode(BrandConfig.self, from: data)
        {
            config = decoded
        } else {
            config = BrandConfig()
        }
    }

    func reset() {
        config = BrandConfig()
    }

    private func save() {
        guard let data = try? JSONEncoder().encode(config) else { return }
        UserDefaults.standard.set(data, forKey: Self.storageKey)
    }
}

extension Color {
    init(hex: String) {
        let cleaned = hex.trimmingCharacters(in: CharacterSet.alphanumerics.inverted)
        var value: UInt64 = 0
        Scanner(string: cleaned).scanHexInt64(&value)
        let r, g, b, a: Double
        switch cleaned.count {
        case 3:
            r = Double((value >> 8) & 0xF) / 15
            g = Double((value >> 4) & 0xF) / 15
            b = Double(value & 0xF) / 15
            a = 1
        case 6:
            r = Double((value >> 16) & 0xFF) / 255
            g = Double((value >> 8) & 0xFF) / 255
            b = Double(value & 0xFF) / 255
            a = 1
        case 8:
            r = Double((value >> 24) & 0xFF) / 255
            g = Double((value >> 16) & 0xFF) / 255
            b = Double((value >> 8) & 0xFF) / 255
            a = Double(value & 0xFF) / 255
        default:
            r = 0.12
            g = 0.45
            b = 0.30
            a = 1
        }
        self.init(.sRGB, red: r, green: g, blue: b, opacity: a)
    }

    var relativeLuminance: Double {
        let color = NSColor(self).usingColorSpace(.sRGB) ?? .black
        func linear(_ component: CGFloat) -> Double {
            let value = Double(component)
            return value <= 0.03928 ? value / 12.92 : pow((value + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * linear(color.redComponent)
            + 0.7152 * linear(color.greenComponent)
            + 0.0722 * linear(color.blueComponent)
    }

    var hexString: String {
        let color = NSColor(self).usingColorSpace(.sRGB) ?? .black
        return String(
            format: "#%02X%02X%02X",
            Int(round(color.redComponent * 255)),
            Int(round(color.greenComponent * 255)),
            Int(round(color.blueComponent * 255))
        )
    }
}
