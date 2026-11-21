// Verifies the Debug state gallery: it lists the one scenario catalog, applies
// a scenario by rebuilding the runtime, and leaves no separate state source.

import XCTest

final class StateGalleryUITests: WhaleReinUITestCase {
    func testGalleryListsCatalogAndAppliesScenario() {
        app.launchArguments = [
            "-ui-scenario", "onboarding.paired",
            "-AppleLanguages", "(en)", "-AppleLocale", "en_US"
        ]
        app.launch()

        waitFor("conversation.gallery").tap()
        XCTAssertTrue(waitFor("gallery.current").exists)

        let target = element("gallery.scenario.conversation.streaming")
        for _ in 0..<8 where !target.exists {
            app.swipeUp()
        }
        XCTAssertTrue(target.waitForExistence(timeout: 5), "gallery must list conversation.streaming")
        target.tap()

        // The rebuilt runtime opens the conversation again and the gallery is
        // gone; the applied scenario is the new environment.
        XCTAssertTrue(waitFor("conversation.settings").waitForExistence(timeout: 10))
        XCTAssertFalse(element("gallery.current").exists)
    }
}
