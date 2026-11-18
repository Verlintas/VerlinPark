@testable import VerlinPark
import XCTest

final class RuntimeReliabilityTests: XCTestCase {
    // MARK: - Event stream generation

    func testEventsBufferUntilConsumerAttaches() {
        var gate = EventStreamGate()
        gate.enqueue(makeEvent(seq: 1))
        gate.enqueue(makeEvent(seq: 2))

        let (stream, continuation) = AsyncStream<Event>.makeStream()
        let installation = gate.install(continuation)

        XCTAssertEqual(installation.buffered.map(\.seq), [1, 2])
        XCTAssertEqual(gate.generation, 1)
        XCTAssertTrue(gate.buffered.isEmpty)
        continuation.finish()
        _ = stream
    }

    func testStaleTerminationDoesNotClearNewContinuation() async {
        var gate = EventStreamGate()
        let (firstStream, firstContinuation) = AsyncStream<Event>.makeStream()
        let first = gate.install(firstContinuation)
        XCTAssertEqual(first.generation, 1)

        let (secondStream, secondContinuation) = AsyncStream<Event>.makeStream()
        let second = gate.install(secondContinuation)
        XCTAssertEqual(second.generation, 2)

        // The old stream's onTermination arrives late.
        gate.clear(generation: first.generation)
        XCTAssertEqual(gate.generation, 2)
        XCTAssertNotNil(gate.continuation)

        gate.enqueue(makeEvent(seq: 7))
        var secondIterator = secondStream.makeAsyncIterator()
        let received = await secondIterator.next()
        XCTAssertEqual(received?.seq, 7)

        gate.clear(generation: second.generation)
        XCTAssertNil(gate.continuation)
        firstContinuation.finish()
        _ = firstStream
    }

    func testDisconnectFinishesStreamAndDropsBuffer() {
        var gate = EventStreamGate()
        gate.enqueue(makeEvent(seq: 1))
        let (stream, continuation) = AsyncStream<Event>.makeStream()
        _ = gate.install(continuation)
        gate.finishCurrent()
        XCTAssertNil(gate.continuation)
        XCTAssertTrue(gate.buffered.isEmpty)
        _ = stream
    }

    // MARK: - Connection generation

    func testStaleReceiveLoopIsRejectedAfterReconnect() {
        var epoch = ConnectionEpoch()
        let oldLoop = epoch.advance() // connect A
        epoch.advance() // intentional disconnect
        let newLoop = epoch.advance() // connect B

        XCTAssertFalse(
            epoch.isCurrent(oldLoop, socketMatches: true),
            "the cancelled loop must not observe the new connection"
        )
        XCTAssertTrue(epoch.isCurrent(newLoop, socketMatches: true))
    }

    func testIntentionalDisconnectInvalidatesLoopFailureClaim() {
        var epoch = ConnectionEpoch()
        let loop = epoch.advance()
        epoch.advance() // disconnect before the cancelled loop reports

        XCTAssertFalse(
            epoch.claimFailure(candidate: loop, socketMatches: true),
            "an intentional disconnect must not trigger the reconnect handler"
        )
        XCTAssertEqual(epoch.value, 2)
    }

    func testServerCloseReportsExactlyOnceForCurrentGeneration() {
        var epoch = ConnectionEpoch()
        let loop = epoch.advance()

        XCTAssertTrue(epoch.claimFailure(candidate: loop, socketMatches: true))
        XCTAssertFalse(
            epoch.claimFailure(candidate: loop, socketMatches: true),
            "a second close/error for the same connection is stale"
        )
    }

    func testSupersededSocketIsStaleEvenForCurrentGeneration() {
        var epoch = ConnectionEpoch()
        let loop = epoch.advance()

        XCTAssertFalse(epoch.isCurrent(loop, socketMatches: false))
        XCTAssertFalse(epoch.claimFailure(candidate: loop, socketMatches: false))
        XCTAssertEqual(epoch.value, 1, "a rejected claim must not advance")
        XCTAssertTrue(epoch.isCurrent(loop, socketMatches: true))
    }

    // MARK: - Session create partial success

