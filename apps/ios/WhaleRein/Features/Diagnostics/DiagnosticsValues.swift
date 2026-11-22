// Supporting value types for the diagnostics state.
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
