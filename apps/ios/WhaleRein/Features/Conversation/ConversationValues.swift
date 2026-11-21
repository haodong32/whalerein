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

/// Committed-history traversal state.
enum HistoryState: Equatable, Sendable {
    case unavailable
    case cachedOnly
    case idle(complete: Bool)
    case loadingMore
    case failed(AppErrorCode)
}

/// Where the visible content came from. The UI labels cached and optimistic
/// content; Core stays authoritative.
enum ContentProvenance: Equatable, Sendable {
    case optimistic
    case confirmed
    case cached
}
