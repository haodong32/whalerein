// Session switcher shared by the compact bottom sheet and the persistent
// wide-layout sidebar. Sessions are the primary content: the current Host is a
// single compact picker row, and sessions are grouped by workspace with one
// title, a running state and the last-active time per row.

import SwiftUI

/// Presentation variant for the shared switcher list. The sheet keeps a Close
/// control; the sidebar uses native sidebar list styling over a material
/// background and has no dismiss control.
enum SessionSwitcherStyle {
    case sheet
    case sidebar
}

/// Models and intents the switcher needs. The shell builds this once.
struct SessionSwitcherModels {
    let hosts: HostListModel
    /// Authoritative Host status shared with the conversation chrome.
    let connectivity: HostConnectivityModel
    let sessions: SessionListModel
    let management: SessionManagementModel
    /// The conversation currently on screen, if any.
    let currentConversation: SessionContext?
}

struct SessionSwitcherActions {
    var refresh: () async -> Void
    var selectHost: (String) async -> Void
    var selectSession: (SessionSummary) -> Void
    var openConversation: (String, SessionSummary) -> Void
    var addHost: () -> Void
    var close: () -> Void
}

struct SessionSwitcherView: View {
    let models: SessionSwitcherModels
    let actions: SessionSwitcherActions

    var body: some View {
        NavigationStack {
            SessionSwitcherContent(models: models, actions: actions, style: .sheet)
                .navigationTitle(L10n.switcherTitle.key)
                .inlineNavigationTitle()
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button(L10n.commonClose.key) {
                            actions.close()
                        }
                        .accessibilityIdentifier("switcher.close")
                    }
                }
        }
        .presentationDetents([.medium, .large])
        .presentationDragIndicator(.visible)
        .presentationContentInteraction(.scrolls)
    }
}

/// Persistent left sidebar for iPhone landscape, iPad and wide Mac Catalyst
/// windows. It uses the same list content and model actions as the sheet.
struct SessionSwitcherSidebar: View {
    let models: SessionSwitcherModels
    let actions: SessionSwitcherActions

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text(L10n.switcherTitle.key)
                    .font(.title3.weight(.semibold))
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 20)
            .padding(.top, 8)
            .padding(.bottom, 4)

            SessionSwitcherContent(models: models, actions: actions, style: .sidebar)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(.regularMaterial)
        .accessibilityIdentifier("session.sidebar")
    }
}

/// Reusable Host/session list. Both presentations use the same actions, states
/// and accessibility identifiers.
struct SessionSwitcherContent: View {
    let models: SessionSwitcherModels
    let actions: SessionSwitcherActions
    var style: SessionSwitcherStyle = .sheet

    @State private var managementSheet: SessionManagementSheet?

    private var showsWorkspaceHeaders: Bool {
        models.sessions.hasMultipleWorkspaces
    }

    var body: some View {
        list
            .modifier(SwitcherListStyle(style: style))
            .refreshable {
                await actions.refresh()
                await loadManagementSupport()
            }
            .task {
                await actions.refresh()
                await loadManagementSupport()
            }
            .sheet(item: $managementSheet) { sheet in
                switch sheet {
                case .create(let hostId):
                    SessionCreateView(
                        hostId: hostId,
                        management: models.management,
                        openConversation: actions.openConversation,
                        refresh: actions.refresh
                    )
                case .details(let hostId, let session, let cached):
                    SessionDetailsView(
                        hostId: hostId,
                        session: session,
                        cached: cached,
                        management: models.management,
                        openConversation: actions.openConversation,
                        refresh: actions.refresh
                    )
                case .fork(let hostId, let session):
                    SessionForkView(
                        hostId: hostId,
                        session: session,
                        management: models.management,
                        openConversation: actions.openConversation,
                        refresh: actions.refresh
                    )
                }
            }
    }

    private var list: some View {
        List {
            SessionSwitcherHostSection(
                models: models,
                actions: actions,
                managementSheet: $managementSheet
            )
            sessionSections
        }
        .listSectionSpacing(.compact)
    }

