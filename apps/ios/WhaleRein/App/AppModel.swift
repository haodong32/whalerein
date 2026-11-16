// Composition root. Builds the single SessionService, owns the conversation as
// the root of the paired experience, and exposes the Host/session switcher and
// the settings/diagnostics sheets. There is no separate Host or session page.
//
// The root also owns app lifecycle behavior: a foreground/background transition
// flushes drafts, stops or restarts the watch, and shows a privacy shield while
// the app is not active. Remote revocation and preference/Core disagreement
// have explicit recovery routes instead of a dead conversation page.

import Foundation
import Observation

enum ConversationEmptyReason: Equatable, Sendable {
    case noHosts
    case noSessions
}

enum ConversationRootState: Equatable, Sendable {
    case restoring
    case ready
    case empty(ConversationEmptyReason)
    case failed(AppErrorCode)
    /// The authorized Host reports a protocol line this client cannot use.
    case incompatible(hostId: String, version: String)
    /// Core reported a non-retryable upgrade condition while loading. This is
    /// distinct from a generic unavailable error and never offers a blind retry
    /// as the primary route.
    case upgradeRequired(AppErrorCode)
}

enum AppScenePhase: Equatable, Sendable {
    case active
    case inactive
    case background
}

struct AppResumeMarker: Equatable, Sendable {
    let recordedAt: Date
    let hostId: String?
    let sessionId: String?
}

@MainActor
@Observable
final class AppModel {
    let options: LaunchOptions
    let service: any SessionService
    let preferences: PreferencesStore
    let dataDirectory: URL

    let pairing: PairingModel
    let hostList: HostListModel
    let sessionList: SessionListModel
    let diagnostics: DiagnosticsModel
    let settings: SettingsModel

    private(set) var conversation: ConversationModel?
    private(set) var isPaired: Bool
    private(set) var rootState: ConversationRootState = .restoring
    private(set) var selectedHostId: String?
    private(set) var isOfflineCached = false
    /// True while the app is not active. The root renders an opaque shield so
    /// the app-switcher snapshot never contains conversation content.
    private(set) var isPrivacyShieldVisible = false
    /// Present after a background transition; used by tests and diagnostics to
    /// distinguish a resume from a cold launch.
    private(set) var resumeMarker: AppResumeMarker?
    /// The device was revoked remotely. The root blocks conversation content
    /// and offers re-pair/forget instead of a generic error or stale transcript.
    private(set) var isRevoked = false

    var showSettings = false
    var showDiagnostics = false
    var showInvitationSheet = false
    var showSwitcher = false

    private var didBootstrap = false
    private var isBootstrapping = false
    private var isInBackground = false

    init(
        service: any SessionService,
        preferences: PreferencesStore,
        dataDirectory: URL,
        options: LaunchOptions
    ) {
        self.service = service
        self.preferences = preferences
        self.dataDirectory = dataDirectory
        self.options = options
        self.isPaired = preferences.value.paired
        self.selectedHostId = preferences.value.lastHostId

        let pairing = PairingModel(service: service)
        self.pairing = pairing
        self.hostList = HostListModel(service: service, preferences: preferences)
        self.sessionList = SessionListModel(service: service, preferences: preferences)
        self.diagnostics = DiagnosticsModel(service: service)
        self.settings = SettingsModel(
            service: service,
            preferences: preferences,
            dataDirectory: dataDirectory,
            isStaging: options.staging,
            isFixtureBacked: service is FixtureSessionService,
            onDeviceReset: {}
        )

        pairing.onPaired = { [weak self] hostId in
            self?.finishPairing(hostId: hostId)
        }
        self.settings.onDeviceReset = { [weak self] in
            self?.deviceDidReset()
        }
    }

    var isUsingFixtures: Bool { service is FixtureSessionService }

    /// True when the stored preferences were discarded and the app is running
    /// on safe defaults. The app then shows the invitation flow unless Core
    /// still has a usable pairing.
    var preferenceRecovery: PreferencesStore.Recovery? { preferences.recovery }

    // MARK: - Launch restore

