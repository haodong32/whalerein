// Diagnostics transitions: one explicit capture and one explicit share sheet.

import XCTest
@testable import WhaleRein

final class DiagnosticsReduceTests: XCTestCase {
    func testLoadPhases() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .loadStarted)
        XCTAssertEqual(state.load, .loading)
        XCTAssertEqual(state.captureGeneration, 1)
        state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        XCTAssertTrue(state.isLoaded)
        state = DiagnosticsReduce.reduce(state, .loadFailed(.hostOffline))
        XCTAssertEqual(state.loadError, .hostOffline)
    }

    func testShareRequiresALoadedCapture() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .shareStarted)
        XCTAssertEqual(state.share, .none)
    }

    func testSharePresentsOnlyAfterStaging() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        state = DiagnosticsReduce.reduce(state, .shareStarted)
        XCTAssertEqual(state.share, .staging)
        XCTAssertNil(state.share.fileURL)

        let url = URL(fileURLWithPath: "/tmp/report.json")
        state = DiagnosticsReduce.reduce(state, .shareStaged(url))
        XCTAssertEqual(state.share, .presented(url))
        XCTAssertEqual(state.shareCount, 1)
    }

    func testSecondShareTapWhilePresentedIsIgnored() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        state = DiagnosticsReduce.reduce(state, .shareStarted)
        state = DiagnosticsReduce.reduce(state, .shareStaged(URL(fileURLWithPath: "/tmp/a.json")))

        state = DiagnosticsReduce.reduce(state, .shareStarted)
        XCTAssertEqual(state.shareCount, 1)
        XCTAssertEqual(state.share.fileURL, URL(fileURLWithPath: "/tmp/a.json"))
    }

    func testShareFinishedKeepsStatusUntilDismissed() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        state = DiagnosticsReduce.reduce(state, .shareStarted)
        state = DiagnosticsReduce.reduce(state, .shareStaged(URL(fileURLWithPath: "/tmp/a.json")))

        state = DiagnosticsReduce.reduce(state, .shareFinished(completed: false))
        XCTAssertEqual(state.share, .finished(completed: false))
        XCTAssertNil(state.share.fileURL)

        // The sheet teardown must not erase the cancellation status.
        state = DiagnosticsReduce.reduce(state, .shareDismissed)
        XCTAssertEqual(state.share, .finished(completed: false))
        XCTAssertTrue(state.share.allowsNewShare)
    }

    func testDismissReturnsToNone() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        state = DiagnosticsReduce.reduce(state, .shareStarted)
        state = DiagnosticsReduce.reduce(state, .shareStaged(URL(fileURLWithPath: "/tmp/a.json")))
        state = DiagnosticsReduce.reduce(state, .shareFinished(completed: true))

        var dismissed = state
        dismissed = DiagnosticsReduce.reduce(dismissed, .shareDismissed)
        XCTAssertEqual(dismissed.share, .finished(completed: true))

        var plain = DiagnosticsState()
        plain = DiagnosticsReduce.reduce(plain, .loadSucceeded)
        plain = DiagnosticsReduce.reduce(plain, .shareStarted)
        plain = DiagnosticsReduce.reduce(plain, .shareStaged(URL(fileURLWithPath: "/tmp/a.json")))
        plain = DiagnosticsReduce.reduce(plain, .shareDismissed)
        XCTAssertEqual(plain.share, .none)
        XCTAssertTrue(plain.share.allowsNewShare)
    }

    func testShareFailureKeepsTheCaptureAndStateOneError() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        state = DiagnosticsReduce.reduce(state, .shareStarted)
        state = DiagnosticsReduce.reduce(state, .shareFailed(.internalError))
        XCTAssertEqual(state.shareError, .internalError)
        XCTAssertTrue(state.isLoaded)
        XCTAssertTrue(state.share.allowsNewShare)
    }

    func testANewCaptureInvalidatesAPreparedShare() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        state = DiagnosticsReduce.reduce(state, .shareStarted)
        state = DiagnosticsReduce.reduce(state, .shareStaged(URL(fileURLWithPath: "/tmp/a.json")))
        let generation = state.captureGeneration

        state = DiagnosticsReduce.reduce(state, .loadStarted)

        XCTAssertEqual(state.share, .none)
        XCTAssertEqual(state.captureGeneration, generation + 1)
    }
}