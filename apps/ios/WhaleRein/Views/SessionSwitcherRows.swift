// Switcher list styling, session rows and activity text.

import SwiftUI

struct SwitcherListStyle: ViewModifier {
    let style: SessionSwitcherStyle

    func body(content: Content) -> some View {
        switch style {
        case .sheet:
            // Plain lists are the dense list style; inset grouped lists are
            // meant for forms and add ~20 pt of chrome per section.
            content.listStyle(.plain)
        case .sidebar:
            content
                .listStyle(.sidebar)
                .scrollContentBackground(.hidden)
        }
    }
}

/// Single-line session row: running state, title and last-active time. The
/// workspace lives in the section header instead of a per-row path.
struct SwitcherSessionRow: View {
    let session: SessionSummary
    let hostId: String
    let isCurrent: Bool

    var body: some View {
        HStack(spacing: 8) {
            if session.running {
                Image(systemName: "waveform")
                    .font(.caption)
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
            }
            Text(verbatim: session.title)
                .font(.body)
                .lineLimit(1)
                .truncationMode(.middle)
            if session.archived {
                Text(L10n.sessionsArchived.key)
                    .font(.caption2.weight(.medium))
                    .foregroundStyle(.secondary)
                    .padding(.horizontal, 5)
                    .padding(.vertical, 1)
                    .background(.quaternary, in: Capsule())
                    .accessibilityHidden(true)
            }
            Spacer(minLength: 8)
            SessionActivityText(date: session.updatedAt)
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            if isCurrent {
                Image(systemName: "checkmark")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityLabel))
        .accessibilityAddTraits(isCurrent ? [.isSelected] : [])
    }

    private var statusText: String {
        if session.archived { return L10n.sessionsArchived.string }
        return session.running ? L10n.sessionsRunning.string : L10n.sessionsStatusIdle.string
    }

    private var accessibilityLabel: String {
        SessionAccessibility.rowLabel(
            title: session.title,
            status: statusText,
            activity: SessionActivityText.plainText(for: session.updatedAt),
            hostId: hostId,
            isCurrent: isCurrent,
            selectedText: L10n.hostsSelected.string
        )
    }
}

/// System relative time for recent activity, switching to a short absolute date
/// once a relative value stops being useful.
struct SessionActivityText: View {
    let date: Date
    private let now = Date()

    var body: some View {
        if Self.isOldEnough(date, now: now) {
            Text(date, format: .dateTime.month(.abbreviated).day())
        } else {
            Text(date, style: .relative)
        }
    }

    static func plainText(for date: Date, now: Date = Date()) -> String {
        if isOldEnough(date, now: now) {
            return date.formatted(.dateTime.month(.abbreviated).day())
        }
        return date.formatted(.relative(presentation: .named))
    }

    static func isOldEnough(_ date: Date, now: Date) -> Bool {
        let seconds = now.timeIntervalSince(date)
        return seconds > 7 * 24 * 60 * 60
    }
}


@MainActor
extension SessionSwitcherModels {
    /// The Host the session list belongs to: the remembered one, then the
    /// first online Host, then the first Host.
    var currentHost: HostSummary? {
        hosts.hosts.first { $0.hostId == hosts.selectedHostId }
            ?? hosts.hosts.first(where: \.online)
            ?? hosts.hosts.first
    }
}
