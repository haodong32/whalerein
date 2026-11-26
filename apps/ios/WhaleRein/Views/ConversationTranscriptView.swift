import Combine
import SwiftUI
import UIKit

struct ConversationTranscriptView: View {
    let model: ConversationModel
    let reasoningVisible: Bool

    @State private var nativeOffset = TranscriptScrollOffset()
    @State private var geometry = TranscriptGeometry()
    @State private var scrollPhase: ScrollPhase = .idle
    @State private var readingRows: [TranscriptRowPosition] = []
    @State private var previousRows: [TranscriptRowPosition] = []
    @State private var olderEdgeArmed = true
    @State private var requestedOlderContinuation: String?
    @State private var viewportRevision = 0
    @State private var correctionAttempts = 0
    @State private var keyboardResizing = false

    @State private var trailingSpace: CGFloat = 0
    @State private var trailingTarget: CGFloat?

    var body: some View {
        ZStack(alignment: .bottomTrailing) {
            ScrollViewReader { proxy in
                ScrollView {
                    transcriptRows
                    .background {
                        TranscriptScrollOffsetProbe(offset: nativeOffset)
                            .frame(width: 0, height: 0)
                    }
                    .padding(.horizontal, 16)
                    .padding(.vertical, 12)
                    .padding(.bottom, trailingSpace)
                }
                .accessibilityIdentifier("conversation.scroll")
                .coordinateSpace(name: TranscriptRowsPreference.space)
                .scrollDismissesKeyboard(.interactively)
                .defaultScrollAnchor(.bottom, for: .initialOffset)
                .defaultScrollAnchor(model.scroll.isAtLatest ? .bottom : .top, for: .sizeChanges)
                .defaultScrollAnchor(.bottom, for: .alignment)
                .onScrollPhaseChange { _, phase in
                    scrollPhase = phase
                    if phase == .interacting { clearTrailingSpace() }
                    if phase == .tracking, requestedOlderContinuation != model.history.continuation {
                        olderEdgeArmed = true
                    }
                    if phase == .tracking || phase == .interacting {
                        model.history.positionChanged = false
                        keyboardResizing = false
                        correctionAttempts = 0
                    }
                    if phase == .idle { followBottomIfNeeded() }
                }
                .onScrollGeometryChange(for: TranscriptGeometry.self) { geometry in
                    TranscriptGeometry(
                        offset: geometry.contentOffset.y + geometry.contentInsets.top,
                        extent: geometry.contentSize.height + geometry.contentInsets.top
                            + geometry.contentInsets.bottom - geometry.bounds.height,
                        viewport: geometry.bounds.height
                    )
                } action: { _, measured in
                    let before = geometry
                    let after = nativeGeometry ?? measured
                    geometry = after
                    if let target = trailingTarget, after.extent > before.extent,
                       nativeOffset.completeTrailingShift(to: target) {
                        trailingTarget = nil
                    }
                    if !keyboardResizing, before.viewport != after.viewport {
                        correctionAttempts = 0
                    }
                    if after.offset > 100 { olderEdgeArmed = true }
                    if !keyboardResizing, scrollPhase == .interacting || scrollPhase == .decelerating {
                        if abs(after.offset - before.offset) > 0.5,
                           before.viewport == after.viewport, before.extent == after.extent,
                           viewportRevision == model.scroll.layoutRevision {
                            if after.nearLatest, after.offset > before.offset {
                                model.updateScroll(isAtLatest: true)
                            } else if !after.nearLatest, after.offset < before.offset {
                                model.updateScroll(isAtLatest: false)
                            }
                            recordReadingRows(previousRows)
                        }
                        if olderEdgeArmed, after.offset <= 100, after.offset < before.offset,
                           after.canScroll, model.canLoadOlderHistory, model.history.errorKey == nil {
                            olderEdgeArmed = false
                            requestedOlderContinuation = model.history.continuation
                            Task { await model.loadOlderHistory() }
                        }
                    }
                    if before.viewport == 0 || before.canScroll != after.canScroll {
                        fillShortTranscript()
                    }
                    followBottomIfNeeded()
                }
                .onPreferenceChange(TranscriptRowsPreference.self) { rows in
                    let oldRows = previousRows
                    previousRows = rows
                    guard !keyboardResizing, trailingTarget == nil else { return }
                    let revision = model.scroll.layoutRevision
                    let windowChanged = viewportRevision != revision
                    viewportRevision = revision
                    if windowChanged {
                        correctionAttempts = 0
                        if !model.scroll.isAtLatest, !oldRows.isEmpty { recordReadingRows(oldRows) }
                    }
                    let userMoving = scrollPhase == .interacting || scrollPhase == .decelerating
                    if model.scroll.isAtLatest || (userMoving && !windowChanged) {
                        correctionAttempts = 0
                        recordReadingRows(rows)
                    } else {
                        restoreReadingRows(rows)
                    }
                }
                .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardWillChangeFrameNotification)) { _ in
                    guard !keyboardResizing else { return }
                    keyboardResizing = true
                    if !model.scroll.isAtLatest { recordReadingRows(previousRows) }
                }
                .onReceive(NotificationCenter.default.publisher(for: UIResponder.keyboardDidChangeFrameNotification)) { _ in
                    guard keyboardResizing else { return }
                    keyboardResizing = false
                    if model.scroll.isAtLatest {
                        followBottomIfNeeded()
                    } else {
                        correctionAttempts = 0
                        restoreReadingRows(previousRows)
                    }
                }
                .onChange(of: model.scrollRequest) { _, request in
                    guard let request else { return }
                    clearTrailingSpace()
                    if request.animated {
                        withAnimation(.easeOut(duration: 0.2)) {
                            proxy.scrollTo(ConversationRowsView.bottomAnchor, anchor: .bottom)
                        }
                    } else {
                        proxy.scrollTo(ConversationRowsView.bottomAnchor, anchor: .bottom)
                    }
                }
                .onChange(of: model.history.continuation) { _, _ in fillShortTranscript() }
                .onChange(of: model.canLoadOlderHistory, initial: true) { _, _ in fillShortTranscript() }
            }

