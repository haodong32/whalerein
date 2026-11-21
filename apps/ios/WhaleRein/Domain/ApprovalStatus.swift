// Exact approval status refresh state. The Host read is scoped to the original
// session/Host; an answer outside that scope is never applied to the card.

import Foundation

enum ApprovalStatusRefresh: Equatable, Sendable {
    case refreshing
    /// The exact read matches the version and digest already displayed.
    case current
    /// The Host reported a newer version or different digest.
    case changed
    /// The request reached a terminal state.
    case resolved
    /// The exact read failed or the request no longer exists.
    case unavailable
    /// The read resolved to a different session than the card's scope.
    case outOfScope

    var isActionable: Bool { self != .refreshing }
}
