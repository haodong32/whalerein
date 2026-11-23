// Launch values consumed by the fixture services and the runtime.
//
// Every value comes from a `Scenario`; the app no longer parses individual
// debug flags. `-integration-fixtures` is the integration plan's marker and
// keeps the real service.

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
    var serviceMode: ServiceMode = .live
    var resetState = false
    var seedPairedFixture = false
    var largeText = false
    var streamOnOpen = false
    var lateEventLeak = false
    var sendUncertain = false
    /// Echo the committed user message back from the Host with a Core message
    /// id, exercising optimistic-send deduplication.
    var sendEcho = false
    var approvalVariant: FixtureApprovalVariant = .current
    var staging = false
    /// Emit a remote-revocation error after the first watch starts.
    var revokedOnOpen = false
    /// End the first watch as soon as it starts, without a remote revocation.
    var watchEndedOnOpen = false
    /// Report the Host connection offline after the watch reset.
    var offlineOnOpen = false
    /// Report a Host protocol major version this client does not support.
    var incompatibleProtocol = false
    /// Report a legacy Host that does not expose a usable protocol version.
    var legacyProtocol = false
    /// Emit a pending-only approval refresh after a decision, like a real Core
    /// state event, to prove the decided card survives.
    var approvalRefreshAfterDecision = false
    /// Make pairing start slow enough for a UI test to enqueue a second link.
    var slowPairing = false
    /// Make the diagnostics preview fail so the retry branch is covered.
    var diagnosticsFailure = false
    /// Simulate a locked/unavailable Keychain at launch.
    var keychainLocked = false
    /// Deliver a deterministic pending question with the first watch.
    var questionsOnOpen = false
    var delayQuestionRemoval = false
    /// Select deterministic Cloud sign-in and directory fixtures.
    var seedCloudSignedIn = false
    /// Return a different account after a sign-out so a UI test can prove a
    /// clean sheet really changed accounts.
    var cloudSwitchAccounts = false
    var cloudStartFailure = false
    var cloudCompleteFailure = false
    var cloudRevoked = false
    /// Complete the fixture's ASWebAuthenticationSession sheet with a
    /// cancellation error instead of a callback.
    var cloudCancelSheet = false
    /// Keep the real system sheet and point the fixture provider at a local
    /// mock OIDC redirect page.
    var cloudSystemSheet = false
    var cloudDirectoryEmpty = false
    var cloudDirectoryUnavailable = false
    /// Deliver a deterministic ordered pending queue.
    var queueOnOpen = false
    /// Report queue reading as unsupported for the active Source.
    var queueUnsupported = false
    /// Make the refresh after a confirmed queue mutation fail.
    var queueMutationRefreshFails = false
    /// Fixture instance a deterministic launch opens.
    var fixtureInstance: FixtureInstance = .standalone
    var arguments: [String] = []

    /// Normal app launch: real Core, no overrides. The integration plan is the
    /// only caller that still inspects launch arguments, for its marker.
    init(arguments: [String] = CommandLine.arguments) {
        self.arguments = arguments
        serviceMode = .live
    }

    /// Maps one catalog entry onto the values the fixture services read.
    init(scenario: Scenario, arguments: [String] = []) {
        let fixture = scenario.fixture
        let launch = scenario.launch
        self.arguments = arguments
        self.serviceMode = switch launch.serviceMode {
        case .fixtures: .fixtures
        case .live: .live
        }
        self.resetState = launch.resetState
        self.seedPairedFixture = fixture.initiallyPaired
        self.largeText = scenario.presentation.accessibilityTextSize
        self.streamOnOpen = fixture.streamOnOpen
        self.lateEventLeak = fixture.lateEventLeak
        self.sendUncertain = fixture.sendUncertain
        self.sendEcho = fixture.sendEcho
        self.approvalVariant = switch fixture.approvalVariant {
        case .current: .current
        case .expired: .expired
        case .changed: .changed
        case .fastClock: .fastClock
        case .shortLived: .shortLived
        case .unavailable: .unavailable
        }
        self.staging = launch.staging
        self.revokedOnOpen = fixture.revokedOnOpen
        self.watchEndedOnOpen = fixture.watchEndedOnOpen
        self.offlineOnOpen = fixture.offlineOnOpen
        self.incompatibleProtocol = fixture.incompatibleProtocol
        self.legacyProtocol = fixture.legacyProtocol
        self.approvalRefreshAfterDecision = fixture.approvalRefreshAfterDecision
        self.slowPairing = fixture.slowPairing
        self.diagnosticsFailure = fixture.diagnosticsFailure
        self.keychainLocked = launch.keychainLocked
        self.questionsOnOpen = fixture.questionsOnOpen
        self.delayQuestionRemoval = fixture.delayQuestionRemoval
        self.seedCloudSignedIn = fixture.cloudSignedIn
        self.cloudSwitchAccounts = fixture.cloudSwitchAccounts
        self.cloudStartFailure = fixture.cloudStartFailure
        self.cloudCompleteFailure = fixture.cloudCompleteFailure
        self.cloudRevoked = fixture.cloudRevoked
        self.cloudCancelSheet = fixture.cloudCancelSheet
        self.cloudSystemSheet = fixture.cloudSystemSheet
        self.cloudDirectoryEmpty = fixture.cloudDirectoryEmpty
        self.cloudDirectoryUnavailable = fixture.cloudDirectoryUnavailable
        self.queueOnOpen = fixture.queueOnOpen
        self.queueUnsupported = fixture.queueUnsupported
        self.queueMutationRefreshFails = fixture.queueMutationRefreshFails
        self.fixtureInstance = launch.fixtureInstance
    }

    static let fixtureInvitationHostId = "studio-mac"
}
