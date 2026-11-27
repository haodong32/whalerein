// The Swift adapter boundary that the future Rust UniFFI binding implements as
// LiveSessionService. Presentation code only depends on this protocol, so
// fixtures can be replaced without touching screens or models.

import Foundation

@MainActor
protocol SessionService: AnyObject {
    /// Stable app configuration instance this service was opened for.
    var instanceId: String { get }
    var coreRevision: String { get }
    var appVersion: String { get }
    /// Creates a fresh event stream for the caller. Each active watch consumer
    /// receives every event for its Host/session scope, plus the unscoped
    /// `hostConnection` transitions.
    func makeEventStream() -> AsyncStream<SessionEventEnvelope>

    // Host connection (one device connection per Host, independent of sessions)
    /// Authoritative connection snapshot. `nil` means it was never opened,
    /// which is different from `offline`.
    func hostConnection(hostId: String) -> HostConnectionSnapshot?
    /// Starts the app-level probe and streams `hostConnection` transitions.
    func startHostMonitor(hostIds: [String])
    /// Stops the probe and closes those connections.
    func stopHostMonitor()

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
    func acceptsWatchEvent(_ envelope: SessionEventEnvelope) -> Bool
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
    /// One capture of Core's immutable redacted diagnostic document.
    ///
    /// `refresh == false` returns the current capture (building it on the first
    /// call); `refresh == true` replaces it with a new explicit capture. Reads
    /// local Core state only: it never writes a file, uploads anything or opens
    /// a channel.
    func diagnosticsCapture(refresh: Bool) async throws -> DiagnosticsCapture
    /// Local-only decoration for the capture's anonymous Host labels. See the
    /// default implementation for the privacy contract.
    func diagnosticsLocalHosts(for capture: DiagnosticsCapture) async -> [DiagnosticsLocalHost]
    /// Clears Core's bounded diagnostic journal for this instance.
    func clearDiagnostics() async throws

    // Device lifecycle
    func forgetDevice() async throws
    func revoke() async throws
}


extension SessionService {
    func acceptsWatchEvent(_ envelope: SessionEventEnvelope) -> Bool { true }

    /// Local-only decoration for the capture's anonymous Host labels.
    ///
    /// An implementation resolves the selected profile's currently authorized
    /// directory entries and its verified Host binding. It returns only labels
    /// it can associate unambiguously; the default returns nothing so every row
    /// stays anonymous. Directory names, OS families and join keys must never
    /// be written into the capture document, logs or telemetry.
    func diagnosticsLocalHosts(for capture: DiagnosticsCapture) async -> [DiagnosticsLocalHost] { [] }
}
