// Cloud welcome and computer directory.
//
// The primary action opens the official hosted sign-in page in the system
// sheet. Self-hosted invitation pairing stays available as the secondary path,
// and the directory distinguishes empty, authorized, eligible, offline and
// unavailable computers. Sessions only open after Core confirms the grant.

import SwiftUI

/// Blocking sign-in progress. Signing out returns to this screen, and a completed
/// provider sheet lands here until the exchanged account is ready, so the
/// welcome page never doubles as a half-signed-in state.
struct CloudSignInProgressView: View {
    let phase: CloudSignInPhase

    var body: some View {
        VStack(spacing: 12) {
            ProgressView()
            Text(phase == .completing ? L10n.cloudCompleting.key : L10n.cloudSigningIn.key)
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("cloud.signingIn")
    }
}

struct CloudSignInRootView: View {
    let cloud: CloudSignInModel

    var body: some View {
        NavigationStack {
            Group {
                if let account = cloud.account {
                    CloudDirectoryView(cloud: cloud, account: account)
                } else {
                    CloudWelcomeView(cloud: cloud)
                }
            }
            .navigationTitle(L10n.appName.key)
            .inlineNavigationTitle()
        }
    }
}

struct CloudWelcomeView: View {
    @Environment(AppShellModel.self) private var app
    let cloud: CloudSignInModel

    var body: some View {
        if cloud.isBusy {
            CloudSignInProgressView(phase: cloud.phase)
        } else {
            welcome
        }
    }

    private var welcome: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                VStack(alignment: .leading, spacing: 8) {
                    Image(systemName: "cloud.fill")
                        .font(.system(size: 40))
                        .foregroundStyle(.tint)
                        .accessibilityHidden(true)
                    Text(L10n.cloudWelcomeTitle.key)
                        .font(.largeTitle.bold())
                        .accessibilityIdentifier("cloud.welcome.title")
                    Text(L10n.cloudWelcomeSubtitle.key)
                        .font(.body)
                        .foregroundStyle(.secondary)
                }

                ServiceSelectionView()

                if let errorKey = cloud.errorCode.map({ L10nKey.errorKey(for: $0) }) {
                    Label {
                        Text(errorKey.key)
                    } icon: {
                        Image(systemName: "exclamationmark.triangle.fill")
                    }
                    .font(.callout)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("cloud.error")
                }

                VStack(spacing: 12) {
                    Button {
                        Task { await cloud.signIn() }
                    } label: {
                        HStack {
                            Spacer()
                            if cloud.phase == .starting {
                                ProgressView().controlSize(.small)
                            }
                            Text(L10n.cloudSignIn.key)
                                .fontWeight(.semibold)
                            Spacer()
                        }
                    }
                    .glassButtonStyle(prominent: true)
                    .controlSize(.large)
                    .disabled(cloud.isBusy)
                    .accessibilityIdentifier("cloud.signIn")

                    Button {
                        app.openManualPairing()
                    } label: {
                        Label(L10n.cloudSelfHosted.key, systemImage: "desktopcomputer")
                    }
                    .glassButtonStyle()
                    .controlSize(.large)
                    .disabled(cloud.isBusy)
                    .accessibilityIdentifier("cloud.selfHosted")
                }

                Text(L10n.cloudSignInNote.key)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("cloud.signInNote")
            }
            .padding(24)
            .frame(maxWidth: 560, alignment: .leading)
            .frame(maxWidth: .infinity)
        }
    }
}

struct CloudDirectoryView: View {
    let cloud: CloudSignInModel
    let account: CloudAccount

    var body: some View {
        List {
            Section {
                VStack(alignment: .leading, spacing: 4) {
                    Text(String(format: L10n.cloudSignedInAs.string, account.email))
                        .font(.callout)
                        .accessibilityIdentifier("cloud.account")
                    Text(account.workspaceId)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                }
                if cloud.logoutPending {
                    Text(L10n.cloudSignOutPending.key)
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("cloud.logoutPending")
                }
            }

            Section(L10n.cloudDirectoryTitle.key) {
                directoryContent
            }

            if let errorKey = cloud.errorCode.map({ L10nKey.errorKey(for: $0) }) {
                Section {
                    Text(errorKey.key)
                        .font(.footnote)
                        .foregroundStyle(.red)
                        .accessibilityIdentifier("cloud.error")
                }
            }

            Section {
                Button(role: .destructive) {
                    Task { await cloud.signOut() }
                } label: {
                    Text(L10n.cloudSignOut.key)
                }
                .accessibilityIdentifier("cloud.signOut")
            }
        }
        .refreshable {
            await cloud.refreshDirectory()
        }
        .toolbar {
            ToolbarItem(placement: .topBarTrailing) {
                Button {
                    Task { await cloud.refreshDirectory() }
                } label: {
                    Image(systemName: "arrow.clockwise")
                }
                .accessibilityLabel(L10n.cloudRefresh.key)
                .accessibilityIdentifier("cloud.refresh")
            }
        }
    }

    @ViewBuilder
    private var directoryContent: some View {
        switch cloud.directory {
        case .idle, .loading:
            HStack(spacing: 8) {
                ProgressView()
                Text(L10n.commonLoading.key)
                    .foregroundStyle(.secondary)
            }
            .accessibilityIdentifier("cloud.directory.loading")
        case .empty:
            VStack(alignment: .leading, spacing: 6) {
                Text(L10n.cloudDirectoryEmptyTitle.key)
                    .font(.headline)
                Text(L10n.cloudDirectoryEmptyBody.key)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .accessibilityIdentifier("cloud.directory.empty")
        case .unavailable(let code):
            VStack(alignment: .leading, spacing: 6) {
                Text(L10n.cloudDirectoryUnavailableTitle.key)
                    .font(.headline)
                    .accessibilityIdentifier("cloud.directory.unavailable")
                Text(L10nKey.errorKey(for: code).key)
                    .font(.footnote)
                    .foregroundStyle(.secondary)
                Button(L10n.cloudRefresh.key) {
                    Task { await cloud.refreshDirectory() }
                }
                .accessibilityIdentifier("cloud.directory.retry")
            }
        case .loaded(let hosts):
            ForEach(hosts) { host in
                CloudDirectoryHostRow(cloud: cloud, host: host)
            }
        }
    }
}

private struct CloudDirectoryHostRow: View {
    let cloud: CloudSignInModel
    let host: CloudDirectoryHost

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
            return online ? .cloudHostOnline : .cloudHostOffline
        case .eligible:
            return .cloudHostEligible
        case .unavailable:
            return .cloudHostUnavailable
        }
    }
}
