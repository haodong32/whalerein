// Live SessionService over the Rust Core UniFFI binding.
//
// The service is the only place that touches `MobileCore`. Every binding call
// runs off the main thread; Core events are drained by one background pump,
// converted into domain envelopes with a monotonic per-service sequence, and
// broadcast to the AsyncStream consumers created by presentation models.
//
// Nothing here logs secrets, invitation URIs, message text or tool parameters.

import Foundation
import WhaleReinMobile

@MainActor
final class LiveSessionService: SessionService {
    let core: MobileCore
    let instanceId: String
    let appVersion: String

    private var eventPump: Task<Void, Never>?
    private var continuations: [UUID: AsyncStream<SessionEventEnvelope>.Continuation] = [:]
    private(set) var connectionStatus: ConnectionStatus = .offline

    private var activeWatch: SessionContext?
    private var watchGeneration: UInt64 = 0
    private var sequence = 0
    private var latestApprovals: [Approval] = []
    private var cachedHistory: [ConversationMessage] = []
    private var cachedHistoryById: [String: ConversationMessage] = [:]
    private var cachedCapabilities: SessionCapabilities?
    private var lastPairingOutcome: PairingOutcome?
    private var pairingWaiters: [UUID: CheckedContinuation<PairingOutcome, Error>] = [:]

    var coreRevision: String { WhaleReinMobile.coreRevision() }

    // MARK: - Construction

    /// Production initializer: the materialized per-instance TOML produced by
    /// the authoritative app document plus an explicit Keychain namespace.
    ///
    /// Opening never creates or rewrites configuration; composition owns that.
    convenience init(
        configPath: URL,
        keychainService: String,
        appVersion: String,
        instanceId: String
    ) throws {
        let store = KeychainSecretStore(service: keychainService)
        try self.init(
            configPath: configPath,
            secretStore: store,
            appVersion: appVersion,
            instanceId: instanceId
        )
    }

    /// Test initializer with an explicit materialized config and secret store.
    init(
        configPath: URL,
        secretStore: any SecretStore,
        appVersion: String,
        instanceId: String
    ) throws {
        do {
            self.core = try MobileCore(
                configPath: configPath.path,
                secretStore: secretStore,
                appVersion: appVersion
            )
        } catch let error as MobileError {
            throw SessionServiceError(Self.map(error: error))
        }
        self.instanceId = instanceId
        self.appVersion = appVersion
        self.eventPump = startEventPump()
    }

    /// Isolated Simulator/test initializer: process-local secrets, no Keychain.
    static func memory(
        configPath: URL,
        appVersion: String,
        instanceId: String
    ) throws -> LiveSessionService {
        let core: MobileCore
        do {
            core = try MobileCore.newWithMemorySecretStore(
                configPath: configPath.path,
                appVersion: appVersion
            )
        } catch let error as MobileError {
            throw SessionServiceError(Self.map(error: error))
        }
        return LiveSessionService(core: core, appVersion: appVersion, instanceId: instanceId)
    }

    private init(core: MobileCore, appVersion: String, instanceId: String) {
        self.core = core
        self.instanceId = instanceId
        self.appVersion = appVersion
        self.eventPump = startEventPump()
    }

    deinit {
        eventPump?.cancel()
    }

    // MARK: - Event stream

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

    private func startEventPump() -> Task<Void, Never> {
        let core = self.core
        return Task.detached(priority: .utility) { [weak self, core] in
            while !Task.isCancelled {
                guard let self else { return }
                let generation = await self.watchGenerationSnapshot()
                let events = core.pollEvents(timeoutMs: 500)
                if events.isEmpty { continue }
                await self.ingest(events, generation: generation)
            }
        }
    }

    private func watchGenerationSnapshot() -> UInt64 { watchGeneration }

    private func ingest(_ events: [ConversationEvent], generation: UInt64) {
        for event in events {
            apply(event, generation: generation)
        }
    }

