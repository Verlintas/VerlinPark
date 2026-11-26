import Darwin
import Foundation

/// Read-only status source used by the Custos instrument.
protocol CustosStatusProviding: Sendable {
    func fetchStatus() async throws -> CustosStatus
}

enum CustosPaths {
    static func dataDirectory(
        environment: [String: String] = ProcessInfo.processInfo.environment,
        homeDirectory: String = NSHomeDirectory()
    ) -> URL {
        if let configured = environment["VERLINPARK_DATA_DIR"], !configured.isEmpty {
            let expanded = NSString(string: configured).expandingTildeInPath
            if expanded.hasPrefix("/") {
                return URL(fileURLWithPath: expanded, isDirectory: true)
            }
            return URL(
                fileURLWithPath: FileManager.default.currentDirectoryPath,
                isDirectory: true
            )
            .appendingPathComponent(expanded, isDirectory: true)
        }
        return URL(fileURLWithPath: homeDirectory, isDirectory: true)
            .appendingPathComponent("Library/Application Support/VerlinPark", isDirectory: true)
    }

    static func custosDirectory(dataDirectory: URL) -> URL {
        dataDirectory.appendingPathComponent("custos", isDirectory: true)
    }

    static func defaultSocketURL(dataDirectory: URL) -> URL {
        custosDirectory(dataDirectory: dataDirectory)
            .appendingPathComponent("custos.sock")
    }

    static func discoveryURL(dataDirectory: URL) -> URL {
        custosDirectory(dataDirectory: dataDirectory)
            .appendingPathComponent("discovery.json")
    }

    static func readDiscovery(dataDirectory: URL) -> CustosDiscoveryRecord? {
        guard let data = try? Data(contentsOf: discoveryURL(dataDirectory: dataDirectory)) else {
            return nil
        }
        return try? JSONDecoder().decode(CustosDiscoveryRecord.self, from: data)
    }

    /// Discovery wins when it is present and well formed; otherwise the
    /// documented default socket path is used.
    static func resolvedSocketPath(dataDirectory: URL) -> String {
        if let record = readDiscovery(dataDirectory: dataDirectory),
           record.isCompatible,
           let socket = record.validSocketPath
        {
            return socket
        }
        return defaultSocketURL(dataDirectory: dataDirectory).path
    }
}

struct CustosDiscoveryRecord: Decodable, Sendable, Equatable {
    static let schema = "verlinpark.custos_discovery/v1"

    let schema: String
    let socket: String

    var isCompatible: Bool {
        schema == Self.schema
    }

    var validSocketPath: String? {
        let path = socket.trimmingCharacters(in: .whitespacesAndNewlines)
        guard path.hasPrefix("/"), path.utf8.count < 104 else { return nil }
        return path
    }
}

struct CustosRPCError: Decodable, Sendable, Equatable {
    let code: String
    let message: String
}

struct CustosRPCEnvelope: Decodable, Sendable, Equatable {
    static let schema = "verlinpark.custos_rpc/v1"

    let schema: String
    let id: Int?
    let ok: Bool
    let result: CustosStatus?
    let error: CustosRPCError?
}

enum CustosSocketError: LocalizedError, Sendable, Equatable {
    case invalidSocketPath
    case unavailable
    case connectFailed(String)
    case writeFailed(String)
    case readFailed(String)
    case timedOut
    case connectionClosed
    case malformedResponse(String)
    case unexpectedSchema(String)
    case rpc(String)

    var errorDescription: String? {
        switch self {
        case .invalidSocketPath:
            "Custos socket path is invalid."
        case .unavailable:
            "Custos socket is unavailable."
        case let .connectFailed(detail):
            "Custos socket connection failed: \(detail)"
        case let .writeFailed(detail):
            "Custos socket write failed: \(detail)"
        case let .readFailed(detail):
            "Custos socket read failed: \(detail)"
        case .timedOut:
            "Custos socket timed out."
        case .connectionClosed:
            "Custos socket closed before a response."
        case let .malformedResponse(detail):
            "Custos response was malformed: \(detail)"
        case let .unexpectedSchema(schema):
            "Custos response schema was \(schema), expected \(CustosRPCEnvelope.schema)."
        case let .rpc(code):
            "Custos returned \(code)."
        }
    }
}

