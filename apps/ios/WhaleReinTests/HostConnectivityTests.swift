// The authoritative Host connection: mapping, seeding, event delivery and the
// single place the app learns whether a Host is reachable.

import Testing
@testable import WhaleRein

@Suite("Host connectivity")
@MainActor
struct HostConnectivityTests {
    @Test("Core states map to one presentation status; nil is never opened")
    func stateMapping() {
        #expect(HostConnectionStatus(state: "connected", code: nil) == .connected)
        #expect(HostConnectionStatus(state: "connecting", code: nil) == .connecting)
        #expect(HostConnectionStatus(state: "reconnecting", code: nil) == .reconnecting)
        #expect(HostConnectionStatus(state: "offline", code: nil) == .offline)
        #expect(HostConnectionStatus(state: "disconnected", code: nil) == .disconnected)
        // A terminal code wins over the raw state: the UI must explain it.
        #expect(
            HostConnectionStatus(state: "offline", code: "revoked")
                == .unavailable("revoked")
        )
        #expect(
            HostConnectionStatus(state: "offline", code: "protocol_version_incompatible")
                == .unavailable("protocol_version_incompatible")
        )

        let model = HostConnectivityModel()
        #expect(model.status(for: "studio-mac") == .unknown)
        #expect(!model.isOnline("studio-mac"))
        model.handle(
            hostEnvelope(hostId: "studio-mac", state: "connected")
        )
        #expect(model.status(for: "studio-mac") == .connected)
        #expect(model.status(for: "studio-mac", fallbackOnline: false) == .connected)
    }

    @Test("Monitoring seeds the snapshot and follows unscoped transitions")
    func monitoringSeedsAndFollowsEvents() async {
        let service = StubSessionService()
        service.setHostConnection(hostId: "studio-mac", state: "connecting")
        let model = HostConnectivityModel()
        model.startMonitoring(service: service, hostIds: ["studio-mac"])

        // Seeded before the first event so the chrome never flashes unknown.
        #expect(model.status(for: "studio-mac") == .connecting)
        #expect(service.startHostMonitorCalls == [["studio-mac"]])

        service.emitHostConnection(hostId: "studio-mac", state: "connected")
        await waitUntil { model.status(for: "studio-mac") == .connected }

        service.emitHostConnection(hostId: "studio-mac", state: "offline", code: "revoked")
        await waitUntil { model.status(for: "studio-mac") == .unavailable("revoked") }
        #expect(!model.isOnline("studio-mac"))

        model.stopMonitoring()
        #expect(service.stopHostMonitorCount == 1)
        await waitUntil { service.activeStreamCount == 0 }
        // The last snapshot stays readable so returning to the foreground can
        // render something until the fresh probe reports.
        #expect(model.status(for: "studio-mac") == .unavailable("revoked"))
    }

    @Test("Reset forgets every snapshot")
    func resetForgetsSnapshots() async {
        let service = StubSessionService()
        service.setHostConnection(hostId: "studio-mac", state: "connected")
        let model = HostConnectivityModel()
        model.startMonitoring(service: service, hostIds: ["studio-mac"])
        #expect(model.status(for: "studio-mac") == .connected)

        model.reset()
        #expect(model.status(for: "studio-mac") == .unknown)
        #expect(service.stopHostMonitorCount == 1)
    }

    @Test("Directory fallback applies only before the first snapshot")
    func presenceFallbackIsTemporary() {
        let model = HostConnectivityModel()
        // No snapshot yet: the directory's discovery hint is better than a
        // false offline flash.
        #expect(model.status(for: "studio-mac", fallbackOnline: true) == .connected)
        #expect(model.status(for: "studio-mac", fallbackOnline: false) == .offline)

        model.handle(hostEnvelope(hostId: "studio-mac", state: "offline"))
        // Once the authoritative value exists, presence never overrides it.
        #expect(model.status(for: "studio-mac", fallbackOnline: true) == .offline)
    }

    @Test("A conversation only sends while its Host is online")
    func sendGatingFollowsConnectivity() async {
        let service = StubSessionService()
        let connectivity = HostConnectivityModel()
        let model = await makeStartedConversation(service: service, connectivity: connectivity)
        model.draft = "hello"
        #expect(!model.canSend)

        connectivity.handle(hostEnvelope(hostId: "studio-mac", state: "connected"))
        #expect(model.canSend)

        connectivity.handle(hostEnvelope(hostId: "studio-mac", state: "reconnecting"))
        #expect(!model.canSend, "a reconnecting Host must not accept a send")
    }
}

private func hostEnvelope(hostId: String, state: String, code: String? = nil) -> SessionEventEnvelope {
    SessionEventEnvelope(
        hostId: hostId,
        sessionId: "",
        sequence: 1,
        event: .hostConnection(hostId: hostId, state: state, code: code, sinceUnixMs: 0)
    )
}
