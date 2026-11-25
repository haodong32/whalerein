// Older-history controls above the transcript.

import SwiftUI

struct ConversationHistoryHeaderView: View {
    let model: ConversationModel

    var body: some View {
        if let provenanceKey = model.historyProvenanceKey {
            Label(provenanceKey.key, systemImage: "clock.arrow.circlepath")
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("history.provenance")
        }
        if model.history.loading {
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text(L10n.conversationLoadingEarlier.key)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .center)
            .accessibilityElement(children: .combine)
            .accessibilityIdentifier("history.loadingEarlier")
        } else if model.history.needsFreshTraversal {
            VStack(spacing: 6) {
                Text(L10n.conversationHistoryChanged.key)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button(L10n.conversationHistoryLoadFresh.key) {
                    Task { await model.loadFreshHistory() }
                }
                .font(.caption)
                .glassButtonStyle()
                .controlSize(.small)
                .accessibilityIdentifier("history.fresh")
            }
            .frame(maxWidth: .infinity, alignment: .center)
        } else if model.showsHistoryContinuation || (model.canLoadOlderHistory && model.history.errorKey != nil) {
            Button(model.history.errorKey == nil ? L10n.conversationContinueLoading.key : L10n.commonRetry.key) {
                Task { await model.loadOlderHistory() }
            }
            .font(.caption)
            .glassButtonStyle()
            .controlSize(.small)
            .frame(maxWidth: .infinity, alignment: .center)
            .accessibilityIdentifier("history.continueLoading")
        } else if model.history.complete {
            Text(L10n.conversationHistoryComplete.key)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .center)
                .accessibilityIdentifier("history.complete")
        }
        if let errorKey = model.history.errorKey {
            Text(errorKey.key)
                .font(.caption)
                .foregroundStyle(.red)
                .frame(maxWidth: .infinity, alignment: .center)
                .accessibilityIdentifier("history.error")
        }
    }
}
