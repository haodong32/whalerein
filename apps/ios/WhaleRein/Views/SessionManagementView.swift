// Create, details, fork and model sheets for session management. Each sheet is
// scoped to one Host/session pair from the switcher; mutation results are shown
// from Core's typed receipt and a confirmed identity is never replaced by a
// later read failure.

import SwiftUI

/// One management sheet. The scope is captured when the user opens the sheet so
/// a Host switch cannot retarget an in-flight request.
enum SessionManagementSheet: Identifiable {
    case create(hostId: String)
    case details(hostId: String, session: SessionSummary, cached: Bool)
    case fork(hostId: String, session: SessionSummary)

    var id: String {
        switch self {
        case .create(let hostId):
            return "create/\(hostId)"
        case .details(let hostId, let session, _):
            return "details/\(hostId)/\(session.sessionId)"
        case .fork(let hostId, let session):
            return "fork/\(hostId)/\(session.sessionId)"
        }
    }
}

// MARK: - Create

struct SessionCreateView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    let hostId: String

    private var management: SessionManagementModel { app.sessionManagement }

    var body: some View {
        @Bindable var management = app.sessionManagement
        NavigationStack {
            Form {
                Section {
                    Toggle(
                        L10nKey.sessionsCreateUseDefaults.key,
                        isOn: $management.createForm.useDefaults
                    )
                    .accessibilityIdentifier("create.useDefaults")
                }

                if !management.createForm.useDefaults {
                    Section {
                        TextField(
                            L10nKey.sessionsCreateWorkspace.key,
                            text: $management.createForm.workspaceId
                        )
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .accessibilityIdentifier("create.workspace")
                        TextField(
                            L10nKey.sessionsCreateHostPath.key,
                            text: $management.createForm.cwd
                        )
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .accessibilityIdentifier("create.cwd")
                        TextField(
                            L10nKey.sessionsCreatePreset.key,
                            text: $management.createForm.agentPreset
                        )
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .accessibilityIdentifier("create.preset")
                    } footer: {
                        Text(L10nKey.sessionsCreateLocationConflict.key)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }

                    Section {
                        TextField(
                            L10nKey.sessionsCreatePreferredId.key,
                            text: $management.createForm.preferredSessionId
                        )
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .accessibilityIdentifier("create.preferredId")
                    }
                }

                validationSection
                supportSection
                resultSection
            }
            .navigationTitle(L10nKey.sessionsCreateTitle.string)
            .inlineNavigationTitle()
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button(L10nKey.commonCancel.key) { dismiss() }
                        .accessibilityIdentifier("create.cancel")
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button(L10nKey.sessionsCreateAction.key) {
                        Task { await submit() }
                    }
                    .disabled(!canSubmit)
                    .accessibilityIdentifier("create.submit")
                }
            }
            .task { await management.loadCreateSupport(hostId: hostId) }
        }
    }

    private var canSubmit: Bool {
        management.createForm.validation == nil
            && management.canCreate(hostId: hostId)
            && management.createState != .creating
    }

    @ViewBuilder
    private var validationSection: some View {
        if let validation = management.createForm.validation {
            Section {
                Label(validation.messageKey.string, systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("create.validation")
            }
        }
        if case .rejected(let validation) = management.createState {
            Section {
                Label(validation.messageKey.string, systemImage: "exclamationmark.triangle")
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("create.rejected")
            }
        }
    }

    @ViewBuilder
    private var supportSection: some View {
        if !management.canCreate(hostId: hostId) {
            Section {
                if management.createSupportKnown(hostId: hostId) {
                    Text(L10nKey.sessionsCreateUnsupported.string)
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("create.unsupported")
                } else {
                    HStack(spacing: 8) {
                        ProgressView()
                        Text(L10nKey.commonLoading.key)
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var resultSection: some View {
        switch management.createState {
        case .idle, .creating:
            if management.createState == .creating {
                Section {
                    HStack(spacing: 8) {
                        ProgressView()
                        Text(L10nKey.commonLoading.key)
                    }
                }
            }
        case .failed(let code):
            Section {
                Text(L10nKey.errorKey(for: code).key)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("create.error")
                Button(L10nKey.commonRetry.key) {
                    Task { await submit() }
                }
                .accessibilityIdentifier("create.retry")
            }
        case .rejected:
            EmptyView()
        case .result(let outcome):
            Section {
                MutationReceiptView(receipt: outcome.receipt)
                if let sessionId = outcome.sessionId {
                    LabeledContent(L10nKey.sessionsCreateReturnedId.key) {
                        Text(verbatim: sessionId)
                    }
                    .accessibilityIdentifier("create.returnedId")
                }
                if let detailError = outcome.detailError {
                    Text(L10nKey.errorKey(for: detailError).key)
                        .foregroundStyle(.secondary)
                }
                if let summary = outcome.openableSummary {
                    Button(L10nKey.sessionsOpen.key) {
                        open(summary)
                    }
                    .accessibilityIdentifier("create.open")
                } else if outcome.receipt.status == .uncertain {
                    // Recovery uses the original command's reconcile key; it
                    // never submits a second create.
                    if let lookupKey = outcome.receipt.reconcileKey,
                       !lookupKey.isEmpty {
                        Button(L10nKey.sessionsReceiptLookup.string) {
                            Task { await lookUp(create: outcome, key: lookupKey) }
                        }
                        .accessibilityIdentifier("create.lookup")
                    }
                } else if outcome.receipt.retryable {
                    Button(L10nKey.commonRetry.key) {
                        Task { await submit() }
                    }
                    .accessibilityIdentifier("create.retry")
                }
            }
        }
    }

    private func submit() async {
        let outcome = await management.submitCreate(hostId: hostId)
        guard let summary = outcome?.openableSummary else { return }
        open(summary)
    }

    /// Looks the uncertain create up under the identity the Host asked the
    /// client to reconcile with. A miss leaves the receipt intact.
    private func lookUp(create outcome: SessionCreateOutcome, key: String) async {
        guard let detail = await management.refreshDetails(hostId: hostId, sessionId: key) else {
            return
        }
        open(detail.summary)
    }

    private func open(_ summary: SessionSummary) {
        app.openConversation(hostId: hostId, session: summary)
        dismiss()
    }
}

// MARK: - Details

struct SessionDetailsView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    let hostId: String
    let session: SessionSummary
    let cached: Bool

    private var management: SessionManagementModel { app.sessionManagement }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    LabeledContent(L10nKey.sessionsDetailsHost.key) {
                        Text(verbatim: hostId)
                            .accessibilityIdentifier("details.host")
                    }
                    LabeledContent(L10nKey.sessionsTitle.key) {
                        Text(verbatim: session.sessionId)
                            .accessibilityIdentifier("details.session")
                    }
                    if cached {
                        Label(L10nKey.sessionsDetailsCached.key, systemImage: "internaldrive")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("details.cached")
                    }
                }

                detailSection
                modelSection
            }
            .navigationTitle(L10nKey.sessionsDetailsTitle.string)
            .inlineNavigationTitle()
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(L10nKey.commonDone.key) { dismiss() }
                        .accessibilityIdentifier("details.done")
                }
            }
            .task(id: "\(hostId)/\(session.sessionId)") {
                await management.loadDetails(hostId: hostId, sessionId: session.sessionId)
            }
        }
    }

    @ViewBuilder
    private var detailSection: some View {
        switch management.detailsState {
        case .idle, .loading:
            Section {
                HStack(spacing: 8) {
                    ProgressView()
                    Text(L10nKey.commonLoading.key)
                }
                .accessibilityIdentifier("details.loading")
            }
        case .unavailable(let code):
            Section {
                Text(L10nKey.sessionsDetailsUnavailable.string)
                    .font(.headline)
                    .accessibilityIdentifier("details.unavailable")
                Text(L10nKey.errorKey(for: code).key)
                    .foregroundStyle(.secondary)
                Text(verbatim: session.title)
                    .foregroundStyle(.secondary)
            }
        case .loaded(let detail):
            Section {
                LabeledContent(L10nKey.sessionsTitle.key) {
                    Text(verbatim: detail.title)
                }
                if !detail.cwd.isEmpty {
                    LabeledContent(L10nKey.sessionsDetailsCwd.key) {
                        Text(verbatim: detail.cwd)
                            .accessibilityIdentifier("details.cwd")
                    }
                }
                LabeledContent(L10nKey.sessionsDetailsCreated.key) {
                    Text(detail.createdAt, format: .dateTime)
                }
                LabeledContent(L10nKey.sessionsDetailsUpdated.key) {
                    Text(detail.updatedAt, format: .dateTime)
                }
                LabeledContent(L10nKey.sessionsRunning.key) {
                    Text(detail.running ? L10nKey.sessionsRunning.string : L10nKey.sessionsStatusIdle.string)
                }
                if !detail.origin.isEmpty {
                    LabeledContent(L10nKey.sessionsDetailsOrigin.key) {
                        Text(verbatim: detail.origin)
                    }
                }
                if !detail.parentSessionId.isEmpty {
                    LabeledContent(L10nKey.sessionsDetailsParent.key) {
                        Text(verbatim: detail.parentSessionId)
                    }
                }
                if detail.queuedPrompts > 0 {
                    LabeledContent(L10nKey.sessionsDetailsQueue.key) {
                        Text(verbatim: String(detail.queuedPrompts))
                    }
                }
            } header: {
                Text(cached ? L10nKey.sessionsDetailsCached.key : L10nKey.sessionsDetailsLive.key)
            }
        }
    }

    @ViewBuilder
    private var modelSection: some View {
        let supportsModel = {
            if case .loaded(let detail) = management.detailsState {
                return detail.management?.modelSelection ?? true
            }
            return true
        }()
        SessionModelSection(
            hostId: hostId,
            sessionId: session.sessionId,
            supported: supportsModel
        )
    }
}

