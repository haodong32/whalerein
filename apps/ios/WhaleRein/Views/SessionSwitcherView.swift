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

struct SessionSwitcherView: View {
    @Environment(AppShellModel.self) private var app

    var body: some View {
        NavigationStack {
            SessionSwitcherContent(style: .sheet)
                .navigationTitle(L10n.switcherTitle.key)
                .inlineNavigationTitle()
                .toolbar {
                    ToolbarItem(placement: .cancellationAction) {
                        Button(L10n.commonClose.key) {
                            app.dismissSheet()
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

            SessionSwitcherContent(style: .sidebar)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(.regularMaterial)
        .accessibilityIdentifier("session.sidebar")
    }
}

/// Reusable Host/session list. Both presentations use the same actions, states
/// and accessibility identifiers.
struct SessionSwitcherContent: View {
    @Environment(AppShellModel.self) private var app
    var style: SessionSwitcherStyle = .sheet

    @State private var managementSheet: SessionManagementSheet?

    private var showsWorkspaceHeaders: Bool {
        app.sessionList.hasMultipleWorkspaces
    }

    var body: some View {
        list
            .modifier(SwitcherListStyle(style: style))
            .refreshable {
                await app.refreshSwitcher()
                await loadManagementSupport()
            }
            .task {
                await app.refreshSwitcher()
                await loadManagementSupport()
            }
            .sheet(item: $managementSheet) { sheet in
                switch sheet {
                case .create(let hostId):
                    SessionCreateView(hostId: hostId)
                case .details(let hostId, let session, let cached):
                    SessionDetailsView(
                        hostId: hostId,
                        session: session,
                        cached: cached
                    )
                case .fork(let hostId, let session):
                    SessionForkView(hostId: hostId, session: session)
                }
            }
    }

    private var list: some View {
        List {
            hostPickerSection
            createSection
            sessionSections
        }
        .listSectionSpacing(.compact)
    }

    private func loadManagementSupport() async {
        guard let host = currentHost else { return }
        await app.sessionManagement.loadCreateSupport(hostId: host.hostId)
    }

    /// Create is offered only for a Host that advertises native creation. The
    /// capability is probed, never assumed.
    @ViewBuilder
    private var createSection: some View {
        if let host = currentHost,
           app.sessionManagement.canCreate(hostId: host.hostId) {
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
            if app.hostList.hosts.isEmpty {
                switch app.hostList.loadState {
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
                            Task { await app.refreshSwitcher() }
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
                app.dismissSheet()
                Task {
                    try? await Task.sleep(for: .milliseconds(350))
                    app.openManualPairing()
                }
            } label: {
                Label(L10n.hostsAdd.key, systemImage: "plus.circle")
            }
            .accessibilityIdentifier("switcher.addHost")
        }
    }

    private var hostPicker: some View {
        Menu {
            ForEach(app.hostList.hosts) { host in
                Button {
                    Task { await app.selectHostInSwitcher(host.hostId) }
                } label: {
                    if app.selectedHostId == host.hostId {
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
        app.hostList.hosts.first { $0.hostId == app.selectedHostId }
            ?? app.hostList.hosts.first(where: \.online)
            ?? app.hostList.hosts.first
    }

    private var hostStatusText: String {
        guard let host = currentHost else { return "" }
        return host.online ? L10n.hostsOnline.string : L10n.hostsOffline.string
    }

    private var hostPickerAccessibilityValue: String {
        guard let host = currentHost else { return "" }
        var parts = [host.hostId, host.online ? L10n.hostsOnline.string : L10n.hostsOffline.string]
        if app.selectedHostId == host.hostId {
            parts.append(L10n.hostsSelected.string)
        }
        return parts.joined(separator: ", ")
    }

    // MARK: - Sessions

    @ViewBuilder
    private var sessionSections: some View {
        if app.sessionList.isShowingCached {
            Section {
                Label(L10n.sessionsCached.key, systemImage: "internaldrive")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("sessions.cached")
            }
        }

        switch app.sessionList.loadState {
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
                        Task { await app.refreshSwitcher() }
                    }
                    .accessibilityIdentifier("sessions.retry")
                }
            }
        case .loaded where app.sessionList.sessions.isEmpty:
            Section {
                Text(L10n.sessionsEmpty.key)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("sessions.empty")
            }
        case .loaded:
            if showsWorkspaceHeaders {
                ForEach(app.sessionList.workspaceGroups) { group in
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
                    sessionRows(in: app.sessionList.workspaceGroups.first)
                } header: {
                    Text(L10n.sessionsTitle.key)
                }
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
        let hostId = app.sessionList.hostId
        let cached = app.sessionList.isShowingCached
        return HStack(spacing: 8) {
            Button {
                app.selectSessionInSwitcher(session)
            } label: {
                SwitcherSessionRow(
                    session: session,
                    hostId: hostId,
                    isCurrent: app.conversation?.hostId == hostId
                        && app.conversation?.sessionId == session.sessionId
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

private struct SwitcherListStyle: ViewModifier {
    let style: SessionSwitcherStyle

    func body(content: Content) -> some View {
        switch style {
        case .sheet:
            // Plain lists are the dense list style; inset grouped lists are
            // meant for forms and add ~20 pt of chrome per section.
            content.listStyle(.plain)
        case .sidebar:
            content
                .listStyle(.sidebar)
                .scrollContentBackground(.hidden)
        }
    }
}

/// Single-line session row: running state, title and last-active time. The
/// workspace lives in the section header instead of a per-row path.
private struct SwitcherSessionRow: View {
    let session: SessionSummary
    let hostId: String
    let isCurrent: Bool

    var body: some View {
        HStack(spacing: 8) {
            if session.running {
                Image(systemName: "waveform")
                    .font(.caption)
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
            }
            Text(verbatim: session.title)
                .font(.body)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 8)
            SessionActivityText(date: session.updatedAt)
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityHidden(true)
            if isCurrent {
                Image(systemName: "checkmark")
                    .font(.footnote.weight(.semibold))
                    .foregroundStyle(.tint)
                    .accessibilityHidden(true)
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(accessibilityLabel))
        .accessibilityAddTraits(isCurrent ? [.isSelected] : [])
    }

    private var statusText: String {
        session.running ? L10n.sessionsRunning.string : L10n.sessionsStatusIdle.string
    }

    private var accessibilityLabel: String {
        SessionAccessibility.rowLabel(
            title: session.title,
            status: statusText,
            activity: SessionActivityText.plainText(for: session.updatedAt),
            hostId: hostId,
            isCurrent: isCurrent,
            selectedText: L10n.hostsSelected.string
        )
    }
}

/// System relative time for recent activity, switching to a short absolute date
/// once a relative value stops being useful.
struct SessionActivityText: View {
    let date: Date
    private let now = Date()

    var body: some View {
        if Self.isOldEnough(date, now: now) {
            Text(date, format: .dateTime.month(.abbreviated).day())
        } else {
            Text(date, style: .relative)
        }
    }

    static func plainText(for date: Date, now: Date = Date()) -> String {
        if isOldEnough(date, now: now) {
            return date.formatted(.dateTime.month(.abbreviated).day())
        }
        return date.formatted(.relative(presentation: .named))
    }

    static func isOldEnough(_ date: Date, now: Date) -> Bool {
        let seconds = now.timeIntervalSince(date)
        return seconds > 7 * 24 * 60 * 60
    }
}
