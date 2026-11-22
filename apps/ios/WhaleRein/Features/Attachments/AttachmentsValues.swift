// Supporting value types for the attachment transfer state.
//
// They live in their own file so the state file holds one primary type.

import Foundation

/// The lifecycle of one verified attachment download.
enum AttachmentTransferPhase: Equatable, Sendable {
    case idle
    /// Core is being asked to start the transfer.
    case preparing
    /// Bytes are moving into the app-owned staging directory.
    case transferring
    /// The downloaded file is being integrity-checked.
    case verifying
    /// The verified file is ready to export.
    case completed
    case failed
    case cancelled

    var isActive: Bool {
        switch self {
        case .preparing, .transferring, .verifying: true
        case .idle, .completed, .failed, .cancelled: false
        }
    }

    var isTerminal: Bool {
        switch self {
        case .completed, .failed, .cancelled: true
        case .idle, .preparing, .transferring, .verifying: false
        }
    }
}

/// The native Files exporter outcome, kept separate from the verified download
/// so a cancelled share sheet never makes the transfer itself look failed.
enum AttachmentExportOutcome: Equatable, Sendable {
    case exported
    case cancelled
    case failed
}

/// Everything known about one attachment occurrence's transfer. The download
/// identifier is present once Core accepted the request; the verified download
/// is present only after completion.
struct AttachmentTransferState: Equatable, Sendable {
    var phase: AttachmentTransferPhase = .idle
    var progress: AttachmentDownloadProgress?
    var download: AttachmentDownload?
    var errorCode: AppErrorCode?
    var exportOutcome: AttachmentExportOutcome?
    var downloadId: String?

    var isActive: Bool {
        phase.isActive
    }

    var isTransferring: Bool {
        phase == .transferring
    }

    var isVerified: Bool {
        phase == .completed && download?.verified == true
    }
}