            if model.history.positionChanged {
                Text(L10n.conversationHistoryPositionChanged.key)
                    .font(.caption)
                    .padding(8)
                    .background(.regularMaterial, in: .capsule)
                    .padding(12)
                    .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
                    .accessibilityIdentifier("history.positionChanged")
                    .allowsHitTesting(false)
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

    private func clearTrailingSpace() {
        trailingTarget = nil
        trailingSpace = 0
    }

    private func restoreReadingRows(_ rows: [TranscriptRowPosition]) {
        if let correction = TranscriptRowPosition.correction(anchors: readingRows, current: rows),
           abs(correction) > 0.5, correctionAttempts < 2 {
            correctionAttempts += 1
            switch nativeOffset.shift(by: correction) {
            case .shifted:
                return
            case .needsTrailingSpace(let size, let target):
                // A removed terminal card may make the prior offset out of
                // range. Keep only the space needed by the reading anchor.
                trailingTarget = target
                trailingSpace += size
                return
            case .unchanged:
                break
            }
        }
        correctionAttempts = 0
        recordReadingRows(rows)
    }

    private func recordReadingRows(_ rows: [TranscriptRowPosition]) {
        let sorted = rows.sorted { $0.top < $1.top }
        let visible = sorted.filter { $0.bottom > 0 && $0.top < geometry.viewport }
        var anchors = visible
        if let previous = sorted.last(where: { $0.bottom <= 0 }) {
            anchors.append(previous)
        }
        if let next = sorted.first(where: { $0.top >= geometry.viewport }) {
            anchors.append(next)
        }
        if readingRows != anchors { readingRows = anchors }
        if let anchor = visible.first { model.noteTopVisibleMessage(anchor.id) }
    }

    private var nativeGeometry: TranscriptGeometry? {
        // SwiftUI can report an already inset-reduced container. Use the
        // same native viewport as offset correction when the probe is ready.
        guard let scrollView = nativeOffset.scrollView, scrollView.bounds.height > 0 else { return nil }
        let insets = scrollView.adjustedContentInset
        return TranscriptGeometry(
            offset: scrollView.contentOffset.y + insets.top,
            extent: scrollView.contentSize.height + insets.top + insets.bottom - scrollView.bounds.height,
            viewport: scrollView.bounds.height
        )
    }

    private func followBottomIfNeeded() {
        guard model.scroll.isAtLatest, scrollPhase == .idle, !keyboardResizing else { return }
        nativeOffset.followBottom()
    }

    private func fillShortTranscript() {
        let measured = nativeGeometry ?? geometry
        guard measured.viewport > 0 else { return }
        Task { await model.updateHistoryViewport(canScroll: measured.canScroll) }
    }

    private var transcriptRows: some View {
        ConversationRowsView(model: model, reasoningVisible: reasoningVisible)
    }
}
