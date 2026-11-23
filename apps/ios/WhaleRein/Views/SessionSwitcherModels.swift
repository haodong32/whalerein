// The switcher's inputs: presentation models and the shell's intents. Kept
// apart from the views so both the sheet and the sidebar stay thin.

import Foundation

struct SessionSwitcherModels {
    let hosts: HostListModel
    /// Authoritative Host status shared with the conversation chrome.
    let connectivity: HostConnectivityModel
    let sessions: SessionListModel
    let management: SessionManagementModel
    /// The conversation currently on screen, if any.
    let currentConversation: SessionContext?
}

struct SessionSwitcherActions {
    var refresh: () async -> Void
    var selectHost: (String) async -> Void
    var selectSession: (SessionSummary) -> Void
    var openConversation: (String, SessionSummary) -> Void
    var addHost: () -> Void
    var close: () -> Void
}
