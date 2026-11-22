// Supporting value types for the settings state.
import Foundation

/// Facts read once when the feature is built.
struct SettingsInfo: Equatable, Sendable {
    let appVersion: String
    let coreRevision: String
    let dataPath: String
    let isStaging: Bool
    let isFixtureBacked: Bool
}

/// A destructive device action. Both clear local pairing state when they
/// succeed; they differ in what Core is asked to do.
enum SettingsAction: Equatable, Sendable {
    case forgetDevice
    case revokeDevice
}

/// Where the user is in a destructive action.
enum SettingsPhase: Equatable, Sendable {
    case idle
    /// The confirmation dialog is open for this action.
    case confirming(SettingsAction)
    /// The service call is running for this action.
    case performing(SettingsAction)
    /// The action succeeded; the screen is about to leave the paired root.
    case finished(SettingsAction)
}

/// On-demand log export. Export writes a bounded redacted snapshot to an
/// app-private staging file and then offers the system share sheet.
enum LogExportPhase: Equatable, Sendable {
    case idle
    case preparing
    case ready(URL)
    case failed

    var fileURL: URL? {
        if case .ready(let url) = self { return url }
        return nil
    }
}
