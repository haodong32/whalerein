// Shared XCTest support for deterministic UI flows against the fixture app.

import XCTest

class WhaleReinUITestCase: XCTestCase {
    var app: XCUIApplication!

    static let invitationLink = "whalerein://pair?access=https%3A%2F%2Fstudio-mac.access.whalerein.test&token=inv_7F3A9C2E5B8D1A4F6C0E&host=studio-mac"
    static let hostId = "studio-mac"
    static let sessionId = "s-001"
    static let approvalId = "approval-release-s-001"

    override func setUpWithError() throws {
        continueAfterFailure = false
        // Existing scenarios assume portrait; the adaptive landscape scenario
        // rotates explicitly after launch.
        XCUIDevice.shared.orientation = .portrait
        app = XCUIApplication()
    }

    override func tearDownWithError() throws {
        app = nil
    }

    @discardableResult
    func launchApp(_ extraArguments: [String] = []) -> XCUIApplication {
        // Every flow starts from a catalog scenario. The legacy flag path is
        // gone; integration tests pass `-integration-fixtures` and keep the
        // real service.
        launchScenario("onboarding.unpaired", extraArguments: extraArguments)
    }

    func element(_ identifier: String) -> XCUIElement {
        app.descendants(matching: .any).matching(identifier: identifier).firstMatch
    }

    @discardableResult
    func waitFor(_ identifier: String, timeout: TimeInterval = 15, file: StaticString = #filePath, line: UInt = #line) -> XCUIElement {
        let element = self.element(identifier)
        XCTAssertTrue(
            element.waitForExistence(timeout: timeout),
            "Expected element \(identifier) to exist",
            file: file,
            line: line
        )
        return element
    }

    /// Scrolls the active scroll view until `identifier` exists. Settings grew
    /// past one screen, so tests must not assume every row is on screen.
    @discardableResult
    func scrollTo(_ identifier: String, maxSwipes: Int = 8) -> XCUIElement {
        let target = element(identifier)
        for _ in 0..<maxSwipes where !target.exists {
            app.swipeUp()
        }
        return target
    }

    func waitForLabel(_ label: String, timeout: TimeInterval = 15, file: StaticString = #filePath, line: UInt = #line) {
        let predicate = NSPredicate(format: "label == %@", label)
        let element = app.descendants(matching: .any).matching(predicate).firstMatch
        XCTAssertTrue(
            element.waitForExistence(timeout: timeout),
            "Expected an element labelled \(label)",
            file: file,
            line: line
        )
    }

    /// Taps a control, dismissing a keyboard or scrolling the frontmost content
    /// when the control is not yet hittable.
    func tap(_ element: XCUIElement, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertTrue(element.waitForExistence(timeout: 15), "Element does not exist", file: file, line: line)
        var attempts = 0
        while !element.isHittable && attempts < 5 {
            if element.exists, element.frame.minY < 140 {
                app.swipeDown()
            } else {
                app.swipeUp()
            }
            attempts += 1
        }
        XCTAssertTrue(element.isHittable, "Element is not hittable", file: file, line: line)
        element.tap()
    }

    func completePairing(file: StaticString = #filePath, line: UInt = #line) {
        let field = waitFor("invitation.field", file: file, line: line)
        tap(field, file: file, line: line)
        field.typeText(Self.invitationLink + "\n")

        let confirm = waitForEnabled("pairing.confirm", file: file, line: line)
        tap(confirm, file: file, line: line)
        // XCTest can leave the confirmation visible after a tap. Retry once
        // while the button is still actionable.
        if !confirm.waitForNonExistence(timeout: 3) {
            if confirm.isEnabled, confirm.isHittable {
                confirm.tap()
            }
            XCTAssertTrue(
                confirm.waitForNonExistence(timeout: 15),
                "Pairing confirmation did not close",
                file: file,
                line: line
            )
        }
        // Pairing lands directly in the restored conversation; there is no Host
        // list page in between.
        waitFor("composer.field", file: file, line: line)
    }

    func openSwitcher(file: StaticString = #filePath, line: UInt = #line) {
        tap(waitFor("conversation.switcher", file: file, line: line))
        waitFor("switcher.close", file: file, line: line)
    }

    func openConversation(
        hostId: String = WhaleReinUITestCase.hostId,
        sessionId: String = WhaleReinUITestCase.sessionId,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        openSwitcher(file: file, line: line)
        chooseHost(hostId, file: file, line: line)
        let session = waitFor("session.row.\(sessionId)", file: file, line: line)
        tap(session, file: file, line: line)
        waitFor("composer.field", file: file, line: line)
    }

    /// Selects a Host from the switcher's compact picker row. The picker opens
    /// a native menu, so accept either the menu item identifier or its label.
    func chooseHost(
        _ hostId: String,
        file: StaticString = #filePath,
        line: UInt = #line
    ) {
        let picker = waitFor("host.picker", file: file, line: line)
        tap(picker, file: file, line: line)
        let byIdentifier = app.descendants(matching: .any)
            .matching(identifier: "host.row.\(hostId)").firstMatch
        if byIdentifier.waitForExistence(timeout: 3) {
            byIdentifier.tap()
            return
        }
        let byLabel = app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", hostId)).firstMatch
        // A rotation or re-layout can dismiss the menu between the wait and
        // the tap; reopen it once before declaring the item missing.
        if !byLabel.waitForExistence(timeout: 2) {
            tap(picker, file: file, line: line)
        }
        XCTAssertTrue(
            byLabel.waitForExistence(timeout: 10),
            "Host menu item \(hostId) was not found",
            file: file,
            line: line
        )
        byLabel.tap()
    }

