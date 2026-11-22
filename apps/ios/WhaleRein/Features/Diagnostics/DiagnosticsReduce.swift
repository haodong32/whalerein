// Pure transitions for the diagnostics screen.

import Foundation

enum DiagnosticsEvent: Equatable, Sendable {
    case loadStarted
    case loadSucceeded
    case loadFailed(AppErrorCode)
    case shareRequested
    case shareStarted
    case shareExported(URL)
    case shareCancelled
    case shareFailed(AppErrorCode)
    case shareDismissed
}

enum DiagnosticsReduce {
    static func reduce(_ state: DiagnosticsState, _ event: DiagnosticsEvent) -> DiagnosticsState {
        var state = state
        switch event {
        case .loadStarted:
            state.load = .loading

        case .loadSucceeded:
            state.load = .loaded

        case .loadFailed(let code):
            state.load = .failed(code)

        case .shareRequested:
            guard state.isLoaded else { break }
            state.share = .confirming

        case .shareStarted:
            state.share = .exporting

        case .shareExported(let url):
            state.share = .ready(url)
            state.exportCount += 1

        case .shareCancelled:
            // The view dismisses the confirmation dialog before reporting the
            // cancel, so `none` is a valid source as well.
            guard state.share == .confirming || state.share == .none else { break }
            state.share = .cancelled

        case .shareFailed(let code):
            state.share = .failed(code)

        case .shareDismissed:
            state.share = .none
        }
        return state
    }
}
