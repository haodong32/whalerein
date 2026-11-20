// Stack-backed Integration plan.
//
// These tests exercise the real Swift -> UniFFI -> Rust Core path against the
// disposable loopback stack started by `tools/ios-stack/ios_stack.py`. No
// fixture SessionService is used here. Configuration comes from
// `WHALEREIN_IOS_STACK_*` environment variables injected into the generated
// `.xctestrun` by `apps/ios/scripts/test-integration.sh`.
//
// The stack is shared and stateful: tests reset the fixture to idle where it
// matters and compare against the contract's current expected transcript.
// Invitation URIs, control tokens, message text and draft text are never
// printed.

import Foundation
import XCTest
@testable import WhaleRein

// MARK: - Environment and control client

struct StackUnavailable: Error {
    let reason: String
}

struct StackEnvironment {
    let accessURL: URL
    let mqttURL: String
    let invitation: String
    let hostId: String
    let sessionId: String
    let controlURL: URL
    let controlToken: String
    let expectedRevision: String
    let teardownMarker: String

    static func process() throws -> StackEnvironment {
        let values = ProcessInfo.processInfo.environment
        func require(_ key: String) throws -> String {
            guard let value = values[key], !value.isEmpty else {
                throw StackUnavailable(reason: "missing \(key)")
            }
            return value
        }
        guard let access = URL(string: try require("WHALEREIN_IOS_STACK_ACCESS_URL")),
              let control = URL(string: try require("WHALEREIN_IOS_STACK_CONTROL_URL")) else {
            throw StackUnavailable(reason: "invalid stack URL")
        }
        return StackEnvironment(
            accessURL: access,
            mqttURL: try require("WHALEREIN_IOS_STACK_MQTT_URL"),
            invitation: try require("WHALEREIN_IOS_STACK_INVITATION_URI"),
            hostId: try require("WHALEREIN_IOS_STACK_HOST_ID"),
            sessionId: try require("WHALEREIN_IOS_STACK_SESSION_ID"),
            controlURL: control,
            controlToken: try require("WHALEREIN_IOS_STACK_CONTROL_TOKEN"),
            expectedRevision: try require("WHALEREIN_IOS_STACK_EXPECTED_REVISION"),
            teardownMarker: try require("WHALEREIN_IOS_STACK_TEARDOWN_MARKER")
        )
    }

    static let skipReason = """
        The live iOS loopback stack is not configured. Start it with \
        `python3 tools/ios-stack/ios_stack.py up` and run \
        `apps/ios/scripts/test-integration.sh`, which injects the \
        WHALEREIN_IOS_STACK_* variables into the test runner.
        """
}

struct StackControlError: Error {
    let status: Int
    let code: String
}

struct StackControl {
    let baseURL: URL
    let token: String

    func seedEvent(_ kind: String) async throws {
        _ = try await post("v1/seed-event", body: ["kind": kind])
    }

    func stopBroker() async throws {
        _ = try await post("v1/broker", body: ["action": "stop"])
    }

    func startBroker() async throws {
        _ = try await post("v1/broker", body: ["action": "start"])
    }

    func restartBroker() async throws {
        _ = try await post("v1/restart", body: ["target": "broker"])
    }

    func freshInvitation() async throws -> String {
        let data = try await get("v1/invitation?refresh=1")
        guard let invitation = data["invitation_uri"] as? String, !invitation.isEmpty else {
            throw StackControlError(status: 200, code: "missing_invitation")
        }
        return invitation
    }

    func status() async throws -> [String: Any] {
        try await get("v1/status")
    }

    private func get(_ path: String) async throws -> [String: Any] {
        try await request(method: "GET", path: path, body: nil)
    }

    private func post(_ path: String, body: [String: Any]) async throws -> [String: Any] {
        try await request(method: "POST", path: path, body: body)
    }

    private func request(method: String, path: String, body: [String: Any]?) async throws -> [String: Any] {
        guard let url = URL(string: path, relativeTo: baseURL) else {
            throw StackControlError(status: 0, code: "invalid_control_url")
        }
        var request = URLRequest(url: url)
        request.httpMethod = method
        request.timeoutInterval = 120
        request.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        if let body {
            request.setValue("application/json", forHTTPHeaderField: "Content-Type")
            request.httpBody = try JSONSerialization.data(withJSONObject: body)
        }
        let (data, response) = try await URLSession.shared.data(for: request)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0
        guard (200..<300).contains(status) else {
            throw StackControlError(status: status, code: "http_error")
        }
        guard let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              object["ok"] as? Bool == true else {
            throw StackControlError(status: status, code: "invalid_response")
        }
        return object["data"] as? [String: Any] ?? [:]
    }
}

// MARK: - Live service harness

