import Foundation
import Observation

enum AppLanguage: String, CaseIterable, Identifiable, Sendable {
    case english = "en"
    case simplifiedChinese = "zh-Hans"

    var id: String {
        rawValue
    }

    var locale: Locale {
        Locale(identifier: rawValue)
    }

    var endonym: String {
        switch self {
        case .english: "English"
        case .simplifiedChinese: "简体中文"
        }
    }

    var shortLabel: String {
        switch self {
        case .english: "EN"
        case .simplifiedChinese: "中"
        }
    }
}

@MainActor
@Observable
final class LanguageStore {
    nonisolated static let storageKey = "verlinpark.language"

    var language: AppLanguage {
        didSet {
            UserDefaults.standard.set(language.rawValue, forKey: Self.storageKey)
        }
    }

    init() {
        if let saved = UserDefaults.standard.string(forKey: Self.storageKey),
           let restored = AppLanguage(rawValue: saved)
        {
            language = restored
            return
        }
        let preferred = Locale.preferredLanguages.first?.lowercased() ?? "en"
        language = preferred.hasPrefix("zh") ? .simplifiedChinese : .english
        UserDefaults.standard.set(language.rawValue, forKey: Self.storageKey)
    }

    var locale: Locale {
        language.locale
    }

    func select(_ language: AppLanguage) {
        self.language = language
    }
}

/// Localization helper for strings that are computed outside a SwiftUI body
/// (section titles, palette entries, keyboard catalog, and dynamic labels).
///
/// Static `Text`/`Button`/`Section` literals are resolved by SwiftUI through the
/// active locale and the `zh-Hans.lproj/Localizable.strings` table. This helper
/// loads the same table explicitly when a plain `String` is required.
enum L10n {
    static var language: AppLanguage {
        let saved = UserDefaults.standard.string(forKey: LanguageStore.storageKey)
        return AppLanguage(rawValue: saved ?? "") ?? .english
    }

    static func t(_ key: String) -> String {
        guard language != .english,
              let path = Bundle.main.path(forResource: language.rawValue, ofType: "lproj"),
              let bundle = Bundle(path: path)
        else {
            return key
        }
        return bundle.localizedString(forKey: key, value: key, table: nil)
    }

    static func format(_ key: String, _ arguments: CVarArg...) -> String {
        String(format: t(key), locale: language.locale, arguments: arguments)
    }
}
