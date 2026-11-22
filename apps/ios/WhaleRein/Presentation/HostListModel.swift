// Host list presentation state: authorized Hosts, online state, pinned
// fingerprint and the remembered selection. Concurrent load requests coalesce
// instead of being dropped: a refresh that arrives while a load is in flight
// schedules one more fetch after the current one.

import Foundation
import Observation

@MainActor
@Observable
final class HostListModel {
    enum LoadState: Equatable {
        case idle
        case loading
        case loaded
        case failed(AppErrorCode)
    }

    /// One business state for the switcher lists; this model owns the Host part.
    private(set) var state = SessionsState()

    var hosts: [HostSummary] { state.hostList }
    var selectedHostId: String? { state.selectedHostId }
    var loadState: LoadState { Self.legacy(state.hosts) }
    var errorKey: L10nKey? { state.hosts.error.map { L10nKey.errorKey(for: $0) } }

    private static func legacy(_ load: Load<[HostSummary]>) -> LoadState {
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
    private var waiters: [CheckedContinuation<Void, Never>] = []

    init(service: any SessionService, preferences: PreferencesStore) {
        self.service = service
        self.preferences = preferences
        state.selectedHostId = preferences.value.lastHostId
    }

    func load() async {
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
            await fetch()
            shouldReload = reloadRequested
        }
        isLoadInFlight = false
        let pending = waiters
        waiters.removeAll()
        for waiter in pending {
            waiter.resume()
        }
    }

    func select(_ hostId: String) {
        guard hosts.contains(where: { $0.hostId == hostId }) else { return }
        state = SessionsReduce.reduce(state, .hostSelected(hostId))
        preferences.update { $0.lastHostId = hostId }
    }

    func host(_ hostId: String) -> HostSummary? {
        hosts.first { $0.hostId == hostId }
    }

    func reset() {
        state = SessionsReduce.reduce(state, .reset)
        reloadRequested = false
    }

    private func fetch() async {
        state = SessionsReduce.reduce(state, .hostsLoadStarted)
        do {
            let loaded = try await service.hosts()
            state = SessionsReduce.reduce(state, .hostsLoaded(loaded))
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            state = SessionsReduce.reduce(state, .hostsFailed(code))
        }
    }
}
