// Read-only summary projection of one captured diagnostic document.
//
// Core owns the single immutable redacted capture. This file only *reads* that
// document to group facts for the summary; it never rewrites, filters or
// re-serializes it, and it never receives the local directory decoration.
// Invalid or unexpected input yields an empty summary, never raw text.

import Foundation

struct DiagnosticsSummary: Equatable, Sendable {
    let schema: String?
    let capturedAt: Date?
    let coreVersion: String?
    let protocolVersion: Int?
    let address: String?
    let hosts: [DiagnosticsHostRow]
    let lastFailure: DiagnosticsFailureFact?
    let redactionFields: [String]
    let fieldNames: [String]

    static let empty = DiagnosticsSummary(
        schema: nil,
        capturedAt: nil,
        coreVersion: nil,
        protocolVersion: nil,
        address: nil,
        hosts: [],
        lastFailure: nil,
        redactionFields: [],
        fieldNames: []
    )

    var hasDocumentedFields: Bool {
        !hosts.isEmpty || lastFailure != nil || capturedAt != nil || schema != nil
    }
}

/// One anonymous Host row from the capture. `label` is snapshot-local; the
/// local directory decoration is resolved separately by the view model.
struct DiagnosticsHostRow: Identifiable, Equatable, Sendable {
    let label: String
    let connectionState: String

    var id: String { label }
}

struct DiagnosticsFailureFact: Equatable, Sendable {
    let code: String
    /// Stable stage/layer facts included by the capture, when it retains them.
    let layer: String?
    let stage: String?
}

enum DiagnosticsSummaryParser {
    static func parse(_ document: String) -> DiagnosticsSummary {
        guard let data = document.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return .empty
        }

        let report = object["report"] as? [String: Any]
        let failure = object["last_failure"] as? [String: Any]
        let events = failure?["events"] as? [[String: Any]]
        let lastEvent = events?.last

        return DiagnosticsSummary(
            schema: string(object, keys: ["schema"]),
            capturedAt: captureDate(object),
            coreVersion: string(object, keys: ["core_version", "coreRevision"]),
            protocolVersion: integer(object, keys: ["protocol_version", "protocolVersion"]),
            address: report.flatMap { string($0, keys: ["address"]) },
            hosts: hosts(object["hosts"]),
            lastFailure: failure.flatMap { failureFact($0, lastEvent: lastEvent) },
            redactionFields: strings(object["redactions"]),
            fieldNames: strings(object["fields"])
        )
    }

    private static func hosts(_ value: Any?) -> [DiagnosticsHostRow] {
        guard let entries = value as? [[String: Any]] else { return [] }
        return entries.compactMap { entry in
            guard let label = string(entry, keys: ["label"]), !label.isEmpty else {
                return nil
            }
            let state = string(entry, keys: ["connection_state", "connectionState"]) ?? "unknown"
            return DiagnosticsHostRow(label: label, connectionState: state)
        }
    }

    private static func failureFact(
        _ failure: [String: Any],
        lastEvent: [String: Any]?
    ) -> DiagnosticsFailureFact? {
        // The stable code is authoritative. A retained event may add the
        // layer/stage that produced it; nothing is inferred from the code text.
        guard let code = string(failure, keys: ["code"]) ?? lastEvent.flatMap({ string($0, keys: ["code"]) }),
              !code.isEmpty else {
            return nil
        }
        return DiagnosticsFailureFact(
            code: code,
            layer: lastEvent.flatMap { string($0, keys: ["layer"]) },
            stage: lastEvent.flatMap { string($0, keys: ["stage"]) }
        )
    }

    private static func captureDate(_ object: [String: Any]) -> Date? {
        guard let value = number(
            object,
            keys: ["captured_at_unix_ms", "generated_at_unix_ms", "generatedAtUnixMs"]
        ) else {
            return nil
        }
        return Date(timeIntervalSince1970: value.doubleValue / 1000)
    }

    private static func string(_ object: [String: Any], keys: [String]) -> String? {
        for key in keys {
            if let value = object[key] as? String, !value.isEmpty {
                return value
            }
        }
        return nil
    }

    private static func integer(_ object: [String: Any], keys: [String]) -> Int? {
        number(object, keys: keys).map { $0.intValue }
    }

    private static func number(_ object: [String: Any], keys: [String]) -> NSNumber? {
        for key in keys {
            if let value = object[key] as? NSNumber {
                return value
            }
        }
        return nil
    }

    private static func strings(_ value: Any?) -> [String] {
        (value as? [String]) ?? []
    }
}

extension DiagnosticsHostRow {
    /// Localized wording for a known connection state. An unknown state is
    /// shown verbatim as the captured fact instead of being guessed.
    var connectionKey: L10nKey? {
        switch connectionState.lowercased() {
        case "connected", "online": return .conversationConnectionOnline
        case "connecting": return .conversationConnectionConnecting
        case "reconnecting": return .conversationConnectionReconnecting
        case "degraded": return .conversationConnectionDegraded
        case "offline", "disconnected": return .conversationConnectionOffline
        default: return nil
        }
    }

    var connectionLabel: String {
        connectionKey?.string ?? connectionState
    }
}

extension DiagnosticsFailureFact {
    /// Stable failure wording for the row: code plus any retained stage facts.
    var displayText: String {
        let context = [layer, stage].compactMap { $0 }.joined(separator: " · ")
        return context.isEmpty ? code : "\(context) · \(code)"
    }
}