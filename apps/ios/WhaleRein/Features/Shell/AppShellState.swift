// Business state of the app shell: which root is on screen, which Host is
// selected, and the flags the root renders beside it.
//
// Feature models own their screens; the shell owns only what decides which
// screen exists and whether the privacy shield or offline banner is visible.

import Foundation

struct AppShellState: Equatable, Sendable {
    var isPaired = false
    var rootState: ConversationRootState = .restoring
    var selectedHostId: String?
    var isOfflineCached = false
    var isPrivacyShieldVisible = false
    var isRevoked = false
    /// An explicit local forget/revoke returns to the invitation flow even on a
    /// Cloud instance, so the reset is visible instead of a silent re-login.
    var prefersInvitationAfterDeviceReset = false
    var isSwitchingInstance = false
    var instanceErrorKey: L10nKey?
    var resumeMarker: AppResumeMarker?
    /// False until the first launch restore finishes. A system deep link that
    /// arrives before then is queued instead of being dropped.
    var isLaunched = false
    var pendingInvitationLink: URL?
}
