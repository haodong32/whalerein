// The authoritative app configuration document and its Swift model.
//
// The Rust binding owns document shape, validation and atomic commit order.
// This layer maps binding records into app-owned values, keeps the last
// successfully committed document, and never suppresses an I/O failure.

import Foundation
import WhaleReinMobile

enum AppService: String, CaseIterable, Sendable {
    case cloud = "whalerein_cloud"
    case standalone
}

enum AppCloudEnvironment: String, CaseIterable, Sendable {
    case production
    case staging
    case local
}

struct AppInstance: Identifiable, Equatable, Sendable {
    let id: String
    let service: AppService
    let environment: AppCloudEnvironment?
    /// Absolute data directory resolved by the binding.
    let dataDirectory: URL
    /// Absolute materialized Core TOML passed to `MobileCore`.
    let configPath: URL
    var hasStandaloneCA: Bool
    var standaloneCAPath: URL?
    /// True when the local Cloud instance pins an imported mkcert root bundle.
    var hasCloudCA: Bool = false
    /// Absolute imported local Cloud CA bundle.
    var cloudCAPath: URL? = nil

    var isStandalone: Bool { service == .standalone }

    var isLocal: Bool {
        service == .cloud && environment == .local
    }

    var isCloudProduction: Bool {
        service == .cloud && (environment == .production || environment == nil)
    }
}

struct AppConfiguration: Equatable, Sendable {
    let documentURL: URL
    let version: UInt32
    let activeInstanceID: String
    let reasoningVisible: Bool
    let instances: [AppInstance]

    var activeInstance: AppInstance? {
        instances.first { $0.id == activeInstanceID }
    }

    func instance(id: String) -> AppInstance? {
        instances.first { $0.id == id }
    }
}

/// Editable instance fields. `dataDirectory` is relative to the app document.
struct AppInstanceInput: Equatable, Sendable {
    var id: String
    var service: AppService
    var environment: AppCloudEnvironment?
    var dataDirectory: String
    var standaloneCAPEM: String?
    /// Imported mkcert root for the local Cloud instance.
    var cloudCAPEM: String? = nil
}

/// One selectable service entry on the welcome/settings surface.
struct AppServiceOption: Identifiable, Equatable, Sendable {
    enum Kind: Equatable, Sendable {
        case existing(instanceID: String)
        /// A Staging instance is created on first selection in a debug build.
        case createStaging
        /// A Local instance is created on first selection in a debug build.
        case createLocal
    }

    let id: String
    let titleKey: L10nKey
    let kind: Kind
    let service: AppService
    let environment: AppCloudEnvironment?
}

/// The seam used by unit tests to inject commit and validation failures.
@MainActor
protocol AppConfigurationBinding {
    func bootstrap(path: String) throws -> AppConfiguration
    func read(path: String) throws -> AppConfiguration
    func validate(path: String, input: AppInstanceInput) throws -> AppConfiguration
    func save(path: String, input: AppInstanceInput) throws -> AppConfiguration
    func activate(path: String, id: String) throws -> AppConfiguration
    func remove(path: String, id: String) throws -> AppConfiguration
    func setReasoningVisible(path: String, visible: Bool) throws -> AppConfiguration
    func removeCA(path: String, id: String) throws -> AppConfiguration
    func removeCloudCA(path: String, id: String) throws -> AppConfiguration
}

@MainActor
final class BindingAppConfigurationBinding: AppConfigurationBinding {
    func bootstrap(path: String) throws -> AppConfiguration {
        try map { try WhaleReinMobile.appConfigBootstrap(path: path) }
    }

    func read(path: String) throws -> AppConfiguration {
        try map { try WhaleReinMobile.appConfigRead(path: path) }
    }

    func validate(path: String, input: AppInstanceInput) throws -> AppConfiguration {
        try map { try WhaleReinMobile.appConfigValidateInstance(path: path, input: input.binding) }
    }

    func save(path: String, input: AppInstanceInput) throws -> AppConfiguration {
        try map { try WhaleReinMobile.appConfigSaveInstance(path: path, input: input.binding) }
    }

    func activate(path: String, id: String) throws -> AppConfiguration {
        try map { try WhaleReinMobile.appConfigSetActive(path: path, id: id) }
    }

    func remove(path: String, id: String) throws -> AppConfiguration {
        try map { try WhaleReinMobile.appConfigRemoveInstance(path: path, id: id) }
    }

    func setReasoningVisible(path: String, visible: Bool) throws -> AppConfiguration {
        try map { try WhaleReinMobile.appConfigSetReasoningVisible(path: path, visible: visible) }
    }

    func removeCA(path: String, id: String) throws -> AppConfiguration {
        try map { try WhaleReinMobile.appConfigRemoveCa(path: path, id: id) }
    }

