// Diagnostics sheet: the captured redacted report (summary plus the full
// document), local-only Host decoration, an explicit new capture, and one
// Share tap that opens the system sheet exactly once.

import SwiftUI
import UIKit

struct DiagnosticsView: View {
    /// The feature model is injected by the presenting shell; the view never
    /// reaches into the app environment for it.
    @Bindable var model: DiagnosticsModel
    @Environment(\.dismiss) private var dismiss
    @State private var showFullReport = false

    var body: some View {
        NavigationStack {
            Group {
                switch model.loadState {
                case .idle, .loading:
                    loading
                case .failed:
                    failure
                case .loaded:
                    loadedContent
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
                        Task { await model.refresh() }
                    } label: {
                        Label(L10n.diagnosticsRefresh.key, systemImage: "arrow.clockwise")
                    }
                    .disabled(model.loadState == .loading)
                    .accessibilityIdentifier("diagnostics.refresh")
                }
                ToolbarItem(placement: .primaryAction) {
                    Button {
                        Task { await model.share() }
                    } label: {
                        Label(L10n.diagnosticsShare.key, systemImage: "square.and.arrow.up")
                    }
                    .disabled(!model.canShare)
                    .accessibilityIdentifier("diagnostics.share")
                }
            }
            .sheet(item: Binding(
                get: { model.shareFile },
                set: { if $0 == nil { model.dismissShare() } }
            )) { file in
                ShareSheet(url: file.url) { completed in
                    model.shareCompleted(completed: completed)
                }
            }
        }
        .task {
            await model.load()
        }
    }

    private var loading: some View {
        VStack(spacing: 10) {
            ProgressView()
            Text(L10n.commonLoading.key).foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }

    private var failure: some View {
        VStack(spacing: 12) {
            Text(L10n.commonError.key).font(.headline)
            if let errorKey = model.errorKey {
                Text(errorKey.key)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                    .accessibilityIdentifier("diagnostics.error")
            }
            Button(L10n.commonRetry.key) {
                Task { await model.refresh() }
            }
            .accessibilityIdentifier("diagnostics.retry")
        }
        .padding(24)
    }

    private var loadedContent: some View {
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

                hostRows
                facts
                fullReport
                shareError
            }
            .padding(20)
            .frame(maxWidth: 700, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
    }

    private var hostRows: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text(L10n.hostsTitle.key)
                .font(.headline)
            if model.summary.hosts.isEmpty {
                Text(L10n.hostsEmpty.key)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("diagnostics.hosts.empty")
            } else {
                ForEach(model.summary.hosts) { host in
                    hostRow(host)
                }
            }
        }
    }

    private func hostRow(_ host: DiagnosticsHostRow) -> some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(alignment: .firstTextBaseline, spacing: 8) {
                Text(verbatim: host.label)
                    .font(.subheadline.weight(.semibold))
                Spacer(minLength: 8)
                Text(host.connectionLabel)
                    .font(.caption.weight(.semibold))
            }
            if let local = model.localHost(for: host.label) {
                // Local directory decoration: intentionally outside the
                // captured document and the shared file.
                Text(decoration(local))
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("diagnostics.local.\(host.label)")
            }
        }
        .padding(10)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("diagnostics.host.\(host.label)")
    }

    private func decoration(_ local: DiagnosticsLocalHost) -> String {
        guard let osFamily = local.osFamily else { return local.displayName }
        return "\(local.displayName) · \(osFamily)"
    }

    private var facts: some View {
        VStack(alignment: .leading, spacing: 8) {
            labeled(
                L10n.diagnosticsLastFailureLabel,
                value: model.summary.lastFailure?.displayText ?? L10n.diagnosticsNoFailure.string
            )
            .accessibilityIdentifier("diagnostics.lastFailure")
            labeled(L10n.diagnosticsAppVersion, value: model.capture?.appVersion ?? "")
                .accessibilityIdentifier("diagnostics.appVersion")
            labeled(
                L10n.diagnosticsCoreVersion,
                value: model.summary.coreVersion ?? model.capture?.coreVersion ?? ""
            )
            .accessibilityIdentifier("diagnostics.coreVersion")
            if let capturedAt = model.summary.capturedAt {
                labeled(L10n.diagnosticsCapturedAt, value: Self.captureText(capturedAt))
                    .accessibilityIdentifier("diagnostics.capturedAt")
            }
            labeled(
                L10n.diagnosticsRedactionLabel,
                value: redactedFields.joined(separator: ", ")
            )
            .accessibilityIdentifier("diagnostics.redaction")
        }
        .font(.callout)
    }

    private var redactedFields: [String] {
        model.summary.redactionFields.isEmpty
            ? (model.capture?.redactionFields ?? [])
            : model.summary.redactionFields
    }

    private var fullReport: some View {
        DisclosureGroup(isExpanded: $showFullReport) {
            ScrollView(.horizontal, showsIndicators: true) {
                Text(verbatim: model.fullReport)
                    .font(.system(.caption, design: .monospaced))
                    .textSelection(.enabled)
                    .padding(12)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .accessibilityIdentifier("diagnostics.preview")
            }
            .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
        } label: {
            Text(L10n.diagnosticsFullReport.key)
                .font(.headline)
                .accessibilityIdentifier("diagnostics.fullReport")
        }
    }

    @ViewBuilder
    private var shareError: some View {
        if let errorKey = model.errorKey {
            Text(errorKey.key)
                .foregroundStyle(.red)
                .accessibilityIdentifier("diagnostics.error")
        }
    }

    private func labeled(_ label: L10nKey, value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(label.key)
                .font(.caption)
                .foregroundStyle(.secondary)
            Text(verbatim: value)
        }
    }

    private static func captureText(_ date: Date) -> String {
        date.formatted(date: .abbreviated, time: .shortened)
    }
}

struct ShareSheet: UIViewControllerRepresentable {
    let url: URL
    var onCompletion: (Bool) -> Void = { _ in }

    func makeUIViewController(context: Context) -> UIActivityViewController {
        let controller = UIActivityViewController(
            activityItems: [url],
            applicationActivities: nil
        )
        // UIKit invokes the completion on the main thread after the sheet ends.
        let completion = onCompletion
        controller.completionWithItemsHandler = { _, completed, _, _ in
            completion(completed)
        }
        return controller
    }

    func updateUIViewController(_ uiViewController: UIActivityViewController, context: Context) {}
}


#if DEBUG
#Preview("Diagnostics fixture") {
    if let scenario = ScenarioCatalog.scenario(id: "diagnostics.baseline") {
        let directory = ScenarioBuilder.temporaryDirectory()
        DiagnosticsView(
            model: DiagnosticsModel(
                service: ScenarioBuilder.service(for: scenario, directory: directory),
                stagingDirectory: directory
            )
        )
    }
}
#endif