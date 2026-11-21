// Opening a Cloud computer: a host without this device's grant must connect
// first, and a successful open must stay on the conversation root.

import XCTest

final class CloudHostOpenFlowUITests: WhaleReinUITestCase {
    func testEnrolledHostWithoutGrantConnectsThenOpensAndStays() {
        let app = launchScenario("cloud.signed-in")

        waitFor("cloud.account")
        XCTAssertTrue(waitFor("cloud.host.connect.granted-mac").exists)
        XCTAssertFalse(element("cloud.host.open.granted-mac").exists)

        waitFor("cloud.host.connect.granted-mac").tap()

        // The conversation root must remain after the pairing bootstrap; a
        // not-paired bounce would return to the Cloud directory.
        let switcher = waitFor("conversation.switcher", timeout: 20)
        XCTAssertTrue(switcher.exists)
        XCTAssertFalse(element("cloud.account").exists)

        sleep(2)
        XCTAssertTrue(element("conversation.switcher").exists)
        XCTAssertFalse(element("cloud.account").exists)
    }
}