    func testSessionStartFailureMapsTypedPartialSuccess() {
        let error = CoreError.rpc(
            code: -32603,
            message: "session ses-1 was created but agent.start failed: provider missing",
            data: .object([
                "code": .string("SESSION_START_FAILED"),
                "phase": .string("agent.start"),
                "session_id": .string("ses-1"),
                "project_id": .string("prj-1"),
                "status": .string("idle"),
                "session_created": .bool(true),
                "retryable": .bool(true),
                "hint": .string("the session exists; retry session.prompt"),
            ])
        )

        let failure = SessionCreateErrorMapper.partialSuccess(from: error)
        XCTAssertEqual(failure?.sessionID, "ses-1")
        XCTAssertEqual(failure?.projectID, "prj-1")
        XCTAssertEqual(failure?.phase, "agent.start")
        XCTAssertEqual(failure?.status, "idle")
        XCTAssertTrue(failure?.retryable == true)
        XCTAssertTrue(
            failure?.warningDetail.contains("leader failed to start") == true
        )
        XCTAssertTrue(failure?.warningDetail.contains("agent.start") == true)
        XCTAssertTrue(failure?.warningDetail.contains("provider missing") == true)
        XCTAssertTrue(failure?.warningDetail.contains("ses-1") == true)
        XCTAssertLessThanOrEqual(failure?.warningDetail.count ?? 0, 400)
    }

    func testSessionStartFailureRequiresAllTypedGuarantees() {
        func data(_ overrides: [String: JSONValue]) -> JSONValue {
            var object: [String: JSONValue] = [
                "code": .string("SESSION_START_FAILED"),
                "phase": .string("agent.start"),
                "session_id": .string("ses-1"),
                "session_created": .bool(true),
            ]
            for (key, value) in overrides {
                object[key] = value
            }
            return .object(object)
        }
        func error(_ payload: JSONValue) -> CoreError {
            .rpc(code: -32603, message: "start failed", data: payload)
        }

        XCTAssertNil(
            SessionCreateErrorMapper.partialSuccess(
                from: error(data(["code": .string("OTHER_CODE")]))
            )
        )
        XCTAssertNil(
            SessionCreateErrorMapper.partialSuccess(
                from: error(data(["session_created": .bool(false)]))
            )
        )
        XCTAssertNil(
            SessionCreateErrorMapper.partialSuccess(
                from: error(data(["session_id": .string("")]))
            )
        )
        XCTAssertNil(
            SessionCreateErrorMapper.partialSuccess(
                from: error(data(["session_id": .string("   ")]))
            )
        )
        XCTAssertNil(
            SessionCreateErrorMapper.partialSuccess(
                from: CoreError.rpc(
                    code: -32603,
                    message: "generic",
                    data: .object([:])
                )
            )
        )
        XCTAssertNil(SessionCreateErrorMapper.partialSuccess(from: CoreError.timeout))
    }

    // MARK: - Event gaps

    func testEventGapDecodesLaggedPayload() {
        let gap = EventGap.decode(from: .object([
            "skipped": .number(2048),
            "truncated": .bool(false),
            "last_seq": .number(3),
            "last_session_id": .string("ses-1"),
            "resume_from_seq": .number(4),
            "session_id": .null,
            "resync_required": .bool(true),
            "reason": .string("lagged"),
        ]))

        XCTAssertEqual(gap?.skipped, 2048)
        XCTAssertEqual(gap?.truncated, false)
        XCTAssertEqual(gap?.lastSeq, 3)
        XCTAssertEqual(gap?.lastSessionID, "ses-1")
        XCTAssertNil(gap?.sessionID)
        XCTAssertEqual(gap?.targetSessionID, "ses-1")
        XCTAssertEqual(gap?.effectiveResumeFromSeq, 4)
        XCTAssertTrue(gap?.resyncRequired == true)
        XCTAssertEqual(gap?.reason, "lagged")
        XCTAssertTrue(gap?.targets(sessionID: "ses-1") == true)
        XCTAssertFalse(gap?.targets(sessionID: "ses-2") == true)
        XCTAssertFalse(gap?.targets(sessionID: nil) == true)
    }

    func testEventGapTruncatedBackfillHasNoInventedSkippedCount() {
        let gap = EventGap.decode(from: .object([
            "skipped": .null,
            "truncated": .bool(true),
            "last_seq": .number(500),
            "last_session_id": .string("ses-1"),
            "resume_from_seq": .number(501),
            "session_id": .string("ses-1"),
            "resync_required": .bool(true),
            "reason": .string("backfill_truncated"),
        ]))

        XCTAssertNil(gap?.skipped)
        XCTAssertEqual(gap?.truncated, true)
        XCTAssertEqual(gap?.targetSessionID, "ses-1")
        XCTAssertEqual(gap?.effectiveResumeFromSeq, 501)
        XCTAssertEqual(gap?.reason, "backfill_truncated")
    }

