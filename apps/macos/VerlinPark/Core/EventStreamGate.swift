import Foundation

/// Owns the single app-wide daemon event stream.
///
/// A stream is identified by a monotonically increasing generation. An older
/// stream's `onTermination` callback can therefore never clear a newer
/// continuation, which otherwise leaves the app connected while live updates
/// silently stop. Events that arrive before a consumer attaches are buffered
/// (bounded) and replayed on installation so `event.subscribe` can happen
/// before `events()` without dropping approvals or questions.
struct EventStreamGate {
    struct Installation {
        let generation: UInt64
        let buffered: [Event]
    }

    let maxBufferedEvents: Int
    private(set) var generation: UInt64 = 0
    private(set) var continuation: AsyncStream<Event>.Continuation?
    private(set) var buffered: [Event] = []

    init(maxBufferedEvents: Int = 4096) {
        self.maxBufferedEvents = max(1, maxBufferedEvents)
    }

    /// Installs a new continuation, finishes any predecessor, and returns the
    /// events that arrived while no consumer was attached. Callers must yield
    /// the returned buffer after installing their loop.
    mutating func install(_ continuation: AsyncStream<Event>.Continuation) -> Installation {
        self.continuation?.finish()
        generation &+= 1
        self.continuation = continuation
        let replay = buffered
        buffered.removeAll(keepingCapacity: true)
        return Installation(generation: generation, buffered: replay)
    }

    /// Clears the continuation only when `generation` is still current.
    mutating func clear(generation: UInt64) {
        guard self.generation == generation else { return }
        continuation = nil
    }

    /// Yields to the active consumer or buffers up to the bound. Older events
    /// are dropped first; the app reconciles projections after reconnect.
    mutating func enqueue(_ event: Event) {
        if let continuation {
            continuation.yield(event)
            return
        }
        buffered.append(event)
        if buffered.count > maxBufferedEvents {
            buffered.removeFirst(buffered.count - maxBufferedEvents)
        }
    }

    /// Finishes the active stream and drops buffered events. Used on
    /// disconnect so a new connection starts from a clean boundary.
    mutating func finishCurrent() {
        continuation?.finish()
        continuation = nil
        buffered.removeAll(keepingCapacity: true)
    }
}