// MARK: - Fork

struct SessionForkView: View {
    @Environment(AppModel.self) private var app
    @Environment(\.dismiss) private var dismiss

    let hostId: String
    let session: SessionSummary

    private var management: SessionManagementModel { app.sessionManagement }
    private var scope: SessionScope {
        SessionScope(hostId: hostId, sessionId: session.sessionId)
    }

    var body: some View {
        NavigationStack {
            List {
                Section {
                    LabeledContent(L10nKey.sessionsDetailsHost.key) {
                        Text(verbatim: hostId)
                    }
                    LabeledContent(L10nKey.sessionsTitle.key) {
                        Text(verbatim: session.sessionId)
                    }
                }

                content
            }
            .navigationTitle(L10nKey.sessionsForkTitle.string)
            .inlineNavigationTitle()
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button(L10nKey.commonDone.key) { dismiss() }
                        .accessibilityIdentifier("fork.done")
                }
            }
            .task(id: scope) {
                if case .idle = management.forkState {
                    await management.forkSession(hostId: hostId, sessionId: session.sessionId)
                }
            }
        }
    }

    @ViewBuilder
    private var content: some View {
        switch management.forkState {
        case .idle, .forking:
            Section {
                HStack(spacing: 8) {
                    ProgressView()
                    Text(L10nKey.commonLoading.key)
                }
                .accessibilityIdentifier("fork.progress")
            }
        case .failed(let code):
            Section {
                Text(L10nKey.errorKey(for: code).key)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("fork.error")
                Button(L10nKey.commonRetry.key) {
                    Task {
                        await management.forkSession(hostId: hostId, sessionId: session.sessionId)
                    }
                }
                .accessibilityIdentifier("fork.retry")
            }
        case .result(let outcome):
            Section {
                MutationReceiptView(receipt: outcome.receipt)
                if let directory = outcome.workingDirectory {
                    LabeledContent(L10nKey.sessionsForkSharedDirectory.key) {
                        Text(verbatim: directory)
                            .accessibilityIdentifier("fork.directory")
                    }
                } else if outcome.isConfirmed {
                    Label(
                        L10nKey.sessionsForkDirectoryUnavailable.string,
                        systemImage: "exclamationmark.triangle"
                    )
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("fork.directoryUnavailable")
                }
                if let childId = outcome.sessionId {
                    LabeledContent(L10nKey.sessionsTitle.key) {
                        Text(verbatim: childId)
                            .accessibilityIdentifier("fork.childId")
                    }
                }
            }

            Section {
                if let summary = outcome.openableSummary {
                    Button(L10nKey.sessionsForkOpenChild.key) {
                        app.openConversation(hostId: hostId, session: summary)
                        dismiss()
                    }
                    .accessibilityIdentifier("fork.openChild")
                }
                // A proven retryable failure may be attempted again; a
                // confirmed or uncertain fork never is.
                if outcome.canRetry {
                    Button(L10nKey.commonRetry.key) {
                        Task {
                            await management.forkSession(hostId: hostId, sessionId: session.sessionId)
                        }
                    }
                    .accessibilityIdentifier("fork.retry")
                } else if outcome.receipt.status == .uncertain {
                    Text(verbatim: outcome.receipt.reconcileKey ?? outcome.receipt.operationId)
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                        .accessibilityIdentifier("fork.reconcile")
                    // A later authoritative session list, not a resubmission,
                    // describes whether the fork produced a child.
                    Button(L10nKey.sessionsReceiptLookup.string) {
                        Task {
                            await app.refreshSwitcher()
                            dismiss()
                        }
                    }
                    .accessibilityIdentifier("fork.lookup")
                }
            }
        }
    }
}

