// Shared deterministic test support: launch options, paired fixture service,
// temporary directories, a recording SessionService stub, and async helpers.

import Foundation
@testable import WhaleRein

func testLaunchOptions(_ extra: [String] = [], approvalState: String? = nil) -> LaunchOptions {
    let suiteName = "WhaleReinTests.\(UUID().uuidString)"
    let defaults = UserDefaults(suiteName: suiteName)!
    defaults.removePersistentDomain(forName: suiteName)
    if let approvalState {
        defaults.set(approvalState, forKey: "ui-approval-state")
    }
    return LaunchOptions(arguments: ["WhaleRein"] + extra, defaults: defaults)
}

func makeTempDirectory() -> URL {
    let url = FileManager.default.temporaryDirectory
        .appendingPathComponent("WhaleReinTests-\(UUID().uuidString)", isDirectory: true)
    try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
    return url
}

@MainActor
func makePairedFixture(
    options: LaunchOptions = testLaunchOptions(),
    clock: FixtureClock = TestFixtureClock(),
    draftStore: (any DraftStore)? = nil
) async throws -> FixtureSessionService {
    let resolvedStore = draftStore ?? InMemoryDraftStore()
    let service = FixtureSessionService(options: options, clock: clock, draftStore: resolvedStore)
    let invitation = FixtureCatalog.invitationURL(hostId: "studio-mac")
    _ = try await service.pairingStart(invitation: invitation.absoluteString)
    try await service.pairingDecide(approved: true, expectedFingerprint: FixtureCatalog.fingerprint)
    return service
}

func fixturePrompt(hostId: String = "studio-mac") -> PairingPrompt {
    PairingPrompt(
        hostId: hostId,
        accessURL: URL(string: "https://\(hostId).access.whalerein.test")!,
        fingerprint: FixtureCatalog.fingerprint,
        fingerprintDisplay: FixtureCatalog.fingerprintDisplay,
        expiresAt: Date().addingTimeInterval(120)
    )
}

func testApproval(
    id: String = "approval-1",
    sessionId: String = "s-001",
    state: ApprovalState = .pending,
    version: Int = 3,
    expiresAt: Date = Date().addingTimeInterval(900),
    digest: String = "digest-1"
) -> Approval {
    Approval(
        approvalId: id,
        sessionId: sessionId,
        toolName: "bash",
        callId: "call-\(id)",
        reason: "Run verification",
        version: version,
        expiresAtUnixMs: Int64(expiresAt.timeIntervalSince1970 * 1000),
        digestHex: digest,
        state: state
    )
}

/// Writes a raw preferences payload so recovery tests can exercise corrupt,
/// migrated and future-schema files without touching UserDefaults.
func writePreferencesPayload(_ payload: String, in directory: URL) throws {
    let url = directory.appendingPathComponent(AppStorageLocations.preferencesFileName)
    try Data(payload.utf8).write(to: url, options: .atomic)
}

@MainActor
func waitUntil(
    timeout: TimeInterval = 3,
    _ condition: @MainActor () -> Bool
) async {
    let deadline = Date().addingTimeInterval(timeout)
    while !condition() && Date() < deadline {
        try? await Task.sleep(for: .milliseconds(5))
    }
}

@MainActor
func pump(_ milliseconds: Int = 20) async {
    try? await Task.sleep(for: .milliseconds(milliseconds))
}

@MainActor
final class StubSessionService: SessionService {
    var connectionStatus: ConnectionStatus = .online
    var coreRevision = "stub-add-ios-client-1"
    var appVersion = "0.1.0"
    private var continuations: [UUID: AsyncStream<SessionEventEnvelope>.Continuation] = [:]