    private func loadManagementSupport() async {
        guard let host = models.currentHost else { return }
        await models.management.loadCreateSupport(hostId: host.hostId)
    }
    // MARK: - Sessions

    @ViewBuilder
    private var sessionSections: some View {
        if models.sessions.isShowingCached {
            Section {
                Label(L10n.sessionsCached.key, systemImage: "internaldrive")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("sessions.cached")
            }
        }

        switch models.sessions.loadState {
        case .idle, .loading:
            Section {
                HStack(spacing: 10) {
                    ProgressView()
                    Text(L10n.sessionsLoading.key)
                        .foregroundStyle(.secondary)
                }
                .accessibilityIdentifier("sessions.loading")
            }
        case .failed(let code):
            Section {
                VStack(alignment: .leading, spacing: 8) {
                    Text(L10n.sessionsError.key)
                        .font(.headline)
                        .accessibilityIdentifier("sessions.error")
                    Text(L10nKey.errorKey(for: code).key)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                    Button(L10n.commonRetry.key) {
                        Task { await actions.refresh() }
                    }
                    .accessibilityIdentifier("sessions.retry")
                }
            }
        case .loaded:
            archivedToggle
            if models.sessions.visibleSessions.isEmpty {
                Section {
                    Text(L10n.sessionsEmpty.key)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("sessions.empty")
                }
            } else if showsWorkspaceHeaders {
                ForEach(models.sessions.workspaceGroups) { group in
                    Section {
                        sessionRows(in: group)
                    } header: {
                        Label {
                            Text(verbatim: group.name)
                        } icon: {
                            Image(systemName: "folder")
                        }
                        .accessibilityLabel(Text(workspaceAccessibilityLabel(group)))
                    }
                }
            } else {
                Section {
                    sessionRows(in: models.sessions.workspaceGroups.first)
                } header: {
                    Text(L10n.sessionsTitle.key)
                }
            }
        }
    }

    @ViewBuilder
    private var archivedToggle: some View {
        if models.sessions.archivedCount > 0 || models.sessions.showsArchived {
            Section {
                Toggle(isOn: Binding(
                    get: { models.sessions.showsArchived },
                    set: { models.sessions.setShowsArchived($0) }
                )) {
                    Label(L10n.sessionsShowArchived.key, systemImage: "archivebox")
                }
                .accessibilityIdentifier("sessions.archivedToggle")
            }
        }
    }

    @ViewBuilder
    private func sessionRows(in group: WorkspaceGroup?) -> some View {
        if let group {
            ForEach(group.sessions) { session in
                row(for: session)
            }
        }
    }

    private func row(for session: SessionSummary) -> some View {
        let hostId = models.sessions.hostId
        let cached = models.sessions.isShowingCached
        return HStack(spacing: 8) {
            Button {
                actions.selectSession(session)
            } label: {
                SwitcherSessionRow(
                    session: session,
                    hostId: hostId,
                    isCurrent: models.currentConversation?.hostId == hostId
                        && models.currentConversation?.sessionId == session.sessionId
                )
            }
            .accessibilityIdentifier("session.row.\(session.sessionId)")

            Menu {
                Button {
                    managementSheet = .details(
                        hostId: hostId,
                        session: session,
                        cached: cached
                    )
                } label: {
                    Label(
                        L10nKey.sessionsDetailsTitle.string,
                        systemImage: "info.circle"
                    )
                }
                .accessibilityIdentifier("session.details.\(session.sessionId)")

                Button {
                    managementSheet = .fork(hostId: hostId, session: session)
                } label: {
                    Label(
                        L10nKey.sessionsForkAction.string,
                        systemImage: "arrow.triangle.branch"
                    )
                }
                .accessibilityIdentifier("session.fork.\(session.sessionId)")
            } label: {
                Image(systemName: "ellipsis.circle")
                    .font(.body)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel(Text(L10nKey.sessionsForkAction.key))
            }
            .accessibilityIdentifier("session.menu.\(session.sessionId)")
        }
    }

    private func workspaceAccessibilityLabel(_ group: WorkspaceGroup) -> String {
        guard let path = group.path else { return group.name }
        return "\(group.name), \(path)"
    }
}
