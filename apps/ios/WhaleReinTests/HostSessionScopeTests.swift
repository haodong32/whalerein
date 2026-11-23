// Host and session scoping, sorting, continuation, cached state, and stale-watch
// event filtering.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Host and session scope")
@MainActor
struct HostSessionScopeTests {
    private func makePreferences() -> PreferencesStore {
        PreferencesStore(directory: makeTempDirectory())
    }

    private func hostEnvelope(hostId: String, state: String) -> SessionEventEnvelope {
        SessionEventEnvelope(
            hostId: hostId,
            sessionId: "",
            sequence: 1,
            event: .hostConnection(
                hostId: hostId,
                state: state,
                code: nil,
                sinceUnixMs: 0
            )
        )
    }

    @Test("host list loads authorized Hosts and remembers selection")
    func loadsHosts() async {
        let service = StubSessionService()
        let preferences = makePreferences()
        let model = HostListModel(service: service, preferences: preferences)
        await model.load()

        #expect(model.loadState == .loaded)
        #expect(model.hosts.map(\.hostId) == ["studio-mac", "travel-laptop"])
        model.select("travel-laptop")
        #expect(model.selectedHostId == "travel-laptop")
        #expect(preferences.value.lastHostId == "travel-laptop")
    }

    @Test("host load failure is localized")
    func hostLoadFailure() async {
        let service = StubSessionService()
        service.hostsHandler = { throw SessionServiceError(.hostOffline) }
        let model = HostListModel(service: service, preferences: makePreferences())
        await model.load()

        #expect(model.loadState == .failed(.hostOffline))
        #expect(model.errorKey == .errorHostOffline)
    }

    @Test("sessions are scoped to the Host and sorted by recent activity")
    func sessionsSorted() async {
        let service = StubSessionService()
        let model = SessionListModel(service: service, preferences: makePreferences())
        await model.load(hostId: "studio-mac")

        #expect(model.sessions.first?.sessionId == "s-001")
        #expect(model.sessions.map(\.sessionId) == ["s-001", "s-002", "s-003", "s-legacy"])
        #expect(model.isShowingCached == false)
        #expect(model.continueSession == nil)
    }

    @Test("archived sessions stay hidden until the archived view is requested")
    func archivedSessions() async {
        let service = StubSessionService()
        service.sessionsHandler = { _ in [
            SessionSummary(sessionId: "live", title: "Live", updatedAtUnixMs: 300, cwd: "/work/a"),
            SessionSummary(
                sessionId: "archived",
                title: "Archived",
                updatedAtUnixMs: 200,
                cwd: "/work/a",
                archived: true
            ),
        ] }
        let preferences = makePreferences()
        preferences.update {
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "archived"
        }
        let model = SessionListModel(service: service, preferences: preferences)
        await model.load(hostId: "studio-mac")

        #expect(model.archivedCount == 1)
        #expect(model.workspaceGroups.flatMap(\.sessions).map(\.sessionId) == ["live"])
        // An archived last session is not offered for continuation while hidden.
        #expect(model.continueSession == nil)

        model.setShowsArchived(true)
        #expect(model.workspaceGroups.flatMap(\.sessions).map(\.sessionId) == ["live", "archived"])
        #expect(model.continueSession?.sessionId == "archived")
        #expect(preferences.value.showsArchivedSessions)

        // The choice survives a fresh model over the same preferences.
        let restored = SessionListModel(service: service, preferences: preferences)
        #expect(restored.showsArchived)
        restored.setShowsArchived(false)
        #expect(!preferences.value.showsArchivedSessions)
    }

    @Test("an offline Host lists cached sessions with an explicit indicator")
    func cachedSessions() async {
        let service = StubSessionService()
        service.sessionsHandler = { _ in throw SessionServiceError(.hostOffline) }
        let model = SessionListModel(service: service, preferences: makePreferences())
        await model.load(hostId: "travel-laptop")

        #expect(model.loadState == .loaded)
        #expect(model.isShowingCached)
        #expect(model.sessions.map(\.sessionId) == ["s-101", "s-102"])
    }

