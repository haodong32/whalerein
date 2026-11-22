// The scenario builder must produce the same deterministic pieces the app and
// the gallery use.

import XCTest
@testable import WhaleRein

final class ScenarioBuilderTests: XCTestCase {
    @MainActor
    func testBuildsLaunchValuesFromTheScenario() throws {
        let scenario = try XCTUnwrap(ScenarioCatalog.scenario(id: "conversation.streaming"))
        let options = ScenarioBuilder.launchOptions(for: scenario)
        XCTAssertEqual(options.serviceMode, .fixtures)
        XCTAssertTrue(options.resetState)
        XCTAssertTrue(options.streamOnOpen)
        XCTAssertTrue(options.seedPairedFixture)
    }

    @MainActor
    func testServiceMatchesTheScenarioInstanceAndDraftStore() {
        let scenario = ScenarioCatalog.scenario(id: "cloud.signed-in")!
        let directory = ScenarioBuilder.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        let service = ScenarioBuilder.service(for: scenario, directory: directory)
        XCTAssertEqual(service.instanceId, "cloud-production")
        XCTAssertEqual(service.serviceKind, .cloud)
    }

    @MainActor
    func testConversationModelUsesTheScenarioService() {
        let scenario = ScenarioCatalog.scenario(id: "conversation.baseline")!
        let directory = ScenarioBuilder.temporaryDirectory()
        defer { try? FileManager.default.removeItem(at: directory) }
        let model = ScenarioBuilder.conversationModel(for: scenario, directory: directory)
        XCTAssertEqual(model.hostId, "studio-mac")
        XCTAssertEqual(model.sessionId, "s-001")
    }
}