    func removeCloudCA(path: String, id: String) throws -> AppConfiguration {
        try map { try WhaleReinMobile.appConfigRemoveCloudCa(path: path, id: id) }
    }

    private func map(_ body: () throws -> WhaleReinMobile.AppConfigView) throws -> AppConfiguration {
        do {
            return try AppConfiguration(view: body(), documentPath: nil)
        } catch let error as SessionServiceError {
            throw error
        } catch let error as MobileError {
            throw AppConfigurationError.map(error)
        } catch {
            throw SessionServiceError(.configuration)
        }
    }
}

enum AppConfigurationError {
    @MainActor
    static func map(_ error: Error) -> SessionServiceError {
        if let service = error as? SessionServiceError { return service }
        if error is MobileError {
            return SessionServiceError(.configuration)
        }
        return SessionServiceError(.configuration)
    }
}

@MainActor
final class AppConfigurationStore {
    let documentPath: String
    private let binding: (any AppConfigurationBinding)?
    /// The last successfully committed document. A failed mutation leaves this
    /// value untouched.
    private(set) var configuration: AppConfiguration

    convenience init(documentPath: String) throws {
        try self.init(
            documentPath: documentPath,
            binding: BindingAppConfigurationBinding()
        )
    }

    init(
        documentPath: String,
        binding: any AppConfigurationBinding
    ) throws {
        self.documentPath = documentPath
        self.binding = binding
        do {
            self.configuration = try binding.bootstrap(path: documentPath)
        } catch let error as SessionServiceError {
            throw error
        } catch {
            throw AppConfigurationError.map(error)
        }
    }

    /// In-memory configuration for deterministic fixture runs and unit tests.
    init(fixture configuration: AppConfiguration) {
        self.documentPath = configuration.documentURL.path
        self.binding = nil
        self.configuration = configuration
    }

    var activeInstance: AppInstance {
        // Bootstrap guarantees the selected instance exists; the fallback keeps
        // a corrupt in-memory fixture from crashing a test.
        configuration.activeInstance
            ?? configuration.instances.first
            ?? AppConfiguration.fixtureInstances(in: configuration.documentURL)
                .first!
    }

    var instances: [AppInstance] { configuration.instances }
    var reasoningVisible: Bool { configuration.reasoningVisible }

    @discardableResult
    func save(_ input: AppInstanceInput) throws -> AppConfiguration {
        guard binding != nil else {
            return try fixtureSave(input)
        }
        return try apply { try $0.save(path: documentPath, input: input) }
    }

    @discardableResult
    func activate(id: String) throws -> AppConfiguration {
        guard binding != nil else {
            guard configuration.instance(id: id) != nil else {
                throw SessionServiceError(.configuration)
            }
            configuration = AppConfiguration(
                documentURL: configuration.documentURL,
                version: configuration.version,
                activeInstanceID: id,
                reasoningVisible: configuration.reasoningVisible,
                instances: configuration.instances
            )
            return configuration
        }
        return try apply { try $0.activate(path: documentPath, id: id) }
    }

    @discardableResult
    func setReasoningVisible(_ visible: Bool) throws -> AppConfiguration {
        guard binding != nil else {
            configuration = AppConfiguration(
                documentURL: configuration.documentURL,
                version: configuration.version,
                activeInstanceID: configuration.activeInstanceID,
                reasoningVisible: visible,
                instances: configuration.instances
            )
            return configuration
        }
        return try apply { try $0.setReasoningVisible(path: documentPath, visible: visible) }
    }

    /// Save the local Cloud instance with its imported mkcert root and select it.
    @discardableResult
    func enableLocal(pem: String) throws -> AppConfiguration {
        let input = AppInstanceInput(
            id: "cloud-local",
            service: .cloud,
            environment: .local,
            dataDirectory: "instances/cloud-local/data",
            standaloneCAPEM: nil,
            cloudCAPEM: pem
        )
        return try save(input)
    }

    @discardableResult
    func removeCloudCA(instanceID: String) throws -> AppConfiguration {
        guard binding != nil else {
            let instances = configuration.instances.filter { $0.id != instanceID }
            configuration = AppConfiguration(
                documentURL: configuration.documentURL,
                version: configuration.version,
                activeInstanceID: configuration.activeInstanceID,
                reasoningVisible: configuration.reasoningVisible,
                instances: instances
            )
            return configuration
        }
        return try apply { try $0.removeCloudCA(path: documentPath, id: instanceID) }
    }

    @discardableResult
    func importCA(instanceID: String, pem: String) throws -> AppConfiguration {
        guard let instance = configuration.instance(id: instanceID), instance.isStandalone else {
            throw SessionServiceError(.configuration)
        }
        let input = AppInstanceInput(
            id: instance.id,
            service: instance.service,
            environment: instance.environment,
            dataDirectory: relativeReference(for: instance.dataDirectory),
            standaloneCAPEM: pem
        )
        return try save(input)
    }

