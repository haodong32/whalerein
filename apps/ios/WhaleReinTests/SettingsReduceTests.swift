// Settings transitions: confirmation, destructive action and log export.

import XCTest
@testable import WhaleRein

final class SettingsReduceTests: XCTestCase {
    func testForgetRequiresConfirmationThenPerforms() {
        var state = SettingsState()
        state = SettingsReduce.reduce(state, .confirmationRequested(.forgetDevice))
        XCTAssertEqual(state.phase, .confirming(.forgetDevice))
        XCTAssertFalse(state.isBusy)

        state = SettingsReduce.reduce(state, .performStarted(.forgetDevice))
        XCTAssertEqual(state.phase, .performing(.forgetDevice))
        XCTAssertTrue(state.isBusy)

        state = SettingsReduce.reduce(state, .performSucceeded(.forgetDevice))
        XCTAssertEqual(state.phase, .finished(.forgetDevice))
        XCTAssertEqual(state.statusKey, .settingsForgotten)
        XCTAssertNil(state.errorCode)
    }

    func testPerformWithoutConfirmationIsIgnored() {
        var state = SettingsState()
        state = SettingsReduce.reduce(state, .performStarted(.revokeDevice))
        XCTAssertEqual(state.phase, .idle)
    }

    func testSecondConfirmationWhileBusyIsIgnored() {
        var state = SettingsState()
        state = SettingsReduce.reduce(state, .confirmationRequested(.forgetDevice))
        state = SettingsReduce.reduce(state, .performStarted(.forgetDevice))
        state = SettingsReduce.reduce(state, .confirmationRequested(.revokeDevice))
        XCTAssertEqual(state.phase, .performing(.forgetDevice))
    }

    func testFailureReturnsToIdleWithOneError() {
        var state = SettingsState()
        state = SettingsReduce.reduce(state, .confirmationRequested(.revokeDevice))
        state = SettingsReduce.reduce(state, .performStarted(.revokeDevice))
        state = SettingsReduce.reduce(state, .performFailed(.keychainUnavailable))
        XCTAssertEqual(state.phase, .idle)
        XCTAssertEqual(state.errorCode, .keychainUnavailable)
        XCTAssertEqual(state.errorKey, .errorKeychainUnavailable)
    }

    func testCancelClearsTheConfirmationOnly() {
        var state = SettingsState()
        state = SettingsReduce.reduce(state, .confirmationRequested(.forgetDevice))
        state = SettingsReduce.reduce(state, .confirmationCancelled)
        XCTAssertEqual(state.phase, .idle)
        XCTAssertNil(state.pendingConfirmation)
    }

    func testLogExportPhases() {
        var state = SettingsState()
        state = SettingsReduce.reduce(state, .logExportStarted)
        XCTAssertEqual(state.logExport, .preparing)
        let url = URL(fileURLWithPath: "/tmp/whalerein-log.jsonl")
        state = SettingsReduce.reduce(state, .logExportReady(url))
        XCTAssertEqual(state.logExport, .ready(url))
        state = SettingsReduce.reduce(state, .logExportFailed)
        XCTAssertEqual(state.logExport, .failed)
    }

    func testFinishedActionIsTerminalUntilReset() {
        var state = SettingsState()
        state = SettingsReduce.reduce(state, .confirmationRequested(.forgetDevice))
        state = SettingsReduce.reduce(state, .performStarted(.forgetDevice))
        state = SettingsReduce.reduce(state, .performSucceeded(.forgetDevice))
        state = SettingsReduce.reduce(state, .performStarted(.revokeDevice))
        XCTAssertEqual(state.phase, .finished(.forgetDevice))
    }
}
