// Business state for the attachment transfers of one conversation.
//
// Each attachment occurrence owns one `AttachmentTransferState`, keyed by the
// opaque `attachmentRef` from the committed block. A transfer therefore has a
// single phase, progress value, error code and export outcome; the supporting
// value types live in `AttachmentsValues.swift`.

import Foundation

struct AttachmentsState: Equatable, Sendable {
    var transfers: [String: AttachmentTransferState] = [:]

    /// The transfer for an occurrence, or an idle value when it never started.
    func transfer(for attachmentRef: String) -> AttachmentTransferState {
        transfers[attachmentRef] ?? AttachmentTransferState()
    }

    /// Occurrences with a transfer that is still running, in stable order.
    var activeAttachmentRefs: [String] {
        transfers
            .filter { $0.value.isActive }
            .keys
            .sorted()
    }
}