    /// Single launch entry point for both paired and unpaired preferences. If
    /// preferences lost the paired flag but Core still has authorized Hosts, the
    /// app adopts the Core pairing instead of stranding the user on onboarding.
    func launchRestore() async {
        guard !isRevoked else { return }
        if !isPaired {
            await adoptExistingCorePairing()
        }
        guard isPaired else { return }
        await bootstrapConversation()
    }

    /// Retries a recoverable launch failure (for example an unlocked device
    /// after a locked-Keychain state). The attempt is user- or scene-driven, never
    /// a timer loop.
    func retryLaunch() async {
        guard !isRevoked else { return }
        didBootstrap = false
        isBootstrapping = false
        if !isPaired {
            await adoptExistingCorePairing()
        }
        guard isPaired else { return }
        await bootstrapConversation()
    }

    /// Restores the conversation root after launch or pairing. Idempotent while
    /// a restore is in flight or after a conversation has been opened.
    func bootstrapConversation() async {
        guard isPaired, conversation == nil, !didBootstrap, !isBootstrapping else { return }
        isBootstrapping = true
        rootState = .restoring
        defer { isBootstrapping = false }

        await hostList.load()
        guard isPaired else { return }
        if case .failed(let code) = hostList.loadState {
            handleBootFailure(code)
            return
        }

        let hosts = hostList.hosts
        guard !hosts.isEmpty else {
            rootState = .empty(.noHosts)
            return
        }

        let host = chooseHost(from: hosts)
        if let version = ProtocolCompatibility.incompatibility(for: host.protocolVersion) {
            selectedHostId = host.hostId
            rootState = .incompatible(hostId: host.hostId, version: version)
            return
        }
        selectedHostId = host.hostId
        hostList.select(host.hostId)

        await sessionList.load(hostId: host.hostId)
        guard isPaired else { return }
        if case .failed(let code) = sessionList.loadState {
            handleBootFailure(code)
            return
        }

        let sessions = sessionList.sessions
        guard !sessions.isEmpty else {
            rootState = .empty(.noSessions)
            return
        }

        let session = restoredSession(from: sessions)
        didBootstrap = true
        openConversation(hostId: host.hostId, session: session)
    }

