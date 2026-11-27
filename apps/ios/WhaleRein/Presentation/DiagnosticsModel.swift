// Diagnostics presentation: one immutable redacted capture from Core, a
// summary that only groups facts from those bytes, an always-available full
// report rendered from the same bytes, and one explicit Share tap that stages a
// protected copy and opens the system picker once.
//
// Local directory names and OS families are decoration resolved separately from
// the capture. They never enter the document, the shared file, logs or metrics.

import Foundation
import Observation

struct DiagnosticsShareFile: Identifiable, Hashable {
    let url: URL

    /// The file URL is the identity so a redraw cannot restart the sheet.
    var id: URL { url }
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

    /// The exact immutable capture. Every summary row parses `capture.text`,
    /// the full report renders it, and share writes `capture.bytes`; nothing
    /// else is serialized.
    private(set) var capture: DiagnosticsCapture?
    private(set) var summary = DiagnosticsSummary.empty
    /// Local-only decoration keyed by the capture's anonymous label.
    private(set) var localHosts: [String: DiagnosticsLocalHost] = [:]

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
        switch state.share {
        case .finished(true): return .diagnosticsExported
        case .finished(false): return .diagnosticsCancelled
        default: return nil
        }
    }

    var shareFile: DiagnosticsShareFile? {
        state.share.fileURL.map(DiagnosticsShareFile.init(url:))
    }

    var shareCount: Int { state.shareCount }

    /// Always the captured document, verbatim.
    var fullReport: String { capture?.text ?? "" }

    var canShare: Bool {
        capture != nil && state.isLoaded && state.share.allowsNewShare
    }

    private let service: any SessionService
    private let fileManager: FileManager
    /// App-owned staging directory for the protected temporary copy.
    private let stagingDirectory: URL

    private static let fileNamePrefix = "WhaleRein-diagnostics-"

    init(
        service: any SessionService,
        stagingDirectory: URL? = nil,
        fileManager: FileManager = .default
    ) {
        self.service = service
        self.fileManager = fileManager
        self.stagingDirectory = stagingDirectory
            ?? fileManager.temporaryDirectory
                .appendingPathComponent("WhaleRein/diagnostics", isDirectory: true)
    }

    func localHost(for label: String) -> DiagnosticsLocalHost? { localHosts[label] }

    // MARK: - Capture

    /// Initial load. An existing capture is kept until the user refreshes, so a
    /// reappearing sheet does not silently replace the inspected document.
    func load() async {
        guard loadState == .idle else { return }
        await captureNow(refresh: false)
    }

    /// Explicit refresh: always produces a new capture. The previous document
    /// and any prepared share file are discarded first.
    func refresh() async {
        guard loadState != .loading else { return }
        await captureNow(refresh: true)
    }

    private func captureNow(refresh: Bool) async {
        discardPreparedFile()
        removeStagedFiles()
        state = DiagnosticsReduce.reduce(state, .loadStarted)
        let generation = state.captureGeneration
        capture = nil
        summary = .empty
        localHosts = [:]
        do {
            let fresh = try await service.diagnosticsCapture(refresh: refresh)
            guard generation == state.captureGeneration else { return }
            let parsed = DiagnosticsSummaryParser.parse(fresh.text)
            capture = fresh
            summary = parsed
            // The report is inspectable immediately; local decoration resolves
            // separately and never delays or alters the captured bytes.
            state = DiagnosticsReduce.reduce(state, .loadSucceeded)
            let decoration = await Self.uniqueLocalHosts(
                await service.diagnosticsLocalHosts(for: fresh),
                for: parsed.hosts.map(\.label)
            )
            guard generation == state.captureGeneration else { return }
            localHosts = decoration
        } catch {
            guard generation == state.captureGeneration else { return }
            let code = (error as? SessionServiceError)?.code ?? .internalError
            capture = nil
            summary = .empty
            localHosts = [:]
            state = DiagnosticsReduce.reduce(state, .loadFailed(code))
        }
    }

    // MARK: - Local decoration

    static func uniqueLocalHosts(
        _ candidates: [DiagnosticsLocalHost],
        for labels: [String]
    ) -> [String: DiagnosticsLocalHost] {
        let known = Set(labels)
        var counts: [String: Int] = [:]
        for candidate in candidates {
            counts[candidate.label, default: 0] += 1
        }
        var result: [String: DiagnosticsLocalHost] = [:]
        for candidate in candidates {
            let name = candidate.displayName.trimmingCharacters(in: .whitespacesAndNewlines)
            guard known.contains(candidate.label),
                  counts[candidate.label] == 1,
                  !name.isEmpty else {
                continue
            }
            let os = candidate.osFamily?.trimmingCharacters(in: .whitespacesAndNewlines)
            result[candidate.label] = DiagnosticsLocalHost(
                label: candidate.label,
                displayName: name,
                osFamily: (os?.isEmpty ?? true) ? nil : os
            )
        }
        return result
    }

    // MARK: - Share

    /// One explicit Share tap. Stages a unique protected copy of the inspected
    /// bytes and presents the system sheet once. A write failure opens nothing.
    func share() async {
        guard let capture, canShare else { return }
        state = DiagnosticsReduce.reduce(state, .shareStarted)
        do {
            let url = try stage(bytes: capture.bytes)
            state = DiagnosticsReduce.reduce(state, .shareStaged(url))
        } catch {
            state = DiagnosticsReduce.reduce(state, .shareFailed(.internalError))
        }
    }

    /// The system sheet finished (or was cancelled). Cleanup is idempotent.
    func shareCompleted(completed: Bool) {
        discardPreparedFile()
        state = DiagnosticsReduce.reduce(state, .shareFinished(completed: completed))
    }

    /// The sheet was dismissed without a completion callback.
    func dismissShare() {
        discardPreparedFile()
        state = DiagnosticsReduce.reduce(state, .shareDismissed)
    }

    private func stage(bytes: Data) throws -> URL {
        try prepareStagingDirectory()
        removeStagedFiles()
        let url = stagingDirectory.appendingPathComponent(
            Self.fileNamePrefix + UUID().uuidString + ".json"
        )
        do {
            try bytes.write(
                to: url,
                options: [.atomic, .completeFileProtection]
            )
            try? fileManager.setAttributes([.posixPermissions: 0o600], ofItemAtPath: url.path)
            return url
        } catch {
            try? fileManager.removeItem(at: url)
            throw error
        }
    }

    private func prepareStagingDirectory() throws {
        try fileManager.createDirectory(
            at: stagingDirectory,
            withIntermediateDirectories: true,
            attributes: [.posixPermissions: 0o700]
        )
        try? fileManager.setAttributes(
            [.posixPermissions: 0o700],
            ofItemAtPath: stagingDirectory.path
        )
    }

    private func discardPreparedFile() {
        guard let url = state.share.fileURL else { return }
        try? fileManager.removeItem(at: url)
    }

    /// Removes staged copies left by an earlier capture or launch. Only files
    /// this model created are touched.
    private func removeStagedFiles() {
        let contents = (try? fileManager.contentsOfDirectory(
            at: stagingDirectory,
            includingPropertiesForKeys: nil
        )) ?? []
        for url in contents where url.lastPathComponent.hasPrefix(Self.fileNamePrefix) {
            try? fileManager.removeItem(at: url)
        }
    }
}