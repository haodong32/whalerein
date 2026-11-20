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

    private(set) var isBusy = false
    private(set) var statusKey: L10nKey?
    private(set) var errorKey: L10nKey?
    private(set) var lastOutcome: Outcome?

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
        guard !isBusy else { return }
        isBusy = true
        statusKey = nil
        errorKey = nil
        lastOutcome = nil
        do {
            switch confirmation {
            case .forget:
                try await service.forgetDevice()
                statusKey = .settingsForgotten
                lastOutcome = .forgotten
            case .revoke:
                try await service.revoke()
                statusKey = .settingsRevoked
                lastOutcome = .revoked
            }
            preferences.update {
                $0.paired = false
                $0.lastHostId = nil
                $0.lastSessionId = nil
                $0.lastSessionTitle = nil
            }
            onDeviceReset()
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            errorKey = L10nKey.errorKey(for: code)
        }
        isBusy = false
    }
}
