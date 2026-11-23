// Session-management behavior against deterministic fixtures: create-form
// validation and pass-through, returned-identity reuse, scoped details, fork
// receipt states, the model picker's disclosure and the separation between
// session acceptance and source-default persistence.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Session management")
@MainActor
struct SessionManagementTests {
    private func makeModel(
        state: FixtureSessionManagementState? = nil
    ) -> (SessionManagementModel, FixtureSessionService, FixtureSessionManagementState) {
        let resolved = state ?? FixtureSessionManagementState()
        let service = FixtureSessionService(
            draftStore: InMemoryDraftStore(),
            initiallyPaired: true,
            sessionManagement: resolved
        )
        return (SessionManagementModel(service: service), service, resolved)
    }

    // MARK: - Create form

    @Test("create form passes values unchanged and validates location combinations")
    func createFormValidation() {
        var form = SessionCreateForm()
        #expect(form.validation == nil)
        #expect(form.input == .defaults)

        form.useDefaults = false
        form.workspaceId = "workspace-1"
        form.cwd = "/work/tree"
        #expect(form.validation == .bothLocations)
        #expect(form.input.workspaceId == "workspace-1")
        #expect(form.input.cwd == "/work/tree")

        form.workspaceId = ""
        form.cwd = "relative/tree"
        #expect(form.validation == .relativeHostPath)

        form.cwd = "/work/tree"
        form.workspaceId = ""
        form.agentPreset = "reviewer"
        form.preferredSessionId = "session-preferred"
        #expect(form.validation == nil)
        #expect(form.input == SessionCreateInput(
            cwd: "/work/tree",
            workspaceId: nil,
            agentPreset: "reviewer",
            preferredSessionId: "session-preferred"
        ))
        #expect(SessionCreateForm.isAbsoluteHostPath("C:\\work"))
        #expect(SessionCreateForm.isAbsoluteHostPath("c:/work"))
        #expect(SessionCreateForm.isAbsoluteHostPath("\\\\server\\share"))
        #expect(!SessionCreateForm.isAbsoluteHostPath("~/work"))
    }

    @Test("create rejects a location conflict before calling the Host")
    func createValidationStopsSubmission() async {
        let (model, _, state) = makeModel()
        await model.loadCreateSupport(hostId: "studio-mac")
        model.createForm.useDefaults = false
        model.createForm.workspaceId = "workspace-1"
        model.createForm.cwd = "/work/tree"
        let outcome = await model.submitCreate(hostId: "studio-mac")
        #expect(outcome == nil)
        #expect(model.createState == .rejected(.bothLocations))
        #expect(state.createCount == 0)
    }

    @Test("create opens the returned identity, including a reused preferred ID")
    func createReusesReturnedIdentity() async {
        let (model, _, state) = makeModel()
        await model.loadCreateSupport(hostId: "studio-mac")
        #expect(model.canCreate(hostId: "studio-mac"))

        model.createForm.useDefaults = false
        model.createForm.preferredSessionId = "session-preferred"
        model.createForm.agentPreset = "reviewer"
        let outcome = await model.submitCreate(hostId: "studio-mac")
        let created = try? #require(outcome)
        #expect(created?.receipt.status == .confirmed)
        #expect(created?.sessionId == "session-preferred")
        #expect(created?.openableSummary?.sessionId == "session-preferred")
        #expect(created?.session?.hostId == "studio-mac")
        // The exact form values reached the adapter, not a normalized version.
        #expect(state.lastCreateInput?.preferredSessionId == "session-preferred")
        #expect(state.lastCreateInput?.agentPreset == "reviewer")
        #expect(state.lastCreateInput?.cwd == nil)
    }

    @Test("create failure and uncertainty keep their own meanings")
    func createFailureAndUncertainty() async {
        let state = FixtureSessionManagementState()
        let (model, _, _) = makeModel(state: state)
        await model.loadCreateSupport(hostId: "studio-mac")

        state.createDisposition = "FAILED"
        let failed = await model.submitCreate(hostId: "studio-mac")
        #expect(failed?.receipt.status == .failed)
        #expect(failed?.sessionId == nil)
        #expect(failed?.receipt.retryable == true)
        #expect(failed?.openableSummary == nil)

        state.createDisposition = "UNKNOWN"
        let unknown = await model.submitCreate(hostId: "studio-mac")
        #expect(unknown?.receipt.status == .uncertain)
        #expect(unknown?.sessionId == nil)
        #expect(unknown?.receipt.reconcileMode == "PREFERRED_SESSION_ID")
        #expect(unknown?.openableSummary == nil)
    }

    @Test("a Host without create capability never offers a submission")
    func createCapabilityAbsent() async {
        let state = FixtureSessionManagementState()
        state.createSupported = false
        let (model, _, _) = makeModel(state: state)
        await model.loadCreateSupport(hostId: "studio-mac")
        #expect(!model.canCreate(hostId: "studio-mac"))
        #expect(model.createSupportKnown(hostId: "studio-mac"))
        let outcome = await model.submitCreate(hostId: "studio-mac")
        #expect(outcome == nil)
        #expect(model.createState == .failed(.unsupportedCapability))
        #expect(state.createCount == 0)
    }

    // MARK: - Details

    @Test("details identify the selected Host and session")
    func detailsScope() async {
        let (model, _, _) = makeModel()
        await model.loadDetails(hostId: "studio-mac", sessionId: "s-001")
        guard case .loaded(let detail) = model.detailsState else {
            Issue.record("details did not load")
            return
        }
        #expect(detail.hostId == "studio-mac")
        #expect(detail.sessionId == "s-001")
        #expect(detail.title == "Release checklist")
        #expect(detail.cwd == "/work/whalerein")
        #expect(detail.management?.modelSelection == true)
        #expect(detail.management?.sessionFork == true)
        #expect(detail.summary.sessionId == "s-001")
        #expect(model.detailsScope == SessionScope(hostId: "studio-mac", sessionId: "s-001"))
    }

    @Test("unavailable details stay scoped and do not invent a summary")
    func detailsUnavailable() async {
        let (model, _, _) = makeModel()
        await model.loadDetails(hostId: "travel-laptop", sessionId: "s-missing")
        guard case .unavailable(let code) = model.detailsState else {
            Issue.record("details should be unavailable")
            return
        }
        #expect(code == .unavailable)
        #expect(model.detailsScope?.hostId == "travel-laptop")
        #expect(model.detailsScope?.sessionId == "s-missing")
    }

    @Test("same session IDs on two Hosts keep separate detail scopes")
    func detailsHostScoped() async {
        let (model, _, _) = makeModel()
        await model.loadDetails(hostId: "studio-mac", sessionId: "s-001")
        await model.loadDetails(hostId: "travel-laptop", sessionId: "s-001")
        #expect(model.detailsScope == SessionScope(hostId: "travel-laptop", sessionId: "s-001"))
        if case .loaded(let detail) = model.detailsState {
            #expect(detail.hostId == "travel-laptop")
        } else if case .unavailable = model.detailsState {
            // The fixture Host does not serve s-001; either way the scope must
            // never silently point at the other Host.
        } else {
            Issue.record("unexpected details state")
        }
    }

    // MARK: - Fork

    @Test("a confirmed fork opens the child with its shared directory")
    func forkConfirmed() async {
        let (model, _, state) = makeModel()
        let outcome = await model.forkSession(hostId: "studio-mac", sessionId: "s-001")
        let forked = try? #require(outcome)
        #expect(forked?.isConfirmed == true)
        #expect(forked?.sessionId == "s-001-fork-1")
        #expect(forked?.workingDirectory == "/work/whalerein")
        #expect(forked?.workingDirectoryError == nil)
        #expect(forked?.openableSummary?.sessionId == "s-001-fork-1")
        #expect(state.forkCount == 1)

        // A confirmed fork is never submitted a second time.
        _ = await model.forkSession(hostId: "studio-mac", sessionId: "s-001")
        #expect(state.forkCount == 1)
        #expect(model.isForkConfirmed(
            for: SessionScope(hostId: "studio-mac", sessionId: "s-001")
        ))
    }

    @Test("a confirmed fork keeps the child when its directory is unreadable")
    func forkDirectoryUnavailable() async {
        let state = FixtureSessionManagementState()
        state.forkDirectoryErrorCode = "not_found"
        let (model, _, _) = makeModel(state: state)
        let outcome = await model.forkSession(hostId: "studio-mac", sessionId: "s-001")
        #expect(outcome?.isConfirmed == true)
        #expect(outcome?.workingDirectory == nil)
        #expect(outcome?.workingDirectoryError == .unavailable)
        #expect(outcome?.child?.sessionId == "s-001-fork-1")
        #expect(outcome?.openableSummary?.sessionId == "s-001-fork-1")
    }

    @Test("fork failure retries only when Core says it is retryable")
    func forkFailureStates() async {
        let state = FixtureSessionManagementState()
        state.forkDisposition = "FAILED"
        state.forkReasonCode = "SOURCE_NO_COMPLETE_TURN"
        state.forkRetryable = false
        let (model, _, _) = makeModel(state: state)
        let failed = await model.forkSession(hostId: "studio-mac", sessionId: "s-001")
        #expect(failed?.receipt.status == .failed)
        #expect(failed?.receipt.reasonCode == "SOURCE_NO_COMPLETE_TURN")
        #expect(failed?.canRetry == false)
        #expect(failed?.sessionId == nil)

        state.forkDisposition = "UNKNOWN"
        state.forkRetryable = false
        let unknown = await model.forkSession(hostId: "studio-mac", sessionId: "s-001")
        #expect(unknown?.receipt.status == .uncertain)
        #expect(unknown?.receipt.reconcileKey == "s-001")
        #expect(unknown?.canRetry == false)
        #expect(unknown?.openableSummary == nil)
    }

    // MARK: - Model

    @Test("the picker keeps next, last used and fallback separate")
    func modelPickerValues() async {
        let (model, _, _) = makeModel()
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        #expect(model.catalog?.complete == true)
        #expect(model.catalog?.defaultSelection?.model == "sonnet")
        #expect(model.sessionModel?.next?.model == "sonnet")
        #expect(model.sessionModel?.lastUsed?.model == "sonnet")
        #expect(model.sessionModel?.defaultSelection?.model == "sonnet")
        #expect(model.sessionModel?.defaultIsFallback == true)
        #expect(model.catalog?.providers.first?.models.first?.efforts.map(\.id)
            == ["low", "medium", "high"])
    }

    @Test("the disclosure is available before any submission")
    func pickerDisclosure() {
        let (model, _, _) = makeModel()
        #expect(model.defaultEffectDisclosureKey == .modelDefaultEffectDisclosure)
        #expect(!L10nKey.modelDefaultEffectDisclosure.string.isEmpty)
    }

    @Test("session acceptance and unconfirmed default persistence stay distinct")
    func selectionReportsBothResults() async {
        let state = FixtureSessionManagementState()
        state.defaultPersistence = "unconfirmed"
        let (model, _, _) = makeModel(state: state)
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        let choice = ModelChoice(provider: "anthropic", model: "opus", effort: "high")
        let outcome = await model.selectModel(
            hostId: "studio-mac",
            sessionId: "s-001",
            choice: choice
        )
        #expect(outcome?.sessionAccepted == true)
        #expect(outcome?.defaultPersistenceUnconfirmed == true)
        #expect(outcome?.defaultPersistence == "unconfirmed")
        #expect(outcome?.submitted == true)
        #expect(state.selectionCount == 1)
        // The recorded choice was refreshed from the source.
        #expect(model.sessionModel?.next == choice)
        #expect(model.selectionState == .result(outcome!))
    }

    @Test("a stale catalog refuses the selection and asks for a reload")
    func staleCatalog() async {
        let (model, _, state) = makeModel()
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        let missing = ModelChoice(provider: "openai", model: "ghost", effort: nil)
        let outcome = await model.selectModel(
            hostId: "studio-mac",
            sessionId: "s-001",
            choice: missing
        )
        #expect(outcome == nil)
        #expect(model.selectionState == .stale)
        #expect(model.isCatalogStale)
        #expect(state.selectionCount == 0)
        model.clearStaleCatalog()
        #expect(!model.isCatalogStale)
        #expect(model.selectionState == .idle)
    }

    @Test("a partial catalog stays usable and keeps provider failures visible")
    func partialCatalog() async {
        let state = FixtureSessionManagementState()
        state.catalogComplete = false
        state.catalogFailure = ModelProviderFailure(
            id: "openai",
            name: "OpenAI",
            code: "unavailable"
        )
        let (model, _, _) = makeModel(state: state)
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        #expect(model.catalog?.complete == false)
        #expect(model.catalog?.providerNamesWithFailures == ["OpenAI"])
        #expect(model.catalog?.providers.count == 2)
        #expect(model.catalogError == nil)
    }

    @Test("missing capability and offline reads stay explicit and independent")
    func modelUnavailableAndOffline() async {
        let state = FixtureSessionManagementState()
        state.sessionModelUnavailable = true
        let (model, _, _) = makeModel(state: state)
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        #expect(model.catalog != nil)
        #expect(model.sessionModel == nil)
        #expect(model.sessionModelError == .unsupportedCapability)

        state.catalogOffline = true
        state.sessionModelUnavailable = false
        await model.loadModel(hostId: "travel-laptop", sessionId: "s-101")
        #expect(model.catalog == nil)
        #expect(model.catalogError == .hostOffline)
        // The passive session read still succeeded: one failure must not hide
        // the other.
        #expect(model.sessionModel != nil)

        state.catalogOffline = false
        state.catalogUnavailable = true
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        #expect(model.catalogError == .unsupportedCapability)
    }

    @Test("a rejected selection is a proven failure")
    func selectionRejected() async {
        let state = FixtureSessionManagementState()
        state.selectionOutcome = "REJECTED"
        let (model, _, _) = makeModel(state: state)
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        let outcome = await model.selectModel(
            hostId: "studio-mac",
            sessionId: "s-001",
            choice: ModelChoice(provider: "anthropic", model: "sonnet", effort: "low")
        )
        #expect(outcome?.sessionAccepted == false)
        #expect(outcome?.receipt.status == .failed)
        #expect(outcome?.selected == nil)
    }

    @Test("an uncertain selection keeps its receipt without inventing identity")
    func selectionUncertain() async {
        let state = FixtureSessionManagementState()
        state.selectionOutcome = "UNKNOWN"
        let (model, _, _) = makeModel(state: state)
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        let outcome = await model.selectModel(
            hostId: "studio-mac",
            sessionId: "s-001",
            choice: ModelChoice(provider: "anthropic", model: "opus", effort: nil)
        )
        #expect(outcome?.receipt.status == .uncertain)
        #expect(outcome?.submitted == false)
        #expect(outcome?.sessionAccepted == false)
    }
}

