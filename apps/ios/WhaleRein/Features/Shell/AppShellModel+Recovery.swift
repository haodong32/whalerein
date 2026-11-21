// Shell recovery behaviour, split out of the model file.

import Foundation

@MainActor
extension AppShellModel {
    // MARK: - Revocation and preference recovery

    /// Remote revocation blocks the conversation and offers re-pair/forget.
    /// Local preferences are kept until the user chooses, so "Forget" can still
    /// clear the local device identity.
    func enterRevoked() {
        conversation?.close()
        conversation = nil
        isRevoked = true
        isPrivacyShieldVisible = false
        dismissSheet()
        dismissSheet()
        dismissSheet()
        dismissSheet()
        selectedHostId = nil
        isOfflineCached = false
        resumeMarker = nil
        hostList.reset()
        sessionList.reset()
        rootState = .restoring
    }

    /// Leaves the blocked revoked state and returns to the invitation flow.
    func beginRepairAfterRevocation() {
        isRevoked = false
        isPaired = false
        preferences.update {
            $0.paired = false
            $0.lastHostId = nil
            $0.lastSessionId = nil
            $0.lastSessionTitle = nil
        }
        resetLocalNavigation()
    }

    /// Clears the local device identity after a remote revocation. The Core call
    /// is best effort because the Host may already have forgotten this device.
    func forgetAfterRevocation() async {
        try? await service.forgetDevice()
        beginRepairAfterRevocation()
    }

    /// Preferences and Core disagreed (for example after restoring a backup).
    /// Falling back to onboarding is the safe route: pairing again is always
    /// available, while a paired root with no Core identity would dead-end.
    func adoptOnboardingFallback() {
        conversation?.close()
        conversation = nil
        isPaired = false
        preferences.update {
            $0.paired = false
            $0.lastHostId = nil
            $0.lastSessionId = nil
            $0.lastSessionTitle = nil
        }
        resetLocalNavigation()
    }

    func resetLocalNavigation() {
        isRevoked = false
        dismissSheet()
        dismissSheet()
        dismissSheet()
        dismissSheet()
        selectedHostId = nil
        isOfflineCached = false
        resumeMarker = nil
        didBootstrap = false
        hostList.reset()
        sessionList.reset()
        pairing.reset()
        rootState = .restoring
    }

    func handleBootFailure(_ code: AppErrorCode) {
        switch code {
        case .revoked:
            enterRevoked()
        case .notPaired:
            adoptOnboardingFallback()
        case .protocolVersionIncompatible, .unsupportedCapability:
            rootState = .upgradeRequired(code)
        default:
            rootState = .failed(code)
        }
    }

    func handleSwitcherFailure(_ code: AppErrorCode) {
        switch code {
        case .revoked:
            enterRevoked()
        case .notPaired:
            adoptOnboardingFallback()
        case .protocolVersionIncompatible, .unsupportedCapability:
            conversation?.close()
            conversation = nil
            isOfflineCached = false
            rootState = .upgradeRequired(code)
        default:
            break
        }
    }

    func handleConversationFatal(_ code: AppErrorCode) {
        switch code {
        case .revoked:
            enterRevoked()
        case .notPaired:
            adoptOnboardingFallback()
        default:
            break
        }
    }

    // MARK: - Reset

    func deviceDidReset() {
        // The device state is gone, so the persisted redacted log and any
        // staging snapshot go with it. Core owns its own diagnostic journal.
        AppLog.shared.discardExportStaging()
        AppLog.shared.reset()
        conversation?.close()
        conversation = nil
        isPaired = false
        isRevoked = false
        prefersInvitationAfterDeviceReset = true
        dismissSheet()
        selectedHostId = nil
        isOfflineCached = false
        resumeMarker = nil
        didBootstrap = false
        hostList.reset()
        sessionList.reset()
        pairing.reset()
        dismissSheet()
        dismissSheet()
        dismissSheet()
        rootState = .restoring
    }
    }