    private func adoptExistingCorePairing() async {
        do {
            let hosts = try await service.hosts()
            guard let first = hosts.first else { return }
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

    private func chooseHost(from hosts: [HostSummary]) -> HostSummary {
        if let remembered = preferences.value.lastHostId,
           let host = hosts.first(where: { $0.hostId == remembered }) {
            return host
        }
        return hosts.first(where: \.online) ?? hosts[0]
    }

    private func restoredSession(from sessions: [SessionSummary]) -> SessionSummary {
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
    }

    /// Reconnects the visible session with a fresh watch instead of leaving a
    /// half-open stream, then refreshes the switcher and cached banner.
    func becomeActive() async {
        isPrivacyShieldVisible = false
        guard isInBackground else { return }
        isInBackground = false
        guard isPaired, !isRevoked else { return }

        if case .failed(.keychainUnavailable) = rootState {
            await retryLaunch()
        }
        if let conversation {
            await conversation.resumeAfterForeground()
        } else if case .empty = rootState {
            didBootstrap = false
            await bootstrapConversation()
        }
        await refreshSwitcher()
        updateCachedBanner()
    }

    // MARK: - Switcher

    func showSessionSwitcher() {
        guard isPaired else { return }
        showSwitcher = true
    }

    /// Refreshes the Host list and the selected Host's sessions. Never changes
    /// the open conversation.
    func refreshSwitcher() async {
        guard isPaired else { return }
        await hostList.load()
        guard isPaired else { return }
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

    // MARK: - Conversation lifecycle

    /// Single entry point for opening a conversation. It replaces the root
    /// conversation, persists the last selection and dismisses the switcher.
    func openConversation(hostId: String, session: SessionSummary) {
        conversation?.close()
        let model = ConversationModel(
            hostId: hostId,
            sessionId: session.sessionId,
            sessionTitle: session.title,
            service: service,
            onFatalError: { [weak self] code in
                self?.handleConversationFatal(code)
            }
        )
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
        showSwitcher = false
        rootState = .ready
        Task { await model.start() }
    }

    private func updateCachedBanner() {
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
        showInvitationSheet = false
        showSwitcher = false
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
        if isPaired {
            showInvitationSheet = true
        }
        await pairing.handleOpenURL(url)
    }

    /// Entry point for pairing an additional Host while already paired, or for
    /// re-pairing from a settings/revocation screen. Pairing state is reset so
    /// a stale invitation attempt cannot leak into the new flow.
    func openManualPairing() {
        pairing.reset()
        showInvitationSheet = true
    }

    func dismissInvitationSheet() {
        showInvitationSheet = false
        if !isPaired {
            pairing.reset()
        }
    }

    // MARK: - Revocation and preference recovery

    /// Remote revocation blocks the conversation and offers re-pair/forget.
    /// Local preferences are kept until the user chooses, so "Forget" can still
    /// clear the local device identity.
    func enterRevoked() {
        conversation?.close()
        conversation = nil
        isRevoked = true
        isPrivacyShieldVisible = false
        showSettings = false
        showDiagnostics = false
        showSwitcher = false
        showInvitationSheet = false
        selectedHostId = nil
        isOfflineCached = false
        resumeMarker = nil
        hostList.reset()
        sessionList.reset()
        rootState = .restoring
    }

    /// Leaves the blocked revoked state and returns to the invitation flow.
    func beginRepairAfterRevocation() {
        isRevoked = false
        isPaired = false
        preferences.update {
            $0.paired = false
            $0.lastHostId = nil
            $0.lastSessionId = nil
            $0.lastSessionTitle = nil
        }
        resetLocalNavigation()
    }

    /// Clears the local device identity after a remote revocation. The Core call
    /// is best effort because the Host may already have forgotten this device.
    func forgetAfterRevocation() async {
        try? await service.forgetDevice()
        beginRepairAfterRevocation()
    }

    /// Preferences and Core disagreed (for example after restoring a backup).
    /// Falling back to onboarding is the safe route: pairing again is always
    /// available, while a paired root with no Core identity would dead-end.
    func adoptOnboardingFallback() {
        conversation?.close()
        conversation = nil
        isPaired = false
        preferences.update {
            $0.paired = false
            $0.lastHostId = nil
            $0.lastSessionId = nil
            $0.lastSessionTitle = nil
        }
        resetLocalNavigation()
    }

    private func resetLocalNavigation() {
        isRevoked = false
        showSettings = false
        showDiagnostics = false
        showSwitcher = false
        showInvitationSheet = false
        selectedHostId = nil
        isOfflineCached = false
        resumeMarker = nil
        didBootstrap = false
        hostList.reset()
        sessionList.reset()
        pairing.reset()
        rootState = .restoring
    }

    private func handleBootFailure(_ code: AppErrorCode) {
        switch code {
        case .revoked:
            enterRevoked()
        case .notPaired:
            adoptOnboardingFallback()
        case .protocolVersionIncompatible, .unsupportedCapability:
            rootState = .upgradeRequired(code)
        default:
            rootState = .failed(code)
        }
    }

    private func handleSwitcherFailure(_ code: AppErrorCode) {
        switch code {
        case .revoked:
            enterRevoked()
        case .notPaired:
            adoptOnboardingFallback()
        case .protocolVersionIncompatible, .unsupportedCapability:
            conversation?.close()
            conversation = nil
            isOfflineCached = false
            rootState = .upgradeRequired(code)
        default:
            break
        }
    }

    private func handleConversationFatal(_ code: AppErrorCode) {
        switch code {
        case .revoked:
            enterRevoked()
        case .notPaired:
            adoptOnboardingFallback()
        default:
            break
        }
    }

    // MARK: - Reset

    func deviceDidReset() {
        conversation?.close()
        conversation = nil
        isPaired = false
        isRevoked = false
        showSwitcher = false
        selectedHostId = nil
        isOfflineCached = false
        resumeMarker = nil
        didBootstrap = false
        hostList.reset()
        sessionList.reset()
        pairing.reset()
        showInvitationSheet = false
        showSettings = false
        showDiagnostics = false
        rootState = .restoring
    }
}
