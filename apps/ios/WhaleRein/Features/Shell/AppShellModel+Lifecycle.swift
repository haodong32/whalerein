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

    /// Ordered owner of process scene transitions. The synchronous part runs
    /// inside this call: the shield is visible and rendering is paused before
    /// the system takes its snapshot and before anything is awaited. The
    /// returned task completes the ordered asynchronous part.
    ///
    /// Every notification is accepted by `AppSceneLifecycle`; a repeat for the
    /// current phase is coalesced. Each accepted transition runs after the
    /// previous one, so a background suspension always finishes before its
    /// foreground resume and a stale completion cannot override a newer phase.
    @discardableResult
    func scenePhaseDidChange(_ phase: AppScenePhase) -> Task<Void, Never> {
        guard let transition = shell.scene.accept(phase) else {
            // A duplicate notification must not perform any side effect twice.
            // Re-apply the current presentation only; a pending resume keeps
            // the shield up.
            reapplyCurrentScenePresentation()
            return sceneTransitionTask ?? Task {}
        }
        applyScenePresentation(
            for: transition.phase,
            resumesObservation: transition.resumesObservation
        )
        let activation = activationGeneration
        let previous = sceneTransitionTask
        let task = Task { [weak self] in
            // Serialize transitions: the async half of an older notification
            // completes before the next accepted one starts.
            await previous?.value
            guard let self, self.isCurrentActivation(activation) else { return }
            await self.performSceneTransition(transition)
        }
        sceneTransitionTask = task
        return task
    }

    /// Awaitable entry used by tests and non-SwiftUI callers. The synchronous
    /// shield and pause still happen before the first suspension.
    func handleScenePhase(_ phase: AppScenePhase) async {
        await scenePhaseDidChange(phase).value
    }

    /// Applies the immediate, non-async part of one accepted phase. The shield
    /// and the rendering pause are set here so a sensitive surface is covered
    /// before any awaiting work starts.
    private func applyScenePresentation(
        for phase: AppScenePhase,
        resumesObservation: Bool
    ) {
        switch phase {
        case .inactive:
            // Inactivity only hides the visible surface and stops clock-driven
            // updates. The watch, its cursor and the event subscription stay.
            isPrivacyShieldVisible = true
            conversation?.pauseForInactivity()
        case .background:
            isPrivacyShieldVisible = true
            conversation?.pauseForInactivity()
            resumeMarker = AppResumeMarker(
                recordedAt: Date(),
                hostId: conversation?.hostId ?? selectedHostId,
                sessionId: conversation?.sessionId
            )
        case .active:
            if isRevoked {
                // The revoked screen carries no conversation content.
                isPrivacyShieldVisible = false
                return
            }
            if resumesObservation {
                // Keep the last authorized frame covered until the ordered
                // resume has reconciled it from the retained cursor.
                isPrivacyShieldVisible = true
            } else {
                isPrivacyShieldVisible = false
                conversation?.resumeForInactivity()
            }
        }
    }

    /// Idempotent presentation for a repeated notification for the current
    /// phase. Unlike a real transition it never starts or completes a resume,
    /// and it never reveals content while a background resume is pending.
    private func reapplyCurrentScenePresentation() {
        switch shell.scene.phase {
        case .inactive, .background:
            isPrivacyShieldVisible = true
            conversation?.pauseForInactivity()
        case .active:
            if isRevoked {
                isPrivacyShieldVisible = false
            } else if shell.scene.observationSuspended {
                isPrivacyShieldVisible = true
            } else {
                isPrivacyShieldVisible = false
                conversation?.resumeForInactivity()
            }
        }
    }

    /// Async half of one accepted transition. Background suspension always
    /// completes, even when a newer notification superseded it, because losing
    /// the draft flush or leaving the watch running would break the suspension
    /// guarantee. A foreground resume, by contrast, is only started by the
    /// newest transition.
    private func performSceneTransition(_ transition: AppSceneLifecycle.Transition) async {
        switch transition.phase {
        case .inactive:
            // Nothing to await: the shield, the pause and the retained watch
            // are entirely synchronous.
            break
        case .background:
            await suspendObservationForBackground()
        case .active:
            if transition.resumesObservation {
                await resumeObservationAfterBackground(transition)
            } else {
                await reconcileInterruptedConversationIfNeeded()
            }
        }
    }

    /// Persists the draft normally and stops the watch exactly once for one
    /// actual background entry. `suspendForBackground` is idempotent, so a
    /// duplicate notification cannot stop the watch twice.
    private func suspendObservationForBackground() async {
        if let conversation {
            await conversation.suspendForBackground()
        }
        connectivity.stopMonitoring()
    }

    /// A brief interruption keeps the watch. If that watch ended for a real
    /// reason while inactive (transport loss or Host rejection), reconcile once
    /// from the cursor instead of presenting a dead conversation.
    private func reconcileInterruptedConversationIfNeeded() async {
        guard let conversation, conversation.needsReconnectAfterInterruption else { return }
        await conversation.reconnectWatch()
    }

    /// One ordered foreground resume for the currently selected, authorized
    /// session. The shield stays up until reconciliation completes; every step
    /// after an await is fenced against a newer transition and a replaced
    /// runtime, so a stale resume cannot reveal content or reopen work.
    private func resumeObservationAfterBackground(_ transition: AppSceneLifecycle.Transition) async {
        guard shell.scene.isCurrent(transition) else { return }
        let activation = activationGeneration
        if isRevoked {
            revealAfterSceneTransition(transition)
            return
        }
        if case .failed(.keychainUnavailable) = rootState {
            await retryLaunch()
            guard shell.scene.isCurrent(transition), isCurrentActivation(activation) else { return }
            if isRevoked {
                revealAfterSceneTransition(transition)
                return
            }
        }
        guard isPaired else {
            revealAfterSceneTransition(transition)
            return
        }
        if let conversation {
            await conversation.resumeAfterForeground()
            guard shell.scene.isCurrent(transition), isCurrentActivation(activation) else { return }
            guard !isRevoked else {
                // A revocation surfaced during the resume must not be followed
                // by switcher or operation refreshes; reveal the blocked state.
                revealAfterSceneTransition(transition)
                return
            }
        } else if case .empty = rootState {
            didBootstrap = false
            await bootstrapConversation()
            guard shell.scene.isCurrent(transition), isCurrentActivation(activation) else { return }
            guard !isRevoked else {
                revealAfterSceneTransition(transition)
                return
            }
        }
        // Reconciled from the retained cursor: the current authorized view may
        // be shown again.
        revealAfterSceneTransition(transition)
        await refreshSwitcher()
        guard shell.scene.isCurrent(transition), isCurrentActivation(activation) else { return }
        updateCachedBanner()
        startHostMonitoringIfNeeded()
        await refreshVisibleOperationResults()
    }

    /// Ends one background suspension and reveals the current content, but only
    /// while this transition is still the newest. A newer background or
    /// inactive notification keeps the shield up.
    private func revealAfterSceneTransition(_ transition: AppSceneLifecycle.Transition) {
        guard shell.scene.isCurrent(transition) else { return }
        shell.scene.observationDidResume()
        isPrivacyShieldVisible = false
    }

    /// Compatibility entry for tests: one actual background entry.
    func enterBackground() async {
        await handleScenePhase(.background)
    }

    /// Compatibility entry for tests: one foreground resume.
    func becomeActive() async {
        await handleScenePhase(.active)
    }

    /// Starts (or restarts) the app-level Host probe for every visible Host.
    /// Called after launch restore and on every completed foreground return.
    func startHostMonitoringIfNeeded() {
        guard isPaired, !isRevoked, shell.scene.isActiveAndResumed else { return }
        let hostIds = hostList.hosts.map(\.hostId)
        guard !hostIds.isEmpty else { return }
        connectivity.startMonitoring(service: service, hostIds: hostIds)
    }
}
