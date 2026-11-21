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

@MainActor
@Observable
final class AppShellModel {
    let options: LaunchOptions
    private(set) var service: any SessionService
    private(set) var preferences: PreferencesStore
    private(set) var dataDirectory: URL
    /// Capabilities decided once for this runtime. Feature models are built
    /// from this set instead of probing the service with `as?`.
    private(set) var featureSet: FeatureSet

    private(set) var pairing: PairingModel
    private(set) var hostList: HostListModel
    private(set) var sessionList: SessionListModel
    private(set) var diagnostics: DiagnosticsModel
    private(set) var settings: SettingsModel
    /// Shared retained-operation list and the one coalesced visible refresh.
    private(set) var operationRecovery: OperationRecoveryModel?
    /// Cloud sign-in/directory presentation for the active instance. `nil` for
    /// standalone instances and unavailable services.
    private(set) var cloudSignIn: CloudSignInModel?
    /// Host-scoped create/details/fork/model presentation. It reads through the
    /// active service adapter and keeps no session cache of its own.
    private(set) var sessionManagement: SessionManagementModel
    /// Selected-instance status, default computer and Cloud app sign-ins.
    private(set) var accountSettings: AccountSettingsModel?

    private(set) var conversation: ConversationModel?
    /// True while the app is not active. The root renders an opaque shield so
    /// the app-switcher snapshot never contains conversation content.
    /// Present after a background transition; used by tests and diagnostics to
    /// distinguish a resume from a cold launch.
    /// The device was revoked remotely. The root blocks conversation content
    /// and offers re-pair/forget instead of a generic error or stale transcript.
    /// After an explicit forget/revoke, return to the invitation flow even for
    /// a Cloud instance so the local identity reset is visible and re-pairing
    /// (or Cloud onboarding) starts from an explicit choice.

    /// Authoritative configuration document. `nil` for the direct unit-test
    /// composition that only owns one service.
    private(set) var configuration: AppConfigurationStore?
    /// Instance the current runtime was built for.
    private(set) var activeInstance: AppInstance?
    /// Monotonic activation fence. Every callback and every in-flight launch
    /// captures this value and drops its result after a newer activation.
    private(set) var activationGeneration: UInt64 = 0

    /// Navigation is owned by the router; the shell only sends intents. The
    /// shell retains it because the router never refers back to the shell.
    var router: AppRouter?

    func present(_ route: AppRoute) { router?.present(route) }
    func dismissSheet() { router?.dismiss() }

    // Read-only navigation projections kept for tests and the transition; the
    // router owns the state.
    var showSettings: Bool {
        get { router?.presented == .settings }
        set { newValue ? present(.settings) : dismissSheet() }
    }
    var showDiagnostics: Bool {
        get { router?.presented == .diagnostics }
        set { newValue ? present(.diagnostics) : dismissSheet() }
    }
    var showInvitationSheet: Bool {
        get { router?.presented == .invitation }
        set { newValue ? present(.invitation) : dismissSheet() }
    }
    var showSwitcher: Bool {
        get { router?.presented == .sessionSwitcher }
        set { newValue ? present(.sessionSwitcher) : dismissSheet() }
    }

    private let serviceFactory: (@MainActor (AppInstance) throws -> any SessionService)?
    private let preferencesFactory: (@MainActor (AppInstance) -> PreferencesStore)?
    private var didBootstrap = false
    private var isBootstrapping = false
    private var isInBackground = false

    init(
        service: any SessionService,
        preferences: PreferencesStore,
        dataDirectory: URL,
        options: LaunchOptions,
        configuration: AppConfigurationStore? = nil,
        activeInstance: AppInstance? = nil,
        serviceFactory: (@MainActor (AppInstance) throws -> any SessionService)? = nil,
        preferencesFactory: (@MainActor (AppInstance) -> PreferencesStore)? = nil
    ) {
        self.service = service
        self.preferences = preferences
        self.dataDirectory = dataDirectory
        self.options = options
        self.configuration = configuration
        self.activeInstance = activeInstance
        self.serviceFactory = serviceFactory
        self.preferencesFactory = preferencesFactory

        let models = Self.makeModels(
            service: service,
            preferences: preferences,
            dataDirectory: dataDirectory,
            options: options,
            isStaging: options.staging || activeInstance?.environment == .staging
        )
        self.pairing = models.pairing
        self.hostList = models.hostList
        self.sessionList = models.sessionList
        self.diagnostics = models.diagnostics
        self.settings = models.settings
        self.sessionManagement = Self.makeManagementModel(service: service)
        let available = !(service is UnavailableSessionService)
        self.featureSet = FeatureSet.resolved(
            instance: activeInstance,
            serviceIsAvailable: available
        )
        self.cloudSignIn = featureSet.cloudAccount
            ? makeCloudSignInModel(service: service, instance: activeInstance)
            : nil
        self.operationRecovery = featureSet.operationRecovery
            ? Self.makeOperationRecovery(
                service: service,
                instanceId: activeInstance?.id ?? service.instanceId
            )
            : nil
        self.accountSettings = featureSet.sessionManagement
            ? makeAccountSettingsModel(service: service, instance: activeInstance)
            : nil
        self.shell.isPaired = preferences.value.paired
        self.shell.selectedHostId = preferences.value.lastHostId
        installCallbacks()
    }

