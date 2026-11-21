// Supporting value types for the account state.
import Foundation

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