    @discardableResult
    func removeCA(instanceID: String) throws -> AppConfiguration {
        guard binding != nil else {
            let instances = configuration.instances.map { instance in
                guard instance.id == instanceID else { return instance }
                var next = instance
                next.hasStandaloneCA = false
                next.standaloneCAPath = nil
                return next
            }
            configuration = AppConfiguration(
                documentURL: configuration.documentURL,
                version: configuration.version,
                activeInstanceID: configuration.activeInstanceID,
                reasoningVisible: configuration.reasoningVisible,
                instances: instances
            )
            return configuration
        }
        return try apply { try $0.removeCA(path: documentPath, id: instanceID) }
    }

    private func fixtureSave(_ input: AppInstanceInput) throws -> AppConfiguration {
        let environmentIsValid = input.service == .standalone
            ? input.environment == nil
            : input.environment != nil
        let cloudCAIsValid = input.cloudCAPEM == nil
            || (input.service == .cloud && input.environment == .local)
        guard !input.id.isEmpty, environmentIsValid, cloudCAIsValid else {
            throw SessionServiceError(.configuration)
        }
        let client = configuration.documentURL.deletingLastPathComponent()
        let instance = AppInstance(
            id: input.id,
            service: input.service,
            environment: input.environment,
            dataDirectory: client.appendingPathComponent(input.dataDirectory, isDirectory: true),
            configPath: client.appendingPathComponent(
                "instances/\(input.id)/instance.toml"
            ),
            hasStandaloneCA: input.standaloneCAPEM != nil,
            standaloneCAPath: input.standaloneCAPEM == nil
                ? nil
                : client.appendingPathComponent("instances/\(input.id)/ca.pem"),
            hasCloudCA: input.service == .cloud
                && input.environment == .local
                && input.cloudCAPEM != nil,
            cloudCAPath: input.cloudCAPEM == nil
                ? nil
                : client.appendingPathComponent("instances/\(input.id)/cloud-ca.pem")
        )
        var instances = configuration.instances.filter { $0.id != input.id }
        instances.append(instance)
        configuration = AppConfiguration(
            documentURL: configuration.documentURL,
            version: configuration.version,
            activeInstanceID: configuration.activeInstanceID,
            reasoningVisible: configuration.reasoningVisible,
            instances: instances.sorted { $0.id < $1.id }
        )
        return configuration
    }

    /// Builds the options shown by the welcome and Settings selectors.
    ///
    /// Production builds never show Staging or Local. A Debug or test build may
    /// show both; Local still needs the desktop mkcert root to be trusted by iOS
    /// before it can connect.
    nonisolated static func serviceOptions(
        for instances: [AppInstance],
        showsStaging: Bool = AppConfigurationStore.defaultShowsStaging,
        showsLocal: Bool = AppConfigurationStore.defaultShowsLocal
    ) -> [AppServiceOption] {
        let visible = instances.filter { showsLocal || $0.environment != .local }
        var options: [AppServiceOption] = []
        if let production = visible.first(where: {
            $0.service == .cloud && $0.environment == .production
        }) ?? visible.first(where: {
            $0.service == .cloud && ($0.environment == nil || $0.environment == .production)
        }) {
            options.append(
                AppServiceOption(
                    id: production.id,
                    titleKey: .serviceCloudProduction,
                    kind: .existing(instanceID: production.id),
                    service: .cloud,
                    environment: .production
                )
            )
        }
        if showsStaging {
            if let staging = visible.first(where: {
                $0.service == .cloud && $0.environment == .staging
            }) {
                options.append(
                    AppServiceOption(
                        id: staging.id,
                        titleKey: .serviceCloudStaging,
                        kind: .existing(instanceID: staging.id),
                        service: .cloud,
                        environment: .staging
                    )
                )
            } else {
                options.append(
                    AppServiceOption(
                        id: "cloud-staging",
                        titleKey: .serviceCloudStaging,
                        kind: .createStaging,
                        service: .cloud,
                        environment: .staging
                    )
                )
            }
        }
        if showsLocal {
            if let local = visible.first(where: {
                $0.service == .cloud && $0.environment == .local
            }) {
                options.append(
                    AppServiceOption(
                        id: local.id,
                        titleKey: .serviceCloudLocal,
                        kind: .existing(instanceID: local.id),
                        service: .cloud,
                        environment: .local
                    )
                )
            } else {
                options.append(
                    AppServiceOption(
                        id: "cloud-local",
                        titleKey: .serviceCloudLocal,
                        kind: .createLocal,
                        service: .cloud,
                        environment: .local
                    )
                )
            }
        }
        if let standalone = visible.first(where: { $0.service == .standalone }) {
            options.append(
                AppServiceOption(
                    id: standalone.id,
                    titleKey: .serviceStandalone,
                    kind: .existing(instanceID: standalone.id),
                    service: .standalone,
                    environment: nil
                )
            )
        }
        return options
    }

