// Verified attachment transfer boundary. Core performs the checked download
// into an app-owned staging directory; the app only observes progress, requests
// cancellation and exports a verified file through native iOS controls.

import Foundation

struct AttachmentDownloadRequest: Sendable {
    let hostId: String
    let sessionId: String
    let attachmentRef: String
    /// App-owned staging directory that owns the final verified file.
    let destination: URL
    let fileName: String?
    let overwrite: Bool
}

struct AttachmentDownloadProgress: Sendable, Equatable {
    let received: Int64
    let total: Int64

    var fraction: Double {
        guard total > 0 else { return 0 }
        return min(1, max(0, Double(received) / Double(total)))
    }
}

struct AttachmentDownload: Sendable, Hashable {
    let url: URL
    let name: String
    let bytes: Int64
    let sha256: String
    let mediaType: String
    let verified: Bool
}

enum AttachmentTransferStatus: String, Sendable {
    case running
    case succeeded
    case failed
    case cancelled
}

struct AttachmentTransferSnapshot: Sendable {
    let downloadId: String
    let status: AttachmentTransferStatus
    let progress: AttachmentDownloadProgress
    let download: AttachmentDownload?
    let errorCode: AppErrorCode?
}

@MainActor
protocol AttachmentTransferService: AnyObject {
    /// Starts a background verified download and returns its identifier.
    func beginAttachmentDownload(_ request: AttachmentDownloadRequest) async throws -> String
    func attachmentDownloadProgress(_ downloadId: String) async -> AttachmentDownloadProgress?
    /// Waits up to `waitMs` for a terminal state. A timeout reports `running`
    /// so the caller can keep rendering progress and poll again.
    func takeAttachmentResult(
        _ downloadId: String,
        waitMs: UInt64
    ) async throws -> AttachmentTransferSnapshot
    func cancelAttachmentDownload(_ downloadId: String) async
}
