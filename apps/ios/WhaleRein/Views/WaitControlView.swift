// Native state-only wait control. It shows the typed outcome, the deadline and
// an independent cancel action. It never starts, stops or replaces the watch.

import SwiftUI

struct WaitControlView: View {
    @Bindable var model: ConversationModel

    private var wait: ConversationWaitController { model.wait }

    var body: some View {
        VStack(alignment: .leading, spacing: 3) {
            HStack(spacing: 8) {
                if wait.isWaiting {
                    Button(role: .destructive) {
                        Task { await wait.cancel() }
                    } label: {
                        Label(L10nKey.waitCancel.key, systemImage: "xmark.circle")
                    }
                    .buttonStyle(.bordered)
                    .disabled(wait.isCancelling)
                    .accessibilityIdentifier("wait.cancel")
                    ProgressView()
                        .controlSize(.small)
                    Text(L10nKey.waitWaiting.key)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("wait.status.waiting")
                } else {
                    Button {
                        Task { await wait.start() }
                    } label: {
                        Label(L10nKey.waitStart.key, systemImage: "hourglass")
                    }
                    .buttonStyle(.bordered)
                    .disabled(!model.connection.isOnline || model.watchEnded)
                    .accessibilityIdentifier("wait.start")
                    .accessibilityLabel(L10nKey.waitStart.key)
                }
                Spacer(minLength: 0)
                if let deadline = wait.deadline {
                    Text(String(format: L10nKey.waitDeadline.string, Self.time(deadline)))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("wait.deadline")
                }
            }
            if let result = wait.result {
                statusLabel(result)
            } else if let error = wait.errorKey {
                Text(error.key)
                    .font(.caption2)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("wait.error")
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("wait.control")
    }

    private func statusLabel(_ result: WaitResult) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Label(
                L10nKey.waitOutcomeKey(for: result.outcome).key,
                systemImage: symbol(for: result.outcome)
            )
            .font(.caption2)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier("wait.status.\(result.outcome.rawValue)")
            if result.outcome == .inputRequired,
               let pending = result.state?.pendingInput, !pending.isEmpty {
                Text(pending.joined(separator: ", "))
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("wait.pendingInput")
            }
            Text(L10nKey.waitObserving.key)
                .font(.caption2)
                .foregroundStyle(.secondary)
        }
    }

    private func symbol(for outcome: WaitOutcome) -> String {
        switch outcome {
        case .idle: return "checkmark.circle"
        case .inputRequired: return "hand.raised"
        case .timeout: return "clock.badge.exclamationmark"
        case .cancelled: return "xmark.circle"
        case .failed: return "exclamationmark.triangle"
        }
    }

    private static func time(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.timeStyle = .short
        formatter.dateStyle = .none
        return formatter.string(from: date)
    }
}
