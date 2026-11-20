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

    private(set) var report: DiagnosticsReport?
    private(set) var previewText = ""
    private(set) var loadState: LoadState = .idle
    private(set) var errorKey: L10nKey?
    private(set) var statusKey: L10nKey?
    private(set) var shareFile: DiagnosticsShareFile?
    private(set) var exportCount = 0

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
        loadState = .loading
        errorKey = nil
        do {
            let loaded = try await service.diagnosticsPreview()
            report = loaded
            previewText = DiagnosticsRedactor.preview(report: loaded)
            loadState = .loaded
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            loadState = .failed(code)
            errorKey = L10nKey.errorKey(for: code)
        }
    }

    /// Call when the user cancels the confirmation. Nothing is exported.
    func cancelShare() {
        statusKey = .diagnosticsCancelled
        shareFile = nil
    }

    /// Exports the redacted file into app-owned staging and returns it for the
    /// system share sheet. Nothing is written before this confirmation.
    func confirmShare() async {
        guard let report else { return }
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
            exportCount += 1
            statusKey = .diagnosticsExported
            shareFile = DiagnosticsShareFile(url: url)
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            errorKey = L10nKey.errorKey(for: code)
        }
    }

    func dismissShare() {
        shareFile = nil
    }
}
