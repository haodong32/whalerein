// Launch-argument handling. UI tests select deterministic fixtures, reset state,
// force an accessibility text size, and script streaming through these values.

import Foundation

enum FixtureApprovalVariant: String, Sendable {
    case current
    case expired
    case changed
    /// A pending request whose device-visible expiry already passed as if the
    /// device clock were fast. The fixture Host still reports it pending.
    case fastClock = "fast-clock"
    /// A pending request that expires shortly after the watch starts.
    case shortLived = "short-lived"
    /// The approval's session disappeared while the request was pending.
    case unavailable
}

enum ServiceMode: Sendable {
    case fixtures
    /// Real Core through the UniFFI binding, configured from the environment.
    case live
    case unavailable
}

struct LaunchOptions: Sendable {
    var serviceMode: ServiceMode
    var resetState: Bool
    var largeText: Bool
    var streamOnOpen: Bool
    var lateEventLeak: Bool
    var sendUncertain: Bool
    /// Echo the committed user message back from the Host with a Core message
    /// id, exercising optimistic-send deduplication.
    var sendEcho: Bool
    var approvalVariant: FixtureApprovalVariant
    var staging: Bool
    /// Emit a remote-revocation error after the first watch starts.
    var revokedOnOpen: Bool
    /// End the first watch as soon as it starts, without a remote revocation.
    var watchEndedOnOpen: Bool
    /// Report the Host connection offline after the watch reset.
    var offlineOnOpen: Bool
    /// Report a Host protocol major version this client does not support.
    var incompatibleProtocol: Bool
    /// Report a legacy Host that does not expose a usable protocol version.
    var legacyProtocol: Bool
    /// Emit a pending-only approval refresh after a decision, like a real Core
    /// state event, to prove the decided card survives.
    var approvalRefreshAfterDecision: Bool
    /// Make pairing start slow enough for a UI test to enqueue a second link.
    var slowPairing: Bool
    /// Make the diagnostics preview fail so the retry/error branch is covered.
    var diagnosticsFailure: Bool
    /// Simulate a locked/unavailable Keychain at launch.
    var keychainLocked: Bool
    var arguments: [String]

    init(arguments: [String] = CommandLine.arguments, defaults: UserDefaults = .standard) {
        self.arguments = arguments
        let flags = Set(arguments)
        if flags.contains("-integration-fixtures") {
            serviceMode = .live
        } else if flags.contains("-ui-fixtures") {
            serviceMode = .fixtures
        } else {
            // A normal app launch talks to the real Core through the binding.
            serviceMode = .live
        }
        resetState = flags.contains("-ui-test-reset")
        largeText = flags.contains("-ui-large-text")
        streamOnOpen = flags.contains("-ui-stream-on-open")
        lateEventLeak = flags.contains("-ui-late-event")
        sendUncertain = flags.contains("-ui-send-uncertain")
        sendEcho = flags.contains("-ui-send-echo")
        staging = flags.contains("-ui-staging") || (Bundle.main.bundleIdentifier?.hasSuffix(".staging") ?? false)
        revokedOnOpen = flags.contains("-ui-revoked-on-open")
        watchEndedOnOpen = flags.contains("-ui-watch-ended")
        offlineOnOpen = flags.contains("-ui-offline")
        incompatibleProtocol = flags.contains("-ui-incompatible-protocol")
        legacyProtocol = flags.contains("-ui-legacy-protocol")
        approvalRefreshAfterDecision = flags.contains("-ui-approval-refresh-after-decision")
        slowPairing = flags.contains("-ui-slow-pairing")
        diagnosticsFailure = flags.contains("-ui-diagnostics-failure")
        keychainLocked = flags.contains("-ui-keychain-locked")
        approvalVariant = FixtureApprovalVariant(
            rawValue: defaults.string(forKey: "ui-approval-state") ?? ""
        ) ?? .current
    }

    static let fixtureInvitationHostId = "studio-mac"
}
