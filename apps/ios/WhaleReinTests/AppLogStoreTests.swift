// Store behavior: bounds, rotation, redaction, export, reset and failure
// isolation. Uses a temporary directory so tests never touch the real log.

import XCTest
@testable import WhaleRein

final class AppLogStoreTests: XCTestCase {
    private var directory: URL!

    override func setUpWithError() throws {
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("AppLogStoreTests-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws {
        try? FileManager.default.removeItem(at: directory)
    }

    private func makeRecord(
        _ event: String = "test.event",
        level: AppLogRecord.Level = .info,
        fields: [String: String] = [:],
        time: Date = Date()
    ) -> AppLogRecord {
        AppLogRecord(
            timeUnixMs: Int64(time.timeIntervalSince1970 * 1000),
            level: level,
            category: .runtime,
            event: event,
            source: .app,
            correlationId: "correlation",
            traceId: nil,
            errorCode: nil,
            appVersion: "test",
            coreRevision: "rev",
            fields: fields
        )
    }

    func testAppendedRecordsSurviveFlushAndRelaunch() throws {
        let bounds = AppLogStore.Bounds(memoryBytes: 64 * 1024)
        let store = AppLogStore(directory: directory, bounds: bounds)
        store.append(makeRecord("first"))
        store.flush()

        // A new store over the same directory sees the persisted line.
        let reopened = AppLogStore(directory: directory, bounds: bounds)
        reopened.append(makeRecord("second"))
        reopened.flush()

        let text = try String(contentsOf: reopened.currentURL, encoding: .utf8)
        XCTAssertTrue(text.contains("first"))
        XCTAssertTrue(text.contains("second"))
        XCTAssertEqual(text.split(separator: "\n").count, 2)
    }

    func testMemoryBoundDropsOldestAndCounts() throws {
        // Suspend the writer so the memory bound, not the disk, decides.
        let writer = DispatchQueue(label: "AppLogStoreTests.writer")
        writer.suspend()
        let bounds = AppLogStore.Bounds(memoryBytes: 1024)
        let store = AppLogStore(directory: directory, bounds: bounds, writer: writer)
        for index in 0..<40 {
            store.append(makeRecord("event-\(index)"))
        }
        XCTAssertGreaterThan(store.droppedCount, 0)
        writer.resume()
        store.flush()
        let text = try String(contentsOf: store.currentURL, encoding: .utf8)
        XCTAssertTrue(text.contains("event-39"))
        XCTAssertFalse(text.contains("\"event\":\"event-0\""))
    }

    func testRotationKeepsTwoBoundedSegments() throws {
        let bounds = AppLogStore.Bounds(memoryBytes: 1024 * 1024, segmentBytes: 2048)
        let store = AppLogStore(directory: directory, bounds: bounds)
        for index in 0..<40 {
            store.append(makeRecord("event-\(index)"))
            store.flush()
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: store.rotatedURL.path))
        let current = try FileManager.default.attributesOfItem(atPath: store.currentURL.path)[.size] as? Int ?? 0
        let rotated = try FileManager.default.attributesOfItem(atPath: store.rotatedURL.path)[.size] as? Int ?? 0
        XCTAssertLessThanOrEqual(current, bounds.segmentBytes)
        XCTAssertLessThanOrEqual(rotated, bounds.segmentBytes)
    }

    func testExportWritesSingleStagingSnapshot() throws {
        let store = AppLogStore(directory: directory)
        store.append(makeRecord("exported"))
        store.flush()
        let first = try store.exportSnapshot()
        XCTAssertTrue(FileManager.default.fileExists(atPath: first.path))
        let text = try String(contentsOf: first, encoding: .utf8)
        XCTAssertTrue(text.contains("exported"))

        let second = try store.exportSnapshot()
        XCTAssertEqual(first, second)
        XCTAssertEqual(try FileManager.default.contentsOfDirectory(
            at: first.deletingLastPathComponent(),
            includingPropertiesForKeys: nil
        ).count, 1)

        store.discardExportStaging()
        XCTAssertFalse(FileManager.default.fileExists(atPath: first.path))
    }

    func testResetDeletesBothSegments() throws {
        let bounds = AppLogStore.Bounds(segmentBytes: 512)
        let store = AppLogStore(directory: directory, bounds: bounds)
        for index in 0..<10 {
            store.append(makeRecord("event-\(index)"))
            store.flush()
        }
        XCTAssertTrue(FileManager.default.fileExists(atPath: store.rotatedURL.path))
        store.reset()
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.currentURL.path))
        XCTAssertFalse(FileManager.default.fileExists(atPath: store.rotatedURL.path))
    }

    func testDebugLevelIsDroppedInReleaseFloor() throws {
        let store = AppLogStore(directory: directory)
        store.append(makeRecord("debug-only", level: .debug))
        store.flush()
        let text = (try? String(contentsOf: store.currentURL, encoding: .utf8)) ?? ""
        #if DEBUG
        XCTAssertTrue(text.contains("debug-only"))
        #else
        XCTAssertFalse(text.contains("debug-only"))
        #endif
    }
}

final class LogRedactorTests: XCTestCase {
    func testOnlyAllowlistedFieldNamesSurvive() {
        let redacted = LogRedactor.fields([
            "status": "ok",
            "message": "secret message contents",
            "token": "bearer abc",
            "elapsed_ms": "42"
        ])
        XCTAssertEqual(redacted, ["status": "ok", "elapsed_ms": "42"])
    }

    func testValuesAreSanitizedAndBounded() {
        let long = String(repeating: "x", count: 500)
        let redacted = LogRedactor.fields(["code": "bad\ncode\twith\rcontrols", "stage": long])
        XCTAssertEqual(redacted["code"], "bad code with controls")
        XCTAssertEqual(redacted["stage"]?.count, LogRedactor.maxFieldValueLength)
    }

    func testIdentifiersHashStablyAndDoNotLeak() {
        let first = LogRedactor.hashIdentifier("host-123")
        let second = LogRedactor.hashIdentifier("host-123")
        XCTAssertEqual(first, second)
        XCTAssertNotEqual(first, LogRedactor.hashIdentifier("host-124"))
        XCTAssertFalse(first.contains("host-123"))
        XCTAssertEqual(first.count, 16)
    }
}
