// State-only native wait and open-answer cancellation receipts.
//
// A wait is an observation. Cancelling one releases only that attempt: the
// conversation watch, its event stream and the session itself keep running.

import Foundation

enum WaitOutcome: String, Equatable, Sendable {
    case idle
    case inputRequired
    case timeout
    case cancelled
    case failed
}

/// Typed source state reported with a settled wait.
struct WaitSnapshot: Hashable, Sendable {
    let active: Bool
    let running: Bool
    let queueAvailable: Bool
    /// `approval`, `question` or `other`, in the order Core reported.
    let pendingInput: [String]
}

struct WaitResult: Hashable, Sendable {
    let waitID: String
    let hostID: String
    let sessionID: String
    let outcome: WaitOutcome
    let state: WaitSnapshot?
    /// Stable binding/Core error code for a failed wait.
    let errorCode: String?
}

/// Receipt for a guarded open-answer cancellation request. A `false` request
/// never proves that no native answer occurred; `commandID` stays the identity
/// used for recovery.
struct QuestionCancelReceipt: Hashable, Sendable {
    let commandID: String
    let cancellationRequested: Bool
}
