import Foundation
import Observation

/// Main-actor state for the sealed Custos instrument. The model owns polling
/// and presentation; the client owns the read-only socket round-trip.
@MainActor
@Observable
final class CustosModel {
    enum Availability: Equatable, Sendable {
        case live
        case signalLost
    }

    private(set) var availability: Availability = .signalLost
    private(set) var status: CustosStatus?
    private(set) var lastError: String?
    private(set) var isRefreshing = false

    @ObservationIgnored private let provider: any CustosStatusProviding
    @ObservationIgnored private let diagnosticsProvider: (any CustosDiagnosticsProviding)?
    @ObservationIgnored private let pollInterval: Duration
    @ObservationIgnored private var pollTask: Task<Void, Never>?

    private(set) var diagnostics: CustosDiagnosticsResult?
    private(set) var diagnosticsError: String?
    private(set) var isGeneratingDiagnostics = false

    init(
        provider: any CustosStatusProviding = CustosUnixSocketClient(),
        diagnosticsProvider: (any CustosDiagnosticsProviding)? = nil,
        pollInterval: Duration = .seconds(2)
    ) {
        self.provider = provider
        self.diagnosticsProvider = diagnosticsProvider ?? (provider as? any CustosDiagnosticsProviding)
        self.pollInterval = pollInterval
    }

    var state: CustosState {
        guard availability == .live, let status else { return .signalLost }
        return status.state
    }

    var stateWord: String {
        state.word
    }

    var stateSentence: String {
        status?.sentence ?? "The read-only Custos socket did not answer. No live state is asserted."
    }

    var dimensionRows: [CustosDimensionRow] {
        status?.dimensionRows ?? CustosDimensionRow.placeholders
    }

    var countItems: [CustosCountItem] {
        status?.countItems ?? Self.placeholderCounts
    }

    var ringLine: String {
        status?.ringLine ?? "UNKNOWN  /  FRESH --  /  STALE --  /  MISSING --  /  QUORUM --"
    }

    var roleLabel: String {
        guard let role = status?.role, !role.isEmpty else { return "--" }
        return role.uppercased()
    }

    var observeOnlyLabel: String {
        guard let status else { return "OBSERVE ONLY" }
        return status.observeOnly ? "OBSERVE ONLY" : "READ ONLY"
    }

    /// Product tier, not the legacy observe-only flag. Advanced is only
    /// reported when the root-owned marker is present; this label never grants
    /// authority.
    var tierLabel: String {
        status?.tierWord ?? "OBSERVER"
    }

    var charterLabel: String {
        status?.shortCharter ?? "--"
    }

    var generatedLabel: String {
        status?.generatedLabel ?? "--"
    }

    var protectionLabel: String {
        availability == .live ? "PROTECTION ACTIVE" : "SIGNAL LOST"
    }

    var menuBarSymbol: String {
        guard availability == .live, let status else { return "diamond" }
        return status.state == .signalLost || status.state == .unknown ? "diamond" : "diamond.fill"
    }

    var menuBarAccessibilityLabel: String {
        "CUSTOS \(stateWord)"
    }

    func start() {
        guard pollTask == nil else { return }
        pollTask = Task { [weak self] in
            guard let self else { return }
            await refreshOnce()
            while !Task.isCancelled {
                do {
                    try await Task.sleep(for: pollInterval)
                } catch {
                    break
                }
                if Task.isCancelled {
                    break
                }
                await refreshOnce()
            }
        }
    }

    func stop() {
        pollTask?.cancel()
        pollTask = nil
    }

    /// Explicit user-consented diagnostic export. This writes local files only;
    /// there is no upload, network call, or caller-supplied path.
    func generateDiagnostics() async {
        guard !isGeneratingDiagnostics else { return }
        guard let diagnosticsProvider else {
            diagnosticsError = "This Custos build has no diagnostic exporter."
            return
        }
        isGeneratingDiagnostics = true
        diagnosticsError = nil
        defer { isGeneratingDiagnostics = false }
        do {
            diagnostics = try await diagnosticsProvider.generateDiagnostics()
        } catch {
            diagnostics = nil
            diagnosticsError = error.localizedDescription
        }
    }

    /// One explicit poll. Tests use this without starting the timer.
    func refreshOnce() async {
        guard !isRefreshing else { return }
        isRefreshing = true
        defer { isRefreshing = false }

        do {
            let fetched = try await provider.fetchStatus()
            guard !Task.isCancelled else { return }
            status = fetched
            availability = .live
            lastError = nil
        } catch {
            guard !Task.isCancelled else { return }
            status = nil
            availability = .signalLost
            lastError = error.localizedDescription
        }
    }

    private static let placeholderCounts: [CustosCountItem] = [
        CustosCountItem(id: "sessions", label: "SESSIONS", value: "--"),
        CustosCountItem(id: "running", label: "RUNNING", value: "--"),
        CustosCountItem(id: "paused", label: "PAUSED", value: "--"),
        CustosCountItem(id: "failed", label: "FAILED", value: "--"),
        CustosCountItem(id: "tasks", label: "TASKS PENDING", value: "--"),
        CustosCountItem(id: "queued", label: "MERGES QUEUED", value: "--"),
        CustosCountItem(id: "conflicts", label: "MERGE CONFLICTS", value: "--"),
        CustosCountItem(id: "effects", label: "EFFECTS UNKNOWN", value: "--"),
        CustosCountItem(id: "approvals", label: "APPROVALS", value: "--"),
        CustosCountItem(id: "leases", label: "LEASES EXPIRED", value: "--"),
        CustosCountItem(id: "event-age", label: "EVENT AGE", value: "--"),
    ]
}
