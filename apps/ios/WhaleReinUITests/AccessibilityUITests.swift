// Localization, large text, dark appearance and VoiceOver label/value checks.

import XCTest

final class AccessibilityUITests: WhaleReinUITestCase {
    func testSimplifiedChineseCoreFlow() {
        launchApp(["-AppleLanguages", "(zh-Hans)", "-AppleLocale", "zh_Hans"])

        let title = waitFor("invitation.title")
        XCTAssertEqual(title.label, "配对你的 iPhone")
        completePairing()
        openConversation()

        let connection = waitFor("conversation.connectionStatus")
        XCTAssertEqual(connection.value as? String, "已连接")
        XCTAssertEqual(waitFor("composer.send").label, "发送")
        XCTAssertEqual(waitFor("approval.allowOnce").label, "允许一次")
        XCTAssertEqual(waitFor("composer.field").label, "向 Host 发送消息")
    }

    func testLargeTextDarkAppearanceAndVoiceOverLabels() {
        launchAndOpenConversation(
            scenario: "conversation.large-text",
            extraArguments: ["-AppleInterfaceStyle", "Dark"]
        )

        let send = waitFor("composer.send")
        XCTAssertEqual(send.label, "Send")
        let allow = waitFor("approval.allowOnce")
        XCTAssertEqual(allow.label, "Allow Once")
        XCTAssertEqual(allow.value as? String, "bash")
        let connection = waitFor("conversation.connectionStatus")
        XCTAssertFalse((connection.value as? String ?? "").isEmpty)

        scrollConversationUp(times: 3)
        let jump = waitFor("conversation.jumpToLatest")
        XCTAssertEqual(jump.label, "Jump to latest")
        tap(jump)
    }

    func testVoiceOverGroupingOnHostAndSessionRows() {
        launchAndOpenConversation()
        openSwitcher()

        let picker = waitFor("host.picker")
        let pickerValue = picker.value as? String ?? ""
        XCTAssertTrue(pickerValue.contains(Self.hostId))
        XCTAssertTrue(pickerValue.contains("Online"))
        XCTAssertTrue(pickerValue.contains("Selected"))
        XCTAssertGreaterThan(pickerValue.count, 20)

        chooseHost(Self.hostId)
        let session = waitFor("session.row.\(Self.sessionId)")
        XCTAssertTrue(session.label.contains("Release checklist"))
        XCTAssertTrue(session.label.contains("Idle") || session.label.contains("Running"))
    }

    func testSettingsExposesVersionsAndLanguageNote() {
        launchAndOpenConversation()
        tap(waitFor("conversation.settings"))

        let revision = waitFor("settings.coreRevision")
        XCTAssertTrue(revision.label.contains("fixture-add-ios-client"))
        XCTAssertTrue(waitFor("settings.languageNote").label.contains("system"))
        XCTAssertTrue(element("settings.fixtureData").exists)
        XCTAssertTrue(waitFor("settings.dataPath").label.contains("WhaleRein"))
    }
}