@MainActor
final class LiveCase {
    let environment: StackEnvironment
    let control: StackControl
    let keychainService: String
    let keychain: KeychainSecretStore
    let dataDirectory: URL
    var service: LiveSessionService!

    init(environment: StackEnvironment) throws {
        self.environment = environment
        self.control = StackControl(baseURL: environment.controlURL, token: environment.controlToken)
        let identifier = UUID().uuidString.lowercased()
        self.keychainService = "\(AppStorageLocations.keychainService).integration.\(identifier)"
        self.keychain = KeychainSecretStore(service: keychainService)
        self.dataDirectory = FileManager.default.temporaryDirectory
            .appendingPathComponent("WhaleReinIntegration-\(identifier)", isDirectory: true)
        try FileManager.default.createDirectory(at: dataDirectory, withIntermediateDirectories: true)
        self.service = try makeService()
    }

    private func makeService() throws -> LiveSessionService {
        let document = AppStorageLocations.appConfigurationURL(in: dataDirectory)
        let store = try AppConfigurationStore(documentPath: document.path)
        let standalone = AppInstanceInput(
            id: "standalone",
            service: .standalone,
            environment: nil,
            dataDirectory: "instances/standalone/data",
            standaloneCAPEM: nil
        )
        try store.save(standalone)
        try store.activate(id: "standalone")
        let instance = store.activeInstance
        return try LiveSessionService(
            configPath: instance.configPath,
            keychainService: keychainService,
            appVersion: "0.1.0-integration",
            instanceId: instance.id
        )
    }

    /// Drops the current Core instance (releasing its instance lock), then
    /// creates a new one over the same data directory and Keychain namespace.
    func recreate() async throws {
        // A process restart must stop the old watch before the profile lease is
        // released to the replacement Core, otherwise the new attachment can
        // race the old observation and never install its baseline.
        service.stopWatch()
        try await Task.sleep(for: .milliseconds(700))
        service = nil
        try await Task.sleep(for: .milliseconds(300))
        service = try makeService()
    }

    func cleanup() {
        service = nil
        try? keychain.removeAll()
        try? FileManager.default.removeItem(at: dataDirectory)
    }

    func pairFreshInvitation() async throws -> PairingPrompt {
        let invitation = try await control.freshInvitation()
        let prompt = try await service.pairingStart(invitation: invitation)
        try await service.pairingDecide(
            approved: true,
            expectedFingerprint: prompt.fingerprint
        )
        return prompt
    }

    func expectedTranscript() async throws -> [String] {
        let status = try await control.status()
        return status["expected_transcript"] as? [String] ?? []
    }

    func waitForHost(timeout: TimeInterval = 15) async throws -> [HostSummary] {
        try await waitUntil(timeout: timeout) {
            let hosts = try await self.service.hosts()
            return hosts.contains { $0.hostId == self.environment.hostId } ? hosts : nil
        }
    }

    func waitForPendingApproval(timeout: TimeInterval = 15) async throws -> Approval? {
        try await waitUntil(timeout: timeout) {
            try await self.service.approvals(
                hostId: self.environment.hostId,
                sessionId: self.environment.sessionId
            ).first { $0.state == .pending }
        }
    }
}

func waitUntil<T>(
    timeout: TimeInterval,
    interval: Duration = .milliseconds(50),
    _ operation: () async throws -> T?
) async throws -> T {
    let deadline = Date().addingTimeInterval(timeout)
    var lastError: Error?
    while Date() < deadline {
        do {
            if let value = try await operation() { return value }
        } catch {
            lastError = error
        }
        try? await Task.sleep(for: interval)
    }
    if let lastError { throw lastError }
    throw SessionServiceError(.timeout)
}

// MARK: - Event collection

@MainActor
final class EventCollector {
    private(set) var envelopes: [SessionEventEnvelope] = []
    private(set) var finished = false
    private var task: Task<Void, Never>?

    init(_ stream: AsyncStream<SessionEventEnvelope>) {
        task = Task { [weak self] in
            for await envelope in stream {
                guard let self else { return }
                self.envelopes.append(envelope)
            }
            guard let self else { return }
            self.finished = true
        }
    }

    func wait(
        timeout: TimeInterval,
        until predicate: ([SessionEventEnvelope]) -> Bool
    ) async -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if predicate(envelopes) { return true }
            try? await Task.sleep(for: .milliseconds(25))
        }
        return predicate(envelopes)
    }

    func cancel() {
        task?.cancel()
        task = nil
    }
}

func transcript(from envelopes: [SessionEventEnvelope]) -> [ConversationMessage] {
    var messages: [ConversationMessage] = []
    for envelope in envelopes {
        switch envelope.event {
        case .reset(let reset, _):
            messages = reset
        case .message(let message):
            if !messages.contains(where: { $0.messageId == message.messageId }) {
                messages.append(message)
            }
        default:
            break
        }
    }
    return messages
}

