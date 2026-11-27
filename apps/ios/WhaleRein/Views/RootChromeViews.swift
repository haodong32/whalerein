// Root-level blocking surfaces: the privacy shield, the revoked device screen
// and the placeholder shown while a conversation root is resolving.

import SwiftUI

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
    let app: AppShellModel
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

struct ConversationRootPlaceholderView: View {
    let app: AppShellModel

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

extension AppScenePhase {
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

// MARK: - Sheet privacy shield

/// One shell-owned flag shared with every presented surface. The root ZStack
/// cannot cover a sheet because a sheet is presented in its own window, so each
/// sheet content reads this value and renders the same opaque shield.
private struct PrivacyShieldVisibleKey: EnvironmentKey {
    static let defaultValue = false
}

extension EnvironmentValues {
    var privacyShieldVisible: Bool {
        get { self[PrivacyShieldVisibleKey.self] }
        set { self[PrivacyShieldVisibleKey.self] = newValue }
    }
}

/// Covers one presented surface with the root privacy shield while the scene is
/// inactive, backgrounded or waiting for an authorized resume.
struct PrivacyShieldOverlay: ViewModifier {
    let isVisible: Bool

    func body(content: Content) -> some View {
        content.overlay {
            if isVisible {
                PrivacyShieldView()
                    .transition(.opacity)
            }
        }
    }
}

/// Environment-driven variant for nested sheets presented from a surface that
/// does not own the shell model (the session switcher, Settings and pairing).
private struct EnvironmentPrivacyShieldOverlay: ViewModifier {
    @Environment(\.privacyShieldVisible) private var isVisible

    func body(content: Content) -> some View {
        content.privacyShield(isVisible)
    }
}

extension View {
    /// Shields an individual presentation with the shell's current flag.
    func privacyShield(_ isVisible: Bool) -> some View {
        modifier(PrivacyShieldOverlay(isVisible: isVisible))
    }

    /// Shields a nested sheet from the shell flag carried in the environment.
    func privacyShieldFromEnvironment() -> some View {
        modifier(EnvironmentPrivacyShieldOverlay())
    }
}

#Preview {
    Text(verbatim: "WhaleRein")
}
