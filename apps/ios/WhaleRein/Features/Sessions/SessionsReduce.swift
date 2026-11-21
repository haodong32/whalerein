// Pure transitions for the session switcher.

import Foundation

enum SessionsEvent: Equatable, Sendable {
    case hostsLoadStarted
    case hostsLoaded([HostSummary])
    case hostsFailed(AppErrorCode)
    case hostSelected(String)
    case sessionsLoadStarted(hostId: String)
    case sessionsLoaded([SessionSummary], cached: Bool)
    case sessionsFailed(AppErrorCode)
    case reset
}

enum SessionsReduce {
    static func reduce(_ state: SessionsState, _ event: SessionsEvent) -> SessionsState {
        var state = state
        switch event {
        case .hostsLoadStarted:
            state.hosts = .loading

        case .hostsLoaded(let hosts):
            state.hosts = .loaded(hosts)
            if let selected = state.selectedHostId,
               !hosts.contains(where: { $0.hostId == selected }) {
                state.selectedHostId = nil
            }
            if state.selectedHostId == nil {
                state.selectedHostId = hosts.first?.hostId
            }

        case .hostsFailed(let code):
            state.hosts = .failed(code)

        case .hostSelected(let hostId):
            guard state.hostList.contains(where: { $0.hostId == hostId }) else { break }
            state.selectedHostId = hostId

        case .sessionsLoadStarted(let hostId):
            state.hostId = hostId
            state.sessions = .loading
            state.showingCached = false

        case .sessionsLoaded(let sessions, let cached):
            state.sessions = .loaded(sessions)
            state.showingCached = cached

        case .sessionsFailed(let code):
            state.sessions = .failed(code)
            state.showingCached = false

        case .reset:
            state = SessionsState()
        }
        return state
    }
}
