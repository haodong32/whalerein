// Settings presentation: versions, language note, staging/fixture indicators,
// and confirmed forget/revoke actions that clear local state.

import Foundation
import Observation

@MainActor
@Observable
final class SettingsModel {
    enum Confirmation: Equatable {
        case forget
        case revoke
    }

    /// One business state for the screen. The legacy property names below are
    /// read-only projections kept while the view migrates to `state`.
    private(set) var state = SettingsState()

    var isBusy: Bool { state.isBusy }
    var statusKey: L10nKey? { state.statusKey }
    var errorKey: L10nKey? { state.errorKey }
    let coreRevision: String
    let appVersion: String
    let isStaging: Bool
    let isFixtureBacked: Bool
    let dataPath: String

    private let service: any SessionService
    private let preferences: PreferencesStore
    var onDeviceReset: () -> Void

    init(
        service: any SessionService,
        preferences: PreferencesStore,
        dataDirectory: URL,
        isStaging: Bool,
        isFixtureBacked: Bool,
        onDeviceReset: @escaping () -> Void
    ) {
        self.service = service
        self.preferences = preferences
        self.dataPath = dataDirectory.path
        self.isStaging = isStaging
        self.isFixtureBacked = isFixtureBacked
        self.onDeviceReset = onDeviceReset
        coreRevision = service.coreRevision
        appVersion = service.appVersion
    }

    /// Opens the confirmation dialog for `action`.
    func requestConfirmation(_ action: SettingsAction) {
        state = SettingsReduce.reduce(state, .confirmationRequested(action))
    }

    func cancelConfirmation() {
        state = SettingsReduce.reduce(state, .confirmationCancelled)
    }

    /// Prepares the redacted log snapshot on demand. Nothing leaves the device
    /// until the user picks a share destination.
    func prepareLogExport() {
        state = SettingsReduce.reduce(state, .logExportStarted)
        if let url = try? AppLog.shared.exportSnapshot() {
            state = SettingsReduce.reduce(state, .logExportReady(url))
        } else {
            state = SettingsReduce.reduce(state, .logExportFailed)
        }
    }

    func perform(_ confirmation: Confirmation) async {
        await perform(confirmation == .forget ? .forgetDevice : .revokeDevice)
    }

    func perform(_ action: SettingsAction) async {
        guard !isBusy else { return }
        state = SettingsReduce.reduce(state, .confirmationRequested(action))
        state = SettingsReduce.reduce(state, .performStarted(action))
        do {
            switch action {
            case .forgetDevice:
                try await service.forgetDevice()
            case .revokeDevice:
                try await service.revoke()
            }
            preferences.update {
                $0.paired = false
                $0.lastHostId = nil
                $0.lastSessionId = nil
                $0.lastSessionTitle = nil
            }
            state = SettingsReduce.reduce(state, .performSucceeded(action))
            onDeviceReset()
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            state = SettingsReduce.reduce(state, .performFailed(code))
        }
    }
}