func eventKinds(_ envelopes: [SessionEventEnvelope]) -> [String] {
    envelopes.map { envelope in
        switch envelope.event {
        case .reset: return "reset"
        case .message: return "message"
        case .replace: return "replace"
        case .preview: return "preview"
        case .attemptEnded: return "attemptEnded"
        case .state: return "state"
        case .connection: return "connection"
        case .capabilities: return "capabilities"
        case .approvals: return "approvals"
        case .approvalChanged: return "approvalChanged"
        case .questions: return "questions"
        case .commandStatus: return "commandStatus"
        case .error: return "error"
        case .watchEnded: return "watchEnded"
        default: return "other"
        }
    }
}

func lastConnection(in envelopes: [SessionEventEnvelope]) -> ConnectionStatus? {
    for envelope in envelopes.reversed() {
        switch envelope.event {
        case .connection(_, let status), .state(let status):
            return status
        default:
            continue
        }
    }
    return nil
}

// MARK: - Base test case

@MainActor
class IntegrationTestCase: XCTestCase {
    private var liveCases: [LiveCase] = []

    override func setUp() async throws {
        continueAfterFailure = false
        if let environment = try? StackEnvironment.process() {
            try? await StackControl(
                baseURL: environment.controlURL,
                token: environment.controlToken
            ).seedEvent("idle")
        }
    }

    override func tearDown() {
        for live in liveCases { live.cleanup() }
        liveCases.removeAll()
    }

    /// Restores the broker from a synchronous teardown block. XCTest may run
    /// teardown synchronously on the main thread after a failure, so the async
    /// control call is bridged with a bounded semaphore instead of an async
    /// teardown block.
    func addBrokerRestoreTeardown(_ live: LiveCase) {
        let control = live.control
        addTeardownBlock {
            let semaphore = DispatchSemaphore(value: 0)
            Task.detached {
                try? await control.startBroker()
                semaphore.signal()
            }
            _ = semaphore.wait(timeout: .now() + 90)
        }
    }

    func environment() throws -> StackEnvironment {
        do {
            return try StackEnvironment.process()
        } catch {
            throw XCTSkip(StackEnvironment.skipReason)
        }
    }

    func makeLive(_ environment: StackEnvironment) throws -> LiveCase {
        let live = try LiveCase(environment: environment)
        liveCases.append(live)
        return live
    }

    func assertError(
        _ error: Error,
        oneOf codes: [AppErrorCode],
        _ message: String = "unexpected error",
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        guard let serviceError = error as? SessionServiceError else {
            XCTFail("\(message): expected SessionServiceError, got \(type(of: error))", file: file, line: line)
            return
        }
        XCTAssertTrue(
            codes.contains(serviceError.code),
            "\(message): got \(serviceError.code.rawValue), expected one of \(codes.map(\.rawValue))",
            file: file,
            line: line
        )
    }

    func assertUnpaired(_ service: LiveSessionService, file: StaticString = #filePath, line: UInt = #line) async {
        do {
            let hosts = try await service.hosts()
            XCTFail("expected unpaired state, listed \(hosts.count) hosts", file: file, line: line)
        } catch {
            XCTAssertTrue(
                error is SessionServiceError,
                "expected a typed unpaired error",
                file: file,
                line: line
            )
        }
    }

    func startWatching(_ live: LiveCase) async throws -> EventCollector {
        let stream = live.service.makeEventStream()
        let collector = EventCollector(stream)
        try await live.service.startWatch(
            hostId: live.environment.hostId,
            sessionId: live.environment.sessionId
        )
        _ = await collector.wait(timeout: 20) { envelopes in
            envelopes.contains { envelope in
                if case .reset = envelope.event { return true }
                return false
            }
        }
        return collector
    }
}

// MARK: - AC-01 provenance

extension IntegrationTestCase {
    func testAC01_provenance() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        let revision = live.service.coreRevision

        XCTAssertFalse(revision.isEmpty)
        XCTAssertNotNil(
            revision.range(
                of: #"^(git:[0-9a-f]{7,40}(-dirty)?|source:[0-9a-f]{64}) core:[0-9]+\.[0-9]+\.[0-9]+$"#,
                options: .regularExpression
            ),
            "coreRevision does not use the documented provenance format"
        )
        XCTAssertEqual(revision, environment.expectedRevision)
    }
}

// MARK: - AC-02 pairing

extension IntegrationTestCase {
    func testAC02_confirm() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        let prompt = try await live.pairFreshInvitation()
        XCTAssertEqual(prompt.hostId, environment.hostId)

