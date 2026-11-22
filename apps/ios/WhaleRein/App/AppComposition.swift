// App composition: one authoritative document, one service per active
// instance, and the per-instance Keychain/data scope.
//
// `LiveSessionService` never creates configuration. It opens the materialized
// `config_path` produced by `AppConfigurationStore` for the committed instance.

import Foundation
import WhaleReinMobile

@MainActor
enum AppComposition {
    static func makeConfigurationStore(
        dataDirectory: URL
    ) throws -> AppConfigurationStore {
        let document = AppStorageLocations.appConfigurationURL(in: dataDirectory)
        return try AppConfigurationStore(documentPath: document.path)
    }

    static func makeFixtureConfigurationStore(
        dataDirectory: URL,
        activeInstance: FixtureInstance = .standalone
    ) -> AppConfigurationStore {
        let document = AppStorageLocations.appConfigurationURL(in: dataDirectory)
        return AppConfigurationStore(
            fixture: .fixture(
                documentURL: document,
                activeInstanceID: activeInstance.instanceId
            )
        )
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

    /// True when this build and instance use the developer file store instead
    /// of the Keychain. Only the local Cloud instance qualifies, and only in
    /// Debug builds where Local is exposed.
    static func usesFileSecrets(for instance: AppInstance) -> Bool {
        #if DEBUG
        instance.isLocal
        #else
        false
        #endif
    }

    /// The developer file store lives beside the materialized instance TOML.
    static func secretsDirectory(for instance: AppInstance) -> URL {
        instance.dataDirectory
            .deletingLastPathComponent()
            .appendingPathComponent("secrets", isDirectory: true)
    }

    /// Builds the live service for one materialized instance. Throws a typed
    /// error so a failed switch can keep the previous runtime.
    static func makeLiveService(
        for instance: AppInstance,
        options: LaunchOptions,
        appVersion: String,
        diagnosticSink: (any CoreDiagnosticSink)? = nil
    ) throws -> any SessionService {
        let secretStore: any SecretStore
        if usesFileSecrets(for: instance) {
            do {
                secretStore = try FileSecretStore(
                    directory: secretsDirectory(for: instance)
                )
            } catch {
                throw SessionServiceError(.internalError)
            }
        } else {
            let keychain = KeychainSecretStore(
                service: keychainService(for: instance, options: options)
            )
            guard keychain.availability() == .available else {
                throw SessionServiceError(.keychainUnavailable)
            }
            secretStore = keychain
        }
        do {
            return try LiveSessionService(
                configPath: instance.configPath,
                secretStore: secretStore,
                appVersion: appVersion,
                instanceId: instance.id,
                diagnosticSink: diagnosticSink
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
        appVersion: String,
        diagnosticSink: (any CoreDiagnosticSink)? = nil
    ) -> RecoverableSessionService {
        RecoverableSessionService(instanceId: instance.id) {
            try makeLiveService(
                for: instance,
                options: options,
                appVersion: appVersion,
                diagnosticSink: diagnosticSink
            )
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
            instanceId: instance.id,
            serviceKind: instance.service
        )
    }
}