/// Delegates real fixture semantics and gates one response so the test can
/// observe the in-flight window before publishing. Later calls are ungated.
@MainActor
final class GatedSessionManagementService: SessionManagementService {
    let fixture: FixtureSessionService
    var createGate: SuspendedResponse<Void>?
    var forkGate: SuspendedResponse<Void>?
    var detailsGate: SuspendedResponse<Void>?
    private var gateNextCreate = true
    private var gateNextFork = true
    private var gateNextDetails = true

    init(fixture: FixtureSessionService) {
        self.fixture = fixture
    }

    var instanceId: String { fixture.instanceId }

    func sessionCreateSupported(hostId: String) async throws -> Bool {
        try await fixture.sessionCreateSupported(hostId: hostId)
    }

    func createSession(
        hostId: String,
        input: SessionCreateInput
    ) async throws -> SessionCreateOutcome {
        if gateNextCreate, let createGate {
            gateNextCreate = false
            try await createGate.wait()
        }
        return try await fixture.createSession(hostId: hostId, input: input)
    }

    func sessionDetails(hostId: String, sessionId: String) async throws -> SessionDetail {
        if gateNextDetails, let detailsGate {
            gateNextDetails = false
            try await detailsGate.wait()
        }
        return try await fixture.sessionDetails(hostId: hostId, sessionId: sessionId)
    }

