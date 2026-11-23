// Composer with draft persistence, capability-aware delivery picker, Send, Stop
// and explicit pending/uncertain status text.

import SwiftUI

struct ComposerView: View {
    @Bindable var model: ConversationModel
    var isCached: Bool = false
    @FocusState.Binding var focused: Bool

    var body: some View {
        VStack(spacing: 6) {
            QueueSurfaceView(model: model)
            WaitControlView(model: model)
            connectionLine
            statusLines

            HStack(alignment: .bottom, spacing: 8) {
                TextField(
                    L10n.conversationDraftPlaceholder.key,
                    text: Binding(
                        get: { model.draft },
                        set: { newValue in
                            model.draft = newValue
                            model.draftChanged()
                        }
                    ),
                    axis: .vertical
                )
                .lineLimit(1...6)
                .focused($focused)
                .padding(.horizontal, 4)
                .padding(.vertical, 7)
                .background(.clear)
                .accessibilityIdentifier("composer.field")
                .accessibilityLabel(L10n.conversationDraftPlaceholder.key)

                if model.isRunning {
                    deliveryMenu
                    stopButton
                }

                sendButton
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 6)
            .liquidGlass(in: Capsule())
        }
        .padding(.horizontal, 12)
        .padding(.top, 4)
        .padding(.bottom, 8)
    }

    /// Connection, running and watch-ended state sit with the composer instead
    /// of occupying a header row under the navigation title.
    private var connectionLine: some View {
        HStack(spacing: 6) {
            Image(systemName: connectionSymbol)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text(connectionText)
                .font(.caption2)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("conversation.connectionStatus")
                .accessibilityLabel(L10n.conversationTitle.key)
                .accessibilityValue(connectionText)
            if isCached {
                Label(L10n.conversationCached.key, systemImage: "internaldrive")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .labelStyle(.titleAndIcon)
                    .accessibilityIdentifier("conversation.cached")
            }
            if model.isRunning {
                Text(L10n.sessionsRunning.key)
                    .font(.caption2.weight(.semibold))
                    .padding(.horizontal, 6)
                    .padding(.vertical, 1)
                    .background(Color(.tertiarySystemFill), in: Capsule())
                    .accessibilityIdentifier("conversation.running")
            }
            Spacer(minLength: 0)
            if model.watchEnded {
                Text(L10n.conversationWatchEnded.key)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("conversation.watchEnded")
                Button(L10n.conversationReconnect.key) {
                    Task { await model.reconnectWatch() }
                }
                .font(.caption2.weight(.semibold))
                .buttonStyle(.borderless)
                .disabled(model.isReconnecting)
                .accessibilityIdentifier("composer.reconnect")
                .accessibilityLabel(L10n.conversationReconnect.key)
                .accessibilityValue(L10n.conversationWatchEndedHint.key)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private var connectionText: String {
        L10nKey.connectionKey(for: model.connection).string
    }

    private var connectionSymbol: String {
        switch model.connection {
        case .online: return "checkmark.circle"
        case .degraded, .reconnecting, .connecting: return "arrow.triangle.2.circlepath"
        case .offline: return "wifi.slash"
        }
    }

    private var statusLines: some View {
        VStack(alignment: .leading, spacing: 2) {
            if let key = model.sendStatusKey {
                statusLabel(key.key, systemImage: "paperplane", color: .secondary)
                    .accessibilityIdentifier("conversation.sendStatus")
            }
            if let key = model.operationStatusKey {
                statusLabel(key.key, systemImage: "envelope.badge", color: .secondary)
                    .accessibilityIdentifier("conversation.operationStatus")
            }
            if model.operationInputRequired {
                statusLabel(L10n.conversationOperationInputRequired.key, systemImage: "questionmark.circle", color: .secondary)
                    .accessibilityIdentifier("conversation.operationInputRequired")
            }
            if let reference = model.operationReferenceText {
                HStack(spacing: 4) {
                    Image(systemName: "text.magnifyingglass")
                        .accessibilityHidden(true)
                    Text(verbatim: reference)
                }
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityElement(children: .combine)
                .accessibilityIdentifier("conversation.operationReference")
            }
            if let key = model.operationReadErrorKey {
                statusLabel(key.key, systemImage: "exclamationmark.triangle", color: .orange)
                    .accessibilityIdentifier("conversation.operationReadError")
            }
            if model.canRecoverSend || model.canReadExactReply {
                HStack(spacing: 12) {
                    if model.canRecoverSend {
                        Button(L10n.conversationOperationRecover.key) {
                            Task { await model.recoverSendResult() }
                        }
                        .font(.caption)
                        .accessibilityIdentifier("conversation.operationRecover")
                    }
                    if model.canReadExactReply {
                        Button(L10n.conversationOperationReadExact.key) {
                            Task { await model.readExactReply() }
                        }
                        .font(.caption)
                        .accessibilityIdentifier("conversation.operationReadExact")
                    }
                }
            }
            if let key = model.stopStatusKey {
                statusLabel(key.key, systemImage: "stop.circle", color: .secondary)
                    .accessibilityIdentifier("conversation.stopStatus")
            }
            if let key = model.draftStorageWarningKey {
                // Non-blocking: the typed text stays usable, but the user is
                // told it may not survive a restart when storage failed.
                statusLabel(key.key, systemImage: "externaldrive.badge.exclamationmark", color: .orange)
                    .accessibilityIdentifier("composer.draftStorageWarning")
            }
            if let key = model.sendErrorKey {
                statusLabel(key.key, systemImage: "exclamationmark.triangle", color: .red)
                    .accessibilityIdentifier("composer.error")
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    /// Status rows combine the icon and the localized text into one element so
    /// the identifier never resolves to an SF Symbol accessibility label such as
    /// "Send" or "Wi-Fi Off".
    private func statusLabel(_ key: LocalizedStringKey, systemImage: String, color: Color) -> some View {
        HStack(spacing: 4) {
            Image(systemName: systemImage)
                .accessibilityHidden(true)
            Text(key)
        }
        .font(.caption)
        .foregroundStyle(color)
        .accessibilityElement(children: .combine)
    }

    private var deliveryMenu: some View {
        Menu {
            ForEach(model.deliveryChoices, id: \.self) { choice in
                Button {
                    model.selectDelivery(choice)
                } label: {
                    if choice == model.delivery {
                        Label(L10nKey.deliveryKey(for: choice).key, systemImage: "checkmark")
                    } else {
                        Text(L10nKey.deliveryKey(for: choice).key)
                    }
                }
                .accessibilityIdentifier("composer.delivery.\(choice.rawValue)")
            }
        } label: {
            Label(
                L10nKey.deliveryKey(for: model.delivery).string,
                systemImage: "arrow.up.arrow.down.circle"
            )
            .font(.caption)
        }
        .accessibilityIdentifier("composer.delivery")
        .accessibilityLabel(L10n.conversationDelivery.key)
        .accessibilityValue(Text(L10nKey.deliveryKey(for: model.delivery).key))
    }

    private var stopButton: some View {
        Button(role: .destructive) {
            Task { await model.stopRun() }
        } label: {
            Image(systemName: "stop.circle.fill")
                .font(.title2)
        }
        .disabled(!model.canStop)
        .accessibilityIdentifier("composer.stop")
        .accessibilityLabel(L10n.conversationStop.key)
    }

    private var sendButton: some View {
        Button {
            Task { await model.send() }
        } label: {
            Image(systemName: "arrow.up.circle.fill")
                .font(.title2)
        }
        .disabled(!model.canSend)
        .accessibilityIdentifier("composer.send")
        .accessibilityLabel(L10n.conversationSend.key)
    }
}
