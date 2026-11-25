// One computer in the Cloud directory. The Host connection is authoritative
// once it has a snapshot; Cloud presence is only the pre-probe fallback.

import SwiftUI

struct CloudDirectoryHostRow: View {
    let cloud: CloudSignInModel
    let host: CloudDirectoryHost
    let hostIdLabel: String
    let connectivity: HostConnectivityModel
    @State private var showsHostID = false

    var body: some View {
        HStack(alignment: .center, spacing: 12) {
            Image(systemName: "desktopcomputer")
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 3) {
                Button {
                    showsHostID = true
                } label: {
                    VStack(alignment: .leading, spacing: 3) {
                        Text(verbatim: host.displayName.isEmpty ? host.hostId : host.displayName)
                            .font(.body.weight(.medium))
                        HStack(alignment: .firstTextBaseline, spacing: 4) {
                            Text(verbatim: hostIdLabel)
                                .font(.caption.monospaced())
                            Image(systemName: "info.circle")
                                .font(.caption)
                                .accessibilityHidden(true)
                        }
                        .foregroundStyle(.secondary)
                    }
                    .multilineTextAlignment(.leading)
                }
                .buttonStyle(.plain)
                .accessibilityHint(Text(L10n.cloudHostIDDetails.key))
                .accessibilityIdentifier("cloud.host.details.\(host.hostId)")
                Text(stateKey.key)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Spacer()
            action
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("cloud.host.\(host.hostId)")
        .alert(L10n.cloudHostID.key, isPresented: $showsHostID) {
            Button(L10n.cloudHostIDCopy.key) {
                PlatformPasteboard.setString(host.hostId)
            }
            Button(L10n.commonClose.key, role: .cancel) {}
        } message: {
            Text(verbatim: host.hostId)
        }
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
            EmptyView()
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
