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
    @Environment(AppModel.self) private var model
    @Environment(\.scenePhase) private var scenePhase
    @State private var availableWidth: CGFloat = 0

    var body: some View {
        @Bindable var app = model
        ZStack {
            content
            if app.isPrivacyShieldVisible {
                PrivacyShieldView()
                    .transition(.opacity)
                    .zIndex(1)
            }
        }
        .task {
            await app.launchRestore()
        }
        .onChange(of: scenePhase) { _, phase in
            let mapped = AppScenePhase(phase)
            Task { await app.handleScenePhase(mapped) }
        }
        .onOpenURL { url in
            Task { await app.handleOpenURL(url) }
        }
        .sheet(isPresented: $app.showSettings) {
            SettingsView()
        }
        .sheet(isPresented: $app.showDiagnostics) {
            DiagnosticsView()
        }
        .sheet(isPresented: $app.showInvitationSheet) {
            NavigationStack {
                InvitationView(isModal: true)
            }
        }
        .sheet(isPresented: $app.showSwitcher) {
            SessionSwitcherView()
        }
    }

    @ViewBuilder
    private var content: some View {
        if model.isRevoked {
            RevokedDeviceView()
        } else if model.isPaired {
            pairedRoot
        } else if let cloud = model.cloudSignIn {
            // Cloud instances use the official hosted sign-in and computer
            // directory; the self-hosted invitation flow remains reachable
            // from the welcome surface.
            CloudSignInRootView(cloud: cloud)
        } else {
            InvitationView()
        }
    }

    private var pairedRoot: some View {
        let showsSidebar = AdaptiveLayout.showsSidebar(width: availableWidth)

        return NavigationStack {
            Group {
                if showsSidebar {
                    HStack(spacing: 0) {
                        SessionSwitcherSidebar()
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
                                model.showSessionSwitcher()
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
                            model.showDiagnostics = true
                        } label: {
                            Image(systemName: "stethoscope")
                        }
                        .accessibilityLabel(L10n.hostsDiagnostics.key)
                        .accessibilityIdentifier("conversation.diagnostics")
                        #endif

                        Button {
                            model.showSettings = true
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
        if let conversation = model.conversation {
            ConversationView(model: conversation)
                // Selecting a different session replaces the conversation and
                // must not inherit the previous transcript's scroll position.
                .id(ObjectIdentifier(conversation))
        } else {
            ConversationRootPlaceholderView()
        }
    }
}

/// Opaque lock screen shown whenever the app is not active. It is rendered
/// during the inactive transition so the app-switcher snapshot never contains
/// conversation rows.
struct PrivacyShieldView: View {
    var body: some View {
        ZStack {
            Color(.systemBackground)
                .ignoresSafeArea()
            VStack(spacing: 12) {
                Image(systemName: "lock.shield")
                    .font(.system(size: 44))
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text(L10n.privacyShieldTitle.key)
                    .font(.title3.weight(.semibold))
                    .accessibilityIdentifier("privacy.shield.title")
                Text(L10n.privacyShieldBody.key)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            .padding(32)
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("privacy.shield")
    }
}

/// Blocking state after a remote revocation. Pairing and forgetting both leave
/// this screen and clear navigation; neither requires finding Settings first.
struct RevokedDeviceView: View {
    @Environment(AppModel.self) private var app
    @State private var isForgetting = false

    var body: some View {
        NavigationStack {
            VStack(spacing: 16) {
                Spacer(minLength: 0)
                Image(systemName: "person.crop.circle.badge.xmark")
                    .font(.system(size: 44))
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text(L10n.revokedTitle.key)
                    .font(.title2.weight(.semibold))
                    .multilineTextAlignment(.center)
                    .accessibilityIdentifier("revoked.title")
                Text(L10n.revokedBody.key)
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .padding(.horizontal, 24)

                VStack(spacing: 12) {
                    Button(L10n.revokedPair.key) {
                        app.beginRepairAfterRevocation()
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .accessibilityIdentifier("revoked.pair")

                    Button(L10n.revokedForget.key, role: .destructive) {
                        isForgetting = true
                        Task {
                            await app.forgetAfterRevocation()
                            isForgetting = false
                        }
                    }
                    .buttonStyle(.bordered)
                    .controlSize(.large)
                    .disabled(isForgetting)
                    .accessibilityIdentifier("revoked.forget")
                }
                .padding(.top, 8)
                Spacer(minLength: 0)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .padding(24)
            .navigationTitle(L10n.appName.key)
            .inlineNavigationTitle()
        }
    }
}

private struct ConversationRootPlaceholderView: View {
    @Environment(AppModel.self) private var app

    var body: some View {
        Group {
            switch app.rootState {
            case .restoring:
                VStack(spacing: 12) {
                    ProgressView()
                    Text(L10n.conversationRestoring.key)
                        .foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityIdentifier("conversation.restoring")
            case .empty(.noHosts):
                ContentUnavailableView {
                    Label(L10n.conversationNoHostsTitle.key, systemImage: "desktopcomputer")
                } description: {
                    Text(L10n.hostsEmpty.key)
                } actions: {
                    Button(L10n.hostsAdd.key) {
                        app.openManualPairing()
                    }
                    .accessibilityIdentifier("conversation.addHost")

                    Button(L10n.commonRetry.key) {
                        Task { await app.bootstrapConversation() }
                    }
                    .accessibilityIdentifier("conversation.retry")
                }
            case .empty(.noSessions):
                ContentUnavailableView {
                    Label(L10n.conversationNoSessionsTitle.key, systemImage: "bubble.left.and.bubble.right")
                } description: {
                    Text(L10n.sessionsEmpty.key)
                } actions: {
                    Button(L10n.conversationChooseSession.key) {
                        app.showSessionSwitcher()
                    }
                    .accessibilityIdentifier("conversation.chooseSession")

                    Button(L10n.commonRetry.key) {
                        Task { await app.bootstrapConversation() }
                    }
                    .accessibilityIdentifier("conversation.retry")
                }
            case .failed(.keychainUnavailable):
                VStack(spacing: 16) {
                    Spacer(minLength: 0)
                    Image(systemName: "lock.circle")
                        .font(.system(size: 44))
                        .foregroundStyle(.secondary)
                        .accessibilityHidden(true)
                    Text(L10n.keychainLockedTitle.key)
                        .font(.title3.weight(.semibold))
                        .multilineTextAlignment(.center)
                        .accessibilityIdentifier("keychain.locked.title")
                    Text(L10n.keychainLockedBody.key)
                        .font(.body)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                    Text(L10n.errorKeychainUnavailable.key)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .accessibilityIdentifier("keychain.locked.error")
                    Button(L10n.commonRetry.key) {
                        Task { await app.retryLaunch() }
                    }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .accessibilityIdentifier("keychain.retry")
                    Spacer(minLength: 0)
                }
                .padding(24)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .accessibilityElement(children: .contain)
                .accessibilityIdentifier("keychain.locked")
            case .failed(let code):
                ContentUnavailableView {
                    Label(L10n.commonError.key, systemImage: "exclamationmark.triangle")
                } description: {
                    Text(L10nKey.errorKey(for: code).key)
                } actions: {
                    Button(L10n.commonRetry.key) {
                        Task { await app.retryLaunch() }
                    }
                    .accessibilityIdentifier("conversation.retry")
                }
            case .incompatible(_, let version):
                ContentUnavailableView {
                    Label(L10n.conversationIncompatibleTitle.key, systemImage: "arrow.triangle.2.circlepath")
                } description: {
                    VStack(spacing: 8) {
                        Text(incompatibleDescription(version: version))
                            .accessibilityIdentifier("conversation.incompatible")
                        Text(L10n.conversationUpgradeGuidance.key)
                            .font(.footnote)
                            .accessibilityIdentifier("conversation.incompatibleGuidance")
                    }
                } actions: {
                    Button(L10n.commonRetry.key) {
                        Task {
                            await app.bootstrapConversation()
                        }
                    }
                    .accessibilityIdentifier("conversation.incompatibleRetry")
                }
            case .upgradeRequired(let code):
                ContentUnavailableView {
                    Label(L10n.conversationIncompatibleTitle.key, systemImage: "arrow.up.circle")
                } description: {
                    VStack(spacing: 8) {
                        Text(L10nKey.errorKey(for: code).key)
                            .accessibilityIdentifier("conversation.upgradeRequired")
                        Text(L10n.conversationUpgradeGuidance.key)
                            .font(.footnote)
                            .accessibilityIdentifier("conversation.upgradeGuidance")
                    }
                } actions: {
                    Button(L10n.commonRetry.key) {
                        Task {
                            await app.bootstrapConversation()
                        }
                    }
                    .accessibilityIdentifier("conversation.upgradeRetry")
                }
            case .ready:
                // A ready root always has a conversation; keep a typed fallback
                // instead of showing a blank page if they ever diverge.
                ProgressView()
                    .accessibilityIdentifier("conversation.restoring")
            }
        }
        .navigationTitle(L10nKey.appName.key)
        .inlineNavigationTitle()
    }

    private func incompatibleDescription(version: String) -> String {
        if ProtocolCompatibility.isLegacyPeer(version) {
            return L10n.conversationIncompatibleLegacyBody.string
        }
        return String(
            format: L10n.conversationIncompatibleBody.string,
            version,
            ProtocolCompatibility.supportedDisplay
        )
    }
}

private extension AppScenePhase {
    init(_ phase: ScenePhase) {
        switch phase {
        case .active:
            self = .active
        case .inactive:
            self = .inactive
        case .background:
            self = .background
        @unknown default:
            self = .active
        }
    }
}

#Preview {
    Text(verbatim: "WhaleRein")
}
