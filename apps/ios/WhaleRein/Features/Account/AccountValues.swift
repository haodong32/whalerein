// Supporting value types for the account state.
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

/// One scoped revocation at a time. The result keeps the requested scope and
/// the state Cloud reported, so a pending account-wide revoke is never shown as
/// a confirmed single sign-out.
enum RevocationPhase: Equatable, Sendable {
    case idle
    case revoking(AccountSessionScope)
    case settled(AccountSessionRevocation)
    case failed(AppErrorCode)

    var scope: AccountSessionScope? {
        switch self {
        case .revoking(let scope):
            return scope
        case .settled(let result):
            return result.scope
        case .idle, .failed:
            return nil
        }
    }
}
