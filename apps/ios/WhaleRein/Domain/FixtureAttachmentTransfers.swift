// Deterministic attachment transfer fixture. It writes synthetic verified
// bytes into the app-owned staging directory, reports bounded progress and
// supports cooperative cancellation so Simulator tests exercise the same
// presentation states as the live adapter.

import Foundation

@MainActor
final class FixtureAttachmentTransfers {
    private struct Transfer {
        var request: AttachmentDownloadRequest
        var progress: AttachmentDownloadProgress
        var status: AttachmentTransferStatus
        var download: AttachmentDownload?
        var errorCode: AppErrorCode?
        var cancelRequested: Bool
        var task: Task<Void, Never>?
    }

    private var transfers: [String: Transfer] = [:]
    private var counter = 0

    func begin(_ request: AttachmentDownloadRequest) -> String {
        counter += 1
        let id = String(format: "fixture-attachment-%08x", counter)
        let total = Int64(FixtureCatalog.attachmentBytes(for: request.attachmentRef).count)
        var transfer = Transfer(
            request: request,
            progress: AttachmentDownloadProgress(received: 0, total: total),
            status: .running,
            download: nil,
            errorCode: nil,
            cancelRequested: false
        )
        transfer.task = Task { [weak self] in
            guard let self else { return }
            let bytes = FixtureCatalog.attachmentBytes(for: request.attachmentRef)
            let step = max(1, bytes.count / 4)
            var offset = 0
            while offset < bytes.count {
                try? await Task.sleep(for: .milliseconds(80))
                guard !Task.isCancelled else { return }
                if self.transfers[id]?.cancelRequested == true {
                    self.transfers[id]?.status = .cancelled
                    self.transfers[id]?.errorCode = .cancelled
                    self.cleanup(id)
                    return
                }
                offset = min(bytes.count, offset + step)
                self.transfers[id]?.progress = AttachmentDownloadProgress(
                    received: Int64(offset),
                    total: total
                )
            }
            guard !Task.isCancelled else { return }
            if self.transfers[id]?.cancelRequested == true {
                self.transfers[id]?.status = .cancelled
                self.transfers[id]?.errorCode = .cancelled
                self.cleanup(id)
                return
            }
            do {
                try FileManager.default.createDirectory(
                    at: request.destination,
                    withIntermediateDirectories: true
                )
                let name = request.fileName ?? FixtureCatalog.attachmentName(for: request.attachmentRef)
                let url = request.destination.appendingPathComponent(name)
                if FileManager.default.fileExists(atPath: url.path), !request.overwrite {
                    self.transfers[id]?.status = .failed
                    self.transfers[id]?.errorCode = .conflict
                    return
                }
                try bytes.write(to: url, options: .atomic)
                self.transfers[id]?.status = .succeeded
                self.transfers[id]?.download = AttachmentDownload(
                    url: url,
                    name: name,
                    bytes: Int64(bytes.count),
                    sha256: FixtureCatalog.attachmentDigest(for: request.attachmentRef),
                    mediaType: "application/octet-stream",
                    verified: true
                )
                self.transfers[id]?.progress = AttachmentDownloadProgress(
                    received: Int64(bytes.count),
                    total: total
                )
            } catch {
                self.transfers[id]?.status = .failed
                self.transfers[id]?.errorCode = .internalError
            }
        }
        transfers[id] = transfer
        return id
    }

    func progress(_ id: String) -> AttachmentDownloadProgress? {
        transfers[id]?.progress
    }

    func take(_ id: String, waitMs: UInt64) async -> AttachmentTransferSnapshot? {
        let deadline = ContinuousClock.now + .milliseconds(Int64(waitMs))
        while ContinuousClock.now < deadline {
            guard let transfer = transfers[id] else { return nil }
            if transfer.status != .running {
                return snapshot(id)
            }
            try? await Task.sleep(for: .milliseconds(20))
        }
        guard let transfer = transfers[id] else { return nil }
        return snapshot(id)
    }

    func cancel(_ id: String) {
        transfers[id]?.cancelRequested = true
    }

    private func snapshot(_ id: String) -> AttachmentTransferSnapshot? {
        guard let transfer = transfers[id] else { return nil }
        return AttachmentTransferSnapshot(
            downloadId: id,
            status: transfer.status,
            progress: transfer.progress,
            download: transfer.download,
            errorCode: transfer.errorCode
        )
    }

    private func cleanup(_ id: String) {
        guard let request = transfers[id]?.request else { return }
        let name = request.fileName ?? FixtureCatalog.attachmentName(for: request.attachmentRef)
        let url = request.destination.appendingPathComponent(name)
        try? FileManager.default.removeItem(at: url)
    }
}
