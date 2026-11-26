// Shell pairing behaviour, split out of the model file.

import Foundation

@MainActor
extension AppShellModel {
    // MARK: - Conversation lifecycle

    /// Single entry point for opening a conversation. It replaces the root
    /// conversation, persists the last selection and dismisses the switcher.
    func openConversation(hostId: String, session: SessionSummary) {
        conversation?.close()
        let generation = activationGeneration
        let model = ConversationModel(
            hostId: hostId,
            sessionId: session.sessionId,
            sessionTitle: session.title,
            service: service,
            connectivity: connectivity,
            onFatalError: { [weak self] code in
                guard let self, self.isCurrentActivation(generation) else { return }
                self.handleConversationFatal(code)
            },
            onReconnected: { [weak self] in
                guard let self, self.isCurrentActivation(generation) else { return }
                Task { await self.refreshVisibleOperationResults() }
            },
            drafts: drafts,
            outgoingStore: outgoingStore
        )
        // Staged attachments belong to the active instance's data directory,
        // never to a process-wide root keyed only by Host and session.
        model.attachments.dataDirectory = activeInstance?.dataDirectory ?? dataDirectory
        conversation = model
        selectedHostId = hostId
        hostList.select(hostId)
        preferences.update {
            $0.paired = true
            $0.lastHostId = hostId
            $0.lastSessionId = session.sessionId
            $0.lastSessionTitle = session.title
        }
        updateCachedBanner()
        dismissSheet()
        rootState = .ready
        Task { await model.start() }
    }

    /// One read-only refresh for the visible instance/Host/session. Automatic
    /// lookup never sends a message or resumes pairing; other sessions stay
    /// available through Settings for manual recovery.
    func refreshVisibleOperationResults() async {
        guard let operationRecovery, let conversation else { return }
        let scope = OperationScope(
            instanceId: activeInstance?.id ?? service.instanceId,
            hostId: conversation.hostId,
            sessionId: conversation.sessionId
        )
        await operationRecovery.refreshVisibleOperations(in: scope)
        guard self.conversation === conversation else { return }
        conversation.applyRecoveredOperations(operationRecovery.operations(for: scope))
    }

    func updateCachedBanner() {
        guard let conversation else {
            isOfflineCached = false
            return
        }
        isOfflineCached = sessionList.isShowingCached
            && sessionList.hostId == conversation.hostId
    }

    // MARK: - Pairing

    func finishPairing(hostId: String) {
        conversation?.close()
        conversation = nil
        preferences.update {
            $0.paired = true
            $0.lastHostId = hostId
        }
        isPaired = true
        isRevoked = false
        prefersInvitationAfterDeviceReset = false
        dismissSheet()
        dismissSheet()
        pairing.reset()
        didBootstrap = false
        isOfflineCached = false
        resumeMarker = nil
        sessionList.reset()
        rootState = .restoring
        Task { await bootstrapConversation() }
    }

    func handleOpenURL(_ url: URL) async {
        guard !isRevoked else { return }
        // The Cloud authorization callback is consumed by the Cloud model and
        // must never be offered to the self-hosted invitation parser. It is
        // fenced independently and must not wait for launch restore.
        if CloudSignInModel.isCloudCallback(url) {
            await cloudSignIn?.receiveCallback(url)
            return
        }
        // A system-delivered link can arrive before `launchRestore` finishes.
        // Queue it until the root exists; otherwise a restore that opens the
        // conversation could dismiss the pairing sheet and silently drop it.
        guard isLaunched else {
            pendingInvitationLink = url
            return
        }
        pendingInvitationLink = nil
        if isPaired {
            present(.invitation)
        }
        await pairing.handleOpenURL(url)
    }

    /// Marks the first launch restore complete and replays a system link that
    /// arrived during it. Safe to call from every restore/bootstrap exit.
    func finishLaunch() {
        isLaunched = true
        guard let url = pendingInvitationLink else { return }
        pendingInvitationLink = nil
        Task { [weak self] in
            await self?.handleOpenURL(url)
        }
    }

    /// Local Cloud sign-out from Settings. Core keeps a scoped pending remote
    /// revocation when Cloud is unreachable and retries it later.
    func signOutCloud() async {
        await cloudSignIn?.signOut()
    }

    /// Leaves the Cloud-authorized root after a local sign-out or a confirmed
    /// remote revocation. The device identity is preserved.
    func handleCloudSignedOut() {
        conversation?.close()
        conversation = nil
        outgoingStore = ConversationOutgoingStore()
        isPaired = false
        preferences.update {
            $0.paired = false
            $0.lastHostId = nil
            $0.lastSessionId = nil
            $0.lastSessionTitle = nil
        }
        selectedHostId = nil
        isOfflineCached = false
        resumeMarker = nil
        didBootstrap = false
        connectivity.reset()
        hostList.reset()
        sessionList.reset()
        pairing.reset()
        rootState = .restoring
        // The signed-out status and pending remote sign-out stay visible in
        // Settings even though the conversation root has been retired.
        Task { @MainActor [weak self] in
            await self?.accountSettings?.load()
        }
    }

    /// Entry point for pairing an additional Host while already paired, or for
    /// re-pairing from a settings/revocation screen. Pairing state is reset so
    /// a stale invitation attempt cannot leak into the new flow.
    func openManualPairing() {
        pairing.reset()
        present(.invitation)
    }

    func dismissInvitationSheet() {
        dismissSheet()
        if !isPaired {
            pairing.reset()
        }
    }
}
