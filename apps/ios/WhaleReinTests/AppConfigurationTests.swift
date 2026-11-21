// Client-configuration document and service-selection rules.
//
// The real binding has Rust-side document/atomic-write tests. These Swift tests
// cover the app-owned model, the result-bearing store seam, and the build-flavor
// option visibility without a Simulator UI pass.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
final class StubAppConfigurationBinding: AppConfigurationBinding {
    var configuration: AppConfiguration
    var bootstrapError: Error?
    var activateError: Error?
    var saveError: Error?
    var removeCAError: Error?
    private(set) var activateCalls: [String] = []

    init(configuration: AppConfiguration) {
        self.configuration = configuration
    }

    func bootstrap(path: String) throws -> AppConfiguration {
        if let bootstrapError { throw bootstrapError }
        return configuration
    }

    func read(path: String) throws -> AppConfiguration { configuration }

    func validate(path: String, input: AppInstanceInput) throws -> AppConfiguration {
        if let saveError { throw saveError }
        return configuration
    }

    func save(path: String, input: AppInstanceInput) throws -> AppConfiguration {
        if let saveError { throw saveError }
        let base = configuration.documentURL.deletingLastPathComponent()
        let instance = AppInstance(
            id: input.id,
            service: input.service,
            environment: input.environment,
            dataDirectory: base.appendingPathComponent(input.dataDirectory, isDirectory: true),
            configPath: base.appendingPathComponent("instances/\(input.id)/instance.toml"),
            hasStandaloneCA: input.standaloneCAPEM != nil,
            standaloneCAPath: input.standaloneCAPEM == nil
                ? nil
                : base.appendingPathComponent("instances/\(input.id)/ca.pem")
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

    func activate(path: String, id: String) throws -> AppConfiguration {
        activateCalls.append(id)
        if let activateError { throw activateError }
        guard let instance = configuration.instance(id: id) else {
            throw SessionServiceError(.configuration)
        }
        configuration = AppConfiguration(
            documentURL: configuration.documentURL,
            version: configuration.version,
            activeInstanceID: id,
            reasoningVisible: configuration.reasoningVisible,
            instances: configuration.instances
        )
        _ = instance
        return configuration
    }

    func remove(path: String, id: String) throws -> AppConfiguration { configuration }

    func setReasoningVisible(path: String, visible: Bool) throws -> AppConfiguration {
        configuration = AppConfiguration(
            documentURL: configuration.documentURL,
            version: configuration.version,
            activeInstanceID: configuration.activeInstanceID,
            reasoningVisible: visible,
            instances: configuration.instances
        )
        return configuration
    }

    func removeCA(path: String, id: String) throws -> AppConfiguration {
        if let removeCAError { throw removeCAError }
        return configuration
    }
}

@Suite("App configuration")
@MainActor
struct AppConfigurationTests {
    private func fixtureConfiguration(
        document: URL = URL(fileURLWithPath: "/tmp/WhaleRein/client/app.toml")
    ) -> AppConfiguration {
        .fixture(documentURL: document)
    }

    private func makeStore(
        document: URL = URL(fileURLWithPath: "/tmp/WhaleRein/client/app.toml")
    ) throws -> (AppConfigurationStore, StubAppConfigurationBinding) {
        let binding = StubAppConfigurationBinding(configuration: fixtureConfiguration(document: document))
        let store = try AppConfigurationStore(documentPath: document.path, binding: binding)
        return (store, binding)
    }

    @Test("production options hide Staging and always hide Local")
    func productionOptionVisibility() {
        var instances = fixtureConfiguration().instances
        instances.append(
            AppInstance(
                id: "cloud-local",
                service: .cloud,
                environment: .local,
                dataDirectory: URL(fileURLWithPath: "/tmp/local/data"),
                configPath: URL(fileURLWithPath: "/tmp/local/instance.toml"),
                hasStandaloneCA: false,
                standaloneCAPath: nil
            )
        )
        let production = AppConfigurationStore.serviceOptions(
            for: instances,
            showsStaging: false
        )
        #expect(production.map(\.id) == ["cloud-production", "standalone"])
        #expect(production.map(\.environment) == [.production, nil])

        let debug = AppConfigurationStore.serviceOptions(
            for: instances,
            showsStaging: true
        )
        #expect(debug.map(\.id) == ["cloud-production", "cloud-staging", "standalone"])
        #expect(!debug.contains { $0.environment == .local })
    }

    @Test("an existing Staging instance is selected instead of creating a new one")
    func existingStagingInstance() {
        var instances = fixtureConfiguration().instances
        instances.append(
            AppInstance(
                id: "staging",
                service: .cloud,
                environment: .staging,
                dataDirectory: URL(fileURLWithPath: "/tmp/staging/data"),
                configPath: URL(fileURLWithPath: "/tmp/staging/instance.toml"),
                hasStandaloneCA: false,
                standaloneCAPath: nil
            )
        )
        let options = AppConfigurationStore.serviceOptions(for: instances, showsStaging: true)
        #expect(options.map(\.id) == ["cloud-production", "staging", "standalone"])
        #expect(options[1].kind == .existing(instanceID: "staging"))
    }

    @Test("a failed activation keeps the previous committed selection")
    func failedWriteKeepsOldSelection() throws {
        let (store, binding) = try makeStore()
        binding.activateError = SessionServiceError(.configuration)
        #expect(throws: SessionServiceError.self) {
            try store.activate(id: "standalone")
        }
        #expect(store.activeInstance.id == "cloud-production")
        #expect(store.configuration.activeInstanceID == "cloud-production")
    }

    @Test("an invalid PEM keeps the prior instance configuration")
    func invalidPEMKeepsPriorConfiguration() throws {
        let (store, binding) = try makeStore()
        let before = store.configuration
        binding.saveError = SessionServiceError(.configuration)
        #expect(throws: SessionServiceError.self) {
            try store.importCA(instanceID: "standalone", pem: "not a certificate")
        }
        #expect(store.configuration == before)
        #expect(!store.activeInstance.hasStandaloneCA)
    }