    func forkSession(hostId: String, sessionId: String) async throws -> SessionForkOutcome {
        if gateNextFork, let forkGate {
            gateNextFork = false
            try await forkGate.wait()
        }
        return try await fixture.forkSession(hostId: hostId, sessionId: sessionId)
    }

    func modelCatalog(hostId: String) async throws -> ModelCatalog {
        try await fixture.modelCatalog(hostId: hostId)
    }

    func readSessionModel(hostId: String, sessionId: String) async throws -> SessionModelState {
        try await fixture.readSessionModel(hostId: hostId, sessionId: sessionId)
    }

    func selectSessionModel(
        hostId: String,
        sessionId: String,
        choice: ModelChoice
    ) async throws -> ModelSelectionOutcome {
        try await fixture.selectSessionModel(hostId: hostId, sessionId: sessionId, choice: choice)
    }
}

@Suite("Session management scope and receipts")
@MainActor
struct SessionManagementScopeTests {
    private func makeGated() -> (SessionManagementModel, GatedSessionManagementService, FixtureSessionManagementState) {
        let state = FixtureSessionManagementState()
        let fixture = FixtureSessionService(
            draftStore: InMemoryDraftStore(),
            initiallyPaired: true,
            sessionManagement: state
        )
        let service = GatedSessionManagementService(fixture: fixture)
        return (SessionManagementModel(service: service), service, state)
    }

