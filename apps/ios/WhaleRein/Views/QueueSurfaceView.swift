// Pending-queue surface for the composer. It renders Core's ordered items and
// only offers actions the Source advertises for the exact item. Editing is a
// whole-content replacement with an explicit attachment-removal warning.

import SwiftUI

struct QueueSurfaceView: View {
    @Bindable var model: ConversationModel

    private var queue: ConversationQueueController { model.queue }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            header
            status
            ForEach(queue.items) { item in
                queueItem(item)
            }
            mutationStatus
            if queue.editItemID != nil {
                editSection
            }
        }
        .padding(.horizontal, 12)
        .padding(.vertical, 6)
        .frame(maxWidth: .infinity, alignment: .leading)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("queue.surface")
    }

    private var header: some View {
        HStack(spacing: 6) {
            Image(systemName: "text.line.first.and.arrowtriangle.forward")
                .accessibilityHidden(true)
            Text(L10nKey.queueTitle.key)
                .font(.caption.weight(.semibold))
            if queue.isCurrent {
                Text("\(queue.items.count)")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("queue.count")
            }
            Spacer(minLength: 0)
            if queue.running {
                Text(L10nKey.queueRunning.key)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
            }
            Button {
                Task { await queue.refresh() }
            } label: {
                Image(systemName: "arrow.clockwise")
            }
            .buttonStyle(.borderless)
            .disabled(queue.isRefreshing || !queue.canRead)
            .accessibilityIdentifier("queue.refresh")
            .accessibilityLabel(L10nKey.queueRefresh.key)
        }
        .font(.caption)
        .foregroundStyle(.secondary)
    }

    @ViewBuilder
    private var status: some View {
        switch queue.freshness {
        case .idle, .loading:
            Label(L10nKey.queueLoading.key, systemImage: "hourglass")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("queue.status.loading")
        case .current where queue.items.isEmpty:
            Text(L10nKey.queueEmpty.key)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("queue.status.empty")
        case .current:
            EmptyView()
        case .stale:
            Label(L10nKey.queueStale.key, systemImage: "clock.arrow.circlepath")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("queue.status.stale")
        case .unavailable:
            Label(L10nKey.queueUnavailable.key, systemImage: "exclamationmark.triangle")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("queue.status.unavailable")
        case .unsupported:
            Label(L10nKey.queueUnsupported.key, systemImage: "questionmark.circle")
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("queue.status.unsupported")
        }
    }

    private func queueItem(_ item: QueueItem) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text(item.placement)
                    .font(.caption2.weight(.semibold))
                    .foregroundStyle(.secondary)
                Spacer(minLength: 0)
                actions(item)
            }
            ForEach(Array(item.blocks.enumerated()), id: \.offset) { _, block in
                blockView(block)
            }
            if item.actions.isEmpty {
                Text(L10nKey.queueBlockUnavailable.key)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("queue.item.unavailable.\(item.itemID)")
            }
        }
        .padding(8)
        .background(Color(.tertiarySystemFill), in: RoundedRectangle(cornerRadius: 10))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("queue.item.\(item.itemID)")
    }

    @ViewBuilder
    private func blockView(_ block: QueueBlock) -> some View {
        switch block {
        case .text(let text):
            Text(text)
                .font(.caption)
                .textSelection(.enabled)
        case .attachment(let mediaType, let name, let size, let available):
            Label(
                available
                    ? "\(name) · \(mediaType) · \(size) B"
                    : "\(name) · \(L10nKey.queueAttachmentUnavailable.string)",
                systemImage: available ? "paperclip" : "exclamationmark.triangle"
            )
            .font(.caption2)
            .foregroundStyle(.secondary)
        case .unavailable:
            Label(L10nKey.queueBlockUnavailable.key, systemImage: "doc.questionmark")
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }

    @ViewBuilder
    private func actions(_ item: QueueItem) -> some View {
        if !item.actions.isEmpty {
            Menu {
                if item.advertises(.edit), queue.supports(.edit) {
                    Button(L10nKey.queueEdit.key) { queue.beginEdit(item.itemID) }
                        .accessibilityIdentifier("queue.edit.\(item.itemID)")
                }
                if item.advertises(.remove), queue.supports(.remove) {
                    Button(role: .destructive) {
                        Task { await queue.withdraw(item.itemID) }
                    } label: {
                        Text(L10nKey.queueWithdraw.key)
                    }
                    .accessibilityIdentifier("queue.withdraw.\(item.itemID)")
                }
                if item.advertises(.promote), queue.supports(.promote) {
                    Button(L10nKey.queuePromote.key) {
                        Task { await queue.promote(item.itemID) }
                    }
                    .accessibilityIdentifier("queue.promote.\(item.itemID)")
                }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .buttonStyle(.borderless)
            .disabled(queue.isMutating)
            .accessibilityIdentifier("queue.actions.\(item.itemID)")
            .accessibilityLabel(L10nKey.queueTitle.key)
        }
    }

    @ViewBuilder
    private var mutationStatus: some View {
        if let mutation = queue.lastMutation {
            Label(
                L10nKey.queueMutationKey(for: mutation.receipt?.status ?? .uncertain).key,
                systemImage: "arrow.triangle.2.circlepath"
            )
            .font(.caption2)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier("queue.mutation.status")
        }
        if let error = queue.mutationErrorKey {
            Text(error.key)
                .font(.caption2)
                .foregroundStyle(.red)
                .accessibilityIdentifier("queue.mutation.error")
        }
    }

    private var editSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            if queue.editWarningVisible {
                Label(L10nKey.queueEditWarning.key, systemImage: "exclamationmark.triangle")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("queue.edit.warning")
            }
            TextField(
                L10nKey.queueEditPlaceholder.key,
                text: Binding(
                    get: { queue.editText },
                    set: { queue.editText = $0 }
                ),
                axis: .vertical
            )
            .lineLimit(1...5)
            .textFieldStyle(.roundedBorder)
            .accessibilityIdentifier("queue.edit.field")
            HStack(spacing: 8) {
                Button(L10nKey.queueSaveEdit.key) {
                    Task { await queue.submitEdit() }
                }
                .buttonStyle(.borderedProminent)
                .disabled(queue.isMutating)
                .accessibilityIdentifier("queue.edit.save")
                Button(L10nKey.queueCancelEdit.key) {
                    queue.cancelEdit()
                }
                .buttonStyle(.bordered)
                .accessibilityIdentifier("queue.edit.cancel")
            }
            if let error = queue.editErrorKey {
                Text(error.key)
                    .font(.caption2)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("queue.edit.error")
            }
        }
        .padding(8)
        .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 10))
    }
}
