// Host protocol compatibility. Core maps a Host's reported protocol version
// into `HostSummary.protocolVersion`; the app must turn an incompatible value
// into an explicit upgrade state instead of failing later with a generic error.
//
// An absent, zero or non-numeric version means the peer is older than the
// versioned contract. It is not "unknown, therefore compatible": the Host must
// upgrade, so it is presented as an upgrade state rather than a retryable
// outage.

import Foundation

enum ProtocolCompatibility {
    /// Major protocol version this client understands. The mobile binding and
    /// Core ship together, so only the major line is negotiable here.
    static let supportedMajor = 1

    /// Human-readable supported line used in upgrade guidance.
    static var supportedDisplay: String { "\(supportedMajor)" }

    /// Parses the leading integer of a version string. `"1"`, `"1.4"` and
    /// `"1.0.0"` all report major 1; an empty or non-numeric value has no major.
    static func major(of raw: String) -> Int? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return nil }
        let digits = trimmed.prefix { $0.isNumber }
        guard !digits.isEmpty else { return nil }
        return Int(digits)
    }

    /// True only when the Host reports exactly the supported major line.
    static func isCompatible(_ raw: String) -> Bool {
        major(of: raw) == supportedMajor
    }

    /// Returns the display version when incompatible, or nil when compatible.
    /// An absent version is normalized to `"0"` so the presentation can explain
    /// that the Host is an older peer rather than showing an empty value.
    static func incompatibility(for raw: String) -> String? {
        isCompatible(raw) ? nil : displayVersion(raw)
    }

    static func displayVersion(_ raw: String) -> String {
        guard let major = major(of: raw), major > 0 else { return "0" }
        return raw.trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// True for an older peer that did not report a usable version.
    static func isLegacyPeer(_ raw: String) -> Bool {
        displayVersion(raw) == "0"
    }
}
