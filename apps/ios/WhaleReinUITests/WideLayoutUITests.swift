// Wide-layout coverage on an iPad simulator. The same test is part of the
// iPhone UI plan but skips with an explicit device reason there; run it with:
//
//   apps/ios/scripts/test.sh UI 'platform=iOS Simulator,name=iPad Pro 13-inch (M5),OS=26.4.1'
//
// The wide sidebar is the same breakpoint used by iPad Split View and Stage
// Manager windows once their width is at least 720 pt.

import UIKit
import XCTest

final class WideLayoutUITests: WhaleReinUITestCase {
    func testIPadShowsPersistentSidebarAndSwitchesConversation() throws {
        guard UIDevice.current.userInterfaceIdiom == .pad else {
            throw XCTSkip(
                "Requires an iPad simulator destination (Split View/Stage Manager widths); "
                    + "run the UI plan with an iPad Pro destination."
            )
        }

        launchApp()
        completePairing()

        let sidebar = waitFor("session.sidebar", timeout: 20)
        XCTAssertTrue(sidebar.exists)
        XCTAssertFalse(
            element("conversation.switcher").exists,
            "The compact switcher control must be hidden while the wide sidebar is visible"
        )

        chooseHost("travel-laptop")
        let session = reveal("session.row.s-101")
        tap(session)
        waitFor("composer.field")
        sleep(2)

        XCTAssertTrue(waitFor("conversation.cached").exists)
        XCTAssertTrue(element("message.msg-0014").waitForExistence(timeout: 10))

        XCUIDevice.shared.orientation = .landscapeLeft
        XCTAssertTrue(waitFor("session.sidebar", timeout: 20).exists)
        XCTAssertTrue(element("message.msg-0014").waitForExistence(timeout: 10))

        XCUIDevice.shared.orientation = .portrait
    }

    private func reveal(_ identifier: String) -> XCUIElement {
        let list = app.collectionViews.firstMatch
        for _ in 0..<6 {
            let candidate = element(identifier)
            if candidate.exists {
                return candidate
            }
            if list.exists {
                list.swipeUp()
            } else {
                app.swipeUp()
            }
        }
        return waitFor(identifier)
    }
}
