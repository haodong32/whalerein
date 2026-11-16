// Diagnostics redaction, preview filtering, confirmed export, and localized
// stage/error selection.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Diagnostics", .serialized)
@MainActor
struct DiagnosticsTests {
    @Test("redaction masks every listed field and recognizable secret")
    func redaction() {
        let report = FixtureCatalog.diagnostics()
        let redacted = DiagnosticsRedactor.exportText(report: report)

        #expect(!redacted.contains("inv_SENSITIVE_VALUE"))
        #expect(!redacted.contains("PRIVATE_KEY_MATERIAL"))
        #expect(!redacted.contains("Do not export this sentence"))
        #expect(!redacted.contains("SENSITIVE_ARGUMENT"))
        #expect(redacted.contains(DiagnosticsRedactor.mask))
        #expect(redacted.contains("mqtt_subscription_failed"))
        #expect(redacted.contains("corr-mqtt-0007"))
    }

    @Test("the preview keeps connection metadata but excludes message content")
    func preview() {
        let report = FixtureCatalog.diagnostics()
        let preview = DiagnosticsRedactor.preview(report: report)

        #expect(preview.contains("coreRevision"))
        #expect(preview.contains("corr-https-0001"))
        #expect(!preview.contains("Do not export this sentence"))
        #expect(!preview.contains("SENSITIVE_ARGUMENT"))
        #expect(!preview.contains("inv_SENSITIVE_VALUE"))
    }

    @Test("preview distinguishes HTTPS success from MQTT failure")
    func stages() async {
        guard let service = try? await makePairedFixture() else {
            Issue.record("fixture service could not pair")
            return
        }
        let model = DiagnosticsModel(service: service)
        await model.load()

        #expect(model.loadState == .loaded)
        #expect(model.stages.count == 4)
        let https = model.stages.first { $0.hostId == "studio-mac" && $0.kind == .https }
        let mqtt = model.stages.first { $0.hostId == "studio-mac" && $0.kind == .mqtt }
        #expect(https?.status == .ok)
        #expect(mqtt?.status == .failed)
        #expect(mqtt?.failureCode == "mqtt_subscription_failed")
        #expect(model.lastFailureCode == "mqtt_subscription_failed")
    }

    @Test("stage status labels are localized for every state")
    func stageLabels() {
        for status in [DiagnosticStageStatus.ok, .failed, .idle] {
            let key = L10nKey.diagnosticStageKey(for: status)
            #expect(!key.string.isEmpty)
            #expect(key.string != key.rawValue)
        }
        #expect(L10nKey.diagnosticStageKey(for: .ok) != L10nKey.diagnosticStageKey(for: .failed))
    }

    @Test("cancelling the share confirmation exports nothing")
    func cancelShare() async {
        let service = StubSessionService()
        let model = DiagnosticsModel(service: service)
        await model.load()
        model.cancelShare()

        #expect(model.exportCount == 0)
        #expect(model.shareFile == nil)
        #expect(model.statusKey == .diagnosticsCancelled)
        #expect(service.exportCount == 0)
    }

    @Test("confirming the share writes a redacted file and presents it")
    func confirmedShare() async {
        let service = StubSessionService()
        let model = DiagnosticsModel(service: service)
        await model.load()
        await model.confirmShare()

        guard let file = model.shareFile else {
            Issue.record("expected a share file")
            return
        }
        #expect(model.exportCount == 1)
        #expect(model.statusKey == .diagnosticsExported)
        let contents = try? String(contentsOf: file.url, encoding: .utf8)
        #expect(contents != nil)
        #expect(contents?.contains("inv_SENSITIVE_VALUE") == false)
        #expect(contents?.contains("Do not export this sentence") == false)
        #expect(contents?.contains("mqtt_subscription_failed") == true)
    }

    @Test("a diagnostics load failure is localized")
    func loadFailure() async {
        let service = StubSessionService()
        service.diagnosticsHandler = { throw SessionServiceError(.hostOffline) }
        let model = DiagnosticsModel(service: service)
        await model.load()

        #expect(model.loadState == .failed(.hostOffline))
        #expect(model.errorKey == .errorHostOffline)
    }
}
