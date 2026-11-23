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

    @Test("cancellation stops a running transfer and reports its own outcome")
    func cancellation() async {
        let service = GatedAttachmentService()
        service.beginId = "download-1"
        let resultGate = SuspendedResponse<AttachmentTransferSnapshot>()
        service.resultGate = resultGate
        let model = AttachmentTransferModel(service: service)
        let downloadTask = Task {
            await model.download(
                hostId: "studio-mac",
                sessionId: "s-001",
                attachment: attachment,
                dataDirectory: makeTempDirectory()
            )
        }
        await resultGate.waitUntilStarted()

        model.cancel(attachment.attachmentRef)
        resultGate.resume(with: snapshot(.cancelled, received: 10, error: .cancelled))
        await downloadTask.value
        await pump(30)

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

/// Start/result gates keep the model's cancellation window observable without
/// timing sleeps. Cancellation records the native ids the model released.
@MainActor
final class GatedAttachmentService: AttachmentTransferService {
    let beginGate = SuspendedResponse<String>()
    var resultGate: SuspendedResponse<AttachmentTransferSnapshot>?
    var beginId: String?
    private(set) var requests: [AttachmentDownloadRequest] = []
    private(set) var cancelled: [String] = []
    private(set) var takeCalls = 0

    func beginAttachmentDownload(_ request: AttachmentDownloadRequest) async throws -> String {
        requests.append(request)
        if let beginId { return beginId }
        return try await beginGate.wait()
    }

    func attachmentDownloadProgress(_ downloadId: String) async -> AttachmentDownloadProgress? {
        nil
    }

    func takeAttachmentResult(
        _ downloadId: String,
        waitMs: UInt64
    ) async throws -> AttachmentTransferSnapshot {
        takeCalls += 1
        if let resultGate {
            return try await resultGate.wait()
        }
        return AttachmentTransferSnapshot(
            downloadId: downloadId,
            status: .running,
            progress: AttachmentDownloadProgress(received: 0, total: 100),
            download: nil,
            errorCode: nil
        )
    }

    func cancelAttachmentDownload(_ downloadId: String) async {
        cancelled.append(downloadId)
    }
}

/// Writes the staged bytes at the Core boundary and enforces the destination's
/// conflict rule. This is what makes same-named files across instances
/// meaningful instead of a canned success.
@MainActor
final class StagingAttachmentService: AttachmentTransferService {
    var content: Data
    let fileName: String
    private var nextId = 0
    private var files: [String: URL] = [:]
    private(set) var requests: [AttachmentDownloadRequest] = []
    private(set) var cancelled: [String] = []

    init(content: String, fileName: String = "report.txt") {
        self.content = Data(content.utf8)
        self.fileName = fileName
    }

    func beginAttachmentDownload(_ request: AttachmentDownloadRequest) async throws -> String {
        requests.append(request)
        let target = request.destination.appendingPathComponent(request.fileName ?? fileName)
        let fileManager = FileManager.default
        if fileManager.fileExists(atPath: target.path) && !request.overwrite {
            throw SessionServiceError(.conflict)
        }
        try fileManager.createDirectory(at: request.destination, withIntermediateDirectories: true)
        try content.write(to: target, options: .atomic)
        nextId += 1
        let id = "staging-\(nextId)"
        files[id] = target
        return id
    }

    func attachmentDownloadProgress(_ downloadId: String) async -> AttachmentDownloadProgress? {
        nil
    }

    func takeAttachmentResult(
        _ downloadId: String,
        waitMs: UInt64
    ) async throws -> AttachmentTransferSnapshot {
        let target = files[downloadId]!
        return AttachmentTransferSnapshot(
            downloadId: downloadId,
            status: .succeeded,
            progress: AttachmentDownloadProgress(received: Int64(content.count), total: Int64(content.count)),
            download: AttachmentDownload(
                url: target,
                name: fileName,
                bytes: Int64(content.count),
                sha256: String(repeating: "b", count: 64),
                mediaType: "text/plain",
                verified: true
            ),
            errorCode: nil
        )
    }

    func cancelAttachmentDownload(_ downloadId: String) async {
        cancelled.append(downloadId)
    }
}

private let lateSnapshot = snapshot(
    .succeeded,
    received: 100,
    download: verifiedDownload()
)

@Suite("Attachment cancellation and instance staging")
@MainActor
struct AttachmentCancellationTests {
    @Test("cancel before the start result cancels the returned native transfer")
    func cancelBeforeStartResult() async {
        let service = GatedAttachmentService()
        let model = AttachmentTransferModel(service: service)
        let task = Task {
            await model.download(
                hostId: "studio-mac",
                sessionId: "s-001",
                attachment: attachment,
                dataDirectory: makeTempDirectory()
            )
        }
        await service.beginGate.waitUntilStarted()
        #expect(model.state(for: attachment.attachmentRef).isDownloading)

        model.cancel(attachment.attachmentRef)
        #expect(model.state(for: attachment.attachmentRef).phase == .cancelled)

        service.beginGate.resume(with: "download-late")
        await task.value

        #expect(service.cancelled == ["download-late"])
        #expect(service.takeCalls == 0, "A cancelled start must not begin polling")
        #expect(!model.state(for: attachment.attachmentRef).isVerified)
        #expect(model.state(for: attachment.attachmentRef).phase == .cancelled)
    }

    @Test("leaving the conversation cancels a start with no id and fences its result")
    func cancelAllBeforeStartResult() async {
        let service = GatedAttachmentService()
        let model = AttachmentTransferModel(service: service)
        let task = Task {
            await model.download(
                hostId: "studio-mac",
                sessionId: "s-001",
                attachment: attachment,
                dataDirectory: makeTempDirectory()
            )
        }
        await service.beginGate.waitUntilStarted()

        model.cancelAll()
        service.beginGate.resume(with: "download-late")
        await task.value

        #expect(service.cancelled == ["download-late"])
        #expect(service.takeCalls == 0)
        #expect(!model.state(for: attachment.attachmentRef).isVerified)
    }

    @Test("a late completion from the replaced conversation cannot publish")
    func lateCompletionIsFenced() async {
        let service = GatedAttachmentService()
        service.beginId = "download-1"
        let resultGate = SuspendedResponse<AttachmentTransferSnapshot>()
        service.resultGate = resultGate
        let model = AttachmentTransferModel(service: service)
        let task = Task {
            await model.download(
                hostId: "studio-mac",
                sessionId: "s-001",
                attachment: attachment,
                dataDirectory: makeTempDirectory()
            )
        }
        await resultGate.waitUntilStarted()

        model.cancelAll()
        resultGate.resume(with: lateSnapshot)
        await task.value
        await pump(60)

        #expect(service.takeCalls > 0)
        #expect(service.cancelled == ["download-1"])
        #expect(!model.state(for: attachment.attachmentRef).isVerified)
        #expect(model.state(for: attachment.attachmentRef).download == nil)
    }

    @Test("equal Host and session ids on two instances keep separate staged bytes")
    func perInstanceStaging() async throws {
        let dataA = makeTempDirectory()
        let dataB = makeTempDirectory()
        let serviceA = StagingAttachmentService(content: "alpha")
        let serviceB = StagingAttachmentService(content: "beta")
        let modelA = AttachmentTransferModel(service: serviceA)
        let modelB = AttachmentTransferModel(service: serviceB)
        modelA.dataDirectory = dataA
        modelB.dataDirectory = dataB

        await modelA.download(hostId: "studio-mac", sessionId: "s-001", attachment: attachment)
        await modelA.waitForTerminal(attachment.attachmentRef)
        await modelB.download(hostId: "studio-mac", sessionId: "s-001", attachment: attachment)
        await modelB.waitForTerminal(attachment.attachmentRef)

        let fileA = try #require(modelA.state(for: attachment.attachmentRef).download?.url)
        let fileB = try #require(modelB.state(for: attachment.attachmentRef).download?.url)
        #expect(modelA.state(for: attachment.attachmentRef).isVerified)
        #expect(modelB.state(for: attachment.attachmentRef).isVerified)
        #expect(fileA.deletingLastPathComponent() != fileB.deletingLastPathComponent())
        #expect(try String(contentsOf: fileA, encoding: .utf8) == "alpha")
        #expect(try String(contentsOf: fileB, encoding: .utf8) == "beta")

        let expectedA = try AppStorageLocations.attachmentStagingDirectory(
            in: dataA,
            hostId: "studio-mac",
            sessionId: "s-001"
        )
        let expectedB = try AppStorageLocations.attachmentStagingDirectory(
            in: dataB,
            hostId: "studio-mac",
            sessionId: "s-001"
        )
        #expect(serviceA.requests.first?.destination == expectedA)
        #expect(serviceB.requests.first?.destination == expectedB)
    }

    @Test("the same instance reports conflict, then overwrites only when asked")
    func sameInstanceConflictAndOverwrite() async throws {
        let service = StagingAttachmentService(content: "one")
        let model = AttachmentTransferModel(service: service)
        model.dataDirectory = makeTempDirectory()

        await model.download(hostId: "studio-mac", sessionId: "s-001", attachment: attachment)
        await model.waitForTerminal(attachment.attachmentRef)
        let file = try #require(model.state(for: attachment.attachmentRef).download?.url)
        #expect(try String(contentsOf: file, encoding: .utf8) == "one")

        service.content = Data("two".utf8)
        await model.download(hostId: "studio-mac", sessionId: "s-001", attachment: attachment)
        await model.waitForTerminal(attachment.attachmentRef)
        #expect(model.state(for: attachment.attachmentRef).phase == .failed)
        #expect(model.state(for: attachment.attachmentRef).errorKey == .conversationAttachmentConflict)
        #expect(try String(contentsOf: file, encoding: .utf8) == "one")

        await model.download(
            hostId: "studio-mac",
            sessionId: "s-001",
            attachment: attachment,
            overwrite: true
        )
        await model.waitForTerminal(attachment.attachmentRef)
        #expect(model.state(for: attachment.attachmentRef).isVerified)
        #expect(try String(contentsOf: file, encoding: .utf8) == "two")
        #expect(service.requests.last?.overwrite == true)
    }
}
