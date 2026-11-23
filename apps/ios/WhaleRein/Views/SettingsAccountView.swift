// Settings sections for selected-instance status, the Core-backed default
// computer, scoped Cloud app sign-ins and production diagnostics.
//
// Every destructive account action shows its exact scope in a confirmation
// before Core or Cloud is called. Cancel leaves all state untouched.

import SwiftUI

struct SettingsAccountView: View {
    let account: AccountSettingsModel?
    let hostConnectivity: HostConnectivityModel
    @Binding var showDiagnostics: Bool
    @State private var pendingScope: AccountSessionScope?

    @ViewBuilder
    var body: some View {
        // Diagnostics are a production support surface; keep them reachable
        // without scrolling past the account and sign-in sections.
        diagnosticsSection
        statusSection
        defaultComputerSection
        if let model = account, model.canManageSessions {
            SettingsAppSessionsSection(model: model)
        }
    }

    // MARK: - Status

    @ViewBuilder
    private var statusSection: some View {
        Section(L10n.settingsStatus.key) {
            if let model = account, let status = model.status {
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
            } else if let error = account?.statusError {
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
            if account?.statusState == .idle {
                await account?.load()
            }
        }
    }

    // MARK: - Default computer

    @ViewBuilder
    private var defaultComputerSection: some View {
        Section(L10n.settingsDefaultTitle.key) {
            if let model = account {
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
                                        hostConnectivity
                                            .status(for: host.hostId, fallbackOnline: host.online)
                                            .presenceKey
                                            .key
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
            await account?.loadStatus()
            await account?.loadHosts()
        }
    }


    private func row(_ label: L10nKey, value: String, identifier: String) -> some View {
        LabeledContent(label.key) {
            Text(verbatim: value)
                .accessibilityIdentifier(identifier)
        }
    }

        private var diagnosticsSection: some View {
            Section(L10n.settingsDiagnosticsTitle.key) {
                Text(L10n.settingsDiagnosticsBody.key)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                Button {
                    // SettingsView owns the diagnostics sheet so it stays inside
                    // the current Settings presentation.
                    showDiagnostics = true
                } label: {
                    Label(L10n.settingsDiagnosticsOpen.key, systemImage: "stethoscope")
                }
                .accessibilityIdentifier("settings.diagnostics.open")
            }
        }
    }