    func launchAndOpenConversation(_ extraArguments: [String] = []) {
        launchAndOpenConversation(scenario: "onboarding.paired", extraArguments: extraArguments)
    }

    /// Launches one catalog scenario. New and migrated tests use this instead
    /// of individual `-ui-*` flags.
    @discardableResult
    func launchScenario(_ id: String, extraArguments: [String] = []) -> XCUIApplication {
        var arguments = ["-ui-scenario", id]
        if !extraArguments.contains("-AppleLanguages") {
            arguments += ["-AppleLanguages", "(en)", "-AppleLocale", "en_US"]
        }
        arguments += extraArguments
        app.launchArguments = arguments
        if app.state != .notRunning {
            app.terminate()
        }
        app.launch()
        return app
    }

    func launchAndOpenConversation(scenario id: String, extraArguments: [String] = []) {
        launchScenario(id, extraArguments: extraArguments)
        waitFor("composer.field")
    }

    @discardableResult
    func waitForEnabled(
        _ identifier: String,
        timeout: TimeInterval = 15,
        file: StaticString = #filePath,
        line: UInt = #line
    ) -> XCUIElement {
        let element = waitFor(identifier, timeout: timeout, file: file, line: line)
        let enabled = NSPredicate(format: "enabled == true")
        let expectation = XCTNSPredicateExpectation(predicate: enabled, object: element)
        XCTAssertEqual(
            XCTWaiter().wait(for: [expectation], timeout: timeout),
            .completed,
            "Expected element \(identifier) to become enabled",
            file: file,
            line: line
        )
        return element
    }

    func countElements(label: String) -> Int {
        app.descendants(matching: .any)
            .matching(NSPredicate(format: "label == %@", label))
            .count
    }

    func assertNoElement(containing text: String, file: StaticString = #filePath, line: UInt = #line) {
        let predicate = NSPredicate(format: "label CONTAINS %@", text)
        let count = app.descendants(matching: .any).matching(predicate).count
        XCTAssertEqual(count, 0, "Unexpected element containing \(text)", file: file, line: line)
    }

    func firstVisibleMessageRow() -> XCUIElement? {
        let predicate = NSPredicate(format: "identifier BEGINSWITH %@", "message.")
        let query = app.descendants(matching: .any).matching(predicate)
        let screen = app.frame
        for index in 0..<min(query.count, 80) {
            let element = query.element(boundBy: index)
            guard element.exists else { continue }
            let frame = element.frame
            if frame.height > 8 && frame.minY > 140 && frame.maxY < screen.height - 260 {
                return element
            }
        }
        return nil
    }

    func scrollConversationUp(times: Int = 3) {
        let scroll = element("conversation.scroll")
        XCTAssertTrue(scroll.waitForExistence(timeout: 15))
        for _ in 0..<times {
            scroll.swipeDown()
        }
    }

    /// Scrolls the transcript until the element can actually receive a tap.
    /// A row can exist while still clipped below or above the visible range.
    func makeConversationElementHittable(_ element: XCUIElement, maxSwipes: Int = 6) {
        let scroll = self.element("conversation.scroll")
        for _ in 0..<maxSwipes where !element.isHittable {
            if scroll.exists {
                scroll.swipeUp()
            }
            Thread.sleep(forTimeInterval: 0.25)
        }
    }

    /// Returns an element, scrolling toward the newest transcript content when a
    /// lazy transcript has not realised a row at the bottom yet.
    /// Waits until an element is no longer in the hierarchy, re-querying each
    /// poll so a rebuilt view cannot keep a stale element alive.
    @discardableResult
    func waitForDisappearance(
        _ identifier: String,
        timeout: TimeInterval = 15
    ) -> Bool {
        let deadline = Date().addingTimeInterval(timeout)
        while Date() < deadline {
            if !element(identifier).exists { return true }
            RunLoop.current.run(until: Date().addingTimeInterval(0.2))
        }
        return !element(identifier).exists
    }

    func revealByScrolling(
        _ identifier: String,
        in scrollIdentifier: String = "conversation.scroll",
        attempts: Int = 5,
        file: StaticString = #filePath,
        line: UInt = #line
    ) -> XCUIElement {
        let direct = element(identifier)
        if direct.waitForExistence(timeout: 2) {
            return direct
        }
        let scroll = waitFor(scrollIdentifier, file: file, line: line)
        for _ in 0..<attempts {
            if element(identifier).exists {
                return element(identifier)
            }
            scroll.swipeUp()
            if element(identifier).waitForExistence(timeout: 1) {
                return element(identifier)
            }
        }
        return waitFor(identifier, file: file, line: line)
    }
}