// MARK: - Shared receipt

struct MutationReceiptView: View {
    let receipt: SessionMutationReceipt

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Label(receipt.status.sessionLabelKey.string, systemImage: icon)
                .foregroundStyle(color)
                .accessibilityIdentifier("receipt.status")
            if let reason = receipt.reasonCode {
                Text(verbatim: reason)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("receipt.reason")
            }
            if let warning = receipt.warningCode {
                Text(verbatim: warning)
                    .font(.caption.monospaced())
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("receipt.warning")
            }
            if let phase = receipt.phase {
                Text(verbatim: phase)
                    .font(.caption.monospaced())
                    .foregroundStyle(.tertiary)
            }
            if !receipt.operationId.isEmpty {
                Text(verbatim: receipt.operationId)
                    .font(.caption2.monospaced())
                    .foregroundStyle(.tertiary)
                    .textSelection(.enabled)
                    .accessibilityIdentifier("receipt.operationId")
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("receipt")
    }

    private var icon: String {
        switch receipt.status {
        case .confirmed: return "checkmark.circle"
        case .failed: return "xmark.octagon"
        case .uncertain: return "questionmark.circle"
        case .accepted, .submitted: return "clock"
        }
    }

    private var color: Color {
        switch receipt.status {
        case .confirmed: return .green
        case .failed: return .red
        case .uncertain: return .orange
        case .accepted, .submitted: return .secondary
        }
    }
}

