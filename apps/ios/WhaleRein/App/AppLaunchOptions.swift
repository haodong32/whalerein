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
    var seedPairedFixture: Bool
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
    /// Deliver a deterministic pending question with the first watch.
    var questionsOnOpen: Bool
    /// Select deterministic Cloud sign-in and directory fixtures.
    var seedCloudSignedIn: Bool
    /// Return a different account after a sign-out so a UI test can prove a
    /// clean sheet really changed accounts.
    var cloudSwitchAccounts: Bool
    var cloudStartFailure: Bool
    var cloudCompleteFailure: Bool
    var cloudRevoked: Bool
    /// Complete the fixture's ASWebAuthenticationSession sheet with a
    /// cancellation error instead of a callback.
    var cloudCancelSheet: Bool
    /// Keep the real system sheet and point the fixture provider at a local
    /// mock OIDC redirect page.
    var cloudSystemSheet: Bool
    var cloudDirectoryEmpty: Bool
    var cloudDirectoryUnavailable: Bool
    /// Deliver a deterministic ordered pending queue.
    var queueOnOpen: Bool
    /// Report queue reading as unsupported for the active Source.
    var queueUnsupported: Bool
    /// Make the refresh after a confirmed queue mutation fail.
    var queueMutationRefreshFails: Bool
    /// Fixture instance a deterministic launch opens.
    var fixtureInstance: FixtureInstance
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
        if case .fixtures = serviceMode {
            seedPairedFixture = resetState && flags.contains("-ui-fixture-paired")
        } else {
            seedPairedFixture = false
        }
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
        questionsOnOpen = flags.contains("-ui-questions")
        seedCloudSignedIn = flags.contains("-ui-cloud-signed-in")
        cloudSwitchAccounts = flags.contains("-ui-cloud-switch")
        cloudStartFailure = flags.contains("-ui-cloud-start-fail")
        cloudCompleteFailure = flags.contains("-ui-cloud-complete-fail")
        cloudRevoked = flags.contains("-ui-cloud-revoked")
        cloudCancelSheet = flags.contains("-ui-cloud-cancel-sheet")
        cloudSystemSheet = flags.contains("-ui-cloud-system-sheet")
        cloudDirectoryEmpty = flags.contains("-ui-cloud-empty")
        cloudDirectoryUnavailable = flags.contains("-ui-cloud-unavailable")
        queueOnOpen = flags.contains("-ui-queue")
        queueUnsupported = flags.contains("-ui-queue-unsupported")
        queueMutationRefreshFails = flags.contains("-ui-queue-refresh-fails")
        let cloudFlags = [
            "-ui-cloud-signed-in", "-ui-cloud-switch", "-ui-cloud-start-fail",
            "-ui-cloud-complete-fail", "-ui-cloud-revoked", "-ui-cloud-cancel-sheet",
            "-ui-cloud-system-sheet", "-ui-cloud-empty", "-ui-cloud-unavailable"
        ]
        fixtureInstance = cloudFlags.contains(where: flags.contains)
            ? .cloudProduction
            : .standalone
        approvalVariant = FixtureApprovalVariant(
            rawValue: defaults.string(forKey: "ui-approval-state") ?? ""
        ) ?? .current
    }

    static let fixtureInvitationHostId = "studio-mac"
}

// MARK: - Scenario bridge

#if DEBUG
// Temporary bridge while the legacy `-ui-*` flags still exist (deleted by task
// 6.4). A scenario-derived launch takes precedence over a flag-derived one, so
// UI tests and the Debug gallery can drive the app from the catalog.
extension LaunchOptions {
    init(
        scenario: Scenario,
        arguments: [String] = CommandLine.arguments,
        defaults: UserDefaults = .standard
    ) {
        if ScenarioCatalog.hasScenarioArgument(arguments) {
            self.init(scenarioOnly: scenario, arguments: arguments)
        } else {
            self.init(arguments: arguments, defaults: defaults)
        }
    }

    /// Maps one catalog entry onto the values the fixture services currently
    /// read. Every field mirrors a legacy flag so behavior is identical.
    init(scenarioOnly scenario: Scenario, arguments: [String]) {
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

    /// The scenario this launch was built from, for Debug surfaces that need to
    /// show what is currently applied.
    var appliedScenarioId: String? {
        ScenarioCatalog.argumentValue("-ui-scenario", in: arguments)
    }
}

#endif
