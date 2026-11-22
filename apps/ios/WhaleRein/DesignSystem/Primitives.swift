// Stateless UI primitives shared by rewritten feature views.
//
// They carry no business state and no service access: a caller passes a value
// and an optional action. Keeping them here stops every screen from inventing
// its own status row, empty state or primary button.

import SwiftUI

/// One label/value row for typed status surfaces (connection, queue, approval).
struct StatusRow: View {
    let title: LocalizedStringKey
    let value: String
    var systemImage: String?
    var tone: Tone = .neutral

    enum Tone {
        case neutral
        case positive
        case warning
        case negative

        var color: Color {
            switch self {
            case .neutral: .secondary
            case .positive: .green
            case .warning: .orange
            case .negative: .red
            }
        }
    }

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 8) {
            if let systemImage {
                Image(systemName: systemImage)
                    .foregroundStyle(tone.color)
                    .accessibilityHidden(true)
            }
            Text(title)
                .foregroundStyle(.secondary)
            Spacer(minLength: 8)
            Text(value)
                .multilineTextAlignment(.trailing)
                .foregroundStyle(tone == .neutral ? .primary : tone.color)
        }
        .font(.subheadline)
        .accessibilityElement(children: .combine)
    }
}

/// Empty or failure surface. The caller owns the wording and the retry action;
/// this view only guarantees the shared layout and accessibility shape.
struct EmptyStateView: View {
    let title: LocalizedStringKey
    var message: LocalizedStringKey?
    var systemImage: String = "tray"
    var actionTitle: LocalizedStringKey?
    var action: (() -> Void)?

    var body: some View {
        VStack(spacing: 12) {
            Image(systemName: systemImage)
                .font(.largeTitle)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            Text(title)
                .font(.headline)
                .multilineTextAlignment(.center)
            if let message {
                Text(message)
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
            }
            if let actionTitle, let action {
                Button(action: action) {
                    Text(actionTitle)
                }
                .buttonStyle(PrimaryButtonStyle())
            }
        }
        .frame(maxWidth: .infinity)
        .padding(24)
        .accessibilityElement(children: .contain)
    }
}

/// Filled primary action used for the one obvious next step on a screen.
struct PrimaryButtonStyle: ButtonStyle {
    func makeBody(configuration: Configuration) -> some View {
        configuration.label
            .font(.body.weight(.semibold))
            .frame(maxWidth: .infinity)
            .padding(.vertical, 12)
            .padding(.horizontal, 16)
            .background(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(Color.accentColor.opacity(configuration.isPressed ? 0.75 : 1))
            )
            .foregroundStyle(.white)
            .opacity(configuration.isPressed ? 0.9 : 1)
    }
}

#Preview("Status rows") {
    VStack(spacing: 8) {
        StatusRow(title: "Connection", value: "Connected", systemImage: "checkmark.circle", tone: .positive)
        StatusRow(title: "Queue", value: "2 pending", systemImage: "list.bullet", tone: .neutral)
        StatusRow(title: "Approval", value: "Expired", systemImage: "clock.badge.exclamationmark", tone: .warning)
    }
    .padding()
}

#Preview("Empty state") {
    EmptyStateView(
        title: "No sessions yet",
        message: "Pair a computer to start a conversation.",
        systemImage: "laptopcomputer",
        actionTitle: "Pair",
        action: {}
    )
}
