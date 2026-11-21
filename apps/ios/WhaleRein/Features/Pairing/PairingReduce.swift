// Pure transitions for the pairing screen.

import Foundation

enum PairingEvent: Equatable, Sendable {
    case inputChanged
    case pastedNothing
    case parseFailed(InvitationParseError)
    case linkQueued
    case linkDrained
    case submitStarted
    case submitSucceeded(PairingPrompt)
    case submitFailed(AppErrorCode)
    case confirmStarted
    case approved(hostId: String)
    case declined
    case cancelled
    case failed(AppErrorCode)
    case reset
}

enum PairingReduce {
    static func reduce(_ state: PairingState, _ event: PairingEvent) -> PairingState {
        var state = state
        switch event {
        case .inputChanged:
            state.errorCode = nil
            state.parseError = nil
            state.hint = nil
            if case .failed = state.phase {
                state.phase = .editing
            }

        case .pastedNothing:
            state.hint = .pastedNothing

        case .parseFailed(let error):
            state.parseError = error
            state.phase = .editing

        case .linkQueued:
            state.hint = .linkQueued

        case .linkDrained:
            state.hint = nil

        case .submitStarted:
            guard !state.busy else { break }
            state.busy = true
            state.errorCode = nil
            state.parseError = nil
            state.phase = .validating

        case .submitSucceeded(let prompt):
            state.busy = false
            state.phase = .awaitingConfirmation(prompt)

        case .submitFailed(let code):
            state.busy = false
            state.errorCode = code
            state.phase = .failed(code)

        case .confirmStarted:
            guard case .awaitingConfirmation(let prompt) = state.phase, !state.busy else { break }
            state.busy = true
            state.phase = .confirming(prompt)

        case .approved(let hostId):
            state.busy = false
            state.phase = .paired(hostId: hostId)
            state.hint = nil

        case .declined:
            state.busy = false
            state.phase = .declined
            state.errorCode = nil

        case .cancelled:
            state.busy = false
            state.phase = .editing
            state.errorCode = nil

        case .failed(let code):
            state.busy = false
            state.errorCode = code
            state.phase = .failed(code)

        case .reset:
            state = PairingState()
        }
        return state
    }
}
