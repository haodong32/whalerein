// Cloud transitions: restore, system sheet, completion, directory and sign-out.

import XCTest
@testable import WhaleRein

final class CloudReduceTests: XCTestCase {
    private func account(_ email: String = "a@example.com") -> CloudAccount {
        CloudAccount(
            accountId: "acct-1",
            email: email,
            issuer: "issuer",
            subject: "subject",
            workspaceId: "workspace"
        )
    }

    private func host(_ id: String, deviceAccess: String = "ready") -> CloudDirectoryHost {
        CloudDirectoryHost(
            hostId: id,
            displayName: id,
            enrolled: true,
            online: true,
            presence: "online",
            observation: "observed",
            enrollmentState: "enrolled",
            pairingEligible: true,
            deviceAccess: deviceAccess
        )
    }

    func testRestoreWithNoAccountReturnsToSignedOut() {
        var state = CloudState()
        state = CloudReduce.reduce(state, .restoreStarted)
        XCTAssertEqual(state.phase, .restoring)
        state = CloudReduce.reduce(state, .restored(account: nil, logoutPending: true))
        XCTAssertEqual(state.phase, .signedOut)
        XCTAssertTrue(state.logoutPending)
        XCTAssertEqual(state.directory, .idle)
    }

    func testSignInThroughTheSheet() {
        var state = CloudState()
        state = CloudReduce.reduce(state, .signInStarted)
        XCTAssertEqual(state.phase, .starting)
        state = CloudReduce.reduce(state, .authorizing)
        XCTAssertEqual(state.phase, .authorizing)
        XCTAssertTrue(state.authPresented)
        state = CloudReduce.reduce(state, .completionStarted)
        XCTAssertEqual(state.phase, .completing)
        state = CloudReduce.reduce(state, .completed(account()))
        XCTAssertEqual(state.phase, .signedIn(account()))
        XCTAssertFalse(state.logoutPending)
        XCTAssertNil(state.errorCode)
    }

    func testCancelIsDistinctFromFailure() {
        var state = CloudState()
        state = CloudReduce.reduce(state, .signInStarted)
        state = CloudReduce.reduce(state, .signInCancelled)
        XCTAssertEqual(state.phase, .signedOut)
        XCTAssertTrue(state.wasCancelled)
        XCTAssertNil(state.errorCode)
        XCTAssertFalse(state.authPresented)

        state = CloudState()
        state = CloudReduce.reduce(state, .signInStarted)
        state = CloudReduce.reduce(state, .signInFailed(.unavailable))
        XCTAssertFalse(state.wasCancelled)
        XCTAssertEqual(state.errorCode, .unavailable)
    }

    func testDirectoryEmptyIsNotUnavailable() {
        var state = CloudState()
        state = CloudReduce.reduce(state, .completed(account()))
        state = CloudReduce.reduce(state, .directoryStarted)
        XCTAssertEqual(state.directory, .loading)
        state = CloudReduce.reduce(state, .directoryLoaded([]))
        XCTAssertEqual(state.directory, .empty)
        state = CloudReduce.reduce(state, .directoryLoaded([host("a")]))
        XCTAssertEqual(state.directory.hosts.map(\.hostId), ["a"])
        state = CloudReduce.reduce(state, .directoryFailed(.hostOffline))
        XCTAssertEqual(state.directory, .unavailable(.hostOffline))
    }

    func testConnectingIsOneHostAtATime() {
        var state = CloudState()
        state = CloudReduce.reduce(state, .completed(account()))
        state = CloudReduce.reduce(state, .connectStarted("a"))
        XCTAssertEqual(state.connectingHostId, "a")
        state = CloudReduce.reduce(state, .connectStarted("b"))
        XCTAssertEqual(state.connectingHostId, "a", "a second connect is ignored")
        state = CloudReduce.reduce(state, .connectFinished)
        XCTAssertNil(state.connectingHostId)
    }

    func testRevokedSessionClearsContentAndKeepsPendingSignOut() {
        var state = CloudState()
        state = CloudReduce.reduce(state, .completed(account()))
        state = CloudReduce.reduce(state, .directoryLoaded([host("a")]))
        state = CloudReduce.reduce(state, .sessionRevoked)
        XCTAssertEqual(state.phase, .signedOut)
        XCTAssertEqual(state.directory, .idle)
        XCTAssertTrue(state.logoutPending)
        XCTAssertEqual(state.errorCode, .revoked)
    }

    func testSignOutRecordsPendingRemoteResult() {
        var state = CloudState()
        state = CloudReduce.reduce(state, .completed(account()))
        state = CloudReduce.reduce(state, .signedOut(pending: true))
        XCTAssertEqual(state.phase, .signedOut)
        XCTAssertTrue(state.logoutPending)
        state = CloudReduce.reduce(state, .signedOutLocally(pending: false))
        XCTAssertFalse(state.logoutPending)
    }

    func testDirectoryLoadRequiresASignedInPhase() {
        var state = CloudState()
        state = CloudReduce.reduce(state, .directoryStarted)
        XCTAssertEqual(state.directory, .idle)
    }
}
