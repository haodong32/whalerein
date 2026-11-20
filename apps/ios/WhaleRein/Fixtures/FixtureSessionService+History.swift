// Fixture conformance for committed-history paging, exact reads and attachment
// transfers. Fixtures are explicitly synthetic: they exercise the same
// presentation seams as Core without claiming Staging or Host behavior.

import Foundation

extension FixtureSessionService: ConversationHistoryService {
    func historyPage(
        hostId: String,
        sessionId: String,
        source: HistorySource,
        continuation: String?,
        maxMessages: Int,
        maxBytes: Int
    ) async throws -> ConversationHistoryPage {
        try requirePaired()
        guard FixtureCatalog.sessions(hostId: hostId).contains(where: { $0.sessionId == sessionId }) else {
            throw SessionServiceError(.unavailable)
        }
        let all = messages(for: hostId, sessionId: sessionId)
        let pageSize = max(1, min(maxMessages, 50))
        let end: Int
        if let continuation {
            guard let parsed = Self.parseContinuation(continuation) else {
                throw SessionServiceError(.staleHistory)
            }
            end = min(parsed, all.count)
        } else {
            end = all.count
        }
        var start = max(0, end - pageSize)
        // Respect the byte budget the same way Core does: drop older messages
        // until the page fits, never splitting a message.
        while start < end {
            let bytes = all[start..<end].reduce(0) { total, message in
                total + message.plainText.utf8.count + 64
            }
            if bytes <= maxBytes { break }
            start += 1
        }
        let page = Array(all[start..<end])
        let complete = start == 0
        return ConversationHistoryPage(
            source: source,
            projection: fixtureProjection,
            complete: complete,
            messages: page,
            activities: page.flatMap { message in
                message.blocks.compactMap(\.toolActivity)
            },
            continuation: complete ? nil : "fixture:\(start)"
        )
    }

    func historyMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        try requirePaired()
        guard FixtureCatalog.hosts.contains(where: { $0.hostId == reference.hostId }) else {
            throw SessionServiceError(.unavailable)
        }
        guard FixtureCatalog.sessions(hostId: reference.hostId)
            .contains(where: { $0.sessionId == reference.sessionId }) else {
            throw SessionServiceError(.security)
        }
        guard reference.projectionId == fixtureProjection else {
            throw SessionServiceError(.staleHistory)
        }
        let all = messages(for: reference.hostId, sessionId: reference.sessionId)
        guard let message = all.first(where: { $0.messageId == reference.messageId }) else {
            throw SessionServiceError(.unavailable)
        }
        return message
    }

    private static func parseContinuation(_ value: String) -> Int? {
        guard value.hasPrefix("fixture:"),
              let index = Int(value.dropFirst("fixture:".count)),
              index >= 0 else {
            return nil
        }
        return index
    }
}

extension FixtureSessionService: AttachmentTransferService {
    func beginAttachmentDownload(_ request: AttachmentDownloadRequest) async throws -> String {
        try requirePaired()
        return fixtureAttachments.begin(request)
    }

    func attachmentDownloadProgress(
        _ downloadId: String
    ) async -> AttachmentDownloadProgress? {
        fixtureAttachments.progress(downloadId)
    }

    func takeAttachmentResult(
        _ downloadId: String,
        waitMs: UInt64
    ) async throws -> AttachmentTransferSnapshot {
        guard let snapshot = await fixtureAttachments.take(downloadId, waitMs: waitMs) else {
            throw SessionServiceError(.unavailable)
        }
        return snapshot
    }

    func cancelAttachmentDownload(_ downloadId: String) async {
        fixtureAttachments.cancel(downloadId)
    }
}
