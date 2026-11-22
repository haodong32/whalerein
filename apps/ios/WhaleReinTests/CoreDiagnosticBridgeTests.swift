// Bridge behavior: only valid Core events reach the store, levels map onto the
// app scale, and a rejected event never blocks the caller.

import XCTest
import WhaleReinMobile
@testable import WhaleRein

final class CoreDiagnosticBridgeTests: XCTestCase {
    private var directory: URL!
    private var log: AppLog!

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("CoreBridgeTests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        log = AppLog(store: AppLogStore(directory: directory), appVersion: "test")
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: directory)
    }

    private func event(
        _ name: String,
        level: String = "info",
        fields: [String: String] = [:]
    ) -> DiagnosticEventView {
        DiagnosticEventView(
            timeUnixMs: 1_700_000_000_000,
            traceId: "trace-abc",
            level: level,
            event: name,
            fields: fields
        )
    }

    func testValidCoreEventIsPersistedWithCorrelation() throws {
        let bridge = CoreDiagnosticBridge(log: log)
        bridge.onEvent(event: event(
            "operation_diagnostic",
            level: "warn",
            fields: ["stage": "send", "outcome": "failed", "code": "timeout"]
        ))
        log.flush()

        let text = try String(contentsOf: log.store.currentURL, encoding: .utf8)
        let record = try XCTUnwrap(
            JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any]
        )
        XCTAssertEqual(record["source"] as? String, "core")
        XCTAssertEqual(record["level"] as? String, "notice")
        XCTAssertEqual(record["correlationId"] as? String, "trace-abc")
        XCTAssertEqual(record["traceId"] as? String, "trace-abc")
        XCTAssertEqual(record["errorCode"] as? String, "timeout")
        XCTAssertEqual(bridge.counters.received, 1)
        XCTAssertEqual(bridge.counters.rejected, 0)
    }

    func testUnknownLevelAndFreeFormNameAreRejected() throws {
        let bridge = CoreDiagnosticBridge(log: log)
        bridge.onEvent(event: event("operation", level: "trace"))
        bridge.onEvent(event: event("not a stable name"))
        bridge.onEvent(event: event("operation", level: "error"))
        log.flush()

        XCTAssertEqual(bridge.counters.rejected, 2)
        XCTAssertEqual(bridge.counters.received, 1)
        let text = try String(contentsOf: log.store.currentURL, encoding: .utf8)
        XCTAssertEqual(text.split(separator: "\n").count, 1)
    }

    func testCoreFieldsOutsideTheAllowlistAreDropped() throws {
        let bridge = CoreDiagnosticBridge(log: log)
        bridge.onEvent(event: event(
            "http_completed",
            fields: ["status": "503", "body": "secret", "token": "abc"]
        ))
        log.flush()
        let text = try String(contentsOf: log.store.currentURL, encoding: .utf8)
        XCTAssertTrue(text.contains("\"status\":\"503\""))
        XCTAssertFalse(text.contains("secret"))
        XCTAssertFalse(text.contains("\"token\""))
    }
}
