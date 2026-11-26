// One serialized draft owner per Host/session. The model owns the typing
// debounce; this actor owns write ordering, lease/generation fencing, flush and
// the conditional clear, so a replaced conversation can never overwrite the
// replacement's newer draft.

import Foundation

actor DraftService {
    struct Key: Hashable, Sendable {
        let hostId: String
        let sessionId: String
    }

    struct Lease: Sendable {
        let key: Key
        let generation: UInt64
    }

    struct Claim: Sendable {
        let lease: Lease
        let text: String
        let editID: UUID
    }

    struct Submission: Sendable {
        let key: Key
        let editID: UUID
    }

    typealias ClearObserver = @MainActor @Sendable (UUID, Bool) -> Void

    private struct Session {
        var generation: UInt64 = 0
        var text: String = ""
        var persisted: String?
        var revision: UInt64 = 0
        var editID = UUID()
        var isLoaded = false
        var onCleared: ClearObserver?
        var inFlight: Task<Void, Error>?
    }

    private let service: any SessionService
    private var sessions: [Key: Session] = [:]

    init(service: any SessionService) {
        self.service = service
    }

    func claim(
        hostId: String, sessionId: String, onCleared: ClearObserver? = nil
    ) async throws -> Claim {
        let key = Key(hostId: hostId, sessionId: sessionId)
        while let inFlight = sessions[key]?.inFlight {
            try? await inFlight.value
        }
        var session = sessions[key] ?? Session()
        session.generation &+= 1
        session.onCleared = onCleared
        sessions[key] = session
        let lease = Lease(key: key, generation: session.generation)
        if session.isLoaded {
            return Claim(lease: lease, text: session.text, editID: session.editID)
        }
        let stored = try await service.loadDraft(hostId: hostId, sessionId: sessionId)
        guard var current = sessions[key], current.generation == session.generation else {
            return Claim(lease: lease, text: stored ?? "", editID: session.editID)
        }
        current.text = stored ?? ""
        current.persisted = stored
        current.isLoaded = true
        sessions[key] = current
        return Claim(lease: lease, text: current.text, editID: current.editID)
    }

    func setAndFlush(_ lease: Lease, text: String, editID: UUID? = nil) async throws {
        guard stage(lease, text: text, editID: editID) != nil else { return }
        try await flush(lease)
    }

    /// Stage exactly the edit being sent before the command crosses the service
    /// boundary. This identity survives a new lease for the unchanged draft.
    func beginSubmission(_ lease: Lease, text: String, editID: UUID) -> Submission? {
        guard stage(lease, text: text, editID: editID) != nil else { return nil }
        return Submission(key: lease.key, editID: editID)
    }

    private func stage(_ lease: Lease, text: String, editID: UUID?) -> Session? {
        guard var session = sessions[lease.key],
              session.generation == lease.generation else { return nil }
        let nextEditID = editID ?? UUID()
        if session.text == text && session.editID == nextEditID { return session }
        session.text = text
        session.editID = nextEditID
        session.revision &+= 1
        sessions[lease.key] = session
        return session
    }

    func flush(_ lease: Lease) async throws {
        guard var session = sessions[lease.key],
              session.generation == lease.generation else { return }
        sessions[lease.key] = session

        while true {
            guard var current = sessions[lease.key],
                  current.generation == lease.generation else { return }
            if let inFlight = current.inFlight {
                try? await inFlight.value
                continue
            }
            if current.persisted == current.text { return }
            let text = current.text
            let revision = current.revision
            try await persist(lease, text: text, revision: revision)
            return
        }
    }

    /// Clears a draft only while this lease still owns the matching text.
    func clearIfCurrent(_ lease: Lease, matching text: String) async throws {
        try await clear(lease.key) { session in
            session.generation == lease.generation && session.text == text
        }
    }

    /// A receipt may outlive the composer that sent it. Clear the original
    /// edit in the shared session, including a new lease that restored it.
    func clearSubmitted(_ submission: Submission) async throws {
        try await clear(submission.key) { $0.editID == submission.editID }
    }

    private func clear(_ key: Key, if matches: (Session) -> Bool) async throws {
        while let inFlight = sessions[key]?.inFlight {
            try? await inFlight.value
        }
        guard var current = sessions[key], matches(current) else { return }
        let editID = current.editID
        let revision = current.revision
        let task = Task {
            do {
                try await service.clearDraft(hostId: key.hostId, sessionId: key.sessionId)
            } catch {
                if var failed = sessions[key] {
                    failed.inFlight = nil
                    sessions[key] = failed
                    await failed.onCleared?(editID, false)
                }
                throw error
            }
            guard var cleared = sessions[key] else { return }
            cleared.inFlight = nil
            cleared.persisted = nil
            // A new edit may have staged itself while storage was clearing.
            // Its flush must still write that edit after this erase.
            if cleared.revision == revision {
                cleared.text = ""
                cleared.editID = UUID()
            }
            sessions[key] = cleared
            await cleared.onCleared?(editID, true)
        }
        current.inFlight = task
        sessions[key] = current
        try await task.value
    }

    /// Final save for a closing model. A replaced lease cannot overwrite its
    /// successor, and reopening can recognize an unchanged original edit.
    func release(_ lease: Lease, text: String, editID: UUID? = nil) async throws {
        try await setAndFlush(lease, text: text, editID: editID)
    }

    private func persist(_ lease: Lease, text: String, revision: UInt64) async throws {
        guard var session = sessions[lease.key],
              session.generation == lease.generation,
              session.revision == revision,
              session.persisted != text else { return }
        let task = Task {
            do {
                try await service.saveDraft(
                    hostId: lease.key.hostId,
                    sessionId: lease.key.sessionId,
                    text: text
                )
            } catch {
                if var failed = sessions[lease.key], failed.generation == lease.generation {
                    failed.inFlight = nil
                    sessions[lease.key] = failed
                }
                throw error
            }
            guard var current = sessions[lease.key],
                  current.generation == lease.generation else { return }
            current.inFlight = nil
            current.persisted = text
            sessions[lease.key] = current
        }
        session.inFlight = task
        sessions[lease.key] = session
        try await task.value
    }
}
