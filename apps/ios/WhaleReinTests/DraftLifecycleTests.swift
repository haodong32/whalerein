// Draft lifecycle and Host/session isolation, including restart persistence for
// the file store and retention on failed or uncertain submission.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Draft lifecycle")
@MainActor
struct DraftLifecycleTests {
    @Test("in-memory drafts are isolated per Host and session")
    func isolation() throws {
        let store = InMemoryDraftStore()
        try store.save("alpha", hostId: "studio-mac", sessionId: "s-001")
        try store.save("beta", hostId: "studio-mac", sessionId: "s-002")
        try store.save("gamma", hostId: "travel-laptop", sessionId: "s-001")

        #expect(try store.load(hostId: "studio-mac", sessionId: "s-001") == "alpha")
        #expect(try store.load(hostId: "studio-mac", sessionId: "s-002") == "beta")
        #expect(try store.load(hostId: "travel-laptop", sessionId: "s-001") == "gamma")
    }

    @Test("clearing one draft leaves the others intact")
    func clearOne() throws {
        let store = InMemoryDraftStore()
        try store.save("alpha", hostId: "studio-mac", sessionId: "s-001")
        try store.save("beta", hostId: "studio-mac", sessionId: "s-002")
        try store.clear(hostId: "studio-mac", sessionId: "s-001")

        #expect(try store.load(hostId: "studio-mac", sessionId: "s-001") == nil)
        #expect(try store.load(hostId: "studio-mac", sessionId: "s-002") == "beta")
    }

    @Test("file drafts survive a new store instance and can be cleared")
    func filePersistence() throws {
        let directory = makeTempDirectory()
        let first = try FileDraftStore(dataDirectory: directory)
        try first.save("survive restart", hostId: "studio-mac", sessionId: "s-001")

        let second = try FileDraftStore(dataDirectory: directory)
        #expect(try second.load(hostId: "studio-mac", sessionId: "s-001") == "survive restart")
        #expect(try second.load(hostId: "travel-laptop", sessionId: "s-101") == nil)

        try second.clear(hostId: "studio-mac", sessionId: "s-001")
        #expect(try second.load(hostId: "studio-mac", sessionId: "s-001") == nil)
    }

    @Test("an empty draft removes the stored value")
    func emptyRemoves() throws {
        let store = InMemoryDraftStore()
        try store.save("alpha", hostId: "studio-mac", sessionId: "s-001")
        try store.save("", hostId: "studio-mac", sessionId: "s-001")
        #expect(try store.load(hostId: "studio-mac", sessionId: "s-001") == nil)
    }

    @Test("the conversation loads an existing draft when it starts")
    func loadsExistingDraft() async throws {
        let dataDirectory = makeTempDirectory()
        let drafts = try FileDraftStore(dataDirectory: dataDirectory)
        try drafts.save("existing draft", hostId: "studio-mac", sessionId: "s-001")
        let service = try await makePairedFixture(draftStore: drafts)
        let model = await makeStartedConversation(service: service)

        #expect(model.draft == "existing draft")
    }

    @Test("a successful send clears the persisted draft")
    func clearedOnSuccess() async throws {
        let store = InMemoryDraftStore()
        let service = try await makePairedFixture(draftStore: store)
        let model = await makeStartedConversation(service: service)
        model.draft = "send me"
        await model.send()

        #expect(model.draft.isEmpty)
        #expect(try store.load(hostId: "studio-mac", sessionId: "s-001") == nil)
    }

    @Test("a failed send keeps the draft")
    func retainedOnFailure() async {
        let service = StubSessionService()
        service.sendHandler = { _, _, _, _ in throw SessionServiceError(.timeout) }
        let model = await makeStartedConversation(service: service)
        model.draft = "keep me"
        await model.persistDraftNow()

        await model.send()

        #expect(model.draft == "keep me")
        let persisted = try? await service.loadDraft(hostId: "studio-mac", sessionId: "s-001")
        #expect(persisted == "keep me")
    }

    @Test("draft saving failure is reported without losing the text")
    func saveFailure() async {
        let service = StubSessionService()
        let model = await makeStartedConversation(service: service)
        model.draft = "typed text"
        await model.persistDraftNow()
        #expect(model.draft == "typed text")
    }
}
