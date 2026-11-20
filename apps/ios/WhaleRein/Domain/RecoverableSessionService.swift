// A SessionService that can recover from an unavailable device Keychain.
//
// At launch the real service cannot be constructed while the device is locked
// (Keychain interaction is not allowed). This wrapper presents a recoverable
// state instead of a generic failure: each protocol call attempts construction
// once, and the user (or a foreground transition) can retry after unlocking.
// It never spins or retries on a timer.

import Foundation

@MainActor
final class RecoverableSessionService: SessionService {
    private(set) var lastFailureCode: AppErrorCode
    private(set) var recoveryAttempts = 0
    private var underlying: (any SessionService)?

    private let factory: () throws -> any SessionService
    private let fallbackInstanceId: String

    init(
        failureCode: AppErrorCode = .keychainUnavailable,
        instanceId: String = "unavailable",
        factory: @escaping () throws -> any SessionService
    ) {
        self.lastFailureCode = failureCode
        self.fallbackInstanceId = instanceId
        self.factory = factory
    }

    var isRecovered: Bool { underlying != nil }

    var connectionStatus: ConnectionStatus { underlying?.connectionStatus ?? .offline }

    var instanceId: String { underlying?.instanceId ?? fallbackInstanceId }

    var coreRevision: String { underlying?.coreRevision ?? "unavailable" }

    var appVersion: String {
        underlying?.appVersion
            ?? (Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String)
            ?? "0.1.0"
    }

    private func current() throws -> any SessionService {
        if let underlying { return underlying }
        recoveryAttempts += 1
        do {
            let service = try factory()
            underlying = service
            return service
        } catch let error as SessionServiceError {
            lastFailureCode = error.code
            throw error
        } catch {
            throw SessionServiceError(lastFailureCode)
        }
    }

    func makeEventStream() -> AsyncStream<SessionEventEnvelope> {
        if let underlying {
            return underlying.makeEventStream()
        }
        return AsyncStream { continuation in
            continuation.finish()
        }
    }

    // MARK: - Pairing

    func pairingStart(invitation: String) async throws -> PairingPrompt {
        try await current().pairingStart(invitation: invitation)
    }

    func pairingDecide(approved: Bool, expectedFingerprint: String?) async throws {
        try await current().pairingDecide(approved: approved, expectedFingerprint: expectedFingerprint)
    }

    func pairingCancel() async {
        await underlying?.pairingCancel()
    }

    // MARK: - Hosts and sessions

    func hosts() async throws -> [HostSummary] {
        try await current().hosts()
    }

    func sessions(hostId: String) async throws -> [SessionSummary] {
        try await current().sessions(hostId: hostId)
    }

    func cachedSessions(hostId: String) async throws -> [SessionSummary] {
        try await current().cachedSessions(hostId: hostId)
    }

    func capabilities(hostId: String, sessionId: String) async throws -> SessionCapabilities {
        try await current().capabilities(hostId: hostId, sessionId: sessionId)
    }

    // MARK: - Watch

    func startWatch(hostId: String, sessionId: String) async throws {
        try await current().startWatch(hostId: hostId, sessionId: sessionId)
    }

    func stopWatch() {
        underlying?.stopWatch()
    }

    // MARK: - Commands

    func send(
        hostId: String,
        sessionId: String,
        text: String,
        delivery: DeliveryChoice
    ) async throws -> CommandReceipt {
        try await current().send(hostId: hostId, sessionId: sessionId, text: text, delivery: delivery)
    }

    func stop(hostId: String, sessionId: String) async throws -> CommandReceipt {
        try await current().stop(hostId: hostId, sessionId: sessionId)
    }

    // MARK: - Questions

    func listQuestions(hostId: String, sessionId: String, cursor: String) async throws -> QuestionPage {
        try await current().listQuestions(hostId: hostId, sessionId: sessionId, cursor: cursor)
    }

    func question(hostId: String, sessionId: String, batchId: String) async throws -> QuestionBatch {
        try await current().question(hostId: hostId, sessionId: sessionId, batchId: batchId)
    }

    func answerQuestion(
        hostId: String,
        sessionId: String,
        batch: QuestionBatch,
        answers: [QuestionAnswer]
    ) async throws -> CommandReceipt {
        try await current().answerQuestion(
            hostId: hostId,
            sessionId: sessionId,
            batch: batch,
            answers: answers
        )
    }

    func cancelQuestionAnswer(
        hostId: String,
        sessionId: String,
        commandId: String
    ) async throws -> Bool {
        try await current().cancelQuestionAnswer(
            hostId: hostId,
            sessionId: sessionId,
            commandId: commandId
        )
    }

    // MARK: - Approvals

    func approvals(hostId: String, sessionId: String) async throws -> [Approval] {
        try await current().approvals(hostId: hostId, sessionId: sessionId)
    }

    func resolveApproval(
        hostId: String,
        sessionId: String,
        approvalId: String,
        version: Int,
        digestHex: String,
        allow: Bool
    ) async throws -> Approval {
        try await current().resolveApproval(
            hostId: hostId,
            sessionId: sessionId,
            approvalId: approvalId,
            version: version,
            digestHex: digestHex,
            allow: allow
        )
    }

    // MARK: - Drafts

    func saveDraft(hostId: String, sessionId: String, text: String) async throws {
        try await current().saveDraft(hostId: hostId, sessionId: sessionId, text: text)
    }

    func loadDraft(hostId: String, sessionId: String) async throws -> String? {
        try await current().loadDraft(hostId: hostId, sessionId: sessionId)
    }

    func clearDraft(hostId: String, sessionId: String) async throws {
        try await current().clearDraft(hostId: hostId, sessionId: sessionId)
    }

    // MARK: - Diagnostics

    func diagnosticsPreview() async throws -> DiagnosticsReport {
        try await current().diagnosticsPreview()
    }

    func exportDiagnostics(to url: URL) async throws -> DiagnosticsReport {
        try await current().exportDiagnostics(to: url)
    }

    // MARK: - Device lifecycle

    func forgetDevice() async throws {
        try await current().forgetDevice()
    }

    func revoke() async throws {
        try await current().revoke()
    }
}
