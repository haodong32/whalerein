// Approval review binding, stale/expired handling, and in-place card updates.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Approval review")
@MainActor
struct ApprovalReviewTests {
    private func approval(
        state: ApprovalState = .pending,
        version: Int = 3,
        expiresAt: Date = Date().addingTimeInterval(900)
    ) -> Approval {
        Approval(
            approvalId: "approval-1",
            sessionId: "s-001",
            toolName: "bash",
            callId: "call-42",
            reason: "Run verification",
            version: version,
            expiresAtUnixMs: Int64(expiresAt.timeIntervalSince1970 * 1000),
            digestHex: "abc123",
            state: state
        )
    }

    private func modelWithApproval(
        service: StubSessionService,
        approval: Approval
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

    @Test("a decision submits the exact reviewed version and digest")
    func submitsReviewedVariant() async {
        let service = StubSessionService()
        let current = approval()
        service.resolveHandler = { _, _, id, version, digest, allow in
            var updated = current
            updated.state = allow ? .allowed : .rejected
            return updated
        }
        let model = await modelWithApproval(service: service, approval: current)

        await model.resolveApproval(current, allow: true)

        #expect(service.resolveCalls.count == 1)
        #expect(service.resolveCalls.first?.approvalId == "approval-1")
        #expect(service.resolveCalls.first?.version == 3)
        #expect(service.resolveCalls.first?.digest == "abc123")
        #expect(model.approvals.first?.state == .allowed)
        #expect(model.approvalErrorKey == nil)
    }

    @Test("Allow Once and Reject update the original card")
    func allowAndRejectUpdateCard() async {
        for allow in [true, false] {
            let service = StubSessionService()
            let current = approval()
            service.resolveHandler = { _, _, _, _, _, allowFlag in
                var updated = current
                updated.state = allowFlag ? .allowed : .rejected
                return updated
            }
            let model = await modelWithApproval(service: service, approval: current)
            await model.resolveApproval(current, allow: allow)

            #expect(model.approvals.count == 1)
            #expect(model.approvals.first?.approvalId == current.approvalId)
            #expect(model.approvals.first?.state == (allow ? .allowed : .rejected))
            #expect(model.approvals.first?.version == current.version)
        }
    }

    @Test("a stale reviewed variant refreshes the card and requires a fresh decision")
    func staleVariantRefreshes() async {
        let service = StubSessionService()
        let current = approval()
        let changed = Approval(
            approvalId: current.approvalId,
            sessionId: current.sessionId,
            toolName: current.toolName,
            callId: current.callId,
            reason: current.reason,
            version: 4,
            expiresAtUnixMs: current.expiresAtUnixMs,
            digestHex: "def456",
            state: .changed
        )
        service.resolveHandler = { _, _, _, _, _, _ in
            throw SessionServiceError(.staleApproval)
        }
        service.approvalsHandler = { _, _ in [changed] }
        let model = await modelWithApproval(service: service, approval: current)

        await model.resolveApproval(current, allow: true)

        #expect(model.approvalErrorKey == .approvalStale)
        #expect(model.approvals.first?.state == .changed)
        #expect(model.approvals.first?.version == 4)
        #expect(model.approvals.first?.state != .allowed)
    }

    @Test("an expired approval cannot be submitted")
    func expiredApproval() async {
        let service = StubSessionService()
        let expired = approval(state: .expired, expiresAt: Date().addingTimeInterval(-60))
        let model = await modelWithApproval(service: service, approval: expired)

        #expect(!model.isApprovalActionable(expired))
        await model.resolveApproval(expired, allow: true)
        #expect(service.resolveCalls.isEmpty)
        #expect(model.approvals.first?.state == .expired)
    }

    @Test("a pending approval past its expiry is treated as expired")
    func pendingPastExpiry() async {
        let service = StubSessionService()
        let pending = approval(expiresAt: Date().addingTimeInterval(-1))
        let model = await modelWithApproval(service: service, approval: pending)

        #expect(model.isApprovalExpired(pending))
        #expect(!model.isApprovalActionable(pending))
        await model.resolveApproval(pending, allow: false)
        #expect(service.resolveCalls.isEmpty)
    }

    @Test("an already resolved approval cannot be submitted again")
    func alreadyResolved() async {
        let service = StubSessionService()
        let allowed = approval(state: .allowed)
        let model = await modelWithApproval(service: service, approval: allowed)

        await model.resolveApproval(allowed, allow: true)
        #expect(service.resolveCalls.isEmpty)
    }

    @Test("an approval changed elsewhere replaces the original card")
    func changedElsewhere() async {
        let service = StubSessionService()
        let current = approval()
        let model = await modelWithApproval(service: service, approval: current)

        let updated = Approval(
            approvalId: current.approvalId,
            sessionId: current.sessionId,
            toolName: current.toolName,
            callId: current.callId,
            reason: "Updated reason",
            version: 5,
            expiresAtUnixMs: current.expiresAtUnixMs,
            digestHex: "updated",
            state: .pending
        )
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 9,
                event: .approvalChanged(updated)
            )
        )

        #expect(model.approvals.count == 1)
        #expect(model.approvals.first?.version == 5)
        #expect(model.approvals.first?.reason == "Updated reason")
    }

    @Test("approval events for another session are ignored")
    func otherSessionIgnored() async {
        let service = StubSessionService()
        let current = approval()
        let model = await modelWithApproval(service: service, approval: current)

        let other = Approval(
            approvalId: "approval-other",
            sessionId: "s-002",
            toolName: current.toolName,
            callId: current.callId,
            reason: current.reason,
            version: current.version,
            expiresAtUnixMs: current.expiresAtUnixMs,
            digestHex: current.digestHex,
            state: .pending
        )
        model.handle(
            SessionEventEnvelope(
                hostId: "studio-mac",
                sessionId: "s-001",
                sequence: 10,
                event: .approvalChanged(other)
            )
        )
        #expect(model.approvals.count == 1)
        #expect(model.approvals.first?.approvalId == current.approvalId)
    }

    @Test("every approval state maps to a localized label")
    func localizedStates() {
        for state in [ApprovalState.pending, .allowed, .rejected, .expired, .changed] {
            let key = L10nKey.approvalStateKey(for: state)
            #expect(!key.string.isEmpty)
            #expect(key.string != key.rawValue)
        }
        #expect(L10nKey.approvalStateKey(for: .allowed) != L10nKey.approvalStateKey(for: .rejected))
    }
}
