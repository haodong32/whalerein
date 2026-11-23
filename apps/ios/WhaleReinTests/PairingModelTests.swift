// Pairing state machine: validation, fingerprint confirmation, decline,
// cancellation, expiry, lost result resume, and localized errors.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Pairing state machine")
@MainActor
struct PairingModelTests {
    @Test("a valid invitation advances to fingerprint confirmation")
    func validInvitation() async throws {
        let service = try await makePairedFixture()
        let model = PairingModel(service: service)
        model.updateInvitation(FixtureCatalog.invitationURL(hostId: "studio-mac").absoluteString)
        await model.submit()

        guard case .awaitingConfirmation(let prompt) = model.phase else {
            Issue.record("expected awaiting confirmation, got \(model.phase)")
            return
        }
        #expect(prompt.hostId == "studio-mac")
        #expect(prompt.fingerprint == FixtureCatalog.fingerprint)
        #expect(model.errorKey == nil)
    }

    @Test("confirming submits the reviewed fingerprint and reaches the paired phase")
    func confirmPairing() async throws {
        let service = try await makePairedFixture()
        let model = PairingModel(service: service)
        model.updateInvitation(FixtureCatalog.invitationURL(hostId: "studio-mac").absoluteString)
        await model.submit()
        await model.confirm()

        #expect(model.phase == .paired(hostId: "studio-mac"))
        let hosts = try await service.hosts()
        #expect(hosts.contains { $0.hostId == "studio-mac" })
    }

    @Test("declining leaves the device unpaired")
    func declinePairing() async throws {
        let service = try await makePairedFixture()
        let model = PairingModel(service: service)
        model.updateInvitation(FixtureCatalog.invitationURL(hostId: "studio-mac").absoluteString)
        await model.submit()
        await model.decline()

        #expect(model.phase == .declined)
        await #expect(throws: SessionServiceError.self) {
            _ = try await service.hosts()
        }
    }

    @Test("invalid text is rejected locally with a localized error")
    func invalidText() async {
        let service = StubSessionService()
        let model = PairingModel(service: service)
        model.updateInvitation("not a link")
        await model.submit()

        #expect(model.phase == .editing)
        #expect(model.errorKey == .invitationMalformed)
        #expect(model.parseError == .malformed)
        #expect(service.pairingStartCount == 0)
    }

    @Test("an expired invitation shows the expiry error before calling the service")
    func expiredInvitation() async {
        let service = StubSessionService()
        let model = PairingModel(service: service)
        let link = InvitationParser.makeLink(
            accessURL: URL(string: "https://host.example")!,
            token: "abcdefgh12345",
            hostId: "studio-mac",
            expiresAt: Date().addingTimeInterval(-10)
        )
        model.updateInvitation(link)
        await model.submit()

        #expect(model.errorKey == .invitationExpired)
        #expect(service.pairingStartCount == 0)
    }

    @Test("editing clears a previous error")
    func editingClearsError() async {
        let service = StubSessionService()
        let model = PairingModel(service: service)
        model.updateInvitation("garbage")
        await model.submit()
        #expect(model.errorKey != nil)
        model.updateInvitation("whalerein://pair")
        #expect(model.errorKey == nil)
    }

    @Test("a lost decision result can resume the original invitation")
    func resumesLostResult() async {
        let service = StubSessionService()
        let prompt = fixturePrompt()
        var decideCalls = 0
        service.pairingStartHandler = { _ in prompt }
        service.pairingDecideHandler = { _, _ in
            decideCalls += 1
            if decideCalls == 1 {
                throw SessionServiceError(.timeout)
            }
        }
        let model = PairingModel(service: service)
        model.updateInvitation(FixtureCatalog.invitationURL(hostId: "studio-mac").absoluteString)
        await model.submit()
        await model.confirm()
        #expect(model.phase == .failed(.timeout))
        #expect(model.canResume)

        await model.resume()
        guard case .awaitingConfirmation = model.phase else {
            Issue.record("expected resumed prompt, got \(model.phase)")
            return
        }
        #expect(service.pairingStartCount == 2)

        await model.confirm()
        #expect(model.phase == .paired(hostId: "studio-mac"))
    }

    @Test("cancelling the prompt returns to editing and notifies the service")
    func cancelsPrompt() async {
        let service = StubSessionService()
        service.pairingStartHandler = { _ in fixturePrompt() }
        let model = PairingModel(service: service)
        model.updateInvitation(FixtureCatalog.invitationURL(hostId: "studio-mac").absoluteString)
        await model.submit()
        await model.cancel()

        #expect(model.phase == .editing)
        #expect(service.pairingCancelCount == 1)
    }

    @Test("a fingerprint mismatch keeps the device unpaired and localizes the failure")
    func fingerprintMismatch() async {
        let service = StubSessionService()
        service.pairingStartHandler = { _ in fixturePrompt() }
        service.pairingDecideHandler = { approved, _ in
            if approved {
                throw SessionServiceError(.fingerprintMismatch)
            }
        }
        let model = PairingModel(service: service)
        model.updateInvitation(FixtureCatalog.invitationURL(hostId: "studio-mac").absoluteString)
        await model.submit()
        await model.confirm()

        #expect(model.phase == .failed(.fingerprintMismatch))
        #expect(model.errorKey != nil)
        #expect(service.pairingDecideArguments.last?.fingerprint == FixtureCatalog.fingerprint)
    }

    @Test("clipboard text fills the invitation field and submits the same link")
    func clipboardPasteSubmits() async {
        let service = StubSessionService()
        service.pairingStartHandler = { _ in fixturePrompt() }
        let model = PairingModel(service: service)
        let link = FixtureCatalog.invitationURL(hostId: "studio-mac")

        model.paste("  \(link.absoluteString)  ")
        #expect(model.invitationText.contains(link.absoluteString))
        await model.submit()
        #expect(model.prompt?.hostId == "studio-mac")
        #expect(service.pairingStartCount == 1)
    }

    @Test("pasting empty or whitespace-only clipboard text reports nothing")
    func clipboardPasteEmpty() {
        let model = PairingModel(service: StubSessionService())
        model.paste("   \n ")
        #expect(model.queuedLinkKey == .invitationPastedNothing)
    }

    @Test("open URL handling fills the field and starts pairing")
    func openURLStartsPairing() async {
        let service = StubSessionService()
        service.pairingStartHandler = { _ in fixturePrompt() }
        let model = PairingModel(service: service)
        await model.handleOpenURL(FixtureCatalog.invitationURL(hostId: "studio-mac"))

        #expect(!model.invitationText.isEmpty)
        guard case .awaitingConfirmation = model.phase else {
            Issue.record("expected awaiting confirmation")
            return
        }
    }
}
