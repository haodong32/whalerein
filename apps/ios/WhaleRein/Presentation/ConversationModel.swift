// Conversation presentation model. Owns the Host-qualified watch, filters stale
// events, keeps scroll/expansion/draft state stable while output streams, and
// reconciles uncertain sends and stops exactly once.
//
// Lifecycle: the model suspends its watch and flushes the draft when the app
// leaves the foreground, then starts a fresh watch and resyncs the visible
// session when it returns. Approval expiry is server-authoritative: the local
// timer only asks the Host to refresh, so a fast device clock cannot disable a
// still-valid request, while an offline device still shows the request as
// expired and disables decisions.

import Foundation
import Observation

struct ConversationScrollRequest: Equatable, Sendable {
    let id: Int
    let animated: Bool
}

@MainActor
@Observable
final class ConversationModel {
    enum LoadState: Equatable {
        case idle
        case loading
        case loaded
        case failed(AppErrorCode)
    }

    /// The user intent currently being submitted to the service. It is set
    /// synchronously before the awaited call so a second tap in the same
    /// submission window is coalesced instead of issuing another command.
    enum PendingSubmission: Equatable, Sendable {
        case send
        case stop
    }

    let hostId: String
    let sessionId: String
    let sessionTitle: String

    private(set) var messages: [ConversationMessage] = []
    private(set) var approvals: [Approval] = []
    /// Core-owned question facet. Only `current` may establish an empty list.
    private(set) var questions: QuestionsState = .loading
    private(set) var questionReview: QuestionReview?
    private(set) var questionAnswerState: QuestionAnswerState = .idle
    private(set) var questionErrorKey: L10nKey?
    private(set) var isLoadingTruncatedQuestions = false
    private(set) var previewText: String?
    private(set) var previewAttemptId: String?
    private(set) var connection: ConnectionStatus = .offline
    private(set) var loadState: LoadState = .idle
    private(set) var errorKey: L10nKey?
    private(set) var approvalErrorKey: L10nKey?
    private(set) var sendErrorKey: L10nKey?
    private(set) var capabilities: SessionCapabilities = .idle
    private(set) var lastSendStatus: CommandStatus?
    private(set) var lastStopStatus: CommandStatus?
    private(set) var inFlightSend: CommandReceipt?
    private(set) var pendingStop: CommandReceipt?
    /// Command submission that has not settled yet. While set, the matching
    /// affordance is disabled and repeated taps are ignored.
    private(set) var pendingSubmission: PendingSubmission?
    private(set) var stopSettled = false
    private(set) var watchEnded = false
    private(set) var isSuspended = false
    private(set) var isReconnecting = false
    private(set) var resolvingApprovalId: String?
    private(set) var lastAppliedSequence = -1
    private(set) var expandedBlockIds: Set<String> = []
    private(set) var scroll = ScrollAnchorState()
    private(set) var scrollRequest: ConversationScrollRequest?
    /// Timestamp used for expiry display. It advances on a timer and on every
    /// server refresh, not by reading `Date()` in the view body.
    private(set) var approvalClock = Date()
    /// Number of clock-driven approval refresh attempts. Tests use it to prove
    /// expiry is advanced by the timer rather than by a redraw.
    private(set) var approvalRefreshCount = 0
    private(set) var eventSubscriptionCount = 0

    var draft: String = ""
    var delivery: DeliveryChoice = .send

    private struct QuestionDraft {
        let guardValue: QuestionGuard
        var selected: [String: Set<String>] = [:]
        var custom: [String: String] = [:]
    }

    private struct OptimisticMessage {
        let localId: String
        let commandId: String
        let text: String
        let committedMessageId: String?
    }

    private let service: any SessionService
    private let onStatusChange: (() -> Void)?
    private let onFatalError: ((AppErrorCode) -> Void)?
    private let onConnectionChange: ((ConnectionStatus) -> Void)?
    private let now: () -> Date

    private var consumeTask: Task<Void, Never>?
    private var consumeGeneration = 0
    private var draftSaveTask: Task<Void, Never>?
    private var approvalExpiryTask: Task<Void, Never>?
    private var scrollRequestCounter = 0
    private var optimisticMessages: [String: OptimisticMessage] = [:]
    /// Text submitted with the currently uncertain send. A later confirmation
    /// may only consume the draft that was actually submitted, never a newer
    /// draft the user typed while the response was in flight.
    private var inFlightSendText: String?
    /// Monotonic epoch for authoritative running/preview state. A Stop receipt
    /// captured before this epoch advanced must not overwrite a newer run.
    private var authoritativeRunEpoch = 0
    /// Epoch captured when the pending Stop was submitted, used by both the
    /// method return and a late status event.
    private var pendingStopSubmissionEpoch: Int?
    /// Status events for commands whose method response has not arrived yet.
    /// Core can queue `commandStatus` before returning the command view, so the
    /// event may legitimately precede the pending admission.
    private var earlyCommandStatuses: [String: CommandStatus] = [:]
    private var earlyCommandStatusOrder: [String] = []
    /// Approvals the Host has confirmed as still pending even though the local
    /// clock says their TTL passed. This is how a fast device clock stops
    /// disabling a valid request without letting a genuinely expired request
    /// stay clickable while offline.
    private var trustedPendingApprovalIds: Set<String> = []
    /// Per-batch answer edits. A draft is kept only while the batch guard is
    /// unchanged, so a changed owner/phase can never inherit a stale selection.
    private var questionDrafts: [String: QuestionDraft] = [:]