    private func apply(_ event: ConversationEvent, generation: UInt64) {
        switch event {
        case .pairingFinished(let hostId, let code):
            let outcome = PairingOutcome(hostId: hostId, code: code)
            lastPairingOutcome = outcome
            resumePairingWaiters(with: outcome)
            return
        case .pairingPrompt:
            return
        default:
            break
        }

        guard generation == watchGeneration, let scope = activeWatch else { return }

        switch event {
        case .state(_, let running, _, let approvals):
            // Core only emits an authoritative SessionState after a
            // successful watch/snapshot handoff, so this implies connectivity
            // even when the short-lived `connected` poll transition is missed.
            if connectionStatus != .online {
                connectionStatus = .online
                broadcast(.state(.online), scope: scope)
            }
            let mapped = approvals.map(Self.map(approval:))
            latestApprovals = mapped
            broadcast(.approvals(mapped), scope: scope)
            var capabilities = cachedCapabilities
                ?? SessionCapabilities(running: running, supportsSteer: false, supportsQueue: false)
            capabilities.running = running
            cachedCapabilities = capabilities
            broadcast(.capabilities(capabilities), scope: scope)
            // Core emits this state after the baseline history has been
            // committed to the encrypted cache. Refresh the local transcript
            // now so a cold watch shows seeded history even though the first
            // reset ran before the baseline completed.
            Task { @MainActor [weak self] in
                await self?.reloadCachedHistory(scope: scope)
            }

        case .historyReset:
            broadcast(.reset(messages: cachedHistory, approvals: latestApprovals), scope: scope)

        case .message(let message):
            let mapped = Self.map(message: message)
            if let cached = cachedHistoryById[mapped.messageId] {
                if cached == mapped {
                    // Already emitted from the local cache before live events.
                    return
                }
                cachedHistoryById[mapped.messageId] = mapped
                if let index = cachedHistory.firstIndex(where: { $0.messageId == mapped.messageId }) {
                    cachedHistory[index] = mapped
                }
                broadcast(.replace(message: mapped), scope: scope)
                return
            }
            broadcast(.message(mapped), scope: scope)

        case .preview(let attemptId, let kind, let text, let replace, let incomplete):
            // Answer and reasoning previews keep distinct typed identities.
            switch kind {
            case "reasoning":
                broadcast(
                    .reasoningPreview(
                        attemptId: attemptId,
                        text: text,
                        replace: replace,
                        incomplete: incomplete
                    ),
                    scope: scope
                )
            case "text":
                broadcast(
                    .preview(attemptId: attemptId, text: text, replace: replace, incomplete: incomplete),
                    scope: scope
                )
            default:
                break
            }

        case .toolActivity(let activity):
            broadcast(
                .toolActivity(Self.map(toolActivity: activity)),
                scope: scope
            )

        case .attemptEnded(let attemptId, let outcome):
            broadcast(
                .attemptEnded(attemptId: attemptId, outcome: Self.map(outcome: outcome)),
                scope: scope
            )

        case .connection(let hostId, let status):
            guard hostId == scope.hostId else { return }
            let mapped = Self.map(connection: status)
            connectionStatus = mapped
            broadcast(.connection(hostId: hostId, status: mapped), scope: scope)

        case .commandStatus(let command):
            broadcast(.commandStatus(Self.map(command: command, delivery: nil)), scope: scope)

        case .approvalChanged(let approval):
            let mapped = Self.map(approval: approval)
            latestApprovals.removeAll { $0.approvalId == mapped.approvalId }
            latestApprovals.append(mapped)
            broadcast(.approvalChanged(mapped), scope: scope)

        case .questions(let view):
            // Core owns reconciliation and freshness. The app never polls or
            // turns a missing/loading view into an empty collection.
            broadcast(.questions(Self.map(questions: view)), scope: scope)

        case .error(let code, _):
            broadcast(
                .error(code: Self.map(code: code), message: Self.staticMessage(for: Self.map(code: code))),
                scope: scope
            )

        case .droppedEvents(let count):
            // The bounded Core queue discarded older events. Surface the loss
            // explicitly instead of silently treating the transcript as whole.
            broadcast(
                .error(
                    code: .internalError,
                    message: "The event stream dropped \(count) events and must be refreshed."
                ),
                scope: scope
            )

        case .watchEnded:
            connectionStatus = .offline
            activeWatch = nil
            broadcast(.watchEnded(reason: "ended"), scope: scope)

        case .pairingFinished, .pairingPrompt:
            break
        }
    }

    private func broadcast(_ event: MessageEvent, scope: SessionContext) {
        sequence += 1
        let envelope = SessionEventEnvelope(
            hostId: scope.hostId,
            sessionId: scope.sessionId,
            sequence: sequence,
            event: event
        )
        for continuation in continuations.values {
            continuation.yield(envelope)
        }
    }

    private func finishStreams() {
        for continuation in continuations.values {
            continuation.finish()
        }
        continuations.removeAll()
    }

    // MARK: - Pairing

    func pairingStart(invitation: String) async throws -> PairingPrompt {
        lastPairingOutcome = nil
        let core = self.core
        let view = try await call { try core.pairingStart(invitation: invitation) }
        guard let accessURL = URL(string: view.accessUrl) else {
            throw SessionServiceError(.invalidInvitation)
        }
        return PairingPrompt(
            hostId: view.hostId,
            accessURL: accessURL,
            fingerprint: view.fingerprint,
            fingerprintDisplay: view.fingerprintDisplay,
            expiresAt: Date().addingTimeInterval(120)
        )
    }

    func pairingDecide(approved: Bool, expectedFingerprint: String?) async throws {
        let core = self.core
        try await call {
            try core.pairingDecide(approved: approved, expectedFingerprint: expectedFingerprint)
        }
        guard approved else { return }
        let outcome = try await awaitPairingOutcome(timeout: .seconds(30))
        guard let hostId = outcome.hostId, !hostId.isEmpty else {
            throw SessionServiceError(Self.map(pairingCode: outcome.code))
        }
    }

