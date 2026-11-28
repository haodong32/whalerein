// Fingerprint confirmation step for pairing.

import SwiftUI

struct PairingConfirmationView: View {
    @Bindable var pairing: PairingModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                Text(L10n.pairingTitle.key)
                    .font(.title2.bold())
                    .accessibilityIdentifier("pairing.title")

                Text(L10n.pairingSubtitle.key)
                    .foregroundStyle(.secondary)

                VStack(alignment: .leading, spacing: 12) {
                    labeledValue(L10n.pairingHostLabel, value: promptHostLabel)
                        .accessibilityIdentifier("pairing.host")
                    VStack(alignment: .leading, spacing: 4) {
                        Text(L10n.pairingFingerprintLabel.key)
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.secondary)
                        Text(promptFingerprint)
                            .font(.system(.footnote, design: .monospaced))
                            .textSelection(.enabled)
                            .accessibilityIdentifier("pairing.fingerprint")
                    }
                }
                .padding(16)
                .frame(maxWidth: .infinity, alignment: .leading)
                .background(Color(.secondarySystemBackground), in: RoundedRectangle(cornerRadius: 12))

                VStack(alignment: .leading, spacing: 8) {
                    Label(L10n.pairingExpiryNote.key, systemImage: "clock")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("pairing.expiryNote")
                    Label(L10n.pairingLostResultNote.key, systemImage: "arrow.uturn.backward")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("pairing.lostResultNote")
                }

                if let errorKey = pairing.errorKey {
                    Text(errorKey.key)
                        .foregroundStyle(.red)
                        .accessibilityIdentifier("pairing.error")
                }

                if let queuedKey = pairing.queuedLinkKey {
                    Text(queuedKey.key)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("pairing.linkQueued")
                }

                VStack(spacing: 12) {
                    Button {
                        Task { await pairing.confirm() }
                    } label: {
                        HStack {
                            Spacer()
                            if pairing.isBusy {
                                ProgressView().controlSize(.small)
                            }
                            Text(L10n.pairingConfirm.key).fontWeight(.semibold)
                            Spacer()
                        }
                    }
                    .glassButtonStyle(prominent: true)
                    .controlSize(.large)
                    .disabled(pairing.isBusy)
                    .accessibilityIdentifier("pairing.confirm")

                    Button(role: .destructive) {
                        Task { await pairing.decline() }
                    } label: {
                        Text(L10n.pairingDecline.key)
                            .frame(maxWidth: .infinity)
                    }
                    .glassButtonStyle()
                    .controlSize(.large)
                    .disabled(pairing.isBusy)
                    .accessibilityIdentifier("pairing.decline")
                }
            }
            .padding(24)
            .frame(maxWidth: 560, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
        .navigationTitle(L10n.pairingTitle.string)
        .inlineNavigationTitle()
    }

    private var promptHost: String {
        pairing.prompt?.hostId ?? ""
    }

    /// Directory display metadata is advisory decoration. When Access did not
    /// answer, the opaque Host id remains the only identity shown.
    private var promptHostLabel: String {
        guard let prompt = pairing.prompt else { return "" }
        let name = prompt.displayName.isEmpty ? prompt.hostId : prompt.displayName
        guard !prompt.osFamily.isEmpty else { return name }
        return "\(name) (\(prompt.osFamily))"
    }

    private var promptFingerprint: String {
        pairing.prompt?.fingerprintDisplay ?? ""
    }

    private func labeledValue(_ label: L10nKey, value: String) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(label.key)
                .font(.caption.weight(.semibold))
                .foregroundStyle(.secondary)
            Text(verbatim: value)
                .font(.body)
        }
    }
}
