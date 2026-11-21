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

    /// Typed successful outcome for the selected instance only. The owning
    /// service is per instance, so another instance's pairing is untouched.
    enum Outcome: Equatable {
        case forgotten
        case revoked
    }

    /// One business state for the screen. The legacy property names below are
    /// read-only projections kept while the view migrates to `state`.
    private(set) var state = SettingsState()

    var isBusy: Bool { state.isBusy }
    var statusKey: L10nKey? { state.statusKey }
    var errorKey: L10nKey? { state.errorKey }
    var lastOutcome: Outcome? {
        switch state.phase {
        case .finished(.forgetDevice): .forgotten
        case .finished(.revokeDevice): .revoked
        default: nil
        }
    }

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

    func perform(_ confirmation: Confirmation) async {
        let action: SettingsAction = confirmation == .forget ? .forgetDevice : .revokeDevice
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
