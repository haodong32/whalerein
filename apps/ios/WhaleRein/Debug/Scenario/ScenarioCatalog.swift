// The complete list of debuggable states. The Debug gallery, SwiftUI previews
// and UI tests all read this one catalog, so a state cannot exist in a test
// without also being inspectable by a human.
//
// Ids are stable and asserted by UI tests: `-ui-scenario <id>`.

import Foundation

#if DEBUG

enum ScenarioCatalog {
    /// Scenario used when no `-ui-scenario` argument was passed.
    static let fallback = Scenario.normal

    static let all: [Scenario] = onboarding + conversation + approvals + questions
        + queue + sessionManagement + cloud + settings + diagnostics

    static func scenario(id: String) -> Scenario? {
        all.first { $0.id == id }
    }

    static func grouped() -> [(category: Scenario.Category, scenarios: [Scenario])] {
        Scenario.Category.allCases.compactMap { category in
            let scenarios = all.filter { $0.category == category }
            guard !scenarios.isEmpty else { return nil }
            return (category, scenarios)
        }
    }

    /// Parses `-ui-scenario <id>`. Unknown ids are a programming error: they
    /// assert in Debug and fall back to the live scenario so a shipped build
    /// never fails to launch.
    static func resolve(arguments: [String]) -> Scenario {
        guard let id = argumentValue("-ui-scenario", in: arguments) else { return fallback }
        if let scenario = scenario(id: id) { return scenario }
        assertionFailure("Unknown -ui-scenario id: \(id)")
        return fallback
    }

    static func argumentValue(_ flag: String, in arguments: [String]) -> String? {
        guard let index = arguments.firstIndex(of: flag), index + 1 < arguments.count else {
            return nil
        }
        return arguments[index + 1]
    }

    static func hasScenarioArgument(_ arguments: [String]) -> Bool {
        arguments.contains("-ui-scenario")
    }

    // MARK: - Onboarding

    private static var onboarding: [Scenario] {
        [
            Scenario(
                id: "onboarding.unpaired",
                category: .onboarding,
                title: "Unpaired invitation",
                notes: "Fresh fixture instance with no paired device.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                entry: .invitation
            ),
            Scenario(
                id: "onboarding.paired",
                category: .onboarding,
                title: "Paired conversation",
                notes: "Fixture instance already paired, restoring the last Host/session.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true),
                entry: .conversation
            ),
            Scenario(
                id: "onboarding.service-selection",
                category: .onboarding,
                title: "Service selection",
                notes: "Debug-only Local/Staging service picker before onboarding.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                entry: .serviceSelection
            ),
            Scenario(
                id: "onboarding.keychain-locked",
                category: .onboarding,
                title: "Locked Keychain",
                notes: "Recoverable service starts from an unavailable secret store.",
                launch: LaunchOverrides(serviceMode: .live, resetState: true, keychainLocked: true),
                entry: .conversation
            ),
            Scenario(
                id: "onboarding.slow-pairing",
                category: .onboarding,
                title: "Slow pairing",
                notes: "Pairing start is slow enough to enqueue a second invitation link.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(slowPairing: true),
                entry: .invitation
            ),
            Scenario(
                id: "onboarding.staging",
                category: .onboarding,
                title: "Staging bundle",
                notes: "Staging bundle identity and Keychain scope.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true, staging: true),
                entry: .invitation
            )
        ]
    }

    // MARK: - Conversation