    func pairingCancel() async {
        let core = self.core
        _ = try? await call { core.pairingCancel() }
        let outcome = PairingOutcome(hostId: nil, code: "cancelled")
        lastPairingOutcome = outcome
        resumePairingWaiters(with: outcome)
    }

    private func awaitPairingOutcome(timeout: Duration) async throws -> PairingOutcome {
        if let outcome = lastPairingOutcome {
            lastPairingOutcome = nil
            return outcome
        }
        let id = UUID()
        let timeoutTask = Task { [weak self] in
            try? await Task.sleep(for: timeout)
            guard !Task.isCancelled else { return }
            await self?.failPairingWaiter(id)
        }
        defer { timeoutTask.cancel() }
        return try await withCheckedThrowingContinuation { continuation in
            pairingWaiters[id] = continuation
        }
    }

    private func failPairingWaiter(_ id: UUID) {
        guard let continuation = pairingWaiters.removeValue(forKey: id) else { return }
        continuation.resume(throwing: SessionServiceError(.timeout))
    }

    private func resumePairingWaiters(with outcome: PairingOutcome) {
        let waiters = pairingWaiters
        pairingWaiters.removeAll()
        for continuation in waiters.values {
            continuation.resume(returning: outcome)
        }
    }

    // MARK: - Hosts and sessions

    func hosts() async throws -> [HostSummary] {
        let core = self.core
        let hosts = try await call { try core.hosts() }
        return hosts.map { host in
            HostSummary(
                hostId: host.hostId,
                online: host.online,
                fingerprint: host.fingerprint,
                protocolVersion: String(host.protocolVersion)
            )
        }
    }

    func sessions(hostId: String) async throws -> [SessionSummary] {
        let core = self.core
        do {
            let sessions = try await call { try core.sessions(hostId: hostId) }
            return sessions.map(Self.map(session:))
        } catch let error as SessionServiceError
            where error.code == .timeout || error.code == .unavailable || error.code == .hostOffline {
            // With the broker down the remote list cannot complete, but the
            // encrypted cache is still an authoritative offline view. Return it
            // instead of surfacing a transport timeout.
            let cached = try await cachedSessions(hostId: hostId)
            if !cached.isEmpty { return cached }
            throw error
        }
    }

    func cachedSessions(hostId: String) async throws -> [SessionSummary] {
        let core = self.core
        let sessions = try await call { try core.cachedSessions(hostId: hostId) }
        return sessions.map(Self.map(session:))
    }

    func capabilities(hostId: String, sessionId: String) async throws -> SessionCapabilities {
        let core = self.core
        let sessions = try await call { try core.sessions(hostId: hostId) }
        guard let summary = sessions.first(where: { $0.sessionId == sessionId }) else {
            throw SessionServiceError(.unavailable)
        }
        let view: SessionCapabilitiesView
        if let reported = summary.capabilities {
            view = reported
        } else {
            // The Source did not report capabilities on the summary. Ask Core
            // directly; it fails with `unavailable` instead of inventing them.
            view = try await call { try core.capabilities(hostId: hostId, sessionId: sessionId) }
        }
        let mapped = Self.map(capabilities: view, running: summary.running)
        cachedCapabilities = mapped
        return mapped
    }

    // MARK: - Watch

    func startWatch(hostId: String, sessionId: String) async throws {
        let core = self.core
        // Stop the previous watch and make its in-flight events stale before
        // the new scope exists.
        watchGeneration &+= 1
        activeWatch = nil
        _ = try? await call { core.stopWatch() }

        // Record the real Source capabilities for this session before the
        // stream starts so state events can update only the running flag.
        cachedCapabilities = try? await capabilities(hostId: hostId, sessionId: sessionId)
        // Warm the durable session list while the Host is reachable so offline
        // lists can fall back to the cache when the broker is down.
        _ = try? await sessions(hostId: hostId)

        // Load the encrypted local transcript so the reset event can restore
        // history after a reconnect or process restart. Live messages with the
        // same immutable id are deduplicated below.
        let cached = try? await call {
            try core.cachedMessages(hostId: hostId, sessionId: sessionId)
        }
        cachedHistory = (cached ?? []).map(Self.map(message:))
        cachedHistoryById = Dictionary(
            uniqueKeysWithValues: cachedHistory.map { ($0.messageId, $0) }
        )

        watchGeneration &+= 1
        let generation = watchGeneration
        let scope = SessionContext(hostId: hostId, sessionId: sessionId)
        activeWatch = scope
        do {
            try await call { try core.startWatch(hostId: hostId, sessionId: sessionId) }
        } catch {
            if watchGeneration == generation {
                activeWatch = nil
            }
            throw error
        }
    }

    func stopWatch() {
        watchGeneration &+= 1
        activeWatch = nil
        connectionStatus = .offline
        let core = self.core
        _ = Task.detached { core.stopWatch() }
        finishStreams()    }

    // MARK: - Commands

