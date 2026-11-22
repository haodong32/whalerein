// Pure transitions for the account and device sections.

import Foundation

enum AccountEvent: Equatable, Sendable {
    case statusLoadStarted
    case statusLoaded(InstanceStatus)
    case statusFailed(AppErrorCode)
    case defaultHostSelected(String)
    case defaultHostSucceeded
    case defaultHostFailed(AppErrorCode)
    case sessionsLoadStarted
    case sessionsLoaded([AccountAppSession])
    case sessionsFailed(AppErrorCode)
    case revokeStarted(AccountSessionScope)
    case revokeSettled(AccountSessionRevocation)
    case revokeFailed(AccountSessionScope, AppErrorCode)
    case message(L10nKey?)
}

enum AccountReduce {
    static func reduce(_ state: AccountState, _ event: AccountEvent) -> AccountState {
        var state = state
        switch event {
        case .statusLoadStarted:
            if !state.status.isLoaded { state.status = .loading }

        case .statusLoaded(let status):
            state.status = .loaded(status)
            if let defaultHost = status.defaultHost {
                state.defaultHostId = defaultHost
            }

        case .statusFailed(let code):
            state.status = .failed(code)

        case .defaultHostSelected(let hostId):
            state.defaultHostId = hostId
            state.defaultHostError = nil

        case .defaultHostSucceeded:
            state.defaultHostError = nil

        case .defaultHostFailed(let code):
            state.defaultHostError = code

        case .sessionsLoadStarted:
            state.sessions = .loading

        case .sessionsLoaded(let sessions):
            state.sessions = .loaded(sessions)

        case .sessionsFailed(let code):
            state.sessions = .failed(code)

        case .revokeStarted(let scope):
            state.revocation = .revoking(scope)

        case .revokeSettled(let result):
            state.revocation = .settled(result)
            if var sessions = state.sessions.value {
                for index in sessions.indices
                where result.affectedSessionIds.contains(sessions[index].sessionId) {
                    sessions[index].state = result.state
                }
                state.sessions = .loaded(sessions)
            }

        case .revokeFailed(_, let code):
            state.revocation = .failed(code)

        case .message(let key):
            state.statusKey = key
        }
        return state
    }
}