    init(
        hostId: String,
        sessionId: String,
        sessionTitle: String,
        service: any SessionService,
        onStatusChange: (() -> Void)? = nil,
        onFatalError: ((AppErrorCode) -> Void)? = nil,
        onConnectionChange: ((ConnectionStatus) -> Void)? = nil,
        now: @escaping () -> Date = Date.init
    ) {
        self.hostId = hostId
        self.sessionId = sessionId
        self.sessionTitle = sessionTitle
        self.service = service
        self.onStatusChange = onStatusChange
        self.onFatalError = onFatalError
        self.onConnectionChange = onConnectionChange
        self.now = now
        self.approvalClock = now()
    }

    // MARK: - Derived state

    var isRunning: Bool { capabilities.running }

    var deliveryChoices: [DeliveryChoice] { DeliveryPolicy.choices(for: capabilities) }

    var showJumpToLatest: Bool { !scroll.isAtLatest || scroll.unreadCount > 0 }

    var canSend: Bool {
        !draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && inFlightSend == nil
            && pendingSubmission == nil
            && !watchEnded
            && !isReconnecting
    }

    var canStop: Bool {
        capabilities.running
            && pendingStop == nil
            && pendingSubmission == nil
    }

    var questionBatches: [QuestionBatch] { questions.batches }

    var questionsFreshness: QuestionFreshness { questions.freshness }

    var questionEmptyIsCurrent: Bool {
        questions.isCurrent && questions.batches.isEmpty
    }

    var hasPendingQuestions: Bool {
        questions.isCurrent && !questions.batches.isEmpty
    }

    var canInteractWithQuestions: Bool {
        questions.isCurrent && !watchEnded && connection.isOnline
            && questionAnswerState != .submitting
    }

    var pendingApprovals: [Approval] {
        approvals.filter { $0.state == .pending }
    }

    var sendStatusKey: L10nKey? {
        if inFlightSend?.status == .uncertain { return .conversationSendUncertain }
        guard let lastSendStatus else { return nil }
        switch lastSendStatus {
        case .accepted:
            return .conversationSendAccepted
        case .uncertain:
            return .conversationSendUncertain
        case .confirmed:
            return .conversationSendAccepted
        case .rejected:
            return .conversationSendRejected
        case .cancelled:
            return .errorCancelled
        }
    }

    var stopStatusKey: L10nKey? {
        guard let lastStopStatus else { return nil }
        switch lastStopStatus {
        case .uncertain, .accepted:
            return .conversationStopPending
        case .confirmed:
            return .conversationStopConfirmed
        case .rejected:
            return .errorConflict
        case .cancelled:
            return .errorCancelled
        }
    }

    var livePreview: String? {
        guard let previewText, !previewText.isEmpty else { return nil }
        return previewText
    }

    /// Reason a pending card's decision controls are disabled. Offline decisions
    /// are refused with an explicit message instead of failing silently.
    var approvalActionDisabledReasonKey: L10nKey? {
        if watchEnded || !connection.isOnline { return .approvalOffline }
        if resolvingApprovalId != nil { return .approvalBusy }
        return nil
    }

    func isExpanded(_ blockId: String) -> Bool { expandedBlockIds.contains(blockId) }

    func isApprovalExpired(_ approval: Approval) -> Bool {
        if approval.state == .expired { return true }
        guard approval.state == .pending else { return false }
        if trustedPendingApprovalIds.contains(approval.approvalId) { return false }
        return approvalClock.timeIntervalSince1970 * 1000
            >= Double(approval.expiresAtUnixMs)
    }

    func isApprovalActionable(_ approval: Approval) -> Bool {
        approval.state == .pending
            && !isApprovalExpired(approval)
            && resolvingApprovalId == nil
            && connection.isOnline
            && !watchEnded
    }

    // MARK: - Lifecycle

    func start() async {
        guard loadState != .loading, loadState != .loaded else { return }
        loadState = .loading
        errorKey = nil
        watchEnded = false
        lastAppliedSequence = -1
        approvalClock = now()
        questions = .loading
        questionReview = nil
        questionErrorKey = nil
        questionAnswerState = .idle
        ensureSubscription()
        do {
            draft = try await service.loadDraft(hostId: hostId, sessionId: sessionId) ?? ""
            capabilities = try await service.capabilities(hostId: hostId, sessionId: sessionId)
            if capabilities.running {
                delivery = .steer
            }
            try await service.startWatch(hostId: hostId, sessionId: sessionId)
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            loadState = .failed(code)
            errorKey = L10nKey.errorKey(for: code)
            reportFatalIfNeeded(code)
        }
    }

    /// Flushes the pending draft and stops the watch before the app is
    /// backgrounded. The local transcript is kept so a resume can render it
    /// immediately while the fresh watch resets it.
    func suspendForBackground() async {
        guard !isSuspended else { return }
        isSuspended = true
        await persistDraftNow()
        stopApprovalTimer()
        consumeTask?.cancel()
        consumeTask = nil
        service.stopWatch()
        connection = .offline
    }

