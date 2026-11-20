// App composition: one authoritative document, one service per active
// instance, and the per-instance Keychain/data scope.
//
// `LiveSessionService` never creates configuration. It opens the materialized
// `config_path` produced by `AppConfigurationStore` for the committed instance.

import Foundation

@MainActor
enum AppComposition {
    static func makeConfigurationStore(
        dataDirectory: URL
    ) throws -> AppConfigurationStore {
        let document = AppStorageLocations.appConfigurationURL(in: dataDirectory)
        return try AppConfigurationStore(documentPath: document.path)
    }

    static func makeFixtureConfigurationStore(
        dataDirectory: URL
    ) -> AppConfigurationStore {
        let document = AppStorageLocations.appConfigurationURL(in: dataDirectory)
        return AppConfigurationStore(fixture: .fixture(documentURL: document))
    }

    static func keychainService(
        for instance: AppInstance,
        options: LaunchOptions
    ) -> String {
        AppStorageLocations.keychainService(
            instanceId: instance.id,
            staging: options.staging || instance.environment == .staging
        )
    }

    /// Builds the live service for one materialized instance. Throws a typed
    /// error so a failed switch can keep the previous runtime.
    static func makeLiveService(
        for instance: AppInstance,
        options: LaunchOptions,
        appVersion: String
    ) throws -> any SessionService {
        let service = keychainService(for: instance, options: options)
        let secretStore = KeychainSecretStore(service: service)
        guard secretStore.availability() == .available else {
            throw SessionServiceError(.keychainUnavailable)
        }
        do {
            return try LiveSessionService(
                configPath: instance.configPath,
                secretStore: secretStore,
                appVersion: appVersion,
                instanceId: instance.id
            )
        } catch let error as SessionServiceError {
            throw error
        } catch {
            throw SessionServiceError(.internalError)
        }
    }

    static func makeRecoverableLiveService(
        for instance: AppInstance,
        options: LaunchOptions,
        appVersion: String
    ) -> RecoverableSessionService {
        RecoverableSessionService(instanceId: instance.id) {
            try makeLiveService(for: instance, options: options, appVersion: appVersion)
        }
    }

    static func makeFixtureService(
        for instance: AppInstance,
        options: LaunchOptions,
        draftStore: any DraftStore,
        initiallyPaired: Bool
    ) -> FixtureSessionService {
        FixtureSessionService(
            options: options,
            draftStore: draftStore,
            initiallyPaired: initiallyPaired,
            instanceId: instance.id
        )
    }
}
