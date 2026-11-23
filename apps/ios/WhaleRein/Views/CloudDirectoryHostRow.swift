// One computer in the Cloud directory. The Host connection is authoritative
// once it has a snapshot; Cloud presence is only the pre-probe fallback.

import SwiftUI

struct CloudDirectoryHostRow: View {
    let cloud: CloudSignInModel
    let host: CloudDirectoryHost
    let connectivity: HostConnectivityModel

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: "desktopcomputer")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Text(host.displayName)
                    .font(.body.weight(.medium))
                Text(stateKey.key)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            action
        }
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("cloud.host.\(host.hostId)")
    }

    @ViewBuilder
    private var action: some View {
        switch host.access {
        case .authorized:
            Button(L10n.cloudHostOpen.key) {
                cloud.openHost(host)
            }
            .buttonStyle(.bordered)
            .accessibilityIdentifier("cloud.host.open.\(host.hostId)")
        case .eligible:
            Button {
                Task { await cloud.connect(host) }
            } label: {
                if cloud.isConnectingHostId == host.hostId {
                    ProgressView().controlSize(.small)
                } else {
                    Text(L10n.cloudHostConnect.key)
                }
            }
            .buttonStyle(.borderedProminent)
            .disabled(cloud.isConnectingHostId != nil)
            .accessibilityIdentifier("cloud.host.connect.\(host.hostId)")
        case .unavailable:
            Text(L10n.cloudHostUnavailable.key)
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("cloud.host.unavailable.\(host.hostId)")
        }
    }

    private var stateKey: L10nKey {
        switch host.access {
        case .authorized(let online):
            // Once the Host connection has a snapshot, it is authoritative and
            // Cloud presence never overrides it.
            if connectivity.hasSnapshot(host.hostId) {
                switch connectivity.status(for: host.hostId) {
                case .connected, .degraded:
                    return .cloudHostOnline
                case .unknown, .connecting:
                    return .conversationConnectionConnecting
                case .reconnecting:
                    return .conversationConnectionReconnecting
                case .disconnected, .offline:
                    return .cloudHostOffline
                case .unavailable(let code):
                    return L10nKey.hostConnectionErrorKey(for: code)
                }
            }
            return online ? .cloudHostOnline : .cloudHostOffline
        case .eligible:
            return .cloudHostEligible
        case .unavailable:
            return .cloudHostUnavailable
        }
    }
}
