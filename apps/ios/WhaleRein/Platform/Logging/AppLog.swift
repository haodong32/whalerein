// The only logging entry point in the app.
//
// It wraps `os.Logger` (per-feature category) for Console/Instruments and the
// persistent `AppLogStore` for on-device support. Records are redacted before
// they reach either sink; interpolations stay private by default.
//
// Level floors are compile-time: Debug keeps `debug`, Release starts at
// `info`. There is no runtime level switch in Release.

import Foundation
import os

final class AppLog: @unchecked Sendable {
    /// Process-wide instance, installed by `AppRuntime.bootstrap` before any
    /// service opens. The placeholder never writes outside the temp directory.
    private static let sharedLock = NSLock()
    private nonisolated(unsafe) static var _shared = AppLog(
        store: AppLogStore(directory: URL(fileURLWithPath: NSTemporaryDirectory())),
        appVersion: "0"
    )

    static var shared: AppLog {
        sharedLock.locked { _shared }
    }

    /// Replaces the process logger once, at bootstrap. Later calls in the same
    /// process are ignored so a runtime rebuild cannot split the log.
    static func install(_ log: AppLog) {
        sharedLock.locked { _shared = log }
    }

    let store: AppLogStore
    let appVersion: String
    private let coreRevision: () -> String?
    private let loggers: [AppLogRecord.Category: Logger]

    init(
        store: AppLogStore,
        appVersion: String,
        coreRevision: @escaping () -> String? = { nil }
    ) {
        self.store = store
        self.appVersion = appVersion
        self.coreRevision = coreRevision
        var loggers: [AppLogRecord.Category: Logger] = [:]
        for category in AppLogRecord.Category.allCases {
            loggers[category] = Logger(
                subsystem: Bundle.main.bundleIdentifier ?? "com.whalerein.app",
                category: category.rawValue
            )
        }
        self.loggers = loggers
    }

    /// Convenience for the common in-process event.
    func log(
        _ level: AppLogRecord.Level,
        _ category: AppLogRecord.Category,
        _ event: String,
        correlation: String? = nil,
        errorCode: String? = nil,
        fields: [String: String] = [:]
    ) {
        record(
            level: level,
            category: category,
            event: event,
            source: .app,
            correlation: correlation,
            traceId: nil,
            errorCode: errorCode,
            fields: fields
        )
    }

    func record(
        level: AppLogRecord.Level,
        category: AppLogRecord.Category,
        event: String,
        source: AppLogRecord.Source,
        correlation: String?,
        traceId: String?,
        errorCode: String?,
        fields: [String: String]
    ) {
        let record = AppLogRecord(
            timeUnixMs: Int64(Date().timeIntervalSince1970 * 1000),
            level: level,
            category: category,
            event: LogRedactor.truncate(LogRedactor.sanitize(event), to: 64),
            source: source,
            correlationId: LogRedactor.correlation(correlation),
            traceId: LogRedactor.correlation(traceId),
            errorCode: errorCode.map { LogRedactor.truncate(LogRedactor.sanitize($0), to: 32) },
            appVersion: appVersion,
            coreRevision: coreRevision(),
            fields: LogRedactor.fields(fields)
        )
        store.append(record)
        loggers[category]?.log(level: level.osLevel, "\(record.event, privacy: .public)")
    }

    func flush() { store.flush() }

    func reset() { store.reset() }

    func exportSnapshot() throws -> URL { try store.exportSnapshot() }

    func discardExportStaging() { store.discardExportStaging() }

    /// Default store under Library/Logs/WhaleRein.
    static func makeDefaultStore(
        fileManager: FileManager = .default,
        appVersion: String
    ) -> AppLogStore {
        let base = fileManager.urls(for: .libraryDirectory, in: .userDomainMask).first
            ?? URL(fileURLWithPath: NSTemporaryDirectory())
        let directory = base
            .appendingPathComponent("Logs", isDirectory: true)
            .appendingPathComponent("WhaleRein", isDirectory: true)
        return AppLogStore(directory: directory, fileManager: fileManager)
    }
}

private extension AppLogRecord.Level {
    var osLevel: OSLogType {
        switch self {
        case .debug: .debug
        case .info: .info
        case .notice: .default
        case .error: .error
        case .fault: .fault
        }
    }
}


private extension NSLock {
    func locked<T>(_ body: () -> T) -> T {
        lock()
        defer { unlock() }
        return body()
    }
}
