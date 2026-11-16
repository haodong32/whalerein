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

    private(set) var sessions: [SessionSummary] = []
    private(set) var loadState: LoadState = .idle
    private(set) var isShowingCached = false
    private(set) var errorKey: L10nKey?
    private(set) var hostId: String = ""

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
        self.hostId = hostId
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
        sessions = []
        loadState = .idle
        isShowingCached = false
        errorKey = nil
        hostId = ""
        requestedHostId = ""
        reloadRequested = false
    }

    private func fetch(hostId: String) async {
        self.hostId = hostId
        loadState = .loading
        errorKey = nil
        do {
            let loaded = try await service.sessions(hostId: hostId)
            apply(loaded)
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            if code == .hostOffline {
                do {
                    let cached = try await service.cachedSessions(hostId: hostId)
                    apply(cached)
                    isShowingCached = true
                } catch {
                    fail(error)
                }
            } else {
                fail(error)
            }
        }
    }

    private func apply(_ loaded: [SessionSummary]) {
        sessions = loaded.sorted { lhs, rhs in
            if lhs.updatedAtUnixMs == rhs.updatedAtUnixMs {
                return lhs.sessionId < rhs.sessionId
            }
            return lhs.updatedAtUnixMs > rhs.updatedAtUnixMs
        }
        loadState = .loaded
        errorKey = nil
        isShowingCached = false
    }

    private func fail(_ error: Error) {
        let code = (error as? SessionServiceError)?.code ?? .internalError
        loadState = .failed(code)
        errorKey = L10nKey.errorKey(for: code)
        isShowingCached = false
    }
}
