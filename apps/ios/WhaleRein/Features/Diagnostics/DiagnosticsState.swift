// Business state for the diagnostics screen: one capture phase and one share
// phase. The captured document itself is content owned by the model; the state
// only records what the screen is doing with it.

import Foundation

struct DiagnosticsState: Equatable, Sendable {
    var load: DiagnosticsLoad = .idle
    var share: DiagnosticsShare = .none
    var shareCount = 0
    /// Identity of the capture the share was staged from. A new capture never
    /// reuses a prepared file.
    var captureGeneration = 0

    var isLoaded: Bool {
        if case .loaded = load { return true }
        return false
    }

    var loadError: AppErrorCode? {
        if case .failed(let code) = load { return code }
        return nil
    }

    var shareError: AppErrorCode? {
        if case .failed(let code) = share { return code }
        return nil
    }
}