    var pairingStartHandler: ((String) async throws -> PairingPrompt)?
    var pairingDecideHandler: ((Bool, String?) async throws -> Void)?
    var hostsHandler: (() async throws -> [HostSummary])?
    var sessionsHandler: ((String) async throws -> [SessionSummary])?
    var cachedSessionsHandler: ((String) async throws -> [SessionSummary])?
    var capabilitiesHandler: ((String, String) async throws -> SessionCapabilities)?
    var startWatchHandler: ((String, String) async throws -> Void)?
    var sendHandler: ((String, String, String, DeliveryChoice) async throws -> CommandReceipt)?
    var stopHandler: ((String, String) async throws -> CommandReceipt)?
    var approvalsHandler: ((String, String) async throws -> [Approval])?
    var resolveHandler: ((String, String, String, Int, String, Bool) async throws -> Approval)?
    var diagnosticsHandler: (() async throws -> DiagnosticsReport)?
    var exportHandler: ((URL) async throws -> DiagnosticsReport)?
    var forgetHandler: (() async throws -> Void)?
    var revokeHandler: (() async throws -> Void)?

    private(set) var pairingStartCount = 0
    private(set) var pairingDecideArguments: [(approved: Bool, fingerprint: String?)] = []
    private(set) var pairingCancelCount = 0
    private(set) var startWatchCalls: [SessionContext] = []
    private(set) var stopWatchCount = 0
    private(set) var sendCalls: [(String, String, String, DeliveryChoice)] = []
    private(set) var stopCalls: [(String, String)] = []
    private(set) var resolveCalls: [(approvalId: String, version: Int, digest: String, allow: Bool)] = []
    private(set) var savedDrafts: [String: String] = [:]
    private(set) var clearedDraftKeys: [String] = []
    private(set) var diagnosticsPreviewCount = 0
    private(set) var exportCount = 0
    private(set) var hostsFetchCount = 0
    private(set) var sessionsFetchCount = 0
    private(set) var eventStreamCount = 0
    private(set) var terminatedStreamCount = 0

    var activeStreamCount: Int { continuations.count }

