// Conversation screen: lazy stable rows, plain-text live preview, native Markdown
// for committed assistant text, typed tool/approval rows, and a composer that
// keeps focus and draft while output streams.

import SwiftUI

struct ConversationView: View {
    @Environment(AppModel.self) private var app
    @Bindable var model: ConversationModel
    @FocusState private var composerFocused: Bool
    @Environment(\.dynamicTypeSize) private var dynamicTypeSize

    private static let bottomAnchor = "conversation.bottom"

    var body: some View {
        VStack(spacing: 0) {
            content
            ComposerView(
                model: model,
                isCached: app.isOfflineCached,
                focused: $composerFocused
            )
        }
        .background(Color(.systemBackground))
        .navigationTitle(model.sessionTitle)
        .inlineNavigationTitle()
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
            if app.conversation === model {
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
            transcript
        }
    }

    private var transcript: some View {
        ZStack(alignment: .bottomTrailing) {
            ScrollViewReader { proxy in
                ScrollView {
                    if usesEagerRows {
                        // Short transcripts and accessibility text sizes render
                        // eagerly. A lazy stack can otherwise leave a blank gap
                        // when an adaptive session replacement resets the
                        // scroll offset before its rows are realised.
                        VStack(alignment: .leading, spacing: 14) {
                            transcriptRows
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 12)
                    } else {
                        LazyVStack(alignment: .leading, spacing: 14) {
                            transcriptRows
                        }
                        .padding(.horizontal, 16)
                        .padding(.vertical, 12)
                    }
                }
                .accessibilityIdentifier("conversation.scroll")
                .scrollDismissesKeyboard(.interactively)
                .defaultScrollAnchor(.bottom)
                .onScrollGeometryChange(for: Bool.self) { geometry in
                    let distance = geometry.contentSize.height
                        - geometry.contentOffset.y
                        - geometry.containerSize.height
                    return distance <= 140
                } action: { _, nearBottom in
                    model.updateScroll(isAtLatest: nearBottom)
                }
                .onChange(of: model.loadState) { _, state in
                    guard state == .loaded else { return }
                    pinToLatest(proxy)
                }
                .onAppear {
                    // A restored launch can present the view after the model
                    // already finished loading, so onChange never fires. Pin the
                    // transcript to the latest row in that case as well.
                    guard model.loadState == .loaded else { return }
                    pinToLatest(proxy)
                }
                .onChange(of: model.scrollRequest) { _, request in
                    guard let request else { return }
                    let target = followTarget
                    if request.animated {
                        withAnimation(.easeOut(duration: 0.2)) {
                            proxy.scrollTo(target, anchor: .bottom)
                        }
                    } else {
                        proxy.scrollTo(target, anchor: .bottom)
                    }
                }
            }

            if model.showJumpToLatest {
                Button {
                    model.jumpToLatest()
                } label: {
                    Label(L10n.conversationJumpToLatest.key, systemImage: "arrow.down.circle.fill")
                        .labelStyle(.titleAndIcon)
                        .font(.caption.weight(.semibold))
                }
                .glassButtonStyle(controlSize: .small)
                .padding(.trailing, 16)
                .padding(.bottom, 12)
                .accessibilityIdentifier("conversation.jumpToLatest")
                .accessibilityLabel(L10n.conversationJumpToLatest.key)
                .accessibilityValue(
                    Text(
                        String(
                            format: L10nKey.unreadCountKey(for: model.scroll.unreadCount).string,
                            model.scroll.unreadCount
                        )
                    )
                )
            }
        }
    }

    /// Short conversations stay eager. Lazy stacks are worthwhile for long
    /// transcripts, but a replaced short session must show its rows
    /// immediately in every window size and orientation.
    private var usesEagerRows: Bool {
        dynamicTypeSize.isAccessibilitySize || model.messages.count <= 50
    }

    private var followTarget: String {
        if model.livePreview != nil {
            return "conversation.previewRow"
        }
        if let approval = model.approvals.last {
            return "approval-\(approval.approvalId)"
        }
        return Self.bottomAnchor
    }

    /// Pins a freshly loaded or replaced conversation to the newest content.
    /// A few retries cover lazy row realisation and keyboard/layout changes
    /// without interfering with a reader who later scrolls up.
    private func pinToLatest(_ proxy: ScrollViewProxy) {
        Task { @MainActor in
            for delay in [60, 180, 450, 900, 1400] {
                try? await Task.sleep(for: .milliseconds(delay))
                proxy.scrollTo(Self.bottomAnchor, anchor: .bottom)
            }
        }
    }

    @ViewBuilder
    private var transcriptRows: some View {
        if model.messages.isEmpty && model.livePreview == nil {
            Text(L10n.conversationEmpty.key)
                .foregroundStyle(.secondary)
                .frame(maxWidth: .infinity, alignment: .center)
                .padding(.vertical, 40)
                .accessibilityIdentifier("conversation.empty")
        }

        ForEach(model.messages) { message in
            MessageRowView(model: model, message: message)
                .id(message.messageId)
        }

        if let preview = model.livePreview {
            LivePreviewRow(text: preview)
                .id("conversation.previewRow")
        }

        ForEach(model.approvals) { approval in
            ApprovalCardView(model: model, approval: approval)
                .id("approval-\(approval.approvalId)")
        }

        Color.clear
            .frame(height: 1)
            .id(Self.bottomAnchor)
    }
}







/// iOS 26 soft scroll-edge blur under the transparent navigation bar. Older
/// deployment targets keep a plain transparent bar.
private struct TopScrollEdgeEffect: ViewModifier {
    func body(content: Content) -> some View {
        if #available(iOS 26.0, *) {
            content.scrollEdgeEffectStyle(.soft, for: .top)
        } else {
            content
        }
    }
}


/// Lightens the system scroll-edge blur so the transparent titlebar reads as
/// glass rather than a solid grey band when a card scrolls beneath it.
private struct TopTitlebarWash: View {
    var body: some View {
        LinearGradient(
            stops: [
                .init(color: Color(.systemBackground).opacity(0.72), location: 0),
                .init(color: Color(.systemBackground).opacity(0.35), location: 0.7),
                .init(color: Color(.systemBackground).opacity(0), location: 1),
            ],
            startPoint: .top,
            endPoint: .bottom
        )
        .frame(height: 120)
        .ignoresSafeArea(edges: .top)
        .allowsHitTesting(false)
        .accessibilityHidden(true)
    }
}