    private static func makeModels(
        service: any SessionService,
        preferences: PreferencesStore,
        dataDirectory: URL,
        options: LaunchOptions,
        isStaging: Bool
    ) -> (
        pairing: PairingModel,
        hostList: HostListModel,
        sessionList: SessionListModel,
        diagnostics: DiagnosticsModel,
        settings: SettingsModel
    ) {
        (
            PairingModel(service: service),
            HostListModel(service: service, preferences: preferences),
            SessionListModel(service: service, preferences: preferences),
            DiagnosticsModel(
                service: service,
                stagingDirectory: Self.diagnosticsStagingDirectory(dataDirectory)
            ),
            SettingsModel(
                service: service,
                preferences: preferences,
                dataDirectory: dataDirectory,
                isStaging: isStaging,
                isFixtureBacked: service is FixtureSessionService,
                onDeviceReset: {}
            )
        )
    }

    /// Builds the management adapter from the active service. A service that
    /// does not implement the adapter (a stub or an older composition) gets an
    /// explicit unavailable implementation instead of invented data.
    private static func makeManagementModel(service: any SessionService) -> SessionManagementModel {
        SessionManagementModel(
            service: (service as? any SessionManagementService)
                ?? UnavailableSessionManagementService()
        )
    }

    /// Pairing and settings callbacks are fenced by the activation that created
    /// them, so a late auth result can never target the replacement runtime.
    private func installCallbacks() {
        let generation = activationGeneration
        pairing.onPaired = { [weak self] hostId in
            guard let self, self.isCurrentActivation(generation) else { return }
            self.finishPairing(hostId: hostId)
        }
        settings.onDeviceReset = { [weak self] in
            guard let self, self.isCurrentActivation(generation) else { return }
            self.deviceDidReset()
        }
    }

    private func isCurrentActivation(_ generation: UInt64) -> Bool {
        generation == activationGeneration
    }

    /// One shell state value. The properties below are read/write projections so
    /// the migration can move call sites without changing behaviour.
    private(set) var shell = AppShellState()

    var isPaired: Bool { get { shell.isPaired } set { shell.isPaired = newValue } }
    var rootState: ConversationRootState {
        get { shell.rootState }
        set { shell.rootState = newValue }
    }
    var selectedHostId: String? {
        get { shell.selectedHostId }
        set { shell.selectedHostId = newValue }
    }
    var isOfflineCached: Bool {
        get { shell.isOfflineCached }
        set { shell.isOfflineCached = newValue }
    }
    var isPrivacyShieldVisible: Bool {
        get { shell.isPrivacyShieldVisible }
        set { shell.isPrivacyShieldVisible = newValue }
    }
    var isRevoked: Bool { get { shell.isRevoked } set { shell.isRevoked = newValue } }
    var prefersInvitationAfterDeviceReset: Bool {
        get { shell.prefersInvitationAfterDeviceReset }
        set { shell.prefersInvitationAfterDeviceReset = newValue }
    }
    var isSwitchingInstance: Bool {
        get { shell.isSwitchingInstance }
        set { shell.isSwitchingInstance = newValue }
    }
    var instanceErrorKey: L10nKey? {
        get { shell.instanceErrorKey }
        set { shell.instanceErrorKey = newValue }
    }
    var resumeMarker: AppResumeMarker? {
        get { shell.resumeMarker }
        set { shell.resumeMarker = newValue }
    }

    var isUsingFixtures: Bool { service is FixtureSessionService }

    /// True when the stored preferences were discarded and the app is running
    /// on safe defaults. The app then shows the invitation flow unless Core
    /// still has a usable pairing.
    var preferenceRecovery: PreferencesStore.Recovery? { preferences.recovery }

