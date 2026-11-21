// Business state for the Settings screen.
//
// The immutable version/identity facts live in `SettingsInfo`; everything the
// user can change or wait for lives in `SettingsState`. A destructive action is
// one phase value, so "busy" and "which action is running" cannot disagree.

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

struct SettingsState: Equatable, Sendable {
    var phase: SettingsPhase = .idle
    /// Localized status line after a successful action.
    var statusKey: L10nKey?
    var errorCode: AppErrorCode?
    var logExport: LogExportPhase = .idle

    var isBusy: Bool {
        if case .performing = phase { return true }
        return false
    }

    /// The action the confirmation dialog is currently asking about.
    var pendingConfirmation: SettingsAction? {
        if case .confirming(let action) = phase { return action }
        return nil
    }

    var errorKey: L10nKey? {
        errorCode.map { L10nKey.errorKey(for: $0) }
    }
}
