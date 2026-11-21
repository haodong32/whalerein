// Diagnostics sheet: per-Host HTTPS and MQTT stages, versions, correlation IDs,
// redacted preview, and a two-step confirmed share.

import SwiftUI
import UIKit

struct DiagnosticsView: View {
    /// The feature model is injected by the presenting shell; the view never
    /// reaches into the app environment for it.
    @Bindable var model: DiagnosticsModel
    @Environment(\.dismiss) private var dismiss
    @State private var showShareConfirmation = false

    var body: some View {
        NavigationStack {
            Group {
                switch model.loadState {
                case .idle, .loading:
                    VStack(spacing: 10) {
                        ProgressView()
                        Text(L10n.commonLoading.key).foregroundStyle(.secondary)
                    }
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
                case .failed:
                    VStack(spacing: 12) {
                        Text(L10n.commonError.key).font(.headline)
                        if let errorKey = model.errorKey {
                            Text(errorKey.key)
                                .foregroundStyle(.secondary)
                                .multilineTextAlignment(.center)
                                .accessibilityIdentifier("diagnostics.error")
                        }
                        Button(L10n.commonRetry.key) {
                            Task { await model.load() }
                        }
                        .accessibilityIdentifier("diagnostics.retry")
                    }
                    .padding(24)
                case .loaded:
                    loadedContent(model: model)
                }
            }
            .navigationTitle(L10n.diagnosticsTitle.key)
            .inlineNavigationTitle()
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L10n.commonDone.key) {
                        dismiss()
                    }
                    .accessibilityIdentifier("diagnostics.done")
                }
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        showShareConfirmation = true
                    } label: {
                        Label(L10n.diagnosticsShare.key, systemImage: "square.and.arrow.up")
                    }
                    .disabled(model.report == nil)
                    .accessibilityIdentifier("diagnostics.share")
                }
            }
            .alert(
                L10n.diagnosticsConfirmTitle.string,
                isPresented: $showShareConfirmation
            ) {
                Button(L10n.diagnosticsConfirmShare.key) {
                    Task { await model.confirmShare() }
                }
                .accessibilityIdentifier("diagnostics.confirmShare")
                Button(L10n.commonCancel.key, role: .cancel) {
                    model.cancelShare()
                }
                .accessibilityIdentifier("diagnostics.cancel")
            } message: {
                Text(L10n.diagnosticsConfirmMessage.key)
            }
            .sheet(item: Binding(
                get: { model.shareFile },
                set: { if $0 == nil { model.dismissShare() } }
            )) { file in
                ShareSheet(url: file.url)
                    .presentationDetents([.medium, .large])
            }
        }
        .task {
            if model.loadState == .idle {
                await model.load()
            }
        }
    }

    private func loadedContent(model: DiagnosticsModel) -> some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text(L10n.diagnosticsSubtitle.key)
                    .font(.callout)
                    .foregroundStyle(.secondary)

                if let statusKey = model.statusKey {
                    Label(statusKey.key, systemImage: "info.circle")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("diagnostics.status")
                }

                VStack(alignment: .leading, spacing: 10) {
                    Text(L10n.diagnosticsStages.key)
                        .font(.headline)
                    ForEach(model.stages) { stage in
                        stageRow(stage)
                    }
                }

                VStack(alignment: .leading, spacing: 10) {
                    labeled(L10n.diagnosticsLastFailureLabel, value: localizedFailure(model.lastFailureCode))
                        .accessibilityIdentifier("diagnostics.lastFailure")
                    labeled(L10n.diagnosticsAppVersion, value: model.appVersion)
                        .accessibilityIdentifier("diagnostics.appVersion")
                    labeled(L10n.diagnosticsCoreVersion, value: model.coreRevision)
                        .accessibilityIdentifier("diagnostics.coreRevision")
                    labeled(
                        L10n.diagnosticsRedactionLabel,
                        value: model.redactionFields.joined(separator: ", ")
                    )
                    .accessibilityIdentifier("diagnostics.redaction")
                }
                .font(.callout)

                VStack(alignment: .leading, spacing: 6) {
                    Text(L10n.diagnosticsPreview.key)
                        .font(.headline)
                    ScrollView(.horizontal, showsIndicators: true) {
                        Text(verbatim: model.previewText)
                            .font(.system(.caption, design: .monospaced))
                            .textSelection(.enabled)
                            .padding(12)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .accessibilityIdentifier("diagnostics.preview")
                    }
                    .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
                }

                if let errorKey = model.errorKey {
                    Text(errorKey.key)
                        .foregroundStyle(.red)
                        .accessibilityIdentifier("diagnostics.error")
                }
            }
            .padding(20)
            .frame(maxWidth: 700, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
    }

    private func stageRow(_ stage: DiagnosticStage) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            VStack(alignment: .leading, spacing: 2) {
                Text(stage.kind == .https ? L10n.diagnosticsHttps.string : L10n.diagnosticsMqtt.string)
                    .font(.subheadline.weight(.semibold))
                Text(verbatim: stage.hostId)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                if let correlationId = stage.correlationId {
                    Text("\(L10n.diagnosticsCorrelation.string): \(correlationId)")
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                }
            }
            Spacer()
            VStack(alignment: .trailing, spacing: 2) {
                Text(L10nKey.diagnosticStageKey(for: stage.status).key)
                    .font(.caption.weight(.semibold))
                if let failureCode = stage.failureCode {
                    Text(verbatim: failureCode)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(10)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("diagnostics.stage.\(stage.hostId).\(stage.kind.rawValue)")
    }

    private func labeled(_ label: L10nKey, value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.key)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(verbatim: value)
        }
    }

    private func localizedFailure(_ code: String?) -> String {
        guard let code else { return L10n.diagnosticsNoFailure.string }
        return code
    }
}

struct ShareSheet: UIViewControllerRepresentable {
    let url: URL

    func makeUIViewController(context: Context) -> UIActivityViewController {
        UIActivityViewController(activityItems: [url], applicationActivities: nil)
    }

    func updateUIViewController(_ uiViewController: UIActivityViewController, context: Context) {}
}