    func send(
        hostId: String,
        sessionId: String,
        text: String,
        delivery: DeliveryChoice
    ) async throws -> CommandReceipt {
        let core = self.core
        let bindingDelivery = Self.map(delivery: delivery)
        let view = try await call {
            try core.send(
                hostId: hostId,
                sessionId: sessionId,
                text: text,
                delivery: bindingDelivery
            )
        }
        return Self.map(command: view, delivery: delivery)
    }

    func stop(hostId: String, sessionId: String) async throws -> CommandReceipt {
        let core = self.core
        let view = try await call { try core.stop(hostId: hostId, sessionId: sessionId) }
        return Self.map(command: view, delivery: nil)
    }

    // MARK: - Pending queue

    func pendingQueue(hostId: String, sessionId: String) async throws -> QueueSnapshot {
        let core = self.core
        let view = try await call {
            try core.pendingQueue(hostId: hostId, sessionId: sessionId)
        }
        return Self.map(queue: view)
    }

    func updateQueueItem(
        hostId: String,
        sessionId: String,
        itemId: String,
        action: QueueAction,
        text: String
    ) async throws -> QueueMutation {
        let core = self.core
        let bindingAction = Self.map(queueAction: action)
        let view = try await call {
            try core.updatePendingQueue(
                hostId: hostId,
                sessionId: sessionId,
                itemId: itemId,
                action: bindingAction,
                text: text
            )
        }
        let receipt = Self.map(command: view.receipt, delivery: nil)
        return QueueMutation(
            itemID: view.itemId,
            action: QueueAction(rawValue: view.action) ?? action,
            operation: OperationReceipt(
                operationId: view.operationId,
                kind: .control,
                scope: OperationScope(
                    instanceId: instanceId,
                    hostId: view.hostId,
                    sessionId: view.sessionId
                ),
                status: Self.map(operationStatus: receipt.status),
                receipt: receipt,
                reply: nil,
                content: nil,
                readError: nil
            )
        )
    }

    // MARK: - Wait

    func beginWait(hostId: String, sessionId: String, timeoutMs: UInt64) async throws -> String {
        let core = self.core
        return try await call {
            try core.beginWait(hostId: hostId, sessionId: sessionId, timeoutMs: timeoutMs)
        }
    }

    func takeWaitResult(waitId: String, waitMs: UInt64) async throws -> WaitResult? {
        let core = self.core
        let view = try await call {
            try core.takeWaitResult(waitId: waitId, waitMs: waitMs)
        }
        return view.map(Self.map(wait:))
    }

    func cancelWait(waitId: String) async throws -> Bool {
        let core = self.core
        return try await call { try core.cancelWait(waitId: waitId) }
    }

    /// Encrypted local transcript for restart/cache verification.
    func cachedTranscript(hostId: String, sessionId: String) async throws -> [ConversationMessage] {
        let core = self.core
        let cached = try await call {
            try core.cachedMessages(hostId: hostId, sessionId: sessionId)
        }
        return cached.map(Self.map(message:))
    }

    // MARK: - Questions

    func listQuestions(hostId: String, sessionId: String, cursor: String) async throws -> QuestionPage {
        let core = self.core
        let page = try await call {
            try core.listQuestions(hostId: hostId, sessionId: sessionId, cursor: cursor)
        }
        return Self.map(page: page)
    }

    func question(hostId: String, sessionId: String, batchId: String) async throws -> QuestionBatch {
        let core = self.core
        let batch = try await call {
            try core.getQuestion(hostId: hostId, sessionId: sessionId, batchId: batchId)
        }
        return Self.map(questionBatch: batch)
    }

    func answerQuestion(
        hostId: String,
        sessionId: String,
        batch: QuestionBatch,
        answers: [QuestionAnswer]
    ) async throws -> CommandReceipt {
        let core = self.core
        let bindingBatch = Self.map(questionBatch: batch)
        let bindingAnswers = answers.map(Self.map(answer:))
        let view = try await call {
            try core.answerQuestion(
                hostId: hostId,
                sessionId: sessionId,
                batch: bindingBatch,
                answers: bindingAnswers
            )
        }
        return Self.map(command: view, delivery: nil)
    }

    func cancelQuestionAnswer(
        hostId: String,
        sessionId: String,
        commandId: String
    ) async throws -> Bool {
        let core = self.core
        return try await call {
            try core.cancelQuestionAnswer(
                hostId: hostId,
                sessionId: sessionId,
                commandId: commandId
            )
        }
    }

    func requestQuestionAnswerCancel(
        hostId: String,
        sessionId: String,
        commandId: String
    ) async throws -> QuestionCancelReceipt {
        let core = self.core
        let view = try await call {
            try core.requestQuestionAnswerCancel(
                hostId: hostId,
                sessionId: sessionId,
                commandId: commandId
            )
        }
        return QuestionCancelReceipt(
            commandID: view.commandId,
            cancellationRequested: view.cancellationRequested
        )
    }

    // MARK: - Approvals

    func approvals(hostId: String, sessionId: String) async throws -> [Approval] {
        let core = self.core
        let approvals = try await call { try core.approvals(hostId: hostId, sessionId: sessionId) }
        return approvals.map(Self.map(approval:))
    }

