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
    private let coreHandle: CoreHandle
    var core: MobileCore { coreHandle.core }
    let instanceId: String
    let appVersion: String

    private var eventPump: Task<Void, Never>?
    private var continuations: [UUID: AsyncStream<SessionEventEnvelope>.Continuation] = [:]

    private var activeWatch: SessionContext?
    private var activeGeneration: UInt64?
    private var watchRequest: UInt64 = 0
    private var watchTransition: Task<Void, Error>?
    private var sequence = 0
    private var lastPairingOutcome: PairingOutcome?
    private var pairingWaiters: [UUID: CheckedContinuation<PairingOutcome, Error>] = [:]

    var coreRevision: String { WhaleReinMobile.coreRevision() }

    // Core startup and shutdown can wait for default-QoS Rust threads.
    private nonisolated static let coreQueue = DispatchQueue(
        label: "com.whalerein.ios.core-lifecycle",
        qos: .default
    )

    private final class CoreHandle {
        // The owning retain is released on the lifecycle queue because the
        // final UniFFI release can join the Rust runtime thread.
        private let reference: Unmanaged<MobileCore>
        var core: MobileCore { reference.takeUnretainedValue() }

        init(_ core: MobileCore) {
            reference = .passRetained(core)
        }

        deinit {
            let reference = self.reference
            LiveSessionService.coreQueue.async { reference.release() }
        }
    }

    // MARK: - Construction

    /// Opens the materialized config off the main actor, then installs the
    /// service and its event pump on the main actor.
    static func open(
        configPath: URL,
        appVersion: String,
        instanceId: String,
        diagnosticSink: (any CoreDiagnosticSink)? = nil,
        secretStore: @escaping @Sendable () throws -> any SecretStore
    ) async throws -> LiveSessionService {
        do {
            let core: MobileCore = try await withCheckedThrowingContinuation { continuation in
                coreQueue.async {
                    continuation.resume(with: Result {
                        let store = try secretStore()
                        if let diagnosticSink {
                            return try MobileCore.newWithDiagnosticSink(
                                configPath: configPath.path,
                                secretStore: store,
                                appVersion: appVersion,
                                sink: diagnosticSink
                            )
                        }
                        return try MobileCore(
                            configPath: configPath.path,
                            secretStore: store,
                            appVersion: appVersion
                        )
                    })
                }
            }
            return LiveSessionService(core: core, appVersion: appVersion, instanceId: instanceId)
        } catch let error as MobileError {
            throw SessionServiceError(Self.map(error: error))
        }
    }

    private init(core: MobileCore, appVersion: String, instanceId: String) {
        self.coreHandle = CoreHandle(core)
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
                guard self != nil else { return }
                let events = core.pollEvents(timeoutMs: 500)
                if events.isEmpty { continue }
                await self?.ingest(events)
            }
        }
    }

    private func ingest(_ events: [ConversationEventEnvelope]) async {
        // Independent events can be handled even while a watch start is
        // returning the generation of its already-produced first baseline.
        for envelope in events where envelope.scope == nil {
            apply(envelope.event, watchScope: nil)
        }
        while let transition = watchTransition {
            _ = try? await transition.value
        }
        for envelope in events {
            guard let scope = envelope.scope else { continue }
            apply(envelope.event, watchScope: scope)
        }
    }

    private func apply(_ event: ConversationEvent, watchScope: WatchEventScope?) {
        switch event {
        case .pairingFinished(let hostId, let code):
            let outcome = PairingOutcome(hostId: hostId, code: code)
            lastPairingOutcome = outcome
            resumePairingWaiters(with: outcome)
            return
        case .pairingPrompt:
            return
        case .hostConnection(let hostId, let state, let code, let sinceUnixMs):
            // Host-scoped and independent of any watch. Broadcast it before the
            // watch guard so it is delivered with no session open and is never
            // dropped when a watch is replaced.
            broadcastHostConnection(
                hostId: hostId,
                state: state,
                code: code,
                sinceUnixMs: sinceUnixMs
            )
            return
        case .commandStatus(let session, let command):
            broadcast(
                .commandStatus(Self.map(command: command, delivery: nil)),
                scope: SessionContext(hostId: session.hostId, sessionId: session.sessionId)
            )
            return
        default:
            break
        }

        guard let watchScope,
              watchScope.generation == activeGeneration,
              let scope = activeWatch,
              scope.hostId == watchScope.hostId, scope.sessionId == watchScope.sessionId else { return }

        func emit(_ event: MessageEvent) {
            broadcast(event, scope: scope, watchScope: watchScope)
        }

        switch event {
        case .state(_, _, let running, _, let approvals):
            emit(.approvals(approvals.map(Self.map(approval:))))
            emit(.runtimeChanged(running: running))

        case .historySnapshot(let page, let continuity):
            emit(.historySnapshot(Self.map(historyPage: page), replacing: continuity == .replacement))

        case .historyUnavailable(let code):
            emit(.historyUnavailable(Self.map(code: code)))

        case .historyReset:
            break

        case .message(let message, _, _):
            emit(.message(Self.map(message: message)))

        case .preview(let attempt, _, let kind, let text, let replace, let incomplete):
            // Answer and reasoning previews keep distinct typed identities.
            switch kind {
            case "reasoning":
                emit(
                    .reasoningPreview(
                        attemptId: attempt.attemptId,
                        text: text,
                        replace: replace,
                        incomplete: incomplete
                    )
                )
            case "text":
                emit(
                    .preview(attemptId: attempt.attemptId, text: text, replace: replace, incomplete: incomplete)
                )
            default:
                break
            }

        case .toolActivity(let activity):
            emit(
                .toolActivity(Self.map(toolActivity: activity))
            )

        case .attemptEnded(let attempt, let outcome):
            emit(
                .attemptEnded(attemptId: attempt.attemptId, outcome: Self.map(outcome: outcome))
            )

        case .approvalChanged(let approval):
            let mapped = Self.map(approval: approval)
            emit(.approvalChanged(mapped))

        case .questions(let view):
            // Core owns reconciliation and freshness. The app never polls or
            // turns a missing/loading view into an empty collection.
            emit(.questions(Self.map(questions: view)))

        case .error(let code, _):
            emit(
                .error(code: Self.map(code: code), message: Self.staticMessage(for: Self.map(code: code)))
            )

        case .droppedEvents(let count):
            // The bounded Core queue discarded older events. Surface the loss
            // explicitly instead of silently treating the transcript as whole.
            emit(
                .error(
                    code: .internalError,
                    message: "The event stream dropped \(count) events and must be refreshed."
                )
            )

        case .watchEnded:
            emit(.watchEnded(reason: "ended"))

        case .pairingFinished, .pairingPrompt, .hostConnection, .commandStatus:
            break
        }
    }

    private func broadcast(_ event: MessageEvent, scope: SessionContext, watchScope: WatchEventScope? = nil) {
        sequence += 1
        let envelope = SessionEventEnvelope(
            hostId: scope.hostId,
            sessionId: scope.sessionId,
            sequence: sequence,
            event: event,
            watchGeneration: watchScope?.generation,
            observationID: watchScope?.observationId
        )
        for continuation in continuations.values {
            continuation.yield(envelope)
        }
    }

    /// Host-scoped events are not owned by a watch: they are delivered with an
    /// empty session id and survive every watch replacement.
    private func broadcastHostConnection(
        hostId: String,
        state: String,
        code: String?,
        sinceUnixMs: UInt64
    ) {
        sequence += 1
        let envelope = SessionEventEnvelope(
            hostId: hostId,
            sessionId: "",
            sequence: sequence,
            event: .hostConnection(
                hostId: hostId,
                state: state,
                code: code,
                sinceUnixMs: sinceUnixMs
            )
        )
        for continuation in continuations.values {
            continuation.yield(envelope)
        }
    }

    // MARK: - Host connection

    func hostConnection(hostId: String) -> HostConnectionSnapshot? {
        let core = self.core
        guard let view = core.hostConnection(hostId: hostId) else { return nil }
        return HostConnectionSnapshot(
            hostId: view.hostId,
            state: view.state,
            code: view.code,
            sinceUnixMs: view.sinceUnixMs
        )
    }

    func startHostMonitor(hostIds: [String]) {
        let core = self.core
        core.startHostMonitor(hostIds: hostIds)
    }

    func stopHostMonitor() {
        let core = self.core
        core.stopHostMonitor()
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
            displayName: view.displayName,
            osFamily: view.osFamily,
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
            self?.failPairingWaiter(id)
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
        return Self.map(capabilities: view, running: summary.running)
    }

    // MARK: - Watch

    func startWatch(hostId: String, sessionId: String) async throws {
        watchRequest &+= 1
        let request = watchRequest
        let previous = watchTransition
        activeWatch = nil
        activeGeneration = nil
        let core = self.core
        let transition = Task {
            defer { if watchRequest == request { watchTransition = nil } }
            _ = try? await previous?.value
            guard watchRequest == request else { throw CancellationError() }
            let generation = try await call {
                try core.startWatch(
                    hostId: hostId,
                    sessionId: sessionId,
                    baseline: .recent(
                        maxMessages: UInt32(ConversationHistoryLimit.messages),
                        maxBytes: UInt32(ConversationHistoryLimit.bytes)
                    )
                )
            }
            guard watchRequest == request else { throw CancellationError() }
            activeWatch = SessionContext(hostId: hostId, sessionId: sessionId)
            activeGeneration = generation
        }
        watchTransition = transition
        try await transition.value
    }

    func stopWatch() {
        watchRequest &+= 1
        let request = watchRequest
        let previous = watchTransition
        activeWatch = nil
        activeGeneration = nil
        let core = self.core
        watchTransition = Task {
            defer { if watchRequest == request { watchTransition = nil } }
            _ = try? await previous?.value
            try await call { core.stopWatch() }
        }
    }

    func acceptsWatchEvent(_ envelope: SessionEventEnvelope) -> Bool {
        envelope.belongsToActiveWatch(generation: activeGeneration, scope: activeWatch)
    }

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

    func diagnosticsCapture(refresh: Bool) async throws -> DiagnosticsCapture {
        let core = self.core
        // Core owns one immutable capture. `diagnosticsSnapshot` returns the
        // existing bytes; only `refreshDiagnostics` replaces them. The model
        // parses and shares exactly these bytes; there is no second export or
        // Swift-side redaction pass.
        let view = try await call {
            if refresh {
                return try core.refreshDiagnostics()
            }
            return try core.diagnosticsSnapshot()
        }
        return DiagnosticsCapture(
            text: view.text,
            bytes: Data(view.bytes),
            localHostRefs: view.hosts.reduce(into: [:]) { refs, host in
                // Duplicate labels are impossible from Core; last-write-wins
                // keeps a malformed binding from trapping.
                refs[host.label] = host.hostRef
            },
            redactionFields: view.redactionFields,
            appVersion: view.appVersion,
            coreVersion: view.coreVersion
        )
    }

    func clearDiagnostics() async throws {
        try core.clearDiagnostics()
    }

    /// Local-only Host decoration for the captured labels.
    ///
    /// The capture carries the profile-scoped `host_ref` per anonymous label.
    /// Only a currently signed-in managed profile can resolve a confirmed
    /// directory name; a standalone profile, missing entitlement, an offline
    /// directory or an ambiguous/foreign reference stays anonymous. The OS
    /// family is included once the verified directory exposes it; today the
    /// binding has no such field, so only the name is decorated.
    func diagnosticsLocalHosts(for capture: DiagnosticsCapture) async -> [DiagnosticsLocalHost] {
        guard !capture.localHostRefs.isEmpty else { return [] }
        guard let directory = try? await cloudHosts() else { return [] }
        var byID: [String: CloudDirectoryHost] = [:]
        for host in directory {
            byID[host.hostId] = host
        }
        return capture.localHostRefs.compactMap { label, ref in
            guard let host = byID[ref],
                  case .authorized = host.access,
                  !host.displayName.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
                return nil
            }
            return DiagnosticsLocalHost(
                label: label,
                displayName: host.displayName,
                osFamily: nil
            )
        }
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
            running: view.running,
            archived: view.archived
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

    static func map(approval view: ApprovalView) -> Approval {
        Approval(
            approvalId: view.approvalId,
            sessionId: view.sessionId,
            toolName: view.toolName,
            callId: view.callId,
            reason: view.reason,
            version: Int(view.version),
            expiresAtUnixMs: view.expiresAtUnixMs,
            digestHex: view.digestHex,
            state: map(approvalState: view.state),
            // The Host already bounded and masked these facts; carry them
            // through verbatim. An empty fact stays absent instead of being
            // guessed from the tool name or reason, and an unknown
            // availability fails closed as `.unavailable`.
            review: ApprovalReviewDetails(
                action: nilIfEmpty(view.reviewAction),
                target: nilIfEmpty(view.reviewTarget),
                impact: nilIfEmpty(view.reviewImpact),
                availability: ApprovalReviewAvailability(
                    bindingLabel: view.reviewAvailability
                )
            )
        )
    }

    /// A Host-supplied review fact is absent when its bounded value is empty.
    static func nilIfEmpty(_ value: String) -> String? {
        value.isEmpty ? nil : value
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
            createdAtUnixMs: view.createdAtUnixMs,
            deliveryId: view.deliveryId
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
        case "invalid_input": return .invalidInput
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
}

// MARK: - Pairing outcome

private struct PairingOutcome {
    let hostId: String?
    let code: String?
}