    /// App-section reasoning visibility. The history feature reads and writes
    /// this through the same authoritative document; no reasoning UI lives here.
    var reasoningVisible: Bool { configuration?.configuration.reasoningVisible ?? false }

    @discardableResult
    func setReasoningVisible(_ visible: Bool) -> AppErrorCode? {
        guard let configuration else { return .configuration }
        do {
            try configuration.setReasoningVisible(visible)
            return nil
        } catch {
            return reportInstanceError(error)
        }
    }

    // MARK: - Instance activation

    /// Selects one configured instance. The binding prepares and commits the
    /// destination document first; the old runtime is replaced only after the
    /// new service exists. Any failure keeps the previous service and selection.
    @discardableResult
    func switchInstance(id: String, force: Bool = false) async -> AppErrorCode? {
        guard let configuration, let serviceFactory else { return .configuration }
        guard !isSwitchingInstance else { return .conflict }
        guard force || id != activeInstance?.id else { return nil }
        isSwitchingInstance = true
        defer { isSwitchingInstance = false }

        // Persist the outgoing draft before leaving its instance.
        await conversation?.suspendForBackground()
        let previousInstance = activeInstance
        let previousService = service
        let previousPreferences = preferences

        do {
            try configuration.activate(id: id)
        } catch {
            return reportInstanceError(error)
        }
        let target = configuration.activeInstance
        let nextService: any SessionService
        do {
            nextService = try serviceFactory(target)
        } catch {
            // The document committed before the runtime existed; restore the
            // previous selection rather than leaving startup and runtime split.
            if let previousInstance {
                try? configuration.activate(id: previousInstance.id)
            }
            return reportInstanceError(error)
        }

        previousService.stopWatch()
        conversation?.close()
        conversation = nil
        let nextPreferences = preferencesFactory?(target) ?? previousPreferences
        replaceRuntime(
            service: nextService,
            preferences: nextPreferences,
            dataDirectory: target.dataDirectory,
            instance: target
        )
        await launchRestore()
        return nil
    }

    /// Handles one selector entry, creating the debug-only Staging instance on
    /// first use before activating it.
    @discardableResult
    func switchToService(_ option: AppServiceOption) async -> AppErrorCode? {
        guard let configuration else { return .configuration }
        switch option.kind {
        case .existing(let instanceID):
            return await switchInstance(id: instanceID)
        case .createStaging:
            let input = AppInstanceInput(
                id: "cloud-staging",
                service: .cloud,
                environment: .staging,
                dataDirectory: "instances/cloud-staging/data",
                standaloneCAPEM: nil
            )
            do {
                try configuration.save(input)
            } catch {
                return reportInstanceError(error)
            }
            return await switchInstance(id: "cloud-staging")
        case .createLocal:
            // Local needs an imported mkcert root bundle; the selector collects
            // it before calling `enableLocal`. Creating the instance here would
            // fail Core validation with a generic configuration error.
            return reportInstanceError(SessionServiceError(.configuration))
        }
    }

    /// Imports the mkcert root bundle for the local Cloud instance and selects
    /// it. An invalid bundle leaves the previous document and trust unchanged.
    @discardableResult
    func enableLocal(pem: String) async -> AppErrorCode? {
        guard let configuration else { return .configuration }
        do {
            try configuration.enableLocal(pem: pem)
        } catch {
            return reportInstanceError(error)
        }
        // Creating the instance is an explicit selection, not a refresh.
        return await switchInstance(id: "cloud-local", force: true)
    }

    @discardableResult
    func removeLocalCA() async -> AppErrorCode? {
        guard let configuration,
              let local = configuration.instances.first(where: { $0.isLocal }) else {
            return nil
        }
        // A local instance cannot open without its CA, so it is removed rather
        // than left half-configured. Leave it first because the active instance
        // cannot be deleted.
        if activeInstance?.id == local.id {
            guard let fallback = configuration.instances.first(where: { $0.isCloudProduction }) else {
                return reportInstanceError(SessionServiceError(.configuration))
            }
            if let code = await switchInstance(id: fallback.id) {
                return code
            }
        }
        do {
            try configuration.removeCloudCA(instanceID: local.id)
        } catch {
            return reportInstanceError(error)
        }
        return nil
    }

    /// Imports a validated private CA for the Standalone instance. An invalid
    /// PEM leaves the previous document and trust unchanged.
    @discardableResult
    func importStandaloneCA(_ pem: String) async -> AppErrorCode? {
        guard let configuration,
              let standalone = configuration.instances.first(where: { $0.isStandalone }) else {
            return reportInstanceError(SessionServiceError(.configuration))
        }
        do {
            try configuration.importCA(instanceID: standalone.id, pem: pem)
        } catch {
            return reportInstanceError(error)
        }
        return await reselectIfActive(id: standalone.id)
    }

