// Diagnostics presentation: redacted preview, per-Host HTTPS/MQTT stages, and a
// two-step share flow that writes the exported file only after confirmation.

import Foundation
import Observation

struct DiagnosticsShareFile: Identifiable, Hashable {
    let id = UUID()
    let url: URL
}

@MainActor
@Observable
final class DiagnosticsModel {
    enum LoadState: Equatable {
        case idle
        case loading
        case loaded
        case failed(AppErrorCode)
    }

    /// One state value for the screen; the legacy names below project from it.
    private(set) var state = DiagnosticsState()

    private(set) var report: DiagnosticsReport?
    private(set) var previewText = ""

    var loadState: LoadState {
        switch state.load {
        case .idle: .idle
        case .loading: .loading
        case .loaded: .loaded
        case .failed(let code): .failed(code)
        }
    }

    var errorKey: L10nKey? {
        (state.loadError ?? state.shareError).map { L10nKey.errorKey(for: $0) }
    }

    var statusKey: L10nKey? {
        if case .cancelled = state.share { return .diagnosticsCancelled }
        if case .ready = state.share { return .diagnosticsExported }
        return nil
    }

    var shareFile: DiagnosticsShareFile? {
        state.share.fileURL.map(DiagnosticsShareFile.init(url:))
    }

    var exportCount: Int { state.exportCount }

    private let service: any SessionService
    /// App-owned staging directory for the redacted report. A file is written
    /// only after the user confirms, and never into a shared temporary area.
    private let stagingDirectory: URL

    init(service: any SessionService, stagingDirectory: URL? = nil) {
        self.service = service
        self.stagingDirectory = stagingDirectory
            ?? FileManager.default
                .urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
                .appendingPathComponent("WhaleRein/diagnostics", isDirectory: true)
    }

    var stages: [DiagnosticStage] { report?.stages ?? [] }
    var lastFailureCode: String? { report?.lastFailureCode }
    var coreRevision: String { report?.coreVersion ?? service.coreRevision }
    var appVersion: String { report?.appVersion ?? service.appVersion }
    var redactionFields: [String] { report?.redactionFields ?? [] }

    func load() async {
        state = DiagnosticsReduce.reduce(state, .loadStarted)
        do {
            let loaded = try await service.diagnosticsPreview()
            report = loaded
            previewText = DiagnosticsRedactor.preview(report: loaded)
            state = DiagnosticsReduce.reduce(state, .loadSucceeded)
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            state = DiagnosticsReduce.reduce(state, .loadFailed(code))
        }
    }

    /// Call when the user cancels the confirmation. Nothing is exported.
    func cancelShare() {
        state = DiagnosticsReduce.reduce(state, .shareCancelled)
    }

    /// Exports the redacted file into app-owned staging and returns it for the
    /// system share sheet. Nothing is written before this confirmation.
    func confirmShare() async {
        guard let report else { return }
        state = DiagnosticsReduce.reduce(state, .shareStarted)
        let url = stagingDirectory.appendingPathComponent("WhaleRein-diagnostics.json")
        do {
            try FileManager.default.createDirectory(
                at: stagingDirectory,
                withIntermediateDirectories: true
            )
            _ = try await service.exportDiagnostics(to: url)
            // Re-write locally with the same redaction so the exported file is
            // guaranteed to match the preview even for a future live adapter.
            let text = DiagnosticsRedactor.exportText(report: report)
            try Data(text.utf8).write(to: url, options: [.atomic, .completeFileProtection])
            state = DiagnosticsReduce.reduce(state, .shareExported(url))
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            state = DiagnosticsReduce.reduce(state, .shareFailed(code))
        }
    }

    func dismissShare() {
        state = DiagnosticsReduce.reduce(state, .shareDismissed)
    }
}
