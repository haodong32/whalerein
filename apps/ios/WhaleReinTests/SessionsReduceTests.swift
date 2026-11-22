// Switcher transitions: Host selection and the cached session list.

import XCTest
@testable import WhaleRein

final class SessionsReduceTests: XCTestCase {
    private func host(_ id: String) -> HostSummary {
        HostSummary(
            hostId: id,
            online: true,
            fingerprint: "fp",
            protocolVersion: "1.0"
        )
    }

    private func session(_ id: String, updatedAt: Int64 = 0) -> SessionSummary {
        SessionSummary(
            sessionId: id,
            title: id,
            updatedAtUnixMs: updatedAt,
            cwd: "/work"
        )
    }

    func testLoadingHostsSelectsTheFirstWhenNothingIsRemembered() {
        var state = SessionsState()
        state = SessionsReduce.reduce(state, .hostsLoadStarted)
        XCTAssertTrue(state.hosts.isLoading)
        state = SessionsReduce.reduce(state, .hostsLoaded([host("a"), host("b")]))
        XCTAssertEqual(state.selectedHostId, "a")
    }

    func testRememberedHostSurvivesAReload() {
        var state = SessionsState()
        state = SessionsReduce.reduce(state, .hostsLoaded([host("a"), host("b")]))
        state = SessionsReduce.reduce(state, .hostSelected("b"))
        state = SessionsReduce.reduce(state, .hostsLoaded([host("a"), host("b")]))
        XCTAssertEqual(state.selectedHostId, "b")
    }

    func testVanishedSelectionFallsBackToTheFirstHost() {
        var state = SessionsState()
        state = SessionsReduce.reduce(state, .hostsLoaded([host("a"), host("b")]))
        state = SessionsReduce.reduce(state, .hostSelected("b"))
        state = SessionsReduce.reduce(state, .hostsLoaded([host("a")]))
        XCTAssertEqual(state.selectedHostId, "a")
    }

    func testSelectingAnUnknownHostIsIgnored() {
        var state = SessionsState()
        state = SessionsReduce.reduce(state, .hostsLoaded([host("a")]))
        state = SessionsReduce.reduce(state, .hostSelected("ghost"))
        XCTAssertEqual(state.selectedHostId, "a")
    }

    func testHostFailureKeepsOneError() {
        var state = SessionsState()
        state = SessionsReduce.reduce(state, .hostsFailed(.hostOffline))
        XCTAssertEqual(state.hosts.error, .hostOffline)
    }

    func testCachedSessionsAreMarkedInTheSameValue() {
        var state = SessionsState()
        state = SessionsReduce.reduce(state, .sessionsLoadStarted(hostId: "h1"))
        XCTAssertTrue(state.sessions.isLoading)
        XCTAssertFalse(state.showingCached)
        state = SessionsReduce.reduce(
            state,
            .sessionsLoaded([session("s1")], cached: true)
        )
        XCTAssertTrue(state.showingCached)
        XCTAssertEqual(state.hostId, "h1")
        XCTAssertEqual(state.sessionList.map(\.sessionId), ["s1"])
    }

    func testLiveLoadClearsTheCachedIndicator() {
        var state = SessionsState()
        state = SessionsReduce.reduce(state, .sessionsLoadStarted(hostId: "h1"))
        state = SessionsReduce.reduce(state, .sessionsLoaded([], cached: true))
        state = SessionsReduce.reduce(state, .sessionsLoaded([], cached: false))
        XCTAssertFalse(state.showingCached)
        XCTAssertTrue(state.isEmpty)
    }

    func testSessionFailureClearsCachedState() {
        var state = SessionsState()
        state = SessionsReduce.reduce(state, .sessionsLoaded([], cached: true))
        state = SessionsReduce.reduce(state, .sessionsFailed(.timeout))
        XCTAssertFalse(state.showingCached)
        XCTAssertEqual(state.sessions.error, .timeout)
    }
}