    /// Restarts the watch and resyncs the visible session after returning to the
    /// foreground, instead of leaving a half-open stream and stale rows.
    func resumeAfterForeground() async {
        guard isSuspended else { return }
        isSuspended = false
        watchEnded = false
        errorKey = nil
        lastAppliedSequence = -1
        approvalClock = now()
        questions = .loading
        questionReview = nil
        questionErrorKey = nil
        questionAnswerState = .idle
        if consumeTask == nil {
            ensureSubscription()
        }
        await restartWatch()
    }

    /// Recovery path for a watch that ended on its own. The composer disables
    /// Send while ended and offers this reconnect action.
    func reconnectWatch() async {
        guard !isReconnecting else { return }
        watchEnded = false
        errorKey = nil
        lastAppliedSequence = -1
        approvalClock = now()
        questions = .loading
        questionReview = nil
        questionErrorKey = nil
        questionAnswerState = .idle
        consumeTask?.cancel()
        consumeTask = nil
        ensureSubscription()
        await restartWatch()
    }

    func close() {
        consumeTask?.cancel()
        consumeTask = nil
        stopApprovalTimer()
        draftSaveTask?.cancel()
        draftSaveTask = nil
        let text = draft
        let hostId = hostId
        let sessionId = sessionId
        let service = service
        // A draft is only 120 ms behind the keyboard. Flush it synchronously
        // enough that switching sessions, leaving the app or killing it does
        // not lose the last edit.
        Task {
            try? await service.saveDraft(hostId: hostId, sessionId: sessionId, text: text)
        }
        service.stopWatch()
    }

    // MARK: - Events

    private func ensureSubscription() {
        guard consumeTask == nil else { return }
        consumeEvents()
    }

    private func consumeEvents() {
        consumeTask?.cancel()
        consumeGeneration += 1
        let generation = consumeGeneration
        eventSubscriptionCount += 1
        let stream = service.makeEventStream()
        consumeTask = Task { [weak self] in
            for await envelope in stream {
                guard let self else { return }
                if Task.isCancelled { return }
                self.handle(envelope)
            }
            guard let self, self.consumeGeneration == generation else { return }
            self.consumeTask = nil
        }
    }

    /// Applies one envelope. Events from another Host/session or an older
    /// sequence are dropped so a late update from a previous watch cannot leak.
    func handle(_ envelope: SessionEventEnvelope) {
        guard envelope.hostId == hostId, envelope.sessionId == sessionId else { return }
        guard envelope.sequence > lastAppliedSequence else { return }
        lastAppliedSequence = envelope.sequence

        switch envelope.event {
        case .reset(let messages, let approvals):
            self.messages = messages
            trustedPendingApprovalIds.removeAll()
            mergeApprovals(approvals, missingPendingState: .resolvedElsewhere)
            optimisticMessages.removeAll()
            previewText = nil
            previewAttemptId = nil
            authoritativeRunEpoch += 1
            connection = .online
            loadState = .loaded
            approvalClock = now()
            scroll.reset()
            onConnectionChange?(connection)
            restartApprovalTimer()

        case .message(let message):
            // A committed message identity is idempotent: a replay must not
            // touch (or consume) any optimistic row.
            guard !messages.contains(where: { $0.messageId == message.messageId }) else { return }
            let echoIndex = message.role == .user ? reconcileUserEcho(message) : nil
            if let echoIndex {
                messages.insert(message, at: min(echoIndex, messages.count))
            } else {
                messages.append(message)
            }
            if message.role == .assistant {
                previewText = nil
                previewAttemptId = nil
            }
            if scroll.recordIncoming(isOwnMessage: false) {
                requestScroll(animated: false)
            }

        case .replace(let message):
            if let index = messages.firstIndex(where: { $0.messageId == message.messageId }) {
                messages[index] = message
            } else if message.role == .user, let echoIndex = reconcileUserEcho(message) {
                messages.insert(message, at: min(echoIndex, messages.count))
            } else {
                messages.append(message)
            }
            if message.role == .assistant {
                previewText = nil
                previewAttemptId = nil
            }
            if scroll.recordIncoming(isOwnMessage: false) {
                requestScroll(animated: false)
            }

        case .preview(let attemptId, let text, _, _):
            previewAttemptId = attemptId
            previewText = text
            if scroll.recordIncoming(isOwnMessage: false) {
                requestScroll(animated: false)
            }

        case .attemptEnded:
            previewText = nil
            previewAttemptId = nil
            authoritativeRunEpoch += 1
            updateCapabilities(running: false)

        case .state(let status):
            updateConnection(status)

        case .connection(let eventHostId, let status):
            guard eventHostId == hostId else { return }
            updateConnection(status)

        case .capabilities(let capabilities):
            // Any capabilities refresh advances the epoch, including an
            // approval-only or same-running update. A Stop submitted just before
            // such an update is therefore treated as stale on its confirmed
            // return, leaving running/preview for one transient frame; the next
            // authoritative idle state self-corrects it. Narrowing this would
            // require an attempt/run identity on the capabilities event.
            authoritativeRunEpoch += 1
            applyCapabilities(capabilities)

        case .approvals(let approvals):
            mergeApprovals(approvals, missingPendingState: .resolvedElsewhere)
            approvalClock = now()
            restartApprovalTimer()

        case .approvalChanged(let approval):
            guard approval.sessionId == sessionId else { return }
            applyServerApproval(approval)
            approvalClock = now()
            restartApprovalTimer()

        case .questions(let state):
            applyQuestions(state)

        case .commandStatus(let receipt):
            applyCommandStatus(receipt)

        case .error(let code, _):
            if code == .revoked {
                onFatalError?(code)
            } else if code == .notPaired {
                onFatalError?(code)
            } else {
                errorKey = .commonError
            }

        case .watchEnded:
            watchEnded = true
            stopApprovalTimer()
            updateConnection(.offline)
        }
    }

