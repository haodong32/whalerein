// Pure transitions for Settings. The reducer never calls a service; the model
// performs the action and reports the outcome.

import Foundation

enum SettingsEvent: Equatable, Sendable {
    case confirmationRequested(SettingsAction)
    case confirmationCancelled
    case performStarted(SettingsAction)
    case performSucceeded(SettingsAction)
    case performFailed(AppErrorCode)
    case logExportStarted
    case logExportReady(URL)
    case logExportFailed
    case logExportDismissed
}

enum SettingsReduce {
    static func reduce(_ state: SettingsState, _ event: SettingsEvent) -> SettingsState {
        var state = state
        switch event {
        case .confirmationRequested(let action):
            // A second request while a call is running is ignored.
            guard !state.isBusy else { break }
            state.phase = .confirming(action)
            state.errorCode = nil

        case .confirmationCancelled:
            guard case .confirming = state.phase else { break }
            state.phase = .idle

        case .performStarted(let action):
            guard case .confirming(let pending) = state.phase, pending == action else { break }
            state.phase = .performing(action)
            state.statusKey = nil
            state.errorCode = nil

        case .performSucceeded(let action):
            guard case .performing(let running) = state.phase, running == action else { break }
            state.phase = .finished(action)
            state.statusKey = action == .forgetDevice ? .settingsForgotten : .settingsRevoked

        case .performFailed(let code):
            guard case .performing = state.phase else { break }
            state.phase = .idle
            state.errorCode = code

        case .logExportStarted:
            guard state.logExport != .preparing else { break }
            state.logExport = .preparing

        case .logExportReady(let url):
            state.logExport = .ready(url)

        case .logExportFailed:
            state.logExport = .failed

        case .logExportDismissed:
            state.logExport = .idle
        }
        return state
    }
}
