// App entry point. Selects the fixture or live service, prepares the app data
// directory, honors launch arguments, and injects the composition root.

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

        let preferences = PreferencesStore(directory: dataDirectory)
        if options.keychainLocked || options.seedPairedFixture {
            preferences.update {
                $0.paired = true
                if $0.lastHostId == nil { $0.lastHostId = LaunchOptions.fixtureInvitationHostId }
                if $0.lastSessionId == nil { $0.lastSessionId = "s-001" }
            }
        }
        let draftStore: any DraftStore
        if let fileStore = try? FileDraftStore(dataDirectory: dataDirectory, fileManager: fileManager) {
            draftStore = fileStore
        } else {
            draftStore = InMemoryDraftStore()
        }

        let service: any SessionService
        let keychainService = options.staging
            ? AppStorageLocations.stagingKeychainService
            : AppStorageLocations.keychainService

        if options.keychainLocked {
            service = RecoverableSessionService(failureCode: .keychainUnavailable) {
                throw SessionServiceError(.keychainUnavailable)
            }
        } else {
            switch options.serviceMode {
            case .fixtures:
                service = FixtureSessionService(
                    options: options,
                    draftStore: draftStore,
                    initiallyPaired: preferences.value.paired
                )
            case .live:
                let probe = KeychainSecretStore(service: keychainService)
                if probe.availability() == .available {
                    do {
                        service = try LiveSessionService(
                            dataDirectory: dataDirectory,
                            secretStore: probe,
                            appVersion: Self.appVersion
                        )
                    } catch {
                        // A transient Core/Keychain failure stays recoverable.
                        service = Self.makeRecoverableLiveService(
                            dataDirectory: dataDirectory,
                            keychainService: keychainService
                        )
                    }
                } else {
                    // Before the first unlock after a reboot the Keychain is
                    // locked. Show an actionable state and retry only on an
                    // explicit user action or foreground transition.
                    service = Self.makeRecoverableLiveService(
                        dataDirectory: dataDirectory,
                        keychainService: keychainService
                    )
                }
            case .unavailable:
                service = UnavailableSessionService()
            }
        }

        _model = State(
            initialValue: AppModel(
                service: service,
                preferences: preferences,
                dataDirectory: dataDirectory,
                options: options
            )
        )
    }

    private static func makeRecoverableLiveService(
        dataDirectory: URL,
        keychainService: String
    ) -> RecoverableSessionService {
        RecoverableSessionService(failureCode: .keychainUnavailable) {
            let retryStore = KeychainSecretStore(service: keychainService)
            guard retryStore.availability() == .available else {
                throw SessionServiceError(.keychainUnavailable)
            }
            do {
                return try LiveSessionService(
                    dataDirectory: dataDirectory,
                    secretStore: retryStore,
                    appVersion: Self.appVersion
                )
            } catch let error as SessionServiceError {
                throw error
            } catch {
                throw SessionServiceError(.keychainUnavailable)
            }
        }
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
