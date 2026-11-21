// Attachment transfer presentation: bounded progress, cancellation, integrity
// and conflict mapping, verified export and distinct destination outcomes. No
// physical iPhone Files destination is required for these states.

import Foundation
import Testing
@testable import WhaleRein

@MainActor
final class ControlledAttachmentService: AttachmentTransferService {
    var beginError: SessionServiceError?
    var script: [AttachmentTransferSnapshot] = []
    private(set) var requests: [AttachmentDownloadRequest] = []
    private(set) var cancelled: [String] = []
    private(set) var takeCalls = 0

    func beginAttachmentDownload(_ request: AttachmentDownloadRequest) async throws -> String {
        if let beginError { throw beginError }
        requests.append(request)
        return "download-1"
    }

    func attachmentDownloadProgress(_ downloadId: String) async -> AttachmentDownloadProgress? {
        script.first?.progress
    }

    func takeAttachmentResult(
        _ downloadId: String,
        waitMs: UInt64
    ) async throws -> AttachmentTransferSnapshot {
        takeCalls += 1
        guard !script.isEmpty else {
            return AttachmentTransferSnapshot(
                downloadId: downloadId,
                status: .running,
                progress: AttachmentDownloadProgress(received: 0, total: 100),
                download: nil,
                errorCode: nil
            )
        }
        if script.count > 1 {
            return script.removeFirst()
        }
        return script[0]
    }

    func cancelAttachmentDownload(_ downloadId: String) async {
        cancelled.append(downloadId)
    }
}

private let attachment = AttachmentBlock(
    mediaType: "text/plain",
    name: "report.txt",
    size: 100,
    available: true,
    attachmentRef: "attachment-1"
)

private func snapshot(
    _ status: AttachmentTransferStatus,
    received: Int64 = 0,
    total: Int64 = 100,
    download: AttachmentDownload? = nil,
    error: AppErrorCode? = nil
) -> AttachmentTransferSnapshot {
    AttachmentTransferSnapshot(
        downloadId: "download-1",
        status: status,
        progress: AttachmentDownloadProgress(received: received, total: total),
        download: download,
        errorCode: error
    )
}

private func verifiedDownload() -> AttachmentDownload {
    AttachmentDownload(
        url: URL(fileURLWithPath: "/tmp/report.txt"),
        name: "report.txt",
        bytes: 100,
        sha256: String(repeating: "a", count: 64),
        mediaType: "text/plain",
        verified: true
    )
}

@Suite("Attachment transfers")
@MainActor
struct AttachmentTransferTests {
    @Test("download reports bounded progress and a verified file")
    func verifiedProgress() async {
        let service = ControlledAttachmentService()
        service.script = [
            snapshot(.running, received: 25),
            snapshot(.running, received: 75),
            snapshot(.succeeded, received: 100, download: verifiedDownload())
        ]
        let model = AttachmentTransferModel(service: service)
        await model.download(
            hostId: "studio-mac",
            sessionId: "s-001",
            attachment: attachment,
            dataDirectory: makeTempDirectory()
        )
        await model.waitForTerminal(attachment.attachmentRef)
        let state = model.state(for: attachment.attachmentRef)
        #expect(state.phase == .verified)
        #expect(state.isVerified)
        #expect(state.download?.verified == true)
        #expect(state.progress?.fraction == 1)
        #expect(service.requests.first?.destination.path.contains("downloads") == true)
        #expect(!service.requests.isEmpty)
    }

    @Test("cancellation stops the transfer and reports its own outcome")
    func cancellation() async {
        let service = ControlledAttachmentService()
        service.script = [
            snapshot(.running, received: 10),
            snapshot(.cancelled, received: 10, error: .cancelled)
        ]
        let model = AttachmentTransferModel(service: service)
        await model.download(
            hostId: "studio-mac",
            sessionId: "s-001",
            attachment: attachment,
            dataDirectory: makeTempDirectory()
        )
        await pump(40)
        model.cancel(attachment.attachmentRef)
        await model.waitForTerminal(attachment.attachmentRef)
        await pump(60)
        #expect(service.cancelled == ["download-1"])
        let state = model.state(for: attachment.attachmentRef)
        #expect(state.phase == .cancelled)
        #expect(state.errorKey == .conversationAttachmentCancelled)
    }