    /// User-initiated content load for a Core view that was too large to inline.
    /// This is not a discovery loop: observation still owns freshness and the
    /// stamp; only the oversized content is paged on demand.
    func loadTruncatedQuestions() async {
        guard questions.truncatedForDisplay, !isLoadingTruncatedQuestions else { return }
        isLoadingTruncatedQuestions = true
        defer { isLoadingTruncatedQuestions = false }
        var collected: [QuestionBatch] = []
        var cursor = ""
        do {
            for _ in 0..<64 {
                let page = try await service.listQuestions(
                    hostId: hostId,
                    sessionId: sessionId,
                    cursor: cursor
                )
                collected.append(contentsOf: page.batches)
                cursor = page.nextCursor
                if cursor.isEmpty { break }
            }
            questions.batches = collected
            questions.batchCount = collected.count
            questions.truncatedForDisplay = false
            questions.error = nil
            questionErrorKey = nil
        } catch {
            questionErrorKey = .questionsUnavailable
        }
    }

    private func applyQuestions(_ state: QuestionsState) {
        // Keep edits only for a batch whose exact guard still matches.
        var guards: [String: QuestionGuard] = [:]
        for batch in state.batches {
            guards[batch.batchID] = batch.guardValue
        }
        questionDrafts = questionDrafts.filter { batchId, draft in
            guards[batchId] == draft.guardValue
        }
        var reviewInvalidated = false
        if let review = questionReview, guards[review.guardValue.batchID] != review.guardValue {
            questionReview = nil
            questionErrorKey = .questionsGuardChanged
            reviewInvalidated = true
        }
        questions = state
        if state.freshness == .current && !reviewInvalidated {
            questionErrorKey = nil
        }
    }

    private func updateConnection(_ status: ConnectionStatus) {
        connection = status
        onConnectionChange?(status)
        // Reconnecting can make a locally-expired pending approval valid again
        // (the device clock was fast); recompute the display clock.
        if status.isOnline {
            approvalClock = now()
            restartApprovalTimer()
        }
    }

    // MARK: - Commands

    func send() async {
        let text = draft.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty, inFlightSend == nil, pendingSubmission == nil, !watchEnded else { return }
        // Resolve the delivery mode before claiming the submission: an
        // unsupported mode is rejected locally and must not block the composer.
        let choice: DeliveryChoice
        do {
            choice = try DeliveryPolicy.resolve(delivery, for: capabilities)
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            sendErrorKey = L10nKey.errorKey(for: code)
            reportFatalIfNeeded(code)
            return
        }
        pendingSubmission = .send
        do {
            var receipt = try await service.send(
                hostId: hostId,
                sessionId: sessionId,
                text: text,
                delivery: choice
            )
            // The submission settled: release the gate even though an uncertain
            // receipt continues to block Send until reconciliation.
            pendingSubmission = nil
            if let early = takeEarlyCommandStatus(for: receipt.commandId) {
                receipt.status = preferredStatus(early, over: receipt.status)
            }
            sendErrorKey = nil
            lastSendStatus = receipt.status
            inFlightSend = receipt.status == .uncertain ? receipt : nil
            inFlightSendText = receipt.status == .uncertain ? text : nil
            if receipt.status != .rejected {
                appendOptimisticUserMessage(text: text, receipt: receipt)
            }
            if choice == .send {
                updateCapabilities(running: true)
            }
            if receipt.status == .uncertain {
                // Keep the current draft (which may already be a newer message)
                // until the original request is reconciled.
                await persistDraftNow()
            } else if receipt.status != .rejected {
                // A successful older send consumes only the text it submitted.
                // Text typed while the response was in flight belongs to a
                // newer message and must survive.
                if draftMatchesSubmission(text) {
                    await clearDraftNow()
                } else {
                    await persistDraftNow()
                }
            }
            if receipt.status == .uncertain {
                requestScroll(animated: true)
            }
        } catch {
            // A definite failure releases the gate so the same intent can be
            // retried; a late settlement of the failed command cannot re-arm it.
            pendingSubmission = nil
            let code = (error as? SessionServiceError)?.code ?? .internalError
            sendErrorKey = L10nKey.errorKey(for: code)
            reportFatalIfNeeded(code)
        }
    }