    @Test("an empty session list reports the loaded empty state")
    func emptySessions() async {
        let service = StubSessionService()
        service.sessionsHandler = { _ in [] }
        let model = SessionListModel(service: service, preferences: makePreferences())
        await model.load(hostId: "studio-mac")

        #expect(model.isEmpty)
        #expect(model.loadState == .loaded)
    }

    @Test("the last opened session is offered only for its own Host")
    func continuationIsHostScoped() async {
        let service = StubSessionService()
        let preferences = makePreferences()
        let model = SessionListModel(service: service, preferences: preferences)
        await model.load(hostId: "studio-mac")
        let session = model.sessions[1]
        model.rememberOpened(session)

        #expect(model.continueSession?.sessionId == session.sessionId)
        #expect(model.isLastOpened(session))

        await model.load(hostId: "travel-laptop")
        #expect(model.continueSession == nil)
        #expect(!model.isLastOpened(session))
    }

    @Test("a late event for another Host never enters the conversation")
    func staleWatchFiltering() async throws {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service, hostId: "studio-mac", sessionId: "s-001")

        model.handle(
            SessionEventEnvelope(
                hostId: "travel-laptop",
                sessionId: "s-101",
                sequence: 99,
                event: .reset(messages: [FixtureCatalog.conversation(sessionId: "s-101").first!], approvals: [])
            )
        )
        #expect(model.messages.isEmpty)

        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 99,
                event: .reset(messages: FixtureCatalog.conversation(sessionId: "s-001"), approvals: [])
            )
        )
        #expect(model.messages.count == FixtureCatalog.transcriptCount)

        model.handle(
            SessionEventEnvelope(
                hostId: "travel-laptop",
                sessionId: "s-101",
                sequence: 100,
                event: .preview(attemptId: "leak", text: "LATE-EVENT-LEAK", replace: true, incomplete: false)
            )
        )
        #expect(model.livePreview == nil)
    }

    @Test("duplicate and older event sequences are dropped")
    func sequenceFiltering() async throws {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        let first = FixtureCatalog.conversation(sessionId: "s-001").first!
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 10,
                event: .message(first)
            )
        )
        #expect(model.messages.count == 1)

        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 10,
                event: .message(first)
            )
        )
        #expect(model.messages.count == 1)

        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 4,
                event: .message(FixtureCatalog.conversation(sessionId: "s-001")[1])
            )
        )
        #expect(model.messages.count == 1)
    }

    @Test("Host connection is Host-scoped and independent of the watched session")
    func connectionScoping() async throws {
        let service = StubSessionService()
        // Start from "never opened": no snapshot exists yet, so Send is gated
        // even though a watch is active. It must not be treated as offline.
        let connectivity = HostConnectivityModel()
        let model = await makeStartedConversation(service: service, connectivity: connectivity)
        model.draft = "hello"
        #expect(connectivity.status(for: "studio-mac") == .unknown)
        #expect(!model.canSend, "Send needs the authoritative Host connection")

        connectivity.handle(hostEnvelope(hostId: "travel-laptop", state: "offline"))
        #expect(connectivity.status(for: "studio-mac") == .unknown)
        #expect(!model.canSend)

        connectivity.handle(hostEnvelope(hostId: "studio-mac", state: "connected"))
        #expect(connectivity.status(for: "studio-mac") == .connected)
        #expect(model.canSend)

        // A different Host going offline never changes this conversation's Host.
        connectivity.handle(hostEnvelope(hostId: "travel-laptop", state: "offline"))
        #expect(connectivity.status(for: "studio-mac") == .connected)
        #expect(model.canSend)

        connectivity.handle(hostEnvelope(hostId: "studio-mac", state: "offline"))
        #expect(connectivity.status(for: "studio-mac") == .offline)
        #expect(!model.canSend)
    }
}
