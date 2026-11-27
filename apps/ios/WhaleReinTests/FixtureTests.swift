// Synthetic fixture determinism and coverage of the required transcript shapes.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Fixtures")
struct FixtureTests {
    @Test("the long transcript has at least 300 rows with unique stable ids")
    func longTranscript() {
        let messages = FixtureCatalog.conversation(sessionId: "s-001")
        #expect(messages.count >= 300)
        #expect(messages.map(\.messageId).count == Set(messages.map(\.messageId)).count)
        #expect(messages.first?.messageId == "msg-0001")
        #expect(messages.last?.messageId == "msg-0320")
    }

    @Test("repeated generation is identical")
    func determinism() {
        let first = FixtureCatalog.conversation(sessionId: "s-001")
        let second = FixtureCatalog.conversation(sessionId: "s-001")
        #expect(first == second)
        #expect(FixtureCatalog.hosts == FixtureCatalog.hosts)
        #expect(FixtureCatalog.sessions(hostId: "studio-mac") == FixtureCatalog.sessions(hostId: "studio-mac"))
    }

    @Test("the transcript includes Markdown, code, tools, notices and questions")
    func contentKinds() {
        let messages = FixtureCatalog.conversation(sessionId: "s-001")
        let blocks = messages.flatMap(\.blocks)
        #expect(blocks.contains { $0.kind == .tool })
        #expect(blocks.contains { $0.kind == .notice })
        #expect(blocks.contains { $0.kind == .question })
        #expect(messages.contains { $0.role == .user })
        #expect(messages.contains { $0.role == .assistant })
        #expect(messages.contains { $0.role == .tool })
        #expect(messages.contains { $0.role == .notice })
        #expect(messages.contains { $0.plainText.contains("```swift") })
        #expect(messages.contains { $0.plainText.contains("## Release summary") })
        #expect(messages.contains { $0.plainText.contains("**") })
    }

    @Test("the final message carries a copyable code block")
    func finalCodeBlock() {
        let last = FixtureCatalog.conversation(sessionId: "s-001").last
        let segments = MarkdownParser.parse(last?.plainText ?? "")
        #expect(segments.contains { if case .code = $0.kind { return true } else { return false } })
    }

    @Test("hosts and sessions are host-scoped and sorted by activity")
    func hostScopedSessions() {
        #expect(FixtureCatalog.hosts.count >= 2)
        #expect(FixtureCatalog.sessions(hostId: "studio-mac").first?.sessionId == "s-001")
        #expect(FixtureCatalog.sessions(hostId: "travel-laptop").first?.sessionId == "s-101")
        #expect(FixtureCatalog.sessions(hostId: "studio-mac").allSatisfy { $0.sessionId != "s-101" })
        let timestamps = FixtureCatalog.sessions(hostId: "studio-mac").map(\.updatedAtUnixMs)
        #expect(timestamps == timestamps.sorted(by: >))
    }

    @Test("approval variants expose pending, expired and reviewable shapes")
    func approvalVariants() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let current = FixtureCatalog.approval(sessionId: "s-001", now: now, variant: .current)
        #expect(current.state == .pending)
        #expect(current.version == 3)
        #expect(current.expiresAtUnixMs > Int64(now.timeIntervalSince1970 * 1000))
        #expect(!current.digestHex.isEmpty)

        let expired = FixtureCatalog.approval(sessionId: "s-001", now: now, variant: .expired)
        #expect(expired.state == .expired)
        #expect(expired.expiresAtUnixMs < Int64(now.timeIntervalSince1970 * 1000))
    }

    @Test("the diagnostics sample captures two Host rows and a retained failure")
    func diagnosticsSample() {
        let capture = FixtureCatalog.diagnostics(now: Date(timeIntervalSince1970: 1_800_000_000))
        let summary = DiagnosticsSummaryParser.parse(capture.text)
        #expect(summary.hosts.map(\.label) == ["host-1", "host-2"])
        #expect(summary.hosts.first?.connectionState == "connected")
        #expect(summary.hosts.last?.connectionState == "offline")
        #expect(capture.redactionFields.contains("device tokens"))
        #expect(summary.lastFailure?.code == "mqtt_subscription_failed")
        #expect(summary.coreVersion == FixtureCatalog.coreRevision)
        #expect(String(decoding: capture.bytes, as: UTF8.self) == capture.text)
    }

    @Test("seeded random values are reproducible")
    func seededRandom() {
        var first = SeededRandom(seed: 42)
        var second = SeededRandom(seed: 42)
        var third = SeededRandom(seed: 43)
        let firstValues = (0..<16).map { _ in first.next() }
        let secondValues = (0..<16).map { _ in second.next() }
        let thirdValues = (0..<16).map { _ in third.next() }
        #expect(firstValues == secondValues)
        #expect(firstValues != thirdValues)
    }

    @Test("the fixture transcript differs between sessions")
    func sessionSpecificTranscript() {
        let release = FixtureCatalog.conversation(sessionId: "s-001")
        let triage = FixtureCatalog.conversation(sessionId: "s-002")
        #expect(release.count != triage.count)
        #expect(release[1].plainText != triage[1].plainText)
    }

    @Test("pairing prompts expose a stable fingerprint")
    func pairingFingerprint() {
        #expect(FixtureCatalog.fingerprint.hasPrefix("SHA256:"))
        #expect(FixtureCatalog.fingerprintDisplay.count > 20)
    }
}