    func stopRun() async {
        guard canStop else { return }
        // Capture the authoritative run this Stop was requested for. If a newer
        // run becomes authoritative before a late confirmation arrives, that
        // confirmation must settle this operation without clearing the new run.
        let submissionEpoch = authoritativeRunEpoch
        pendingSubmission = .stop
        do {
            var receipt = try await service.stop(hostId: hostId, sessionId: sessionId)
            // The submission settled; an accepted or uncertain Stop remains
            // pending through `pendingStop` until the Host reconciles it.
            pendingSubmission = nil
            if let early = takeEarlyCommandStatus(for: receipt.commandId) {
                receipt.status = preferredStatus(early, over: receipt.status)
            }
            sendErrorKey = nil
            applyStopReceipt(receipt, submissionEpoch: submissionEpoch)
        } catch {
            // A definite failure releases the gate so Stop can be retried. A
            // late settlement for the failed command is only remembered as an
            // early status and never resurrects this cleared intent.
            pendingSubmission = nil
            let code = (error as? SessionServiceError)?.code ?? .internalError
            sendErrorKey = L10nKey.errorKey(for: code)
            reportFatalIfNeeded(code)
        }
    }

    // MARK: - Questions

    /// Current edits for one question item. Presentation reads this instead of
    /// owning selection state, so observation updates cannot wipe an edit for an
    /// unchanged batch guard.
    func questionSelection(batchId: String, itemId: String) -> Set<String> {
        questionDrafts[batchId]?.selected[itemId] ?? []
    }

    func questionCustomText(batchId: String, itemId: String) -> String {
        questionDrafts[batchId]?.custom[itemId] ?? ""
    }

    func toggleQuestionOption(batchId: String, itemId: String, label: String) {
        guard let batch = questions.batches.first(where: { $0.batchID == batchId }),
              batch.guardValue.isAnswerablePhase,
              let item = batch.questions.first(where: { $0.id == itemId }),
              item.options.contains(where: { $0.label == label }) else { return }
        var draft = questionDrafts[batchId]
            ?? QuestionDraft(guardValue: batch.guardValue)
        guard draft.guardValue == batch.guardValue else { return }
        var selected = draft.selected[itemId] ?? []
        if item.multiSelect {
            if selected.contains(label) {
                selected.remove(label)
            } else {
                selected.insert(label)
            }
        } else {
            selected = [label]
        }
        draft.selected[itemId] = selected
        questionDrafts[batchId] = draft
        questionErrorKey = nil
    }

    func setQuestionCustomText(batchId: String, itemId: String, text: String) {
        guard let batch = questions.batches.first(where: { $0.batchID == batchId }),
              let item = batch.questions.first(where: { $0.id == itemId }) else { return }
        var draft = questionDrafts[batchId]
            ?? QuestionDraft(guardValue: batch.guardValue)
        guard draft.guardValue == batch.guardValue else { return }
        draft.custom[itemId] = String(text.prefix(4_000))
        questionDrafts[batchId] = draft
        questionErrorKey = nil
    }

    /// Captures the exact reviewed guard and inputs. This is the explicit phase
    /// review: nothing is submitted until the caller confirms this snapshot.
    func beginQuestionReview(batchId: String) -> QuestionReview? {
        guard canInteractWithQuestions,
              let batch = questions.batches.first(where: { $0.batchID == batchId }) else {
            questionErrorKey = .questionsUnavailable
            return nil
        }
        guard batch.canAnswer else {
            questionErrorKey = .questionsUnavailable
            return nil
        }
        let draft = questionDrafts[batchId]
            ?? QuestionDraft(guardValue: batch.guardValue)
        guard draft.guardValue == batch.guardValue else {
            questionErrorKey = .questionsGuardChanged
            return nil
        }
        var answers: [QuestionAnswer] = []
        for item in batch.questions {
            let allowed = Set(item.options.map(\.label))
            let selected = (draft.selected[item.id] ?? []).filter { allowed.contains($0) }
            let custom = (draft.custom[item.id] ?? "")
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if selected.isEmpty && custom.isEmpty { continue }
            answers.append(
                QuestionAnswer(
                    id: item.id,
                    selected: selected.sorted(),
                    custom: custom.isEmpty ? nil : custom
                )
            )
        }
        guard !answers.isEmpty else {
            questionErrorKey = .questionsSelectAnswer
            return nil
        }
        let review = QuestionReview(guardValue: batch.guardValue, answers: answers)
        questionReview = review
        questionErrorKey = nil
        return review
    }

    func cancelQuestionReview() {
        questionReview = nil
    }

    /// Submit the reviewed snapshot. The task is independent of the visible
    /// observation: closing or replacing the watch never cancels it.
    func submitQuestionReview() async {
        guard let review = questionReview else { return }
        guard canInteractWithQuestions,
              let batch = questions.batches.first(where: { $0.batchID == review.guardValue.batchID }),
              batch.guardValue == review.guardValue else {
            questionReview = nil
            questionAnswerState = .failed
            questionErrorKey = .questionsGuardChanged
            return
        }
        questionAnswerState = .submitting
        questionErrorKey = nil
        do {
            let receipt = try await service.answerQuestion(
                hostId: hostId,
                sessionId: sessionId,
                batch: batch,
                answers: review.answers
            )
            switch receipt.status {
            case .uncertain:
                questionAnswerState = .uncertain(commandId: receipt.commandId)
            case .rejected:
                questionAnswerState = .failed
                questionErrorKey = .questionsAnswerRejected
            default:
                questionAnswerState = .submitted(receipt.status)
                questionReview = nil
                questionDrafts[batch.batchID] = nil
            }
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            questionAnswerState = .failed
            questionErrorKey = code == .conflict || code == .staleApproval
                ? .questionsGuardChanged
                : L10nKey.errorKey(for: code)
            reportFatalIfNeeded(code)
        }
    }

