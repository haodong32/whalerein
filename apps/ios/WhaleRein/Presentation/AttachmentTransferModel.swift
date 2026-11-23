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

    /// App-owned root for this model's transfers. The shell points it at the
    /// active instance's data directory so equal Host/session IDs on different
    /// instances can never share staged bytes.
    var dataDirectory: URL = AppStorageLocations.applicationSupportDirectory()

    private struct Attempt: Equatable, Sendable {
        let id: UInt64
        let scope: UInt64
        let attachmentRef: String
    }

    private let service: (any AttachmentTransferService)?
    private var pollTasks: [String: Task<Void, Never>] = [:]
    /// One cancellable attempt per attachment. An attempt is removed as soon as
    /// cancel/close invalidates it, so a late start result cannot begin polling.
    private var attempts: [String: Attempt] = [:]
    private var attemptCounter: UInt64 = 0
    /// Bumped by `cancelAll` so every result from the replaced conversation is
    /// fenced, even when the same attachment ref reappears there.
    private var scopeGeneration: UInt64 = 0

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
                in: dataDirectory ?? self.dataDirectory,
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
        attemptCounter &+= 1
        let attempt = Attempt(
            id: attemptCounter,
            scope: scopeGeneration,
            attachmentRef: attachment.attachmentRef
        )
        attempts[attachment.attachmentRef] = attempt
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
            // Cancel or a conversation switch may have happened while the
            // native start was in flight. Release the returned transfer
            // instead of polling into the replaced conversation.
            guard isCurrent(attempt) else {
                await service.cancelAttachmentDownload(id)
                return
            }
            var started = self.state(for: attachment.attachmentRef)
            started.downloadId = id
            states[attachment.attachmentRef] = started
            startPolling(attempt: attempt, downloadId: id, service: service)
        } catch {
            guard isCurrent(attempt) else { return }
            let code = (error as? SessionServiceError)?.code ?? .internalError
            states[attachment.attachmentRef] = State(
                phase: .failed,
                errorKey: Self.downloadErrorKey(for: code)
            )
        }
    }

    /// Cancels one transfer, including a start that has not returned its
    /// native id yet. The returned id is cancelled by `download` once it
    /// arrives; no polling or completion can publish after this call.
    func cancel(_ attachmentRef: String) {
        guard state(for: attachmentRef).isDownloading else { return }
        invalidate(attachmentRef)
        var state = state(for: attachmentRef)
        if let id = state.downloadId {
            Task { [service] in
                await service?.cancelAttachmentDownload(id)
            }
        }
        state.phase = .cancelled
        state.downloadId = nil
        state.errorKey = .conversationAttachmentCancelled
        states[attachmentRef] = state
    }

    /// Cancels every in-flight transfer when the conversation is replaced.
    /// The scope fence drops any late progress or completion from the old
    /// conversation, even when the replacement uses the same attachment refs.
    func cancelAll() {
        scopeGeneration &+= 1
        let inFlight = attempts
        attempts.removeAll()
        for (attachmentRef, _) in inFlight {
            pollTasks[attachmentRef]?.cancel()
            pollTasks.removeValue(forKey: attachmentRef)
        }
        let downloading = states.filter { $0.value.isDownloading }
        var ids: [String] = []
        for (attachmentRef, state) in downloading {
            if let id = state.downloadId {
                ids.append(id)
            }
            var cancelled = state
            cancelled.phase = .cancelled
            cancelled.downloadId = nil
            cancelled.errorKey = .conversationAttachmentCancelled
            states[attachmentRef] = cancelled
        }
        guard let service, !ids.isEmpty else { return }
        for id in ids {
            Task { await service.cancelAttachmentDownload(id) }
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

    private func isCurrent(_ attempt: Attempt) -> Bool {
        attempt.scope == scopeGeneration
            && attempts[attempt.attachmentRef]?.id == attempt.id
    }

    private func invalidate(_ attachmentRef: String) {
        attempts.removeValue(forKey: attachmentRef)
        pollTasks.removeValue(forKey: attachmentRef)?.cancel()
    }

    private func startPolling(
        attempt: Attempt,
        downloadId: String,
        service: any AttachmentTransferService
    ) {
        pollTasks[attempt.attachmentRef]?.cancel()
        pollTasks[attempt.attachmentRef] = Task { [weak self] in
            while !Task.isCancelled {
                do {
                    let snapshot = try await service.takeAttachmentResult(downloadId, waitMs: 250)
                    guard let self, self.isCurrent(attempt) else { return }
                    self.apply(snapshot, for: attempt.attachmentRef)
                    if snapshot.status != .running { return }
                } catch {
                    guard let self, self.isCurrent(attempt) else { return }
                    let code = (error as? SessionServiceError)?.code ?? .internalError
                    var state = self.state(for: attempt.attachmentRef)
                    state.phase = .failed
                    state.errorKey = Self.downloadErrorKey(for: code)
                    self.states[attempt.attachmentRef] = state
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
