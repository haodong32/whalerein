// Business state for the pairing/invitation screen.
//
// One phase covers editing, validating, the fingerprint prompt, the decision
// and the terminal states. The transient hint and the parse error sit beside it
// because they describe the current input, not the pairing step.

import Foundation

import Foundation

struct PairingState: Equatable, Sendable {
    var phase: PairingPhase = .editing
    var errorCode: AppErrorCode?
    var parseError: InvitationParseError?
    var busy = false
    var hint: PairingHint?

    var prompt: PairingPrompt? {
        switch phase {
        case .awaitingConfirmation(let prompt), .confirming(let prompt): prompt
        default: nil
        }
    }

    var isBusy: Bool { busy }
}
