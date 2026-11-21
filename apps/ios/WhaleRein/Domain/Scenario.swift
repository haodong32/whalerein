// One scenario is the complete definition of a debuggable app state. The
// launch flag, fixture behavior, presentation override and destination screen
// are fields of the same value, so previews, UI tests and the Debug gallery
// cannot drift apart.
//
// Scenario ids are stable: they are passed as `-ui-scenario <id>` and asserted
// by UI tests. Changing an id is a breaking test change.

import Foundation

/// Which surface a scenario opens on after the runtime finishes launching.
enum ScenarioEntry: String, Equatable, Sendable, CaseIterable {
    case conversation
    case invitation
    case cloudSignIn
    case serviceSelection
    case settings
    case diagnostics
    case sessionSwitcher

    var title: String {
        switch self {
        case .conversation: "Conversation"
        case .invitation: "Invitation"
        case .cloudSignIn: "Cloud sign-in"
        case .serviceSelection: "Service selection"
        case .settings: "Settings"
        case .diagnostics: "Diagnostics"
        case .sessionSwitcher: "Session switcher"
        }
    }
}

/// Presentation-only overrides. These change how the same business state is
/// drawn, never what it is.
struct PresentationOverrides: Equatable, Sendable {
    /// Largest supported Dynamic Type size instead of the system default.
    var accessibilityTextSize = false
    /// Force the compact or wide layout regardless of the measured width.
    var layout: LayoutOverride?

    enum LayoutOverride: String, Equatable, Sendable {
        case compact
        case wide
    }
}

/// Deterministic service behavior for a scenario. These values replace the
/// legacy `-ui-*` flags one-for-one; the fixture services read them instead of
/// parsing `CommandLine.arguments`.
struct FixtureScenario: Equatable, Sendable {
    enum ApprovalVariant: String, Equatable, Sendable, CaseIterable {
        case current
        case expired
        case changed
        /// A pending request whose device-visible expiry already passed as if
        /// the device clock were fast. The fixture Host still reports it pending.
        case fastClock = "fast-clock"
        /// A pending request that expires shortly after the watch starts.
        case shortLived = "short-lived"
        /// The approval's session disappeared while the request was pending.
        case unavailable
    }

    var initiallyPaired = false
    var streamOnOpen = false
    var lateEventLeak = false
    var sendUncertain = false
    /// Echo the committed user message back from the Host with a Core message
    /// id, exercising optimistic-send deduplication.
    var sendEcho = false
    var approvalVariant: ApprovalVariant = .current
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
    /// Deliver a deterministic pending question with the first watch.
    var questionsOnOpen = false
    /// Select deterministic Cloud sign-in and directory fixtures.
    var cloudSignedIn = false
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
}

/// Launch-time overrides: which service kind runs, whether local state resets,
/// and whether the device is locked.
struct LaunchOverrides: Equatable, Sendable {
    enum ServiceMode: String, Equatable, Sendable {
        /// Real Core through the UniFFI binding.
        case live
        /// Deterministic in-process services.
        case fixtures
    }

    var serviceMode: ServiceMode = .live
    var resetState = false
    /// Simulate a locked Keychain so recovery starts from an unavailable store.
    var keychainLocked = false
    /// Use the staging bundle identity and Keychain scope.
    var staging = false
}

struct Scenario: Identifiable, Equatable, Sendable {
    enum Category: String, Equatable, Sendable, CaseIterable {
        case onboarding
        case conversation
        case approvals
        case questions
        case queue
        case history
        case recovery
        case sessionManagement = "session-management"
        case cloud
        case settings
        case diagnostics
        case layout

        var title: String {
            switch self {
            case .onboarding: "Onboarding"
            case .conversation: "Conversation"
            case .approvals: "Approvals"
            case .questions: "Questions"
            case .queue: "Queue"
            case .history: "History"
            case .recovery: "Recovery"
            case .sessionManagement: "Session management"
            case .cloud: "Cloud account"
            case .settings: "Settings"
            case .diagnostics: "Diagnostics"
            case .layout: "Layout"
            }
        }
    }

    /// Stable identifier used by `-ui-scenario` and UI tests.
    let id: String
    let category: Category
    let title: String
    /// What this scenario verifies; shown by the Debug gallery.
    let notes: String
    let launch: LaunchOverrides
    let fixture: FixtureScenario
    let presentation: PresentationOverrides
    let entry: ScenarioEntry

    init(
        id: String,
        category: Category,
        title: String,
        notes: String,
        launch: LaunchOverrides = LaunchOverrides(),
        fixture: FixtureScenario = FixtureScenario(),
        presentation: PresentationOverrides = PresentationOverrides(),
        entry: ScenarioEntry = .conversation
    ) {
        self.id = id
        self.category = category
        self.title = title
        self.notes = notes
        self.launch = launch
        self.fixture = fixture
        self.presentation = presentation
        self.entry = entry
    }

    /// The normal, unflagged launch: real Core, persisted state untouched.
    static let normal = Scenario(
        id: "normal",
        category: .onboarding,
        title: "Normal launch",
        notes: "Real Core through the binding with existing on-device state.",
        entry: .conversation
    )

    /// Default scenario for deterministic UI work: fresh state, fixtures.
    static let fixturesDefault = Scenario(
        id: "fixtures.default",
        category: .onboarding,
        title: "Fixtures, fresh state",
        notes: "Unpaired fixture instance with reset local state.",
        launch: LaunchOverrides(serviceMode: .fixtures, resetState: true),
        entry: .invitation
    )
}
