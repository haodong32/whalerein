// The single assembly point of the client. Everything the app needs to build a
// runtime — the active instance, the service for it, per-instance preferences,
// the configuration document, a clock and a UUID source — is created here and
// handed to feature models explicitly.
//
// No feature creates its own dependencies and no view reaches into the
// environment for a service.

import Foundation

/// Deterministic time source. Production passes the system clock; tests and
/// previews pass a fixed or advancing fake.
protocol AppClock: Sendable {
    var now: Date { get }
    func sleep(for duration: Duration) async throws
}

struct SystemClock: AppClock {
    var now: Date { Date() }

    func sleep(for duration: Duration) async throws {
        try await Task.sleep(for: duration)
    }
}

/// Everything a runtime needs for one launch. Built once from launch arguments
/// and the committed configuration document.
@MainActor
struct AppEnvironment {
    let appVersion: String
    /// Root of the per-app Application Support directory.
    let dataDirectory: URL
    /// The scenario this launch represents. Always non-nil; a normal live
    /// launch is `Scenario.normal`.
    let scenario: Scenario
    let clock: any AppClock
    let uuid: @Sendable () -> UUID
    /// Nil when the configuration document could not be opened; the runtime
    /// then presents recovery instead of a paired surface.
    let configuration: AppConfigurationStore?
    /// The instance the service was opened for, when there is one.
    let activeInstance: AppInstance?
    /// True when the fixture service (rather than Core) backs this runtime.
    let usesFixtures: Bool

    private let serviceFactory: @MainActor (AppInstance) async throws -> any SessionService
    private let preferencesFactory: (String, String) -> PreferencesStore

    init(
        appVersion: String,
        dataDirectory: URL,
        scenario: Scenario,
        clock: any AppClock,
        uuid: @escaping @Sendable () -> UUID,
        configuration: AppConfigurationStore?,
        activeInstance: AppInstance?,
        usesFixtures: Bool,
        serviceFactory: @escaping @MainActor (AppInstance) async throws -> any SessionService,
        preferencesFactory: @escaping (String, String) -> PreferencesStore
    ) {
        self.appVersion = appVersion
        self.dataDirectory = dataDirectory
        self.scenario = scenario
        self.clock = clock
        self.uuid = uuid
        self.configuration = configuration
        self.activeInstance = activeInstance
        self.usesFixtures = usesFixtures
        self.serviceFactory = serviceFactory
        self.preferencesFactory = preferencesFactory
    }

    /// Builds the service for any instance the user can switch to. Throws so a
    /// failed switch can keep the previous runtime alive.
    func makeService(for instance: AppInstance) async throws -> any SessionService {
        try await serviceFactory(instance)
    }

    /// Per-instance non-secret UI state (last Host/session). The file lives
    /// beside the instance's materialized configuration, never at the legacy
    /// app root.
    func makePreferences(instanceId: String, fallback: String) -> PreferencesStore {
        preferencesFactory(instanceId, fallback)
    }

}
