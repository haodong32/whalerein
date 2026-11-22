// Live adapters for committed-history paging, exact reads and verified
// attachment transfers. All work runs through Core; no history or download
// semantics are reimplemented here.

import Foundation
import WhaleReinMobile

extension LiveSessionService: ConversationHistoryService {
    func historyPage(
        hostId: String,
        sessionId: String,
        source: HistorySource,
        continuation: String?,
        maxMessages: Int,
        maxBytes: Int
    ) async throws -> ConversationHistoryPage {
        let core = self.core
        let useCache = source == .cache
        let messages = max(1, min(maxMessages, 1_000))
        let bytes = max(1, min(maxBytes, 1_024 * 1_024))
        let view = try await call {
            try core.historyPage(
                hostId: hostId,
                sessionId: sessionId,
                cache: useCache,
                continuation: continuation,
                maxMessages: UInt32(messages),
                maxBytes: UInt32(bytes)
            )
        }
        return Self.map(historyPage: view)
    }

    func historyMessage(_ reference: MessageReference) async throws -> ConversationMessage {
        try await readMessage(reference)
    }
}

extension LiveSessionService: AttachmentTransferService {
    func beginAttachmentDownload(_ request: AttachmentDownloadRequest) async throws -> String {
        let core = self.core
        let binding = WhaleReinMobile.AttachmentDownloadRequestView(
            reference: WhaleReinMobile.AttachmentReferenceView(
                session: WhaleReinMobile.SessionKeyView(
                    hostId: request.hostId,
                    sessionId: request.sessionId
                ),
                attachmentRef: request.attachmentRef
            ),
            destination: request.destination.path,
            fileName: request.fileName,
            overwrite: request.overwrite
        )
        return try await call { try core.beginAttachmentDownload(request: binding) }
    }

    func attachmentDownloadProgress(
        _ downloadId: String
    ) async -> AttachmentDownloadProgress? {
        let core = self.core
        return try? await call {
            core.attachmentDownloadProgress(downloadId: downloadId)
                .map(Self.map(attachmentProgress:))
        }
    }

    func takeAttachmentResult(
        _ downloadId: String,
        waitMs: UInt64
    ) async throws -> AttachmentTransferSnapshot {
        let core = self.core
        let view = try await call {
            try core.takeAttachmentResult(
                downloadId: downloadId,
                waitMs: min(waitMs, 30_000)
            )
        }
        return Self.map(attachmentTransfer: view)
    }

    func cancelAttachmentDownload(_ downloadId: String) async {
        let core = self.core
        _ = try? await call { core.cancelAttachmentDownload(downloadId: downloadId) }
    }
}

extension LiveSessionService {
    static func map(historyPage view: WhaleReinMobile.HistoryPageView) -> ConversationHistoryPage {
        ConversationHistoryPage(
            source: view.source.lowercased() == "cache" ? .cache : .online,
            projection: view.projection,
            complete: view.complete,
            messages: view.messages.map(map(message:)),
            activities: view.activities.map(map(toolActivity:)),
            continuation: view.continuation
        )
    }

    static func map(toolActivity view: WhaleReinMobile.ToolActivityView) -> ToolActivity {
        ToolActivity(
            callId: view.callId,
            name: view.name,
            summary: view.summary,
            state: ToolState(rawValue: view.state) ?? .unknown,
            stages: view.stages,
            progress: view.progress,
            failureCode: view.failureCode,
            result: view.result.map(map(toolResultPart:)),
            callMissing: view.callMissing
        )
    }

    static func map(toolResultPart view: WhaleReinMobile.ToolResultPartView) -> ToolResultPart {
        switch view {
        case .text(let text):
            return .text(text)
        case .attachment(let mediaType, let name, let size, let available, let attachmentRef):
            return .attachment(
                AttachmentBlock(
                    mediaType: mediaType,
                    name: name,
                    size: Int64(size),
                    available: available,
                    attachmentRef: attachmentRef
                )
            )
        }
    }

    static func map(attachment view: WhaleReinMobile.AttachmentView) -> AttachmentBlock {
        AttachmentBlock(
            mediaType: view.mediaType,
            name: view.name,
            size: Int64(view.size),
            available: view.available,
            attachmentRef: view.attachmentRef
        )
    }

    static func map(
        attachmentProgress view: WhaleReinMobile.AttachmentProgressView
    ) -> AttachmentDownloadProgress {
        AttachmentDownloadProgress(
            received: Int64(view.received),
            total: Int64(view.total)
        )
    }

    static func map(
        attachmentTransfer view: WhaleReinMobile.AttachmentTransferView
    ) -> AttachmentTransferSnapshot {
        AttachmentTransferSnapshot(
            downloadId: view.downloadId,
            status: AttachmentTransferStatus(rawValue: view.state) ?? .failed,
            progress: map(attachmentProgress: view.progress),
            download: view.download.map { download in
                AttachmentDownload(
                    url: URL(fileURLWithPath: download.path),
                    name: download.name,
                    bytes: Int64(download.bytes),
                    sha256: download.sha256,
                    mediaType: download.mediaType,
                    verified: download.verified
                )
            },
            errorCode: view.errorCode.map(map(code:))
        )
    }
}
