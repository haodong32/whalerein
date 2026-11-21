// Host protocol compatibility and Host-qualified session accessibility. A Host
// on an unsupported protocol line must produce explicit upgrade guidance, and a
// session row must name its Host so same-named sessions are unambiguous.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Protocol compatibility")
@MainActor
struct ProtocolCompatibilityTests {
    private func makeApp(
        service: StubSessionService,
        configure: (inout AppPreferences) -> Void = { $0.paired = true }
    ) -> AppShellModel {
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update(configure)
        return AppShellModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )
    }

    @Test("major-version parsing accepts the supported line and rejects a newer one")
    func versionParsing() {
        #expect(ProtocolCompatibility.major(of: "1") == 1)
        #expect(ProtocolCompatibility.major(of: "1.4") == 1)
        #expect(ProtocolCompatibility.major(of: "2") == 2)
        #expect(ProtocolCompatibility.major(of: "") == nil)
        #expect(ProtocolCompatibility.major(of: "unknown") == nil)

        #expect(ProtocolCompatibility.isCompatible("1"))
        #expect(ProtocolCompatibility.isCompatible("1.9"))
        #expect(!ProtocolCompatibility.isCompatible("2"))
        // An absent or unparseable version is an older peer that must upgrade,
        // not an unknown value that may be treated as compatible.
        #expect(!ProtocolCompatibility.isCompatible(""))
        #expect(!ProtocolCompatibility.isCompatible("0"))
        #expect(!ProtocolCompatibility.isCompatible("unknown"))
        #expect(ProtocolCompatibility.incompatibility(for: "2") == "2")
        #expect(ProtocolCompatibility.incompatibility(for: "") == "0")
        #expect(ProtocolCompatibility.incompatibility(for: "0") == "0")
        #expect(ProtocolCompatibility.isLegacyPeer(""))
        #expect(ProtocolCompatibility.isLegacyPeer("0"))
        #expect(!ProtocolCompatibility.isLegacyPeer("2"))
    }

    @Test("a newer Host protocol shows an incompatible root with upgrade guidance")
    func incompatibleHostBlocksConversation() async {
        let service = StubSessionService()
        let newer = HostSummary(
            hostId: "studio-mac",
            online: true,
            fingerprint: FixtureCatalog.fingerprint,
            protocolVersion: "2"
        )
        service.hostsHandler = { [newer] }
        let app = makeApp(service: service)

        await app.bootstrapConversation()

        #expect(app.rootState == .incompatible(hostId: "studio-mac", version: "2"))
        #expect(app.conversation == nil)
        #expect(app.selectedHostId == "studio-mac")
    }

    @Test("a Host without a usable protocol version is an old peer that must upgrade")
    func legacyHostBlocksConversation() async {
        let service = StubSessionService()
        let legacy = HostSummary(
            hostId: "studio-mac",
            online: true,
            fingerprint: FixtureCatalog.fingerprint,
            protocolVersion: "0"
        )
        service.hostsHandler = { [legacy] }
        let app = makeApp(service: service)

        await app.bootstrapConversation()

        #expect(app.rootState == .incompatible(hostId: "studio-mac", version: "0"))
        #expect(app.conversation == nil)
    }

    @Test("a non-retryable binding upgrade error becomes an upgrade-required root state")
    func bindingUpgradeError() async {
        let service = StubSessionService()
        service.hostsHandler = { throw SessionServiceError(.protocolVersionIncompatible) }
        let app = makeApp(service: service)

        await app.bootstrapConversation()

        #expect(app.rootState == .upgradeRequired(.protocolVersionIncompatible))
        #expect(app.conversation == nil)
    }

    @Test("protocol and capability wire codes map to distinct non-retryable app errors")
    func wireCodeMapping() {
        #expect(LiveSessionService.map(code: "protocol_version_incompatible") == .protocolVersionIncompatible)
        #expect(LiveSessionService.map(code: "unsupported_capability") == .unsupportedCapability)
        #expect(LiveSessionService.map(code: "unavailable") == .unavailable)
        // A rejected request shape is not a pairing failure.
        #expect(LiveSessionService.map(code: "invalid_input") == .invalidInput)
        #expect(L10nKey.errorKey(for: .invalidInput) == .errorInvalidInput)
        #expect(L10nKey.errorKey(for: .protocolVersionIncompatible) == .errorProtocolIncompatible)
        #expect(L10nKey.errorKey(for: .unsupportedCapability) == .errorUnsupportedCapability)
        #expect(L10nKey.errorKey(for: .protocolVersionIncompatible) != .errorUnavailable)
    }

    @Test("selecting an incompatible Host in the switcher replaces the conversation with guidance")
    func incompatibleHostFromSwitcher() async {
        let service = StubSessionService()
        let app = makeApp(service: service)
        await app.bootstrapConversation()
        #expect(app.conversation?.sessionId == "s-001")

        let newer = HostSummary(
            hostId: "travel-laptop",
            online: true,
            fingerprint: "other",
            protocolVersion: "2"
        )
        let online = FixtureCatalog.hosts[0]
        service.hostsHandler = { [online, newer] }
        await app.refreshSwitcher()
        app.showSwitcher = true
        await app.selectHostInSwitcher("travel-laptop")

        #expect(app.rootState == .incompatible(hostId: "travel-laptop", version: "2"))
        #expect(app.conversation == nil)
        #expect(!app.isOfflineCached)
    }

    @Test("session accessibility labels include the Host and distinguish same-named sessions")
    func hostQualifiedSessionLabels() {
        let studio = SessionAccessibility.rowLabel(
            title: "Build triage",
            status: "Idle",
            activity: "now",
            hostId: "studio-mac",
            isCurrent: false,
            selectedText: "Selected"
        )
        let laptop = SessionAccessibility.rowLabel(
            title: "Build triage",
            status: "Idle",
            activity: "now",
            hostId: "travel-laptop",
            isCurrent: false,
            selectedText: "Selected"
        )

        #expect(studio.contains("Host studio-mac"))
        #expect(laptop.contains("Host travel-laptop"))
        #expect(studio != laptop)
        #expect(studio.contains("Build triage"))
        #expect(studio.contains("Idle"))
    }

    @Test("session activity labels stay Host-qualified when a row is current")
    func currentSessionLabel() {
        let label = SessionAccessibility.rowLabel(
            title: "Release checklist",
            status: "Running",
            activity: "2 minutes ago",
            hostId: "studio-mac",
            isCurrent: true,
            selectedText: "Selected"
        )
        #expect(label.contains("Host studio-mac"))
        #expect(label.contains("Selected"))
    }
}
