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
    let serviceSelection: ServiceSelectionContext
    /// Authoritative Host status. Before the first snapshot exists the Cloud
    /// directory's own presence hint is the only available signal.
    let connectivity: HostConnectivityModel
    /// Injected by the shell so this screen never reads the app environment.
    var onPairSelfHosted: () -> Void = {}

    var body: some View {
        NavigationStack {
            Group {
                if let account = cloud.account {
                    CloudDirectoryView(cloud: cloud, account: account, connectivity: connectivity)
                } else {
                    CloudWelcomeView(cloud: cloud, serviceSelection: serviceSelection, onPairSelfHosted: onPairSelfHosted)
                }
            }
            .navigationTitle(L10n.appName.key)
            .inlineNavigationTitle()
        }
    }
}

struct CloudWelcomeView: View {
    let cloud: CloudSignInModel
    let serviceSelection: ServiceSelectionContext
    var onPairSelfHosted: () -> Void = {}

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

                ServiceSelectionView(context: serviceSelection)

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
                        onPairSelfHosted()
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
    let connectivity: HostConnectivityModel

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
                CloudDirectoryHostRow(cloud: cloud, host: host, connectivity: connectivity)
            }
        }
    }
}
