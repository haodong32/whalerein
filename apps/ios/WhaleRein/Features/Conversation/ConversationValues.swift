// Supporting value types for the conversation state.
//
// They live in their own file so the state file holds one primary type.

import Foundation

/// A command the user submitted and is waiting for Core to settle.
struct PendingCommand: Equatable, Sendable {
    enum Kind: Equatable, Sendable {
        case send(text: String)
        case stop
    }

    let id: String
    let kind: Kind
    let submittedAt: Date
}

extension PendingCommand.Kind {
    var isStop: Bool {
        if case .stop = self { return true }
        return false
    }
}

/// Identity of a native wait attempt.
struct WaitHandle: Equatable, Sendable {
    let id: String
    let timeoutMs: UInt64
}

/// The command lifecycle for the conversation, exactly one value at a time.
enum CommandLifecycle: Equatable, Sendable {
    case idle
    case submitted(PendingCommand)
    case accepted(PendingCommand, CommandReceipt)
    case delivered(CommandReceipt)
    case uncertain(PendingCommand, CommandReceipt)
    case rejected(PendingCommand, AppErrorCode)

    var pending: PendingCommand? {
        switch self {
        case .submitted(let command), .accepted(let command, _), .uncertain(let command, _),
             .rejected(let command, _):
            return command
        case .idle, .delivered:
            return nil
        }
    }

    var receipt: CommandReceipt? {
        switch self {
        case .accepted(_, let receipt), .delivered(let receipt), .uncertain(_, let receipt):
            return receipt
        case .idle, .submitted, .rejected:
            return nil
        }
    }

    var isSettled: Bool {
        switch self {
        case .idle, .delivered, .rejected: true
        case .submitted, .accepted, .uncertain: false
        }
    }
}

/// Pending queue availability. Reading and mutating are separate capabilities
/// on the service, so "unsupported" is a state, not an empty list.
enum QueueState: Equatable, Sendable {
    case unavailable
    case ready(QueueSnapshot)
}

/// Native wait lifecycle. A finished wait keeps its outcome so the UI can show
/// why it ended; it never revives itself.
enum WaitState: Equatable, Sendable {
    case idle
    case active(WaitHandle)
    case finished(WaitOutcome)
}

/// The one approval the screen currently acts on.
enum ApprovalSlot: Equatable, Sendable {
    case none
    case pending(Approval)
    case stale(Approval)
    case resolving(Approval)

    var approval: Approval? {
        switch self {
        case .none: nil
        case .pending(let approval), .stale(let approval), .resolving(let approval): approval
        }
    }

    var isActionable: Bool {
        if case .pending = self { return true }
        return false
    }
}

/// Question facet of the conversation: Core-owned batches, the user's review
/// draft, the answer submission and the truncated-page indicator.
struct QuestionFacet: Equatable, Sendable {
    var questions: QuestionsState = .loading
    /// The review the user is currently editing.
    var review: QuestionReview?
    /// Reviews already submitted for a batch but not yet removed by the Host.
    var settledReviews: [String: QuestionReview] = [:]
    /// Terminal answer state per settled batch, so several cards can stay
    /// truthful while the observation catches up.
    var settledAnswers: [String: QuestionAnswerState] = [:]
    var answeredBatchId: String?
    var answer: QuestionAnswerState = .idle
    var cancelReceipt: QuestionCancelReceipt?
    var errorKey: L10nKey?
    var loadingTruncated = false
}

/// Older-history facet: one one-shot traversal beside the live watch.
struct HistoryFacet: Equatable, Sendable {
    var source: HistorySource?
    var projection: String?
    var continuation: String?
    var baselineContinuation: String?
    var traversal = 0
    var didAutoFill = false
    var didAutomaticallyRebase = false
    var canScroll = true
    var positionChanged = false
    var awaitsOnlineReplacement = false
    var provenance: HistoryProvenance?
    var loading = false
    var complete = false
    var needsFreshTraversal = false
    var errorKey: L10nKey?
}

/// Streaming preview of the reply currently being generated, plus the typed
/// activity rows that will be replaced by committed content.
struct PreviewFacet: Equatable, Sendable {
    var text: String?
    var attemptId: String?
    var reasoning: String?
    var toolActivities: [ToolActivity] = []
}

/// Approval refresh bookkeeping: the exact-status refreshes, the local clock
/// used for expiry display and the refresh counter.
struct ApprovalFacet: Equatable, Sendable {
    var statusRefresh: [String: ApprovalStatusRefresh] = [:]
    var clock = Date(timeIntervalSince1970: 0)
    var refreshCount = 0
    var trustedPendingIds: Set<String> = []
}

/// The retained send operation and its exact-content read.
struct OperationFacet: Equatable, Sendable {
    var lastSendOperationId: String?
    var result: OperationReceipt?
    var errorKey: L10nKey?
    var readErrorKey: L10nKey?
    var recovering = false
    var readingExactReply = false
}

/// Watch lifecycle beside the connection status.
struct WatchFacet: Equatable, Sendable {
    var ended = false
    var suspended = false
    var reconnecting = false
    /// Visible rendering and clock-driven updates are paused while the scene is
    /// inactive or shielded. The authorized observation and its cursor stay
    /// live, so returning to active needs no resubscription.
    var paused = false
}
