// Settings sections for selected-instance status, the Core-backed default
// computer, scoped Cloud app sign-ins and production diagnostics.
//
// Every destructive account action shows its exact scope in a confirmation
// before Core or Cloud is called. Cancel leaves all state untouched.

import SwiftUI

struct SettingsAccountView: View {
    @Environment(AppModel.self) private var app
    @State private var pendingScope: AccountSessionScope?
    @State private var showDiagnostics = false

    @ViewBuilder
    var body: some View {
        // Diagnostics are a production support surface; keep them reachable
        // without scrolling past the account and sign-in sections.
        diagnosticsSection
        statusSection
        defaultComputerSection
        if let model = app.accountSettings, model.canManageSessions {
            appSessionsSection(model)
        }
    }

    // MARK: - Status

    @ViewBuilder
    private var statusSection: some View {
        Section(L10n.settingsStatus.key) {
            if let model = app.accountSettings, let status = model.status {
                row(
                    L10n.settingsStatusService,
                    value: status.service.labelKey.string,
                    identifier: "settings.status.service"
                )
                row(
                    L10n.settingsStatusAccount,
                    value: status.accountState.labelKey.string
                        + (status.remoteSignoutPending
                            ? " · \(L10n.settingsStatusPendingSignOut.string)"
                            : ""),
                    identifier: "settings.status.account"
                )
                row(
                    L10n.settingsStatusDeviceAccess,
                    value: status.deviceAccess.labelKey.string,
                    identifier: "settings.status.deviceAccess"
                )
                row(
                    L10n.settingsStatusDefaultHost,
                    value: status.defaultHost ?? L10n.settingsDefaultNone.string,
                    identifier: "settings.status.defaultHost"
                )
                if let environment = status.testingEnvironment {
                    row(
                        L10n.settingsStatusTestingEnvironment,
                        value: environment,
                        identifier: "settings.status.testingEnvironment"
                    )
                }
                if status.remoteSignoutPending {
                    Text(L10n.settingsStatusPendingSignOut.key)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("settings.status.pendingSignOut")
                }
            } else if let error = app.accountSettings?.statusError {
                Text(L10n.settingsStatusUnavailable.key)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("settings.status.unavailable")
                Text(L10nKey.errorKey(for: error).key)
                    .font(.caption)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("settings.status.error")
            } else {
                ProgressView()
                    .accessibilityIdentifier("settings.status.loading")
            }
        }
        .task {
            if app.accountSettings?.statusState == .idle {
                await app.accountSettings?.load()
            }
        }
    }

    // MARK: - Default computer

    @ViewBuilder
    private var defaultComputerSection: some View {
        Section(L10n.settingsDefaultTitle.key) {
            if let model = app.accountSettings {
                if model.hosts.isEmpty {
                    Text(
                        model.defaultHostError == nil
                            ? L10n.settingsDefaultNone.key
                            : L10n.settingsDefaultUnavailable.key
                    )
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("settings.defaultHost.none")
                } else {
                    ForEach(model.hosts) { host in
                        Button {
                            Task { await model.selectDefaultHost(host.hostId) }
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(verbatim: host.hostId)
                                    Text(
                                        host.online
                                            ? L10n.hostsOnline.key
                                            : L10n.hostsOffline.key
                                    )
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                }
                                Spacer()
                                if model.defaultHostId == host.hostId {
                                    Label(
                                        L10n.settingsDefaultCurrent.key,
                                        systemImage: "checkmark.circle.fill"
                                    )
                                    .font(.caption)
                                    .foregroundStyle(.tint)
                                    .accessibilityIdentifier(
                                        "settings.defaultHost.current.\(host.hostId)"
                                    )
                                } else {
                                    Text(L10n.settingsDefaultSelect.key)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                            }
                        }
                        .disabled(model.defaultHostId == host.hostId || model.isBusy)
                        .accessibilityIdentifier("settings.defaultHost.\(host.hostId)")
                    }
                }
                if let statusKey = model.statusKey {
                    Text(statusKey.key)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("settings.defaultHost.status")
                }
                if let error = model.defaultHostError {
                    Text(L10nKey.errorKey(for: error).key)
                        .font(.footnote)
                        .foregroundStyle(.red)
                        .accessibilityIdentifier("settings.defaultHost.error")
                }
            }
        }
        .task {
            await app.accountSettings?.loadStatus()
            await app.accountSettings?.loadHosts()
        }
    }

    // MARK: - Cloud app sign-ins

    @ViewBuilder
    private func appSessionsSection(_ model: AccountSettingsModel) -> some View {
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

    // MARK: - Diagnostics

    private var diagnosticsSection: some View {
        Section(L10n.settingsDiagnosticsTitle.key) {
            Text(L10n.settingsDiagnosticsBody.key)
                .font(.footnote)
                .foregroundStyle(.secondary)
            Button {
                showDiagnostics = true
            } label: {
                Label(L10n.settingsDiagnosticsOpen.key, systemImage: "stethoscope")
            }
            .accessibilityIdentifier("settings.diagnostics.open")
        }
        .sheet(isPresented: $showDiagnostics) {
            DiagnosticsView()
        }
    }

    private func isSessionsFailed(_ state: AccountSettingsModel.LoadState) -> Bool {
        if case .failed = state { return true }
        return false
    }

    private func row(_ label: L10nKey, value: String, identifier: String) -> some View {
        LabeledContent(label.key) {
            Text(verbatim: value)
                .accessibilityIdentifier(identifier)
        }
    }
}
