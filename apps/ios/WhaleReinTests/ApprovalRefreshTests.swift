// Approval refresh semantics: a Core approval listing returns only pending
// requests. The client must keep an already-decided card, never let a stale
// pending list overwrite a decision, and surface an unavailable session instead
// of a permanently actionable card.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Approval refresh")
@MainActor
struct ApprovalRefreshTests {
    private func envelope(_ event: MessageEvent, sequence: Int) -> SessionEventEnvelope {
        SessionEventEnvelope(hostId: "studio-mac", sessionId: "s-001", sequence: sequence, event: event)
    }

    private func modelWith(
        approval: Approval,
        service: StubSessionService
    ) async -> ConversationModel {
        let model = await makeStartedConversation(service: service)
        model.handle(envelope(.reset(messages: [], approvals: [approval]), sequence: 1))
        return model
    }

    @Test("a pending-only approval event cannot remove an already-decided card")
    func decidedCardSurvivesEvent() async {
        let service = StubSessionService()
        let approval = testApproval()
        service.resolveHandler = { _, _, _, _, _, allow in
            var updated = approval
            updated.state = allow ? .allowed : .rejected
            return updated
        }
        let model = await modelWith(approval: approval, service: service)

        await model.resolveApproval(approval, allow: true)
        #expect(model.approvals.first?.state == .allowed)

        // Core publishes only pending approvals after the decision.
        model.handle(envelope(.approvals([]), sequence: 2))

        #expect(model.approvals.count == 1, "The decided card must stay visible")
        #expect(model.approvals[0].state == .allowed)
        #expect(!model.isApprovalActionable(model.approvals[0]))
    }

    @Test("a refresh with a pending-only list keeps the decided card")
    func decidedCardSurvivesRefresh() async {
        let service = StubSessionService()
        let approval = testApproval()
        service.resolveHandler = { _, _, _, _, _, allow in
            var updated = approval
            updated.state = allow ? .allowed : .rejected
            return updated
        }
        let model = await modelWith(approval: approval, service: service)
        await model.resolveApproval(approval, allow: false)

        service.approvalsHandler = { _, _ in [] }
        await model.refreshApprovals()

        #expect(model.approvals.count == 1)
        #expect(model.approvals[0].state == .rejected)
    }

    @Test("a stale pending refresh cannot downgrade a just-confirmed decision")
    func staleRefreshDoesNotDowngrade() async {
        let service = StubSessionService()
        let approval = testApproval()
        service.resolveHandler = { _, _, _, _, _, allow in
            var updated = approval
            updated.state = allow ? .allowed : .rejected
            return updated
        }
        let model = await modelWith(approval: approval, service: service)
        await model.resolveApproval(approval, allow: true)
        let resolveCount = service.resolveCalls.count

        // The refresh was already in flight and returns the older pending
        // version after the decision.
        service.approvalsHandler = { _, _ in [approval] }
        await model.refreshApprovals()

        #expect(model.approvals.first?.state == .allowed)
        #expect(service.resolveCalls.count == resolveCount)
        #expect(!model.isApprovalActionable(model.approvals[0]))
    }

    @Test("the Core unavailable state is distinct from changed and is not actionable")
    func unavailableStateMapping() {
        #expect(LiveSessionService.map(approvalState: "unavailable") == .unavailable)
        #expect(LiveSessionService.map(approvalState: "session_unavailable") == .unavailable)
        #expect(LiveSessionService.map(approvalState: "resolved_elsewhere") == .resolvedElsewhere)
        #expect(LiveSessionService.map(approvalState: "pending") == .pending)
        #expect(LiveSessionService.map(approvalState: "mystery") == .changed)
        #expect(L10nKey.approvalStateKey(for: .unavailable) == .approvalUnavailable)
        #expect(L10nKey.approvalStateKey(for: .unavailable) != .approvalChanged)
    }

    @Test("a deleted session marks pending cards unavailable instead of leaving them clickable")
    func unavailableSessionMarksCards() async {
        let service = StubSessionService()
        let approval = testApproval()
        let model = await modelWith(approval: approval, service: service)
        #expect(model.isApprovalActionable(approval))

        service.approvalsHandler = { _, _ in throw SessionServiceError(.unavailable) }
        await model.refreshApprovals()

        #expect(model.approvals.count == 1)
        #expect(model.approvals[0].state == .unavailable)
        #expect(!model.isApprovalActionable(model.approvals[0]))
        #expect(L10nKey.approvalStateKey(for: model.approvals[0].state) == .approvalUnavailable)
    }

    @Test("a transient unavailable card becomes actionable when the Host reports it pending again")
    func unavailableRecoversWhenHostReportsPending() async {
        let service = StubSessionService()
        let approval = testApproval()
        let model = await modelWith(approval: approval, service: service)

        service.approvalsHandler = { _, _ in throw SessionServiceError(.unavailable) }
        await model.refreshApprovals()
        #expect(model.approvals.first?.state == .unavailable)
        #expect(!model.isApprovalActionable(model.approvals[0]))

        // The Host is reachable again and still lists the same pending guard.
        service.approvalsHandler = { _, _ in [approval] }
        await model.refreshApprovals()
        #expect(model.approvals.first?.state == .pending)
        #expect(model.isApprovalActionable(model.approvals[0]))
    }

    @Test("an unresolved pending request that disappears from the Host list becomes terminal")
    func missingPendingBecomesTerminal() async {
        let service = StubSessionService()
        let approval = testApproval()
        let model = await modelWith(approval: approval, service: service)

        service.approvalsHandler = { _, _ in [] }
        model.handle(envelope(.approvals([]), sequence: 2))

        #expect(model.approvals.count == 1)
        #expect(model.approvals[0].state == .resolvedElsewhere)
        #expect(!model.isApprovalActionable(model.approvals[0]))
    }

    @Test("a changed variant delivered by the Host remains a fresh pending decision")
    func changedVariantStaysReviewable() async {
        let service = StubSessionService()
        let approval = testApproval()
        let model = await modelWith(approval: approval, service: service)

        let changed = Approval(
            approvalId: approval.approvalId,
            sessionId: approval.sessionId,
            toolName: approval.toolName,
            callId: approval.callId,
            reason: approval.reason,
            version: approval.version + 1,
            expiresAtUnixMs: approval.expiresAtUnixMs,
            digestHex: "new-digest",
            state: .pending
        )
        model.handle(envelope(.approvalChanged(changed), sequence: 2))

        #expect(model.approvals.count == 1)
        #expect(model.approvals[0].version == approval.version + 1)
        #expect(model.approvals[0].state == .pending)
        #expect(model.isApprovalActionable(model.approvals[0]))
    }
}
