// Diagnostics: one immutable captured document drives the summary, the full
// report and the shared file; local directory decoration stays outside the
// bytes; one explicit Share tap stages one protected copy.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Diagnostics", .serialized)
@MainActor
struct DiagnosticsTests {
    // MARK: - Helpers

    private func document(
        capturedAt: Int64 = 1_800_000_000_000,
        hosts: [(label: String, state: String)] = [
            ("host-1", "connected"),
            ("host-2", "offline"),
        ],
        failure: String? = """
        {"code": "mqtt_subscription_failed", "events": [{"layer": "mqtt", "stage": "subscribe", "outcome": "failed", "code": "mqtt_subscription_failed"}]}
        """
    ) -> String {
        let hostRows = hosts
            .map { #"{"label": "\#($0.label)", "connection_state": "\#($0.state)"}"# }
            .joined(separator: ",")
        return """
        {
          "schema": "whalerein/v1",
          "core_version": "core-test-1",
          "protocol_version": 1,
          "captured_at_unix_ms": \(capturedAt),
          "report": {
            "schema": "whalerein/v1",
            "address": "https://studio-mac.access.whalerein.test",
            "commands": {"non_terminal": 1, "unknown": 0}
          },
          "hosts": [\(hostRows)],
          "last_failure": \(failure ?? "null"),
          "fields": ["schema", "core_version", "hosts.label"],
          "redactions": ["message content", "tool arguments"]
        }
        """
    }

    private func capture(
        _ text: String? = nil,
        redactionFields: [String] = ["message content", "tool arguments"]
    ) -> DiagnosticsCapture {
        let resolved = text ?? document()
        return DiagnosticsCapture(
            text: resolved,
            bytes: Data(resolved.utf8),
            localHostRefs: ["host-1": "studio-mac", "host-2": "travel-laptop"],
            redactionFields: redactionFields,
            appVersion: "0.1.0",
            coreVersion: "core-test-1"
        )
    }

    private func stagedFiles(in directory: URL) -> [URL] {
        let contents = (try? FileManager.default.contentsOfDirectory(
            at: directory,
            includingPropertiesForKeys: nil
        )) ?? []
        return contents.filter { $0.lastPathComponent.hasPrefix("WhaleRein-diagnostics-") }
    }

    // MARK: - Capture

    @Test("the full report and the shared file are the exact captured bytes")
    func summaryAndFileUseTheSameBytes() async throws {
        let service = StubSessionService()
        let expected = capture()
        service.diagnosticsHandler = { _ in expected }
        let staging = makeTempDirectory()
        let model = DiagnosticsModel(service: service, stagingDirectory: staging)

        await model.load()

        #expect(model.fullReport == expected.text)
        #expect(model.capture == expected)
        #expect(model.summary.hosts.map(\.label) == ["host-1", "host-2"])
        #expect(model.summary.hosts.first?.connectionState == "connected")
        #expect(model.summary.coreVersion == "core-test-1")
        #expect(model.summary.protocolVersion == 1)
        #expect(model.summary.lastFailure?.code == "mqtt_subscription_failed")
        #expect(model.summary.lastFailure?.stage == "subscribe")

        await model.share()
        let file = try #require(model.shareFile)
        let sharedBytes = try Data(contentsOf: file.url)
        #expect(sharedBytes == expected.bytes)
        #expect(String(decoding: sharedBytes, as: UTF8.self) == model.fullReport)
    }

    @Test("loading and previewing write no file")
    func loadHasNoFileSideEffect() async {
        let service = StubSessionService()
        service.diagnosticsHandler = { _ in self.capture() }
        let staging = makeTempDirectory()
        let model = DiagnosticsModel(service: service, stagingDirectory: staging)

        await model.load()
        await Task.yield()

        #expect(model.loadState == .loaded)
        #expect(stagedFiles(in: staging).isEmpty)
        #expect(model.shareFile == nil)
    }

    @Test("an explicit refresh creates a new capture; load does not")
    func refreshCreatesANewCapture() async {
        let service = StubSessionService()
        var count = 0
        service.diagnosticsHandler = { _ in
            count += 1
            return self.capture(
                self.document(capturedAt: 1_800_000_000_000 + Int64(count))
            )
        }
        let model = DiagnosticsModel(service: service, stagingDirectory: makeTempDirectory())

        await model.load()
        let first = model.fullReport
        await model.load()
        #expect(service.diagnosticsCaptureCount == 1)
        #expect(model.fullReport == first)

        await model.refresh()
        #expect(service.diagnosticsCaptureCount == 2)
        #expect(service.diagnosticsRefreshFlags == [false, true])
        #expect(model.fullReport != first)
        let capturedMs = Int64(
            (model.summary.capturedAt?.timeIntervalSince1970 ?? 0) * 1000
        )
        #expect(capturedMs == 1_800_000_000_002)
    }

    @Test("a failed refresh offers no stale share action")
    func failedRefreshClearsCaptureAndFile() async {
        let service = StubSessionService()
        service.diagnosticsHandler = { _ in self.capture() }
        let staging = makeTempDirectory()
        let model = DiagnosticsModel(service: service, stagingDirectory: staging)
        await model.load()
        await model.share()
        let prepared = model.shareFile?.url
        #expect(prepared != nil)

        service.diagnosticsHandler = { _ in throw SessionServiceError(.hostOffline) }
        await model.refresh()

        #expect(model.capture == nil)
        #expect(model.fullReport.isEmpty)
        #expect(model.shareFile == nil)
        #expect(!model.canShare)
        #expect(model.errorKey == .errorHostOffline)
        if let prepared {
            #expect(!FileManager.default.fileExists(atPath: prepared.path))
        }
        #expect(stagedFiles(in: staging).isEmpty)
    }

    // MARK: - Share

    @Test("one Share tap stages a unique protected copy and presents once")
    func shareStagesAndPresentsOnce() async throws {
        let service = StubSessionService()
        service.diagnosticsHandler = { _ in self.capture() }
        let staging = makeTempDirectory()
        let model = DiagnosticsModel(service: service, stagingDirectory: staging)
        await model.load()

        await model.share()
        #expect(model.shareCount == 1)
        let first = try #require(model.shareFile)
        #expect(stagedFiles(in: staging).map(\.lastPathComponent) == [first.url.lastPathComponent])

        // A second tap while the sheet is presenting must not stage or present
        // another sheet.
        await model.share()
        #expect(model.shareCount == 1)
        #expect(model.shareFile?.url == first.url)

        model.shareCompleted(completed: true)
        #expect(model.shareFile == nil)
        #expect(!FileManager.default.fileExists(atPath: first.url.path))
        #expect(model.statusKey == .diagnosticsExported)

        await model.share()
        #expect(model.shareCount == 2)
        let second = try #require(model.shareFile)
        #expect(second.url != first.url, "each share stages a unique temporary copy")
        #expect(stagedFiles(in: staging).count == 1)
    }

    @Test("dismissing or cancelling cleans the temporary copy and sends nothing")
    func cancellationCleansUp() async throws {
        let service = StubSessionService()
        service.diagnosticsHandler = { _ in self.capture() }
        let staging = makeTempDirectory()
        let model = DiagnosticsModel(service: service, stagingDirectory: staging)
        await model.load()
        await model.share()
        let file = try #require(model.shareFile)

        model.shareCompleted(completed: false)

        #expect(!FileManager.default.fileExists(atPath: file.url.path))
        #expect(model.statusKey == .diagnosticsCancelled)
        #expect(stagedFiles(in: staging).isEmpty)
    }

    @Test("a write failure opens no picker")
    func writeFailureOpensNoPicker() async {
        let service = StubSessionService()
        service.diagnosticsHandler = { _ in self.capture() }
        let parent = makeTempDirectory()
        // A regular file where the staging directory should be makes staging
        // fail before any picker can be opened.
        let blocked = parent.appendingPathComponent("blocked")
        try? Data("not a directory".utf8).write(to: blocked)
        let model = DiagnosticsModel(service: service, stagingDirectory: blocked)
        await model.load()

        await model.share()

        #expect(model.shareFile == nil)
        #expect(model.shareCount == 0)
        #expect(model.errorKey == .errorInternal)
    }

    // MARK: - Local decoration

    private func decoratingService(
        names: @escaping (DiagnosticsCapture) async -> [DiagnosticsLocalHost]
    ) -> StubSessionService {
        let service = StubSessionService()
        service.diagnosticsHandler = { _ in self.capture() }
        service.diagnosticsLocalHostsHandler = names
        return service
    }

    @Test("directory decoration is local-only and never in the bytes")
    func localDecorationStaysOutsideTheReport() async throws {
        let service = decoratingService { _ in
            [
                DiagnosticsLocalHost(label: "host-1", displayName: "Studio Mac", osFamily: "macOS"),
                DiagnosticsLocalHost(label: "host-2", displayName: "Studio Mac", osFamily: "macOS"),
            ]
        }
        let staging = makeTempDirectory()
        let model = DiagnosticsModel(service: service, stagingDirectory: staging)
        await model.load()

        #expect(model.localHost(for: "host-1")?.displayName == "Studio Mac")
        #expect(model.localHost(for: "host-2")?.osFamily == "macOS")
        #expect(!model.fullReport.contains("Studio Mac"))
        #expect(!model.fullReport.contains("macOS"))

        await model.share()
        let file = try #require(model.shareFile)
        let shared = try String(contentsOf: file.url, encoding: .utf8)
        #expect(!shared.contains("Studio Mac"))
        #expect(!shared.contains("macOS"))
        #expect(shared == model.fullReport)
    }

    @Test("a missing or ambiguous mapping falls back to anonymous labels")
    func ambiguousMappingFallsBack() async {
        let duplicate = decoratingService { _ in
            [
                DiagnosticsLocalHost(label: "host-1", displayName: "One", osFamily: "macOS"),
                DiagnosticsLocalHost(label: "host-1", displayName: "Two", osFamily: "Linux"),
            ]
        }
        let model = DiagnosticsModel(service: duplicate, stagingDirectory: makeTempDirectory())
        await model.load()
        #expect(model.localHost(for: "host-1") == nil)
        #expect(model.summary.hosts.first?.label == "host-1")

        let unknown = decoratingService { _ in
            [DiagnosticsLocalHost(label: "host-9", displayName: "Foreign", osFamily: "Linux")]
        }
        let second = DiagnosticsModel(service: unknown, stagingDirectory: makeTempDirectory())
        await second.load()
        #expect(second.localHosts.isEmpty)

        let blank = decoratingService { _ in
            [DiagnosticsLocalHost(label: "host-1", displayName: "   ", osFamily: "macOS")]
        }
        let third = DiagnosticsModel(service: blank, stagingDirectory: makeTempDirectory())
        await third.load()
        #expect(third.localHost(for: "host-1") == nil)
    }

    @Test("the inspected document is not refreshed implicitly")
    func repeatedLoadKeepsTheCapture() async {
        let service = StubSessionService()
        service.diagnosticsHandler = { _ in self.capture() }
        let model = DiagnosticsModel(service: service, stagingDirectory: makeTempDirectory())
        await model.load()
        let first = model.capture

        await model.load()

        #expect(model.capture == first)
        #expect(service.diagnosticsCaptureCount == 1)
    }

    @Test("a profile switch drops the previous decoration")
    func profileSwitchDropsDecoration() async {
        let decorated = decoratingService { _ in
            [DiagnosticsLocalHost(label: "host-1", displayName: "Studio Mac", osFamily: "macOS")]
        }
        let first = DiagnosticsModel(service: decorated, stagingDirectory: makeTempDirectory())
        await first.load()
        #expect(first.localHost(for: "host-1")?.displayName == "Studio Mac")

        // Replacing the runtime creates a new model for the newly selected
        // profile; the old model never leaks its decoration into the new one.
        let plain = StubSessionService()
        plain.diagnosticsHandler = { _ in self.capture() }
        let second = DiagnosticsModel(service: plain, stagingDirectory: makeTempDirectory())
        await second.load()

        #expect(second.localHost(for: "host-1") == nil)
        #expect(!second.fullReport.contains("Studio Mac"))
        #expect(first.localHost(for: "host-1") != nil)
    }

    @Test("loss of entitlement drops decoration on the next capture")
    func entitlementLossDropsDecoration() async {
        let service = decoratingService { _ in
            [DiagnosticsLocalHost(label: "host-1", displayName: "Studio Mac", osFamily: "macOS")]
        }
        let model = DiagnosticsModel(service: service, stagingDirectory: makeTempDirectory())
        await model.load()
        #expect(model.localHost(for: "host-1") != nil)

        service.diagnosticsLocalHostsHandler = { _ in [] }
        await model.refresh()

        #expect(model.localHosts.isEmpty)
        #expect(!model.fullReport.contains("Studio Mac"))
    }

    // MARK: - Failure and parsing

    @Test("a diagnostics load failure is localized and offers no file")
    func loadFailure() async {
        let service = StubSessionService()
        service.diagnosticsHandler = { _ in throw SessionServiceError(.hostOffline) }
        let model = DiagnosticsModel(service: service, stagingDirectory: makeTempDirectory())
        await model.load()

        #expect(model.loadState == .failed(.hostOffline))
        #expect(model.errorKey == .errorHostOffline)
        #expect(model.shareFile == nil)
        #expect(!model.canShare)
    }

    @Test("an unknown connection state is shown verbatim, not guessed")
    func unknownConnectionState() {
        let summary = DiagnosticsSummaryParser.parse(
            document(hosts: [("host-1", "mystery_state")])
        )
        let host = summary.hosts.first
        #expect(host?.connectionState == "mystery_state")
        #expect(host?.connectionKey == nil)
        #expect(host?.connectionLabel == "mystery_state")
    }

    @Test("invalid capture bytes yield an empty summary without exposing input")
    func invalidDocumentYieldsEmptySummary() {
        let summary = DiagnosticsSummaryParser.parse("{ not json")
        #expect(summary == .empty)
    }
}