// App entry point. Bootstraps the authoritative client document, builds the
// service for the committed instance, honors launch arguments, and injects the
// composition root.
//
// Legacy `preferences.json`/`core/` at the old app root are never read,
// migrated or deleted. New per-instance state lives under
// `Application Support/WhaleRein/client/instances/<id>/`.

import SwiftUI

@main
struct WhaleReinApp: App {
    @State private var model: AppModel

    init() {
        let options = LaunchOptions()
        let fileManager = FileManager.default
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

        switch options.serviceMode {
        case .fixtures:
            let configuration = AppComposition.makeFixtureConfigurationStore(
                dataDirectory: dataDirectory
            )
            let active = configuration.activeInstance
            let preferences = Self.instancePreferences(
                dataDirectory: dataDirectory,
                instanceId: active.id,
                fallback: "fixtures"
            )
            if options.seedPairedFixture {
                preferences.update {
                    $0.paired = true
                    if $0.lastHostId == nil { $0.lastHostId = LaunchOptions.fixtureInvitationHostId }
                    if $0.lastSessionId == nil { $0.lastSessionId = "s-001" }
                }
            }
            let service = AppComposition.makeFixtureService(
                for: active,
                options: options,
                draftStore: draftStore,
                initiallyPaired: preferences.value.paired
            )
            _model = State(
                initialValue: AppModel(
                    service: service,
                    preferences: preferences,
                    dataDirectory: active.dataDirectory,
                    options: options,
                    configuration: configuration,
                    activeInstance: active,
                    serviceFactory: { instance in
                        AppComposition.makeFixtureService(
                            for: instance,
                            options: options,
                            draftStore: draftStore,
                            initiallyPaired: false
                        )
                    },
                    preferencesFactory: { instance in
                        Self.instancePreferences(
                            dataDirectory: dataDirectory,
                            instanceId: instance.id,
                            fallback: "fixtures"
                        )
                    }
                )
            )
        case .live:
            let configuration: AppConfigurationStore
            do {
                configuration = try AppComposition.makeConfigurationStore(
                    dataDirectory: dataDirectory
                )
            } catch {
                let unavailable = UnavailableSessionService(
                    code: .configuration,
                    instanceId: "unavailable"
                )
                let preferences = Self.instancePreferences(
                    dataDirectory: dataDirectory,
                    instanceId: "unavailable",
                    fallback: "unavailable"
                )
                _model = State(
                    initialValue: AppModel(
                        service: unavailable,
                        preferences: preferences,
                        dataDirectory: dataDirectory,
                        options: options
                    )
                )
                return
            }
            let active = configuration.activeInstance
            let preferences = Self.instancePreferences(
                dataDirectory: dataDirectory,
                instanceId: active.id,
                fallback: active.id
            )
            let service: any SessionService
            if options.keychainLocked {
                service = AppComposition.makeRecoverableLiveService(
                    for: active,
                    options: options,
                    appVersion: Self.appVersion
                )
            } else {
                do {
                    service = try AppComposition.makeLiveService(
                        for: active,
                        options: options,
                        appVersion: Self.appVersion
                    )
                } catch {
                    // A transient Core/Keychain failure stays recoverable and
                    // retries only on an explicit action or foreground return.
                    service = AppComposition.makeRecoverableLiveService(
                        for: active,
                        options: options,
                        appVersion: Self.appVersion
                    )
                }
            }
            _model = State(
                initialValue: AppModel(
                    service: service,
                    preferences: preferences,
                    dataDirectory: active.dataDirectory,
                    options: options,
                    configuration: configuration,
                    activeInstance: active,
                    serviceFactory: { instance in
                        try AppComposition.makeLiveService(
                            for: instance,
                            options: options,
                            appVersion: Self.appVersion
                        )
                    },
                    preferencesFactory: { instance in
                        Self.instancePreferences(
                            dataDirectory: dataDirectory,
                            instanceId: instance.id,
                            fallback: active.id
                        )
                    }
                )
            )
        case .unavailable:
            let preferences = Self.instancePreferences(
                dataDirectory: dataDirectory,
                instanceId: "unavailable",
                fallback: "unavailable"
            )
            _model = State(
                initialValue: AppModel(
                    service: UnavailableSessionService(),
                    preferences: preferences,
                    dataDirectory: dataDirectory,
                    options: options
                )
            )
        }
    }

    /// Per-instance non-secret UI state (last Host/session). The file lives
    /// beside the instance's materialized configuration, never at the legacy
    /// app root.
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

    private static var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.1.0"
    }

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .modifier(DynamicTypeOverride(enabled: model.options.largeText))
        }
    }
}

private struct DynamicTypeOverride: ViewModifier {
    let enabled: Bool

    func body(content: Content) -> some View {
        if enabled {
            content.dynamicTypeSize(.accessibility1)
        } else {
            content.dynamicTypeSize(.xSmall ... .accessibility5)
        }
    }
}
