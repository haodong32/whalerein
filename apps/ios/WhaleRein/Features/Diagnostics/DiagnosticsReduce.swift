// Pure transitions for the diagnostics screen. One explicit capture and one
// explicit share; nothing is staged before the single Share tap.

import Foundation

enum DiagnosticsEvent: Equatable, Sendable {
    case loadStarted
    case loadSucceeded
    case loadFailed(AppErrorCode)
    case shareStarted
    case shareStaged(URL)
    case shareFailed(AppErrorCode)
    case shareFinished(completed: Bool)
    case shareDismissed
}

enum DiagnosticsReduce {
    static func reduce(_ state: DiagnosticsState, _ event: DiagnosticsEvent) -> DiagnosticsState {
        var state = state
        switch event {
        case .loadStarted:
            state.load = .loading
            // A new capture invalidates any prepared file and its picker.
            state.share = .none
            state.captureGeneration += 1

        case .loadSucceeded:
            state.load = .loaded

        case .loadFailed(let code):
            state.load = .failed(code)
            state.share = .none

        case .shareStarted:
            guard state.isLoaded, state.share.allowsNewShare else { break }
            state.share = .staging

        case .shareStaged(let url):
            guard state.share == .staging else { break }
            state.share = .presented(url)
            state.shareCount += 1

        case .shareFailed(let code):
            guard state.share == .staging else { break }
            state.share = .failed(code)

        case .shareFinished(let completed):
            guard state.share.isPresented else { break }
            state.share = .finished(completed: completed)

        case .shareDismissed:
            // Only a dismissal of the presented file returns to idle; a status
            // recorded by `shareFinished` survives the sheet teardown.
            guard state.share.isPresented else { break }
            state.share = .none
        }
        return state
    }
}