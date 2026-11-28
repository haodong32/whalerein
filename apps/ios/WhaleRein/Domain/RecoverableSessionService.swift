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

    private var opening: Task<any SessionService, Error>?
    private let factory: @MainActor () async throws -> any SessionService
    private let fallbackInstanceId: String

    init(
        failureCode: AppErrorCode = .keychainUnavailable,
        instanceId: String = "unavailable",
        factory: @escaping @MainActor () async throws -> any SessionService
    ) {
        self.lastFailureCode = failureCode
        self.fallbackInstanceId = instanceId
        self.factory = factory
    }

    var isRecovered: Bool { underlying != nil }

    var instanceId: String { underlying?.instanceId ?? fallbackInstanceId }

    var coreRevision: String { underlying?.coreRevision ?? "unavailable" }

    var appVersion: String {
        underlying?.appVersion
            ?? (Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String)
            ?? "0.1.0"
    }

    private func current() async throws -> any SessionService {
        if let underlying { return underlying }
        if let opening { return try await opening.value }
        recoveryAttempts += 1
        let attempt = Task {
            defer { opening = nil }
            do {
                let service = try await factory()
                underlying = service
                return service
            } catch let error as SessionServiceError {
                lastFailureCode = error.code
                throw error
            } catch {
                throw SessionServiceError(lastFailureCode)
            }
        }
        opening = attempt
        return try await attempt.value
    }

    func makeEventStream() -> AsyncStream<SessionEventEnvelope> {
        if let underlying {
            return underlying.makeEventStream()
        }
        return AsyncStream { continuation in
            continuation.finish()
        }
    }

    // MARK: - Host connection

    func hostConnection(hostId: String) -> HostConnectionSnapshot? {
        underlying?.hostConnection(hostId: hostId)
    }

    func startHostMonitor(hostIds: [String]) {
        // The shell loads hosts before monitoring, which opens the service.
        underlying?.startHostMonitor(hostIds: hostIds)
    }

    func stopHostMonitor() {
        underlying?.stopHostMonitor()
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

    func acceptsWatchEvent(_ envelope: SessionEventEnvelope) -> Bool {
        underlying?.acceptsWatchEvent(envelope) ?? false
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

    // MARK: - Pending queue

    func pendingQueue(hostId: String, sessionId: String) async throws -> QueueSnapshot {
        try await current().pendingQueue(hostId: hostId, sessionId: sessionId)
    }

    func updateQueueItem(
        hostId: String,
        sessionId: String,
        itemId: String,
        action: QueueAction,
        text: String
    ) async throws -> QueueMutation {
        try await current().updateQueueItem(
            hostId: hostId,
            sessionId: sessionId,
            itemId: itemId,
            action: action,
            text: text
        )
    }

    // MARK: - Wait

    func beginWait(hostId: String, sessionId: String, timeoutMs: UInt64) async throws -> String {
        try await current().beginWait(hostId: hostId, sessionId: sessionId, timeoutMs: timeoutMs)
    }

    func takeWaitResult(waitId: String, waitMs: UInt64) async throws -> WaitResult? {
        try await current().takeWaitResult(waitId: waitId, waitMs: waitMs)
    }

    func cancelWait(waitId: String) async throws -> Bool {
        try await current().cancelWait(waitId: waitId)
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

    func requestQuestionAnswerCancel(
        hostId: String,
        sessionId: String,
        commandId: String
    ) async throws -> QuestionCancelReceipt {
        try await current().requestQuestionAnswerCancel(
            hostId: hostId,
            sessionId: sessionId,
            commandId: commandId
        )
    }

    // MARK: - Approvals

    func approvals(hostId: String, sessionId: String) async throws -> [Approval] {
        try await current().approvals(hostId: hostId, sessionId: sessionId)
    }

    func approval(hostId: String, approvalId: String) async throws -> Approval {
        try await current().approval(hostId: hostId, approvalId: approvalId)
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

    func diagnosticsCapture(refresh: Bool) async throws -> DiagnosticsCapture {
        try await current().diagnosticsCapture(refresh: refresh)
    }

    func clearDiagnostics() async throws {
        try await current().clearDiagnostics()
    }

    func diagnosticsLocalHosts(for capture: DiagnosticsCapture) async -> [DiagnosticsLocalHost] {
        guard let service = try? await current() else { return [] }
        return await service.diagnosticsLocalHosts(for: capture)
    }

    // MARK: - Device lifecycle

    func forgetDevice() async throws {
        try await current().forgetDevice()
    }

    func revoke() async throws {
        try await current().revoke()
    }

    // MARK: - Instance status

    func instanceStatus() async throws -> InstanceStatus {
        try await statusService().instanceStatus()
    }

    func defaultHost() async throws -> String? {
        try await statusService().defaultHost()
    }

    func selectDefaultHost(hostId: String) async throws {
        try await statusService().selectDefaultHost(hostId: hostId)
    }

    func currentCloudSessionId() async throws -> String? {
        try await statusService().currentCloudSessionId()
    }

    private func statusService() async throws -> any InstanceStatusService {
        guard let service = try await current() as? any InstanceStatusService else {
            throw SessionServiceError(.unavailable)
        }
        return service
    }
}

extension RecoverableSessionService: InstanceStatusService {}

// The recoverable wrapper must keep presenting the Cloud account and retained
// operation surfaces while the Keychain is unavailable. Without these
// forwarders the shell would decide the instance has no Cloud account at all,
// so a locked-device launch would hide sign-out, the directory and recovery
// even though the same Core state is restored after unlock.
extension RecoverableSessionService: CloudAccountService {
    func cloudAuthStart() async throws -> CloudAuthAttempt {
        try await cloudService().cloudAuthStart()
    }

    func cloudAuthComplete(callbackURL: URL) async throws -> CloudAccount {
        try await cloudService().cloudAuthComplete(callbackURL: callbackURL)
    }

    func cloudAuthCancel() async {
        try? await cloudService().cloudAuthCancel()
    }

    func cloudStatus() async throws -> CloudAccountStatus {
        try await cloudService().cloudStatus()
    }

    func cloudRefreshStatus() async throws -> CloudAccount {
        try await cloudService().cloudRefreshStatus()
    }

    func cloudSignOut() async throws -> Bool {
        try await cloudService().cloudSignOut()
    }

    func cloudAccountSessions() async throws -> [CloudAccountSession] {
        try await cloudService().cloudAccountSessions()
    }

    func cloudRevokeSession(id: String) async throws -> CloudAccountSessionState {
        try await cloudService().cloudRevokeSession(id: id)
    }

    func cloudRevokeAllSessions() async throws -> CloudAccountSessionState {
        try await cloudService().cloudRevokeAllSessions()
    }

    func cloudHosts() async throws -> [CloudDirectoryHost] {
        try await cloudService().cloudHosts()
    }

    func connectCloud(hostId: String) async throws -> CloudConnectResult {
        try await cloudService().connectCloud(hostId: hostId)
    }

    private func cloudService() async throws -> any CloudAccountService {
        guard let service = try await current() as? any CloudAccountService else {
            throw SessionServiceError(.unsupportedCapability)
        }
        return service
    }
}

extension RecoverableSessionService: OperationRecoveryService {
    func listRetainedOperations() async throws -> [OperationReceipt] {
        try await recoveryService().listRetainedOperations()
    }

    func recoverSend(operationId: String, follow: Bool) async throws -> OperationReceipt {
        try await recoveryService().recoverSend(operationId: operationId, follow: follow)
    }

    func readMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        try await recoveryService().readMessage(reference)
    }

    private func recoveryService() async throws -> any OperationRecoveryService {
        guard let service = try await current() as? any OperationRecoveryService else {
            throw SessionServiceError(.unsupportedCapability)
        }
        return service
    }
}

extension RecoverableSessionService: ConversationHistoryService {
    private func historyService() async throws -> any ConversationHistoryService {
        guard let history = try await current() as? any ConversationHistoryService else {
            throw SessionServiceError(.unsupportedCapability)
        }
        return history
    }

    func historyPage(
        hostId: String,
        sessionId: String,
        source: HistorySource,
        continuation: String?,
        maxMessages: Int,
        maxBytes: Int
    ) async throws -> ConversationHistoryPage {
        try await historyService().historyPage(
            hostId: hostId, sessionId: sessionId, source: source,
            continuation: continuation, maxMessages: maxMessages, maxBytes: maxBytes
        )
    }

    func historyMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        try await historyService().historyMessage(reference)
    }
}
