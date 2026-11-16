// Composer send/stop behavior: idle Send starts work, running sends default to
// Steer, Queue is honored, unsupported modes keep the draft, and uncertain
// results reconcile exactly once.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Conversation commands")
@MainActor
struct ConversationCommandTests {
    @Test("idle send starts work, clears the persisted draft and defaults to Steer")
    func idleSend() async throws {
        let service = try await makePairedFixture()
        let model = await makeStartedConversation(service: service)
        model.draft = "ship the release"
        model.draftChanged()
        await pump(150)

        await model.send()

        #expect(model.lastSendStatus == .accepted)
        #expect(model.messages.contains { $0.role == .user && $0.plainText == "ship the release" })
        #expect(model.draft.isEmpty)
        #expect(model.isRunning)
        #expect(model.delivery == .steer)
        let persisted = try await service.loadDraft(hostId: "studio-mac", sessionId: "s-001")
        #expect(persisted == nil)
    }

    @Test("running send uses the selected Steer and Queue modes")
    func runningModes() async throws {
        let service = try await makePairedFixture()
        let model = await makeStartedConversation(service: service)
        model.draft = "first"
        await model.send()
        #expect(model.delivery == .steer)

        model.draft = "second"
        model.selectDelivery(.steer)
        await model.send()
        #expect(model.lastSendStatus == .accepted)
        #expect(model.delivery == .steer)

        model.draft = "third"
        model.selectDelivery(.queue)
        await model.send()
        #expect(model.lastSendStatus == .accepted)
    }

    @Test("an unsupported mode keeps the draft and reports a localized error")
    func unsupportedModeKeepsDraft() async throws {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in SessionCapabilities.idle }
        let model = await makeStartedConversation(service: service)
        model.draft = "keep me"
        model.delivery = .steer

        await model.send()

        #expect(service.sendCalls.isEmpty)
        #expect(model.draft == "keep me")
        #expect(model.sendErrorKey == .conversationDeliveryUnsupported)
    }

    @Test("a send error keeps the draft available")
    func sendFailureKeepsDraft() async throws {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, _ in throw SessionServiceError(.timeout) }
        let model = await makeStartedConversation(service: service)
        model.draft = "retry me"

        await model.send()

        #expect(model.draft == "retry me")
        #expect(model.sendErrorKey == .errorTimeout)
        #expect(model.lastSendStatus == nil)
    }

    @Test("an uncertain send keeps the draft until the original request settles")
    func uncertainSendReconciliation() async throws {
        let service = StubSessionService()
        let receipt = CommandReceipt(commandId: "cmd-u1", requestId: "req-u1", status: .uncertain, delivery: .send)
        service.sendHandler = { _, _, _, _ in receipt }
        let model = await makeStartedConversation(service: service)
        model.draft = "uncertain text"

        await model.send()
        #expect(model.lastSendStatus == .uncertain)
        #expect(model.inFlightSend?.commandId == "cmd-u1")
        #expect(model.draft == "uncertain text")

        var settled = receipt
        settled.status = .confirmed
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 50,
                event: .commandStatus(settled)
            )
        )
        #expect(model.lastSendStatus == .confirmed)
        #expect(model.inFlightSend == nil)
        #expect(model.draft.isEmpty)

        let messageCount = model.messages.count
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 51,
                event: .commandStatus(settled)
            )
        )
        #expect(model.messages.count == messageCount)
        #expect(model.lastSendStatus == .confirmed)
    }

    @Test("stop stays pending until the Host settles it")
    func stopSettlement() async throws {
        let service = StubSessionService()
        let stopReceipt = CommandReceipt(commandId: "cmd-s1", requestId: "req-s1", status: .uncertain, delivery: nil)
        service.capabilitiesHandler = { _, _ in .running }
        service.stopHandler = { _, _ in stopReceipt }
        let model = await makeStartedConversation(service: service)

        await model.stopRun()
        #expect(model.lastStopStatus == .uncertain)
        #expect(model.pendingStop?.commandId == "cmd-s1")
        #expect(!model.stopSettled)
        #expect(model.stopStatusKey == .conversationStopPending)

        var settled = stopReceipt
        settled.status = .confirmed
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 60,
                event: .commandStatus(settled)
            )
        )
        #expect(model.stopSettled)
        #expect(model.lastStopStatus == .confirmed)
        #expect(!model.isRunning)
        #expect(model.stopStatusKey == .conversationStopConfirmed)
    }

    @Test("an unknown command settlement is ignored")
    func unknownSettlementIgnored() async throws {
        let service = StubSessionService()
        service.capabilitiesHandler = { _, _ in .running }
        let model = await makeStartedConversation(service: service)

        let unknown = CommandReceipt(commandId: "cmd-other", requestId: "req-other", status: .confirmed, delivery: nil)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 70,
                event: .commandStatus(unknown)
            )
        )
        #expect(model.lastSendStatus == nil)
        #expect(model.lastStopStatus == nil)
        #expect(model.isRunning)
    }

    @Test("a settled attempt ends the running state and clears the preview")
    func attemptEnded() async throws {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 80,
                event: .preview(attemptId: "attempt-1", text: "streaming", replace: true, incomplete: true)
            )
        )
        #expect(model.livePreview == "streaming")
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 81,
                event: .attemptEnded(attemptId: "attempt-1", outcome: .completed)
            )
        )
        #expect(model.livePreview == nil)
        #expect(!model.isRunning)
    }
}