    func makeEventStream() -> AsyncStream<SessionEventEnvelope> {
        eventStreamCount += 1
        let id = UUID()
        let (stream, continuation) = AsyncStream.makeStream(of: SessionEventEnvelope.self)
        continuations[id] = continuation
        continuation.onTermination = { [weak self] _ in
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.continuations[id] = nil
                self.terminatedStreamCount += 1
            }
        }
        return stream
    }

    func emit(_ event: MessageEvent, hostId: String = "studio-mac", sessionId: String = "s-001", sequence: Int = 1) {
        let envelope = SessionEventEnvelope(hostId: hostId, sessionId: sessionId, sequence: sequence, event: event)
        for continuation in continuations.values {
            continuation.yield(envelope)
        }
    }

    func emitToAll(_ events: [MessageEvent], hostId: String = "studio-mac", sessionId: String = "s-001") {
        for (index, event) in events.enumerated() {
            emit(event, hostId: hostId, sessionId: sessionId, sequence: index + 1)
        }
    }

    func pairingStart(invitation: String) async throws -> PairingPrompt {
        pairingStartCount += 1
        guard let pairingStartHandler else { throw SessionServiceError(.unavailable) }
        return try await pairingStartHandler(invitation)
    }

    func pairingDecide(approved: Bool, expectedFingerprint: String?) async throws {
        pairingDecideArguments.append((approved, expectedFingerprint))
        guard let pairingDecideHandler else { throw SessionServiceError(.unavailable) }
        try await pairingDecideHandler(approved, expectedFingerprint)
    }

    func pairingCancel() async {
        pairingCancelCount += 1
    }

    func hosts() async throws -> [HostSummary] {
        hostsFetchCount += 1
        guard let hostsHandler else { return FixtureCatalog.hosts }
        return try await hostsHandler()
    }

    func sessions(hostId: String) async throws -> [SessionSummary] {
        sessionsFetchCount += 1
        guard let sessionsHandler else { return FixtureCatalog.sessions(hostId: hostId) }
        return try await sessionsHandler(hostId)
    }

    func cachedSessions(hostId: String) async throws -> [SessionSummary] {
        guard let cachedSessionsHandler else { return FixtureCatalog.sessions(hostId: hostId) }
        return try await cachedSessionsHandler(hostId)
    }

    func capabilities(hostId: String, sessionId: String) async throws -> SessionCapabilities {
        guard let capabilitiesHandler else { return .idle }
        return try await capabilitiesHandler(hostId, sessionId)
    }

    func startWatch(hostId: String, sessionId: String) async throws {
        startWatchCalls.append(SessionContext(hostId: hostId, sessionId: sessionId))
        if let startWatchHandler {
            try await startWatchHandler(hostId, sessionId)
        }
    }

    func stopWatch() {
        stopWatchCount += 1
    }

    func send(hostId: String, sessionId: String, text: String, delivery: DeliveryChoice) async throws -> CommandReceipt {
        sendCalls.append((hostId, sessionId, text, delivery))
        guard let sendHandler else {
            return CommandReceipt(commandId: "cmd-1", requestId: "req-1", status: .accepted, delivery: delivery)
        }
        return try await sendHandler(hostId, sessionId, text, delivery)
    }

    func stop(hostId: String, sessionId: String) async throws -> CommandReceipt {
        stopCalls.append((hostId, sessionId))
        guard let stopHandler else {
            return CommandReceipt(commandId: "cmd-stop-1", requestId: "req-stop-1", status: .uncertain, delivery: nil)
        }
        return try await stopHandler(hostId, sessionId)
    }

    func approvals(hostId: String, sessionId: String) async throws -> [Approval] {
        guard let approvalsHandler else { return [] }
        return try await approvalsHandler(hostId, sessionId)
    }

    func resolveApproval(
        hostId: String,
        sessionId: String,
        approvalId: String,
        version: Int,
        digestHex: String,
        allow: Bool
    ) async throws -> Approval {
        resolveCalls.append((approvalId, version, digestHex, allow))
        guard let resolveHandler else { throw SessionServiceError(.conflict) }
        return try await resolveHandler(hostId, sessionId, approvalId, version, digestHex, allow)
    }

    func saveDraft(hostId: String, sessionId: String, text: String) async throws {
        let key = AppStorageLocations.sessionKey(hostId: hostId, sessionId: sessionId)
        if text.isEmpty {
            savedDrafts.removeValue(forKey: key)
        } else {
            savedDrafts[key] = text
        }
    }

    func loadDraft(hostId: String, sessionId: String) async throws -> String? {
        savedDrafts[AppStorageLocations.sessionKey(hostId: hostId, sessionId: sessionId)]
    }

    func clearDraft(hostId: String, sessionId: String) async throws {
        let key = AppStorageLocations.sessionKey(hostId: hostId, sessionId: sessionId)
        clearedDraftKeys.append(key)
        savedDrafts.removeValue(forKey: key)
    }

    func diagnosticsPreview() async throws -> DiagnosticsReport {
        diagnosticsPreviewCount += 1
        guard let diagnosticsHandler else { return FixtureCatalog.diagnostics() }
        return try await diagnosticsHandler()
    }

    func exportDiagnostics(to url: URL) async throws -> DiagnosticsReport {
        exportCount += 1
        guard let exportHandler else { return try await diagnosticsPreview() }
        return try await exportHandler(url)
    }

    func forgetDevice() async throws {
        if let forgetHandler { try await forgetHandler() }
    }

    func revoke() async throws {
        if let revokeHandler { try await revokeHandler() }
    }
}

@MainActor
func makeStartedConversation(
    service: any SessionService,
    hostId: String = "studio-mac",
    sessionId: String = "s-001",
    title: String = "Release checklist"
) async -> ConversationModel {
    let model = ConversationModel(hostId: hostId, sessionId: sessionId, sessionTitle: title, service: service)
    await model.start()
    await pump(160)
    return model
}
