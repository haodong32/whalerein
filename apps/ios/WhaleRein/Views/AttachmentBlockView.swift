// Verified attachment row. Core downloads into app-owned staging and verifies
// the bytes; only then can the reader share or export the file. Download and
// native-destination outcomes are shown separately.

import SwiftUI
import UniformTypeIdentifiers

struct AttachmentBlockView: View {
    @Bindable var model: ConversationModel
    let attachment: AttachmentBlock
    let hostId: String
    let sessionId: String

    @State private var exportDocument: VerifiedAttachmentDocument?
    @State private var isExporting = false

    private var state: AttachmentTransferModel.State {
        model.attachments.state(for: attachment.attachmentRef)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 8) {
                Image(systemName: "paperclip")
                    .foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: attachment.name.isEmpty
                        ? L10n.conversationAttachment.string
                        : attachment.name)
                        .font(.subheadline.weight(.semibold))
                    if attachment.size > 0 {
                        Text(verbatim: ByteCountFormatter.string(
                            fromByteCount: attachment.size,
                            countStyle: .file
                        ))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 8)
                statusBadge
            }

            if !attachment.available {
                Text(L10n.conversationAttachmentUnavailable.key)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            } else if state.isDownloading {
                if let progress = state.progress {
                    ProgressView(value: progress.fraction)
                        .accessibilityIdentifier("attachment.progress.\(attachment.attachmentRef)")
                        .accessibilityValue(Text("\(Int(progress.fraction * 100))%"))
                } else {
                    ProgressView()
                }
                HStack {
                    Text(L10n.conversationAttachmentDownloading.key)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button(L10n.conversationAttachmentCancel.key) {
                        model.attachments.cancel(attachment.attachmentRef)
                    }
                    .font(.caption)
                    .accessibilityIdentifier("attachment.cancel.\(attachment.attachmentRef)")
                }
            } else if let download = state.download, state.isVerified {
                HStack(spacing: 12) {
                    ShareLink(item: download.url) {
                        Label(L10n.conversationAttachmentShare.key, systemImage: "square.and.arrow.up")
                            .font(.caption)
                    }
                    .accessibilityIdentifier("attachment.share.\(attachment.attachmentRef)")

                    Button {
                        exportDocument = VerifiedAttachmentDocument(sourceURL: download.url)
                        isExporting = true
                    } label: {
                        Label(L10n.conversationAttachmentExport.key, systemImage: "folder")
                            .font(.caption)
                    }
                    .accessibilityIdentifier("attachment.export.\(attachment.attachmentRef)")
                }
            } else {
                HStack {
                    Button(state.phase == .failed || state.phase == .cancelled
                        ? L10n.commonRetry.key
                        : L10n.conversationAttachmentDownload.key) {
                        Task {
                            await model.attachments.download(
                                hostId: hostId,
                                sessionId: sessionId,
                                attachment: attachment
                            )
                        }
                    }
                    .font(.caption)
                    .glassButtonStyle()
                    .controlSize(.small)
                    .accessibilityIdentifier("attachment.download.\(attachment.attachmentRef)")

                    if state.errorKey == .conversationAttachmentConflict {
                        Button(L10n.conversationAttachmentReplace.key) {
                            Task {
                                await model.attachments.download(
                                    hostId: hostId,
                                    sessionId: sessionId,
                                    attachment: attachment,
                                    overwrite: true
                                )
                            }
                        }
                        .font(.caption)
                        .accessibilityIdentifier("attachment.replace.\(attachment.attachmentRef)")
                    }
                }
            }

            if let errorKey = state.errorKey, state.phase != .downloading {
                Text(errorKey.key)
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("attachment.error.\(attachment.attachmentRef)")
            }
            if let outcome = state.exportOutcome {
                Text(Self.exportKey(for: outcome).key)
                    .font(.footnote)
                    .foregroundStyle(outcome == .exported ? .secondary : .red)
                    .accessibilityIdentifier("attachment.exportOutcome.\(attachment.attachmentRef)")
            }
        }
        .padding(12)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("attachment.\(attachment.attachmentRef)")
        .fileExporter(
            isPresented: $isExporting,
            document: exportDocument,
            contentType: Self.contentType(for: attachment.mediaType),
            defaultFilename: state.download?.name ?? attachment.name
        ) { result in
            isExporting = false
            exportDocument = nil
            model.attachments.recordExport(attachment.attachmentRef, result: result)
        }
    }

    @ViewBuilder
    private var statusBadge: some View {
        switch state.phase {
        case .verified:
            Label(L10n.conversationAttachmentVerified.key, systemImage: "checkmark.seal")
                .font(.caption2)
                .foregroundStyle(.green)
                .accessibilityIdentifier("attachment.verified.\(attachment.attachmentRef)")
        case .downloading:
            ProgressView()
                .controlSize(.small)
        default:
            EmptyView()
        }
    }

    static func exportKey(for outcome: AttachmentTransferModel.ExportOutcome) -> L10nKey {
        switch outcome {
        case .exported: return .conversationAttachmentExported
        case .cancelled: return .conversationAttachmentExportCancelled
        case .failed: return .conversationAttachmentExportFailed
        }
    }

    static func contentType(for mediaType: String) -> UTType {
        UTType(mimeType: mediaType) ?? .data
    }
}

/// FileDocument wrapper for a file Core already verified. Reading the source
/// never re-derives or re-verifies content; the exporter only copies it.
struct VerifiedAttachmentDocument: FileDocument {
    static var readableContentTypes: [UTType] { [.data] }

    let sourceURL: URL

    init(sourceURL: URL) {
        self.sourceURL = sourceURL
    }

    init(configuration: ReadConfiguration) throws {
        throw CocoaError(.fileReadUnsupportedScheme)
    }

    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper {
        try FileWrapper(url: sourceURL, options: .immediate)
    }
}