    @Test("integrity and conflict failures keep distinct messages")
    func failureMapping() async {
        let integrity = ControlledAttachmentService()
        integrity.script = [snapshot(.failed, error: .security)]
        let model = AttachmentTransferModel(service: integrity)
        await model.download(
            hostId: "studio-mac",
            sessionId: "s-001",
            attachment: attachment,
            dataDirectory: makeTempDirectory()
        )
        await model.waitForTerminal(attachment.attachmentRef)
        #expect(model.state(for: attachment.attachmentRef).errorKey == .conversationAttachmentIntegrity)

        let conflict = ControlledAttachmentService()
        conflict.script = [
            snapshot(.failed, error: .conflict),
            snapshot(.succeeded, received: 100, download: verifiedDownload())
        ]
        let replaced = AttachmentTransferModel(service: conflict)
        await replaced.download(
            hostId: "studio-mac",
            sessionId: "s-001",
            attachment: attachment,
            dataDirectory: makeTempDirectory()
        )
        await replaced.waitForTerminal(attachment.attachmentRef)
        #expect(replaced.state(for: attachment.attachmentRef).errorKey == .conversationAttachmentConflict)
        await replaced.download(
            hostId: "studio-mac",
            sessionId: "s-001",
            attachment: attachment,
            dataDirectory: makeTempDirectory(),
            overwrite: true
        )
        await replaced.waitForTerminal(attachment.attachmentRef)
        #expect(replaced.state(for: attachment.attachmentRef).isVerified)
        #expect(conflict.requests.last?.overwrite == true)
    }

    @Test("unavailable attachment never starts a transfer")
    func unavailable() async {
        let service = ControlledAttachmentService()
        let model = AttachmentTransferModel(service: service)
        await model.download(
            hostId: "studio-mac",
            sessionId: "s-001",
            attachment: AttachmentBlock(
                mediaType: "text/plain",
                name: "missing.txt",
                size: 0,
                available: false,
                attachmentRef: "attachment-missing"
            ),
            dataDirectory: makeTempDirectory()
        )
        #expect(service.requests.isEmpty)
        #expect(model.state(for: "attachment-missing").errorKey == .conversationAttachmentUnavailable)
    }

    @Test("export outcome is distinct from the verified download outcome")
    func exportOutcomes() async {
        let service = ControlledAttachmentService()
        service.script = [snapshot(.succeeded, received: 100, download: verifiedDownload())]
        let model = AttachmentTransferModel(service: service)
        await model.download(
            hostId: "studio-mac",
            sessionId: "s-001",
            attachment: attachment,
            dataDirectory: makeTempDirectory()
        )
        await model.waitForTerminal(attachment.attachmentRef)
        #expect(model.state(for: attachment.attachmentRef).isVerified)

        model.recordExport(attachment.attachmentRef, result: .success(URL(fileURLWithPath: "/tmp/x")))
        #expect(model.state(for: attachment.attachmentRef).exportOutcome == .exported)

        model.recordExport(
            attachment.attachmentRef,
            result: .failure(SessionServiceError(.cancelled))
        )
        #expect(model.state(for: attachment.attachmentRef).exportOutcome == .cancelled)
        #expect(model.state(for: attachment.attachmentRef).phase == .verified)

        model.recordExport(
            attachment.attachmentRef,
            result: .failure(SessionServiceError(.unavailable))
        )
        #expect(model.state(for: attachment.attachmentRef).exportOutcome == .failed)
    }

    @Test("reasoning preference persists in the App section across a restart")
    func reasoningPersists() throws {
        let dataDirectory = makeTempDirectory()
        let document = AppStorageLocations.appConfigurationURL(in: dataDirectory)
        let store = try AppConfigurationStore(documentPath: document.path)
        #expect(store.reasoningVisible == false)
        try store.setReasoningVisible(true)

        let reopened = try AppConfigurationStore(documentPath: document.path)
        #expect(reopened.reasoningVisible)
    }
}
