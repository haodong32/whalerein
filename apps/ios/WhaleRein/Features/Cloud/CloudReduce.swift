// Pure transitions for Cloud sign-in and the computer directory.

import Foundation

enum CloudEvent: Equatable, Sendable {
    case restoreStarted
    case restored(account: CloudAccount?, logoutPending: Bool)
    case restoreFailed(AppErrorCode)

    case signInStarted
    case authorizing
    case authSheetPresented
    case authSheetDismissed
    case signInCancelled
    case signInFailed(AppErrorCode)

    case completionStarted
    case completed(CloudAccount)
    case completionFailed(AppErrorCode)

    case directoryStarted
    case directoryLoaded([CloudDirectoryHost])
    case directoryFailed(AppErrorCode)

    case connectStarted(String)
    case connectFinished
    case connectFailed(AppErrorCode)

    case sessionVerified(CloudAccount)
    case sessionRevoked
    case signedOut(pending: Bool)
    case signedOutLocally(pending: Bool)
    case errorRaised(AppErrorCode)
}

enum CloudReduce {
    static func reduce(_ state: CloudState, _ event: CloudEvent) -> CloudState {
        var state = state
        switch event {
        case .restoreStarted:
            guard state.phase == .signedOut else { break }
            state.phase = .restoring

        case .restored(let account, let pending):
            state.logoutPending = pending
            state.phase = account.map(CloudSignInPhase.signedIn) ?? .signedOut
            if account == nil { state.directory = .idle }

        case .restoreFailed(let code):
            state.phase = .signedOut
            state.directory = .idle
            state.errorCode = code

        case .signInStarted:
            state.errorCode = nil
            state.wasCancelled = false
            state.phase = .starting

        case .authorizing:
            state.phase = .authorizing
            state.authPresented = true

        case .authSheetPresented:
            state.authPresented = true

        case .authSheetDismissed:
            state.authPresented = false

        case .signInCancelled:
            state.phase = .signedOut
            state.authPresented = false
            state.wasCancelled = true
            state.errorCode = nil

        case .signInFailed(let code):
            state.phase = .signedOut
            state.authPresented = false
            state.errorCode = code

        case .completionStarted:
            state.phase = .completing

        case .completed(let account):
            state.phase = .signedIn(account)
            state.logoutPending = false
            state.errorCode = nil

        case .completionFailed(let code):
            state.phase = .signedOut
            state.directory = .idle
            state.errorCode = code

        case .directoryStarted:
            guard state.phase.isSignedIn else { break }
            state.directory = .loading

        case .directoryLoaded(let hosts):
            state.directory = hosts.isEmpty ? .empty : .loaded(hosts)

        case .directoryFailed(let code):
            state.directory = .unavailable(code)

        case .connectStarted(let hostId):
            guard state.phase.isSignedIn, state.connectingHostId == nil else { break }
            state.connectingHostId = hostId
            state.errorCode = nil

        case .connectFinished:
            state.connectingHostId = nil

        case .connectFailed(let code):
            state.connectingHostId = nil
            state.errorCode = code

        case .sessionVerified(let account):
            guard state.phase.isSignedIn else { break }
            state.phase = .signedIn(account)

        case .sessionRevoked:
            state.logoutPending = true
            state.phase = .signedOut
            state.directory = .idle
            state.errorCode = .revoked

        case .errorRaised(let code):
            state.errorCode = code

        case .signedOut(let pending), .signedOutLocally(let pending):
            state.logoutPending = pending
            state.phase = .signedOut
            state.directory = .idle
            state.errorCode = nil
        }
        return state
    }
}
