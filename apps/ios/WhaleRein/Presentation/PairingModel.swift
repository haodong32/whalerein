// Pairing presentation state machine. Invitation validation happens locally
// first so errors are localized without touching the service; the service owns
// the confirmed fingerprint and the asynchronous decision.

import Foundation
import Observation

@MainActor
@Observable
final class PairingModel {
    /// Kept as the model's public phase name for existing views and tests.
    typealias Phase = PairingPhase

    var invitationText: String = ""
    /// One business state; the legacy names below are read-only projections.
    private(set) var state = PairingState()

    var phase: Phase { state.phase }
    var isBusy: Bool { state.isBusy }
    var parseError: InvitationParseError? { state.parseError }
    var errorKey: L10nKey? {
        if let parseError = state.parseError {
            return L10nKey.invitationErrorKey(for: parseError)
        }
        if let code = state.errorCode, case .failed = state.phase {
            return L10nKey.errorKey(for: code)
        }
        return nil
    }
    /// Set when a deep link arrives while another pairing step is in flight.
    /// The link is queued and disclosed instead of being silently dropped.
    var queuedLinkKey: L10nKey? {
        switch state.hint {
        case .linkQueued: .pairingLinkQueued
        case .pastedNothing: .invitationPastedNothing
        case nil: nil
        }
    }

    private func apply(_ event: PairingEvent) {
        state = PairingReduce.reduce(state, event)
    }

    private let service: any SessionService
    var onPaired: ((String) -> Void)?
    private var lastSubmittedInvitation: String?
    private var pendingURL: URL?

    init(service: any SessionService, onPaired: ((String) -> Void)? = nil) {
        self.service = service
        self.onPaired = onPaired
    }

    var prompt: PairingPrompt? {
        switch phase {
        case .awaitingConfirmation(let prompt), .confirming(let prompt):
            return prompt
        default:
            return nil
        }
    }

    var canResume: Bool {
        if case .failed = phase { return lastSubmittedInvitation != nil }
        return false
    }

    func updateInvitation(_ text: String) {
        invitationText = text
        apply(.inputChanged)
    }

    func paste(_ text: String) {
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            apply(.pastedNothing)
            return
        }
        updateInvitation(text)
    }

    func handleOpenURL(_ url: URL) async {
        // A link that arrives while a validation, confirmation or decision is
        // in flight is queued with visible feedback. It is processed after the
        // current step finishes instead of being dropped.
        if isBusy || prompt != nil {
            pendingURL = url
            apply(.linkQueued)
            return
        }
        updateInvitation(url.absoluteString)
        await submit()
    }

    func submit() async {
        guard !isBusy else { return }
        let raw = invitationText.trimmingCharacters(in: .whitespacesAndNewlines)
        switch InvitationParser.parse(raw) {
        case .failure(let error):
            apply(.parseFailed(error))
            return
        case .success:
            break
        }

        apply(.submitStarted)
        lastSubmittedInvitation = raw
        do {
            let prompt = try await service.pairingStart(invitation: raw)
            apply(.submitSucceeded(prompt))
        } catch {
            fail(error)
        }
        await drainPendingLinkIfPossible()
    }

    func resume() async {
        guard lastSubmittedInvitation != nil else { return }
        await submit()
    }

    func confirm() async {
        guard case .awaitingConfirmation(let prompt) = phase, !isBusy else { return }
        apply(.confirmStarted)
        do {
            try await service.pairingDecide(approved: true, expectedFingerprint: prompt.fingerprint)
            apply(.approved(hostId: prompt.hostId))
            pendingURL = nil
            onPaired?(prompt.hostId)
        } catch {
            fail(error)
        }
        if case .failed = phase {
            await drainPendingLinkIfPossible()
        }
    }

    func decline() async {
        guard prompt != nil, !isBusy else { return }
        do {
            try await service.pairingDecide(approved: false, expectedFingerprint: nil)
            apply(.declined)
        } catch {
            fail(error)
        }
        await drainPendingLinkIfPossible()
    }

    func cancel() async {
        await service.pairingCancel()
        apply(.cancelled)
        await drainPendingLinkIfPossible()
    }

    func reset() {
        apply(.reset)
        invitationText = ""
        lastSubmittedInvitation = nil
        pendingURL = nil
    }

    private func drainPendingLinkIfPossible() async {
        guard let url = pendingURL, !isBusy, prompt == nil else { return }
        pendingURL = nil
        apply(.linkDrained)
        updateInvitation(url.absoluteString)
        await submit()
    }

    private func fail(_ error: Error) {
        let code = (error as? SessionServiceError)?.code ?? .internalError
        apply(.failed(code))
    }
}
