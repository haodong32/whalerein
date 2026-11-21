// Runtime activation: destination commit, service replacement, per-instance
// preference scope, and activation-generation fencing for late callbacks.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Instance activation")
@MainActor
struct InstanceActivationTests {
    private struct Harness {
        let app: AppModel
        let store: AppConfigurationStore
        let binding: StubAppConfigurationBinding
        let oldService: StubSessionService
        let newService: StubSessionService
        let destinationPreferences: PreferencesStore
    }

    private func makeHarness(
        factoryError: SessionServiceError? = nil,
        activateError: Error? = nil
    ) throws -> Harness {
        let dataDirectory = makeTempDirectory()
        let document = AppStorageLocations.appConfigurationURL(in: dataDirectory)
        let binding = StubAppConfigurationBinding(
            configuration: .fixture(documentURL: document)
        )
        binding.activateError = activateError
        let store = try AppConfigurationStore(documentPath: document.path, binding: binding)
        let sourcePreferences = PreferencesStore(
            directory: AppStorageLocations.instanceDirectory(
                in: dataDirectory,
                instanceId: "cloud-production"
            )
        )
        sourcePreferences.update {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-001"
        }
        let destinationPreferences = PreferencesStore(
            directory: AppStorageLocations.instanceDirectory(
                in: dataDirectory,
                instanceId: "standalone"
            )
        )
        let oldService = StubSessionService()
        oldService.instanceId = "cloud-production"
        let newService = StubSessionService()
        newService.instanceId = "standalone"
        newService.hostsHandler = { () async throws -> [HostSummary] in
            throw SessionServiceError(.notPaired)
        }
        let app = AppModel(
            service: oldService,
            preferences: sourcePreferences,
            dataDirectory: dataDirectory,
            options: testLaunchOptions(),
            configuration: store,
            activeInstance: store.activeInstance,
            serviceFactory: { instance in
                if let factoryError { throw factoryError }
                return instance.id == "standalone" ? newService : oldService
            },
            preferencesFactory: { instance in
                instance.id == "standalone" ? destinationPreferences : sourcePreferences
            }
        )
        return Harness(
            app: app,
            store: store,
            binding: binding,
            oldService: oldService,
            newService: newService,
            destinationPreferences: destinationPreferences
        )
    }

    @Test("a successful switch commits the selection and replaces the runtime")
    func successfulSwitch() async throws {
        let harness = try makeHarness()
        let generation = harness.app.activationGeneration

        let error = await harness.app.switchInstance(id: "standalone")

        #expect(error == nil)
        #expect(harness.app.activeInstance?.id == "standalone")
        #expect(harness.app.service === harness.newService)
        #expect(harness.app.service.instanceId == "standalone")
        #expect(harness.app.activationGeneration == generation + 1)
        #expect(harness.binding.activateCalls == ["standalone"])
        #expect(harness.oldService.stopWatchCount > 0)
        #expect(harness.app.preferences === harness.destinationPreferences)
    }

    @Test("a failed document write returns an error and keeps the old runtime")
    func failedWriteKeepsRuntime() async throws {
        let harness = try makeHarness(
            activateError: SessionServiceError(.configuration)
        )
        let error = await harness.app.switchInstance(id: "standalone")

        #expect(error == .configuration)
        #expect(harness.app.activeInstance?.id == "cloud-production")
        #expect(harness.app.service === harness.oldService)
        #expect(harness.oldService.stopWatchCount == 0)
        #expect(harness.app.instanceErrorKey == .errorConfiguration)
    }

    @Test("a service construction failure restores the committed selection")
    func failedServiceConstructionRollsBackSelection() async throws {
        let harness = try makeHarness(
            factoryError: SessionServiceError(.keychainUnavailable)
        )
        let error = await harness.app.switchInstance(id: "standalone")

        #expect(error == .keychainUnavailable)
        #expect(harness.app.activeInstance?.id == "cloud-production")
        #expect(harness.app.service === harness.oldService)
        // The binding committed the destination and then restored the previous
        // selection so startup and runtime cannot disagree.
        #expect(harness.binding.activateCalls == ["standalone", "cloud-production"])
    }

