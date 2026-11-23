// Shell lifecycle behaviour, split out of the model file.

import Foundation

@MainActor
extension AppShellModel {
    // MARK: - Launch restore

    /// Single launch entry point for both paired and unpaired preferences. If
    /// preferences lost the paired flag but Core still has authorized Hosts, the
    /// app adopts the Core pairing instead of stranding the user on onboarding.
    func launchRestore() async {
        defer { finishLaunch() }
        guard !isRevoked else { return }
        let generation = activationGeneration
        if let cloudSignIn {
            // Restore the locally verified Cloud session before deciding
            // whether this instance has a computer to open. A paired instance
            // still needs its account restored for Settings and sign-out.
            await cloudSignIn.restore()
            guard isCurrentActivation(generation) else { return }
        }
        if !isPaired {
            await adoptExistingCorePairing()
        }
        guard isCurrentActivation(generation), isPaired else { return }
        await bootstrapConversation()
    }

    /// Retries a recoverable launch failure (for example an unlocked device
    /// after a locked-Keychain state). The attempt is user- or scene-driven, never
    /// a timer loop.
    func retryLaunch() async {
        guard !isRevoked else { return }
        let generation = activationGeneration
        didBootstrap = false
        isBootstrapping = false
        // A locked Keychain also prevented the Cloud account restore. Retry it
        // here so the same account and directory return after unlock instead of
        // leaving a stale signed-out surface beside a restored conversation.
        if let cloudSignIn {
            await cloudSignIn.restore()
            guard isCurrentActivation(generation) else { return }
        }
        if !isPaired {
            await adoptExistingCorePairing()
        }
        guard isCurrentActivation(generation), isPaired else { return }
        await bootstrapConversation()
    }

    /// Restores the conversation root after launch or pairing. Idempotent while
    /// a restore is in flight or after a conversation has been opened.
    func bootstrapConversation() async {
        guard isPaired, conversation == nil, !didBootstrap, !isBootstrapping else { return }
        defer { finishLaunch() }
        let generation = activationGeneration
        isBootstrapping = true
        rootState = .restoring
        defer { isBootstrapping = false }

        await hostList.load()
        guard isCurrentActivation(generation), isPaired else { return }
        if case .failed(let code) = hostList.loadState {
            handleBootFailure(code)
            return
        }

        let hosts = hostList.hosts
        guard !hosts.isEmpty else {
            rootState = .empty(.noHosts)
            return
        }
        // One app-level probe for every Host the user can see. This also seeds
        // the initial snapshot before the conversation renders its chrome.
        connectivity.startMonitoring(service: service, hostIds: hosts.map(\.hostId))

        let host = chooseHost(from: hosts)
        if let version = ProtocolCompatibility.incompatibility(for: host.protocolVersion) {
            selectedHostId = host.hostId
            rootState = .incompatible(hostId: host.hostId, version: version)
            return
        }
        selectedHostId = host.hostId
        hostList.select(host.hostId)

        await sessionList.load(hostId: host.hostId)
        guard isCurrentActivation(generation), isPaired else { return }
        if case .failed(let code) = sessionList.loadState {
            handleBootFailure(code)
            return
        }

        // Launch restoration follows the switcher's visibility rule: an
        // archived Session is not reopened unless the archived view is on.
        let sessions = sessionList.visibleSessions
        guard !sessions.isEmpty else {
            rootState = .empty(.noSessions)
            return
        }

        let session = restoredSession(from: sessions)
        didBootstrap = true
        openConversation(hostId: host.hostId, session: session)
    }

    func adoptExistingCorePairing() async {
        let generation = activationGeneration
        do {
            let hosts = try await service.hosts()
            guard isCurrentActivation(generation), let first = hosts.first else { return }
            isPaired = true
            didBootstrap = false
            rootState = .restoring
            preferences.update {
                $0.paired = true
                if $0.lastHostId == nil {
                    $0.lastHostId = first.hostId
                }
            }
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            handleBootFailure(code)
        }
    }

    func chooseHost(from hosts: [HostSummary]) -> HostSummary {
        if let remembered = preferences.value.lastHostId,
           let host = hosts.first(where: { $0.hostId == remembered }) {
            return host
        }
        return hosts.first(where: \.online) ?? hosts[0]
    }

    func restoredSession(from sessions: [SessionSummary]) -> SessionSummary {
        if let remembered = preferences.value.lastSessionId,
           let session = sessions.first(where: { $0.sessionId == remembered }) {
            return session
        }
        return sessions[0]
    }

    // MARK: - Scene lifecycle

    func handleScenePhase(_ phase: AppScenePhase) async {
        switch phase {
        case .active:
            await becomeActive()
        case .inactive, .background:
            await enterBackground()
        }
    }

    /// Hides sensitive content immediately (including during the transition to
    /// inactive, before the system takes its snapshot), flushes the draft and
    /// records where the session was when the app was suspended.
    func enterBackground() async {
        isPrivacyShieldVisible = true
        guard !isInBackground else { return }
        isInBackground = true
        resumeMarker = AppResumeMarker(
            recordedAt: Date(),
            hostId: conversation?.hostId ?? selectedHostId,
            sessionId: conversation?.sessionId
        )
        if let conversation {
            await conversation.suspendForBackground()
        }
        connectivity.stopMonitoring()
    }

    /// Reconnects the visible session with a fresh watch instead of leaving a
    /// half-open stream, then refreshes the switcher and cached banner.
    func becomeActive() async {
        isPrivacyShieldVisible = false
        guard isInBackground else { return }
        isInBackground = false
        guard isPaired, !isRevoked else { return }
        let generation = activationGeneration

        if case .failed(.keychainUnavailable) = rootState {
            await retryLaunch()
        }
        guard isCurrentActivation(generation) else { return }
        if let conversation {
            await conversation.resumeAfterForeground()
        } else if case .empty = rootState {
            didBootstrap = false
            await bootstrapConversation()
        }
        guard isCurrentActivation(generation) else { return }
        await refreshSwitcher()
        updateCachedBanner()
        startHostMonitoringIfNeeded()
        await refreshVisibleOperationResults()
    }

    /// Starts (or restarts) the app-level Host probe for every visible Host.
    /// Called after launch restore and on every foreground return.
    func startHostMonitoringIfNeeded() {
        guard isPaired, !isRevoked, !isInBackground else { return }
        let hostIds = hostList.hosts.map(\.hostId)
        guard !hostIds.isEmpty else { return }
        connectivity.startMonitoring(service: service, hostIds: hostIds)
    }
}
