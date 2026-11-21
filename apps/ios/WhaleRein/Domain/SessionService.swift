// The Swift adapter boundary that the future Rust UniFFI binding implements as
// LiveSessionService. Presentation code only depends on this protocol, so
// fixtures can be replaced without touching screens or models.

import Foundation

@MainActor
protocol SessionService: AnyObject {
    var connectionStatus: ConnectionStatus { get }
    /// Stable app configuration instance this service was opened for.
    var instanceId: String { get }
    var coreRevision: String { get }
    var appVersion: String { get }
    /// Creates a fresh event stream for the caller. Each active watch consumer
    /// receives every event for its Host/session scope.
    func makeEventStream() -> AsyncStream<SessionEventEnvelope>

    // Pairing
    func pairingStart(invitation: String) async throws -> PairingPrompt
    func pairingDecide(approved: Bool, expectedFingerprint: String?) async throws
    func pairingCancel() async

    // Hosts and sessions
    func hosts() async throws -> [HostSummary]
    func sessions(hostId: String) async throws -> [SessionSummary]
    func cachedSessions(hostId: String) async throws -> [SessionSummary]
    func capabilities(hostId: String, sessionId: String) async throws -> SessionCapabilities

    // Watch
    func startWatch(hostId: String, sessionId: String) async throws
    func stopWatch()

    // Commands
    func send(hostId: String, sessionId: String, text: String, delivery: DeliveryChoice) async throws -> CommandReceipt
    func stop(hostId: String, sessionId: String) async throws -> CommandReceipt

    // Pending queue (Core-owned order and per-item action advertisement)
    func pendingQueue(hostId: String, sessionId: String) async throws -> QueueSnapshot
    func updateQueueItem(
        hostId: String,
        sessionId: String,
        itemId: String,
        action: QueueAction,
        text: String
    ) async throws -> QueueMutation

    // Wait (state-only; cancellation releases only the wait attempt)
    func beginWait(hostId: String, sessionId: String, timeoutMs: UInt64) async throws -> String
    func takeWaitResult(waitId: String, waitMs: UInt64) async throws -> WaitResult?
    func cancelWait(waitId: String) async throws -> Bool

    // Questions (Core-owned observation state; reads never acquire a claim)
    func listQuestions(hostId: String, sessionId: String, cursor: String) async throws -> QuestionPage
    func question(hostId: String, sessionId: String, batchId: String) async throws -> QuestionBatch
    func answerQuestion(
        hostId: String,
        sessionId: String,
        batch: QuestionBatch,
        answers: [QuestionAnswer]
    ) async throws -> CommandReceipt
    func cancelQuestionAnswer(hostId: String, sessionId: String, commandId: String) async throws -> Bool
    func requestQuestionAnswerCancel(
        hostId: String,
        sessionId: String,
        commandId: String
    ) async throws -> QuestionCancelReceipt

    // Approvals
    func approvals(hostId: String, sessionId: String) async throws -> [Approval]
    func approval(hostId: String, approvalId: String) async throws -> Approval
    func resolveApproval(
        hostId: String,
        sessionId: String,
        approvalId: String,
        version: Int,
        digestHex: String,
        allow: Bool
    ) async throws -> Approval

    // Drafts
    func saveDraft(hostId: String, sessionId: String, text: String) async throws
    func loadDraft(hostId: String, sessionId: String) async throws -> String?
    func clearDraft(hostId: String, sessionId: String) async throws

    // Diagnostics
    func diagnosticsPreview() async throws -> DiagnosticsReport
    func exportDiagnostics(to url: URL) async throws -> DiagnosticsReport

    // Device lifecycle
    func forgetDevice() async throws
    func revoke() async throws
}
