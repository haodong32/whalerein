// Diagnostics transitions: load and the two-step share flow.

import XCTest
@testable import WhaleRein

final class DiagnosticsReduceTests: XCTestCase {
    func testLoadPhases() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .loadStarted)
        XCTAssertEqual(state.load, .loading)
        state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        XCTAssertTrue(state.isLoaded)
        state = DiagnosticsReduce.reduce(state, .loadFailed(.hostOffline))
        XCTAssertEqual(state.loadError, .hostOffline)
    }

    func testShareRequiresALoadedReport() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .shareRequested)
        XCTAssertEqual(state.share, .none)
    }

    func testShareWritesOnlyAfterConfirm() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        state = DiagnosticsReduce.reduce(state, .shareRequested)
        XCTAssertEqual(state.share, .confirming)
        state = DiagnosticsReduce.reduce(state, .shareStarted)
        XCTAssertEqual(state.share, .exporting)
        let url = URL(fileURLWithPath: "/tmp/report.json")
        state = DiagnosticsReduce.reduce(state, .shareExported(url))
        XCTAssertEqual(state.share, .ready(url))
        XCTAssertEqual(state.exportCount, 1)
    }

    func testCancelOnlyLeavesConfirmation() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        state = DiagnosticsReduce.reduce(state, .shareRequested)
        state = DiagnosticsReduce.reduce(state, .shareCancelled)
        XCTAssertEqual(state.share, .cancelled)
    }

    func testShareFailureKeepsOneError() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .shareFailed(.internalError))
        XCTAssertEqual(state.shareError, .internalError)
    }

    func testDismissReturnsToNone() {
        var state = DiagnosticsState()
        state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        state = DiagnosticsReduce.reduce(state, .shareRequested)
        state = DiagnosticsReduce.reduce(state, .shareDismissed)
        XCTAssertEqual(state.share, .none)
    }
}
