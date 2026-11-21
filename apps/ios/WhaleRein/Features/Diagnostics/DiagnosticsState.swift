// Business state for the diagnostics screen: one load phase and one share
// phase. The report itself is content produced by the service; the state only
// records what the screen is doing with it.

import Foundation

enum DiagnosticsLoad: Equatable, Sendable {
    case idle
    case loading
    case loaded
    case failed(AppErrorCode)
}

enum DiagnosticsShare: Equatable, Sendable {
    case none
    /// The user has been asked to confirm; nothing is written yet.
    case confirming
    case exporting
    case ready(URL)
    case cancelled
    case failed(AppErrorCode)

    var fileURL: URL? {
        if case .ready(let url) = self { return url }
        return nil
    }
}

struct DiagnosticsState: Equatable, Sendable {
    var load: DiagnosticsLoad = .idle
    var share: DiagnosticsShare = .none
    var exportCount = 0

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