    /// Rebuild the runtime for a CA change only when that instance is active.
    private func reselectIfActive(id: String) async -> AppErrorCode? {
        guard activeInstance?.id == id else { return nil }
        return await switchInstance(id: id, force: true)
    }

    @discardableResult
    func removeStandaloneCA() async -> AppErrorCode? {
        guard let configuration,
              let standalone = configuration.instances.first(where: { $0.isStandalone }) else {
            return reportInstanceError(SessionServiceError(.configuration))
        }
        do {
            try configuration.removeCA(instanceID: standalone.id)
        } catch {
            return reportInstanceError(error)
        }
        return await reselectIfActive(id: standalone.id)
    }

    private func replaceRuntime(
        service: any SessionService,
        preferences: PreferencesStore,
        dataDirectory: URL,
        instance: AppInstance?
    ) {
        activationGeneration &+= 1
        self.service = service
        self.preferences = preferences
        self.dataDirectory = dataDirectory
        self.activeInstance = instance
        self.isPaired = preferences.value.paired
        self.selectedHostId = preferences.value.lastHostId
        self.isRevoked = false
        self.prefersInvitationAfterDeviceReset = false
        self.isOfflineCached = false
        self.isPrivacyShieldVisible = false
        self.resumeMarker = nil
        self.rootState = .restoring
        self.didBootstrap = false
        self.isBootstrapping = false
        self.dismissSheet()
        self.dismissSheet()
        self.hostList = HostListModel(service: service, preferences: preferences)
        self.sessionList = SessionListModel(service: service, preferences: preferences)
        self.diagnostics = DiagnosticsModel(
            service: service,
            stagingDirectory: Self.diagnosticsStagingDirectory(dataDirectory)
        )
        self.pairing = PairingModel(service: service)
        self.settings = SettingsModel(
            service: service,
            preferences: preferences,
            dataDirectory: dataDirectory,
            isStaging: options.staging || instance?.environment == .staging,
            isFixtureBacked: service is FixtureSessionService,
            onDeviceReset: {}
        )
        self.sessionManagement = Self.makeManagementModel(service: service)
        let available = !(service is UnavailableSessionService)
        self.featureSet = FeatureSet.resolved(instance: instance, serviceIsAvailable: available)
        self.operationRecovery = featureSet.operationRecovery
            ? Self.makeOperationRecovery(
                service: service,
                instanceId: instance?.id ?? service.instanceId
            )
            : nil
        self.cloudSignIn = featureSet.cloudAccount
            ? makeCloudSignInModel(service: service, instance: instance)
            : nil
        self.accountSettings = featureSet.sessionManagement
            ? makeAccountSettingsModel(service: service, instance: instance)
            : nil
        installCallbacks()
    }

    private static func makeOperationRecovery(
        service: any SessionService,
        instanceId: String
    ) -> OperationRecoveryModel? {
        guard let recovery = service as? any OperationRecoveryService else { return nil }
        return OperationRecoveryModel(service: recovery, instanceId: instanceId)
    }

    /// Redacted diagnostic exports go to an app-owned staging directory for the
    /// selected instance, not a process-wide temporary drop box.
    static func diagnosticsStagingDirectory(_ dataDirectory: URL) -> URL {
        dataDirectory
            .appendingPathComponent("diagnostics", isDirectory: true)
    }

    /// Fixtures replace the system sheet with a deterministic callback so UI
    /// tests can cover the app-side sign-in flow. Live builds always use the
    /// real authentication session.
    private func authenticator(options: LaunchOptions) -> any CloudWebAuthenticator {
        if case .fixtures = options.serviceMode {
            if options.cloudSystemSheet {
                return WebAuthenticationSessionRunner()
            }
            return FixtureCloudAuthenticator(cancels: options.cloudCancelSheet)
        }
        return WebAuthenticationSessionRunner()
    }

    /// Builds the status/default-computer/app-sign-in model for the activation
    /// that owns `service`. Revoking the current app session fences through the
    /// same signed-out path as Cloud sign-out.
    private func makeAccountSettingsModel(
        service: any SessionService,
        instance: AppInstance?
    ) -> AccountSettingsModel {
        let generation = activationGeneration
        let instanceId = instance?.id ?? service.instanceId
        let model = AccountSettingsModel(service: service)
        model.onSignedOut = { [weak self] in
            guard let self,
                  self.activationGeneration == generation,
                  (self.activeInstance?.id ?? self.service.instanceId) == instanceId else {
                return
            }
            self.cloudSignIn?.markSignedOutLocally(pending: true)
            self.handleCloudSignedOut()
        }
        return model
    }

