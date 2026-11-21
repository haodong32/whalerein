// Owns instance activation, the monotonic activation fence, scene transitions
// and the current runtime group. A switch tears the old group down before the
// new one is built, so a late event can never write into a replaced runtime.
//
// During the rewrite the legacy aggregator still builds the runtime group; it
// is deleted by task 6.1 once every screen has moved to a feature slice.

import Foundation
import Observation
import WhaleReinMobile

@MainActor
@Observable
final class AppRuntime {
    private(set) var environment: AppEnvironment
    let router: AppRouter
    /// Temporary bridge to the pre-rewrite aggregator.
    private(set) var legacy: AppModel

    /// Monotonic fence. Every asynchronous result carries the generation it
    /// started under and is dropped when the runtime has moved on.
    private(set) var activationGeneration: UInt64 = 0
    private let launchInterval = AppSignposts.beginLaunch()

    init(environment: AppEnvironment, legacy: AppModel, router: AppRouter? = nil) {
        self.environment = environment
        self.legacy = legacy
        self.router = router ?? AppRouter()
    }

    /// Launch restore for the runtime and its current group.
    func start() async {
        await legacy.launchRestore()
        applyScenarioEntry()
        AppSignposts.endLaunch(launchInterval)
        AppLog.shared.log(.info, .runtime, "app.launched")
    }

    /// Opens the surface a Debug scenario selects. Legacy surfaces still take
    /// their flag from the model until each feature owns its route; the entry
    /// mapping itself lives here so previews and the gallery get the same
    /// destination as a test launch.
    private func applyScenarioEntry() {
        switch environment.scenario.entry {
        case .settings:
            legacy.showSettings = true
        case .diagnostics:
            legacy.showDiagnostics = true
        case .sessionSwitcher:
            legacy.showSessionSwitcher()
        case .conversation, .invitation, .cloudSignIn, .serviceSelection:
            break
        }
    }

    func handleScenePhase(_ phase: AppScenePhase) async {
        if phase == .background {
            AppLog.shared.flush()
        }
        await legacy.handleScenePhase(phase)
    }

    func handleOpenURL(_ url: URL) async {
        await legacy.handleOpenURL(url)
    }

    /// One built group: the environment and the models it assembled.
    struct RuntimeGroup {
        let environment: AppEnvironment
        let model: AppModel
    }

    /// Builds one runtime from launch arguments and the committed document.
    static func bootstrap(
        arguments: [String] = CommandLine.arguments,
        fileManager: FileManager = .default
    ) -> AppRuntime {
        AppLog.install(AppLog(
            store: AppLog.makeDefaultStore(appVersion: Self.appVersion),
            appVersion: Self.appVersion
        ))
        #if DEBUG
        let scenario = ScenarioCatalog.resolve(arguments: arguments)
        let forceScenario = ScenarioCatalog.hasScenarioArgument(arguments)
        #else
        let scenario = Scenario.normal
        let forceScenario = false
        #endif
        let group = makeGroup(
            scenario: scenario,
            arguments: arguments,
            forceScenario: forceScenario,
            fileManager: fileManager
        )
        return AppRuntime(environment: group.environment, legacy: group.model)
    }

    /// Replaces the current group with one built for `scenario`. Debug only:
    /// Release has no scenario surface, so the group never changes at runtime.
    func apply(_ scenario: Scenario) {
        #if DEBUG
        let group = Self.makeGroup(
            scenario: scenario,
            arguments: [],
            forceScenario: true,
            fileManager: .default
        )
        environment = group.environment
        legacy = group.model
        activationGeneration &+= 1
        applyScenarioEntry()
        #endif
    }