// MARK: - Model picker

/// Authoritative model picker. The source catalog, the recorded session choice
/// and the source default are displayed as separate values. The DSH default
/// effect is disclosed before any selection is submitted.
struct SessionModelSection: View {
    @Environment(AppModel.self) private var app

    let hostId: String
    let sessionId: String
    let supported: Bool

    private var management: SessionManagementModel { app.sessionManagement }

    var body: some View {
        Section {
            if !supported {
                Label(L10nKey.modelUnavailable.string, systemImage: "questionmark.circle")
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("model.unsupported")
            } else if management.isModelLoading && management.catalog == nil {
                HStack(spacing: 8) {
                    ProgressView()
                    Text(L10nKey.commonLoading.key)
                }
                .accessibilityIdentifier("model.loading")
            } else {
                recordedChoiceRows
                catalogRows
                selectionRows
            }
        } header: {
            Text(L10nKey.modelTitle.string)
        }
        .task(id: "\(hostId)/\(sessionId)") {
            guard supported else { return }
            await management.loadModel(hostId: hostId, sessionId: sessionId)
        }
    }

    @ViewBuilder
    private var recordedChoiceRows: some View {
        if let sessionModel = management.sessionModel {
            LabeledContent(L10nKey.modelNext.key) {
                Text(verbatim: choiceLabel(sessionModel.next))
                    .accessibilityIdentifier("model.next")
            }
            LabeledContent(L10nKey.modelLastUsed.key) {
                Text(verbatim: choiceLabel(sessionModel.lastUsed))
                    .accessibilityIdentifier("model.lastUsed")
            }
            LabeledContent(L10nKey.modelSourceDefault.key) {
                HStack(spacing: 6) {
                    Text(verbatim: choiceLabel(sessionModel.defaultSelection))
                    if sessionModel.defaultIsFallback {
                        Text(L10nKey.modelFallback.string)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .accessibilityIdentifier("model.fallback")
                    }
                }
            }
            if !sessionModel.pendingKnown {
                Text(L10nKey.modelPendingUnknown.string)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("model.pendingUnknown")
            }
        }
        if let error = management.sessionModelError {
            Text(L10nKey.modelSessionUnavailable.string)
                .font(.caption)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("model.sessionUnavailable")
            Text(L10nKey.errorKey(for: error).key)
                .font(.caption2)
                .foregroundStyle(.tertiary)
        }
    }

