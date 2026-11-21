// Settings must export a redacted snapshot on demand and never upload.

import XCTest

final class LogExportUITests: WhaleReinUITestCase {
    func testSettingsPreparesLogSnapshotForSharing() {
        app.launchArguments = [
            "-ui-scenario", "settings.baseline",
            "-AppleLanguages", "(en)", "-AppleLocale", "en_US"
        ]
        app.launch()

        let export = scrollTo("settings.exportLogs")
        export.tap()
        XCTAssertTrue(
            waitFor("settings.shareLogs").waitForExistence(timeout: 10),
            "exporting must offer the system share action"
        )
    }
}
