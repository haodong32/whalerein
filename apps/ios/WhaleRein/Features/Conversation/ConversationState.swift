// Business state for one conversation screen.
//
// One enum per dimension means an illegal combination cannot be constructed:
// there is no `isSending` flag that can disagree with a receipt, no parallel
// error field per operation, and no "idle but has pending approval" state.
//
// The value is `Equatable` and contains no SwiftUI or service types, so every
// transition is testable without a UI or a running Core.

import Foundation

/// The one state value of the conversation screen. Supporting dimensions live
/// in `ConversationValues.swift`.
struct ConversationState: Equatable, Sendable {
    var capabilities: SessionCapabilities = .idle
    var lifecycle: CommandLifecycle = .idle
    var queue: QueueState = .unavailable
    var wait: WaitState = .idle
    var approval: ApprovalSlot = .none
    var messages: [ConversationMessage] = []
    var approvals: [Approval] = []
    var question = QuestionFacet()
    var historyFacet = HistoryFacet()
    var viewport = ScrollAnchorState()
    var previewFacet = PreviewFacet()
    var approvalFacet = ApprovalFacet()
    var operation = OperationFacet()
    /// Terminal receipt of the most recent command, for the status row.
    var lastReceipt: CommandReceipt?
    /// Which command the last receipt belongs to (send or stop).
    var lastCommandKind: PendingCommand.Kind?
    /// A definite local/service failure. A settlement receipt is not an error.
    var commandError: AppErrorCode?
    var loadError: AppErrorCode?
    var isLoading = false
    var isLoaded = false
    var watch = WatchFacet()

    var canSend: Bool {
        guard isLoaded else { return false }
        guard !capabilities.running || capabilities.supportsSteer else { return false }
        return lifecycle.isSettled
    }

    var canStop: Bool {
        guard isLoaded else { return false }
        return capabilities.running && capabilities.supportsInterrupt
    }

    /// Approval actions are disabled while another decision is in flight.
    var canDecideApproval: Bool { approval.isActionable }
}
