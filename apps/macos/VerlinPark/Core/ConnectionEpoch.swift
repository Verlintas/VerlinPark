import Foundation

/// Monotonic identity for one WebSocket connection.
///
/// Every intentional `connect`/`disconnect` advances the epoch. A receive loop
/// captures the epoch and socket it was started with, and may only mutate
/// shared client state, fail pending RPCs, or notify the disconnect handler
/// while both still match. This makes a cancelled old loop harmless after a
/// reconnect, while a real server close can still report exactly once.
struct ConnectionEpoch: Equatable, Sendable {
    private(set) var value: UInt64 = 0

    /// Advances the epoch and returns the new generation.
    @discardableResult
    mutating func advance() -> UInt64 {
        value &+= 1
        return value
    }

    /// True when `candidate` is the current generation and the caller still
    /// holds the current socket.
    func isCurrent(_ candidate: UInt64, socketMatches: Bool) -> Bool {
        candidate == value && socketMatches
    }

    /// Claims the right to report a receive failure for `candidate`. The claim
    /// advances the epoch, so a second close/error for the same connection is
    /// stale and cannot report twice. A stale generation or socket is rejected
    /// before advancing.
    mutating func claimFailure(candidate: UInt64, socketMatches: Bool) -> Bool {
        guard isCurrent(candidate, socketMatches: socketMatches) else { return false }
        advance()
        return true
    }
}
