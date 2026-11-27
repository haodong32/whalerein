// Protected approval review: Host-projected action/target/impact are shown as
// verified facts, missing context is stated as a limitation without blocking
// Allow, changed facts require a fresh review, and terminal cards expose no
// details.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Approval review details")
@MainActor
struct ApprovalReviewDetailTests {
    private func makeModel(
        _ service: StubSessionService,
        _ approval: Approval
    ) async -> ConversationModel {
        let model = await makeStartedConversation(service: service)
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 1,
                event: .reset(messages: [], approvals: [approval])
            )
        )
        return model
    }

    private func details(
        target: String = "Sources/App.swift",
        impact: String = "Replaces the selected file content.",
        availability: ApprovalReviewAvailability = .complete
    ) -> ApprovalReviewDetails {
        ApprovalReviewDetails(
            action: "Change one file",
            target: target,
            impact: impact,
            availability: availability
        )
    }

    @Test("verified action, target and impact render exactly as supplied")
    func rendersVerifiedFacts() async {
        let service = StubSessionService()
        let approval = testApproval(review: details())
        let model = await makeModel(service, approval)

        let review = model.approvalReview(approval)
        #expect(review?.action == "Change one file")
        #expect(review?.target == "Sources/App.swift")
        #expect(review?.impact == "Replaces the selected file content.")
        #expect(review?.isComplete == true)
        #expect(model.canAllowApproval(approval))
        #expect(model.approvalReviewLimitationKey(approval) == nil)
    }

    @Test("missing essential context stays visible with a limitation and remains allow-able")
    func insufficientContextStaysAllowable() async {
        let service = StubSessionService()
        let approval = testApproval(
            review: ApprovalReviewDetails(
                action: "Change one file",
                target: nil,
                impact: "Replaces the selected file content.",
                availability: .incomplete
            )
        )
        service.resolveHandler = { _, _, _, _, _, _ in
            var updated = approval
            updated.state = .allowed
            return updated
        }
        let model = await makeModel(service, approval)

        #expect(model.isApprovalActionable(approval), "rejection and local review stay available")
        #expect(model.canAllowApproval(approval))
        #expect(model.approvalReview(approval)?.action == "Change one file")
        #expect(model.approvalReview(approval)?.target == nil)
        #expect(model.approvalReview(approval)?.impact == "Replaces the selected file content.")
        #expect(model.approvalReviewLimitationKey(approval) == .approvalReviewIncomplete)

        await model.resolveApproval(approval, allow: true)
        #expect(service.resolveCalls.count == 1)
        #expect(service.resolveCalls.first?.version == approval.version)
        #expect(service.resolveCalls.first?.digest == approval.digestHex)
        #expect(model.approvals.first?.state == .allowed)

        // Rejection stays available for the same incomplete projection.
        let rejectService = StubSessionService()
        rejectService.resolveHandler = { _, _, _, _, _, _ in
            var updated = approval
            updated.state = .rejected
            return updated
        }
        let rejectModel = await makeModel(rejectService, approval)
        await rejectModel.resolveApproval(approval, allow: false)
        #expect(rejectService.resolveCalls.count == 1)
        #expect(rejectModel.approvals.first?.state == .rejected)
    }

    @Test("an absent projection states the limitation instead of inventing facts")
    func absentProjectionStaysAllowable() async {
        let service = StubSessionService()
        let approval = testApproval(review: nil)
        service.resolveHandler = { _, _, _, _, _, _ in
            var updated = approval
            updated.state = .allowed
            return updated
        }
        let model = await makeModel(service, approval)

        #expect(model.approvalReview(approval) == nil)
        #expect(model.canAllowApproval(approval))
        #expect(model.approvalReviewLimitationKey(approval) == .approvalReviewMissing)

        await model.resolveApproval(approval, allow: true)
        #expect(service.resolveCalls.count == 1)
        #expect(service.resolveCalls.first?.version == approval.version)
        #expect(service.resolveCalls.first?.digest == approval.digestHex)
        #expect(model.approvals.first?.state == .allowed)
    }

    @Test("an unavailable projection hides details but still allows this exact variant")
    func unavailableHidesDetailsButAllows() async {
        let service = StubSessionService()
        let approval = testApproval(
            review: details(availability: .unavailable)
        )
        service.resolveHandler = { _, _, _, _, _, _ in
            var updated = approval
            updated.state = .allowed
            return updated
        }
        let model = await makeModel(service, approval)

        #expect(model.approvalReview(approval) == nil)
        #expect(model.approvalReviewLimitationKey(approval) == .approvalDetailsHidden)
        #expect(model.canAllowApproval(approval))

        await model.resolveApproval(approval, allow: true)
        #expect(service.resolveCalls.count == 1)
        #expect(service.resolveCalls.first?.version == approval.version)
        #expect(service.resolveCalls.first?.digest == approval.digestHex)
        #expect(model.approvals.first?.state == .allowed)
    }

    @Test("a terminal or revoked card hides review details")
    func terminalCardHidesDetails() async {
        let service = StubSessionService()
        let resolved = testApproval(state: .allowed, review: details())
        let model = await makeModel(service, resolved)

        #expect(model.approvalReview(resolved) == nil)
        #expect(model.approvalReviewLimitationKey(resolved) == nil)
        #expect(!model.canAllowApproval(resolved))

        // A session the Host no longer reports marks pending cards unavailable,
        // which hides their protected details as well.
        let pending = testApproval(review: details())
        let second = await makeModel(service, pending)
        #expect(second.approvalReview(pending) != nil)
        service.approvalsHandler = { _, _ in throw SessionServiceError(.unavailable) }
        await second.refreshApprovals()
        #expect(second.approvals.first?.state == .unavailable)
        if let card = second.approvals.first {
            #expect(second.approvalReview(card) == nil)
            #expect(!second.canAllowApproval(card))
        }
    }

    @Test("changed review facts require a fresh review of the current card")
    func changedFactsRequireFreshReview() async {
        let service = StubSessionService()
        let original = testApproval()
        let model = await makeModel(service, original)

        let changed = Approval(
            approvalId: original.approvalId,
            sessionId: original.sessionId,
            toolName: original.toolName,
            callId: original.callId,
            reason: original.reason,
            version: original.version + 1,
            expiresAtUnixMs: original.expiresAtUnixMs,
            digestHex: "digest-2",
            state: .pending,
            review: details(
                target: "Sources/Other.swift",
                impact: "Replaces a different file."
            )
        )
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 2,
                event: .approvalChanged(changed)
            )
        )

        // Deciding from the stale card must not reach the Host.
        await model.resolveApproval(original, allow: true)
        #expect(service.resolveCalls.isEmpty)
        #expect(model.approvalErrorKey == .approvalStale)

        // The current card shows the new facts and can be reviewed again.
        let current = model.approvals[0]
        #expect(current.version == 4)
        #expect(model.approvalReview(current)?.target == "Sources/Other.swift")
        #expect(current.review != original.review)

        service.resolveHandler = { _, _, approvalId, version, digest, allow in
            Approval(
                approvalId: approvalId,
                sessionId: changed.sessionId,
                toolName: changed.toolName,
                callId: changed.callId,
                reason: changed.reason,
                version: version,
                expiresAtUnixMs: changed.expiresAtUnixMs,
                digestHex: digest,
                state: allow ? .allowed : .rejected,
                review: changed.review
            )
        }
        await model.resolveApproval(current, allow: true)
        #expect(service.resolveCalls.count == 1)
        #expect(service.resolveCalls.first?.version == 4)
        #expect(service.resolveCalls.first?.digest == "digest-2")
        #expect(model.approvalErrorKey == nil)
    }

    @Test("every review limitation has localized wording in both languages")
    func localizedLimitations() {
        for key in [
            L10nKey.approvalReviewMissing,
            L10nKey.approvalReviewIncomplete,
            L10nKey.approvalDetailsHidden,
            L10nKey.approvalAction,
            L10nKey.approvalTarget,
            L10nKey.approvalImpact,
        ] {
            #expect(!key.string.isEmpty)
            #expect(key.string != key.rawValue)
        }
    }
}