    /// Builds the environment and the model group for one scenario.
    static func makeGroup(
        scenario: Scenario,
        arguments: [String],
        forceScenario: Bool,
        fileManager: FileManager
    ) -> RuntimeGroup {
        #if DEBUG
        let options = forceScenario
            ? LaunchOptions(scenarioOnly: scenario, arguments: arguments)
            : LaunchOptions(scenario: scenario, arguments: arguments)
        #else
        let options = LaunchOptions()
        #endif

        let dataDirectory: URL
        if options.resetState {
            dataDirectory = (try? AppStorageLocations.resetDataDirectory(fileManager: fileManager))
                ?? AppStorageLocations.applicationSupportDirectory(fileManager: fileManager)
        } else {
            dataDirectory = (try? AppStorageLocations.prepareDataDirectory(fileManager: fileManager))
                ?? AppStorageLocations.applicationSupportDirectory(fileManager: fileManager)
        }

        let draftStore: any DraftStore
        if let fileStore = try? FileDraftStore(dataDirectory: dataDirectory, fileManager: fileManager) {
            draftStore = fileStore
        } else {
            draftStore = InMemoryDraftStore()
        }

        let configuration: AppConfigurationStore?
        switch options.serviceMode {
        case .fixtures:
            configuration = AppComposition.makeFixtureConfigurationStore(
                dataDirectory: dataDirectory
            )
        case .live:
            configuration = try? AppComposition.makeConfigurationStore(
                dataDirectory: dataDirectory
            )
        case .unavailable:
            configuration = nil
        }

        let activeInstance = configuration?.activeInstance
        let preferences = Self.instancePreferences(
            dataDirectory: dataDirectory,
            instanceId: activeInstance?.id ?? "unavailable",
            fallback: activeInstance?.id ?? "unavailable"
        )

        // One process logger and one Core bridge. The bridge is installed on
        // every service the runtime builds, so an instance switch keeps the
        // same correlated timeline. Rebuilding a group reuses both.
        let appLog = AppLog.shared
        let diagnosticBridge = CoreDiagnosticBridge(log: appLog)

        if options.serviceMode == .fixtures, options.seedPairedFixture {
            preferences.update {
                $0.paired = true
                if $0.lastHostId == nil {
                    $0.lastHostId = LaunchOptions.fixtureInvitationHostId
                }
                if $0.lastSessionId == nil {
                    $0.lastSessionId = "s-001"
                }
            }
        }

        let environment = AppEnvironment(
            appVersion: Self.appVersion,
            dataDirectory: dataDirectory,
            scenario: scenario,
            clock: SystemClock(),
            uuid: { UUID() },
            configuration: configuration,
            activeInstance: activeInstance,
            usesFixtures: options.serviceMode == .fixtures,
            serviceFactory: { instance in
                try Self.makeService(
                    for: instance,
                    options: options,
                    draftStore: draftStore,
                    initiallyPaired: false,
                    diagnosticSink: diagnosticBridge
                )
            },
            preferencesFactory: { instanceId, fallback in
                Self.instancePreferences(
                    dataDirectory: dataDirectory,
                    instanceId: instanceId,
                    fallback: fallback
                )
            }
        )

        let service: any SessionService
        if let activeInstance {
            service = (try? Self.makeService(
                for: activeInstance,
                options: options,
                draftStore: draftStore,
                initiallyPaired: preferences.value.paired,
                recoverableFallback: true,
                diagnosticSink: diagnosticBridge
            )) ?? UnavailableSessionService()
        } else if options.serviceMode == .unavailable || (options.serviceMode == .live && configuration == nil) {
            service = UnavailableSessionService(
                code: .configuration,
                instanceId: "unavailable"
            )
        } else {
            service = UnavailableSessionService()
        }

        let legacy = AppModel(
            service: service,
            preferences: preferences,
            dataDirectory: activeInstance?.dataDirectory ?? dataDirectory,
            options: options,
            configuration: configuration,
            activeInstance: activeInstance,
            serviceFactory: { instance in
                try environment.makeService(for: instance)
            },
            preferencesFactory: { instance in
                environment.makePreferences(instanceId: instance.id, fallback: instance.id)
            }
        )

        return RuntimeGroup(environment: environment, model: legacy)
    }

    /// One service for one instance: fixtures for deterministic launches, or
    /// the live Core-backed service with a recoverable wrapper when opening
    /// fails transiently.
    private static func makeService(
        for instance: AppInstance,
        options: LaunchOptions,
        draftStore: any DraftStore,
        initiallyPaired: Bool,
        recoverableFallback: Bool = false,
        diagnosticSink: MobileDiagnosticSink? = nil
    ) throws -> any SessionService {
        switch options.serviceMode {
        case .fixtures:
            return AppComposition.makeFixtureService(
                for: instance,
                options: options,
                draftStore: draftStore,
                initiallyPaired: initiallyPaired
            )
        case .live, .unavailable:
            if options.keychainLocked {
                return AppComposition.makeRecoverableLiveService(
                    for: instance,
                    options: options,
                    appVersion: Self.appVersion,
                    diagnosticSink: diagnosticSink
                )
            }
            do {
                return try AppComposition.makeLiveService(
                    for: instance,
                    options: options,
                    appVersion: Self.appVersion,
                    diagnosticSink: diagnosticSink
                )
            } catch {
                guard recoverableFallback else { throw error }
                // A transient Core/Keychain failure stays recoverable and
                // retries only on an explicit action or foreground return.
                return AppComposition.makeRecoverableLiveService(
                    for: instance,
                    options: options,
                    appVersion: Self.appVersion,
                    diagnosticSink: diagnosticSink
                )
            }
        }
    }

    private static func instancePreferences(
        dataDirectory: URL,
        instanceId: String,
        fallback: String
    ) -> PreferencesStore {
        let id = instanceId.isEmpty ? fallback : instanceId
        let directory = AppStorageLocations.instanceDirectory(
            in: dataDirectory,
            instanceId: id
        )
        try? FileManager.default.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        #if os(iOS)
        try? FileManager.default.setAttributes(
            [.protectionKey: FileProtectionType.complete],
            ofItemAtPath: directory.path
        )
        #endif
        return PreferencesStore(directory: directory)
    }

    static var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.1.0"
    }
}
