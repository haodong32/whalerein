// Localized selection for errors and status values. This is the single place
// presentation models ask for a key, which keeps status/error text out of views
// and makes both languages testable without UI automation.

import Foundation

extension L10nKey {
    static func errorKey(for code: AppErrorCode) -> L10nKey {
        switch code {
        case .invalidInvitation, .pairingDeclined, .fingerprintMismatch:
            return .pairingFailed
        case .invitationExpired:
            return .pairingExpired
        case .notPaired:
            return .errorNotPaired
        case .revoked:
            return .errorRevoked
        case .hostOffline:
            return .errorHostOffline
        case .unavailable:
            return .errorUnavailable
        case .conflict, .staleApproval:
            return .errorConflict
        case .timeout:
            return .errorTimeout
        case .cancelled:
            return .errorCancelled
        case .security:
            return .errorSecurity
        case .internalError:
            return .errorInternal
        case .unsupportedDelivery:
            return .conversationDeliveryUnsupported
        case .draftUnavailable:
            return .errorDraftUnavailable
        case .integrationUnavailable:
            return .errorIntegrationUnavailable
        case .protocolVersionIncompatible:
            return .errorProtocolIncompatible
        case .unsupportedCapability:
            return .errorUnsupportedCapability
        case .keychainUnavailable:
            return .errorKeychainUnavailable
        case .configuration:
            return .errorConfiguration
        case .staleHistory:
            return .errorStaleHistory
        case .invalidInput:
            return .errorInvalidInput
        }
    }

    static func invitationErrorKey(for error: InvitationParseError) -> L10nKey {
        switch error {
        case .empty:
            return .invitationEmpty
        case .malformed:
            return .invitationMalformed
        case .unsupportedScheme:
            return .invitationUnsupportedScheme
        case .missingToken:
            return .invitationMissingToken
        case .invalidAccessURL:
            return .invitationInvalidAccessURL
        case .expired:
            return .invitationExpired
        }
    }

    static func connectionKey(for status: ConnectionStatus) -> L10nKey {
        switch status {
        case .offline:
            return .conversationConnectionOffline
        case .connecting:
            return .conversationConnectionConnecting
        case .online:
            return .conversationConnectionOnline
        case .degraded:
            return .conversationConnectionDegraded
        case .reconnecting:
            return .conversationConnectionReconnecting
        }
    }

    static func commandKey(for status: CommandStatus) -> L10nKey {
        switch status {
        case .accepted:
            return .conversationSendAccepted
        case .uncertain:
            return .conversationSendUncertain
        case .confirmed:
            return .conversationSendAccepted
        case .rejected:
            return .conversationSendRejected
        case .cancelled:
            return .errorCancelled
        }
    }

    static func approvalStatusKey(for status: ApprovalStatusRefresh) -> L10nKey {
        switch status {
        case .refreshing, .current:
            return .approvalStatusCurrent
        case .changed:
            return .approvalStatusChanged
        case .resolved:
            return .approvalStatusResolved
        case .unavailable:
            return .approvalStatusUnavailable
        case .outOfScope:
            return .approvalStatusOutOfScope
        }
    }

    static func waitOutcomeKey(for outcome: WaitOutcome) -> L10nKey {
        switch outcome {
        case .idle: return .waitIdle
        case .inputRequired: return .waitInputRequired
        case .timeout: return .waitTimeout
        case .cancelled: return .waitCancelled
        case .failed: return .waitFailed
        }
    }

    static func queueMutationKey(for status: CommandStatus) -> L10nKey {
        switch status {
        case .accepted, .confirmed: return .queueMutationSaved
        case .uncertain: return .queueMutationUncertain
        case .rejected, .cancelled: return .queueMutationFailed
        }
    }

    static func approvalStateKey(for state: ApprovalState) -> L10nKey {
        switch state {
        case .pending:
            return .approvalPending
        case .allowed:
            return .approvalAllowed
        case .rejected:
            return .approvalRejected
        case .expired:
            return .approvalExpired
        case .changed:
            return .approvalChanged
        case .unavailable:
            return .approvalUnavailable
        case .resolvedElsewhere:
            return .approvalResolvedElsewhere
        }
    }

    /// English distinguishes one from many. Simplified Chinese uses the same
    /// wording in both catalogs; selecting by count keeps the caller explicit
    /// and testable instead of embedding grammar in a view.
    static func messageCountKey(for count: Int) -> L10nKey {
        count == 1 ? .conversationMessageCount : .conversationMessageCountPlural
    }

    static func unreadCountKey(for count: Int) -> L10nKey {
        count == 1 ? .conversationUnreadCount : .conversationUnreadCountPlural
    }

    static func diagnosticStageKey(for status: DiagnosticStageStatus) -> L10nKey {
        switch status {
        case .ok:
            return .diagnosticsStageOk
        case .failed:
            return .diagnosticsStageFailed
        case .idle:
            return .diagnosticsStageIdle
        }
    }

    static func deliveryKey(for delivery: DeliveryChoice) -> L10nKey {
        switch delivery {
        case .send:
            return .conversationDeliverySend
        case .steer:
            return .conversationDeliverySteer
        case .queue:
            return .conversationDeliveryQueue
        }
    }

    /// Convenience for presentation models rendering a localized error.
    static func localizedErrorText(_ error: Error) -> String {
        if let serviceError = error as? SessionServiceError {
            return L10nKey.errorKey(for: serviceError.code).string
        }
        return L10nKey.errorInternal.string
    }
}