    func testEventGapFallsBackToLastSeqAndRejectsOverflow() {
        let fallback = EventGap.decode(from: .object([
            "last_seq": .number(10),
            "last_session_id": .string("ses-1"),
        ]))
        XCTAssertNil(fallback?.resumeFromSeq)
        XCTAssertEqual(fallback?.effectiveResumeFromSeq, 11)
        XCTAssertTrue(fallback?.resyncRequired == true)

        // 2^64 is outside the persisted sequence range and must not wrap.
        let overflow = EventGap.decode(from: .object([
            "last_seq": .number(1.844_674_407_370_955_2e19),
        ]))
        XCTAssertNil(overflow?.lastSeq)
        XCTAssertNil(overflow?.effectiveResumeFromSeq)
    }

    func testOnlyEventGapMethodProducesAGapSignal() {
        let payload = JSONValue.object([
            "last_seq": .number(7),
            "last_session_id": .string("ses-1"),
            "resume_from_seq": .number(8),
            "resync_required": .bool(true),
        ])
        XCTAssertNil(
            EventGap.notification(method: "event.appended", params: payload)
        )
        XCTAssertNil(EventGap.notification(method: nil, params: payload))
        XCTAssertNil(EventGap.notification(method: "event.gap", params: nil))
        XCTAssertNil(
            EventGap.notification(method: "event.gap", params: .null)
        )
        XCTAssertEqual(
            EventGap.notification(method: "event.gap", params: payload)?.targetSessionID,
            "ses-1"
        )
    }

    func testEventGapIgnoresUnknownAndMalformedPayloadsSafely() {
        XCTAssertNil(EventGap.decode(from: .null))
        XCTAssertNil(EventGap.decode(from: .string("event.gap")))
        XCTAssertNil(EventGap.decode(from: .object(["type": .string("event.gap")])))

        // Recognizable keys with wrong value types still produce a gap (and a
        // conservative resync), but never fabricated values.
        let malformed = EventGap.decode(from: .object([
            "skipped": .string("many"),
            "last_seq": .bool(true),
            "session_id": .number(7),
            "resync_required": .string("yes"),
            "reason": .string("lagged"),
        ]))
        XCTAssertNotNil(malformed)
        XCTAssertNil(malformed?.skipped)
        XCTAssertNil(malformed?.lastSeq)
        XCTAssertNil(malformed?.targetSessionID)
        XCTAssertNil(malformed?.effectiveResumeFromSeq)
        XCTAssertTrue(malformed?.resyncRequired == true)
    }

    // MARK: - RPC timeouts

    func testLongOperationsGetBoundedLargerTimeout() {
        XCTAssertGreaterThan(
            RPCTimeouts.timeout(for: "session.land"),
            RPCTimeouts.timeout(for: "ping")
        )
        XCTAssertGreaterThan(
            RPCTimeouts.timeout(for: "session.diagnosis"),
            RPCTimeouts.timeout(for: "session.prompt")
        )
        XCTAssertEqual(RPCTimeouts.timeout(for: "ping"), .seconds(15))
        XCTAssertEqual(RPCTimeouts.timeout(for: "session.land"), .seconds(180))
        XCTAssertEqual(RPCTimeouts.timeout(for: "session.diagnosis"), .seconds(300))
        XCTAssertEqual(RPCTimeouts.timeout(for: "session.prompt"), .seconds(60))
        XCTAssertEqual(RPCTimeouts.timeout(for: "unknown.method"), RPCTimeouts.standard)
    }

    // MARK: - Provider credentials

    func testMissingKeychainItemIsNotTreatedAsAnEmptyKey() {
        let store = FakeSecretStore(values: [:])
        let result = ProviderCredentialResolver.resolve(
            typedKey: "",
            hasStoredKeyFlag: true,
            endpointRequiresKey: true,
            store: store,
            account: "provider.apiKey"
        )
        XCTAssertEqual(result, .missingStoredKey)
        XCTAssertEqual(store.readCount, 1)
    }

    func testStoredKeyIsReadOnceAndUsed() {
        let store = FakeSecretStore(values: ["provider.apiKey": "sk-stored"])
        let result = ProviderCredentialResolver.resolve(
            typedKey: "",
            hasStoredKeyFlag: true,
            endpointRequiresKey: true,
            store: store,
            account: "provider.apiKey"
        )
        XCTAssertEqual(result, .key("sk-stored"))
    }

    func testTypedKeyWinsWithoutKeychainAccess() {
        let store = FakeSecretStore(values: ["provider.apiKey": "sk-stored"])
        let result = ProviderCredentialResolver.resolve(
            typedKey: "sk-typed",
            hasStoredKeyFlag: true,
            endpointRequiresKey: true,
            store: store,
            account: "provider.apiKey"
        )
        XCTAssertEqual(result, .key("sk-typed"))
        XCTAssertEqual(store.readCount, 0)
    }

