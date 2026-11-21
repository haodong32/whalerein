// Bridges Core's existing redacted diagnostic stream into the app log.
//
// Core produces the events; this adapter never re-derives policy. It validates
// the level and event name, applies the shared field redactor to values Core
// already treats as redacted, assigns a receipt timestamp and appends to the
// same bounded store as app records. Failure is swallowed by the binding, so a
// broken sink can never change a Core result.

import Foundation
import WhaleReinMobile

final class CoreDiagnosticBridge: MobileDiagnosticSink, @unchecked Sendable {
    private let log: AppLog
    private let clock: () -> Date
    private let lock = NSLock()
    private var received = 0
    private var rejected = 0

    init(log: AppLog, clock: @escaping () -> Date = { Date() }) {
        self.log = log
        self.clock = clock
    }

    /// Records accepted/rejected by the bridge. Exposed for diagnostics tests.
    var counters: (received: Int, rejected: Int) {
        lock.locked { (received, rejected) }
    }

    func onEvent(event: DiagnosticEventView) {
        guard let level = Self.level(from: event.level),
              Self.isValidEventName(event.event) else {
            lock.locked { rejected += 1 }
            return
        }
        lock.locked { received += 1 }
        log.record(
            level: level,
            category: Self.category(for: event.event),
            event: event.event,
            source: .core,
            correlation: event.traceId,
            traceId: event.traceId,
            errorCode: event.fields["code"],
            fields: event.fields
        )
    }

    private static func level(from raw: String) -> AppLogRecord.Level? {
        switch raw {
        case "debug": .debug
        case "info": .info
        case "warn": .notice
        case "error": .error
        default: nil
        }
    }

    /// Core event names are stable snake_case identifiers. Anything else is a
    /// programming error and is rejected rather than written as free-form text.
    private static func isValidEventName(_ name: String) -> Bool {
        guard (1...64).contains(name.count) else { return false }
        return name.allSatisfy { $0.isLowercase || $0.isNumber || $0 == "_" }
    }

    private static func category(for event: String) -> AppLogRecord.Category {
        if event.hasPrefix("http") || event.hasPrefix("access_request") {
            return .cloud
        }
        if event.hasPrefix("mqtt") || event.hasPrefix("operation") {
            return .conversation
        }
        if event.hasPrefix("configuration") {
            return .runtime
        }
        return .runtime
    }
}

private extension NSLock {
    func locked<T>(_ body: () -> T) -> T {
        lock()
        defer { unlock() }
        return body()
    }
}
