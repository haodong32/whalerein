// Business state for the Cloud sign-in surface.
//
// One phase covers restore, the system sheet and completion; one directory
// value covers the computer list. The flags that used to sit beside them
// (error, cancel, sheet visibility, pending sign-out, connecting host) are
// fields of the same value, so they cannot disagree with the phase.

import Foundation

struct CloudState: Equatable, Sendable {
    var phase: CloudSignInPhase = .signedOut
    var directory: CloudDirectoryState = .idle
    var errorCode: AppErrorCode?
    /// Core could not confirm the remote sign-out; it retries on the next call.
    var logoutPending = false
    /// The system authentication sheet is on screen.
    var authPresented = false
    /// The last attempt ended because the user cancelled the sheet.
    var wasCancelled = false
    /// The computer currently being connected through Core.
    var connectingHostId: String?

    var account: CloudAccount? { phase.account }
    var isSignedIn: Bool { phase.isSignedIn }
    var isConnecting: Bool { connectingHostId != nil }
    var directoryHosts: [CloudDirectoryHost] { directory.hosts }
}