    func testLocalEndpointNeverReadsKeychain() {
        let store = FakeSecretStore(values: ["provider.apiKey": "sk-stored"])
        let result = ProviderCredentialResolver.resolve(
            typedKey: "",
            hasStoredKeyFlag: true,
            endpointRequiresKey: false,
            store: store,
            account: "provider.apiKey"
        )
        XCTAssertEqual(result, .notRequired)
        XCTAssertEqual(store.readCount, 0)
    }

    @MainActor
    func testKeychainSaveFailureAbortsBeforeDaemonConfigure() async {
        let store = FakeSecretStore(values: [:])
        store.saveError = FakeFailure()
        let model = AppModel(secretStore: store)
        model.providerBaseURL = "https://api.example.com/v1"
        model.providerModel = "test-model"
        model.providerAPIKey = "sk-typed"
        model.providerHasStoredKey = false

        await model.saveProviderSettings()

        XCTAssertTrue(
            model.providerStatus?.hasPrefix("Could not save the API key") == true,
            "a Keychain write failure must surface before configuring the daemon"
        )
        XCTAssertFalse(model.providerHasStoredKey)
        XCTAssertEqual(model.providerAPIKey, "sk-typed")
        XCTAssertNil(store.read(account: "provider.apiKey"))
    }

    // MARK: - Notification approval policy

    func testOnlyWorkspaceWritesCanResolveInline() {
        XCTAssertTrue(NotificationApprovalPolicy.allowsInlineResolution(risk: "workspace_write"))
        for risk in [
            "destructive", "host_state", "external", "mcp", "sensitive_gui", "", "unknown",
        ] {
            XCTAssertFalse(
                NotificationApprovalPolicy.allowsInlineResolution(risk: risk),
                "risk \(risk) must require in-app review"
            )
        }
        XCTAssertFalse(NotificationApprovalPolicy.allowsInlineResolution(risk: nil))
    }

    func testReviewCategoryForUnsafeRisk() {
        XCTAssertEqual(
            NotificationApprovalPolicy.categoryIdentifier(risk: "workspace_write"),
            NotificationApprovalPolicy.inlineCategory
        )
        XCTAssertEqual(
            NotificationApprovalPolicy.categoryIdentifier(risk: "host_state"),
            NotificationApprovalPolicy.reviewCategory
        )
        XCTAssertEqual(
            NotificationApprovalPolicy.categoryIdentifier(risk: nil),
            NotificationApprovalPolicy.reviewCategory
        )
    }

    @MainActor
    func testApprovalNoticeDetailIncludesRiskAndTargetWithoutNewlines() {
        let detail = AppModel.approvalNoticeDetail(
            tool: "shell",
            agent: "Worker",
            risk: "host_state",
            input: .object([
                "command": .string("rm -rf build\nsecond line"),
            ])
        )
        XCTAssertTrue(detail.contains("host state"))
        XCTAssertTrue(detail.contains("rm -rf build"))
        XCTAssertFalse(detail.contains("\n"))
        XCTAssertLessThanOrEqual(detail.count, 220)
    }

    // MARK: - Discovery recovery

    func testDiscoveryRecoveryKeepsAliveDaemonOnTransientFailure() {
        XCTAssertEqual(
            DiscoveryRecoveryPolicy.decide(
                healthy: false,
                processAlive: true,
                isVerlinParkDaemon: true
            ),
            .keepUnhealthy
        )
        XCTAssertEqual(
            DiscoveryRecoveryPolicy.decide(
                healthy: false,
                processAlive: false,
                isVerlinParkDaemon: true
            ),
            .removeStale
        )
        XCTAssertEqual(
            DiscoveryRecoveryPolicy.decide(
                healthy: false,
                processAlive: true,
                isVerlinParkDaemon: false
            ),
            .removeStale
        )
        XCTAssertEqual(
            DiscoveryRecoveryPolicy.decide(
                healthy: true,
                processAlive: true,
                isVerlinParkDaemon: true
            ),
            .reuse
        )
    }

