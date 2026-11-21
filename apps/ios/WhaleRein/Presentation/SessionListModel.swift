// Session list presentation state: Host-scoped, sorted by recent activity, with
// a last-session continuation affordance and a cached/offline indicator.
// Concurrent load requests coalesce to the latest requested Host instead of the
// older in-flight request winning; cached state clears as soon as a live load
// succeeds.

import Foundation
import Observation

@MainActor
@Observable
final class SessionListModel {
    enum LoadState: Equatable {
        case idle
        case loading
        case loaded
        case failed(AppErrorCode)
    }

    /// One business state for the switcher lists; this model owns the session
    /// part and the cached indicator.
    private(set) var state = SessionsState()

    var sessions: [SessionSummary] { state.sessionList }
    var isShowingCached: Bool { state.showingCached }
    var hostId: String { state.hostId }
    var loadState: LoadState { Self.legacy(state.sessions) }
    var errorKey: L10nKey? { state.sessions.error.map { L10nKey.errorKey(for: $0) } }

    private static func legacy(_ load: Load<[SessionSummary]>) -> LoadState {
        switch load {
        case .idle: .idle
        case .loading: .loading
        case .loaded: .loaded
        case .failed(let code): .failed(code)
        }
    }

    private let service: any SessionService
    private let preferences: PreferencesStore

    private var isLoadInFlight = false
    private var reloadRequested = false
    private var requestedHostId = ""
    private var waiters: [CheckedContinuation<Void, Never>] = []

    init(service: any SessionService, preferences: PreferencesStore) {
        self.service = service
        self.preferences = preferences
    }

    /// Sessions grouped by workspace for the switcher. A single workspace
    /// yields one group so callers can decide whether to show section headers.
    var workspaceGroups: [WorkspaceGroup] {
        WorkspaceGrouping.groups(for: sessions, otherName: L10n.sessionsWorkspaceOther.string)
    }

    var hasMultipleWorkspaces: Bool { workspaceGroups.count > 1 }

    var continueSession: SessionSummary? {
        guard let last = preferences.value.lastSessionId else { return nil }
        return sessions.first { $0.sessionId == last }
    }

    var isEmpty: Bool { loadState == .loaded && sessions.isEmpty }

    func load(hostId: String) async {
        state = SessionsReduce.reduce(state, .sessionsLoadStarted(hostId: hostId))
        requestedHostId = hostId
        if isLoadInFlight {
            reloadRequested = true
            await withCheckedContinuation { continuation in
                waiters.append(continuation)
            }
            return
        }
        isLoadInFlight = true
        var shouldReload = true
        while shouldReload {
            reloadRequested = false
            await fetch(hostId: requestedHostId)
            shouldReload = reloadRequested
        }
        isLoadInFlight = false
        let pending = waiters
        waiters.removeAll()
        for waiter in pending {
            waiter.resume()
        }
    }

    func rememberOpened(_ session: SessionSummary) {
        preferences.update {
            $0.lastHostId = hostId
            $0.lastSessionId = session.sessionId
            $0.lastSessionTitle = session.title
        }
    }

    func isLastOpened(_ session: SessionSummary) -> Bool {
        preferences.value.lastSessionId == session.sessionId
            && preferences.value.lastHostId == hostId
    }

    func reset() {
        state = SessionsReduce.reduce(state, .reset)
        requestedHostId = ""
        reloadRequested = false
    }

    private func fetch(hostId: String) async {
        state = SessionsReduce.reduce(state, .sessionsLoadStarted(hostId: hostId))
        do {
            let loaded = try await service.sessions(hostId: hostId)
            apply(loaded)
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            if code == .hostOffline {
                do {
                    let cached = try await service.cachedSessions(hostId: hostId)
                    apply(cached, cached: true)
                } catch {
                    fail(error)
                }
            } else {
                fail(error)
            }
        }
    }

    private func apply(_ loaded: [SessionSummary], cached: Bool = false) {
        let sorted = loaded.sorted { lhs, rhs in
            if lhs.updatedAtUnixMs == rhs.updatedAtUnixMs {
                return lhs.sessionId < rhs.sessionId
            }
            return lhs.updatedAtUnixMs > rhs.updatedAtUnixMs
        }
        state = SessionsReduce.reduce(state, .sessionsLoaded(sorted, cached: cached))
    }

    private func fail(_ error: Error) {
        let code = (error as? SessionServiceError)?.code ?? .internalError
        state = SessionsReduce.reduce(state, .sessionsFailed(code))
    }
}
