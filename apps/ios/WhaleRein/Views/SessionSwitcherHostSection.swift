// Host picker and create entry for the session switcher.

import SwiftUI

struct SessionSwitcherHostSection: View {
    let models: SessionSwitcherModels
    let actions: SessionSwitcherActions
    @Binding var managementSheet: SessionManagementSheet?


    @ViewBuilder
    var body: some View {
        createSection
        hostPickerSection
    }

    @ViewBuilder
    private var createSection: some View {
        if let host = currentHost,
           models.management.canCreate(hostId: host.hostId) {
            Section {
                Button {
                    managementSheet = .create(hostId: host.hostId)
                } label: {
                    Label(
                        L10nKey.sessionsCreateTitle.string,
                        systemImage: "plus.circle"
                    )
                }
                .accessibilityIdentifier("sessions.create")
            }
        }
    }

    // MARK: - Host picker

    private var hostPickerSection: some View {
        Section {
            if models.hosts.hosts.isEmpty {
                switch models.hosts.loadState {
                case .idle, .loading:
                    HStack(spacing: 10) {
                        ProgressView()
                        Text(L10n.commonLoading.key)
                            .foregroundStyle(.secondary)
                    }
                    .accessibilityIdentifier("hosts.loading")
                case .failed(let code):
                    VStack(alignment: .leading, spacing: 8) {
                        Text(L10n.hostsLoadError.key)
                            .font(.headline)
                        Text(L10nKey.errorKey(for: code).key)
                            .font(.callout)
                            .foregroundStyle(.secondary)
                        Button(L10n.commonRetry.key) {
                            Task { await actions.refresh() }
                        }
                        .accessibilityIdentifier("hosts.retry")
                    }
                case .loaded:
                    Text(L10n.hostsEmpty.key)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("hosts.empty")
                }
            } else {
                hostPicker
            }
        } footer: {
            Button {
                // Replace the switcher sheet with the pairing sheet.
                actions.close()
                Task {
                    try? await Task.sleep(for: .milliseconds(350))
                    actions.addHost()
                }
            } label: {
                Label(L10n.hostsAdd.key, systemImage: "plus.circle")
            }
            .accessibilityIdentifier("switcher.addHost")
        }
    }

    private var hostPicker: some View {
        Menu {
            ForEach(models.hosts.hosts) { host in
                Button {
                    Task { await actions.selectHost(host.hostId) }
                } label: {
                    if models.hosts.selectedHostId == host.hostId {
                        Label(host.hostId, systemImage: "checkmark")
                    } else {
                        Text(verbatim: host.hostId)
                    }
                }
                .accessibilityIdentifier("host.row.\(host.hostId)")
            }
        } label: {
            HStack(spacing: 10) {
                Image(systemName: currentHost?.online == true ? "desktopcomputer" : "wifi.slash")
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .accessibilityHidden(true)
                Text(verbatim: currentHost?.hostId ?? L10n.hostsPicker.string)
                    .font(.body)
                    .foregroundStyle(.primary)
                Spacer(minLength: 8)
                Text(hostStatusText)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Image(systemName: "chevron.up.chevron.down")
                    .font(.caption2)
                    .foregroundStyle(.tertiary)
                    .accessibilityHidden(true)
            }
            .contentShape(Rectangle())
            .foregroundStyle(.primary)
        }
        .tint(.primary)
        .accessibilityIdentifier("host.picker")
        .accessibilityLabel(Text(L10n.hostsPicker.key))
        .accessibilityValue(Text(hostPickerAccessibilityValue))
    }

    private var currentHost: HostSummary? {
        models.hosts.hosts.first { $0.hostId == models.hosts.selectedHostId }
            ?? models.hosts.hosts.first(where: \.online)
            ?? models.hosts.hosts.first
    }

    private var hostStatusText: String {
        guard let host = currentHost else { return "" }
        return host.online ? L10n.hostsOnline.string : L10n.hostsOffline.string
    }

    private var hostPickerAccessibilityValue: String {
        guard let host = currentHost else { return "" }
        var parts = [host.hostId, host.online ? L10n.hostsOnline.string : L10n.hostsOffline.string]
        if models.hosts.selectedHostId == host.hostId {
            parts.append(L10n.hostsSelected.string)
        }
        return parts.joined(separator: ", ")
    }
}
