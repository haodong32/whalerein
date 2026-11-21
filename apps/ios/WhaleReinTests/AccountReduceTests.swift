// Account transitions: remote reads and scoped revocation.

import XCTest
@testable import WhaleRein

final class AccountReduceTests: XCTestCase {
    private func status(defaultHost: String? = nil) -> InstanceStatus {
        InstanceStatus(
            service: .cloud,
            testingEnvironment: nil,
            instanceId: "i",
            accountState: .signedIn,
            remoteSignoutPending: false,
            defaultHost: defaultHost,
            deviceAccess: .ready,
            configPath: "/tmp/config.toml",
            dataDir: "/tmp"
        )
    }

    func testStatusLoadPhases() {
        var state = AccountState()
        state = AccountReduce.reduce(state, .statusLoadStarted)
        XCTAssertTrue(state.status.isLoading)
        state = AccountReduce.reduce(state, .statusLoaded(status(defaultHost: "studio-mac")))
        XCTAssertEqual(state.status.value?.defaultHost, "studio-mac")
        XCTAssertEqual(state.defaultHostId, "studio-mac")
        state = AccountReduce.reduce(state, .statusFailed(.hostOffline))
        XCTAssertEqual(state.status.error, .hostOffline)
    }

    func testReloadDoesNotFlashLoadingAfterSuccess() {
        var state = AccountState()
        state = AccountReduce.reduce(state, .statusLoaded(status()))
        state = AccountReduce.reduce(state, .statusLoadStarted)
        XCTAssertTrue(state.status.isLoaded)
    }

    func testSessionsAndDefaultHost() {
        var state = AccountState()
        state = AccountReduce.reduce(state, .sessionsLoadStarted)
        XCTAssertTrue(state.sessions.isLoading)
        state = AccountReduce.reduce(state, .sessionsLoaded([]))
        XCTAssertTrue(state.sessions.isLoaded)
        state = AccountReduce.reduce(state, .defaultHostSelected("host-1"))
        XCTAssertEqual(state.defaultHostId, "host-1")
        state = AccountReduce.reduce(state, .defaultHostFailed(.hostOffline))
        XCTAssertEqual(state.defaultHostError, .hostOffline)
        state = AccountReduce.reduce(state, .defaultHostSucceeded)
        XCTAssertNil(state.defaultHostError)
    }

    func testRevocationSettlesAndUpdatesListedSessions() {
        let sessions = [
            AccountAppSession(
                sessionId: "a", instanceId: "i", expiresAt: 1, state: .active, isCurrent: true
            ),
            AccountAppSession(
                sessionId: "b", instanceId: "i", expiresAt: 2, state: .active, isCurrent: false
            )
        ]
        var state = AccountState()
        state = AccountReduce.reduce(state, .sessionsLoaded(sessions))
        state = AccountReduce.reduce(state, .revokeStarted(.one("a")))
        XCTAssertTrue(state.isRevoking)
        let result = AccountSessionRevocation(
            scope: .one("a"), state: .revoked, affectedSessionIds: ["a"]
        )
        state = AccountReduce.reduce(state, .revokeSettled(result))
        XCTAssertEqual(state.revocation, .settled(result))
        XCTAssertEqual(state.sessions.value?.first?.state, .revoked)
        XCTAssertEqual(state.sessions.value?.last?.state, .active)
        XCTAssertFalse(state.isRevoking)
    }

    func testRevocationFailureKeepsOneError() {
        var state = AccountState()
        state = AccountReduce.reduce(state, .revokeStarted(.all))
        state = AccountReduce.reduce(state, .revokeFailed(.all, .timeout))
        XCTAssertEqual(state.revocation, .failed(.timeout))
        XCTAssertNil(state.revocation.scope)
    }

    func testMessageIsClearedByAnEvent() {
        var state = AccountState()
        state = AccountReduce.reduce(state, .message(.settingsDefaultSaved))
        XCTAssertEqual(state.statusKey, .settingsDefaultSaved)
        state = AccountReduce.reduce(state, .message(nil))
        XCTAssertNil(state.statusKey)
    }
}