    /// Builds the Cloud model for the activation that owns `service`. Callbacks
    /// capture both the instance ID and activation generation, so a late sheet
    /// or deep link from a replaced runtime can never mutate the new one.
    private func makeCloudSignInModel(
        service: any SessionService,
        instance: AppInstance?
    ) -> CloudSignInModel? {
        // A standalone instance has no Cloud account surface, even though the
        // fixture type also implements the Cloud protocol.
        guard instance?.service == .cloud,
              let cloud = service as? any CloudAccountService else { return nil }
        let generation = activationGeneration
        let instanceId = instance?.id ?? service.instanceId
        let model = CloudSignInModel(
            service: cloud,
            fence: CloudSignInFence(
                instanceId: instanceId,
                activationGeneration: generation
            ),
            authenticator: authenticator(options: options),
            fenceIsCurrent: { [weak self] in
                guard let self else { return false }
                return self.activationGeneration == generation
                    && (self.activeInstance?.id ?? self.service.instanceId) == instanceId
            }
        )
        model.onOpenHost = { [weak self] hostId in
            guard let self, self.activationGeneration == generation else { return }
            Task { @MainActor in self.finishPairing(hostId: hostId) }
        }
        model.onSignedOut = { [weak self] in
            guard let self, self.activationGeneration == generation else { return }
            self.handleCloudSignedOut()
        }
        return model
    }

    @discardableResult
    private func reportInstanceError(_ error: Error) -> AppErrorCode {
        let code = (error as? SessionServiceError)?.code ?? .internalError
        instanceErrorKey = L10nKey.errorKey(for: code)
        return code
    }

    // MARK: - Launch restore

    /// Single launch entry point for both paired and unpaired preferences. If
    /// preferences lost the paired flag but Core still has authorized Hosts, the
    /// app adopts the Core pairing instead of stranding the user on onboarding.
    func launchRestore() async {
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
        await refreshVisibleOperationResults()
    }

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
            onFatalError: { [weak self] code in
                guard let self, self.isCurrentActivation(generation) else { return }
                self.handleConversationFatal(code)
            },
            onReconnected: { [weak self] in
                guard let self, self.isCurrentActivation(generation) else { return }
                Task { await self.refreshVisibleOperationResults() }
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
        dismissSheet()
        rootState = .ready
        Task { await model.start() }
    }

    /// One read-only refresh for the visible instance/Host/session. Automatic
    /// lookup never sends a message or resumes pairing; other sessions stay
    /// available through Settings for manual recovery.
    private func refreshVisibleOperationResults() async {
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
        // must never be offered to the self-hosted invitation parser.
        if CloudSignInModel.isCloudCallback(url) {
            await cloudSignIn?.receiveCallback(url)
            return
        }
        if isPaired {
            present(.invitation)
        }
        await pairing.handleOpenURL(url)
    }

    /// Local Cloud sign-out from Settings. Core keeps a scoped pending remote
    /// revocation when Cloud is unreachable and retries it later.
    func signOutCloud() async {
        await cloudSignIn?.signOut()
    }

    /// Leaves the Cloud-authorized root after a local sign-out or a confirmed
    /// remote revocation. The device identity is preserved.
    private func handleCloudSignedOut() {
        conversation?.close()
        conversation = nil
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

    // MARK: - Revocation and preference recovery

    /// Remote revocation blocks the conversation and offers re-pair/forget.
    /// Local preferences are kept until the user chooses, so "Forget" can still
    /// clear the local device identity.
    func enterRevoked() {
        conversation?.close()
        conversation = nil
        isRevoked = true
        isPrivacyShieldVisible = false
        dismissSheet()
        dismissSheet()
        dismissSheet()
        dismissSheet()
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
        dismissSheet()
        dismissSheet()
        dismissSheet()
        dismissSheet()
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
        prefersInvitationAfterDeviceReset = true
        dismissSheet()
        selectedHostId = nil
        isOfflineCached = false
        resumeMarker = nil
        didBootstrap = false
        hostList.reset()
        sessionList.reset()
        pairing.reset()
        dismissSheet()
        dismissSheet()
        dismissSheet()
        rootState = .restoring
    }
}
