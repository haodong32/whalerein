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
    /// The protected temporary copy is being written. No picker is open.
    case staging
    /// Exactly one system share sheet is presenting this file.
    case presented(URL)
    /// The sheet ended; `completed` distinguishes a destination from a cancel.
    case finished(completed: Bool)
    case failed(AppErrorCode)

    var fileURL: URL? {
        if case .presented(let url) = self { return url }
        return nil
    }

    /// A second tap while staging or presenting must not open a second sheet.
    var allowsNewShare: Bool {
        switch self {
        case .none, .finished, .failed: return true
        case .staging, .presented: return false
        }
    }

    var isPresented: Bool {
        if case .presented = self { return true }
        return false
    }
}