// Root: invitation while unpaired, otherwise the conversation root. Wide
// layouts keep a persistent Host/session sidebar beside the conversation;
// compact layouts keep the bottom-sheet switcher. Settings, diagnostics and
// manual pairing remain sheets.
//
// The root also owns the privacy shield and the blocking revocation and
// incompatible-Host states, so no stale conversation content or dead navigation
// is visible in those cases.

import SwiftUI

struct RootView: View {
    /// The shell injects its model and runtime directly; only the remaining
    /// feature views still read the environment while they migrate.
    let app: AppShellModel
    let runtime: AppRuntime
    @Environment(\.scenePhase) private var scenePhase
    @State private var availableWidth: CGFloat = 0

    var body: some View {
        ZStack {
            content
            if app.isPrivacyShieldVisible {
                PrivacyShieldView()
                    .transition(.opacity)
                    .zIndex(1)
            }
        }
        .task {
            await runtime.start()
        }
        .onChange(of: scenePhase) { _, phase in
            let mapped = AppScenePhase(phase)
            Task { await app.handleScenePhase(mapped) }
        }
        .onOpenURL { url in
            Task { await app.handleOpenURL(url) }
        }
        .sheet(isPresented: sheet(.settings)) {
            SettingsView(
                settings: app.settings,
                diagnostics: app.diagnostics,
                account: app.accountSettings,
                operations: app.operationRecovery,
                cloud: app.cloudSignIn,
                dataPath: app.dataDirectory.path,
                serviceSelection: serviceSelection,
                reasoningVisible: app.reasoningVisible,
                setReasoningVisible: { app.setReasoningVisible($0) },
                onSignOutCloud: { Task { await app.signOutCloud() } },
                onAddHost: { app.openManualPairing() }
            )
        }
        .sheet(isPresented: sheet(.diagnostics)) {
            DiagnosticsView(model: app.diagnostics)
        }
        .sheet(isPresented: sheet(.invitation)) {
            NavigationStack {
                InvitationView(
                    pairing: app.pairing,
                    serviceSelection: serviceSelection,
                    isModal: true,
                    onDismiss: { app.dismissInvitationSheet() }
                )
            }
        }
        .sheet(isPresented: sheet(.sessionSwitcher)) {
            SessionSwitcherView(
                models: switcherModels,
                actions: switcherActions
            )
        }
        #if DEBUG
        .sheet(isPresented: Bindable(runtime.router).showsStateGallery) {
            StateGalleryView()
        }
        #endif
    }

    private var serviceSelection: ServiceSelectionContext {
        ServiceSelectionContext(
            configuration: app.configuration,
            activeInstance: app.activeInstance,
            actions: ServiceSelectionActions(
                switchToService: { await app.switchToService($0) },
                enableLocal: { await app.enableLocal(pem: $0) },
                removeLocalCA: { await app.removeLocalCA() },
                importStandaloneCA: { await app.importStandaloneCA($0) },
                removeStandaloneCA: { await app.removeStandaloneCA() }
            )
        )
    }

    private var switcherModels: SessionSwitcherModels {
        SessionSwitcherModels(
            hosts: app.hostList,
            sessions: app.sessionList,
            management: app.sessionManagement,
            currentConversation: app.conversation.map {
                SessionContext(hostId: $0.hostId, sessionId: $0.sessionId)
            }
        )
    }

    private var switcherActions: SessionSwitcherActions {
        SessionSwitcherActions(
            refresh: { await app.refreshSwitcher() },
            selectHost: { await app.selectHostInSwitcher($0) },
            selectSession: { app.selectSessionInSwitcher($0) },
            openConversation: { app.openConversation(hostId: $0, session: $1) },
            addHost: { app.openManualPairing() },
            close: { app.dismissSheet() }
        )
    }

    /// Binds one route to a sheet presentation. Dismissing clears only the
    /// route that is currently presented.
    private func sheet(_ route: AppRoute) -> Binding<Bool> {
        Binding(
            get: { runtime.router.presented == route },
            set: { presented in
                if !presented, runtime.router.presented == route {
                    runtime.router.dismiss()
                }
            }
        )
    }

    @ViewBuilder
    private var content: some View {
        if app.isRevoked {
            RevokedDeviceView(app: app)
        } else if app.isPaired {
            pairedRoot
        } else if let cloud = app.cloudSignIn, !app.prefersInvitationAfterDeviceReset {
            // Cloud instances use the official hosted sign-in and computer
            // directory; the self-hosted invitation flow remains reachable
            // from the welcome surface. An explicit local forget/revoke
            // returns to the invitation flow so the reset is visible.
            CloudSignInRootView(cloud: cloud, serviceSelection: serviceSelection) {
                app.openManualPairing()
            }
        } else {
            InvitationView(
                pairing: app.pairing,
                serviceSelection: serviceSelection,
                onDismiss: { app.dismissInvitationSheet() }
            )
        }
    }

    private var pairedRoot: some View {
        let showsSidebar = AdaptiveLayout.showsSidebar(width: availableWidth)

        return NavigationStack {
            Group {
                if showsSidebar {
                    HStack(spacing: 0) {
                        SessionSwitcherSidebar(
                            models: switcherModels,
                            actions: switcherActions
                        )
                            .frame(width: AdaptiveLayout.sidebarWidth)
                        Divider()
                        conversationContent
                            .frame(maxWidth: .infinity, maxHeight: .infinity)
                    }
                } else {
                    conversationContent
                }
            }
            .toolbar {
                    if !showsSidebar {
                        ToolbarItem(placement: .topBarLeading) {
                            Button {
                                app.showSessionSwitcher()
                            } label: {
                                Image(systemName: "sidebar.left")
                            }
                            .accessibilityLabel(L10n.switcherOpen.key)
                            .accessibilityIdentifier("conversation.switcher")
                        }
                    }
                    ToolbarItemGroup(placement: .topBarTrailing) {
                        // Diagnostics are a release-engineering surface. Keep the
                        // entry point out of shipped builds while the underlying
                        // redacted report stays available in Debug/Test.
                        #if DEBUG
                        Button {
                            runtime.router.present(.diagnostics)
                        } label: {
                            Image(systemName: "stethoscope")
                        }
                        .accessibilityLabel(L10n.hostsDiagnostics.key)
                        .accessibilityIdentifier("conversation.diagnostics")

                        Button {
                            runtime.router.showsStateGallery = true
                        } label: {
                            Image(systemName: "square.grid.2x2")
                        }
                        .accessibilityLabel("State gallery")
                        .accessibilityIdentifier("conversation.gallery")
                        #endif

                        Button {
                            runtime.router.present(.settings)
                        } label: {
                            Image(systemName: "gearshape")
                        }
                        .accessibilityLabel(L10n.hostsSettings.key)
                        .accessibilityIdentifier("conversation.settings")
                }
            }
        }
        .onGeometryChange(for: CGFloat.self) { proxy in
            proxy.size.width
        } action: { width in
            availableWidth = width
        }
    }

    @ViewBuilder
    private var conversationContent: some View {
        if let conversation = app.conversation {
            ConversationView(
                model: conversation,
                reasoningVisible: app.reasoningVisible,
                isCached: app.isOfflineCached,
                isCurrent: { app.conversation === conversation }
            )
                // Selecting a different session replaces the conversation and
                // must not inherit the previous transcript's scroll position.
                .id(ObjectIdentifier(conversation))
        } else {
            ConversationRootPlaceholderView(app: app)
        }
    }
}

/// Opaque lock screen shown whenever the app is not active. It is rendered
/// during the inactive transition so the app-switcher snapshot never contains
/// conversation rows.
