// Session details and its model section.

import SwiftUI

struct SessionDetailsView: View {
    @Environment(\.dismiss) private var dismiss

    let hostId: String
    let session: SessionSummary
    let cached: Bool
    let management: SessionManagementModel
    var openConversation: (String, SessionSummary) -> Void = { _, _ in }
    var refresh: (() async -> Void)?

    var body: some View {
        NavigationStack {
            List {
                Section {
                    LabeledContent(L10nKey.sessionsDetailsHost.key) {
                        Text(verbatim: hostId)
                            .accessibilityIdentifier("details.host")
                    }
                    LabeledContent(L10nKey.sessionsTitle.key) {
                        Text(verbatim: session.sessionId)
                            .accessibilityIdentifier("details.session")
                    }
                    if cached {
                        Label(L10nKey.sessionsDetailsCached.key, systemImage: "internaldrive")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("details.cached")
                    }
                }

                detailSection
                modelSection
            }
            .navigationTitle(L10nKey.sessionsDetailsTitle.string)
            .inlineNavigationTitle()
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(L10nKey.commonDone.key) { dismiss() }
                        .accessibilityIdentifier("details.done")
                }
            }
            .task(id: "\(hostId)/\(session.sessionId)") {
                await management.loadDetails(hostId: hostId, sessionId: session.sessionId)
            }
        }
    }

    @ViewBuilder
    private var detailSection: some View {
        switch management.detailsState {
        case .idle, .loading:
            Section {
                HStack(spacing: 8) {
                    ProgressView()
                    Text(L10nKey.commonLoading.key)
                }
                .accessibilityIdentifier("details.loading")
            }
        case .unavailable(let code):
            Section {
                Text(L10nKey.sessionsDetailsUnavailable.string)
                    .font(.headline)
                    .accessibilityIdentifier("details.unavailable")
                Text(L10nKey.errorKey(for: code).key)
                    .foregroundStyle(.secondary)
                Text(verbatim: session.title)
                    .foregroundStyle(.secondary)
            }
        case .loaded(let detail):
            Section {
                LabeledContent(L10nKey.sessionsTitle.key) {
                    Text(verbatim: detail.title)
                }
                if !detail.cwd.isEmpty {
                    LabeledContent(L10nKey.sessionsDetailsCwd.key) {
                        Text(verbatim: detail.cwd)
                            .accessibilityIdentifier("details.cwd")
                    }
                }
                LabeledContent(L10nKey.sessionsDetailsCreated.key) {
                    Text(detail.createdAt, format: .dateTime)
                }
                LabeledContent(L10nKey.sessionsDetailsUpdated.key) {
                    Text(detail.updatedAt, format: .dateTime)
                }
                LabeledContent(L10nKey.sessionsRunning.key) {
                    Text(detail.running ? L10nKey.sessionsRunning.string : L10nKey.sessionsStatusIdle.string)
                }
                if !detail.origin.isEmpty {
                    LabeledContent(L10nKey.sessionsDetailsOrigin.key) {
                        Text(verbatim: detail.origin)
                    }
                }
                if !detail.parentSessionId.isEmpty {
                    LabeledContent(L10nKey.sessionsDetailsParent.key) {
                        Text(verbatim: detail.parentSessionId)
                    }
                }
                if detail.queuedPrompts > 0 {
                    LabeledContent(L10nKey.sessionsDetailsQueue.key) {
                        Text(verbatim: String(detail.queuedPrompts))
                    }
                }
            } header: {
                Text(cached ? L10nKey.sessionsDetailsCached.key : L10nKey.sessionsDetailsLive.key)
            }
        }
    }

    @ViewBuilder
    private var modelSection: some View {
        let supportsModel = {
            if case .loaded(let detail) = management.detailsState {
                return detail.management?.modelSelection ?? true
            }
            return true
        }()
        SessionModelSection(
            hostId: hostId,
            sessionId: session.sessionId,
            supported: supportsModel,
            management: management
        )
    }
}

// MARK: - Fork
