import Foundation

/// Finds or launches the local `verlinpadd` helper and returns its discovery
/// record. The app is a client; the daemon owns all session state.
@MainActor
final class DaemonController {
    private let fileManager = FileManager.default
    private var process: Process?

    var dataDirectory: URL {
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
        dataDirectory.appendingPathComponent("run/daemon.json")
    }

    func ensureRunning() async throws -> Discovery {
        if let discovery = readDiscovery(), await isHealthy(discovery) {
            return discovery
        }

        try? fileManager.removeItem(at: discoveryURL)

        guard let binary = locateDaemon() else {
            throw CoreError.daemonNotFound(
                "Could not find verlinpadd. Run `just build` (or `cargo build`) and try again."
            )
        }

        try launch(binary)

        for _ in 0 ..< 100 {
            try await Task.sleep(for: .milliseconds(100))
            if let discovery = readDiscovery(), await isHealthy(discovery) {
                return discovery
            }
        }

        throw CoreError.daemonNotFound(
            "verlinpadd did not become healthy. Check \(dataDirectory.appendingPathComponent("logs/daemon.log").path)."
        )
    }

    func readDiscovery() -> Discovery? {
        guard let data = try? Data(contentsOf: discoveryURL) else {
            return nil
        }
        return try? JSONDecoder().decode(Discovery.self, from: data)
    }

    private func isHealthy(_ discovery: Discovery) async -> Bool {
        guard let url = URL(string: "http://127.0.0.1:\(discovery.port)/health") else {
            return false
        }
        var request = URLRequest(url: url)
        request.timeoutInterval = 1
        do {
            let (_, response) = try await URLSession.shared.data(for: request)
            return (response as? HTTPURLResponse)?.statusCode == 200
        } catch {
            return false
        }
    }

    private func launch(_ binary: URL) throws {
        let logURL = dataDirectory.appendingPathComponent("logs/daemon.log")
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
