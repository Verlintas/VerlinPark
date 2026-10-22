import Darwin
import Foundation
import Observation

@MainActor
@Observable
final class MemoryMonitor {
    var appBytes: UInt64 = 0
    var daemonBytes: UInt64 = 0

    private var task: Task<Void, Never>?

    var totalBytes: UInt64 {
        appBytes + daemonBytes
    }

    var text: String {
        let formatted = ByteCountFormatter.string(
            fromByteCount: Int64(totalBytes),
            countStyle: .memory
        )
        return formatted.replacingOccurrences(of: "Zero KB", with: "0 MB")
    }

    func start() {
        guard task == nil else { return }
        refresh()
        task = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(2))
                self?.refresh()
            }
        }
    }

    func refresh() {
        appBytes = Self.residentBytes(pid: getpid())
        if let discovery = readDiscovery() {
            daemonBytes = Self.residentBytes(pid: Int32(discovery.pid))
        } else {
            daemonBytes = 0
        }
    }

    private func readDiscovery() -> Discovery? {
        let base = FileManager.default.urls(
            for: .applicationSupportDirectory,
            in: .userDomainMask
        ).first ?? URL(fileURLWithPath: NSHomeDirectory())
        let url = base
            .appendingPathComponent("VerlinPark", isDirectory: true)
            .appendingPathComponent("run/daemon.json")
        guard let data = try? Data(contentsOf: url) else { return nil }
        return try? JSONDecoder().decode(Discovery.self, from: data)
    }

    private static func residentBytes(pid: Int32) -> UInt64 {
        guard pid > 0 else { return 0 }
        var info = proc_taskinfo()
        let size = MemoryLayout<proc_taskinfo>.size
        let result = proc_pidinfo(
            pid,
            PROC_PIDTASKINFO,
            0,
            &info,
            Int32(size)
        )
        guard result == Int32(size) else { return 0 }
        return info.pti_resident_size
    }
}
