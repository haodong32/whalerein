// Supporting value types for the pairing state.

import Foundation

enum PairingPhase: Equatable, Sendable {
    case editing
    case validating
    case awaitingConfirmation(PairingPrompt)
    case confirming(PairingPrompt)
    case paired(hostId: String)
    case declined
    case failed(AppErrorCode)
}

enum PairingHint: Equatable, Sendable {
    case pastedNothing
    case linkQueued
}
