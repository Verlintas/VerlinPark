import Foundation

/// Finds or launches the local `verlinpadd` helper and returns its discovery
/// record. The app is a client; the daemon owns all session state.
///
/// Discovery is only removed after bounded health retries plus positive
/// evidence that the recorded process is dead (or is no longer verlinpadd).
@MainActor
final class DaemonController {
    private let fileManager: FileManager
    private let healthProbe: any DaemonHealthProbe
    private let processEvidence: any DaemonProcessEvidenceProviding
    private let dataDirectory: URL?
    private let locateOverride: (() -> URL?)?
    private let healthRetryDelays: [Duration]
    private var process: Process?

    init(
        dataDirectory: URL? = nil,
        fileManager: FileManager = .default,
        healthProbe: any DaemonHealthProbe = URLSessionDaemonHealthProbe(),
        processEvidence: any DaemonProcessEvidenceProviding = SystemDaemonProcessEvidence(),
        locateOverride: (() -> URL?)? = nil,
        healthRetryDelays: [Duration] = [.zero, .milliseconds(500), .milliseconds(1500)]
    ) {
        self.dataDirectory = dataDirectory
        self.fileManager = fileManager
        self.healthProbe = healthProbe
        self.processEvidence = processEvidence
        self.locateOverride = locateOverride
        self.healthRetryDelays = healthRetryDelays
    }

    var resolvedDataDirectory: URL {
        if let dataDirectory {
            return dataDirectory
        }
        if let path = ProcessInfo.processInfo.environment["VERLINPARK_DATA_DIR"] {
            return URL(fileURLWithPath: path, isDirectory: true)
        }
        let base = fileManager.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first ?? URL(fileURLWithPath: NSHomeDirectory())
        return base.appendingPathComponent("VerlinPark", isDirectory: true)
    }

    var discoveryURL: URL {
        resolvedDataDirectory.appendingPathComponent("run/daemon.json")
    }

    func ensureRunning() async throws -> Discovery {
        if let discovery = readDiscovery() {
            let alive = processEvidence.isAlive(pid: discovery.pid)
            let isDaemon = alive && processEvidence.isVerlinParkDaemon(pid: discovery.pid)
            let decision = DiscoveryRecoveryPolicy.decide(
                healthy: false,
                processAlive: alive,
                isVerlinParkDaemon: isDaemon
            )
            switch decision {
            case .reuse:
                return discovery
            case .keepUnhealthy:
                if await waitForHealth(discovery) {
                    return discovery
                }
                throw CoreError.daemonNotFound(
                    "verlinpadd (pid \(discovery.pid)) is running but /health did not respond after \(healthRetryDelays.count) attempts. The discovery file was preserved. Check \(resolvedDataDirectory.appendingPathComponent("logs/daemon.log").path)."
                )
            case .removeStale:
                // The recorded process is dead or its pid now belongs to a
                // different binary; only then is the record stale.
                try? fileManager.removeItem(at: discoveryURL)
            }
        }

        guard let binary = locateDaemon() else {
            throw CoreError.daemonNotFound(
                "Could not find verlinpadd. Run `just build` (or `cargo build`) and try again."
            )
        }

        try launch(binary)

        for _ in 0 ..< 100 {
            try await Task.sleep(for: .milliseconds(100))
            if let discovery = readDiscovery(),
               await healthProbe.isHealthy(discovery: discovery, timeout: 0.5)
            {
                return discovery
            }
        }

        throw CoreError.daemonNotFound(
            "verlinpadd did not become healthy. Check \(resolvedDataDirectory.appendingPathComponent("logs/daemon.log").path)."
        )
    }

    func readDiscovery() -> Discovery? {
        guard let data = try? Data(contentsOf: discoveryURL) else {
            return nil
        }
        return try? JSONDecoder().decode(Discovery.self, from: data)
    }

    private func waitForHealth(_ discovery: Discovery) async -> Bool {
        for (index, delay) in healthRetryDelays.enumerated() {
            if Task.isCancelled {
                return false
            }
            if index > 0, delay > .zero {
                try? await Task.sleep(for: delay)
            }
            if await healthProbe.isHealthy(discovery: discovery, timeout: 2) {
                return true
            }
        }
        return false
    }

    private func launch(_ binary: URL) throws {
        let logURL = resolvedDataDirectory.appendingPathComponent("logs/daemon.log")
        try fileManager.createDirectory(
            at: logURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        if !fileManager.fileExists(atPath: logURL.path) {
            fileManager.createFile(atPath: logURL.path, contents: nil)
        }
        let handle = try FileHandle(forWritingTo: logURL)
        _ = try handle.seekToEnd()

        let process = Process()
        process.executableURL = binary
        process.standardOutput = handle
        process.standardError = handle
        process.environment = ProcessInfo.processInfo.environment
        try process.run()
        self.process = process
    }

    private func locateDaemon() -> URL? {
        if let locateOverride {
            return locateOverride()
        }

        if let path = ProcessInfo.processInfo.environment["VERLINPARK_DAEMON"],
           fileManager.isExecutableFile(atPath: path)
        {
            return URL(fileURLWithPath: path)
        }

        let bundledHelper = Bundle.main.bundleURL
            .appendingPathComponent("Contents/Helpers/verlinpadd")
        if fileManager.isExecutableFile(atPath: bundledHelper.path) {
            return bundledHelper
        }

        if let bundled = Bundle.main.url(forAuxiliaryExecutable: "verlinpadd") {
            return bundled
        }

        #if DEBUG
            // Development fallback: derive the repository root from this source
            // file and look in Cargo's target directory.
            var root = URL(fileURLWithPath: #filePath)
            for _ in 0 ..< 5 {
                root.deleteLastPathComponent()
            }
            for candidate in ["target/debug/verlinpadd", "target/release/verlinpadd"] {
                let url = root.appendingPathComponent(candidate)
                if fileManager.isExecutableFile(atPath: url.path) {
                    return url
                }
            }

            let cwd = URL(fileURLWithPath: fileManager.currentDirectoryPath)
            for candidate in ["target/debug/verlinpadd", "target/release/verlinpadd"] {
                let url = cwd.appendingPathComponent(candidate)
                if fileManager.isExecutableFile(atPath: url.path) {
                    return url
                }
            }
        #endif

        return nil
    }
}