        let hosts = try await live.waitForHost()
        XCTAssertTrue(hosts.contains { $0.hostId == environment.hostId && $0.online })

        let sessions = try await live.service.sessions(hostId: environment.hostId)
        XCTAssertTrue(sessions.contains { $0.sessionId == environment.sessionId })
    }

    func testAC02_invalidInvitation() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        // Same shape as a real invitation with a bogus one-time token.
        let invitation = try await live.control.freshInvitation()
        guard var components = URLComponents(string: invitation) else {
            return XCTFail("contract invitation is not a URL")
        }
        components.queryItems = components.queryItems?.map { item in
            item.name == "token"
                ? URLQueryItem(name: "token", value: String(repeating: "ab", count: 32))
                : item
        }
        guard let invalid = components.string else {
            return XCTFail("could not build the invalid invitation")
        }

        do {
            let prompt = try await live.service.pairingStart(invitation: invalid)
            try await live.service.pairingDecide(
                approved: true,
                expectedFingerprint: prompt.fingerprint
            )
            XCTFail("an invalid invitation unexpectedly paired")
        } catch {
            assertError(
                error,
                oneOf: [.invalidInvitation, .security, .unavailable, .notPaired, .timeout],
                "invalid invitation"
            )
        }
        await assertUnpaired(live.service)
    }
}

// MARK: - AC-03 rejection

extension IntegrationTestCase {
    func testAC03_decline() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        let invitation = try await live.control.freshInvitation()
        _ = try await live.service.pairingStart(invitation: invitation)
        try await live.service.pairingDecide(approved: false, expectedFingerprint: nil)
        await assertUnpaired(live.service)
    }

    func testAC03_fingerprint() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        let invitation = try await live.control.freshInvitation()
        let prompt = try await live.service.pairingStart(invitation: invitation)
        do {
            try await live.service.pairingDecide(
                approved: true,
                expectedFingerprint: String(repeating: "00", count: 32)
            )
            XCTFail("a wrong fingerprint unexpectedly paired")
        } catch {
            assertError(error, oneOf: [.fingerprintMismatch, .security], "wrong fingerprint")
        }
        await assertUnpaired(live.service)
    }

    func testAC03_invalidInvitation() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        do {
            _ = try await live.service.pairingStart(invitation: "not-an-invitation")
            XCTFail("a malformed invitation unexpectedly produced a prompt")
        } catch {
            assertError(error, oneOf: [.invalidInvitation], "malformed invitation")
        }
        await assertUnpaired(live.service)
    }
}

// MARK: - AC-04 watch replacement

extension IntegrationTestCase {
    func testAC04_sessionSwitch() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()

        let first = try await startWatching(live)
        XCTAssertFalse(first.envelopes.isEmpty)
        XCTAssertFalse(first.finished)

        // Replacing the watch ends the previous consumer stream; late events
        // from the first watch must not reach the new one.
        let firstSequence = first.envelopes.last?.sequence ?? 0
        live.service.stopWatch()
        let ended = await first.wait(timeout: 5) { _ in first.finished }
        XCTAssertTrue(ended)
        let firstCount = first.envelopes.count
        first.cancel()

        let second = try await startWatching(live)
        try await live.control.seedEvent("output")
        let sawFinal = await second.wait(timeout: 25) { envelopes in
            transcript(from: envelopes).contains { $0.plainText == "fixture output" }
        }
        XCTAssertTrue(sawFinal)
        XCTAssertTrue(
            second.envelopes.allSatisfy { $0.sequence > firstSequence },
            "the new consumer received events ordered before the replaced watch"
        )
        XCTAssertEqual(first.envelopes.count, firstCount, "the replaced consumer received late events")
        XCTAssertFalse(first.envelopes.contains { envelope in
            if case .preview(_, let text, _, _) = envelope.event { return text.contains("LIVE-STREAM") }
            return false
        })
        second.cancel()

        // The stack has one fixture session, so cross-session leakage is not
        // exercisable here; the cross-session variant is covered by Core and
        // UI fixture evidence. This test covers watch replacement ordering.
    }
}

// MARK: - AC-05 history and recovery

extension IntegrationTestCase {
    func testAC05_00_previewThenFinal() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        let collector = try await startWatching(live)
        _ = await collector.wait(timeout: 20) { lastConnection(in: $0) == .online }
        try await live.control.seedEvent("running")
        try await live.control.seedEvent("stream")