    /// Requests cancellation of an uncertain open answer. When `false`, the
    /// answer may still complete; the UI must not promise a rollback.
    func cancelQuestionAnswer() async {
        guard case .uncertain(let commandId) = questionAnswerState else { return }
        do {
            let requested = try await service.cancelQuestionAnswer(
                hostId: hostId,
                sessionId: sessionId,
                commandId: commandId
            )
            questionErrorKey = requested ? .questionsCancelRequested : .questionsCancelUnconfirmed
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            questionErrorKey = L10nKey.errorKey(for: code)
        }
    }

    // MARK: - Approvals

    func resolveApproval(_ approval: Approval, allow: Bool) async {
        guard isApprovalActionable(approval) else {
            if approval.state == .pending, let reason = approvalActionDisabledReasonKey {
                approvalErrorKey = reason
            }
            return
        }
        let review = ApprovalReview(approval: approval)
        resolvingApprovalId = approval.approvalId
        approvalErrorKey = nil
        defer { resolvingApprovalId = nil }
        do {
            let updated = try await service.resolveApproval(
                hostId: hostId,
                sessionId: sessionId,
                approvalId: review.approvalId,
                version: review.version,
                digestHex: review.digestHex,
                allow: allow
            )
            applyServerApproval(updated)
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            approvalErrorKey = code == .staleApproval || code == .conflict
                ? .approvalStale
                : L10nKey.errorKey(for: code)
            await refreshApprovals()
            if code == .revoked || code == .notPaired {
                reportFatalIfNeeded(code)
            }
        }
    }

    /// Refreshes approvals from the Host. Core's approval listing returns only
    /// pending requests, so the result is merged with locally decided terminal
    /// cards rather than replacing them (APPR-04/18). A still-pending approval
    /// missing from the list is marked expired; an unavailable session marks
    /// its pending cards unavailable instead of leaving a dead clickable card
    /// (APPR-19).
    func refreshApprovals(restartTimer: Bool = true) async {
        do {
            let loaded = try await service.approvals(hostId: hostId, sessionId: sessionId)
            approvalClock = now()
            mergeApprovals(loaded, missingPendingState: .expired)
            trustPendingApprovalsReportedByServer(loaded)
            if restartTimer {
                restartApprovalTimer()
            }
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            if code == .unavailable {
                markPendingApprovalsUnavailable()
            }
            if code == .revoked || code == .notPaired {
                reportFatalIfNeeded(code)
            }
            // Other failures keep the last known approvals; the next event or
            // reconnect refreshes them.
        }
    }

    /// Advances the expiry display on demand (foreground, timer, tests).
    func refreshApprovalExpiry() {
        approvalClock = now()
        restartApprovalTimer()
    }

    private func applyServerApproval(_ approval: Approval) {
        guard approval.sessionId == sessionId else { return }
        if let index = approvals.firstIndex(where: { $0.approvalId == approval.approvalId }) {
            if shouldAccept(approval, over: approvals[index]) {
                approvals[index] = approval
            }
        } else {
            approvals.append(approval)
        }
    }

    private func mergeApprovals(
        _ incoming: [Approval],
        missingPendingState: ApprovalState
    ) {
        let scoped = incoming.filter { $0.sessionId == sessionId }
        var incomingById = Dictionary<String, Approval>()
        for approval in scoped {
            incomingById[approval.approvalId] = approval
        }

        var merged = approvals.filter { $0.sessionId == sessionId }
        for index in merged.indices {
            let local = merged[index]
            if let server = incomingById[local.approvalId],
               shouldAccept(server, over: local) {
                merged[index] = server
            }
        }
        let knownIds = Set(merged.map(\.approvalId))
        for approval in scoped where !knownIds.contains(approval.approvalId) {
            merged.append(approval)
        }
        // Trust only survives for requests the Host still reports pending.
        let pendingIds = Set(
            merged.filter { $0.state == .pending }.map(\.approvalId)
        )
        trustedPendingApprovalIds.formIntersection(pendingIds)
        // A pending approval the Host no longer lists was resolved or expired
        // elsewhere. Keep the card with a terminal state so it cannot be
        // clicked and the user still sees what happened.
        for index in merged.indices where merged[index].state == .pending {
            if incomingById[merged[index].approvalId] == nil {
                merged[index].state = missingPendingState
            }
        }
        approvals = merged
    }

    /// A decided local card must never be downgraded back to pending by an
    /// older pending-only refresh. A server terminal state with a newer
    /// version still wins.
    private func shouldAccept(_ server: Approval, over local: Approval) -> Bool {
        guard local.state.isTerminal else { return true }
        if server.state == .pending { return false }
        return server.version >= local.version
    }

