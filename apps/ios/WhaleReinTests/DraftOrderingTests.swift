// The draft coordinator owns ordering and ownership. These tests exercise the
// actor directly with a stub SessionService rather than duplicating its debounce
// and lease logic in every conversation model.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
private final class GatedDraftService: StubSessionService {
    let saveGate = SuspendedResponse<Void>()
    private(set) var storedDraft: String?
    private(set) var order: [String] = []
    private var gatedOnce = false

    override func saveDraft(hostId: String, sessionId: String, text: String) async throws {
        order.append("save-start")
        if !gatedOnce {
            gatedOnce = true
            try await saveGate.wait()
        }
        order.append("save-finish")
        storedDraft = text
    }

    override func loadDraft(hostId: String, sessionId: String) async throws -> String? {
        storedDraft
    }

    override func clearDraft(hostId: String, sessionId: String) async throws {
        order.append("clear")
        storedDraft = nil
    }
}

@MainActor
private func loadDraft(_ service: StubSessionService) async -> String? {
    try? await service.loadDraft(hostId: "studio-mac", sessionId: "s-001")
}

@Suite("Draft coordinator")
@MainActor
struct DraftOrderingTests {
    @Test("a claim loads the persisted draft and a flush writes the latest text")
    func claimPersistAndFlush() async throws {
        let service = StubSessionService()
        try await service.saveDraft(hostId: "studio-mac", sessionId: "s-001", text: "existing")
        let drafts = DraftService(service: service)

        let claim = try await drafts.claim(hostId: "studio-mac", sessionId: "s-001")
        #expect(claim.text == "existing")

        try await drafts.setAndFlush(claim.lease, text: "new text")
        #expect(await loadDraft(service) == "new text")
    }

    @Test("flush saves the latest edit")
    func flushSavesLatestEdit() async throws {
        let service = StubSessionService()
        let drafts = DraftService(service: service)
        let claim = try await drafts.claim(hostId: "studio-mac", sessionId: "s-001")

        try await drafts.setAndFlush(claim.lease, text: "first")
        try await drafts.setAndFlush(claim.lease, text: "latest")
        #expect(await loadDraft(service) == "latest")
    }

    @Test("a conditional clear never clears a newer draft")
    func conditionalClearKeepsNewerText() async throws {
        let service = StubSessionService()
        let drafts = DraftService(service: service)
        let claim = try await drafts.claim(hostId: "studio-mac", sessionId: "s-001")

        try await drafts.setAndFlush(claim.lease, text: "newer")
        try await drafts.clearIfCurrent(claim.lease, matching: "submitted")
        #expect(await loadDraft(service) == "newer")
    }

    @Test("a replacement lease fences the old model's release")
    func replacementFencesOldRelease() async throws {
        let service = StubSessionService()
        let drafts = DraftService(service: service)
        let first = try await drafts.claim(hostId: "studio-mac", sessionId: "s-001")
        try await drafts.setAndFlush(first.lease, text: "first draft")

        let second = try await drafts.claim(hostId: "studio-mac", sessionId: "s-001")
        #expect(second.text == "first draft")

        try? await drafts.release(first.lease, text: "stale overwrite")
        #expect(await loadDraft(service) == "first draft")

        try await drafts.setAndFlush(second.lease, text: "second draft")
        #expect(await loadDraft(service) == "second draft")
    }

    @Test("a clear waits for an in-flight save before removing the text")
    func clearWaitsForInFlightSave() async {
        let service = GatedDraftService()
        let drafts = DraftService(service: service)
        let claim = try? await drafts.claim(hostId: "studio-mac", sessionId: "s-001")
        guard let claim else { return }

        let save = Task { try? await drafts.setAndFlush(claim.lease, text: "submitted") }
        await service.saveGate.waitUntilStarted()
        let clear = Task { try? await drafts.clearIfCurrent(claim.lease, matching: "submitted") }
        await pump(20)
        #expect(service.order == ["save-start"])

        service.saveGate.resume(with: ())
        await save.value
        await clear.value
        #expect(service.order == ["save-start", "save-finish", "clear"])
        #expect(service.storedDraft == nil)
    }
}
