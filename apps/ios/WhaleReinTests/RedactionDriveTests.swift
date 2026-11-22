// Drives a synthetic success, a typed failure and a retry through the real log
// facade and the Core bridge, then asserts the persisted records carry the
// category, level and correlation but never the sensitive marker.

import XCTest
import WhaleReinMobile
@testable import WhaleRein

final class RedactionDriveTests: XCTestCase {
    private var directory: URL!
    private var log: AppLog!
    private let marker = "SENSITIVE_MARKER_9f4c"

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("RedactionDrive-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        log = AppLog(store: AppLogStore(directory: directory), appVersion: "test")
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: directory)
    }

    private func lines() throws -> [[String: Any]] {
        log.flush()
        let text = try String(contentsOf: log.store.currentURL, encoding: .utf8)
        return try text.split(separator: "\n").map { line in
            try XCTUnwrap(
                JSONSerialization.jsonObject(with: Data(line.utf8)) as? [String: Any]
            )
        }
    }

    func testSyntheticOperationFailureAndRetryAreRedacted() throws {
        // Success, then a typed failure, then a retry: one category and level per
        // record, with a stable correlation id.
        log.log(.info, .conversation, "operation_started", correlation: "op-1",
                fields: ["status": "started", "message": marker])
        log.log(.error, .conversation, "operation_failed", correlation: "op-1",
                errorCode: "timeout", fields: ["code": "timeout", "body": marker])
        log.log(.info, .conversation, "operation_retried", correlation: "op-1",
                fields: ["attempt": "2"])

        let records = try lines()
        XCTAssertEqual(records.count, 3)
        XCTAssertEqual(records.map { $0["category"] as? String }, ["conversation", "conversation", "conversation"])
        XCTAssertEqual(records.map { $0["level"] as? String }, ["info", "error", "info"])
        XCTAssertEqual(records.map { $0["correlationId"] as? String }, ["op-1", "op-1", "op-1"])
        XCTAssertEqual(records[1]["errorCode"] as? String, "timeout")

        let text = try String(contentsOf: log.store.currentURL, encoding: .utf8)
        XCTAssertFalse(text.contains(marker), "sensitive content must never be written")
        XCTAssertNil(records[0]["fields"].flatMap { ($0 as? [String: String])?["message"] })
        XCTAssertNil(records[1]["fields"].flatMap { ($0 as? [String: String])?["body"] })
    }

    func testCoreEventKeepsItsTraceIdAndDropsSensitiveFields() throws {
        let bridge = CoreDiagnosticBridge(log: log)
        bridge.onEvent(event: DiagnosticEventView(
            timeUnixMs: 1_700_000_000_000,
            traceId: "trace-42",
            level: "warn",
            event: "operation_diagnostic",
            fields: [
                "stage": "send",
                "outcome": "failed",
                "code": "timeout",
                "message": marker
            ],
        ))
        let record = try XCTUnwrap(try lines().first)
        XCTAssertEqual(record["source"] as? String, "core")
        XCTAssertEqual(record["level"] as? String, "notice")
        XCTAssertEqual(record["traceId"] as? String, "trace-42")
        XCTAssertEqual(record["correlationId"] as? String, "trace-42")
        let fields = try XCTUnwrap(record["fields"] as? [String: String])
        XCTAssertEqual(fields["stage"], "send")
        XCTAssertNil(fields["message"])
        let text = try String(contentsOf: log.store.currentURL, encoding: .utf8)
        XCTAssertFalse(text.contains(marker))
    }
}
