import Foundation

/// Guards the automatic client → daemon provider handoff that runs when the
/// app reconnects.
///
/// The daemon owns its live provider configuration. A reconnect may re-apply
/// settings the app itself saved, but it must not replace a provider that was
/// configured outside this client (for example, a daemon launched with
/// environment variables or edited from another client) just because the
/// client still has stale defaults on disk. Replacing a provider requires an
/// explicit Settings save.
enum ProviderSyncRules {
    static func normalizedEndpoint(_ value: String?) -> String {
        let trimmed = (value ?? "").trimmingCharacters(in: .whitespacesAndNewlines)
        var normalized = trimmed.lowercased()
        while normalized.hasSuffix("/") {
            normalized.removeLast()
        }
        return normalized
    }

    static func normalizedToken(_ value: String?) -> String {
        (value ?? "").trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    }

    /// True only when the stored client provider describes the same endpoint,
    /// provider kind, and model the daemon already reports.
    static func matchesDaemon(
        kind: String,
        baseURL: String,
        model: String,
        daemonProvider: String?,
        daemonBaseURL: String?,
        daemonModel: String?
    ) -> Bool {
        let kind = normalizedToken(kind)
        let model = normalizedToken(model)
        let provider = normalizedToken(daemonProvider)
        let daemonModel = normalizedToken(daemonModel)
        guard !kind.isEmpty, !model.isEmpty, !provider.isEmpty, !daemonModel.isEmpty, kind == provider,
              model == daemonModel else { return false }
        return normalizedEndpoint(baseURL) == normalizedEndpoint(daemonBaseURL)
    }

    /// An automatic reconnect applies stored settings only to an unconfigured
    /// daemon, or when the stored settings already match the daemon. A mismatch
    /// is an explicit operator decision, never an automatic overwrite.
    static func shouldApplyOnConnect(
        daemonConfigured: Bool,
        kind: String,
        baseURL: String,
        model: String,
        daemonProvider: String?,
        daemonBaseURL: String?,
        daemonModel: String?
    ) -> Bool {
        guard daemonConfigured else { return true }
        return matchesDaemon(
            kind: kind,
            baseURL: baseURL,
            model: model,
            daemonProvider: daemonProvider,
            daemonBaseURL: daemonBaseURL,
            daemonModel: daemonModel
        )
    }
}