    private static var conversation: [Scenario] {
        [
            Scenario(
                id: "conversation.baseline",
                category: .conversation,
                title: "Conversation baseline",
                notes: "Paired fixture with a restored session and no streaming.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true)
            ),
            Scenario(
                id: "conversation.streaming",
                category: .conversation,
                title: "Streaming reply",
                notes: "First watch streams a deterministic reply.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, streamOnOpen: true)
            ),
            Scenario(
                id: "conversation.send-uncertain",
                category: .conversation,
                title: "Send uncertain",
                notes: "A send settles as uncertain and offers recovery.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, sendUncertain: true)
            ),
            Scenario(
                id: "conversation.send-echo",
                category: .conversation,
                title: "Send echo deduplication",
                notes: "The Host echoes the committed message with a Core message id.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, sendEcho: true)
            ),
            Scenario(
                id: "conversation.offline",
                category: .conversation,
                title: "Offline after watch reset",
                notes: "The Host reports offline after the first watch resets.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, offlineOnOpen: true)
            ),
            Scenario(
                id: "conversation.watch-ended",
                category: .conversation,
                title: "Watch ended",
                notes: "The watch ends as soon as it starts without revocation.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, watchEndedOnOpen: true)
            ),
            Scenario(
                id: "conversation.revoked",
                category: .conversation,
                title: "Remote revocation",
                notes: "A remote revocation arrives after the first watch starts.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, revokedOnOpen: true)
            ),
            Scenario(
                id: "conversation.late-event",
                category: .conversation,
                title: "Late event after close",
                notes: "A late event arrives after the conversation was closed.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, lateEventLeak: true)
            ),
            Scenario(
                id: "conversation.incompatible-protocol",
                category: .conversation,
                title: "Incompatible Host protocol",
                notes: "The Host reports a protocol major version this client cannot use.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, incompatibleProtocol: true)
            ),
            Scenario(
                id: "conversation.legacy-protocol",
                category: .conversation,
                title: "Legacy Host without version",
                notes: "The Host exposes no usable protocol version.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, legacyProtocol: true)
            ),
            Scenario(
                id: "conversation.large-text",
                category: .conversation,
                title: "Largest Dynamic Type",
                notes: "Accessibility text size applied to the conversation.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true),
                presentation: PresentationOverrides(accessibilityTextSize: true)
            ),
            Scenario(
                id: "conversation.compact-diagnostics",
                category: .conversation,
                title: "Conversation diagnostics entry",
                notes: "Debug diagnostics toolbar entry from the conversation.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true),
                entry: .diagnostics
            )
        ]
    }

    // MARK: - Approvals

