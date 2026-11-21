// Business state for the Settings screen.
//
// The immutable version/identity facts live in `SettingsInfo`; everything the
// user can change or wait for lives in `SettingsState`. A destructive action is
// one phase value, so "busy" and "which action is running" cannot disagree.

import Foundation


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
