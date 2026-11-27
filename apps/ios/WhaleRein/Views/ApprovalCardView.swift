// Approval card. Binds every decision to the reviewed version and digest, shows
// expiry/changed state in text (not color), and updates the original card in
// place after a confirmed decision.

import SwiftUI

struct ApprovalCardView: View {
    @Bindable var model: ConversationModel
    let approval: Approval

    private var effectiveState: ApprovalState {
        if model.isApprovalExpired(approval) {
            return .expired
        }
        return approval.state
    }

    private var stateKey: L10nKey {
        L10nKey.approvalStateKey(for: effectiveState)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Image(systemName: "hand.raised")
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
                Text(L10n.approvalTitle.key)
                    .font(.headline)
                Spacer()
                Text(String(format: L10n.approvalVersion.string, Int64(approval.version)))
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            VStack(alignment: .leading, spacing: 6) {
                row(L10n.approvalTool, approval.toolName)
                if let review = model.approvalReview(approval) {
                    // Verified, credential-masked facts from the Host
                    // projection. Nothing is inferred from the tool name.
                    reviewRows(review)
                        .accessibilityIdentifier("approval.review.\(approval.approvalId)")
                }
                row(L10n.approvalReason, approval.reason)
                row(L10n.approvalExpires, Self.expiryText(for: approval))
            }
            .font(.subheadline)

            Text(stateKey.key)
                .font(.footnote.weight(.semibold))
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .background(Color(.tertiarySystemFill), in: Capsule())
                .accessibilityIdentifier("approval.state.\(approval.approvalId)")
                .accessibilityLabel(L10n.approvalTitle.key)
                .accessibilityValue(stateKey.key)

            HStack(spacing: 8) {
                Button {
                    Task { await model.refreshApprovalStatus(approval) }
                } label: {
                    Label(L10nKey.approvalRefresh.key, systemImage: "arrow.clockwise")
                        .font(.caption2)
                }
                .buttonStyle(.borderless)
                .disabled(model.approvalStatusRefresh[approval.approvalId] == .refreshing)
                .accessibilityIdentifier("approval.refresh.\(approval.approvalId)")

                if let status = model.approvalStatusRefresh[approval.approvalId],
                   status != .refreshing {
                    Text(L10nKey.approvalStatusKey(for: status).key)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("approval.status.\(approval.approvalId)")
                }
            }

            if effectiveState == .changed {
                Label(L10n.approvalChanged.key, systemImage: "arrow.triangle.2.circlepath")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }

            if effectiveState == .expired {
                Label(L10n.approvalExpired.key, systemImage: "clock.badge.exclamationmark")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }

            if effectiveState == .unavailable {
                Label(L10n.approvalUnavailableBody.key, systemImage: "trash.slash")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("approval.unavailable.\(approval.approvalId)")
            }

            if let limitationKey = model.approvalReviewLimitationKey(approval) {
                Label(limitationKey.key, systemImage: "exclamationmark.shield")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("approval.limit.\(approval.approvalId)")
            }

            if effectiveState == .pending,
               let reasonKey = model.approvalActionDisabledReasonKey,
               model.resolvingApprovalId != approval.approvalId {
                HStack(spacing: 4) {
                    Image(systemName: reasonKey == .approvalOffline ? "wifi.slash" : "hourglass")
                        .accessibilityHidden(true)
                    Text(reasonKey.key)
                }
                .font(.footnote)
                .foregroundStyle(.secondary)
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("approval.disabledReason.\(approval.approvalId)")
            }

            if let errorKey = model.approvalErrorKey, model.resolvingApprovalId == nil {
                Text(errorKey.key)
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("approval.error.\(approval.approvalId)")
            }

            if effectiveState == .pending {
                HStack(spacing: 12) {
                    Button {
                        Task { await model.resolveApproval(approval, allow: true) }
                    } label: {
                        Text(L10n.approvalAllowOnce.key)
                            .frame(maxWidth: .infinity)
                    }
                    .glassButtonStyle(prominent: true)
                    .controlSize(.large)
                    .disabled(!model.canAllowApproval(approval))
                    .accessibilityIdentifier("approval.allowOnce")
                    .accessibilityLabel(L10n.approvalAllowOnce.key)
                    .accessibilityValue(Text(approval.toolName))

                    Button(role: .destructive) {
                        Task { await model.resolveApproval(approval, allow: false) }
                    } label: {
                        Text(L10n.approvalReject.key)
                            .frame(maxWidth: .infinity)
                    }
                    .glassButtonStyle()
                    .controlSize(.large)
                    .disabled(!model.isApprovalActionable(approval))
                    .accessibilityIdentifier("approval.reject")
                    .accessibilityLabel(L10n.approvalReject.key)
                    .accessibilityValue(Text(approval.toolName))
                }

                if model.resolvingApprovalId == approval.approvalId {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text(L10n.approvalResolving.key)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        .padding(16)
        .frame(maxWidth: .infinity, alignment: .leading)
        .liquidGlass(in: RoundedRectangle(cornerRadius: 20))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("approval.card.\(approval.approvalId)")
    }

    @ViewBuilder
    private func reviewRows(_ review: ApprovalReviewDetails) -> some View {
        if let action = review.action, !action.isEmpty {
            row(L10n.approvalAction, action)
        }
        if let target = review.target, !target.isEmpty {
            row(L10n.approvalTarget, target)
        }
        if let impact = review.impact, !impact.isEmpty {
            row(L10n.approvalImpact, impact)
        }
    }

    private func row(_ label: L10nKey, _ value: String) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            Text(label.key)
                .foregroundStyle(.secondary)
                .frame(minWidth: 72, alignment: .leading)
            Text(verbatim: value)
                .textSelection(.enabled)
            Spacer(minLength: 0)
        }
    }

    static func expiryText(for approval: Approval) -> String {
        let formatter = DateFormatter()
        formatter.dateStyle = .short
        formatter.timeStyle = .short
        return String(format: L10n.approvalExpiresAt.string, formatter.string(from: approval.expiresAt))
    }
}