    @MainActor
    func testLiveDaemonDiscoveryFileIsPreservedOnHealthFailure() async throws {
        let fixture = try DiscoveryFixture()
        defer { fixture.remove() }
        let controller = DaemonController(
            dataDirectory: fixture.directory,
            healthProbe: AlwaysUnhealthyProbe(),
            processEvidence: AliveDaemonEvidence(),
            locateOverride: { nil },
            healthRetryDelays: [.zero]
        )

        do {
            _ = try await controller.ensureRunning()
            XCTFail("An unhealthy but live daemon must not be relaunched")
        } catch {
            // Expected: the app reports a connection problem instead of
            // deleting the only discovery record.
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.discoveryURL.path))
    }

    @MainActor
    func testDeadDaemonDiscoveryIsRemovedOnlyWithProcessEvidence() async throws {
        let fixture = try DiscoveryFixture()
        defer { fixture.remove() }
        let controller = DaemonController(
            dataDirectory: fixture.directory,
            healthProbe: AlwaysUnhealthyProbe(),
            processEvidence: DeadDaemonEvidence(),
            locateOverride: { nil },
            healthRetryDelays: [.zero]
        )

        do {
            _ = try await controller.ensureRunning()
            XCTFail("No daemon binary is available in this fixture")
        } catch {
            // Expected: the stale record is removed, then locate fails.
        }
        XCTAssertFalse(FileManager.default.fileExists(atPath: fixture.discoveryURL.path))
    }

    @MainActor
    func testHealthRetryRecoversWithoutDeletingDiscovery() async throws {
        let fixture = try DiscoveryFixture()
        defer { fixture.remove() }
        let controller = DaemonController(
            dataDirectory: fixture.directory,
            healthProbe: RecoveringHealthProbe(),
            processEvidence: AliveDaemonEvidence(),
            locateOverride: { nil },
            healthRetryDelays: [.zero, .milliseconds(1)]
        )

        let discovery = try await controller.ensureRunning()
        XCTAssertEqual(discovery.port, fixture.discovery.port)
        XCTAssertTrue(FileManager.default.fileExists(atPath: fixture.discoveryURL.path))
    }

    // MARK: - Helpers

    private func makeEvent(seq: UInt64) -> Event {
        Event(
            sessionId: "session-1",
            seq: seq,
            type: "test",
            payload: .null,
            createdAt: "2026-11-19T00:00:00Z"
        )
    }
}

private struct FakeFailure: LocalizedError {
    var errorDescription: String? {
        "Keychain write failed in test"
    }
}

private final class FakeSecretStore: SecretStore, @unchecked Sendable {
    private var values: [String: String]
    private(set) var readCount = 0
    var saveError: Error?

    init(values: [String: String]) {
        self.values = values
    }

    func read(account: String) -> String? {
        readCount += 1
        return values[account]
    }

    func save(_ value: String, account: String) throws {
        if let saveError {
            throw saveError
        }
        values[account] = value
    }

    func delete(account: String) {
        values.removeValue(forKey: account)
    }
}

private struct AlwaysUnhealthyProbe: DaemonHealthProbe {
    func isHealthy(discovery _: Discovery, timeout _: TimeInterval) async -> Bool {
        false
    }
}

private struct RecoveringHealthProbe: DaemonHealthProbe {
    private let state = ProbeState()

    func isHealthy(discovery _: Discovery, timeout _: TimeInterval) async -> Bool {
        state.calls += 1
        return state.calls > 1
    }

    private final class ProbeState: @unchecked Sendable {
        var calls = 0
    }
}

private struct AliveDaemonEvidence: DaemonProcessEvidenceProviding {
    func isAlive(pid _: Int) -> Bool {
        true
    }

    func isVerlinParkDaemon(pid _: Int) -> Bool {
        true
    }
}

private struct DeadDaemonEvidence: DaemonProcessEvidenceProviding {
    func isAlive(pid _: Int) -> Bool {
        false
    }

    func isVerlinParkDaemon(pid _: Int) -> Bool {
        false
    }
}

private struct DiscoveryFixture {
    let directory: URL
    let discovery: Discovery

    var discoveryURL: URL {
        directory.appendingPathComponent("run/daemon.json")
    }

    init() throws {
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("vp-discovery-test-\(UUID().uuidString)", isDirectory: true)
        let run = directory.appendingPathComponent("run", isDirectory: true)
        try FileManager.default.createDirectory(at: run, withIntermediateDirectories: true)
        discovery = Discovery(
            pid: 99999,
            port: 65000,
            token: "test-token",
            protocol: 1,
            version: "test",
            startedAt: "2026-11-19T00:00:00Z"
        )
        let data = try JSONEncoder().encode(discovery)
        try data.write(to: run.appendingPathComponent("daemon.json"))
    }

    func remove() {
        try? FileManager.default.removeItem(at: directory)
    }
}