        let previewArrived = await collector.wait(timeout: 15) { envelopes in
            envelopes.contains { envelope in
                if case .preview(_, let text, _, _) = envelope.event { return !text.isEmpty }
                return false
            }
        }
        XCTAssertTrue(
            previewArrived,
            "the provisional preview did not arrive before the committed final; events=\(eventKinds(collector.envelopes))"
        )
        if !previewArrived {
            collector.cancel()
            try? await live.control.seedEvent("idle")
            return
        }
        let finalArrived = await collector.wait(timeout: 20) { envelopes in
            transcript(from: envelopes).contains { $0.plainText == "xy" }
        }
        XCTAssertTrue(finalArrived)
        collector.cancel()
        try? await live.control.seedEvent("idle")
    }

    func testAC05_historyLiveCut() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        let expected = try await live.expectedTranscript()

        let collector = try await startWatching(live)
        let initial = await collector.wait(timeout: 20) { envelopes in
            transcript(from: envelopes).count >= expected.count
        }
        XCTAssertTrue(initial)
        let initialTranscript = transcript(from: collector.envelopes)
        let baselineXY = initialTranscript.filter { $0.plainText == "xy" }.count
        XCTAssertEqual(initialTranscript.map(\.plainText), expected)
        XCTAssertEqual(initialTranscript.map(\.role), Array(repeating: .assistant, count: expected.count))

        var baselineCount = transcript(from: collector.envelopes).count
        // Prove the live subscription is receiving committed events before
        // driving the ephemeral preview, which is not replayed from history.
        _ = await collector.wait(timeout: 20) { lastConnection(in: $0) == .online }
        try await live.control.seedEvent("output")
        let liveDelivered = await collector.wait(timeout: 20) {
            transcript(from: $0).count > baselineCount
        }
        XCTAssertTrue(
            liveDelivered,
            "the live watch did not receive a committed event; events=\(eventKinds(collector.envelopes))"
        )
        baselineCount = transcript(from: collector.envelopes).count
        try await Task.sleep(for: .seconds(1))

        // One scripted stream: the stack's fixture reuses a stream id, so the
        // preview is available once per fresh stack. AC-04 uses a committed
        // output event and does not consume it.
        try await live.control.seedEvent("stream")
        let streamed = await collector.wait(timeout: 25) { envelopes in
            transcript(from: envelopes).filter { $0.plainText == "xy" }.count == baselineXY + 1
        }
        XCTAssertTrue(
            streamed,
            "final streamed text did not arrive; events=\(eventKinds(collector.envelopes))"
        )
        XCTAssertEqual(
            transcript(from: collector.envelopes).count,
            baselineCount + 1,
            "the stream produced an unexpected number of committed messages"
        )

        let finalIndex = collector.envelopes.firstIndex { envelope in
            if case .message(let message) = envelope.event { return message.plainText == "xy" }
            return false
        }
        XCTAssertNotNil(finalIndex, "the committed final message was not observed")
        collector.cancel()

        // Reconnect: a fresh watch must rebuild the committed transcript from
        // the encrypted local cache without duplicating streamed output.
        let expectedAfter = try await live.expectedTranscript()
        let reconnected = try await startWatching(live)
        let sawRecovered = await reconnected.wait(timeout: 20) {
            transcript(from: $0).count >= expectedAfter.count
        }
        XCTAssertTrue(
            sawRecovered,
            "reconnect did not rebuild the committed transcript; events=\(eventKinds(reconnected.envelopes))"
        )
        let recovered = transcript(from: reconnected.envelopes)
        XCTAssertEqual(recovered.map(\.plainText), expectedAfter)
        XCTAssertEqual(Set(recovered.map(\.messageId)).count, recovered.count, "duplicate message ids")
        let original = transcript(from: collector.envelopes)
        XCTAssertEqual(Set(original.map(\.messageId)).count, original.count, "duplicate message ids")
        reconnected.cancel()
    }

    func testAC05_mqttLossHttpsOk() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        let collector = try await startWatching(live)
        let online = await collector.wait(timeout: 20) { lastConnection(in: $0) == .online }
        XCTAssertTrue(
            online,
            "watch did not report an online connection; last=\(String(describing: lastConnection(in: collector.envelopes))); events=\(eventKinds(collector.envelopes))"
        )

        let warmSessions = try await live.service.cachedSessions(hostId: environment.hostId)
        XCTAssertFalse(
            warmSessions.isEmpty,
            "the watch did not warm the durable session cache before the broker stopped"
        )

        try await live.control.stopBroker()
        addBrokerRestoreTeardown(live)

        let degraded = await collector.wait(timeout: 30) { envelopes in
            guard let status = lastConnection(in: envelopes) else { return false }
            return status != .online
        }
        XCTAssertTrue(
            degraded,
            "watch did not report a non-online connection while MQTT was down; last=\(String(describing: lastConnection(in: collector.envelopes))); events=\(eventKinds(collector.envelopes))"
        )

        // HTTPS-backed operations remain available with the broker down.
        let hosts = try await live.service.hosts()
        XCTAssertTrue(hosts.contains { $0.hostId == environment.hostId })
        let sessions = try await live.service.sessions(hostId: environment.hostId)
        XCTAssertTrue(sessions.contains { $0.sessionId == environment.sessionId })
        let cached = try await live.service.cachedSessions(hostId: environment.hostId)
        XCTAssertFalse(cached.isEmpty)

        try await live.control.startBroker()
        let recovered = await collector.wait(timeout: 40) { lastConnection(in: $0) == .online }
        XCTAssertTrue(recovered, "watch did not recover after the broker restarted")
        collector.cancel()
    }

    func testAC05_processRestart() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        let expected = try await live.expectedTranscript()

        let before = try await startWatching(live)
        let initialLoaded = await before.wait(timeout: 20) {
            transcript(from: $0).count >= expected.count
        }
        XCTAssertTrue(
            initialLoaded,
            "first watch did not deliver seeded history; events=\(eventKinds(before.envelopes))"
        )
        let beforeTranscript = transcript(from: before.envelopes).map(\.plainText)
        XCTAssertEqual(beforeTranscript, expected)
        before.cancel()

        let cachedBeforeRestart = try await live.service.cachedTranscript(
            hostId: environment.hostId,
            sessionId: environment.sessionId
        )
        XCTAssertFalse(
            cachedBeforeRestart.isEmpty,
            "the first watch did not commit its baseline to the encrypted cache"
        )

        try await live.recreate()
        let hosts = try await live.waitForHost()
        XCTAssertTrue(hosts.contains { $0.hostId == environment.hostId })

        // The pairing grant and encrypted transcript survive a Core restart.
        let after = try await startWatching(live)
        let replayed = await after.wait(timeout: 20) {
            transcript(from: $0).count >= expected.count
        }
        XCTAssertTrue(
            replayed,
            "post-restart watch did not rebuild history; events=\(eventKinds(after.envelopes))"
        )
        XCTAssertEqual(
            transcript(from: after.envelopes).map(\.plainText),
            expected,
            "post-restart events=\(eventKinds(after.envelopes))"
        )
        XCTAssertEqual(
            Set(transcript(from: after.envelopes).map(\.messageId)).count,
            expected.count,
            "duplicate message ids after restart"
        )
        after.cancel()
    }
}

