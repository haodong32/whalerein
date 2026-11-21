// Service used when the live binding cannot initialize. It keeps the app
// running and reports a clear localized failure instead of crashing.
//
// The integration agent selects LiveSessionService; this type remains the
// explicit startup-failure state.

import Foundation

@MainActor
final class UnavailableSessionService: SessionService {
    let connectionStatus: ConnectionStatus = .offline
    let instanceId: String
    let coreRevision = "unavailable"
    let appVersion: String = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.1.0"

    private let code: AppErrorCode

    init(code: AppErrorCode = .integrationUnavailable, instanceId: String = "unavailable") {
        self.code = code
        self.instanceId = instanceId
    }

    func makeEventStream() -> AsyncStream<SessionEventEnvelope> {
        AsyncStream { continuation in
            continuation.finish()
        }
    }

    private func fail() throws -> Never {
        throw SessionServiceError(code)
    }

    func pairingStart(invitation: String) async throws -> PairingPrompt { try fail() }
    func pairingDecide(approved: Bool, expectedFingerprint: String?) async throws { try fail() }
    func pairingCancel() async {}
    func hosts() async throws -> [HostSummary] { try fail() }
    func sessions(hostId: String) async throws -> [SessionSummary] { try fail() }
    func cachedSessions(hostId: String) async throws -> [SessionSummary] { try fail() }
    func capabilities(hostId: String, sessionId: String) async throws -> SessionCapabilities { try fail() }
    func startWatch(hostId: String, sessionId: String) async throws { try fail() }
    func stopWatch() {}
    func send(hostId: String, sessionId: String, text: String, delivery: DeliveryChoice) async throws -> CommandReceipt { try fail() }
    func stop(hostId: String, sessionId: String) async throws -> CommandReceipt { try fail() }
    func pendingQueue(hostId: String, sessionId: String) async throws -> QueueSnapshot { try fail() }
    func updateQueueItem(
        hostId: String,
        sessionId: String,
        itemId: String,
        action: QueueAction,
        text: String
    ) async throws -> QueueMutation { try fail() }
    func beginWait(hostId: String, sessionId: String, timeoutMs: UInt64) async throws -> String { try fail() }
    func takeWaitResult(waitId: String, waitMs: UInt64) async throws -> WaitResult? { try fail() }
    func cancelWait(waitId: String) async throws -> Bool { try fail() }
    func listQuestions(hostId: String, sessionId: String, cursor: String) async throws -> QuestionPage { try fail() }
    func question(hostId: String, sessionId: String, batchId: String) async throws -> QuestionBatch { try fail() }
    func answerQuestion(
        hostId: String,
        sessionId: String,
        batch: QuestionBatch,
        answers: [QuestionAnswer]
    ) async throws -> CommandReceipt { try fail() }
    func cancelQuestionAnswer(
        hostId: String,
        sessionId: String,
        commandId: String
    ) async throws -> Bool { try fail() }
    func requestQuestionAnswerCancel(
        hostId: String,
        sessionId: String,
        commandId: String
    ) async throws -> QuestionCancelReceipt { try fail() }
    func approvals(hostId: String, sessionId: String) async throws -> [Approval] { try fail() }
    func approval(hostId: String, approvalId: String) async throws -> Approval { try fail() }
    func resolveApproval(
        hostId: String,
        sessionId: String,
        approvalId: String,
        version: Int,
        digestHex: String,
        allow: Bool
    ) async throws -> Approval { try fail() }
    func saveDraft(hostId: String, sessionId: String, text: String) async throws { try fail() }
    func loadDraft(hostId: String, sessionId: String) async throws -> String? { try fail() }
    func clearDraft(hostId: String, sessionId: String) async throws { try fail() }
    func diagnosticsPreview() async throws -> DiagnosticsReport { try fail() }
    func exportDiagnostics(to url: URL) async throws -> DiagnosticsReport { try fail() }
    func forgetDevice() async throws { try fail() }
    func revoke() async throws { try fail() }
}
