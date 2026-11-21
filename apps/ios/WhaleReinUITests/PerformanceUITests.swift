// Launch and first-conversation performance. The clock metric covers launch,
// runtime restore and the first rendered conversation for one scenario.

import XCTest

final class PerformanceUITests: WhaleReinUITestCase {
    func testColdLaunchAndFirstConversationRender() {
        measure(metrics: [XCTClockMetric()]) {
            launchScenario("onboarding.paired")
            waitFor("composer.field", timeout: 20)
            app.terminate()
        }
    }
}
