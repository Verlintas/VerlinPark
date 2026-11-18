import Darwin
import Foundation

/// Explicit decision for an existing discovery record. A transient `/health`
/// failure must never delete a live daemon's only discovery file.
enum DiscoveryRecoveryDecision: Equatable {
    case reuse
    case keepUnhealthy
    case removeStale
}

enum DiscoveryRecoveryPolicy {
    static func decide(
        healthy: Bool,
        processAlive: Bool,
        isVerlinParkDaemon: Bool
    ) -> DiscoveryRecoveryDecision {
        if healthy {
            return .reuse
        }
        if processAlive, isVerlinParkDaemon {
            return .keepUnhealthy
        }
        return .removeStale
    }
}

protocol DaemonHealthProbe: Sendable {
    func isHealthy(discovery: Discovery, timeout: TimeInterval) async -> Bool
}

struct URLSessionDaemonHealthProbe: DaemonHealthProbe {
    private struct HealthResponse: Decodable {
        let ok: Bool?
        let version: String?
        let `protocol`: Int?
    }

    func isHealthy(discovery: Discovery, timeout: TimeInterval) async -> Bool {
        guard let url = URL(string: "http://127.0.0.1:\(discovery.port)/health") else {
            return false
        }
        var request = URLRequest(url: url)
        request.timeoutInterval = timeout
        do {
            let (data, response) = try await URLSession.shared.data(for: request)
            guard (response as? HTTPURLResponse)?.statusCode == 200, let health = try? JSONDecoder().decode(
                HealthResponse.self,
                from: data
            ) else {
                return false
            }
            if let ok = health.ok, !ok {
                return false
            }
            // Do not send the discovery token to a port that answers with a
            // different protocol/version than the record describes.
            if let value = health.protocol, value != discovery.protocol {
                return false
            }
            if let version = health.version, version != discovery.version {
                return false
            }
            return true
        } catch {
            return false
        }
    }
}

protocol DaemonProcessEvidenceProviding: Sendable {
    func isAlive(pid: Int) -> Bool
    func isVerlinParkDaemon(pid: Int) -> Bool
}

struct SystemDaemonProcessEvidence: DaemonProcessEvidenceProviding {
    func isAlive(pid: Int) -> Bool {
        guard pid > 0 else { return false }
        if kill(pid_t(pid), 0) == 0 {
            return true
        }
        return errno == EPERM
    }

    func isVerlinParkDaemon(pid: Int) -> Bool {
        guard pid > 0 else { return false }
        var buffer = [UInt8](repeating: 0, count: 4096)
        let length = buffer.withUnsafeMutableBytes { raw in
            proc_pidpath(pid_t(pid), raw.baseAddress, UInt32(raw.count))
        }
        guard length > 0 else { return false }
        let path = String(decoding: buffer.prefix(Int(length)), as: UTF8.self)
        return URL(fileURLWithPath: path).lastPathComponent.contains("verlinpadd")
    }
}