    nonisolated static var defaultShowsStaging: Bool {
        #if DEBUG
        true
        #else
        false
        #endif
    }

    /// Local is a developer-only environment. It is exposed only in Debug/test
    /// builds; the desktop mkcert CA must still be trusted by iOS at runtime.
    nonisolated static var defaultShowsLocal: Bool {
        #if DEBUG
        true
        #else
        false
        #endif
    }

    private func apply(
        _ mutate: (any AppConfigurationBinding) throws -> AppConfiguration
    ) throws -> AppConfiguration {
        guard let binding else {
            // Fixture mode owns the document in memory. Activation still has
            // to be result-bearing so the caller can fence a failed switch.
            throw SessionServiceError(.configuration)
        }
        do {
            let next = try mutate(binding)
            configuration = next
            return next
        } catch let error as SessionServiceError {
            throw error
        } catch {
            throw AppConfigurationError.map(error)
        }
    }

    /// Relative form of an absolute instance path for round-tripping edits.
    private func relativeReference(for url: URL) -> String {
        let base = URL(fileURLWithPath: documentPath).deletingLastPathComponent()
        let basePath = base.standardizedFileURL.path
        let path = url.standardizedFileURL.path
        guard path.hasPrefix(basePath + "/") else { return path }
        return String(path.dropFirst(basePath.count + 1))
    }
}

extension AppConfiguration {
    /// Maps one binding view. A document without a resolvable active instance is
    /// rejected rather than silently selecting another service.
    init(view: WhaleReinMobile.AppConfigView, documentPath: String?) throws {
        guard let active = view.instances.first(where: { $0.id == view.app.activeInstanceId }) else {
            throw SessionServiceError(.configuration)
        }
        let instances = view.instances.compactMap { instance -> AppInstance? in
            guard let service = AppService(rawValue: instance.service) else { return nil }
            let environment = instance.environment.flatMap(AppCloudEnvironment.init(rawValue:))
            return AppInstance(
                id: instance.id,
                service: service,
                environment: environment,
                dataDirectory: URL(fileURLWithPath: instance.dataDir, isDirectory: true),
                configPath: URL(fileURLWithPath: instance.configPath),
                hasStandaloneCA: instance.hasStandaloneCa,
                standaloneCAPath: instance.standaloneCaPath.map {
                    URL(fileURLWithPath: $0)
                },
                hasCloudCA: instance.hasCloudCa,
                cloudCAPath: instance.cloudCaPath.map {
                    URL(fileURLWithPath: $0)
                }
            )
        }
        guard instances.contains(where: { $0.id == active.id }) else {
            throw SessionServiceError(.configuration)
        }
        self.documentURL = URL(fileURLWithPath: documentPath ?? view.path)
        self.version = view.version
        self.activeInstanceID = view.app.activeInstanceId
        self.reasoningVisible = view.app.reasoningVisible
        self.instances = instances
    }

    static func fixture(documentURL: URL) -> AppConfiguration {
        AppConfiguration(
            documentURL: documentURL,
            version: 1,
            activeInstanceID: "cloud-production",
            reasoningVisible: false,
            instances: fixtureInstances(in: documentURL)
        )
    }

    static func fixtureInstances(in documentURL: URL) -> [AppInstance] {
        let client = documentURL.deletingLastPathComponent()
        return [
            AppInstance(
                id: "cloud-production",
                service: .cloud,
                environment: .production,
                dataDirectory: client
                    .appendingPathComponent("instances/cloud-production/data", isDirectory: true),
                configPath: client.appendingPathComponent("instances/cloud-production/instance.toml"),
                hasStandaloneCA: false,
                standaloneCAPath: nil
            ),
            AppInstance(
                id: "standalone",
                service: .standalone,
                environment: nil,
                dataDirectory: client
                    .appendingPathComponent("instances/standalone/data", isDirectory: true),
                configPath: client.appendingPathComponent("instances/standalone/instance.toml"),
                hasStandaloneCA: false,
                standaloneCAPath: nil
            )
        ]
    }
}

extension AppInstanceInput {
    var binding: WhaleReinMobile.InstanceInput {
        WhaleReinMobile.InstanceInput(
            id: id,
            service: service.rawValue,
            environment: environment?.rawValue,
            dataDir: dataDirectory,
            standaloneCaPem: standaloneCAPEM,
            cloudCaPem: cloudCAPEM
        )
    }
}
