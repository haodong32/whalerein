// Pure transitions for the attachment transfers of one conversation. The
// reducer never starts, polls or cancels a transfer; the model performs those
// side effects and reports their results as events.

import Foundation

enum AttachmentsEvent: Equatable, Sendable {
    case startRequested(attachmentRef: String, total: Int64)
    case downloadStarted(attachmentRef: String, downloadId: String)
    case progressUpdated(attachmentRef: String, progress: AttachmentDownloadProgress)
    case verificationStarted(attachmentRef: String)
    case completed(attachmentRef: String, download: AttachmentDownload)
    case failed(attachmentRef: String, code: AppErrorCode)
    case cancelled(attachmentRef: String)
    case exportRecorded(attachmentRef: String, outcome: AttachmentExportOutcome)
    case exportCleared(attachmentRef: String)
}

enum AttachmentsReduce {
    static func reduce(_ state: AttachmentsState, _ event: AttachmentsEvent) -> AttachmentsState {
        var state = state
        switch event {
        case .startRequested(let attachmentRef, let total):
            // A repeated start while running is ignored; an explicit retry
            // after a terminal phase starts from a clean transfer.
            guard !state.transfer(for: attachmentRef).isActive else { break }
            var transfer = AttachmentTransferState(phase: .preparing)
            transfer.progress = AttachmentDownloadProgress(received: 0, total: total)
            state.transfers[attachmentRef] = transfer

        case .downloadStarted(let attachmentRef, let downloadId):
            guard var transfer = state.transfers[attachmentRef],
                  transfer.phase == .preparing
            else { break }
            transfer.phase = .transferring
            transfer.downloadId = downloadId
            transfer.errorCode = nil
            state.transfers[attachmentRef] = transfer

        case .progressUpdated(let attachmentRef, let progress):
            guard var transfer = state.transfers[attachmentRef],
                  transfer.phase.isActive
            else { break }
            transfer.progress = progress
            state.transfers[attachmentRef] = transfer

        case .verificationStarted(let attachmentRef):
            guard var transfer = state.transfers[attachmentRef],
                  transfer.phase == .transferring
            else { break }
            transfer.phase = .verifying
            state.transfers[attachmentRef] = transfer

        case .completed(let attachmentRef, let download):
            guard var transfer = state.transfers[attachmentRef],
                  transfer.phase.isActive
            else { break }
            transfer.phase = .completed
            transfer.download = download
            transfer.errorCode = nil
            state.transfers[attachmentRef] = transfer

        case .failed(let attachmentRef, let code):
            guard var transfer = state.transfers[attachmentRef],
                  transfer.phase.isActive
            else { break }
            transfer.phase = .failed
            transfer.download = nil
            transfer.errorCode = code
            state.transfers[attachmentRef] = transfer

        case .cancelled(let attachmentRef):
            guard var transfer = state.transfers[attachmentRef],
                  transfer.phase.isActive
            else { break }
            transfer.phase = .cancelled
            transfer.download = nil
            transfer.errorCode = .cancelled
            state.transfers[attachmentRef] = transfer

        case .exportRecorded(let attachmentRef, let outcome):
            guard var transfer = state.transfers[attachmentRef],
                  transfer.isVerified
            else { break }
            transfer.exportOutcome = outcome
            state.transfers[attachmentRef] = transfer

        case .exportCleared(let attachmentRef):
            guard var transfer = state.transfers[attachmentRef] else { break }
            transfer.exportOutcome = nil
            state.transfers[attachmentRef] = transfer
        }
        return state
    }
}
