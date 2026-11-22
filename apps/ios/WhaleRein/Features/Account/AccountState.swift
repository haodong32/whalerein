// Business state for the account and device settings sections.
//
// Each remote read is one `Load` value, so a section cannot be "loading" and
// "failed" at the same time and no parallel error field is needed.

import Foundation


struct AccountState: Equatable, Sendable {
    var status: Load<InstanceStatus> = .idle
    var defaultHostId: String?
    var defaultHostError: AppErrorCode?
    var sessions: Load<[AccountAppSession]> = .idle
    var revocation: RevocationPhase = .idle
    var statusKey: L10nKey?

    var isRevoking: Bool {
        if case .revoking = revocation { return true }
        return false
    }
}