    func approval(hostId: String, approvalId: String) async throws -> Approval {
        let core = self.core
        let view = try await call {
            try core.getApproval(hostId: hostId, approvalId: approvalId)
        }
        return Self.map(approval: view)
    }

    func resolveApproval(
        hostId: String,
        sessionId: String,
        approvalId: String,
        version: Int,
        digestHex: String,
        allow: Bool
    ) async throws -> Approval {
        let core = self.core
        // The exact Host-scoped read, not the pending list, owns the guard the
        // decision is bound to. A changed, resolved or out-of-scope request is
        // refused before any decision is submitted.
        let reviewed = try await call {
            try core.getApproval(hostId: hostId, approvalId: approvalId)
        }
        guard reviewed.sessionId == sessionId else {
            throw SessionServiceError(.conflict)
        }
        guard reviewed.state == "pending" else {
            throw SessionServiceError(.staleApproval)
        }
        guard reviewed.version == Int64(version),
              reviewed.digestHex.caseInsensitiveCompare(digestHex) == .orderedSame else {
            throw SessionServiceError(.staleApproval)
        }
        _ = try await call {
            try core.resolveApproval(
                hostId: hostId,
                sessionId: sessionId,
                approvalId: approvalId,
                version: Int64(version),
                digestHex: digestHex,
                allow: allow
            )
        }
        var mapped = Self.map(approval: reviewed)
        mapped.state = allow ? .allowed : .rejected
        return mapped
    }

    // MARK: - Drafts

    func saveDraft(hostId: String, sessionId: String, text: String) async throws {
        let core = self.core
        try await call { try core.saveDraft(hostId: hostId, sessionId: sessionId, text: text) }
    }

    func loadDraft(hostId: String, sessionId: String) async throws -> String? {
        let core = self.core
        return try await call { try core.loadDraft(hostId: hostId, sessionId: sessionId) }
    }

    func clearDraft(hostId: String, sessionId: String) async throws {
        let core = self.core
        try await call { try core.clearDraft(hostId: hostId, sessionId: sessionId) }
    }

    // MARK: - Diagnostics

    func diagnosticsPreview() async throws -> DiagnosticsReport {
        let core = self.core
        let view = try await call { try core.diagnosticsPreview() }
        return Self.map(diagnostics: view)
    }

    func exportDiagnostics(to url: URL) async throws -> DiagnosticsReport {
        let core = self.core
        let view = try await call { try core.exportDiagnostics(path: url.path) }
        return Self.map(diagnostics: view)
    }

    // MARK: - Device lifecycle

    func forgetDevice() async throws {
        let core = self.core
        try await call { try core.forgetDevice() }
        stopWatch()
    }

    func revoke() async throws {
        let core = self.core
        try await call { try core.revoke() }
        stopWatch()
    }

    // MARK: - Core calls

    /// Runs one synchronous Core call on a detached thread and converts binding
    /// failures into typed app errors. Raw Core text never crosses this seam.
    // MARK: - Feature extension access

    /// Binding access for feature adapters that live in separate files.
    var bindingCore: MobileCore { core }

    /// Runs one binding call with this service's typed error mapping.
    func bindingCall<T: Sendable>(_ body: @escaping @Sendable () throws -> T) async throws -> T {
        try await call(body)
    }

    func call<T: Sendable>(_ body: @escaping @Sendable () throws -> T) async throws -> T {
        do {
            return try await Task.detached(priority: .userInitiated) { try body() }.value
        } catch let error as MobileError {
            throw SessionServiceError(Self.map(error: error))
        } catch let error as SessionServiceError {
            throw error
        } catch {
            throw SessionServiceError(.internalError)
        }
    }

    // MARK: - Binding mapping

    private static func map(questions view: WhaleReinMobile.QuestionsView) -> QuestionsState {
        QuestionsState(
            freshness: QuestionFreshness(bindingLabel: view.freshness),
            stamp: view.stamp.map {
                QuestionStamp(observationEpoch: $0.observationEpoch, revision: $0.revision)
            },
            batches: view.batches.map(map(questionBatch:)),
            error: view.error,
            truncatedForDisplay: view.truncatedForDisplay,
            batchCount: Int(view.batchCount)
        )
    }

    private static func map(page view: WhaleReinMobile.QuestionPageView) -> QuestionPage {
        QuestionPage(
            batches: view.batches.map(map(questionBatch:)),
            nextCursor: view.nextCursor,
            stamp: view.stamp.map {
                QuestionStamp(observationEpoch: $0.observationEpoch, revision: $0.revision)
            },
            complete: view.complete
        )
    }

    private static func map(questionBatch view: WhaleReinMobile.QuestionBatchView) -> QuestionBatch {
        QuestionBatch(
            sessionID: view.sessionId,
            batchID: view.batchId,
            sourceEpoch: view.sourceEpoch,
            version: view.version,
            digestHex: view.digestHex,
            phase: view.phase,
            questions: view.questions.map(map(questionItem:)),
            answerable: view.answerable,
            contentUnavailable: view.contentUnavailable
        )
    }