/// One-shot line-delimited JSON client for `custosd`. It only knows the
/// read-only `status` method; mutation verbs do not exist in this type.
struct CustosUnixSocketClient: CustosStatusProviding {
    static let statusRequestLine = #"{"id":1,"method":"status","params":{}}"# + "\n"

    let dataDirectory: URL
    let timeout: TimeInterval

    init(dataDirectory: URL = CustosPaths.dataDirectory(), timeout: TimeInterval = 5) {
        self.dataDirectory = dataDirectory
        self.timeout = max(0.1, timeout)
    }

    func fetchStatus() async throws -> CustosStatus {
        let socketPath = CustosPaths.resolvedSocketPath(dataDirectory: dataDirectory)
        let timeout = timeout
        return try await withCheckedThrowingContinuation { continuation in
            DispatchQueue.global(qos: .utility).async {
                do {
                    let status = try Self.performStatus(socketPath: socketPath, timeout: timeout)
                    continuation.resume(returning: status)
                } catch {
                    continuation.resume(throwing: error)
                }
            }
        }
    }

    // MARK: - Synchronous socket work

    private static func performStatus(socketPath: String, timeout: TimeInterval) throws -> CustosStatus {
        let descriptor = try connect(socketPath: socketPath, timeout: timeout)
        defer { Darwin.close(descriptor) }

        try writeAll(Data(statusRequestLine.utf8), descriptor: descriptor)
        let payload = try readLine(descriptor: descriptor)

        let envelope: CustosRPCEnvelope
        do {
            envelope = try JSONDecoder().decode(CustosRPCEnvelope.self, from: payload)
        } catch {
            throw CustosSocketError.malformedResponse(error.localizedDescription)
        }

        guard envelope.schema == CustosRPCEnvelope.schema else {
            throw CustosSocketError.unexpectedSchema(envelope.schema)
        }
        guard envelope.ok else {
            throw CustosSocketError.rpc(envelope.error?.code ?? "RPC_ERROR")
        }
        guard envelope.id == 1 else {
            throw CustosSocketError.malformedResponse("response id did not match request")
        }
        guard let status = envelope.result else {
            throw CustosSocketError.malformedResponse("response had no result")
        }
        return status
    }