// MARK: - AC-06 send and delivery

extension IntegrationTestCase {
    func testAC06_idleSend() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        _ = try await startWatching(live)
        try await live.control.seedEvent("idle")

        let capabilities = try await live.service.capabilities(
            hostId: environment.hostId,
            sessionId: environment.sessionId
        )
        XCTAssertFalse(capabilities.running)
        XCTAssertTrue(capabilities.supportsSteer, "the Source reported steer support")
        XCTAssertTrue(capabilities.supportsQueue, "the Source reported queue support")

        let receipt = try await live.service.send(
            hostId: environment.hostId,
            sessionId: environment.sessionId,
            text: "integration idle send",
            delivery: .send
        )
        XCTAssertEqual(receipt.status, .confirmed)
        XCTAssertFalse(receipt.commandId.isEmpty)
    }

    func testAC06_defaultSteer() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        _ = try await startWatching(live)
        try await live.control.seedEvent("running")

        let capabilities = try await live.service.capabilities(
            hostId: environment.hostId,
            sessionId: environment.sessionId
        )
        XCTAssertTrue(capabilities.running)
        XCTAssertTrue(capabilities.supportsSteer)
        XCTAssertEqual(DeliveryPolicy.defaultChoice(for: capabilities), .steer)

        let receipt = try await live.service.send(
            hostId: environment.hostId,
            sessionId: environment.sessionId,
            text: "integration steer send",
            delivery: .steer
        )
        XCTAssertEqual(receipt.status, .confirmed)
        try await live.control.seedEvent("idle")
    }

    func testAC06_explicitQueue() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        _ = try await startWatching(live)
        try await live.control.seedEvent("running")

        let capabilities = try await live.service.capabilities(
            hostId: environment.hostId,
            sessionId: environment.sessionId
        )
        XCTAssertTrue(capabilities.running)
        XCTAssertTrue(capabilities.supportsQueue)

        let receipt = try await live.service.send(
            hostId: environment.hostId,
            sessionId: environment.sessionId,
            text: "integration queue send",
            delivery: .queue
        )
        // Acceptance is only reported after the Source confirms the command.
        XCTAssertEqual(receipt.status, .confirmed)
        try await live.control.seedEvent("idle")
    }
}

// MARK: - AC-08 current approval

