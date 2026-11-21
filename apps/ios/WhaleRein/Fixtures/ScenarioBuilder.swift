// Builds the deterministic objects a scenario describes.
//
// Previews, the Debug gallery and tests share this path so a scenario is
// constructed exactly once, with the same service, draft store and pairing.

import Foundation

@MainActor
enum ScenarioBuilder {
    /// Launch values for one scenario.
    static func launchOptions(for scenario: Scenario) -> LaunchOptions {
        LaunchOptions(scenario: scenario)
    }

    /// A deterministic service for one scenario. The caller owns the directory.
    static func service(
        for scenario: Scenario,
        directory: URL,
        draftStore: (any DraftStore)? = nil,
        fileManager: FileManager = .default
    ) -> FixtureSessionService {
        let store: any DraftStore
        if let draftStore {
            store = draftStore
        } else if let fileStore = try? FileDraftStore(
            dataDirectory: directory,
            fileManager: fileManager
        ) {
            store = fileStore
        } else {
            store = InMemoryDraftStore()
        }
        return FixtureSessionService(
            options: launchOptions(for: scenario),
            draftStore: store,
            initiallyPaired: scenario.fixture.initiallyPaired,
            instanceId: scenario.launch.fixtureInstance.instanceId,
            serviceKind: scenario.launch.fixtureInstance == .cloudProduction ? .cloud : .standalone
        )
    }

    /// A conversation model over the scenario's deterministic service. The
    /// caller starts it with `await model.start()`.
    static func conversationModel(
        for scenario: Scenario,
        directory: URL,
        hostId: String = "studio-mac",
        sessionId: String = "s-001",
        title: String = "Release checklist"
    ) -> ConversationModel {
        ConversationModel(
            hostId: hostId,
            sessionId: sessionId,
            sessionTitle: title,
            service: service(for: scenario, directory: directory)
        )
    }

    /// A fresh temporary directory for a preview or test scenario.
    static func temporaryDirectory(fileManager: FileManager = .default) -> URL {
        let url = fileManager.temporaryDirectory
            .appendingPathComponent("WhaleReinScenario-\(UUID().uuidString)", isDirectory: true)
        try? fileManager.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }
}
