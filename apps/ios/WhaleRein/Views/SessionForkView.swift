// Fork and the scoped mutation receipt shown beside it.

import SwiftUI

struct SessionForkView: View {
    @Environment(\.dismiss) private var dismiss

    let hostId: String
    let session: SessionSummary
    let management: SessionManagementModel
    var openConversation: (String, SessionSummary) -> Void = { _, _ in }
    var refresh: (() async -> Void)?
    private var scope: SessionScope {
        SessionScope(hostId: hostId, sessionId: session.sessionId)
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    LabeledContent(L10nKey.sessionsDetailsHost.key) {
                        Text(verbatim: hostId)
                    }
                    LabeledContent(L10nKey.sessionsTitle.key) {
                        Text(verbatim: session.sessionId)
                    }
                }

                content
            }
            .navigationTitle(L10nKey.sessionsForkTitle.string)
            .inlineNavigationTitle()
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(L10nKey.commonDone.key) { dismiss() }
                        .accessibilityIdentifier("fork.done")
                }
            }
            .task(id: scope) {
                if case .idle = management.forkState {
                    await management.forkSession(hostId: hostId, sessionId: session.sessionId)
                }
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch management.forkState {
        case .idle, .forking:
            Section {
                HStack(spacing: 8) {
                    ProgressView()
                    Text(L10nKey.commonLoading.key)
                }
                .accessibilityIdentifier("fork.progress")
            }
        case .failed(let code):
            Section {
                Text(L10nKey.errorKey(for: code).key)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("fork.error")
                Button(L10nKey.commonRetry.key) {
                    Task {
                        await management.forkSession(hostId: hostId, sessionId: session.sessionId)
                    }
                }
                .accessibilityIdentifier("fork.retry")
            }
        case .result(let outcome):
            Section {
                MutationReceiptView(receipt: outcome.receipt)
                if let directory = outcome.workingDirectory {
                    LabeledContent(L10nKey.sessionsForkSharedDirectory.key) {
                        Text(verbatim: directory)
                            .accessibilityIdentifier("fork.directory")
                    }
                } else if outcome.isConfirmed {
                    Label(
                        L10nKey.sessionsForkDirectoryUnavailable.string,
                        systemImage: "exclamationmark.triangle"
                    )
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("fork.directoryUnavailable")
                }
                if let childId = outcome.sessionId {
                    LabeledContent(L10nKey.sessionsTitle.key) {
                        Text(verbatim: childId)
                            .accessibilityIdentifier("fork.childId")
                    }
                }
            }

            Section {
                if let summary = outcome.openableSummary {
                    Button(L10nKey.sessionsForkOpenChild.key) {
                        openConversation(hostId, summary)
                        dismiss()
                    }
                    .accessibilityIdentifier("fork.openChild")
                }
                // A proven retryable failure may be attempted again; a
                // confirmed or uncertain fork never is.
                if outcome.canRetry {
                    Button(L10nKey.commonRetry.key) {
                        Task {
                            await management.forkSession(hostId: hostId, sessionId: session.sessionId)
                        }
                    }
                    .accessibilityIdentifier("fork.retry")
                } else if outcome.receipt.status == .uncertain {
                    Text(verbatim: outcome.receipt.reconcileKey ?? outcome.receipt.operationId)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("fork.reconcile")
                    // A later authoritative session list, not a resubmission,
                    // describes whether the fork produced a child.
                    Button(L10nKey.sessionsReceiptLookup.string) {
                        Task {
                            await refresh?()
                            dismiss()
                        }
                    }
                    .accessibilityIdentifier("fork.lookup")
                }
            }
        }
    }
}

// MARK: - Shared receipt
