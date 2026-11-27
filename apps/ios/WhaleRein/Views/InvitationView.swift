// Invitation entry: open a link or paste it, localized validation errors, and
// the explicit Host fingerprint confirmation step.

import SwiftUI

struct InvitationView: View {
    let pairing: PairingModel
    let serviceSelection: ServiceSelectionContext
    var isModal = false
    var onDismiss: () -> Void = {}
    @Environment(\.dismiss) private var dismiss

    @State private var pasteboardError: L10nKey?
    @FocusState private var fieldFocused: Bool

    var body: some View {
        @Bindable var pairing = pairing
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                header

                ServiceSelectionView(context: serviceSelection)

                VStack(alignment: .leading, spacing: 8) {
                    Text(L10n.invitationFieldPlaceholder.key)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.secondary)
                    TextField(
                        L10n.invitationFieldPlaceholder.key,
                        text: Binding(
                            get: { pairing.invitationText },
                            set: { pairing.updateInvitation($0) }
                        )
                    )
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .keyboardType(.URL)
                    .submitLabel(.go)
                    .onSubmit {
                        Task { await pairing.submit() }
                    }
                    .focused($fieldFocused)
                    .padding(12)
                    .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))
                    .accessibilityIdentifier("invitation.field")
                    .accessibilityLabel(L10n.invitationFieldPlaceholder.key)
                }

                if let errorKey = effectiveErrorKey {
                    Label {
                        Text(errorKey.key)
                    } icon: {
                        Image(systemName: "exclamationmark.triangle.fill")
                    }
                    .font(.callout)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("invitation.error")
                }

                if case .declined = pairing.phase {
                    statusText(L10n.pairingDeclined)
                    statusText(L10n.pairingUnpairedAfterFailure)
                }

                if let queuedKey = pairing.queuedLinkKey {
                    Text(queuedKey.key)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("pairing.linkQueued")
                }

                if pairing.canResume {
                    Button {
                        Task { await pairing.resume() }
                    } label: {
                        Label(L10n.pairingResume.key, systemImage: "arrow.clockwise")
                    }
                    .glassButtonStyle()
                    .accessibilityIdentifier("invitation.resume")
                }

                VStack(spacing: 12) {
                    Button {
                        Task { await pairing.submit() }
                    } label: {
                        HStack {
                            Spacer()
                            if pairing.isBusy {
                                ProgressView()
                                    .controlSize(.small)
                            }
                            Text(L10n.invitationContinue.key)
                                .fontWeight(.semibold)
                            Spacer()
                        }
                    }
                    .glassButtonStyle(prominent: true)
                    .controlSize(.large)
                    .disabled(pairing.isBusy || pairing.invitationText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                    .accessibilityIdentifier("invitation.continue")

                    Button {
                        pasteFromClipboard()
                    } label: {
                        Label(L10n.invitationPaste.key, systemImage: "doc.on.clipboard")
                    }
                    .glassButtonStyle()
                    .controlSize(.large)
                    .disabled(pairing.isBusy)
                    .accessibilityIdentifier("invitation.paste")
                }

                Text(L10n.invitationHint.key)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("invitation.hint")

                if case .validating = pairing.phase {
                    HStack(spacing: 8) {
                        ProgressView()
                        Text(L10n.invitationValidating.key)
                            .foregroundStyle(.secondary)
                    }
                    .accessibilityIdentifier("invitation.validating")
                }
            }
            .padding(24)
            .frame(maxWidth: 560, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
        .scrollDismissesKeyboard(.interactively)
        .navigationTitle(isModal ? L10n.hostsSwitchTitle.string : L10n.appName.string)
        .inlineNavigationTitle()
        .toolbar {
            if isModal {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L10n.commonClose.key) {
                        onDismiss()
                        dismiss()
                    }
                    .accessibilityIdentifier("invitation.close")
                }
            }
        }
        .sheet(
            isPresented: Binding(
                get: { pairing.prompt != nil },
                set: { presented in
                    if !presented, pairing.prompt != nil {
                        Task { await pairing.cancel() }
                    }
                }
            )
        ) {
            NavigationStack {
                PairingConfirmationView(pairing: pairing)
            }
            .privacyShieldFromEnvironment()
            .interactiveDismissDisabled()
        }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 8) {
            Image(systemName: "iphone.gen3.radiowaves.left.and.right")
                .font(.system(size: 40))
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            Text(L10n.invitationTitle.key)
                .font(.largeTitle.bold())
                .accessibilityIdentifier("invitation.title")
            Text(L10n.invitationSubtitle.key)
                .font(.body)
                .foregroundStyle(.secondary)
        }
    }

    private var effectiveErrorKey: L10nKey? {
        pasteboardError ?? pairing.errorKey
    }

    private func statusText(_ key: L10nKey) -> some View {
        Text(key.key)
            .font(.callout)
            .foregroundStyle(.secondary)
    }

    private func pasteFromClipboard() {
        pasteboardError = nil
        guard let value = PlatformPasteboard.string,
              !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            pasteboardError = .invitationPastedNothing
            return
        }
        pairing.paste(value)
    }
}


#if DEBUG
#Preview("Invitation fixture") {
    if let scenario = ScenarioCatalog.scenario(id: "onboarding.unpaired") {
        let directory = ScenarioBuilder.temporaryDirectory()
        let service = ScenarioBuilder.service(for: scenario, directory: directory)
        InvitationView(
            pairing: PairingModel(service: service),
            serviceSelection: ServiceSelectionContext(
                configuration: nil,
                activeInstance: nil,
                actions: ServiceSelectionActions(
                    switchToService: { _ in nil },
                    enableLocal: { _ in nil },
                    removeLocalCA: { nil },
                    importStandaloneCA: { _ in nil },
                    removeStandaloneCA: { nil }
                )
            )
        )
    }
}
#endif