    @Test("confirmed create with a failed details read keeps its identity and retries only details")
    func confirmedCreateRetriesDetailsOnly() async throws {
        let (model, _, state) = makeGated()
        state.createDetailFails = true
        model.prepareCreate(hostId: "studio-mac")
        await model.loadCreateSupport(hostId: "studio-mac")

        let submitted = await model.submitCreate(hostId: "studio-mac")
        let outcome = try #require(submitted)
        #expect(outcome.receipt.status == .confirmed)
        #expect(outcome.sessionId == "s-created-1")
        #expect(outcome.session == nil)
        #expect(outcome.detailError == .unavailable)
        #expect(outcome.openableSummary?.sessionId == "s-created-1")
        #expect(state.createCount == 1)

        // A repeated submission reuses the confirmed identity instead of
        // creating a second session.
        let repeated = await model.submitCreate(hostId: "studio-mac")
        #expect(repeated?.sessionId == "s-created-1")
        #expect(state.createCount == 1)

        // The retry reads details only; it never calls create again.
        let detail = await model.retryCreateDetails(
            hostId: "studio-mac",
            sessionId: "s-created-1"
        )
        #expect(detail?.hostId == "studio-mac")
        #expect(detail?.sessionId == "s-created-1")
        #expect(state.createCount == 1)
        guard case .result(let refreshed) = model.createState else {
            Issue.record("confirmed receipt disappeared after a detail retry")
            return
        }
        #expect(refreshed.detailError == nil)
        #expect(refreshed.session?.sessionId == "s-created-1")
    }

