// Scoped mutation receipts and the per-session model section.

import SwiftUI

struct MutationReceiptView: View {
    let receipt: SessionMutationReceipt

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(receipt.status.sessionLabelKey.string, systemImage: icon)
                .foregroundStyle(color)
                .accessibilityIdentifier("receipt.status")
            if let reason = receipt.reasonCode {
                Text(verbatim: reason)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("receipt.reason")
            }
            if let warning = receipt.warningCode {
                Text(verbatim: warning)
                    .font(.caption.monospaced())
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("receipt.warning")
            }
            if let phase = receipt.phase {
                Text(verbatim: phase)
                    .font(.caption.monospaced())
                    .foregroundStyle(.tertiary)
            }
            if !receipt.operationId.isEmpty {
                Text(verbatim: receipt.operationId)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.tertiary)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("receipt.operationId")
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("receipt")
    }

    private var icon: String {
        switch receipt.status {
        case .confirmed: return "checkmark.circle"
        case .failed: return "xmark.octagon"
        case .uncertain: return "questionmark.circle"
        case .accepted, .submitted: return "clock"
        }
    }

    private var color: Color {
        switch receipt.status {
        case .confirmed: return .green
        case .failed: return .red
        case .uncertain: return .orange
        case .accepted, .submitted: return .secondary
        }
    }
}

// MARK: - Model picker

/// Authoritative model picker. The source catalog, the recorded session choice
/// and the source default are displayed as separate values. The DSH default
/// effect is disclosed before any selection is submitted.
struct SessionModelSection: View {
    let hostId: String
    let sessionId: String
    let supported: Bool
    let management: SessionManagementModel

    var body: some View {
        Section {
            if !supported {
                Label(L10nKey.modelUnavailable.string, systemImage: "questionmark.circle")
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("model.unsupported")
            } else if management.isModelLoading && management.catalog == nil {
                HStack(spacing: 8) {
                    ProgressView()
                    Text(L10nKey.commonLoading.key)
                }
                .accessibilityIdentifier("model.loading")
            } else {
                recordedChoiceRows
                catalogRows
                selectionRows
            }
        } header: {
            Text(L10nKey.modelTitle.string)
        }
        .task(id: "\(hostId)/\(sessionId)") {
            guard supported else { return }
            await management.loadModel(hostId: hostId, sessionId: sessionId)
        }
    }

    @ViewBuilder
    private var recordedChoiceRows: some View {
        if let sessionModel = management.sessionModel {
            LabeledContent(L10nKey.modelNext.key) {
                Text(verbatim: choiceLabel(sessionModel.next))
                    .accessibilityIdentifier("model.next")
            }
            LabeledContent(L10nKey.modelLastUsed.key) {
                Text(verbatim: choiceLabel(sessionModel.lastUsed))
                    .accessibilityIdentifier("model.lastUsed")
            }
            LabeledContent(L10nKey.modelSourceDefault.key) {
                HStack(spacing: 6) {
                    Text(verbatim: choiceLabel(sessionModel.defaultSelection))
                    if sessionModel.defaultIsFallback {
                        Text(L10nKey.modelFallback.string)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("model.fallback")
                    }
                }
            }
            if !sessionModel.pendingKnown {
                Text(L10nKey.modelPendingUnknown.string)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("model.pendingUnknown")
            }
        }
        if let error = management.sessionModelError {
            Text(L10nKey.modelSessionUnavailable.string)
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("model.sessionUnavailable")
            Text(L10nKey.errorKey(for: error).key)
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
    }

    @ViewBuilder
    private var catalogRows: some View {
        if let catalog = management.catalog {
            Text(L10nKey.modelDefaultEffectDisclosure.string)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("model.disclosure")

            ForEach(catalog.providers) { provider in
                ForEach(provider.models) { model in
                    modelMenu(provider: provider, model: model)
                }
            }

            if !catalog.complete {
                Text(L10nKey.modelPartial.string)
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("model.partial")
                ForEach(catalog.failures) { failure in
                    Text(verbatim: "\(failure.name): \(failure.code)")
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                }
            }
        } else if let error = management.catalogError {
            Text(
                error == .unsupportedCapability
                    ? L10nKey.modelUnavailable.string
                    : L10nKey.errorKey(for: error).string
            )
            .font(.callout)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier("model.unavailable")
            Button(L10nKey.modelReload.key) {
                Task { await management.loadModel(hostId: hostId, sessionId: sessionId) }
            }
            .accessibilityIdentifier("model.reload")
        }
    }

    @ViewBuilder
    private var selectionRows: some View {
        switch management.selectionState {
        case .idle:
            EmptyView()
        case .submitting:
            HStack(spacing: 8) {
                ProgressView()
                Text(L10nKey.commonLoading.key)
            }
            .accessibilityIdentifier("model.submitting")
        case .stale:
            Text(L10nKey.modelStale.string)
                .font(.callout)
                .foregroundStyle(.orange)
                .accessibilityIdentifier("model.stale")
            Button(L10nKey.modelReload.key) {
                management.clearStaleCatalog()
                Task { await management.loadModel(hostId: hostId, sessionId: sessionId) }
            }
            .accessibilityIdentifier("model.reloadStale")
        case .failed(let code):
            Text(L10nKey.errorKey(for: code).key)
                .font(.callout)
                .foregroundStyle(.red)
                .accessibilityIdentifier("model.selectionFailed")
        case .result(let outcome):
            MutationReceiptView(receipt: outcome.receipt)
            Text(
                outcome.sessionAccepted
                    ? L10nKey.modelSessionAccepted.string
                    : L10nKey.modelSessionRejected.string
            )
            .font(.callout)
            .accessibilityIdentifier("model.sessionAccepted")
            if outcome.defaultPersistenceUnconfirmed {
                Text(L10nKey.modelDefaultUnconfirmed.string)
                    .font(.callout)
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("model.defaultUnconfirmed")
            }
            if let error = management.modelRefreshError {
                Text(L10nKey.errorKey(for: error).key)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("model.refreshFailed")
            }
        }
    }

    private func modelMenu(provider: ModelProvider, model: ModelEntry) -> some View {
        Menu {
            ForEach(model.efforts) { effort in
                Button(effort.name) {
                    select(
                        ModelChoice(provider: provider.id, model: model.id, effort: effort.id)
                    )
                }
            }
            Button(L10nKey.modelDefaultEffort.string) {
                select(ModelChoice(provider: provider.id, model: model.id, effort: nil))
            }
        } label: {
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: "\(provider.name) · \(model.name)")
                        .foregroundStyle(.primary)
                    if !model.efforts.isEmpty {
                        Text(verbatim: model.efforts.map(\.name).joined(separator: ", "))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 8)
                if let next = management.sessionModel?.next,
                   next.provider == provider.id, next.model == model.id {
                    Image(systemName: "checkmark")
                        .foregroundStyle(.tint)
                        .accessibilityHidden(true)
                }
            }
            .contentShape(Rectangle())
        }
        .disabled(management.isModelLoading)
        .accessibilityIdentifier("model.row.\(provider.id).\(model.id)")
    }

    private func select(_ choice: ModelChoice) {
        Task {
            await management.selectModel(
                hostId: hostId,
                sessionId: sessionId,
                choice: choice
            )
        }
    }

    private func choiceLabel(_ choice: ModelChoice?) -> String {
        guard let choice else { return L10nKey.commonUnknown.string }
        if let effort = choice.effort {
            return "\(choice.provider)/\(choice.model) (\(effort))"
        }
        return "\(choice.provider)/\(choice.model)"
    }
}
