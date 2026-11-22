// One remote read: idle, loading, loaded or failed with a typed error.
//
// Used by every feature that loads remote state, so a section cannot be
// "loading" and "failed" at the same time and no parallel error field exists.

import Foundation

enum Load<Value: Equatable & Sendable>: Equatable, Sendable {
    case idle
    case loading
    case loaded(Value)
    case failed(AppErrorCode)

    var value: Value? {
        if case .loaded(let value) = self { return value }
        return nil
    }

    var isLoading: Bool {
        if case .loading = self { return true }
        return false
    }

    var error: AppErrorCode? {
        if case .failed(let code) = self { return code }
        return nil
    }

    var isLoaded: Bool {
        if case .loaded = self { return true }
        return false
    }
}
