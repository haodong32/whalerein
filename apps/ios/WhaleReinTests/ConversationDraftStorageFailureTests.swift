// Draft storage failures. Production swallows no longer hide a failed read,
// scheduled save, explicit save or close flush: the conversation still opens,
// typed text stays available in memory, the watch still starts, and the user
// can tell whether the draft is likely to survive a restart.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
private final class FailingDraftService: StubSessionService {
    var failSave = false
    var failLoad = false
    var failClear = false
    private(set) var saveAttempts = 0
    private(set) var loadAttempts = 0
    private(set) var clearAttempts = 0

    override func saveDraft(hostId: String, sessionId: String, text: String) async throws {
        saveAttempts += 1
        if failSave { throw SessionServiceError(.draftUnavailable) }
        try await super.saveDraft(hostId: hostId, sessionId: sessionId, text: text)
    }

    override func loadDraft(hostId: String, sessionId: String) async throws -> String? {
        loadAttempts += 1
        if failLoad { throw SessionServiceError(.draftUnavailable) }
        return try await super.loadDraft(hostId: hostId, sessionId: sessionId)
    }

    override func clearDraft(hostId: String, sessionId: String) async throws {
        clearAttempts += 1
        if failClear { throw SessionServiceError(.draftUnavailable) }
        try await super.clearDraft(hostId: hostId, sessionId: sessionId)
    }
}

@Suite("Draft storage failure")
@MainActor
struct ConversationDraftStorageFailureTests {
    @Test("a draft read failure still opens the conversation and starts the watch")
    func readFailureDoesNotBlockStartup() async {
        let service = FailingDraftService()
        service.failLoad = true

        let model = await makeStartedConversation(service: service)

        #expect(model.loadState != .failed(.draftUnavailable))
        #expect(model.errorKey == nil)
        #expect(service.startWatchCalls.count == 1)
        #expect(service.activeStreamCount == 1)
        #expect(model.draftLoadErrorKey == .errorDraftUnavailable)
        #expect(model.draftStorageWarningKey == .conversationDraftStorageWarning)
        #expect(model.draft.isEmpty)
    }

    @Test("a read failure during a watch restart still restarts the watch")
    func readFailureDoesNotBlockRestart() async {
        let service = FailingDraftService()
        let model = await makeStartedConversation(service: service)
        model.draft = "typed before restart"
        service.failLoad = true

        await model.restartWatch()

        #expect(service.startWatchCalls.count == 2)
        #expect(model.draftLoadErrorKey == .errorDraftUnavailable)
        #expect(model.draftStorageWarningKey == .conversationDraftStorageWarning)
        // The in-memory text survives because the read failure is not treated
        // as authoritative empty content.
        #expect(model.draft == "typed before restart")
    }

    @Test("an explicit save failure keeps the text and reports restart risk")
    func explicitSaveFailureIsObservable() async {
        let service = FailingDraftService()
        service.failSave = true
        let model = await makeStartedConversation(service: service)
        model.draft = "typed text"

        await model.persistDraftNow()

        #expect(model.draft == "typed text")
        #expect(model.draftPersistenceErrorKey == .errorDraftUnavailable)
        #expect(model.draftMayNotSurviveRestart)
        #expect(model.draftStorageWarningKey == .conversationDraftStorageWarning)
        #expect(service.saveAttempts == 1)

        // A later successful write clears the warning and really persists.
        service.failSave = false
        await model.persistDraftNow()
        #expect(model.draftPersistenceErrorKey == nil)
        #expect(!model.draftMayNotSurviveRestart)
        #expect(model.draftStorageWarningKey == nil)
        let persisted = try? await service.loadDraft(hostId: "studio-mac", sessionId: "s-001")
        #expect(persisted == "typed text")
    }

    @Test("a scheduled save failure is observable after the debounce")
    func scheduledSaveFailureIsObservable() async {
        let service = FailingDraftService()
        service.failSave = true
        let model = await makeStartedConversation(service: service)
        model.draft = "debounced text"

        model.draftChanged()
        await waitUntil { model.draftPersistenceErrorKey != nil }

        #expect(model.draftPersistenceErrorKey == .errorDraftUnavailable)
        #expect(model.draft == "debounced text")
        #expect(service.saveAttempts == 1)
    }

    @Test("a failed close flush is observable and the text stays in memory")
    func closeFlushFailureIsObservable() async {
        let service = FailingDraftService()
        service.failSave = true
        let model = await makeStartedConversation(service: service)
        model.draft = "last edit"

        model.close()
        await waitUntil { model.closeDraftFlushDidComplete }

        #expect(model.closeDraftFlushDidComplete)
        #expect(model.closeDraftFlushErrorKey == .errorDraftUnavailable)
        #expect(model.draftMayNotSurviveRestart)
        #expect(model.draftStorageWarningKey == .conversationDraftStorageWarning)
        #expect(model.draft == "last edit")
    }

    @Test("a successful close flush reports that the draft survived")
    func closeFlushSuccessIsObservable() async {
        let service = FailingDraftService()
        let model = await makeStartedConversation(service: service)
        model.draft = "saved at close"

        model.close()
        await waitUntil { model.closeDraftFlushDidComplete }

        #expect(model.closeDraftFlushDidComplete)
        #expect(model.closeDraftFlushErrorKey == nil)
        #expect(!model.draftMayNotSurviveRestart)
        #expect(model.draftStorageWarningKey == nil)
        let persisted = try? await service.loadDraft(hostId: "studio-mac", sessionId: "s-001")
        #expect(persisted == "saved at close")
    }
}