    @Test("a failed activation still reports the typed error code")
    func failedActivationReturnsCode() throws {
        let (store, binding) = try makeStore()
        binding.activateError = SessionServiceError(.keychainUnavailable)
        do {
            _ = try store.activate(id: "standalone")
            Issue.record("activation should have failed")
        } catch let error as SessionServiceError {
            #expect(error.code == .keychainUnavailable)
        }
        #expect(store.activeInstance.id == "cloud-production")
    }

    @Test("Keychain namespaces are stable, distinct per instance, and keep the staging suffix")
    func perInstanceKeychainScope() {
        let production = AppStorageLocations.keychainService(instanceId: "cloud-production")
        let standalone = AppStorageLocations.keychainService(instanceId: "standalone")
        let staging = AppStorageLocations.keychainService(
            instanceId: "cloud-staging",
            staging: true
        )
        #expect(production == "com.whalerein.ios.secrets.cloud-production")
        #expect(production == AppStorageLocations.keychainService(instanceId: "cloud-production"))
        #expect(standalone == "com.whalerein.ios.secrets.standalone")
        #expect(standalone != production)
        #expect(staging == "com.whalerein.ios.staging.secrets.cloud-staging")
    }

    @Test("reasoning visibility is read and written through the App section")
    func reasoningVisibility() throws {
        let (store, _) = try makeStore()
        #expect(!store.reasoningVisible)
        try store.setReasoningVisible(true)
        #expect(store.reasoningVisible)
    }

    @Test("the real binding bootstraps, activates, and restarts from the committed selection")
    func realBindingRoundTrip() throws {
        let dataDirectory = makeTempDirectory()
        let document = AppStorageLocations.appConfigurationURL(in: dataDirectory)
        let store = try AppConfigurationStore(documentPath: document.path)

        #expect(store.activeInstance.id == "cloud-production")
        #expect(Set(store.instances.map(\.id)) == ["cloud-production", "standalone"])
        #expect(store.activeInstance.environment == .production)
        #expect(FileManager.default.fileExists(atPath: document.path))
        #expect(FileManager.default.fileExists(atPath: store.activeInstance.configPath.path))

        // Invalid PEM is rejected by Core validation and leaves the document
        // and CA reference unchanged.
        #expect(throws: SessionServiceError.self) {
            try store.importCA(instanceID: "standalone", pem: "not a certificate")
        }
        #expect(store.configuration.instance(id: "standalone")?.hasStandaloneCA == false)

        try store.activate(id: "standalone")
        #expect(store.activeInstance.id == "standalone")
        #expect(FileManager.default.fileExists(atPath: store.activeInstance.configPath.path))

        // A fresh store over the same path reads the committed selection.
        let reopened = try AppConfigurationStore(documentPath: document.path)
        #expect(reopened.activeInstance.id == "standalone")
    }

    @Test("the fixture document contains Cloud production and Standalone")
    func fixtureDocumentInstances() {
        let configuration = fixtureConfiguration()
        #expect(configuration.instances.map(\.id) == ["cloud-production", "standalone"])
        #expect(configuration.activeInstanceID == "cloud-production")
        #expect(configuration.activeInstance?.environment == .production)
    }
}