    private static func map(questionItem view: WhaleReinMobile.QuestionItemView) -> QuestionItem {
        QuestionItem(
            id: view.id,
            question: view.question,
            detail: view.detail,
            header: view.header,
            options: view.options.map {
                QuestionOption(label: $0.label, description: $0.description)
            },
            multiSelect: view.multiSelect,
            intent: view.intent.map { intent in
                QuestionIntent(
                    kind: intent.kind,
                    approve: intent.approve,
                    reject: intent.reject,
                    always: intent.always,
                    callID: intent.callId,
                    subject: intent.subject.map {
                        QuestionSubject(name: $0.name, icon: $0.icon)
                    },
                    displayQuestion: intent.displayQuestion,
                    displayDetail: intent.displayDetail,
                    displayApprove: intent.displayApprove
                )
            }
        )
    }

    /// Rebuild the exact reviewed guard for Core. Content is carried for the
    /// UniFFI record shape; Core only revalidates the guard fields.
    private static func map(questionBatch batch: QuestionBatch) -> WhaleReinMobile.QuestionBatchView {
        WhaleReinMobile.QuestionBatchView(
            sessionId: batch.sessionID,
            batchId: batch.batchID,
            sourceEpoch: batch.sourceEpoch,
            version: batch.version,
            digestHex: batch.digestHex,
            phase: batch.phase,
            questions: batch.questions.map { item in
                WhaleReinMobile.QuestionItemView(
                    id: item.id,
                    question: item.question,
                    detail: item.detail,
                    header: item.header,
                    options: item.options.map {
                        WhaleReinMobile.QuestionOptionView(
                            label: $0.label,
                            description: $0.description
                        )
                    },
                    multiSelect: item.multiSelect,
                    intent: item.intent.map { intent in
                        WhaleReinMobile.QuestionIntentView(
                            kind: intent.kind,
                            approve: intent.approve,
                            reject: intent.reject,
                            always: intent.always,
                            callId: intent.callID,
                            subject: intent.subject.map {
                                WhaleReinMobile.QuestionSubjectView(
                                    name: $0.name,
                                    icon: $0.icon
                                )
                            },
                            displayQuestion: intent.displayQuestion,
                            displayDetail: intent.displayDetail,
                            displayApprove: intent.displayApprove
                        )
                    }
                )
            },
            answerable: batch.answerable,
            contentUnavailable: batch.contentUnavailable
        )
    }

    private static func map(answer: QuestionAnswer) -> WhaleReinMobile.QuestionAnswerView {
        WhaleReinMobile.QuestionAnswerView(
            id: answer.id,
            selected: answer.selected,
            custom: answer.custom
        )
    }

    private static func map(session view: WhaleReinMobile.SessionSummaryView) -> SessionSummary {
        SessionSummary(
            sessionId: view.sessionId,
            title: view.title,
            updatedAtUnixMs: view.updatedAtUnixMs,
            cwd: view.cwd,
            running: view.running
        )
    }

    private static func map(queue view: WhaleReinMobile.PendingQueueView) -> QueueSnapshot {
        QueueSnapshot(
            available: view.available,
            running: view.running,
            items: view.items.map(map(queueItem:))
        )
    }

    private static func map(queueItem view: WhaleReinMobile.PendingQueueItemView) -> QueueItem {
        QueueItem(
            itemID: view.itemId,
            placement: view.placement,
            deliveryID: view.deliveryId,
            blocks: view.blocks.map { block in
                switch block {
                case .text(let text):
                    return .text(text)
                case .attachment(let mediaType, let name, let size, let available):
                    return .attachment(
                        mediaType: mediaType,
                        name: name,
                        size: size,
                        available: available
                    )
                case .unavailable:
                    return .unavailable
                }
            },
            actions: view.actions.compactMap(QueueAction.init(rawValue:))
        )
    }

    private static func map(queueAction action: QueueAction) -> WhaleReinMobile.QueueAction {
        switch action {
        case .edit: return .edit
        case .remove: return .remove
        case .promote: return .promote
        }
    }

    private static func map(wait view: WhaleReinMobile.WaitResultView) -> WaitResult {
        WaitResult(
            waitID: view.waitId,
            hostID: view.hostId,
            sessionID: view.sessionId,
            outcome: map(waitOutcome: view.outcome),
            state: view.state.map { state in
                WaitSnapshot(
                    active: state.active,
                    running: state.running,
                    queueAvailable: state.queueAvailable,
                    pendingInput: state.pendingInput
                )
            },
            errorCode: view.errorCode
        )
    }

    private static func map(waitOutcome outcome: WhaleReinMobile.WaitOutcome) -> WaitOutcome {
        switch outcome {
        case .idle: return .idle
        case .inputRequired: return .inputRequired
        case .timeout: return .timeout
        case .cancelled: return .cancelled
        case .failed: return .failed
        }
    }

