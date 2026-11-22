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
    @Environment(\.dismiss) private var dismiss

    let hostId: String
    let management: SessionManagementModel
    var openConversation: (String, SessionSummary) -> Void = { _, _ in }
    var refresh: (() async -> Void)?

    var body: some View {
        @Bindable var management = management
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
        openConversation(hostId, summary)
        dismiss()
    }
}

// MARK: - Details
