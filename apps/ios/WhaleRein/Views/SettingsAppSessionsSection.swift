// Scoped Cloud app-session list and revocation controls.

import SwiftUI

struct SettingsAppSessionsSection: View {
    let model: AccountSettingsModel
    @State private var pendingScope: AccountSessionScope?

    // MARK: - Cloud app sign-ins

    @ViewBuilder
    var body: some View {
        Section(L10n.settingsAppSessionsTitle.key) {
            ForEach(model.sessions) { session in
                appSessionRow(session, model: model)
            }
            if model.sessions.isEmpty {
                Text(
                    model.sessionsError != nil
                        ? L10n.settingsAppSessionsUnavailable.key
                        : L10n.settingsAppSessionsEmpty.key
                )
                .font(.footnote)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("settings.appSessions.empty")
            }

            Button(role: .destructive) {
                pendingScope = .all
            } label: {
                Text(L10n.settingsAppSessionsRevokeAll.key)
            }
            .disabled(model.isBusy || model.sessions.isEmpty)
            .accessibilityIdentifier("settings.appSession.revokeAll")

            if model.revokingScope != nil {
                ProgressView()
                    .accessibilityIdentifier("settings.appSessions.busy")
            }
            if let revocation = model.lastRevocation {
                Text(revocationSummary(revocation))
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("settings.appSession.revocation")
            }
            if let error = model.sessionsError {
                Text(L10nKey.errorKey(for: error).key)
                    .font(.footnote)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("settings.appSessions.error")
            }
            if let statusKey = model.statusKey {
                Text(statusKey.key)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("settings.appSession.status")
            }
        }
        .task {
            if model.sessionsState == .idle {
                await model.loadSessions()
            }
        }
        .alert(
            pendingScope == .all
                ? L10n.settingsAppSessionsConfirmAllTitle.string
                : L10n.settingsAppSessionsConfirmOneTitle.string,
            isPresented: Binding(
                get: { pendingScope != nil },
                set: { if !$0 { pendingScope = nil } }
            )
        ) {
            Button(L10n.commonCancel.key, role: .cancel) {
                pendingScope = nil
            }
            .accessibilityIdentifier("settings.appSession.cancel")
            Button(
                pendingScope == .all
                    ? L10n.settingsAppSessionsRevokeAll.string
                    : L10n.settingsAppSessionsRevoke.string,
                role: .destructive
            ) {
                let scope = pendingScope
                pendingScope = nil
                guard let scope else { return }
                Task {
                    switch scope {
                    case .one(let id):
                        if let session = model.sessions.first(where: { $0.sessionId == id }) {
                            await model.revoke(session: session)
                        }
                    case .all:
                        await model.revokeAllSessions()
                    }
                }
            }
            .accessibilityIdentifier("settings.appSession.confirm")
        } message: {
            Text(
                pendingScope == .all
                    ? L10n.settingsAppSessionsConfirmAllMessage.string
                    : L10n.settingsAppSessionsConfirmOneMessage.string
            )
        }
    }

    private func appSessionRow(
        _ session: AccountAppSession,
        model: AccountSettingsModel
    ) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: session.sessionId)
                        .font(.caption.monospaced())
                        .lineLimit(1)
                        .truncationMode(.middle)
                    Text(
                        session.isCurrent
                            ? L10n.settingsAppSessionsCurrent.key
                            : L10n.settingsAppSessionsOther.key
                    )
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("settings.appSessionScope.\(session.sessionId)")
                }
                Spacer()
                Text(session.stateKey.key)
                    .font(.caption2)
                    .foregroundStyle(session.isActive ? Color.secondary : Color.orange)
                    .accessibilityIdentifier("settings.appSessionState.\(session.sessionId)")
            }
            HStack(spacing: 16) {
                Text(verbatim: session.instanceId)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                Text(
                    session.expiresAtDate.formatted(date: .abbreviated, time: .shortened)
                )
                .font(.caption2)
                .foregroundStyle(.secondary)
                Spacer()
                if session.isActive {
                    Button(L10n.settingsAppSessionsRevoke.key, role: .destructive) {
                        pendingScope = .one(session.sessionId)
                    }
                    .font(.caption)
                    .disabled(model.isBusy)
                    .accessibilityIdentifier(
                        "settings.appSession.revoke.\(session.sessionId)"
                    )
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("settings.appSession.\(session.sessionId)")
    }

    private func revocationSummary(_ revocation: AccountSessionRevocation) -> String {
        let scope: String
        switch revocation.scope {
        case .one(let id):
            scope = "\(L10n.settingsAppSessionsRevoke.string): \(id)"
        case .all:
            scope = L10n.settingsAppSessionsRevokeAll.string
        }
        let state = revocation.isPending
            ? L10n.settingsAppSessionsPending.string
            : L10n.settingsAppSessionsRevoked.string
        return "\(scope) — \(state)"
    }

}
