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
        if model.isLoadingOlderHistory {
            HStack(spacing: 8) {
                ProgressView()
                    .controlSize(.small)
                Text(L10n.conversationLoadingEarlier.key)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, alignment: .center)
            .accessibilityIdentifier("history.loading")
        } else if model.historyNeedsFreshTraversal {
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
        } else if !model.historyComplete, model.canLoadOlderHistory {
            Button(L10n.conversationLoadEarlier.key) {
                Task { await model.loadOlderHistory() }
            }
            .font(.caption)
            .glassButtonStyle()
            .controlSize(.small)
            .frame(maxWidth: .infinity, alignment: .center)
            .accessibilityIdentifier("history.loadEarlier")
        } else if model.historyComplete {
            Text(L10n.conversationHistoryComplete.key)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .center)
                .accessibilityIdentifier("history.complete")
        }
        if let historyErrorKey = model.historyErrorKey {
            Text(historyErrorKey.key)
                .font(.caption)
                .foregroundStyle(.red)
                .frame(maxWidth: .infinity, alignment: .center)
                .accessibilityIdentifier("history.error")
        }
    }
}
