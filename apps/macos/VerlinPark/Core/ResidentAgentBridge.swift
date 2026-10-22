import Foundation

/// Reserved integration seam for NovaBAIC's Aviiya.
///
/// VerlinPark does not implement a local Aviiya runtime. The resident surface
/// stays visible for layout purposes, but the actual assistant will be provided
/// by NovaBAIC through a future bridge (shared memory, tools, and sessions).
enum ResidentAgentStatus: String, Sendable {
    case unavailable
    case connecting
    case ready
}

protocol ResidentAgentBridge: Sendable {
    var displayName: String { get }
    var status: ResidentAgentStatus { get }
    func send(_ message: String) async throws -> AsyncStream<String>
}

struct UnavailableResidentAgent: ResidentAgentBridge {
    let displayName = "Aviiya"
    let status = ResidentAgentStatus.unavailable

    func send(_: String) async throws -> AsyncStream<String> {
        AsyncStream { continuation in
            continuation.finish()
        }
    }
}
