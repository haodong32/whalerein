// Transparent titlebar treatment for the conversation screen.

import SwiftUI

/// iOS 26 soft scroll-edge blur under the transparent navigation bar. Older
/// deployment targets keep a plain transparent bar.
struct TopScrollEdgeEffect: ViewModifier {
    func body(content: Content) -> some View {
#if compiler(>=6.2)
        if #available(iOS 26.0, *) {
            content.scrollEdgeEffectStyle(.soft, for: .top)
        } else {
            content
        }
#else
        content
#endif
    }
}


/// Lightens the system scroll-edge blur so the transparent titlebar reads as
/// glass rather than a solid grey band when a card scrolls beneath it.
struct TopTitlebarWash: View {
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

/// One authoritative Host connection, shown once next to the session title.
/// The composer keeps only session-scoped exceptions (watch ended, cached
/// transcript), so this is the single place the conversation reports Host
/// status.
struct HostConnectionStatusChrome: View {
    let status: HostConnectionStatus

    var body: some View {
        HStack(spacing: 4) {
            Image(systemName: symbol)
                .font(.caption2)
                .accessibilityHidden(true)
            Text(L10nKey.connectionKey(for: status).key)
                .font(.caption2)
        }
        .foregroundStyle(.secondary)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("host.connectionStatus")
        .accessibilityLabel(Text(L10n.conversationTitle.key))
        .accessibilityValue(Text(L10nKey.connectionKey(for: status).string))
    }

    private var symbol: String {
        switch status {
        case .connected:
            "checkmark.circle"
        case .unknown, .connecting, .degraded, .reconnecting:
            "arrow.triangle.2.circlepath"
        case .disconnected, .offline, .unavailable:
            status.symbol
        }
    }
}