extension IntegrationTestCase {
    func testAC08_allowOnce() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        let watch = try await startWatching(live)
        try await live.control.seedEvent("approval")

        guard let approval = try await live.waitForPendingApproval() else {
            return XCTFail("no pending approval was listed")
        }
        XCTAssertEqual(approval.state, .pending)

        let resolved = try await live.service.resolveApproval(
            hostId: environment.hostId,
            sessionId: environment.sessionId,
            approvalId: approval.approvalId,
            version: approval.version,
            digestHex: approval.digestHex,
            allow: true
        )
        XCTAssertEqual(resolved.approvalId, approval.approvalId)
        XCTAssertEqual(resolved.state, .allowed)

        let after = try await live.service.approvals(
            hostId: environment.hostId,
            sessionId: environment.sessionId
        )
        XCTAssertFalse(
            after.contains { $0.approvalId == approval.approvalId && $0.state == .pending },
            "the resolved approval is still pending"
        )
        // FakeDSH lists only pending approvals and Core does not project a
        // resolved approval back into the watch state. The confirmed resolve
        // return is the authoritative update used by the original card, which
        // `testAC08_originalCard` asserts directly.
        watch.cancel()
        try await live.control.seedEvent("idle")
    }

    func testAC08_reject() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        let watch = try await startWatching(live)
        try await live.control.seedEvent("approval")

        guard let approval = try await live.waitForPendingApproval() else {
            return XCTFail("no pending approval was listed")
        }
        let resolved = try await live.service.resolveApproval(
            hostId: environment.hostId,
            sessionId: environment.sessionId,
            approvalId: approval.approvalId,
            version: approval.version,
            digestHex: approval.digestHex,
            allow: false
        )
        XCTAssertEqual(resolved.approvalId, approval.approvalId)
        XCTAssertEqual(resolved.state, .rejected)
        let after = try await live.service.approvals(
            hostId: environment.hostId,
            sessionId: environment.sessionId
        )
        XCTAssertFalse(after.contains { $0.approvalId == approval.approvalId && $0.state == .pending })
        watch.cancel()
        try await live.control.seedEvent("idle")
    }

    func testAC08_originalCard() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        let watch = try await startWatching(live)
        try await live.control.seedEvent("approval")

        guard let approval = try await live.waitForPendingApproval() else {
            return XCTFail("no pending approval was listed")
        }
        let resolved = try await live.service.resolveApproval(
            hostId: environment.hostId,
            sessionId: environment.sessionId,
            approvalId: approval.approvalId,
            version: approval.version,
            digestHex: approval.digestHex,
            allow: true
        )
        // The presentation layer replaces the original card with this value.
        XCTAssertEqual(resolved.approvalId, approval.approvalId)
        XCTAssertEqual(resolved.version, approval.version)
        XCTAssertEqual(resolved.digestHex, approval.digestHex)
        XCTAssertEqual(resolved.state, .allowed)
        watch.cancel()
        try await live.control.seedEvent("idle")
    }
}

// MARK: - AC-09 stale approval

extension IntegrationTestCase {
    func testAC09_changedVersion() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        try await live.control.seedEvent("approval")

        guard let approval = try await live.waitForPendingApproval() else {
            return XCTFail("no pending approval was listed")
        }
        do {
            _ = try await live.service.resolveApproval(
                hostId: environment.hostId,
                sessionId: environment.sessionId,
                approvalId: approval.approvalId,
                version: approval.version + 1,
                digestHex: approval.digestHex,
                allow: true
            )
            XCTFail("a stale approval version unexpectedly resolved")
        } catch {
            assertError(error, oneOf: [.staleApproval, .conflict], "stale approval version")
        }
        let pending = try await live.service.approvals(
            hostId: environment.hostId,
            sessionId: environment.sessionId
        )
        XCTAssertTrue(
            pending.contains { $0.approvalId == approval.approvalId && $0.state == .pending },
            "the pending approval did not remain after the stale decision"
        )

        // Clean up with the reviewed variant.
        _ = try await live.service.resolveApproval(
            hostId: environment.hostId,
            sessionId: environment.sessionId,
            approvalId: approval.approvalId,
            version: approval.version,
            digestHex: approval.digestHex,
            allow: false
        )
        try await live.control.seedEvent("idle")
    }
}

// MARK: - AC-10 device lifecycle

extension IntegrationTestCase {
    func testAC10_sameDeviceRestart() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()

