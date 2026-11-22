// One persisted app log record. The schema is versioned and deliberately flat:
// every field is allowlisted before it is encoded, so a record can be appended
// without a second serialization pass.
//
// Redaction happens before the record reaches the store. `fields` may only
// contain values produced by `LogRedactor`.

import Foundation

struct AppLogRecord: Codable, Equatable, Sendable {
    /// Bumped when the on-disk shape changes in a way readers must know about.
    static let schemaVersion = 1

    enum Level: String, Codable, Sendable, CaseIterable, Comparable {
        case debug
        case info
        case notice
        case error
        case fault

        private var rank: Int {
            switch self {
            case .debug: 0
            case .info: 1
            case .notice: 2
            case .error: 3
            case .fault: 4
            }
        }

        static func < (lhs: Level, rhs: Level) -> Bool { lhs.rank < rhs.rank }

        /// Compile-time floor: Debug builds keep debug events, Release builds
        /// start at info. There is no runtime switch in Release.
        static var floor: Level {
            #if DEBUG
            .debug
            #else
            .info
            #endif
        }
    }

    enum Category: String, Codable, Sendable, CaseIterable {
        case conversation
        case cloud
        case account
        case settings
        case pairing
        case diagnostics
        case runtime
        case session
        case attachments
    }

    enum Source: String, Codable, Sendable {
        case app
        case core
    }

    var schemaVersion = AppLogRecord.schemaVersion
    var timeUnixMs: Int64
    var level: Level
    var category: Category
    /// Stable event/operation name; never free-form text.
    var event: String
    var source: Source
    /// Correlates records produced by one user action or Core trace.
    var correlationId: String?
    var traceId: String?
    var errorCode: String?
    var appVersion: String
    var coreRevision: String?
    /// Allowlisted scalar fields. Values are already redacted.
    var fields: [String: String]

}

/// Small, explicit redaction surface shared by app logs and the Core bridge.
///
/// The rules mirror the design: no message contents, tool parameters, pairing
/// links, credentials, tokens or keys; identifiers are hashed or truncated;
/// free-form text is never accepted.
enum LogRedactor {
    /// Fields an app or Core record may carry. Anything else is dropped.
    static let allowedFieldNames: Set<String> = [
        "status",
        "elapsed_ms",
        "layer",
        "stage",
        "outcome",
        "code",
        "polls",
        "bytes",
        "limit",
        "retry_in_ms",
        "reason",
        "reason_code",
        "request_kind",
        "count",
        "duration_ms",
        "capability",
        "operation",
        "screen",
        "mode",
        "attempt"
    ]

    /// Values longer than this are truncated; identifiers should be hashed
    /// before they get here.
    static let maxFieldValueLength = 96
    static let maxCorrelationLength = 64

    /// Keeps only allowlisted names and bounded scalar values. Newlines and
    /// control characters are stripped so one record always stays one line.
    static func fields(_ raw: [String: String]) -> [String: String] {
        var result: [String: String] = [:]
        for (name, value) in raw where allowedFieldNames.contains(name) {
            let bounded = truncate(sanitize(value), to: maxFieldValueLength)
            if !bounded.isEmpty {
                result[name] = bounded
            }
        }
        return result
    }

    /// A stable, non-reversible identifier for correlation. Uses a truncated
    /// SHA-256 so the same identifier correlates across records without being
    /// readable in an exported file.
    static func hashIdentifier(_ value: String) -> String {
        guard !value.isEmpty else { return "" }
        let digest = SHA256Digest.hex(Data(value.utf8))
        return String(digest.prefix(16))
    }

    /// Correlation ids are opaque tokens (UUIDs, trace ids). They are bounded
    /// and stripped of separators that could break the line format.
    static func correlation(_ value: String?) -> String? {
        guard let value, !value.isEmpty else { return nil }
        return truncate(sanitize(value), to: maxCorrelationLength)
    }

    static func sanitize(_ value: String) -> String {
        value
            .replacingOccurrences(of: "\n", with: " ")
            .replacingOccurrences(of: "\r", with: " ")
            .replacingOccurrences(of: "\t", with: " ")
            .filter { character in
                !character.isNewline && character.unicodeScalars.allSatisfy { scalar in
                    !CharacterSet.controlCharacters.contains(scalar)
                }
            }
            .trimmingCharacters(in: .whitespaces)
    }

    static func truncate(_ value: String, to limit: Int) -> String {
        guard value.count > limit else { return value }
        return String(value.prefix(limit))
    }
}