    static func map(capabilities view: SessionCapabilitiesView, running: Bool) -> SessionCapabilities {
        let modes = Set(view.deliveryModes.map { $0.lowercased() })
        return SessionCapabilities(
            running: running,
            supportsSteer: modes.contains("steer"),
            supportsQueue: modes.contains("enqueue_after_run") || modes.contains("queue"),
            supportsInterrupt: view.interrupt,
            supportsQueueRead: view.pendingQueueRead,
            supportsQueueEdit: view.pendingQueueEdit,
            supportsQueueRemove: view.pendingQueueRemove,
            supportsQueuePromote: view.pendingQueueSteer,
            supportsHistoryPage: view.historyPage,
            supportsHistoryExact: view.historyExact,
            supportsAttachments: view.attachments,
            supportsReasoning: view.liveReasoning,
            supportsToolActivity: view.toolActivity
        )
    }

    private static func map(approval view: ApprovalView) -> Approval {
        Approval(
            approvalId: view.approvalId,
            sessionId: view.sessionId,
            toolName: view.toolName,
            callId: view.callId,
            reason: view.reason,
            version: Int(view.version),
            expiresAtUnixMs: view.expiresAtUnixMs,
            digestHex: view.digestHex,
            state: map(approvalState: view.state)
        )
    }

    static func map(message view: MessageView) -> ConversationMessage {
        ConversationMessage(
            messageId: view.messageId,
            role: map(role: view.role),
            blocks: view.blocks.map { block in
                ConversationBlock(
                    blockId: block.blockId,
                    kind: map(blockKind: block.kind),
                    text: block.text,
                    toolName: block.toolName,
                    toolActivity: block.toolActivity.map(map(toolActivity:)),
                    attachment: block.attachment.map(map(attachment:))
                )
            },
            createdAtUnixMs: view.createdAtUnixMs
        )
    }

    static func map(command view: CommandView, delivery: DeliveryChoice?) -> CommandReceipt {
        CommandReceipt(
            commandId: view.commandId,
            requestId: view.requestId,
            status: map(commandStatus: view.status),
            delivery: delivery,
            messageId: view.messageId,
            deliveryId: view.deliveryId
        )
    }

    static func map(role: String) -> MessageRole {
        switch role.lowercased() {
        case "user": return .user
        case "assistant": return .assistant
        case "tool": return .tool
        case "notice": return .notice
        default: return .system
        }
    }

    static func map(blockKind: String) -> BlockKind {
        switch blockKind.lowercased() {
        case "text": return .text
        case "reasoning": return .reasoning
        case "tool": return .tool
        case "attachment": return .attachment
        case "question": return .question
        case "notice": return .notice
        default: return .notice
        }
    }

    /// Maps a Source approval state string. `unavailable` and
    /// `resolved_elsewhere` are distinct from `changed`, so a dead request does
    /// not display a misleading "review the latest version" message and cannot
    /// look actionable.
    static func map(approvalState: String) -> ApprovalState {
        switch approvalState.lowercased() {
        case "pending": return .pending
        case "approved", "allowed": return .allowed
        case "rejected": return .rejected
        case "expired": return .expired
        case "changed", "stale": return .changed
        case "unavailable", "session_unavailable", "deleted", "not_found":
            return .unavailable
        case "resolved_elsewhere", "resolved": return .resolvedElsewhere
        default: return .changed
        }
    }

    private static func map(outcome: String) -> AttemptOutcome {
        switch outcome.lowercased() {
        case "committed": return .completed
        case "interrupted": return .cancelled
        default: return .failed
        }
    }

    private func reloadCachedHistory(scope: SessionContext) async {
        let core = self.core
        guard let cached = try? await call({
            try core.cachedMessages(hostId: scope.hostId, sessionId: scope.sessionId)
        }) else { return }
        let mapped = cached.map(Self.map(message:))
        guard mapped.map(\.messageId) != cachedHistory.map(\.messageId) else { return }
        cachedHistory = mapped
        cachedHistoryById = Dictionary(uniqueKeysWithValues: mapped.map { ($0.messageId, $0) })
        broadcast(.reset(messages: mapped, approvals: latestApprovals), scope: scope)
    }

    private static func map(connection status: String) -> ConnectionStatus {
        switch status.lowercased() {
        case "connected": return .online
        case "reconnecting": return .reconnecting
        case "pairing", "connecting", "handshaking", "syncing": return .connecting
        case "degraded": return .degraded
        default: return .offline
        }
    }

    static func map(operationStatus status: CommandStatus) -> OperationStatus {
        switch status {
        case .accepted: return .accepted
        case .confirmed: return .confirmed
        case .uncertain: return .uncertain
        case .rejected, .cancelled: return .failed
        }
    }

    private static func map(commandStatus status: String) -> CommandStatus {
        switch status.lowercased() {
        case "succeeded", "confirmed": return .confirmed
        case "received", "host_accepted", "accepted": return .accepted
        case "rejected": return .rejected
        case "cancelled": return .cancelled
        default: return .uncertain
        }
    }