    @ViewBuilder
    private var catalogRows: some View {
        if let catalog = management.catalog {
            Text(L10nKey.modelDefaultEffectDisclosure.string)
                .font(.footnote)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("model.disclosure")

            ForEach(catalog.providers) { provider in
                ForEach(provider.models) { model in
                    modelMenu(provider: provider, model: model)
                }
            }

            if !catalog.complete {
                Text(L10nKey.modelPartial.string)
                    .font(.caption)
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("model.partial")
                ForEach(catalog.failures) { failure in
                    Text(verbatim: "\(failure.name): \(failure.code)")
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                }
            }
        } else if let error = management.catalogError {
            Text(
                error == .unsupportedCapability
                    ? L10nKey.modelUnavailable.string
                    : L10nKey.errorKey(for: error).string
            )
            .font(.callout)
            .foregroundStyle(.secondary)
            .accessibilityIdentifier("model.unavailable")
            Button(L10nKey.modelReload.key) {
                Task { await management.loadModel(hostId: hostId, sessionId: sessionId) }
            }
            .accessibilityIdentifier("model.reload")
        }
    }

    @ViewBuilder
    private var selectionRows: some View {
        switch management.selectionState {
        case .idle:
            EmptyView()
        case .submitting:
            HStack(spacing: 8) {
                ProgressView()
                Text(L10nKey.commonLoading.key)
            }
            .accessibilityIdentifier("model.submitting")
        case .stale:
            Text(L10nKey.modelStale.string)
                .font(.callout)
                .foregroundStyle(.orange)
                .accessibilityIdentifier("model.stale")
            Button(L10nKey.modelReload.key) {
                management.clearStaleCatalog()
                Task { await management.loadModel(hostId: hostId, sessionId: sessionId) }
            }
            .accessibilityIdentifier("model.reloadStale")
        case .failed(let code):
            Text(L10nKey.errorKey(for: code).key)
                .font(.callout)
                .foregroundStyle(.red)
                .accessibilityIdentifier("model.selectionFailed")
        case .result(let outcome):
            MutationReceiptView(receipt: outcome.receipt)
            Text(
                outcome.sessionAccepted
                    ? L10nKey.modelSessionAccepted.string
                    : L10nKey.modelSessionRejected.string
            )
            .font(.callout)
            .accessibilityIdentifier("model.sessionAccepted")
            if outcome.defaultPersistenceUnconfirmed {
                Text(L10nKey.modelDefaultUnconfirmed.string)
                    .font(.callout)
                    .foregroundStyle(.orange)
                    .accessibilityIdentifier("model.defaultUnconfirmed")
            }
            if let error = management.modelRefreshError {
                Text(L10nKey.errorKey(for: error).key)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .accessibilityIdentifier("model.refreshFailed")
            }
        }
    }

    private func modelMenu(provider: ModelProvider, model: ModelEntry) -> some View {
        Menu {
            ForEach(model.efforts) { effort in
                Button(effort.name) {
                    select(
                        ModelChoice(provider: provider.id, model: model.id, effort: effort.id)
                    )
                }
            }
            Button(L10nKey.modelDefaultEffort.string) {
                select(ModelChoice(provider: provider.id, model: model.id, effort: nil))
            }
        } label: {
            HStack(spacing: 8) {
                VStack(alignment: .leading, spacing: 2) {
                    Text(verbatim: "\(provider.name) · \(model.name)")
                        .foregroundStyle(.primary)
                    if !model.efforts.isEmpty {
                        Text(verbatim: model.efforts.map(\.name).joined(separator: ", "))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }
                Spacer(minLength: 8)
                if let next = management.sessionModel?.next,
                   next.provider == provider.id, next.model == model.id {
                    Image(systemName: "checkmark")
                        .foregroundStyle(.tint)
                        .accessibilityHidden(true)
                }
            }
            .contentShape(Rectangle())
        }
        .disabled(management.isModelLoading)
        .accessibilityIdentifier("model.row.\(provider.id).\(model.id)")
    }

    private func select(_ choice: ModelChoice) {
        Task {
            await management.selectModel(
                hostId: hostId,
                sessionId: sessionId,
                choice: choice
            )
        }
    }

    private func choiceLabel(_ choice: ModelChoice?) -> String {
        guard let choice else { return L10nKey.commonUnknown.string }
        if let effort = choice.effort {
            return "\(choice.provider)/\(choice.model) (\(effort))"
        }
        return "\(choice.provider)/\(choice.model)"
    }
}
