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
    }

    private struct Session {
        var generation: UInt64 = 0
        var text: String = ""
        var persisted: String?
        var revision: UInt64 = 0
        var inFlight: Task<Void, Error>?
    }

    private let service: any SessionService
    private var sessions: [Key: Session] = [:]

    init(service: any SessionService) {
        self.service = service
    }

    func claim(hostId: String, sessionId: String) async throws -> Claim {
        let key = Key(hostId: hostId, sessionId: sessionId)
        if let old = sessions[key] {
            try? await old.inFlight?.value
        }
        var session = sessions[key] ?? Session()
        session.generation &+= 1
        session.revision &+= 1
        session.text = ""
        session.persisted = nil
        session.inFlight = nil
        sessions[key] = session

        let stored = try await service.loadDraft(hostId: hostId, sessionId: sessionId)
        guard var current = sessions[key], current.generation == session.generation else {
            return Claim(lease: Lease(key: key, generation: session.generation), text: stored ?? "")
        }
        current.text = stored ?? ""
        current.persisted = stored
        sessions[key] = current
        return Claim(
            lease: Lease(key: key, generation: current.generation),
            text: current.text
        )
    }

    func update(_ lease: Lease, text: String) {
        guard var session = sessions[lease.key],
              session.generation == lease.generation else { return }
        session.text = text
        session.revision &+= 1
        sessions[lease.key] = session
    }

    func setAndFlush(_ lease: Lease, text: String) async throws {
        guard var session = sessions[lease.key],
              session.generation == lease.generation else { return }
        session.text = text
        session.revision &+= 1
        sessions[lease.key] = session
        try await flush(lease)
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

    /// Clears only when this lease still owns the draft and the text is still
    /// exactly the submitted text. A newer edit or a newer conversation wins.
    func clearIfCurrent(_ lease: Lease, matching text: String) async throws {
        guard var session = sessions[lease.key],
              session.generation == lease.generation else { return }
        sessions[lease.key] = session
        if let inFlight = session.inFlight {
            try? await inFlight.value
        }
        guard var current = sessions[lease.key],
              current.generation == lease.generation,
              current.text == text else { return }
        let task = Task { [service] in
            try await service.clearDraft(hostId: lease.key.hostId, sessionId: lease.key.sessionId)
        }
        current.inFlight = task
        sessions[lease.key] = current
        do {
            try await task.value
        } catch {
            guard var failed = sessions[lease.key],
                  failed.generation == lease.generation else { throw error }
            failed.inFlight = nil
            sessions[lease.key] = failed
            throw error
        }
        guard var cleared = sessions[lease.key],
              cleared.generation == lease.generation else { return }
        cleared.inFlight = nil
        cleared.persisted = nil
        cleared.text = ""
        sessions[lease.key] = cleared
    }

    /// Final save for a closing model. If a replacement has claimed the same
    /// session, the generation check makes this a no-op.
    func release(_ lease: Lease, text: String) async throws {
        guard var session = sessions[lease.key],
              session.generation == lease.generation else { return }
        session.text = text
        session.revision &+= 1
        sessions[lease.key] = session
        try await flush(lease)
    }

    private func persist(_ lease: Lease, text: String, revision: UInt64) async throws {
        guard var session = sessions[lease.key],
              session.generation == lease.generation,
              session.revision == revision,
              session.persisted != text else { return }
        let task = Task { [service] in
            try await service.saveDraft(
                hostId: lease.key.hostId,
                sessionId: lease.key.sessionId,
                text: text
            )
        }
        session.inFlight = task
        sessions[lease.key] = session
        do {
            try await task.value
        } catch {
            guard var failed = sessions[lease.key],
                  failed.generation == lease.generation else { throw error }
            failed.inFlight = nil
            sessions[lease.key] = failed
            throw error
        }
        guard var current = sessions[lease.key],
              current.generation == lease.generation else { return }
        current.inFlight = nil
        current.persisted = text
        sessions[lease.key] = current
    }
}
