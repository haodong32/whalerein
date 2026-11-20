// Feature adapter boundary for session creation, details, fork and the model
// catalog. `LiveSessionService` and `FixtureSessionService` implement it in
// separate extension files so the base service stays unchanged.

import Foundation

@MainActor
protocol SessionManagementService: AnyObject {
    /// Stable app configuration instance this adapter belongs to.
    var instanceId: String { get }
    /// Whether the Host advertises native session creation.
    func sessionCreateSupported(hostId: String) async throws -> Bool
    /// Create a session with optional Host-owned inputs passed unchanged.
    func createSession(hostId: String, input: SessionCreateInput) async throws -> SessionCreateOutcome
    /// Read the selected Host's summary for exactly this session.
    func sessionDetails(hostId: String, sessionId: String) async throws -> SessionDetail
    /// Fork the latest completed native turn and report the child directory.
    func forkSession(hostId: String, sessionId: String) async throws -> SessionForkOutcome
    /// Read the current source catalog; never a cached or offline value.
    func modelCatalog(hostId: String) async throws -> ModelCatalog
    /// Read the passive session model observation.
    func readSessionModel(hostId: String, sessionId: String) async throws -> SessionModelState
    /// Submit exactly one native model selection. No prompt is sent.
    func selectSessionModel(
        hostId: String,
        sessionId: String,
        choice: ModelChoice
    ) async throws -> ModelSelectionOutcome
}

/// Used when the active service predates the management adapter (for example a
/// test stub). Every operation fails explicitly instead of inventing data.
@MainActor
final class UnavailableSessionManagementService: SessionManagementService {
    let instanceId = ""

    func sessionCreateSupported(hostId: String) async throws -> Bool { false }

    func createSession(
        hostId: String,
        input: SessionCreateInput
    ) async throws -> SessionCreateOutcome {
        throw SessionServiceError(.integrationUnavailable)
    }

    func sessionDetails(hostId: String, sessionId: String) async throws -> SessionDetail {
        throw SessionServiceError(.integrationUnavailable)
    }

    func forkSession(hostId: String, sessionId: String) async throws -> SessionForkOutcome {
        throw SessionServiceError(.integrationUnavailable)
    }

    func modelCatalog(hostId: String) async throws -> ModelCatalog {
        throw SessionServiceError(.integrationUnavailable)
    }

    func readSessionModel(hostId: String, sessionId: String) async throws -> SessionModelState {
        throw SessionServiceError(.integrationUnavailable)
    }

    func selectSessionModel(
        hostId: String,
        sessionId: String,
        choice: ModelChoice
    ) async throws -> ModelSelectionOutcome {
        throw SessionServiceError(.integrationUnavailable)
    }
}
