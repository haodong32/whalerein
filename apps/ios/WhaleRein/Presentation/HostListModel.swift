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

    private(set) var hosts: [HostSummary] = []
    private(set) var loadState: LoadState = .idle
    private(set) var errorKey: L10nKey?
    private(set) var selectedHostId: String?

    private let service: any SessionService
    private let preferences: PreferencesStore

    private var isLoadInFlight = false
    private var reloadRequested = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    init(service: any SessionService, preferences: PreferencesStore) {
        self.service = service
        self.preferences = preferences
        selectedHostId = preferences.value.lastHostId
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
        selectedHostId = hostId
        preferences.update { $0.lastHostId = hostId }
    }

    func host(_ hostId: String) -> HostSummary? {
        hosts.first { $0.hostId == hostId }
    }

    func reset() {
        hosts = []
        loadState = .idle
        errorKey = nil
        selectedHostId = nil
        reloadRequested = false
    }

    private func fetch() async {
        loadState = .loading
        errorKey = nil
        do {
            let loaded = try await service.hosts()
            hosts = loaded
            loadState = .loaded
            if let selectedHostId, !loaded.contains(where: { $0.hostId == selectedHostId }) {
                self.selectedHostId = nil
            }
            if selectedHostId == nil {
                selectedHostId = loaded.first?.hostId
            }
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            loadState = .failed(code)
            errorKey = L10nKey.errorKey(for: code)
        }
    }
}
