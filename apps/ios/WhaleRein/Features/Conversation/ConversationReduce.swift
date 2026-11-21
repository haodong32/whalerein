// Pure state transitions for the conversation. `reduce` never performs IO and
// never reads the clock; every event carries what the transition needs.
//
// The model layer translates user intents and service results into these
// events and applies the result, so state tests need no service and model
// tests cover orchestration only.

import Foundation

enum ConversationAction: Equatable, Sendable {
    // Load
    case loadStarted
    case loadFailed(AppErrorCode)
    case snapshot(messages: [ConversationMessage], approvals: [Approval])
    case watchEnded
    case connectionChanged(ConnectionStatus)
    case capabilitiesLoaded(SessionCapabilities)

    // Content
    case messageReceived(ConversationMessage, provenance: ContentProvenance)
    case messageReplaced(ConversationMessage)

    // Commands
    case sendSubmitted(PendingCommand)
    case sendAccepted(PendingCommand, CommandReceipt)
    case sendDelivered(CommandReceipt)
    case sendUncertain(PendingCommand, CommandReceipt)
    case sendRejected(PendingCommand, AppErrorCode)
    case stopSubmitted(PendingCommand)
    case stopAccepted(PendingCommand, CommandReceipt)
    case stopDelivered(CommandReceipt)
    case stopRejected(PendingCommand, AppErrorCode)

    // Queue
    case queueLoaded(QueueSnapshot)
    case queueUnavailable

    // Wait
    case waitStarted(WaitHandle)
    case waitFinished(WaitOutcome)

    // Approvals
    case approvalPresented(Approval)
    case approvalRefreshed(Approval)
    case approvalExpired(Approval)
    case approvalResolving(Approval)
    case approvalResolved
    case approvalRejected(AppErrorCode)

    // History
    case historyUnavailable
    case historyCachedOnly
    case historyLoaded(complete: Bool)
    case historyLoadingMore
    case historyFailed(AppErrorCode)
}

enum ConversationReduce {
    static func reduce(_ state: ConversationState, _ event: ConversationAction) -> ConversationState {
        var state = state
        switch event {
        case .loadStarted:
            state.loadError = nil
            state.isLoaded = false

        case .loadFailed(let code):
            state.loadError = code
            state.isLoaded = false

        case .snapshot(let messages, let approvals):
            state.messages = messages
            state.isLoaded = true
            state.loadError = nil
            state.provenance = .confirmed
            state.watchEnded = false
            state.connection = .online
            // A pending approval that is no longer reported has been decided
            // elsewhere; the screen must not keep acting on it.
            if let current = state.approval.approval,
               !approvals.contains(where: { $0.id == current.id }) {
                state.approval = .none
            }

        case .watchEnded:
            state.watchEnded = true
            state.connection = .offline

        case .connectionChanged(let status):
            state.connection = status
            if status == .online { state.watchEnded = false }

        case .capabilitiesLoaded(let capabilities):
            state.capabilities = capabilities
            if !capabilities.supportsQueueRead {
                state.queue = .unavailable
            }
            if !capabilities.supportsHistoryPage {
                state.history = .unavailable
            }

        case .messageReceived(let message, let provenance):
            if let index = state.messages.firstIndex(where: { $0.messageId == message.messageId }) {
                state.messages[index] = message
            } else {
                state.messages.append(message)
            }
            state.provenance = provenance

        case .messageReplaced(let message):
            if let index = state.messages.firstIndex(where: { $0.messageId == message.messageId }) {
                state.messages[index] = message
            } else {
                state.messages.append(message)
            }

        case .sendSubmitted(let command), .stopSubmitted(let command):
            // A second submission in the same window is coalesced, not queued.
            guard state.lifecycle.isSettled else { break }
            state.lifecycle = .submitted(command)

        case .sendAccepted(let command, let receipt), .stopAccepted(let command, let receipt):
            guard case .submitted(let current) = state.lifecycle, current.id == command.id else {
                break
            }
            state.lifecycle = .accepted(command, receipt)

        case .sendDelivered(let receipt), .stopDelivered(let receipt):
            state.lifecycle = .delivered(receipt)

        case .sendUncertain(let command, let receipt):
            guard case .submitted(let current) = state.lifecycle, current.id == command.id else {
                break
            }
            state.lifecycle = .uncertain(command, receipt)

        case .sendRejected(let command, let code), .stopRejected(let command, let code):
            guard case .submitted(let current) = state.lifecycle, current.id == command.id else {
                break
            }
            state.lifecycle = .rejected(command, code)

        case .queueLoaded(let snapshot):
            state.queue = .ready(snapshot)

        case .queueUnavailable:
            state.queue = .unavailable

        case .waitStarted(let handle):
            guard case .idle = state.wait else { break }
            state.wait = .active(handle)

        case .waitFinished(let outcome):
            guard case .active = state.wait else { break }
            state.wait = .finished(outcome)

        case .approvalPresented(let approval):
            state.approval = .pending(approval)

        case .approvalRefreshed(let approval):
            // A pending-only refresh must not resurrect a decided card.
            guard case .pending = state.approval else { break }
            state.approval = .pending(approval)

        case .approvalExpired(let approval):
            guard case .pending = state.approval else { break }
            state.approval = .stale(approval)

        case .approvalResolving(let approval):
            guard case .pending = state.approval else { break }
            state.approval = .resolving(approval)

        case .approvalResolved:
            guard case .resolving = state.approval else { break }
            state.approval = .none

        case .approvalRejected:
            guard case .resolving(let approval) = state.approval else { break }
            // A rejected decision returns the card to actionable, not to stale.
            state.approval = .pending(approval)

        case .historyUnavailable:
            state.history = .unavailable

        case .historyCachedOnly:
            state.history = .cachedOnly

        case .historyLoaded(let complete):
            state.history = .idle(complete: complete)

        case .historyLoadingMore:
            guard case .idle = state.history else { break }
            state.history = .loadingMore

        case .historyFailed(let code):
            state.history = .failed(code)
        }
        return state
    }
}