    private static var approvals: [Scenario] {
        FixtureScenario.ApprovalVariant.allCases.map { variant in
            Scenario(
                id: "approvals.\(variant.rawValue)",
                category: .approvals,
                title: "Approval \(variant.rawValue)",
                notes: "Pending approval in the \(variant.rawValue) variant.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, approvalVariant: variant)
            )
        } + [
            Scenario(
                id: "approvals.refresh-after-decision",
                category: .approvals,
                title: "Refresh after decision",
                notes: "A pending-only refresh arrives after a decision; the decided card survives.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(
                    initiallyPaired: true,
                    approvalRefreshAfterDecision: true
                )
            )
        ]
    }

    // MARK: - Questions

    private static var questions: [Scenario] {
        [
            Scenario(
                id: "questions.pending",
                category: .questions,
                title: "Pending questions",
                notes: "A deterministic question batch arrives with the first watch.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, questionsOnOpen: true)
            )
        ]
    }

    // MARK: - Queue

    private static var queue: [Scenario] {
        [
            Scenario(
                id: "queue.pending",
                category: .queue,
                title: "Pending queue",
                notes: "An ordered pending queue is delivered with the first watch.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, queueOnOpen: true)
            ),
            Scenario(
                id: "queue.unsupported",
                category: .queue,
                title: "Queue unsupported",
                notes: "The Source reports queue reading as unsupported.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, queueUnsupported: true)
            ),
            Scenario(
                id: "queue.refresh-fails",
                category: .queue,
                title: "Queue refresh fails",
                notes: "The refresh after a confirmed mutation fails.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(
                    initiallyPaired: true,
                    queueOnOpen: true,
                    queueMutationRefreshFails: true
                )
            )
        ]
    }

    // MARK: - Session management

    private static var sessionManagement: [Scenario] {
        [
            Scenario(
                id: "sessions.compact",
                category: .sessionManagement,
                title: "Session switcher (compact)",
                notes: "Compact layout switcher sheet with Hosts and sessions.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true),
                entry: .sessionSwitcher
            ),
            Scenario(
                id: "sessions.wide",
                category: .sessionManagement,
                title: "Session sidebar (wide)",
                notes: "Wide layout with a persistent Host/session sidebar.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true),
                presentation: PresentationOverrides(layout: .wide)
            ),
            Scenario(
                id: "sessions.compact-layout",
                category: .sessionManagement,
                title: "Forced compact layout",
                notes: "Compact layout regardless of the measured width.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true),
                presentation: PresentationOverrides(layout: .compact)
            )
        ]
    }

    // MARK: - Cloud

    private static var cloud: [Scenario] {
        [
            Scenario(
                id: "cloud.signed-in",
                category: .cloud,
                title: "Cloud signed in",
                notes: "Deterministic Cloud account and computer directory.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(cloudSignedIn: true),
                entry: .cloudSignIn
            ),
            Scenario(
                id: "cloud.switch-accounts",
                category: .cloud,
                title: "Cloud switch accounts",
                notes: "Sign-out returns a different account.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(cloudSignedIn: true, cloudSwitchAccounts: true),
                entry: .cloudSignIn
            ),
            Scenario(
                id: "cloud.start-failure",
                category: .cloud,
                title: "Cloud start failure",
                notes: "Sign-in start fails before the system sheet is presented.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(cloudStartFailure: true),
                entry: .cloudSignIn
            ),
            Scenario(
                id: "cloud.complete-failure",
                category: .cloud,
                title: "Cloud completion failure",
                notes: "The callback completes with an error.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(cloudCompleteFailure: true),
                entry: .cloudSignIn
            ),
            Scenario(
                id: "cloud.revoked",
                category: .cloud,
                title: "Cloud session revoked",
                notes: "The Cloud session is revoked while the app is open.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(cloudSignedIn: true, cloudRevoked: true),
                entry: .cloudSignIn
            ),
            Scenario(
                id: "cloud.cancel-sheet",
                category: .cloud,
                title: "Cloud sheet cancelled",
                notes: "The system authentication sheet completes with a cancellation.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(cloudCancelSheet: true),
                entry: .cloudSignIn
            ),
            Scenario(
                id: "cloud.system-sheet",
                category: .cloud,
                title: "Real system sheet",
                notes: "The real ASWebAuthenticationSession against a local mock OIDC page.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(cloudSystemSheet: true),
                entry: .cloudSignIn
            ),
            Scenario(
                id: "cloud.directory-empty",
                category: .cloud,
                title: "Cloud directory empty",
                notes: "Signed in with no computers available.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(cloudSignedIn: true, cloudDirectoryEmpty: true),
                entry: .cloudSignIn
            ),
            Scenario(
                id: "cloud.directory-unavailable",
                category: .cloud,
                title: "Cloud directory unavailable",
                notes: "The directory read fails while signed in.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(cloudSignedIn: true, cloudDirectoryUnavailable: true),
                entry: .cloudSignIn
            )
        ]
    }

    // MARK: - Settings

    private static var settings: [Scenario] {
        [
            Scenario(
                id: "settings.baseline",
                category: .settings,
                title: "Settings",
                notes: "Settings sheet over a paired conversation.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true),
                entry: .settings
            ),
            Scenario(
                id: "settings.account",
                category: .settings,
                title: "Account and device",
                notes: "Device revoke, forget and scoped sign-out entries.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, cloudSignedIn: true),
                entry: .settings
            )
        ]
    }

    // MARK: - Diagnostics

    private static var diagnostics: [Scenario] {
        [
            Scenario(
                id: "diagnostics.baseline",
                category: .diagnostics,
                title: "Diagnostics report",
                notes: "Redacted report preview over a paired conversation.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true),
                entry: .diagnostics
            ),
            Scenario(
                id: "diagnostics.failure",
                category: .diagnostics,
                title: "Diagnostics failure",
                notes: "Preview fails so the retry branch is exercised.",
                launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
                fixture: FixtureScenario(initiallyPaired: true, diagnosticsFailure: true),
                entry: .diagnostics
            )
        ]
    }
}
#endif
