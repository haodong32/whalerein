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

typealias ConversationHistoryPrepend = HistoryFacet.Prepend

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

    var messages: [ConversationMessage] { state.messages }
    var approvals: [Approval] { state.approvals }
    /// Core-owned question facet. Only `current` may establish an empty list.
    /// Exact approval status refreshes keyed by the original approval identity.
    /// The one business state of this screen. Every dimension below projects
    /// from it; the legacy property names stay read-only for existing views and
    /// are removed once each view is rewritten.
    var state = ConversationState()

    var questions: QuestionsState {
        get { state.question.questions }
        set { state.question.questions = newValue }
    }
    var questionReview: QuestionReview? {
        get { state.question.review }
        set { state.question.review = newValue }
    }
    var questionAnswerState: QuestionAnswerState {
        get { state.question.answer }
        set { state.question.answer = newValue }
    }
    var questionCancelReceipt: QuestionCancelReceipt? {
        get { state.question.cancelReceipt }
        set { state.question.cancelReceipt = newValue }
    }
    var questionErrorKey: L10nKey? {
        get { state.question.errorKey }
        set { state.question.errorKey = newValue }
    }
    var isLoadingTruncatedQuestions: Bool {
        get { state.question.loadingTruncated }
        set { state.question.loadingTruncated = newValue }
    }

    var historyProvenance: HistoryProvenance? {
        get { state.historyFacet.provenance }
        set { state.historyFacet.provenance = newValue }
    }
    var isLoadingOlderHistory: Bool {
        get { state.historyFacet.loading }
        set { state.historyFacet.loading = newValue }
    }
    var historyComplete: Bool {
        get { state.historyFacet.complete }
        set { state.historyFacet.complete = newValue }
    }
    var historyNeedsFreshTraversal: Bool {
        get { state.historyFacet.needsFreshTraversal }
        set { state.historyFacet.needsFreshTraversal = newValue }
    }
    var historyErrorKey: L10nKey? {
        get { state.historyFacet.errorKey }
        set { state.historyFacet.errorKey = newValue }
    }
    var historyPrepend: HistoryFacet.Prepend? {
        get { state.historyFacet.prepend }
        set { state.historyFacet.prepend = newValue }
    }

    var previewText: String? {
        get { state.previewFacet.text }
        set { state.previewFacet.text = newValue }
    }
    var previewAttemptId: String? {
        get { state.previewFacet.attemptId }
        set { state.previewFacet.attemptId = newValue }
    }
    var reasoningPreviewText: String? {
        get { state.previewFacet.reasoning }
        set { state.previewFacet.reasoning = newValue }
    }
    var liveToolActivities: [ToolActivity] {
        get { state.previewFacet.toolActivities }
        set { state.previewFacet.toolActivities = newValue }
    }

    var approvalStatusRefresh: [String: ApprovalStatusRefresh] {
        get { state.approvalFacet.statusRefresh }
        set { state.approvalFacet.statusRefresh = newValue }
    }
    var approvalClock: Date {
        get { state.approvalFacet.clock }
        set { state.approvalFacet.clock = newValue }
    }
    var approvalRefreshCount: Int {
        get { state.approvalFacet.refreshCount }
        set { state.approvalFacet.refreshCount = newValue }
    }
    var trustedPendingApprovalIds: Set<String> {
        get { state.approvalFacet.trustedPendingIds }
        set { state.approvalFacet.trustedPendingIds = newValue }
    }
    var lastSendOperationId: String? {
        get { state.operation.lastSendOperationId }
        set { state.operation.lastSendOperationId = newValue }
    }
    var operationResult: OperationReceipt? {
        get { state.operation.result }
        set { state.operation.result = newValue }
    }
    var operationErrorKey: L10nKey? {
        get { state.operation.errorKey }
        set { state.operation.errorKey = newValue }
    }
    var operationReadErrorKey: L10nKey? {
        get { state.operation.readErrorKey }
        set { state.operation.readErrorKey = newValue }
    }
    var isRecoveringOperation: Bool {
        get { state.operation.recovering }
        set { state.operation.recovering = newValue }
    }
    var isReadingExactReply: Bool {
        get { state.operation.readingExactReply }
        set { state.operation.readingExactReply = newValue }
    }
    var watchEnded: Bool {
        get { state.watch.ended }
        set { state.watch.ended = newValue }
    }
    var isSuspended: Bool {
        get { state.watch.suspended }
        set { state.watch.suspended = newValue }
    }
    var isReconnecting: Bool {
        get { state.watch.reconnecting }
        set { state.watch.reconnecting = newValue }
    }

    var connection: ConnectionStatus { state.connection }
    var loadState: LoadState {
        if let error = state.loadError { return .failed(error) }
        if state.isLoaded { return .loaded }
        return state.isLoading ? .loading : .idle
    }
    var errorKey: L10nKey?
    var approvalErrorKey: L10nKey?
    var capabilities: SessionCapabilities { state.capabilities }
    var sendErrorKey: L10nKey? {
        guard let code = state.commandError else { return nil }
        return L10nKey.errorKey(for: code)
    }
    var lastSendStatus: CommandStatus? {
        guard state.lastCommandKind?.isStop == false else { return nil }
        return state.lastReceipt?.status
    }
    var lastStopStatus: CommandStatus?
    /// Original operation identity and the message-level recovery result for
    /// the last send. Submission status stays separate from these values.
    var inFlightSend: CommandReceipt? {
        if case .uncertain(_, let receipt) = state.lifecycle { return receipt }
        return nil
    }
    var pendingStop: CommandReceipt?
    /// Command submission that has not settled yet. While set, the matching
    /// affordance is disabled and repeated taps are ignored.
    var pendingSubmission: PendingSubmission? {
        guard case .submitted(let command) = state.lifecycle else { return nil }
        return command.kind.isStop ? .stop : .send
    }
    var stopSettled = false
    var resolvingApprovalId: String?
    var lastAppliedSequence = -1
    var expandedBlockIds: Set<String> = []
    var scroll = ScrollAnchorState()
    var scrollRequest: ConversationScrollRequest?
    /// Timestamp used for expiry display. It advances on a timer and on every
    /// server refresh, not by reading `Date()` in the view body.
    /// Number of clock-driven approval refresh attempts. Tests use it to prove
    /// expiry is advanced by the timer rather than by a redraw.
    var eventSubscriptionCount = 0

    // Older committed history is an independent one-shot read; it never
    // replaces the single live watch.
    /// Verified attachment transfers for committed attachment blocks.
    let attachments: AttachmentTransferModel

    var historyContinuation: String?
    var historyProjection: String?
    var historySource: HistorySource?
    var historyGeneration = 0
    var isClosed = false
    var pagedMessageIds: Set<String> = []
    var liveMessageIds: Set<String> = []
    let historyPageMessages = 50
    let historyPageBytes = 256 * 1024

    var draft: String = ""
    var delivery: DeliveryChoice = .queue

    struct QuestionDraft {
        let guardValue: QuestionGuard
        var selected: [String: Set<String>] = [:]
        var custom: [String: String] = [:]
    }

    struct OptimisticMessage {
        let localId: String
        let commandId: String
        let text: String
        let committedMessageId: String?
    }

    let service: any SessionService
    /// Queue and wait are independent observations; neither owns the watch.
    let queue: ConversationQueueController
    let wait: ConversationWaitController
    let onStatusChange: (() -> Void)?
    let onFatalError: ((AppErrorCode) -> Void)?
    let onConnectionChange: ((ConnectionStatus) -> Void)?
    let onReconnected: (() -> Void)?
    let now: () -> Date

    var consumeTask: Task<Void, Never>?
    var consumeGeneration = 0
    var draftSaveTask: Task<Void, Never>?
    var approvalExpiryTask: Task<Void, Never>?
    var scrollRequestCounter = 0
    var optimisticMessages: [String: OptimisticMessage] = [:]
    /// Text submitted with the currently uncertain send. A later confirmation
    /// may only consume the draft that was actually submitted, never a newer
    /// draft the user typed while the response was in flight.
    var inFlightSendText: String?
    /// Monotonic epoch for authoritative running/preview state. A Stop receipt
    /// captured before this epoch advanced must not overwrite a newer run.
    var authoritativeRunEpoch = 0
    /// Epoch captured when the pending Stop was submitted, used by both the
    /// method return and a late status event.
    var pendingStopSubmissionEpoch: Int?
    /// Status events for commands whose method response has not arrived yet.
    /// Core can queue `commandStatus` before returning the command view, so the
    /// event may legitimately precede the pending admission.
    var earlyCommandStatuses: [String: CommandStatus] = [:]
    var earlyCommandStatusOrder: [String] = []
    /// Approvals the Host has confirmed as still pending even though the local
    /// clock says their TTL passed. This is how a fast device clock stops
    /// disabling a valid request without letting a genuinely expired request
    /// stay clickable while offline.
    /// Per-batch answer edits. A draft is kept only while the batch guard is
    /// unchanged, so a changed owner/phase can never inherit a stale selection.
    var questionDrafts: [String: QuestionDraft] = [:]

    init(
        hostId: String,
        sessionId: String,
        sessionTitle: String,
        service: any SessionService,
        onStatusChange: (() -> Void)? = nil,
        onFatalError: ((AppErrorCode) -> Void)? = nil,
        onConnectionChange: ((ConnectionStatus) -> Void)? = nil,
        onReconnected: (() -> Void)? = nil,
        now: @escaping () -> Date = Date.init
    ) {
        self.hostId = hostId
        self.sessionId = sessionId
        self.sessionTitle = sessionTitle
        self.service = service
        self.queue = ConversationQueueController(
            hostId: hostId,
            sessionId: sessionId,
            service: service
        )
        self.wait = ConversationWaitController(
            hostID: hostId,
            sessionID: sessionId,
            service: service
        )
        self.onStatusChange = onStatusChange
        self.onFatalError = onFatalError
        self.onConnectionChange = onConnectionChange
        self.onReconnected = onReconnected
        self.now = now
        self.attachments = AttachmentTransferModel(
            service: service as? any AttachmentTransferService
        )
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

    /// Message-level result for the original send. Submission receipt
    /// (`sendStatusKey`) stays separate from confirmation and content reads.
    var operationStatusKey: L10nKey? {
        guard let operationResult else { return nil }
        switch operationResult.status {
        case .submitted: return .conversationOperationSubmitted
        case .accepted: return .conversationOperationAccepted
        case .uncertain: return .conversationOperationUncertain
        case .failed: return .conversationOperationFailed
        case .confirmed: return .conversationOperationConfirmed
        }
    }

    var operationInputRequired: Bool { operationResult?.inputKinds != nil }

    var canRecoverSend: Bool {
        lastSendOperationId != nil && !isRecoveringOperation && !watchEnded
    }

    var canReadExactReply: Bool {
        operationResult?.canReadExactReply == true && !isReadingExactReply
    }

    var operationReferenceText: String? { operationResult?.confirmedReference?.messageId }

    var operationDeliveryIdentity: String? {
        operationResult?.receipt?.deliveryId ?? operationResult?.receipt?.messageId
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
            && approvalStatusRefresh[approval.approvalId] != .refreshing
    }
}
