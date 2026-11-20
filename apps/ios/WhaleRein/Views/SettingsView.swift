// Settings and about: localized versions, language note, staging/fixture
// indicators, data location, and confirmed forget/revoke actions.

import SwiftUI

struct SettingsView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss
    @State private var confirmation: SettingsModel.Confirmation?

    var body: some View {
        NavigationStack {
            Form {
                Section(L10n.settingsAbout.key) {
                    LabeledContent(L10n.settingsCoreRevision.key) {
                        Text(verbatim: app.settings.coreRevision)
                            .font(.footnote.monospaced())
                            .accessibilityIdentifier("settings.coreRevision")
                    }
                    LabeledContent(L10n.settingsAppVersion.key) {
                        Text(verbatim: app.settings.appVersion)
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

                Section(L10n.settingsAppSection.key) {
                    Toggle(
                        L10n.settingsReasoning.key,
                        isOn: Binding(
                            get: { app.reasoningVisible },
                            set: { _ = app.setReasoningVisible($0) }
                        )
                    )
                    .accessibilityIdentifier("settings.reasoning")
                    Text(L10n.settingsReasoningNote.key)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                }

                Section(L10n.settingsData.key) {
                    VStack(alignment: .leading, spacing: 4) {
                        Text(L10n.settingsStorage.key)
                        Text(verbatim: app.dataDirectory.path)
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
                    if app.settings.isStaging {
                        Label(L10n.settingsStaging.key, systemImage: "flag")
                            .accessibilityIdentifier("settings.staging")
                    }
                    if app.settings.isFixtureBacked {
                        Label(L10n.settingsFixtureData.key, systemImage: "shippingbox")
                            .accessibilityIdentifier("settings.fixtureData")
                    }
                    if let statusKey = app.settings.statusKey {
                        Text(statusKey.key)
                            .font(.footnote)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("settings.status")
                    }
                    if let errorKey = app.settings.errorKey {
                        Text(errorKey.key)
                            .font(.footnote)
                            .foregroundStyle(.red)
                            .accessibilityIdentifier("settings.error")
                    }
                }

                SettingsOperationsView()

                Section {
                    ServiceSelectionView(showsStandaloneCA: true)
                }

                Section(L10n.hostsTitle.key) {
                    Button {
                        // Dismiss this sheet first: two sheets cannot be
                        // presented from the same root at once.
                        dismiss()
                        Task {
                            try? await Task.sleep(for: .milliseconds(350))
                            app.openManualPairing()
                        }
                    } label: {
                        Label(L10n.settingsAddHost.key, systemImage: "plus.circle")
                    }
                    .accessibilityIdentifier("settings.addHost")
                }

                Section {
                    Button(role: .destructive) {
                        confirmation = .forget
                    } label: {
                        Text(L10n.settingsForget.key)
                    }
                    .disabled(app.settings.isBusy)
                    .accessibilityIdentifier("settings.forget")

                    Button(role: .destructive) {
                        confirmation = .revoke
                    } label: {
                        Text(L10n.settingsRevoke.key)
                    }
                    .disabled(app.settings.isBusy)
                    .accessibilityIdentifier("settings.revoke")
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
            .alert(
                confirmation == .revoke ? L10n.settingsRevokeConfirm.string : L10n.settingsForgetConfirm.string,
                isPresented: Binding(
                    get: { confirmation != nil },
                    set: { if !$0 { confirmation = nil } }
                )
            ) {
                Button(L10n.commonCancel.key, role: .cancel) {
                    confirmation = nil
                }
                Button(confirmation == .revoke ? L10n.settingsRevoke.string : L10n.settingsForget.string, role: .destructive) {
                    guard let confirmation else { return }
                    self.confirmation = nil
                    Task { await app.settings.perform(confirmation) }
                }
                .accessibilityIdentifier("settings.confirm")
            }
        }
    }
}