    @Test("late pairing and reset callbacks from the previous activation are ignored")
    func lateCallbacksAreFenced() async throws {
        let harness = try makeHarness()
        let oldPairing = harness.app.pairing
        let oldSettings = harness.app.settings
        #expect(await harness.app.switchInstance(id: "standalone") == nil)
        let pairedBefore = harness.app.isPaired
        let hostBefore = harness.app.selectedHostId
        let generation = harness.app.activationGeneration

        oldPairing.onPaired?("stale-host")
        oldSettings.onDeviceReset()

        #expect(harness.app.isPaired == pairedBefore)
        #expect(harness.app.selectedHostId == hostBefore)
        #expect(harness.app.activationGeneration == generation)
        #expect(harness.app.service === harness.newService)
    }

    @Test("an in-flight old launch cannot publish after activation")
    func inFlightLaunchIsFenced() async throws {
        let harness = try makeHarness()
        let gate = SuspendedResponse<[HostSummary]>()
        harness.oldService.hostsHandler = { try await gate.wait() }
        let launch = Task { await harness.app.bootstrapConversation() }
        await gate.waitUntilStarted()

        #expect(await harness.app.switchInstance(id: "standalone") == nil)
        gate.resume(with: FixtureCatalog.hosts)
        await launch.value

        #expect(harness.app.service === harness.newService)
        #expect(harness.app.conversation == nil)
        #expect(harness.app.rootState != .ready)
        #expect(harness.app.activeInstance?.id == "standalone")
    }

    @Test("Local is exposed and created only in a debug build flavor")
    func localOptionCreation() async throws {
        let harness = try makeHarness()
        let hidden = AppConfigurationStore.serviceOptions(
            for: harness.store.instances,
            showsStaging: false,
            showsLocal: false
        )
        #expect(!hidden.contains { $0.environment == .local })

        let options = AppConfigurationStore.serviceOptions(
            for: harness.store.instances,
            showsStaging: true,
            showsLocal: true
        )
        let local = try #require(options.first { $0.environment == .local })
        #expect(local.kind == .createLocal)

        // Local cannot be created without its imported mkcert root: the generic
        // creation path refuses instead of saving an instance Core rejects.
        let error = await harness.app.switchToService(local)
        #expect(error == .configuration)
        #expect(!harness.store.instances.contains { $0.id == "cloud-local" })

        // Importing the bundle creates the isolated instance and selects it.
        #expect(await harness.app.enableLocal(pem: "pem-bundle") == nil)
        #expect(harness.app.activeInstance?.id == "cloud-local")
        let created = try #require(harness.store.instances.first { $0.id == "cloud-local" })
        #expect(created.hasCloudCA)
        #expect(created.isLocal)

        // Removing the CA leaves the instance first and deletes it, because a
        // local instance cannot open without its CA.
        #expect(await harness.app.removeLocalCA() == nil)
        #expect(harness.app.activeInstance?.id == "cloud-production")
        #expect(!harness.store.instances.contains { $0.id == "cloud-local" })
    }

    @Test("Staging options are exposed only when the build flavor allows them")
    func stagingOptionCreation() async throws {
        let harness = try makeHarness()
        let options = AppConfigurationStore.serviceOptions(
            for: harness.store.instances,
            showsStaging: true
        )
        let staging = try #require(options.first { $0.environment == .staging })

        let error = await harness.app.switchToService(staging)

        #expect(error == nil, "switchToService returned \(String(describing: error))")
        #expect(harness.app.activeInstance?.id == "cloud-staging")
        #expect(harness.store.instances.contains { $0.id == "cloud-staging" })
    }
}
