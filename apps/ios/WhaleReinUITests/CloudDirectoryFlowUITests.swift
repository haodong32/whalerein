// Directory states and account switching flows over deterministic fixtures.

import XCTest

final class CloudDirectoryFlowUITests: WhaleReinUITestCase {
    func testEmptyDirectoryIsDistinctFromAnError() {
        let app = launchScenario("cloud.directory-empty")
        XCTAssertTrue(waitFor("cloud.account").exists)
        XCTAssertTrue(waitFor("cloud.directory.empty").exists)
        XCTAssertFalse(element("cloud.directory.unavailable").exists)
    }

    func testUnavailableDirectoryOffersRetry() {
        let app = launchScenario("cloud.directory-unavailable")
        XCTAssertTrue(waitFor("cloud.account").exists)
        XCTAssertTrue(waitFor("cloud.directory.unavailable").exists)
        XCTAssertTrue(app.buttons["cloud.directory.retry"].exists)
    }
}
