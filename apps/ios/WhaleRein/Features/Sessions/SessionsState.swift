// Business state for the session switcher: the authorized Hosts and the
// selected Host's sessions. Each list is one `Load`, so a list cannot be
// loading and failed at once, and the cached indicator is part of the same
// value as the sessions it describes.

import Foundation

struct SessionsState: Equatable, Sendable {
    var hosts: Load<[HostSummary]> = .idle
    var selectedHostId: String?
    var sessions: Load<[SessionSummary]> = .idle
    /// The listed sessions came from Core's cache because the Host is offline.
    var showingCached = false
    /// Host the session list currently belongs to.
    var hostId = ""

    var hostList: [HostSummary] { hosts.value ?? [] }
    var sessionList: [SessionSummary] { sessions.value ?? [] }

    var isEmpty: Bool { sessions.isLoaded && sessionList.isEmpty }
}