    /// The Host's approval listing is the authority for validity. A pending
    /// request it still returns is valid even if the local clock disagrees.
    private func trustPendingApprovalsReportedByServer(_ loaded: [Approval]) {
        let nowMs = approvalClock.timeIntervalSince1970 * 1000
        for approval in loaded
        where approval.sessionId == sessionId
            && approval.state == .pending
            && nowMs >= Double(approval.expiresAtUnixMs) {
            trustedPendingApprovalIds.insert(approval.approvalId)
        }
    }

    private func markPendingApprovalsUnavailable() {
        for index in approvals.indices where approvals[index].state == .pending {
            approvals[index].state = .unavailable
        }
    }

    private func restartWatch() async {
        guard !isReconnecting else { return }
        isReconnecting = true
        defer { isReconnecting = false }
        do {
            let loadedDraft = try await service.loadDraft(hostId: hostId, sessionId: sessionId) ?? ""
            if draft.isEmpty, !loadedDraft.isEmpty {
                draft = loadedDraft
            }
            capabilities = try await service.capabilities(hostId: hostId, sessionId: sessionId)
            try await service.startWatch(hostId: hostId, sessionId: sessionId)
            errorKey = nil
            if loadState != .loaded {
                loadState = .loading
            }
            await refreshApprovals()
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            errorKey = L10nKey.errorKey(for: code)
            if case .failed = loadState {
                // Keep the retry screen that is already visible.
            } else if loadState != .loaded {
                loadState = .failed(code)
            } else {
                watchEnded = true
            }
            reportFatalIfNeeded(code)
        }
    }

    private func reportFatalIfNeeded(_ code: AppErrorCode) {
        guard code == .revoked || code == .notPaired else { return }
        onFatalError?(code)
    }

    // MARK: - UI state

    func toggleExpansion(_ blockId: String) {
        if expandedBlockIds.contains(blockId) {
            expandedBlockIds.remove(blockId)
        } else {
            expandedBlockIds.insert(blockId)
        }
    }

    func selectDelivery(_ choice: DeliveryChoice) {
        guard DeliveryPolicy.isSupported(choice, for: capabilities) else { return }
        delivery = choice
    }

    func draftChanged() {
        draftSaveTask?.cancel()
        let text = draft
        let hostId = hostId
        let sessionId = sessionId
        draftSaveTask = Task { [service] in
            try? await Task.sleep(for: .milliseconds(120))
            if Task.isCancelled { return }
            try? await service.saveDraft(hostId: hostId, sessionId: sessionId, text: text)
        }
    }

    func persistDraftNow() async {
        draftSaveTask?.cancel()
        draftSaveTask = nil
        try? await service.saveDraft(hostId: hostId, sessionId: sessionId, text: draft)
    }

    func updateScroll(isAtLatest: Bool) {
        scroll.update(isAtLatest: isAtLatest)
    }

    func noteTopVisibleMessage(_ messageId: String?) {
        scroll.noteTopVisibleMessage(messageId)
    }

    func jumpToLatest() {
        scroll.jumpToLatest()
        requestScroll(animated: true)
    }

    // MARK: - Internals

