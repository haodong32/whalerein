// Shell switcher behaviour, split out of the model file.

import Foundation

@MainActor
extension AppShellModel {
    // MARK: - Switcher

    func showSessionSwitcher() {
        guard isPaired else { return }
        present(.sessionSwitcher)
    }

    /// Refreshes the Host list and the selected Host's sessions. Never changes
    /// the open conversation.
    func refreshSwitcher() async {
        guard isPaired else { return }
        let generation = activationGeneration
        await hostList.load()
        guard isCurrentActivation(generation), isPaired else { return }
        if case .failed(let code) = hostList.loadState {
            handleSwitcherFailure(code)
            return
        }
        guard !hostList.hosts.isEmpty else {
            updateCachedBanner()
            return
        }

        let hostId: String
        if let selectedHostId, hostList.hosts.contains(where: { $0.hostId == selectedHostId }) {
            hostId = selectedHostId
        } else {
            hostId = chooseHost(from: hostList.hosts).hostId
            selectedHostId = hostId
            hostList.select(hostId)
        }
        if let host = hostList.host(hostId),
           let version = ProtocolCompatibility.incompatibility(for: host.protocolVersion) {
            rootState = .incompatible(hostId: hostId, version: version)
            updateCachedBanner()
            return
        }
        await sessionList.load(hostId: hostId)
        guard isCurrentActivation(generation) else { return }
        if case .failed(let code) = sessionList.loadState {
            handleSwitcherFailure(code)
        }
        updateCachedBanner()
    }

    /// Loads another Host's sessions into the switcher while leaving the open
    /// conversation untouched until the user picks a session.
    func selectHostInSwitcher(_ hostId: String) async {
        guard isPaired, hostList.hosts.contains(where: { $0.hostId == hostId }) else { return }
        if let host = hostList.host(hostId),
           let version = ProtocolCompatibility.incompatibility(for: host.protocolVersion) {
            selectedHostId = hostId
            hostList.select(hostId)
            conversation?.close()
            conversation = nil
            sessionList.reset()
            isOfflineCached = false
            rootState = .incompatible(hostId: hostId, version: version)
            return
        }
        selectedHostId = hostId
        hostList.select(hostId)
        sessionList.reset()
        await sessionList.load(hostId: hostId)
        updateCachedBanner()
    }

    func selectSessionInSwitcher(_ session: SessionSummary) {
        guard let hostId = selectedHostId,
              sessionList.sessions.contains(where: { $0.sessionId == session.sessionId }) else {
            return
        }
        openConversation(hostId: hostId, session: session)
    }
}