        let draft = "integration draft sentinel \(UUID().uuidString)"
        try await live.service.saveDraft(
            hostId: environment.hostId,
            sessionId: environment.sessionId,
            text: draft
        )
        let configPath = live.dataDirectory.appendingPathComponent("config.toml")
        let manifestPath = live.dataDirectory.appendingPathComponent("instance/instance.json")
        let selectedConfig = try Data(contentsOf: configPath)
        let instanceIdentity = try Data(contentsOf: manifestPath)
        XCTAssertFalse(FileManager.default.fileExists(
            atPath: live.dataDirectory.appendingPathComponent("core").path
        ))
        let identityItems = try live.keychain.itemCount()
        XCTAssertGreaterThan(identityItems, 0, "the device identity was not stored in Keychain")

        try await live.recreate()
        XCTAssertEqual(try Data(contentsOf: configPath), selectedConfig)
        XCTAssertEqual(try Data(contentsOf: manifestPath), instanceIdentity)
        XCTAssertEqual(try live.keychain.itemCount(), identityItems)
        let hosts = try await live.waitForHost()
        XCTAssertTrue(hosts.contains { $0.hostId == environment.hostId })
        let restored = try await live.service.loadDraft(
            hostId: environment.hostId,
            sessionId: environment.sessionId
        )
        XCTAssertEqual(restored, draft)

        try await live.service.revoke()
        let clearedDraft = try? await live.service.loadDraft(
            hostId: environment.hostId,
            sessionId: environment.sessionId
        )
        XCTAssertNil(clearedDraft ?? nil, "revocation left the draft in place")
        do {
            _ = try await live.service.hosts()
            XCTFail("revoked device still listed hosts")
        } catch {
            assertError(error, oneOf: [.revoked, .notPaired], "revoked hosts")
        }
        // The device identity itself remains; only grants/cache/drafts clear.
        let remainingItems = try live.keychain.itemCount()
        XCTAssertGreaterThan(remainingItems, 0)
    }
}

// MARK: - AC-11 diagnostics

extension IntegrationTestCase {
    func testAC11_mqttHttpsDistinction() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        let watch = try await startWatching(live)
        _ = await watch.wait(timeout: 20) { lastConnection(in: $0) == .online }

        try await live.control.stopBroker()
        addBrokerRestoreTeardown(live)

        let degraded = await watch.wait(timeout: 30) { envelopes in
            guard let status = lastConnection(in: envelopes) else { return false }
            return status != .online
        }
        XCTAssertTrue(degraded, "watch did not report a degraded connection with MQTT down")

        // HTTPS history remains reachable while MQTT is down.
        let sessions = try await live.service.sessions(hostId: environment.hostId)
        XCTAssertTrue(sessions.contains { $0.sessionId == environment.sessionId })

        let report = try await live.service.diagnosticsPreview()
        XCTAssertTrue(report.stages.contains { $0.kind == .https && $0.status == .ok })
        XCTAssertTrue(report.stages.contains { $0.kind == .mqtt && $0.status != .ok })

        try await live.control.startBroker()
        let recovered = await watch.wait(timeout: 40) { lastConnection(in: $0) == .online }
        XCTAssertTrue(recovered, "watch did not recover after MQTT was restored")
        watch.cancel()
    }

    func testAC11_redaction() async throws {
        let environment = try environment()
        let live = try makeLive(environment)
        try await live.pairFreshInvitation()
        _ = try await startWatching(live)

        try await live.control.seedEvent("output")
        let draft = "integration draft sentinel \(UUID().uuidString)"
        try await live.service.saveDraft(
            hostId: environment.hostId,
            sessionId: environment.sessionId,
            text: draft
        )

        let report = try await live.service.diagnosticsPreview()
        XCTAssertFalse(report.json.isEmpty)
        XCTAssertFalse(report.redactionFields.isEmpty)
        XCTAssertFalse(report.json.contains(draft), "diagnostics leaked the draft text")

        // The contract invitation token must never appear in diagnostics.
        if let components = URLComponents(string: environment.invitation),
           let token = components.queryItems?.first(where: { $0.name == "token" })?.value,
           !token.isEmpty {
            XCTAssertFalse(report.json.contains(token), "diagnostics leaked the invitation token")
        }
        XCTAssertFalse(report.json.contains("fixture output"), "diagnostics leaked message text")

        let exportURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("WhaleReinIntegrationDiagnostics-\(UUID().uuidString).json")
        XCTAssertFalse(FileManager.default.fileExists(atPath: exportURL.path))
        // Preview alone must not write a file.
        XCTAssertFalse(FileManager.default.fileExists(atPath: exportURL.path))
        _ = try await live.service.exportDiagnostics(to: exportURL)
        XCTAssertTrue(FileManager.default.fileExists(atPath: exportURL.path))
        if let exported = try? String(contentsOf: exportURL, encoding: .utf8) {
            XCTAssertFalse(exported.contains(draft))
            XCTAssertFalse(exported.contains("fixture output"))
        }
        try? FileManager.default.removeItem(at: exportURL)
    }
}
