// Presentation state for verified attachment transfers. Core owns the checked
// download; this model only starts it, renders bounded progress, requests
// cancellation and records the distinct native-export outcome.

import Foundation
import Observation

@MainActor
@Observable
final class AttachmentTransferModel {
    enum Phase: Equatable, Sendable {
        case idle
        case downloading
        case verified
        case failed
        case cancelled
    }

    enum ExportOutcome: Equatable, Sendable {
        case exported
        case cancelled
        case failed
    }

    struct State: Equatable, Sendable {
        var phase: Phase = .idle
        var progress: AttachmentDownloadProgress?
        var download: AttachmentDownload?
        var errorKey: L10nKey?
        var exportOutcome: ExportOutcome?
        var downloadId: String?

        var isDownloading: Bool { phase == .downloading }
        var isVerified: Bool { phase == .verified && download?.verified == true }
    }

    private(set) var states: [String: State] = [:]

    private let service: (any AttachmentTransferService)?
    private var pollTasks: [String: Task<Void, Never>] = [:]

    init(service: (any AttachmentTransferService)?) {
        self.service = service
    }

    func state(for attachmentRef: String) -> State {
        states[attachmentRef] ?? State()
    }

    /// Starts a download into the app-owned session staging directory. A
    /// repeated call while running is ignored; an explicit retry after a
    /// failure starts a fresh Core transfer.
    func download(
        hostId: String,
        sessionId: String,
        attachment: AttachmentBlock,
        dataDirectory: URL? = nil,
        overwrite: Bool = false
    ) async {
        guard attachment.available else {
            states[attachment.attachmentRef] = State(
                phase: .failed,
                errorKey: .conversationAttachmentUnavailable
            )
            return
        }
        guard let service else {
            states[attachment.attachmentRef] = State(
                phase: .failed,
                errorKey: .errorUnsupportedCapability
            )
            return
        }
        if state(for: attachment.attachmentRef).isDownloading { return }
        let destination: URL
        do {
            destination = try AppStorageLocations.attachmentStagingDirectory(
                in: dataDirectory ?? AppStorageLocations.applicationSupportDirectory(),
                hostId: hostId,
                sessionId: sessionId
            )
        } catch {
            states[attachment.attachmentRef] = State(
                phase: .failed,
                errorKey: .conversationAttachmentFailed
            )
            return
        }
        var state = State(phase: .downloading)
        state.progress = AttachmentDownloadProgress(received: 0, total: attachment.size)
        state.errorKey = nil
        state.download = nil
        state.exportOutcome = nil
        states[attachment.attachmentRef] = state

        do {
            let id = try await service.beginAttachmentDownload(
                AttachmentDownloadRequest(
                    hostId: hostId,
                    sessionId: sessionId,
                    attachmentRef: attachment.attachmentRef,
                    destination: destination,
                    fileName: nil,
                    overwrite: overwrite
                )
            )
            var started = self.state(for: attachment.attachmentRef)
            started.downloadId = id
            states[attachment.attachmentRef] = started
            startPolling(attachmentRef: attachment.attachmentRef, downloadId: id, service: service)
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            states[attachment.attachmentRef] = State(
                phase: .failed,
                errorKey: Self.downloadErrorKey(for: code)
            )
        }
    }

    func cancel(_ attachmentRef: String) {
        guard let id = state(for: attachmentRef).downloadId, let service else { return }
        Task { await service.cancelAttachmentDownload(id) }
    }

    /// Cancels every in-flight transfer when the conversation is replaced.
    func cancelAll() {
        guard let service else { return }
        for (attachmentRef, state) in states where state.isDownloading {
            if let id = state.downloadId {
                Task { await service.cancelAttachmentDownload(id) }
            }
            pollTasks[attachmentRef]?.cancel()
        }
    }

    /// Records the result of the native Files exporter separately from the
    /// verified-download outcome.
    func recordExport(_ attachmentRef: String, result: Result<URL, Error>) {
        var state = state(for: attachmentRef)
        guard state.isVerified else { return }
        switch result {
        case .success:
            state.exportOutcome = .exported
        case .failure(let error):
            state.exportOutcome = Self.isCancellation(error) ? .cancelled : .failed
        }
        states[attachmentRef] = state
    }

    /// Test/UI hand-off: waits until a transfer leaves the running phase.
    func waitForTerminal(_ attachmentRef: String, timeout: Duration = .seconds(10)) async {
        let deadline = ContinuousClock.now + timeout
        while state(for: attachmentRef).isDownloading, ContinuousClock.now < deadline {
            try? await Task.sleep(for: .milliseconds(20))
        }
    }

    private func startPolling(
        attachmentRef: String,
        downloadId: String,
        service: any AttachmentTransferService
    ) {
        pollTasks[attachmentRef]?.cancel()
        pollTasks[attachmentRef] = Task { [weak self] in
            while !Task.isCancelled {
                do {
                    let snapshot = try await service.takeAttachmentResult(downloadId, waitMs: 250)
                    guard let self else { return }
                    self.apply(snapshot, for: attachmentRef)
                    if snapshot.status != .running { return }
                } catch {
                    guard let self else { return }
                    let code = (error as? SessionServiceError)?.code ?? .internalError
                    var state = self.state(for: attachmentRef)
                    state.phase = .failed
                    state.errorKey = Self.downloadErrorKey(for: code)
                    self.states[attachmentRef] = state
                            return
                }
            }
        }
    }

    private func apply(_ snapshot: AttachmentTransferSnapshot, for attachmentRef: String) {
        var state = state(for: attachmentRef)
        state.progress = snapshot.progress
        switch snapshot.status {
        case .running:
            state.phase = .downloading
        case .succeeded:
            state.phase = .verified
            state.download = snapshot.download
            state.errorKey = nil
        case .cancelled:
            state.phase = .cancelled
            state.errorKey = .conversationAttachmentCancelled
        case .failed:
            state.phase = .failed
            state.errorKey = Self.downloadErrorKey(for: snapshot.errorCode)
        }
        states[attachmentRef] = state
    }

    private static func downloadErrorKey(for code: AppErrorCode?) -> L10nKey {
        switch code {
        case .security:
            return .conversationAttachmentIntegrity
        case .conflict:
            return .conversationAttachmentConflict
        case .cancelled:
            return .conversationAttachmentCancelled
        case .unavailable, .hostOffline, .timeout, .notPaired, .revoked:
            return .conversationAttachmentUnavailable
        default:
            return .conversationAttachmentFailed
        }
    }

    private static func isCancellation(_ error: Error) -> Bool {
        if let sessionError = error as? SessionServiceError {
            return sessionError.code == .cancelled
        }
        let nsError = error as NSError
        return nsError.domain == NSCocoaErrorDomain
            && nsError.code == NSUserCancelledError
    }
}
