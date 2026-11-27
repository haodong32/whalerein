// Settings and about: localized versions, language note, staging/fixture
// indicators, data location, and confirmed forget/revoke actions.

import SwiftUI

struct SettingsView: View {
    let settings: SettingsModel
    let diagnostics: DiagnosticsModel
    let account: AccountSettingsModel?
    let operations: OperationRecoveryModel?
    let cloud: CloudSignInModel?
    let dataPath: String
    let serviceSelection: ServiceSelectionContext
    /// Authoritative Host status for the default-computer list.
    let hostConnectivity: HostConnectivityModel
    let reasoningVisible: Bool
    /// Returns a typed error when the preference could not be saved.
    let setReasoningVisible: (Bool) -> AppErrorCode?
    let onSignOutCloud: () -> Void
    let onAddHost: () -> Void

    @Environment(\.dismiss) private var dismiss
    /// The destructive action awaiting confirmation, owned by the model.
    private var pendingAction: SettingsAction? { settings.state.pendingConfirmation }
    @State private var reasoningErrorKey: L10nKey?
    @State private var showDiagnostics = false

    var body: some View {
        NavigationStack {
            Form {
                Section(L10n.settingsAbout.key) {
                    LabeledContent(L10n.settingsCoreRevision.key) {
                        Text(verbatim: settings.coreRevision)
                            .font(.footnote.monospaced())
                            .accessibilityIdentifier("settings.coreRevision")
                    }
                    LabeledContent(L10n.settingsAppVersion.key) {
                        Text(verbatim: settings.appVersion)
                            .accessibilityIdentifier("settings.appVersion")
                    }
                    VStack(alignment: .leading, spacing: 4) {
                        Text(L10n.settingsLanguage.key)
                        Text(L10n.settingsLanguageNote.key)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("settings.languageNote")
                }

                Section {
                    if settings.isStaging {
                        Label(L10n.settingsStaging.key, systemImage: "flag")
                            .accessibilityIdentifier("settings.staging")
                    }
                    if settings.isFixtureBacked {
                        Label(L10n.settingsFixtureData.key, systemImage: "shippingbox")
                            .accessibilityIdentifier("settings.fixtureData")
                    }
                    if let statusKey = settings.statusKey {
                        Text(statusKey.key)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("settings.status")
                    }
                    if let errorKey = settings.errorKey {
                        Text(errorKey.key)
                            .font(.footnote)
                            .foregroundStyle(.red)
                            .accessibilityIdentifier("settings.error")
                    }
                }

                Section(L10n.settingsData.key) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(L10n.settingsStorage.key)
                        Text(verbatim: dataPath)
                            .font(.caption2.monospaced())
                            .foregroundStyle(.secondary)
                        Text(L10n.settingsStorageNote.key)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                    }
                    .accessibilityElement(children: .combine)
                    .accessibilityIdentifier("settings.dataPath")
                }

                Section {
                    Button(role: .destructive) {
                        settings.requestConfirmation(.forgetDevice)
                    } label: {
                        Text(L10n.settingsForget.key)
                    }
                    .disabled(settings.isBusy)
                    .accessibilityIdentifier("settings.forget")

                    Button(role: .destructive) {
                        settings.requestConfirmation(.revokeDevice)
                    } label: {
                        Text(L10n.settingsRevoke.key)
                    }
                    .disabled(settings.isBusy)
                    .accessibilityIdentifier("settings.revoke")
                }

                Section(L10n.settingsAppSection.key) {
                    Toggle(
                        L10n.settingsReasoning.key,
                        isOn: Binding(
                            get: { reasoningVisible },
                            set: { visible in
                                if let code = setReasoningVisible(visible) {
                                    reasoningErrorKey = L10nKey.errorKey(for: code)
                                } else {
                                    reasoningErrorKey = nil
                                }
                            }
                        )
                    )
                    .accessibilityIdentifier("settings.reasoning")
                    Text(L10n.settingsReasoningNote.key)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    if let reasoningErrorKey {
                        Text(reasoningErrorKey.key)
                            .font(.footnote)
                            .foregroundStyle(.red)
                            .accessibilityIdentifier("settings.reasoningError")
                    }
                }

                SettingsOperationsView(model: operations)

                Section(L10n.settingsLogs.key) {
                    if let url = settings.state.logExport.fileURL {
                        ShareLink(item: url) {
                            Text(L10n.settingsShareLogs.key)
                        }
                        .accessibilityIdentifier("settings.shareLogs")
                    } else {
                        Button {
                            settings.prepareLogExport()
                        } label: {
                            Text(L10n.settingsExportLogs.key)
                        }
                        .disabled(settings.state.logExport == .preparing)
                        .accessibilityIdentifier("settings.exportLogs")
                    }
                    Text(L10n.settingsExportLogsNote.key)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }


                Section {
                    ServiceSelectionView(showsStandaloneCA: true, context: serviceSelection)
                }

                if let cloud = cloud {
                    Section(L10n.cloudSettingsTitle.key) {
                        if let account = cloud.account {
                            LabeledContent(L10n.cloudAccountLabel.key) {
                                Text(verbatim: account.email)
                                    .accessibilityIdentifier("settings.cloudAccount")
                            }
                            if cloud.logoutPending {
                                Text(L10n.cloudSignOutPending.key)
                                    .font(.footnote)
                                    .foregroundStyle(.secondary)
                                    .accessibilityIdentifier("settings.cloudLogoutPending")
                            }
                            Button(role: .destructive) {
                                onSignOutCloud()
                            } label: {
                                Text(L10n.cloudSignOut.key)
                            }
                            .accessibilityIdentifier("settings.cloudSignOut")
                        } else {
                            Text(L10n.cloudSignedOut.key)
                                .font(.footnote)
                                .foregroundStyle(.secondary)
                                .accessibilityIdentifier("settings.cloudSignedOut")
                            Button(L10n.cloudSignIn.key) {
                                dismiss()
                                Task {
                                    try? await Task.sleep(for: .milliseconds(350))
                                    await cloud.signIn()
                                }
                            }
                            .accessibilityIdentifier("settings.cloudSignIn")
                        }
                    }
                }

                SettingsAccountView(
                    account: account,
                    hostConnectivity: hostConnectivity,
                    showDiagnostics: $showDiagnostics
                )

                Section(L10n.hostsTitle.key) {
                    Button {
                        // Dismiss this sheet first: two sheets cannot be
                        // presented from the same root at once.
                        dismiss()
                        Task {
                            try? await Task.sleep(for: .milliseconds(350))
                            onAddHost()
                        }
                    } label: {
                        Label(L10n.settingsAddHost.key, systemImage: "plus.circle")
                    }
                    .accessibilityIdentifier("settings.addHost")
                }

            }
            .navigationTitle(L10n.settingsTitle.key)
            .inlineNavigationTitle()
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(L10n.commonDone.key) {
                        dismiss()
                    }
                    .accessibilityIdentifier("settings.done")
                }
            }
            .sheet(isPresented: $showDiagnostics) {
                DiagnosticsView(model: diagnostics)
                    .privacyShieldFromEnvironment()
            }
            .alert(
                pendingAction == .revokeDevice
                    ? L10n.settingsRevokeConfirm.string
                    : L10n.settingsForgetConfirm.string,
                isPresented: Binding(
                    get: { pendingAction != nil },
                    set: { if !$0 { settings.cancelConfirmation() } }
                )
            ) {
                Button(L10n.commonCancel.key, role: .cancel) {
                    settings.cancelConfirmation()
                }
                Button(
                    pendingAction == .revokeDevice
                        ? L10n.settingsRevoke.string
                        : L10n.settingsForget.string,
                    role: .destructive
                ) {
                    // Capture the action before the alert dismisses: dismissal
                    // clears the confirmation in the model.
                    guard let action = pendingAction else { return }
                    Task { await settings.perform(action) }
                }
                .accessibilityIdentifier("settings.confirm")
            }
        }
    }
}