    private static func map(delivery: DeliveryChoice) -> WhaleReinMobile.DeliveryChoice {
        switch delivery {
        case .send: return .send
        case .steer: return .steer
        case .queue: return .queue
        }
    }

    static func map(error: MobileError) -> AppErrorCode {
        map(code: error.code)
    }

    /// Maps a stable Core/binding error code. Upgrade conditions have their own
    /// non-retryable cases so the app can present an upgrade prompt instead of a
    /// generic retryable failure.
    static func map(code: String) -> AppErrorCode {
        switch code {
        case "invalid_input": return .invalidInvitation
        case "not_paired": return .notPaired
        case "not_found": return .unavailable
        case "revoked": return .revoked
        case "host_offline": return .hostOffline
        case "conflict": return .conflict
        case "timeout": return .timeout
        case "cancelled": return .cancelled
        case "stale_history": return .staleHistory
        case "security": return .security
        case "unavailable": return .unavailable
        case "protocol_version_incompatible": return .protocolVersionIncompatible
        case "unsupported_capability": return .unsupportedCapability
        default: return .internalError
        }
    }

    private static func map(pairingCode: String?) -> AppErrorCode {
        switch pairingCode {
        case "security": return .fingerprintMismatch
        case "cancelled": return .cancelled
        case .some(let code): return map(code: code)
        case .none: return .internalError
        }
    }

    private static func staticMessage(for code: AppErrorCode) -> String {
        L10nKey.errorKey(for: code).string
    }

    // MARK: - Diagnostics mapping

    private static func map(diagnostics view: DiagnosticsReportView) -> DiagnosticsReport {
        let object = (try? JSONSerialization.jsonObject(with: Data(view.json.utf8))) as? [String: Any]
        let hosts = object?["hosts"] as? [[String: Any]] ?? []
        let address = (object?["report"] as? [String: Any])?["address"] as? String
        let lastFailure = view.lastFailureCode
            ?? ((object?["last_failure"] as? [String: Any])?["code"] as? String)
            ?? lastFailureFromEvents(object)
        let generatedAt = generatedDate(from: object)
        let failureStage = failureStage(for: lastFailure)
        var stages: [DiagnosticStage] = []

        for (index, host) in hosts.enumerated() {
            let label = host["label"] as? String ?? "host-\(index + 1)"
            let state = (host["connection_state"] as? String ?? "unknown").lowercased()
            let https: DiagnosticStageStatus
            if failureStage == .https {
                https = .failed
            } else if address?.isEmpty == false {
                https = .ok
            } else {
                https = .idle
            }
            let mqtt: DiagnosticStageStatus
            if failureStage == .mqtt {
                mqtt = .failed
            } else {
                switch state {
                case "connected": mqtt = .ok
                case "failed", "offline", "revoked": mqtt = .failed
                default: mqtt = .idle
                }
            }
            stages.append(
                DiagnosticStage(
                    hostId: label,
                    kind: .https,
                    status: https,
                    correlationId: "https-\(label)",
                    failureCode: failureStage == .https ? lastFailure : nil
                )
            )
            stages.append(
                DiagnosticStage(
                    hostId: label,
                    kind: .mqtt,
                    status: mqtt,
                    correlationId: "mqtt-\(label)",
                    failureCode: failureStage == .mqtt ? lastFailure : nil
                )
            )
        }

        if stages.isEmpty, let lastFailure {
            let kind: DiagnosticStageKind = failureStage == .https ? .https : .mqtt
            stages = [
                DiagnosticStage(
                    hostId: "local",
                    kind: kind,
                    status: .failed,
                    correlationId: "core-local",
                    failureCode: lastFailure
                )
            ]
        }

        return DiagnosticsReport(
            json: view.json,
            redactionFields: view.redactionFields,
            lastFailureCode: lastFailure,
            appVersion: view.appVersion,
            coreVersion: view.coreVersion,
            stages: stages,
            generatedAt: generatedAt
        )
    }

    private static func lastFailureFromEvents(_ object: [String: Any]?) -> String? {
        guard let events = (object?["last_failure"] as? [String: Any])?["events"] as? [[String: Any]] else {
            return nil
        }
        return events.last?["code"] as? String
    }

    private static func generatedDate(from object: [String: Any]?) -> Date {
        if let value = object?["generated_at_unix_ms"] as? NSNumber {
            return Date(timeIntervalSince1970: value.doubleValue / 1000)
        }
        return Date()
    }

    private static func failureStage(for code: String?) -> DiagnosticStageKind? {
        guard let code = code?.lowercased() else { return nil }
        if code.contains("mqtt") || code.contains("broker") || code.contains("subscription") {
            return .mqtt
        }
        if code.contains("https") || code.contains("tls") || code.contains("access") || code.contains("http") {
            return .https
        }
        return nil
    }
}

// MARK: - Pairing outcome

private struct PairingOutcome {
    let hostId: String?
    let code: String?
}