    @Test("a failed details retry keeps the confirmed create receipt visible")
    func failedDetailsRetryKeepsReceipt() async throws {
        let (model, _, state) = makeGated()
        state.createDetailFails = true
        state.createdDetailsFail = true
        model.prepareCreate(hostId: "studio-mac")
        await model.loadCreateSupport(hostId: "studio-mac")
        let submitted = await model.submitCreate(hostId: "studio-mac")
        _ = try #require(submitted)

        let detail = await model.retryCreateDetails(
            hostId: "studio-mac",
            sessionId: "s-created-1"
        )
        #expect(detail == nil)
        #expect(state.createCount == 1)
        guard case .result(let unchanged) = model.createState else {
            Issue.record("a failed detail retry replaced the confirmed receipt")
            return
        }
        #expect(unchanged.receipt.status == .confirmed)
        #expect(unchanged.sessionId == "s-created-1")
        #expect(unchanged.detailError == .unavailable)
    }

    @Test("a late create result from an earlier sheet is fenced to its Host")
    func lateCreateIsFenced() async throws {
        let (model, service, state) = makeGated()
        model.prepareCreate(hostId: "studio-mac")
        await model.loadCreateSupport(hostId: "studio-mac")
        let gate = SuspendedResponse<Void>()
        service.createGate = gate

        let first = Task { await model.submitCreate(hostId: "studio-mac") }
        await gate.waitUntilStarted()
        // A new sheet for another Host replaces the scope before the first
        // Host answers.
        model.prepareCreate(hostId: "travel-laptop")
        gate.resume(with: ())
        let late = await first.value

        #expect(late == nil)
        #expect(model.createState == .idle)
        #expect(model.isCreating == false)
        await model.loadCreateSupport(hostId: "travel-laptop")

        let submitted = await model.submitCreate(hostId: "travel-laptop")
        let current = try #require(submitted)
        #expect(current.hostId == "travel-laptop")
        #expect(current.receipt.status == .confirmed)
        #expect(state.createCount == 2)
    }

    @Test("a late fork from another scope cannot become the visible child")
    func lateForkIsFenced() async throws {
        let (model, service, _) = makeGated()
        let studio = SessionScope(hostId: "studio-mac", sessionId: "s-001")
        let travel = SessionScope(hostId: "travel-laptop", sessionId: "s-101")
        let gate = SuspendedResponse<Void>()
        service.forkGate = gate

        let first = Task { await model.forkSession(hostId: studio.hostId, sessionId: studio.sessionId) }
        await gate.waitUntilStarted()
        // Selecting another session fences the old scope and immediately
        // starts the fork the user asked for.
        let travelResult = await model.forkSession(
            hostId: travel.hostId,
            sessionId: travel.sessionId
        )
        #expect(model.forkScope == travel)
        let travelOutcome = try #require(travelResult)
        #expect(travelOutcome.hostId == travel.hostId)
        #expect(travelOutcome.parentSessionId == travel.sessionId)
        #expect(travelOutcome.child?.hostId == travel.hostId)
        #expect(model.isForkConfirmed(for: travel))

        gate.resume(with: ())
        let late = await first.value
        #expect(late == nil)
        #expect(model.isForkConfirmed(for: travel))
        #expect(!model.isForkConfirmed(for: studio))

        // A confirmed child is reused instead of forking a second time.
        let duplicate = await model.forkSession(
            hostId: travel.hostId,
            sessionId: travel.sessionId
        )
        #expect(duplicate?.isConfirmed == true)
    }

