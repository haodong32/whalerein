// Pairing presentation state machine. Invitation validation happens locally
// first so errors are localized without touching the service; the service owns
// the confirmed fingerprint and the asynchronous decision.

import Foundation
import Observation

@MainActor
@Observable
final class PairingModel {
    enum Phase: Equatable {
        case editing
        case validating
        case awaitingConfirmation(PairingPrompt)
        case confirming(PairingPrompt)
        case paired(hostId: String)
        case declined
        case failed(AppErrorCode)
    }

    var invitationText: String = ""
    private(set) var phase: Phase = .editing
    private(set) var errorKey: L10nKey?
    private(set) var parseError: InvitationParseError?
    private(set) var isBusy = false
    /// Set when a deep link arrives while another pairing step is in flight.
    /// The link is queued and disclosed instead of being silently dropped.
    private(set) var queuedLinkKey: L10nKey?

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
        errorKey = nil
        parseError = nil
        if case .failed = phase {
            phase = .editing
        }
    }

    func paste(_ text: String) {
        guard !text.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            errorKey = .invitationPastedNothing
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
            queuedLinkKey = .pairingLinkQueued
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
            parseError = error
            errorKey = L10nKey.invitationErrorKey(for: error)
            phase = .editing
            return
        case .success:
            break
        }

        isBusy = true
        errorKey = nil
        parseError = nil
        phase = .validating
        lastSubmittedInvitation = raw
        do {
            let prompt = try await service.pairingStart(invitation: raw)
            phase = .awaitingConfirmation(prompt)
        } catch {
            fail(error)
        }
        isBusy = false
        await drainPendingLinkIfPossible()
    }

    func resume() async {
        guard lastSubmittedInvitation != nil else { return }
        await submit()
    }

    func confirm() async {
        guard case .awaitingConfirmation(let prompt) = phase, !isBusy else { return }
        isBusy = true
        phase = .confirming(prompt)
        do {
            try await service.pairingDecide(approved: true, expectedFingerprint: prompt.fingerprint)
            phase = .paired(hostId: prompt.hostId)
            pendingURL = nil
            queuedLinkKey = nil
            onPaired?(prompt.hostId)
        } catch {
            fail(error)
        }
        isBusy = false
        if case .failed = phase {
            await drainPendingLinkIfPossible()
        }
    }

    func decline() async {
        guard prompt != nil, !isBusy else { return }
        isBusy = true
        do {
            try await service.pairingDecide(approved: false, expectedFingerprint: nil)
            phase = .declined
            errorKey = nil
        } catch {
            fail(error)
        }
        isBusy = false
        await drainPendingLinkIfPossible()
    }

    func cancel() async {
        await service.pairingCancel()
        phase = .editing
        isBusy = false
        errorKey = nil
        await drainPendingLinkIfPossible()
    }

    func reset() {
        phase = .editing
        invitationText = ""
        errorKey = nil
        parseError = nil
        isBusy = false
        lastSubmittedInvitation = nil
        pendingURL = nil
        queuedLinkKey = nil
    }

    private func drainPendingLinkIfPossible() async {
        guard let url = pendingURL, !isBusy, prompt == nil else { return }
        pendingURL = nil
        queuedLinkKey = nil
        updateInvitation(url.absoluteString)
        await submit()
    }

    private func fail(_ error: Error) {
        let code = (error as? SessionServiceError)?.code ?? .internalError
        phase = .failed(code)
        errorKey = L10nKey.errorKey(for: code)
    }
}
