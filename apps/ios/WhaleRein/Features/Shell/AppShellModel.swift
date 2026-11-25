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
    var service: any SessionService
    var preferences: PreferencesStore
    var dataDirectory: URL
    /// Capabilities decided once for this runtime. Feature models are built
    /// from this set instead of probing the service with `as?`.
    var featureSet: FeatureSet

    var pairing: PairingModel
    var hostList: HostListModel
    /// One authoritative Host connection for every screen.
    var connectivity: HostConnectivityModel
    var sessionList: SessionListModel
    var diagnostics: DiagnosticsModel
    var settings: SettingsModel
    /// Shared retained-operation list and the one coalesced visible refresh.
    var operationRecovery: OperationRecoveryModel?
    /// Cloud sign-in/directory presentation for the active instance. `nil` for
    /// standalone instances and unavailable services.
    var cloudSignIn: CloudSignInModel?
    /// Host-scoped create/details/fork/model presentation. It reads through the
    /// active service adapter and keeps no session cache of its own.
    var sessionManagement: SessionManagementModel
    /// Selected-instance status, default computer and Cloud app sign-ins.
    var accountSettings: AccountSettingsModel?

    var conversation: ConversationModel?
    /// Shared serialized draft owner. Conversation models take a lease; a
    /// replacement invalidates the old lease before it can write.
    var drafts: DraftService
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
    var configuration: AppConfigurationStore?
    /// Instance the current runtime was built for.
    var activeInstance: AppInstance?
    /// Monotonic activation fence. Every callback and every in-flight launch
    /// captures this value and drops its result after a newer activation.
    var activationGeneration: UInt64 = 0

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

    let serviceFactory: (@MainActor (AppInstance) async throws -> any SessionService)?
    let preferencesFactory: (@MainActor (AppInstance) -> PreferencesStore)?
    var didBootstrap = false
    var isBootstrapping = false
    var isInBackground = false

    init(
        service: any SessionService,
        preferences: PreferencesStore,
        dataDirectory: URL,
        options: LaunchOptions,
        configuration: AppConfigurationStore? = nil,
        activeInstance: AppInstance? = nil,
        serviceFactory: (@MainActor (AppInstance) async throws -> any SessionService)? = nil,
        preferencesFactory: (@MainActor (AppInstance) -> PreferencesStore)? = nil
    ) {
        self.service = service
        self.drafts = DraftService(service: service)
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
        self.connectivity = HostConnectivityModel()
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
    static func makeManagementModel(service: any SessionService) -> SessionManagementModel {
        SessionManagementModel(
            service: (service as? any SessionManagementService)
                ?? UnavailableSessionManagementService()
        )
    }

    /// Pairing and settings callbacks are fenced by the activation that created
    /// them, so a late auth result can never target the replacement runtime.
    func installCallbacks() {
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

    func isCurrentActivation(_ generation: UInt64) -> Bool {
        generation == activationGeneration
    }

    /// One shell state value. The properties below are read/write projections so
    /// the migration can move call sites without changing behaviour.
    var shell = AppShellState()

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
    var isLaunched: Bool {
        get { shell.isLaunched }
        set { shell.isLaunched = newValue }
    }
    var pendingInvitationLink: URL? {
        get { shell.pendingInvitationLink }
        set { shell.pendingInvitationLink = newValue }
    }

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
}
