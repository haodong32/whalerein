// Service used when the live binding cannot initialize. It keeps the app
// running and reports a clear localized failure instead of crashing.
//
// The integration agent selects LiveSessionService; this type remains the
// explicit startup-failure state.

import Foundation

@MainActor
final class UnavailableSessionService: SessionService {
    let connectionStatus: ConnectionStatus = .offline
    let coreRevision = "unavailable"
    let appVersion: String = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.1.0"

    private let code: AppErrorCode

    init(code: AppErrorCode = .integrationUnavailable) {
        self.code = code
    }

    func makeEventStream() -> AsyncStream<SessionEventEnvelope> {
        AsyncStream { continuation in
            continuation.finish()
        }
    }

    private func fail() throws -> Never {
        throw SessionServiceError(code)
    }

    func pairingStart(invitation: String) async throws -> PairingPrompt { try fail() }
    func pairingDecide(approved: Bool, expectedFingerprint: String?) async throws { try fail() }
    func pairingCancel() async {}
    func hosts() async throws -> [HostSummary] { try fail() }
    func sessions(hostId: String) async throws -> [SessionSummary] { try fail() }
    func cachedSessions(hostId: String) async throws -> [SessionSummary] { try fail() }
    func capabilities(hostId: String, sessionId: String) async throws -> SessionCapabilities { try fail() }
    func startWatch(hostId: String, sessionId: String) async throws { try fail() }
    func stopWatch() {}
    func send(hostId: String, sessionId: String, text: String, delivery: DeliveryChoice) async throws -> CommandReceipt { try fail() }
    func stop(hostId: String, sessionId: String) async throws -> CommandReceipt { try fail() }
    func approvals(hostId: String, sessionId: String) async throws -> [Approval] { try fail() }
    func resolveApproval(
        hostId: String,
        sessionId: String,
        approvalId: String,
        version: Int,
        digestHex: String,
        allow: Bool
    ) async throws -> Approval { try fail() }
    func saveDraft(hostId: String, sessionId: String, text: String) async throws { try fail() }
    func loadDraft(hostId: String, sessionId: String) async throws -> String? { try fail() }
    func clearDraft(hostId: String, sessionId: String) async throws { try fail() }
    func diagnosticsPreview() async throws -> DiagnosticsReport { try fail() }
    func exportDiagnostics(to url: URL) async throws -> DiagnosticsReport { try fail() }
    func forgetDevice() async throws { try fail() }
    func revoke() async throws { try fail() }
}