    @discardableResult
    private func appendOptimisticUserMessage(text: String, receipt: CommandReceipt) -> String? {
        // The committed echo may have arrived before the send response (Core can
        // deliver the message event first). In that case the authoritative row
        // already exists and a local placeholder would be a duplicate.
        if let committedId = receipt.messageId,
           messages.contains(where: { $0.messageId == committedId }) {
            return nil
        }
        let messageId = "local-\(receipt.commandId)"
        guard !messages.contains(where: { $0.messageId == messageId }) else { return nil }
        optimisticMessages[messageId] = OptimisticMessage(
            localId: messageId,
            commandId: receipt.commandId,
            text: text,
            committedMessageId: receipt.messageId
        )
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
            createdAtUnixMs: Int64(now().timeIntervalSince1970 * 1000)
        )
        messages.append(message)
        scroll.recordIncoming(isOwnMessage: true)
        requestScroll(animated: true)
        return messageId
    }

    /// Removes the optimistic row that stands in for this committed echo and
    /// returns its index, so the committed message can take the same position.
    /// Matching uses the Core message id when the receipt supplied one; only
    /// Sources that never echo a correlation id fall back to exact text. Text
    /// fallback must not merge two distinct authoritative identities.
    private func reconcileUserEcho(_ message: ConversationMessage) -> Int? {
        let echoText = normalizedText(message.plainText)
        let match = messages.firstIndex { local in
            guard let pending = optimisticMessages[local.messageId] else { return false }
            if let committed = pending.committedMessageId {
                return committed == message.messageId
            }
            return normalizedText(pending.text) == echoText
        }
        guard let match else { return nil }
        let local = messages.remove(at: match)
        optimisticMessages.removeValue(forKey: local.messageId)
        return match
    }

    private func normalizedText(_ text: String) -> String {
        text.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private func draftMatchesSubmission(_ submittedText: String) -> Bool {
        draft.trimmingCharacters(in: .whitespacesAndNewlines) == submittedText
    }

    private func applyCommandStatus(_ receipt: CommandReceipt) {
        if let inFlightSend, inFlightSend.commandId == receipt.commandId {
            lastSendStatus = receipt.status
            if receipt.status != .uncertain {
                self.inFlightSend = nil
                if receipt.status == .confirmed || receipt.status == .accepted {
                    let submitted = inFlightSendText
                    if let submitted, draftMatchesSubmission(submitted) {
                        clearDraftAfterSubmission()
                    }
                }
                inFlightSendText = nil
            }
            return
        }
        if let pendingStop, pendingStop.commandId == receipt.commandId {
            let submissionEpoch = pendingStopSubmissionEpoch ?? authoritativeRunEpoch
            applyStopReceipt(receipt, submissionEpoch: submissionEpoch)
            return
        }
        // The method response may still be in flight; remember the early
        // settlement so the return can apply it instead of discarding it.
        rememberEarlyCommandStatus(receipt)
    }

    private func applyStopReceipt(_ receipt: CommandReceipt, submissionEpoch: Int) {
        lastStopStatus = receipt.status
        switch receipt.status {
        case .confirmed:
            pendingStop = nil
            pendingStopSubmissionEpoch = nil
            stopSettled = true
            // Settle the operation, but only let it stop the run it was
            // requested for. A newer authoritative run remains running.
            if submissionEpoch == authoritativeRunEpoch {
                updateCapabilities(running: false)
                previewText = nil
                previewAttemptId = nil
            }
        case .accepted, .uncertain:
            pendingStop = receipt
            pendingStopSubmissionEpoch = submissionEpoch
            stopSettled = false
        case .rejected, .cancelled:
            pendingStop = nil
            pendingStopSubmissionEpoch = nil
            stopSettled = true
        }
    }

    private func preferredStatus(_ early: CommandStatus, over returned: CommandStatus) -> CommandStatus {
        switch returned {
        case .confirmed, .rejected, .cancelled:
            return returned
        case .accepted, .uncertain:
            return early
        }
    }

    private func rememberEarlyCommandStatus(_ receipt: CommandReceipt) {
        if earlyCommandStatuses[receipt.commandId] == nil {
            earlyCommandStatusOrder.append(receipt.commandId)
        }
        earlyCommandStatuses[receipt.commandId] = receipt.status
        while earlyCommandStatusOrder.count > 32 {
            let oldest = earlyCommandStatusOrder.removeFirst()
            earlyCommandStatuses.removeValue(forKey: oldest)
        }
    }

    private func takeEarlyCommandStatus(for commandId: String) -> CommandStatus? {
        earlyCommandStatusOrder.removeAll { $0 == commandId }
        return earlyCommandStatuses.removeValue(forKey: commandId)
    }

    private func clearDraftAfterSubmission() {
        draft = ""
        draftSaveTask?.cancel()
        draftSaveTask = nil
        Task { [service, hostId, sessionId] in
            try? await service.clearDraft(hostId: hostId, sessionId: sessionId)
        }
    }

    func clearDraftNow() async {
        draft = ""
        draftSaveTask?.cancel()
        draftSaveTask = nil
        try? await service.clearDraft(hostId: hostId, sessionId: sessionId)
    }

    private func updateCapabilities(running: Bool) {
        var next = capabilities
        next.running = running
        applyCapabilities(next)
    }

    private func applyCapabilities(_ next: SessionCapabilities) {
        let wasRunning = capabilities.running
        capabilities = next
        let options = DeliveryPolicy.choices(for: capabilities)
        if capabilities.running && !wasRunning {
            delivery = options.first ?? .send
        } else if !capabilities.running {
            delivery = .send
        } else if !options.contains(delivery) {
            delivery = options.first ?? .send
        }
        onStatusChange?()
    }

    // MARK: - Approval expiry clock

    private func restartApprovalTimer(fastVerification: Bool = true) {
        stopApprovalTimer()
        let pending = approvals.filter { $0.state == .pending }
        guard !pending.isEmpty else { return }
        let current = now()
        approvalClock = current
        let nextExpiry = pending.map(\.expiresAt).min() ?? current
        let remaining = nextExpiry.timeIntervalSince(current)
        let allTrusted = pending.allSatisfy { trustedPendingApprovalIds.contains($0.approvalId) }
        // When the local clock already says the request is expired but the Host
        // still reports it pending, verify with the Host once, then recheck at a
        // low frequency instead of spinning. Offline uses the slow path too.
        let delay: Duration
        if remaining > 0 {
            delay = .milliseconds(max(50, Int(remaining * 1000)))
        } else if fastVerification && connection.isOnline && !allTrusted {
            delay = .milliseconds(250)
        } else {
            delay = .seconds(30)
        }
        approvalExpiryTask = Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard let self, !Task.isCancelled else { return }
            self.approvalClock = self.now()
            self.approvalRefreshCount += 1
            await self.refreshApprovals(restartTimer: false)
            self.restartApprovalTimer(fastVerification: false)
        }
    }

    private func stopApprovalTimer() {
        approvalExpiryTask?.cancel()
        approvalExpiryTask = nil
    }

    private func requestScroll(animated: Bool) {
        scrollRequestCounter += 1
        scrollRequest = ConversationScrollRequest(id: scrollRequestCounter, animated: animated)
    }
}