    @Test("a late details read cannot retarget the selected session")
    func lateDetailsAreFenced() async throws {
        let (model, service, _) = makeGated()
        let gate = SuspendedResponse<Void>()
        service.detailsGate = gate

        let first = Task { await model.loadDetails(hostId: "studio-mac", sessionId: "s-001") }
        await gate.waitUntilStarted()
        await model.loadDetails(hostId: "travel-laptop", sessionId: "s-101")
        gate.resume(with: ())
        await first.value

        #expect(model.detailsScope == SessionScope(hostId: "travel-laptop", sessionId: "s-101"))
        if case .loaded(let detail) = model.detailsState {
            #expect(detail.hostId == "travel-laptop")
        } else {
            #expect(model.detailsState == .unavailable(.unavailable))
        }
    }

    @Test("an accepted receipt stays visible when the follow-up refresh fails")
    func acceptedReceiptSurvivesRefreshFailure() async throws {
        let (model, _, state) = makeGated()
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        state.defaultPersistence = "unconfirmed"
        state.sessionModelUnavailable = true
        let choice = ModelChoice(provider: "anthropic", model: "opus", effort: "high")

        let selected = await model.selectModel(
            hostId: "studio-mac",
            sessionId: "s-001",
            choice: choice
        )
        let outcome = try #require(selected)

        #expect(outcome.sessionAccepted)
        #expect(outcome.defaultPersistenceUnconfirmed)
        #expect(model.selectionState == .result(outcome))
        #expect(model.modelRefreshError == .unsupportedCapability)
        // The accepted receipt is still the visible result, not a failure.
        guard case .result(let visible) = model.selectionState else {
            Issue.record("the accepted receipt was replaced by the refresh failure")
            return
        }
        #expect(visible.sessionAccepted)
    }

    @Test("an explicit persisted default is the only success value")
    func persistedDefaultIsSuccess() async throws {
        let (model, _, state) = makeGated()
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        state.defaultPersistence = "persisted"
        let selected = await model.selectModel(
            hostId: "studio-mac",
            sessionId: "s-001",
            choice: ModelChoice(provider: "anthropic", model: "opus", effort: nil)
        )
        let outcome = try #require(selected)
        #expect(outcome.sessionAccepted)
        #expect(outcome.defaultPersistenceConfirmed)
        #expect(!outcome.defaultPersistenceUnconfirmed)
    }

    @Test("rejected and uncertain default writes are not reported as success")
    func rejectedOrUncertainDefaultIsUnconfirmed() async throws {
        for value in ["rejected", "uncertain", "unconfirmed"] {
            let (model, _, state) = makeGated()
            await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
            state.defaultPersistence = value
            let selected = await model.selectModel(
                hostId: "studio-mac",
                sessionId: "s-001",
                choice: ModelChoice(provider: "anthropic", model: "opus", effort: nil)
            )
            let outcome = try #require(selected)
            #expect(outcome.sessionAccepted)
            #expect(outcome.defaultPersistenceConfirmed == false)
            #expect(
                outcome.defaultPersistenceUnconfirmed,
                "\(value) must not be presented as a persisted default"
            )
        }
    }

    @Test("a stale catalog refuses submission and asks for a reload")
    func staleCatalogAsksForReload() async throws {
        let (model, _, state) = makeGated()
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        let outcome = await model.selectModel(
            hostId: "studio-mac",
            sessionId: "s-001",
            choice: ModelChoice(provider: "anthropic", model: "removed", effort: nil)
        )
        #expect(outcome == nil)
        #expect(model.selectionState == .stale)
        #expect(model.isCatalogStale)
        #expect(state.selectionCount == 0)

        model.clearStaleCatalog()
        await model.loadModel(hostId: "studio-mac", sessionId: "s-001")
        let reselected = await model.selectModel(
            hostId: "studio-mac",
            sessionId: "s-001",
            choice: ModelChoice(provider: "anthropic", model: "opus", effort: "high")
        )
        let reloaded = try #require(reselected)
        #expect(reloaded.sessionAccepted)
        #expect(state.selectionCount == 1)
    }
}