    private static func connect(socketPath: String, timeout: TimeInterval) throws -> Int32 {
        let pathBytes = Array(socketPath.utf8)
        guard !pathBytes.isEmpty, pathBytes.count < 104, socketPath.hasPrefix("/") else {
            throw CustosSocketError.invalidSocketPath
        }

        var address = sockaddr_un()
        address.sun_family = sa_family_t(AF_UNIX)
        #if os(macOS)
            address.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        #endif
        let capacity = MemoryLayout.size(ofValue: address.sun_path)
        withUnsafeMutablePointer(to: &address.sun_path) { pointer in
            pointer.withMemoryRebound(to: CChar.self, capacity: capacity) { buffer in
                for (index, byte) in pathBytes.enumerated() {
                    buffer[index] = CChar(bitPattern: byte)
                }
                buffer[pathBytes.count] = 0
            }
        }

        let descriptor = socket(AF_UNIX, SOCK_STREAM, 0)
        guard descriptor >= 0 else {
            throw CustosSocketError.connectFailed(lastError())
        }

        var keepOpen = false
        defer {
            if !keepOpen {
                Darwin.close(descriptor)
            }
        }

        let existingFlags = fcntl(descriptor, F_GETFL, 0)
        guard existingFlags >= 0, fcntl(descriptor, F_SETFL, existingFlags | O_NONBLOCK) == 0 else {
            throw CustosSocketError.connectFailed(lastError())
        }

        let result = withUnsafePointer(to: &address) { pointer in
            pointer.withMemoryRebound(to: sockaddr.self, capacity: 1) { socketAddress in
                Darwin.connect(
                    descriptor,
                    socketAddress,
                    socklen_t(MemoryLayout<sockaddr_un>.size)
                )
            }
        }

        if result != 0 {
            guard errno == EINPROGRESS else {
                throw CustosSocketError.connectFailed(lastError())
            }
            var pollDescriptor = pollfd(fd: descriptor, events: Int16(POLLOUT), revents: 0)
            let milliseconds = Int32(max(1, timeout * 1000))
            let pollResult = Darwin.poll(&pollDescriptor, 1, milliseconds)
            if pollResult == 0 {
                throw CustosSocketError.timedOut
            }
            guard pollResult > 0 else {
                if errno == EINTR {
                    throw CustosSocketError.unavailable
                }
                throw CustosSocketError.connectFailed(lastError())
            }

            var socketError: Int32 = 0
            var socketErrorSize = socklen_t(MemoryLayout<Int32>.size)
            guard getsockopt(
                descriptor,
                SOL_SOCKET,
                SO_ERROR,
                &socketError,
                &socketErrorSize
            ) == 0 else {
                throw CustosSocketError.connectFailed(lastError())
            }
            guard socketError == 0 else {
                throw CustosSocketError.connectFailed(
                    String(cString: strerror(socketError))
                )
            }
        }

        guard fcntl(descriptor, F_SETFL, existingFlags) == 0 else {
            throw CustosSocketError.connectFailed(lastError())
        }

        var receiveTimeout = timeval(tv_sec: Int(timeout), tv_usec: 0)
        var sendTimeout = timeval(tv_sec: Int(timeout), tv_usec: 0)
        setsockopt(
            descriptor,
            SOL_SOCKET,
            SO_RCVTIMEO,
            &receiveTimeout,
            socklen_t(MemoryLayout<timeval>.size)
        )
        setsockopt(
            descriptor,
            SOL_SOCKET,
            SO_SNDTIMEO,
            &sendTimeout,
            socklen_t(MemoryLayout<timeval>.size)
        )

        var noSignal: Int32 = 1
        setsockopt(
            descriptor,
            SOL_SOCKET,
            SO_NOSIGPIPE,
            &noSignal,
            socklen_t(MemoryLayout<Int32>.size)
        )

        keepOpen = true
        return descriptor
    }

    private static func writeAll(_ data: Data, descriptor: Int32) throws {
        try data.withUnsafeBytes { buffer in
            guard let base = buffer.baseAddress else { return }
            var offset = 0
            while offset < buffer.count {
                let written = Darwin.write(
                    descriptor,
                    base.advanced(by: offset),
                    buffer.count - offset
                )
                if written > 0 {
                    offset += written
                } else if written < 0, errno == EINTR {
                    continue
                } else {
                    throw CustosSocketError.writeFailed(lastError())
                }
            }
        }
    }

    private static func readLine(descriptor: Int32) throws -> Data {
        let limit = 1 << 20
        var data = Data()
        var buffer = [UInt8](repeating: 0, count: 4096)

        while data.count <= limit {
            let count = buffer.withUnsafeMutableBytes { bytes in
                Darwin.read(descriptor, bytes.baseAddress, bytes.count)
            }
            if count > 0 {
                data.append(contentsOf: buffer[0 ..< count])
                if let newline = data.firstIndex(of: 0x0A) {
                    return Data(data[..<newline])
                }
            } else if count == 0 {
                break
            } else if errno == EINTR {
                continue
            } else if errno == EAGAIN || errno == EWOULDBLOCK || errno == ETIMEDOUT {
                throw CustosSocketError.timedOut
            } else {
                throw CustosSocketError.readFailed(lastError())
            }
        }

        if data.isEmpty {
            throw CustosSocketError.connectionClosed
        }
        if data.count > limit {
            throw CustosSocketError.malformedResponse("response exceeded 1 MiB")
        }
        return data
    }

    private static func lastError() -> String {
        String(cString: strerror(errno))
    }
}
