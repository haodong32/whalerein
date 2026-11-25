// Conversation screen: lazy stable rows, plain-text live preview, native Markdown
// for committed assistant text, typed tool/approval rows, and a composer that
// keeps focus and draft while output streams.

import SwiftUI

struct ConversationView: View {
    @Bindable var model: ConversationModel
    /// Presentation inputs the shell owns; the screen does not read it.
    let reasoningVisible: Bool
    let isCached: Bool
    /// True while this model is still the shell's current conversation.
    let isCurrent: () -> Bool
    @FocusState private var composerFocused: Bool

    var body: some View {
        VStack(spacing: 0) {
            content
            ComposerView(
                model: model,
                isCached: isCached,
                focused: $composerFocused
            )
        }
        .background(Color(.systemBackground))
        .navigationTitle(model.sessionTitle)
        .inlineNavigationTitle()
        .toolbar {
            ToolbarItem(placement: .principal) {
                VStack(spacing: 1) {
                    Text(model.sessionTitle)
                        .font(.headline)
                        .lineLimit(1)
                    HostConnectionStatusChrome(
                        status: model.connectivity.status(for: model.hostId)
                    )
                }
            }
        }
        .toolbarBackground(.hidden, for: .navigationBar)
        .modifier(TopScrollEdgeEffect())
        .overlay(alignment: .top) {
            TopTitlebarWash()
        }
        .task {
            await model.start()
        }
        .onDisappear {
            // A superseded conversation must not stop the watch that replaced it.
            if isCurrent() {
                model.close()
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch model.loadState {
        case .idle, .loading:
            VStack(spacing: 10) {
                ProgressView()
                Text(L10n.commonLoading.key)
                    .foregroundStyle(.secondary)
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .accessibilityIdentifier("conversation.loading")
        case .failed:
            VStack(spacing: 12) {
                Text(L10n.commonError.key)
                    .font(.headline)
                if let errorKey = model.errorKey {
                    Text(errorKey.key)
                        .multilineTextAlignment(.center)
                        .foregroundStyle(.secondary)
                }
                Button(L10n.commonRetry.key) {
                    Task { await model.start() }
                }
                .accessibilityIdentifier("conversation.retry")
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity)
        case .loaded:
            ConversationTranscriptView(model: model, reasoningVisible: reasoningVisible)
        }
    }
}
