// Fixture-backed SessionService. Deterministic data, no network. The integration
// agent replaces this with LiveSessionService implementing the same protocol.

import Foundation

/// One fixture wait attempt. The continuation is resumed once by whichever of
/// completion, cancellation or the caller's take deadline happens first.
@MainActor
private final class FixtureWaitAttempt {
    var hostID: String = ""
    var sessionID: String = ""
    var result: WaitResult?
    var waiter: CheckedContinuation<WaitResult?, Never>?
    var task: Task<Void, Never>?
}

@MainActor
final class FixtureSessionService: SessionService {
    let instanceId: String
    let coreRevision: String = FixtureCatalog.coreRevision
    var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.1.0"
    }

    private var continuations: [UUID: AsyncStream<SessionEventEnvelope>.Continuation] = [:]
    private(set) var connectionStatus: ConnectionStatus = .online

    private let options: LaunchOptions
    private let clock: FixtureClock
    private let draftStore: any DraftStore

    private var paired: Bool
    private var revoked = false
    private var pendingPrompt: PairingPrompt?
    private var pendingInvitation: String?
    private var activeWatch: SessionContext?
    private var previousWatch: SessionContext?
    private var sequence = 0
    private var commandCounter = 0
    private var didStream = false
    private var didSimulateApprovalChange = false
    private var didReturnUncertain = false
    private var runningSessions: Set<String> = []
    private var messagesBySession: [String: [ConversationMessage]] = [:]
    private var approvalsBySession: [String: Approval] = [:]
    private var questionBatchesBySession: [String: [QuestionBatch]] = [:]
    private var questionRevisionBySession: [String: UInt64] = [:]
    private var questionInitializedSessions: Set<String> = []
    private var openQuestionCommands: Set<String> = []
    private var queueBySession: [String: QueueSnapshot] = [:]
    private var queueReadFails = false
    private var queueMutations: [(itemID: String, action: QueueAction, text: String)] = []
    private var waitAttempts: [String: FixtureWaitAttempt] = [:]
    private var waitCounter = 0
    private var streamTask: Task<Void, Never>?
    private var leakTask: Task<Void, Never>?
    private var settlementTasks: [Task<Void, Never>] = []
    private var lifecycleTasks: [Task<Void, Never>] = []
    private var didEmitLifecycleEvents = false

    init(
        options: LaunchOptions = LaunchOptions(),
        clock: FixtureClock = SystemFixtureClock(),
        draftStore: any DraftStore,
        initiallyPaired: Bool = false,
        instanceId: String = "fixtures"
    ) {
        self.instanceId = instanceId
        self.options = options
        self.clock = clock
        self.draftStore = draftStore
        self.paired = initiallyPaired
    }

    func makeEventStream() -> AsyncStream<SessionEventEnvelope> {
        let id = UUID()
        let (stream, continuation) = AsyncStream.makeStream(of: SessionEventEnvelope.self)
        continuations[id] = continuation
        continuation.onTermination = { [weak self] _ in
            Task { @MainActor [weak self] in
                self?.continuations[id] = nil
            }
        }
        return stream
    }

    // MARK: - Pairing

    func pairingStart(invitation: String) async throws -> PairingPrompt {
        if revoked { throw SessionServiceError(.revoked) }
        // Resume the same attempt when the original response was lost.
        if let pending = pendingPrompt, pendingInvitation == invitation {
            return pending
        }
        if options.slowPairing {
            await clock.sleep(for: .milliseconds(1_500))
        }
        switch InvitationParser.parse(invitation, now: clock.now()) {
        case .failure(let error):
            let code: AppErrorCode = error == .expired ? .invitationExpired : .invalidInvitation
            throw SessionServiceError(code, detail: "invitation parse failed")
        case .success(let value):
            let hostId = value.hostHint ?? LaunchOptions.fixtureInvitationHostId
            guard FixtureCatalog.hosts.contains(where: { $0.hostId == hostId }) else {
                throw SessionServiceError(.invalidInvitation, detail: "unknown host")
            }
            let prompt = PairingPrompt(
                hostId: hostId,
                accessURL: value.accessURL,
                fingerprint: FixtureCatalog.fingerprint,
                fingerprintDisplay: FixtureCatalog.fingerprintDisplay,
                expiresAt: clock.now().addingTimeInterval(120)
            )
            pendingPrompt = prompt
            pendingInvitation = invitation
            return prompt
        }
    }

    func pairingDecide(approved: Bool, expectedFingerprint: String?) async throws {
        guard let prompt = pendingPrompt else {
            throw SessionServiceError(.invalidInvitation, detail: "no pending prompt")
        }
        if let expectedFingerprint, expectedFingerprint != prompt.fingerprint {
            throw SessionServiceError(.fingerprintMismatch)
        }
        if approved, clock.now() >= prompt.expiresAt {
            pendingPrompt = nil
            pendingInvitation = nil
            throw SessionServiceError(.invitationExpired)
        }
        if approved {
            paired = true
            pendingPrompt = nil
            pendingInvitation = nil
        } else {
            paired = false
            pendingPrompt = nil
            pendingInvitation = nil
        }
    }

    func pairingCancel() async {
        pendingPrompt = nil
        pendingInvitation = nil
    }

    // MARK: - Hosts and sessions

    func hosts() async throws -> [HostSummary] {
        try requirePaired()
        guard options.incompatibleProtocol || options.legacyProtocol else {
            return FixtureCatalog.hosts
        }
        let version = options.legacyProtocol ? "0" : "2"
        return FixtureCatalog.hosts.map { host in
            HostSummary(
                hostId: host.hostId,
                online: host.online,
                fingerprint: host.fingerprint,
                protocolVersion: version
            )
        }
    }

    func sessions(hostId: String) async throws -> [SessionSummary] {
        try requirePaired()
        guard let host = FixtureCatalog.hosts.first(where: { $0.hostId == hostId }) else {
            throw SessionServiceError(.unavailable, detail: "unknown host")
        }
        guard host.online else { throw SessionServiceError(.hostOffline) }
        return FixtureCatalog.sessions(hostId: hostId).map(withRunningState)
    }

    func cachedSessions(hostId: String) async throws -> [SessionSummary] {
        try requirePaired()
        return FixtureCatalog.sessions(hostId: hostId).map(withRunningState)
    }

    func capabilities(hostId: String, sessionId: String) async throws -> SessionCapabilities {
        try requirePaired()
        let readSupported = !options.queueUnsupported
        if sessionId == "s-legacy" {
            return SessionCapabilities(
                running: runningSessions.contains(sessionId),
                supportsSteer: false,
                supportsQueue: true,
                supportsQueueRead: readSupported,
                supportsQueueEdit: false,
                supportsQueueRemove: false,
                supportsQueuePromote: false
            )
        }
        return SessionCapabilities(
            running: runningSessions.contains(sessionId),
            supportsSteer: true,
            supportsQueue: true,
            supportsQueueRead: readSupported,
            supportsQueueEdit: readSupported,
            supportsQueueRemove: readSupported,
            supportsQueuePromote: readSupported
        )
    }

    /// Mirrors the Source-reported running state the fixture keeps in memory.
    private func withRunningState(_ session: SessionSummary) -> SessionSummary {
        var copy = session
        copy.running = runningSessions.contains(session.sessionId)
        return copy
    }

    // MARK: - Watch

    func startWatch(hostId: String, sessionId: String) async throws {
        try requirePaired()
        guard FixtureCatalog.sessions(hostId: hostId).contains(where: { $0.sessionId == sessionId }) else {
            throw SessionServiceError(.unavailable, detail: "unknown session")
        }
        let context = SessionContext(hostId: hostId, sessionId: sessionId)
        let sameWatch = activeWatch == context
        if !sameWatch {
            streamTask?.cancel()
            streamTask = nil
            if let activeWatch {
                previousWatch = activeWatch
            }
        } else if streamTask == nil, options.streamOnOpen {
            // A UI test can restart the same watch after the view was rebuilt.
            // Allow the deterministic scripted stream to run again instead of
            // leaving the transcript without its preview phase.
            didStream = false
        }
        activeWatch = context
        let messages = messages(for: hostId, sessionId: sessionId)
        let approval = approval(for: hostId, sessionId: sessionId)
        emit(.reset(messages: messages, approvals: [approval]))
        if options.questionsOnOpen {
            emitQuestionsForTesting(hostId: hostId, sessionId: sessionId)
        }
        startScriptedStreamIfNeeded(hostId: hostId, sessionId: sessionId)
        scheduleLifecycleEventsIfNeeded(hostId: hostId, sessionId: sessionId)
        if options.lateEventLeak, let previousWatch, previousWatch != activeWatch {
            leakTask?.cancel()
            leakTask = Task { [weak self] in
                await self?.clock.sleep(for: .milliseconds(1_200))
                guard let self, !Task.isCancelled else { return }
                self.emit(
                    .preview(attemptId: "leaked-attempt", text: "LATE-EVENT-LEAK", replace: true, incomplete: true),
                    hostId: previousWatch.hostId,
                    sessionId: previousWatch.sessionId
                )
            }
        }
    }

    func stopWatch() {
        streamTask?.cancel()
        streamTask = nil
        leakTask?.cancel()
        leakTask = nil
        settlementTasks.forEach { $0.cancel() }
        settlementTasks.removeAll()
        lifecycleTasks.forEach { $0.cancel() }
        lifecycleTasks.removeAll()
        if let activeWatch {
            emit(.watchEnded(reason: "stopped"))
            previousWatch = activeWatch
        }
        activeWatch = nil
    }

    /// Deterministic one-shot lifecycle scenarios used by UI tests. They fire
    /// after the first watch's reset so the app has a loaded conversation.
    private func scheduleLifecycleEventsIfNeeded(hostId: String, sessionId: String) {
        guard !didEmitLifecycleEvents else { return }
        didEmitLifecycleEvents = true
        if options.revokedOnOpen {
            let task = Task { [weak self] in
                guard let self else { return }
                await self.clock.sleep(for: .milliseconds(180))
                guard !Task.isCancelled else { return }
                self.revoked = true
                self.emit(
                    .error(code: .revoked, message: "revoked"),
                    hostId: hostId,
                    sessionId: sessionId
                )
            }
            lifecycleTasks.append(task)
        } else if options.watchEndedOnOpen {
            let task = Task { [weak self] in
                guard let self else { return }
                await self.clock.sleep(for: .milliseconds(180))
                guard !Task.isCancelled else { return }
                self.activeWatch = nil
                self.connectionStatus = .offline
                self.emit(.watchEnded(reason: "ended"), hostId: hostId, sessionId: sessionId)
            }
            lifecycleTasks.append(task)
        } else if options.offlineOnOpen {
            let task = Task { [weak self] in
                guard let self else { return }
                await self.clock.sleep(for: .milliseconds(180))
                guard !Task.isCancelled else { return }
                self.connectionStatus = .offline
                self.emit(.connection(hostId: hostId, status: .offline), hostId: hostId, sessionId: sessionId)
            }
            lifecycleTasks.append(task)
        }
    }

    private func startScriptedStreamIfNeeded(hostId: String, sessionId: String) {
        guard options.streamOnOpen, !didStream else { return }
        didStream = true
        runningSessions.insert(sessionId)
        streamTask = Task { [weak self] in
            guard let self else { return }
            var accumulated = ""
            let attemptId = "attempt-live"
            for chunk in 1...8 {
                await self.clock.sleep(for: .milliseconds(1_200))
                if Task.isCancelled { return }
                accumulated += "LIVE-STREAM-CHUNK \(String(format: "%02d", chunk))\n"
                self.emit(
                    .preview(attemptId: attemptId, text: accumulated, replace: true, incomplete: chunk < 8),
                    hostId: hostId,
                    sessionId: sessionId
                )
            }
            let finalMessage = ConversationMessage(
                messageId: "msg-live-0001",
                role: .assistant,
                blocks: [
                    ConversationBlock(
                        blockId: "msg-live-0001-block-0",
                        kind: .text,
                        text: "Streaming completed. All **8** synthetic chunks were applied."
                    )
                ],
                createdAtUnixMs: Int64(self.clock.now().timeIntervalSince1970 * 1000)
            )
            self.appendMessage(finalMessage, hostId: hostId, sessionId: sessionId)
            self.runningSessions.remove(sessionId)
            self.emit(.message(finalMessage), hostId: hostId, sessionId: sessionId)
            self.emit(.attemptEnded(attemptId: attemptId, outcome: .completed), hostId: hostId, sessionId: sessionId)
        }
    }

    // MARK: - Commands

    func send(hostId: String, sessionId: String, text: String, delivery: DeliveryChoice) async throws -> CommandReceipt {
        try requirePaired()
        guard activeWatch == SessionContext(hostId: hostId, sessionId: sessionId) else {
            throw SessionServiceError(.unavailable, detail: "watch not active")
        }
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw SessionServiceError(.invalidInvitation, detail: "empty message")
        }
        let capabilities = try await capabilities(hostId: hostId, sessionId: sessionId)
        if capabilities.running {
            switch delivery {
            case .send:
                throw SessionServiceError(.unsupportedDelivery)
            case .steer where !capabilities.supportsSteer:
                throw SessionServiceError(.unsupportedDelivery)
            case .queue where !capabilities.supportsQueue:
                throw SessionServiceError(.unsupportedDelivery)
            case .steer, .queue:
                break
            }
        } else if delivery != .send {
            throw SessionServiceError(.unsupportedDelivery)
        }

        commandCounter += 1
        let receiptId = String(format: "cmd-%04d", commandCounter)
        let uncertain = options.sendUncertain && !didReturnUncertain
        if uncertain { didReturnUncertain = true }
        if !capabilities.running {
            runningSessions.insert(sessionId)
        }
        let echoMessageId = options.sendEcho ? "msg-echo-\(receiptId)" : nil
        var receipt = CommandReceipt(
            commandId: receiptId,
            requestId: "req-\(receiptId)",
            status: uncertain ? .uncertain : .accepted,
            delivery: delivery,
            messageId: echoMessageId,
            deliveryId: options.sendEcho ? "del-\(receiptId)" : nil
        )
        if uncertain {
            scheduleSettlement(receipt: receipt, hostId: hostId, sessionId: sessionId)
        }
        if let echoMessageId {
            scheduleEcho(
                messageId: echoMessageId,
                text: text,
                hostId: hostId,
                sessionId: sessionId
            )
        }
        return receipt
    }

    // MARK: - Pending queue

    func pendingQueue(hostId: String, sessionId: String) async throws -> QueueSnapshot {
        try requirePaired()
        if options.queueUnsupported {
            throw SessionServiceError(.unsupportedCapability, detail: "queue read unsupported")
        }
        if queueReadFails {
            throw SessionServiceError(.unavailable, detail: "queue read failed")
        }
        if let existing = queueBySession[sessionId] {
            return existing
        }
        let created = QueueSnapshot(
            available: true,
            running: runningSessions.contains(sessionId),
            items: options.queueOnOpen ? Self.fixtureQueueItems() : []
        )
        queueBySession[sessionId] = created
        return created
    }

    func updateQueueItem(
        hostId: String,
        sessionId: String,
        itemId: String,
        action: QueueAction,
        text: String
    ) async throws -> QueueMutation {
        try requirePaired()
        guard var snapshot = queueBySession[sessionId] else {
            throw SessionServiceError(.unavailable, detail: "queue not read")
        }
        guard let index = snapshot.items.firstIndex(where: { $0.itemID == itemId }) else {
            // The item was consumed between the menu and the action.
            throw SessionServiceError(.conflict, detail: "queue item consumed")
        }
        guard snapshot.items[index].advertises(action) else {
            throw SessionServiceError(.conflict, detail: "queue action not advertised")
        }
        queueMutations.append((itemID: itemId, action: action, text: text))
        var items = snapshot.items
        switch action {
        case .edit:
            let original = items[index]
            items[index] = QueueItem(
                itemID: original.itemID,
                placement: original.placement,
                deliveryID: original.deliveryID,
                blocks: [.text(text)],
                actions: original.actions
            )
        case .remove:
            items.remove(at: index)
        case .promote:
            let original = items[index]
            items[index] = QueueItem(
                itemID: original.itemID,
                placement: "steering",
                deliveryID: original.deliveryID,
                blocks: original.blocks,
                actions: original.actions
            )
        }
        snapshot = QueueSnapshot(
            available: true,
            running: snapshot.running,
            items: items
        )
        queueBySession[sessionId] = snapshot
        commandCounter += 1
        let receipt = CommandReceipt(
            commandId: String(format: "cmd-queue-%04d", commandCounter),
            requestId: String(format: "req-queue-%04d", commandCounter),
            status: .confirmed,
            delivery: nil
        )
        if options.queueMutationRefreshFails {
            queueReadFails = true
        }
        return QueueMutation(
            operationID: receipt.commandId,
            hostID: hostId,
            sessionID: sessionId,
            itemID: itemId,
            action: action,
            receipt: receipt
        )
    }

    // MARK: - Wait

    func beginWait(hostId: String, sessionId: String, timeoutMs: UInt64) async throws -> String {
        try requirePaired()
        waitCounter += 1
        let waitID = String(format: "wait-%04d", waitCounter)
        let attempt = FixtureWaitAttempt()
        attempt.hostID = hostId
        attempt.sessionID = sessionId
        waitAttempts[waitID] = attempt
        let idle = WaitResult(
            waitID: waitID,
            hostID: hostId,
            sessionID: sessionId,
            outcome: .idle,
            state: WaitSnapshot(
                active: false,
                running: runningSessions.contains(sessionId),
                queueAvailable: true,
                pendingInput: []
            ),
            errorCode: nil
        )
        let task = Task { [weak self] in
            guard let self else { return }
            await self.clock.sleep(for: .milliseconds(80))
            guard !Task.isCancelled else { return }
            self.settleWait(waitID, result: idle)
        }
        attempt.task = task
        return waitID
    }

    func takeWaitResult(waitId: String, waitMs: UInt64) async throws -> WaitResult? {
        try requirePaired()
        guard let attempt = waitAttempts[waitId] else {
            throw SessionServiceError(.unavailable, detail: "wait not found")
        }
        if let result = attempt.result {
            waitAttempts[waitId] = nil
            return result
        }
        return await withCheckedContinuation { continuation in
            attempt.waiter = continuation
            attempt.task?.cancel()
            attempt.task = Task { [weak self] in
                try? await Task.sleep(for: .milliseconds(Int(waitMs)))
                guard !Task.isCancelled else { return }
                self?.fulfilWait(waitId, result: nil)
            }
        }
    }

    func cancelWait(waitId: String) async throws -> Bool {
        try requirePaired()
        guard let attempt = waitAttempts[waitId], attempt.result == nil else {
            return false
        }
        let cancelled = WaitResult(
            waitID: waitId,
            hostID: attempt.hostID,
            sessionID: attempt.sessionID,
            outcome: .cancelled,
            state: nil,
            errorCode: "cancelled"
        )
        settleWait(waitId, result: cancelled)
        return true
    }

    /// Complete one fixture wait deterministically from a test.
    func completeWaitForTesting(
        _ waitID: String,
        outcome: WaitOutcome,
        state: WaitSnapshot? = nil,
        errorCode: String? = nil
    ) {
        guard let attempt = waitAttempts[waitID] else { return }
        settleWait(
            waitID,
            result: WaitResult(
                waitID: waitID,
                hostID: attempt.hostID,
                sessionID: attempt.sessionID,
                outcome: outcome,
                state: state,
                errorCode: errorCode
            )
        )
    }

    private func settleWait(_ waitID: String, result: WaitResult) {
        guard let attempt = waitAttempts[waitID] else { return }
        if let waiter = attempt.waiter {
            attempt.waiter = nil
            attempt.task?.cancel()
            attempt.task = nil
            waitAttempts[waitID] = nil
            waiter.resume(returning: result)
        } else {
            attempt.result = result
        }
    }

    private func fulfilWait(_ waitID: String, result: WaitResult?) {
        guard let attempt = waitAttempts[waitID] else { return }
        let waiter = attempt.waiter
        attempt.waiter = nil
        if result != nil {
            waitAttempts[waitID] = nil
        }
        waiter?.resume(returning: result)
    }

    /// Replace the queue fixture deterministically from a test.
    func seedQueueForTesting(
        _ items: [QueueItem],
        hostId: String = "studio-mac",
        sessionId: String = "s-001"
    ) {
        queueBySession[sessionId] = QueueSnapshot(
            available: true,
            running: runningSessions.contains(sessionId),
            items: items
        )
    }

    func setQueueReadFailureForTesting(_ fails: Bool) {
        queueReadFails = fails
    }

    var queueMutationsForTesting: [(itemID: String, action: QueueAction, text: String)] {
        queueMutations
    }

    private static func fixtureQueueItems() -> [QueueItem] {
        [
            QueueItem(
                itemID: "queue-1",
                placement: "queued",
                deliveryID: "delivery-1",
                blocks: [.text("first queued instruction")],
                actions: [.edit, .remove]
            ),
            QueueItem(
                itemID: "queue-2",
                placement: "queued",
                deliveryID: "delivery-2",
                blocks: [
                    .text("second queued instruction"),
                    .attachment(
                        mediaType: "image/png",
                        name: "screenshot.png",
                        size: 42,
                        available: true
                    ),
                ],
                actions: [.edit, .remove, .promote]
            ),
        ]
    }

    func stop(hostId: String, sessionId: String) async throws -> CommandReceipt {
        try requirePaired()
        guard activeWatch == SessionContext(hostId: hostId, sessionId: sessionId) else {
            throw SessionServiceError(.unavailable, detail: "watch not active")
        }
        guard runningSessions.contains(sessionId) else {
            throw SessionServiceError(.conflict, detail: "not running")
        }
        commandCounter += 1
        let receipt = CommandReceipt(
            commandId: String(format: "cmd-%04d", commandCounter),
            requestId: String(format: "req-cmd-%04d", commandCounter),
            status: .uncertain,
            delivery: nil
        )
        let stopReceipt = receipt
        let task = Task { [weak self] in
            guard let self else { return }
            await self.clock.sleep(for: .milliseconds(320))
            if Task.isCancelled { return }
            self.runningSessions.remove(sessionId)
            var settled = stopReceipt
            settled.status = .confirmed
            self.emit(.commandStatus(settled), hostId: hostId, sessionId: sessionId)
            self.emit(.attemptEnded(attemptId: "attempt-live", outcome: .cancelled), hostId: hostId, sessionId: sessionId)
        }
        settlementTasks.append(task)
        return receipt
    }

    private func scheduleSettlement(receipt: CommandReceipt, hostId: String, sessionId: String) {
        // UI tests need a visible uncertain window before the Host settles.
        let delay = options.sendUncertain ? 4_000 : 320
        let task = Task { [weak self] in
            guard let self else { return }
            await self.clock.sleep(for: .milliseconds(delay))
            if Task.isCancelled { return }
            var settled = receipt
            settled.status = .confirmed
            self.emit(.commandStatus(settled), hostId: hostId, sessionId: sessionId)
            self.emit(.attemptEnded(attemptId: "attempt-live", outcome: .completed), hostId: hostId, sessionId: sessionId)
        }
        settlementTasks.append(task)
    }

    /// Simulates a Core that commits the user message and echoes it back with
    /// its own message id.
    private func scheduleEcho(
        messageId: String,
        text: String,
        hostId: String,
        sessionId: String
    ) {
        let task = Task { [weak self] in
            guard let self else { return }
            await self.clock.sleep(for: .milliseconds(120))
            guard !Task.isCancelled else { return }
            let message = ConversationMessage(
                messageId: messageId,
                role: .user,
                blocks: [
                    ConversationBlock(
                        blockId: "\(messageId)-block-0",
                        kind: .text,
                        text: text
                    )
                ],
                createdAtUnixMs: Int64(self.clock.now().timeIntervalSince1970 * 1000)
            )
            self.appendMessage(message, hostId: hostId, sessionId: sessionId)
            self.emit(.message(message), hostId: hostId, sessionId: sessionId)
        }
        settlementTasks.append(task)
    }

    // MARK: - Questions

    func listQuestions(hostId: String, sessionId: String, cursor: String) async throws -> QuestionPage {
        try requirePaired()
        let batches = questionBatches(hostId: hostId, sessionId: sessionId)
        return QuestionPage(
            batches: batches,
            nextCursor: "",
            stamp: questionStamp(sessionId: sessionId),
            complete: true
        )
    }

    func question(hostId: String, sessionId: String, batchId: String) async throws -> QuestionBatch {
        try requirePaired()
        guard let batch = questionBatches(hostId: hostId, sessionId: sessionId)
            .first(where: { $0.batchID == batchId }) else {
            throw SessionServiceError(.unavailable, detail: "question not found")
        }
        return batch
    }

    func answerQuestion(
        hostId: String,
        sessionId: String,
        batch: QuestionBatch,
        answers: [QuestionAnswer]
    ) async throws -> CommandReceipt {
        try requirePaired()
        guard activeWatch == SessionContext(hostId: hostId, sessionId: sessionId) else {
            throw SessionServiceError(.unavailable, detail: "watch not active")
        }
        guard let current = questionBatches(hostId: hostId, sessionId: sessionId)
            .first(where: { $0.batchID == batch.batchID }) else {
            throw SessionServiceError(.conflict, detail: "question is no longer pending")
        }
        guard current.guardValue == batch.guardValue else {
            throw SessionServiceError(.conflict, detail: "question guard changed")
        }
        guard batch.canAnswer else {
            throw SessionServiceError(.conflict, detail: "question phase is not answerable")
        }
        let questionIds = Set(batch.questions.map(\.id))
        guard !answers.isEmpty,
              answers.allSatisfy({ questionIds.contains($0.id) }) else {
            throw SessionServiceError(.invalidInvitation, detail: "unknown question id")
        }
        questionBatchesBySession[sessionId] = []
        questionRevisionBySession[sessionId] = (questionRevisionBySession[sessionId] ?? 1) + 1
        emitQuestionsForTesting(hostId: hostId, sessionId: sessionId)
        commandCounter += 1
        let receipt = CommandReceipt(
            commandId: String(format: "cmd-answer-%04d", commandCounter),
            requestId: String(format: "req-answer-%04d", commandCounter),
            status: .accepted,
            delivery: nil
        )
        openQuestionCommands.insert(receipt.commandId)
        let receiptID = receipt.commandId
        let task = Task { [weak self] in
            guard let self else { return }
            await self.clock.sleep(for: .milliseconds(320))
            guard !Task.isCancelled else { return }
            self.openQuestionCommands.remove(receiptID)
            self.emit(.commandStatus(receipt), hostId: hostId, sessionId: sessionId)
        }
        settlementTasks.append(task)
        return receipt
    }

    func cancelQuestionAnswer(
        hostId: String,
        sessionId: String,
        commandId: String
    ) async throws -> Bool {
        try requirePaired()
        return openQuestionCommands.remove(commandId) != nil
    }

    func requestQuestionAnswerCancel(
        hostId: String,
        sessionId: String,
        commandId: String
    ) async throws -> QuestionCancelReceipt {
        let requested = try await cancelQuestionAnswer(
            hostId: hostId,
            sessionId: sessionId,
            commandId: commandId
        )
        return QuestionCancelReceipt(commandID: commandId, cancellationRequested: requested)
    }

    // MARK: - Approvals

    func approvals(hostId: String, sessionId: String) async throws -> [Approval] {
        try requirePaired()
        return [approval(for: hostId, sessionId: sessionId)]
    }

    func approval(hostId: String, approvalId: String) async throws -> Approval {
        try requirePaired()
        let current = approval(for: hostId, sessionId: "s-001")
        guard current.approvalId == approvalId else {
            throw SessionServiceError(.unavailable, detail: "approval not found")
        }
        return current
    }

    func resolveApproval(
        hostId: String,
        sessionId: String,
        approvalId: String,
        version: Int,
        digestHex: String,
        allow: Bool
    ) async throws -> Approval {
        try requirePaired()
        var current = approval(for: hostId, sessionId: sessionId)
        guard current.approvalId == approvalId, current.sessionId == sessionId else {
            throw SessionServiceError(.conflict, detail: "approval not found")
        }
        guard current.state == .pending else {
            throw SessionServiceError(.conflict, detail: "approval is not pending")
        }
        // `fastClock` simulates a device clock ahead of the Host: the fixture
        // Host still considers the request valid, so it accepts the decision
        // even though the app's local clock is past the absolute expiry.
        if options.approvalVariant != .fastClock {
            guard clock.now().timeIntervalSince1970 * 1000 < Double(current.expiresAtUnixMs) else {
                current.state = .expired
                approvalsBySession[sessionId] = current
                emit(.approvalChanged(current), hostId: hostId, sessionId: sessionId)
                throw SessionServiceError(.conflict, detail: "approval expired")
            }
        }
        guard current.version == version, current.digestHex == digestHex else {
            current.state = .changed
            approvalsBySession[sessionId] = current
            emit(.approvalChanged(current), hostId: hostId, sessionId: sessionId)
            throw SessionServiceError(.staleApproval, detail: "reviewed variant is stale")
        }
        if options.approvalVariant == .changed, !didSimulateApprovalChange {
            didSimulateApprovalChange = true
            current = Approval(
                approvalId: current.approvalId,
                sessionId: current.sessionId,
                toolName: current.toolName,
                callId: current.callId,
                reason: current.reason + " (updated command)",
                version: current.version + 1,
                expiresAtUnixMs: current.expiresAtUnixMs,
                digestHex: "b7e2c4a6d8f01e3c5a7b9d1f3e5c7a9b1d3f5e7c9a1b3d5f7e9c1a3b5d7f9e1c",
                state: .pending
            )
            approvalsBySession[sessionId] = current
            emit(.approvalChanged(current), hostId: hostId, sessionId: sessionId)
            throw SessionServiceError(.staleApproval, detail: "approval changed elsewhere")
        }
        current.state = allow ? .allowed : .rejected
        approvalsBySession[sessionId] = current
        emit(.approvalChanged(current), hostId: hostId, sessionId: sessionId)
        schedulePendingOnlyRefreshIfNeeded(hostId: hostId, sessionId: sessionId)
        return current
    }

    /// Simulates a real Core state refresh whose approval list contains only
    /// pending requests. The decided card must survive on the client.
    private func schedulePendingOnlyRefreshIfNeeded(hostId: String, sessionId: String) {
        guard options.approvalRefreshAfterDecision else { return }
        let task = Task { [weak self] in
            guard let self else { return }
            await self.clock.sleep(for: .milliseconds(300))
            guard !Task.isCancelled else { return }
            self.emit(.approvals([]), hostId: hostId, sessionId: sessionId)
        }
        settlementTasks.append(task)
    }

    // MARK: - Drafts

    func saveDraft(hostId: String, sessionId: String, text: String) async throws {
        try draftStore.save(text, hostId: hostId, sessionId: sessionId)
    }

    func loadDraft(hostId: String, sessionId: String) async throws -> String? {
        try draftStore.load(hostId: hostId, sessionId: sessionId)
    }

    func clearDraft(hostId: String, sessionId: String) async throws {
        try draftStore.clear(hostId: hostId, sessionId: sessionId)
    }

    // MARK: - Diagnostics

    func diagnosticsPreview() async throws -> DiagnosticsReport {
        try requirePaired()
        if options.diagnosticsFailure {
            throw SessionServiceError(.unavailable)
        }
        let now = clock.now()
        return FixtureCatalog.diagnostics(now: now)
    }

    func exportDiagnostics(to url: URL) async throws -> DiagnosticsReport {
        try requirePaired()
        let report = try await diagnosticsPreview()
        let text = DiagnosticsRedactor.exportText(report: report)
        try Data(text.utf8).write(to: url, options: [.atomic, .completeFileProtection])
        return report
    }

    // MARK: - Device lifecycle

    func forgetDevice() async throws {
        paired = false
        revoked = false
        pendingPrompt = nil
        pendingInvitation = nil
        stopWatch()
        runningSessions.removeAll()
        approvalsBySession.removeAll()
        messagesBySession.removeAll()
        for host in FixtureCatalog.hosts {
            for session in FixtureCatalog.sessions(hostId: host.hostId) {
                try? draftStore.clear(hostId: host.hostId, sessionId: session.sessionId)
            }
        }
    }

    func revoke() async throws {
        stopWatch()
        paired = false
        revoked = true
        pendingPrompt = nil
        pendingInvitation = nil
        runningSessions.removeAll()
        approvalsBySession.removeAll()
        messagesBySession.removeAll()
        for host in FixtureCatalog.hosts {
            for session in FixtureCatalog.sessions(hostId: host.hostId) {
                try? draftStore.clear(hostId: host.hostId, sessionId: session.sessionId)
            }
        }
    }

    // MARK: - Test helpers

    /// Emits the deterministic question state for one session. Unit tests use
    /// this to drive freshness and guard changes without a Core observation.
    func emitQuestionStateForTesting(_ state: QuestionsState, hostId: String, sessionId: String) {
        emit(.questions(state), hostId: hostId, sessionId: sessionId)
    }

    private func emitQuestionsForTesting(hostId: String, sessionId: String) {
        let batches = questionBatches(hostId: hostId, sessionId: sessionId)
        emit(
            .questions(
                QuestionsState(
                    freshness: .current,
                    stamp: questionStamp(sessionId: sessionId),
                    batches: batches,
                    error: nil
                )
            ),
            hostId: hostId,
            sessionId: sessionId
        )
    }

    private func questionBatches(hostId: String, sessionId: String) -> [QuestionBatch] {
        if let cached = questionBatchesBySession[sessionId] {
            return cached
        }
        guard options.questionsOnOpen,
              questionInitializedSessions.insert(sessionId).inserted else {
            return []
        }
        let batch = FixtureCatalog.questionBatch(sessionId: sessionId)
        questionBatchesBySession[sessionId] = [batch]
        questionRevisionBySession[sessionId] = 1
        return [batch]
    }

    private func questionStamp(sessionId: String) -> QuestionStamp {
        QuestionStamp(
            observationEpoch: "fixture-questions-\(sessionId)",
            revision: questionRevisionBySession[sessionId] ?? 1
        )
    }

    /// Emits an event for the active watch. Unit tests use this to drive the
    /// presentation model without starting scripted timers.
    func emitForTesting(_ event: MessageEvent) {
        guard let activeWatch else { return }
        emit(event, hostId: activeWatch.hostId, sessionId: activeWatch.sessionId)
    }

    func emitForTesting(_ event: MessageEvent, hostId: String, sessionId: String) {
        emit(event, hostId: hostId, sessionId: sessionId)
    }

    @discardableResult
    func settleCommandsForTesting() -> Bool {
        let tasks = settlementTasks
        settlementTasks.removeAll()
        for task in tasks {
            task.cancel()
        }
        return !tasks.isEmpty
    }

    // MARK: - Internal

    private func requirePaired() throws {
        if revoked { throw SessionServiceError(.revoked) }
        guard paired else { throw SessionServiceError(.notPaired) }
    }

    private func messages(for hostId: String, sessionId: String) -> [ConversationMessage] {
        if let cached = messagesBySession[sessionId] { return cached }
        let generated = FixtureCatalog.conversation(sessionId: sessionId, hostId: hostId, now: clock.now())
        messagesBySession[sessionId] = generated
        return generated
    }

    private func appendMessage(_ message: ConversationMessage, hostId: String, sessionId: String) {
        var current = messages(for: hostId, sessionId: sessionId)
        current.append(message)
        messagesBySession[sessionId] = current
    }

    private func approval(for hostId: String, sessionId: String) -> Approval {
        if var cached = approvalsBySession[sessionId] {
            // The fixture Host enforces its own short TTL. `fastClock` is
            // intentionally exempt so the Host keeps reporting it pending.
            if options.approvalVariant == .shortLived,
               cached.state == .pending,
               clock.now().timeIntervalSince1970 * 1000 >= Double(cached.expiresAtUnixMs) {
                cached.state = .expired
                approvalsBySession[sessionId] = cached
            }
            return cached
        }
        let created = FixtureCatalog.approval(
            sessionId: sessionId,
            now: clock.now(),
            variant: options.approvalVariant
        )
        approvalsBySession[sessionId] = created
        return created
    }

    private func emit(_ event: MessageEvent, hostId: String? = nil, sessionId: String? = nil) {
        let host = hostId ?? activeWatch?.hostId ?? ""
        let session = sessionId ?? activeWatch?.sessionId ?? ""
        sequence += 1
        let envelope = SessionEventEnvelope(hostId: host, sessionId: session, sequence: sequence, event: event)
        for continuation in continuations.values {
            continuation.yield(envelope)
        }
    }
}
