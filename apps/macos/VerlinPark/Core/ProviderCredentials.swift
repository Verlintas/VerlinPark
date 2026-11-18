import Foundation

/// Minimal secret store seam so provider key behavior can be tested with a fake
/// and so Keychain failures never silently reconfigure the daemon provider.
protocol SecretStore: Sendable {
    func read(account: String) -> String?
    func save(_ value: String, account: String) throws
    func delete(account: String)
}

struct KeychainSecretStore: SecretStore {
    func read(account: String) -> String? {
        Keychain.read(account: account)
    }

    func save(_ value: String, account: String) throws {
        try Keychain.save(value, account: account)
    }

    func delete(account: String) {
        Keychain.delete(account: account)
    }
}

/// What the app can send to `agent.configure` on this attempt.
enum ProviderCredential: Equatable {
    /// A typed or stored key is available.
    case key(String)
    /// The endpoint is local and does not need an API key.
    case notRequired
    /// UserDefaults says a key is stored but the Keychain item is missing or
    /// unreadable. The daemon configuration must be left unchanged.
    case missingStoredKey
    /// No key is typed or stored. Explicit save may intentionally configure an
    /// unauthenticated endpoint; reconnect must not clobber a live daemon.
    case notConfigured
}

enum ProviderCredentialResolver {
    static func resolve(
        typedKey: String,
        hasStoredKeyFlag: Bool,
        endpointRequiresKey: Bool,
        store: any SecretStore,
        account: String
    ) -> ProviderCredential {
        let typed = typedKey.trimmingCharacters(in: .whitespacesAndNewlines)
        if !typed.isEmpty {
            return .key(typed)
        }
        if !endpointRequiresKey {
            return .notRequired
        }
        guard hasStoredKeyFlag else {
            return .notConfigured
        }
        guard
            let stored = store.read(account: account)?
                .trimmingCharacters(in: .whitespacesAndNewlines),
            !stored.isEmpty
        else {
            return .missingStoredKey
        }
        return .key(stored)
    }
}
