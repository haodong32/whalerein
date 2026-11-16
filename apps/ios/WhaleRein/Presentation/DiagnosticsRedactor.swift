// Redaction for diagnostics previews and exports. The report is built by Core;
// this presentation-layer pass removes listed fields plus recognizable secrets
// and limits the preview to connection/version metadata.

import Foundation

enum DiagnosticsRedactor {
    static let mask = "[REDACTED]"

    static let previewKeys: Set<String> = [
        "schema", "generatedAtUnixMs", "appVersion", "coreRevision", "connection", "redaction",
    ]

    /// Redacts the listed fields recursively and masks values that look like
    /// credentials or invitation links.
    static func redact(json: String, fields: [String]) -> String {
        guard let data = json.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) else {
            return redactLines(json, fields: fields)
        }
        let redacted = redact(object: object, fields: Set(fields))
        guard let output = try? JSONSerialization.data(
            withJSONObject: redacted,
            options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        ) else {
            return redactLines(json, fields: fields)
        }
        return String(decoding: output, as: UTF8.self)
    }

    /// The user-visible preview keeps only metadata needed to diagnose a
    /// connection, so message content and tool parameters cannot leak.
    static func preview(report: DiagnosticsReport) -> String {
        let redacted = redact(json: report.json, fields: report.redactionFields)
        guard let data = redacted.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return redacted
        }
        let limited = object.filter { previewKeys.contains($0.key) }
        guard let output = try? JSONSerialization.data(
            withJSONObject: limited,
            options: [.prettyPrinted, .sortedKeys, .withoutEscapingSlashes]
        ) else {
            return redacted
        }
        return String(decoding: output, as: UTF8.self)
    }

    /// The exported file uses the same redaction but keeps all diagnostic
    /// sections so Host-side troubleshooting has context.
    static func exportText(report: DiagnosticsReport) -> String {
        redact(json: report.json, fields: report.redactionFields)
    }

    private static func redact(object: Any, fields: Set<String>, depth: Int = 0) -> Any {
        if depth > 16 { return mask }
        if let dictionary = object as? [String: Any] {
            var result: [String: Any] = [:]
            for (key, value) in dictionary {
                if fields.contains(key) || isSensitiveKey(key) {
                    result[key] = mask
                } else {
                    result[key] = redact(object: value, fields: fields, depth: depth + 1)
                }
            }
            return result
        }
        if let array = object as? [Any] {
            return array.map { redact(object: $0, fields: fields, depth: depth + 1) }
        }
        if let string = object as? String, looksSensitive(string) {
            return mask
        }
        return object
    }

    private static func isSensitiveKey(_ key: String) -> Bool {
        let lowered = key.lowercased()
        return ["token", "secret", "password", "authorization", "cookie", "privatekey", "devicekey"]
            .contains { lowered.contains($0) }
    }

    private static func looksSensitive(_ value: String) -> Bool {
        let lowered = value.lowercased()
        return lowered.hasPrefix("whalerein://pair")
            || lowered.contains("token=")
            || lowered.hasPrefix("inv_")
            || lowered.contains("private_key")
    }

    private static func redactLines(_ json: String, fields: [String]) -> String {
        json.split(separator: "\n", omittingEmptySubsequences: false).map { line -> String in
            let lowered = line.lowercased()
            if fields.contains(where: { lowered.contains("\($0.lowercased())") })
                || ["token", "secret", "password", "privatekey"].contains(where: { lowered.contains($0) }) {
                return mask
            }
            return String(line)
        }.joined(separator: "\n")
    }
}
