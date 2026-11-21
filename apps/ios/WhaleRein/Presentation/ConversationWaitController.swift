// Native state-only wait controller. The attempt has its own identity, deadline
// and cancellation. It never owns or stops the conversation watch, so cancelling
// a wait releases only that observation.

import Foundation
import Observation

@MainActor
@Observable
final class ConversationWaitController {
    enum Phase: Equatable {
        case idle
        case waiting
        case settled
    }

    let hostID: String
    let sessionID: String

    private(set) var phase: Phase = .idle
    private(set) var result: WaitResult?
    private(set) var waitID: String?
    private(set) var deadline: Date?
    private(set) var errorKey: L10nKey?
    private(set) var isCancelling = false

    private let service: any SessionService
    private var observeTask: Task<Void, Never>?

    init(hostID: String, sessionID: String, service: any SessionService) {
        self.hostID = hostID
        self.sessionID = sessionID
        self.service = service
    }

    var isWaiting: Bool { phase == .waiting }

    /// Begin one wait. The default is the CLI's observation budget; the native
    /// deadline owns the outcome, not this controller.
    func start(timeoutMs: UInt64 = 120_000) async {
        guard phase != .waiting else { return }
        observeTask?.cancel()
        errorKey = nil
        result = nil
        do {
            let id = try await service.beginWait(
                hostId: hostID,
                sessionId: sessionID,
                timeoutMs: timeoutMs
            )
            waitID = id
            deadline = Date().addingTimeInterval(Double(timeoutMs) / 1000)
            phase = .waiting
            observeTask = Task { [weak self] in
                guard let self else { return }
                do {
                    let settled = try await self.service.takeWaitResult(
                        waitId: id,
                        waitMs: timeoutMs + 5_000
                    )
                    guard !Task.isCancelled else { return }
                    if let settled {
                        self.finish(settled)
                    } else {
                        self.fail(.timeout)
                    }
                } catch {
                    guard !Task.isCancelled else { return }
                    let code = (error as? SessionServiceError)?.code ?? .internalError
                    self.errorKey = L10nKey.errorKey(for: code)
                    self.phase = .settled
                }
            }
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            errorKey = L10nKey.errorKey(for: code)
            phase = .settled
        }
    }

    /// Cancel only this wait attempt. The eventual binding result reports
    /// `cancelled`; the conversation observation continues independently.
    func cancel() async {
        guard phase == .waiting, let waitID else { return }
        isCancelling = true
        defer { isCancelling = false }
        do {
            _ = try await service.cancelWait(waitId: waitID)
            // The pending take receives the cancelled result and settles.
        } catch {
            let code = (error as? SessionServiceError)?.code ?? .internalError
            errorKey = L10nKey.errorKey(for: code)
        }
    }

    func reset() {
        observeTask?.cancel()
        observeTask = nil
        phase = .idle
        result = nil
        waitID = nil
        deadline = nil
        errorKey = nil
    }

    /// Release the attempt when the conversation closes. This cannot touch the
    /// watch, which the model stops separately.
    func close() {
        let id = waitID
        observeTask?.cancel()
        observeTask = nil
        if let id, phase == .waiting {
            Task { [service] in
                _ = try? await service.cancelWait(waitId: id)
            }
        }
        phase = .idle
        waitID = nil
        deadline = nil
    }

    private func finish(_ settled: WaitResult) {
        result = settled
        waitID = nil
        deadline = nil
        phase = .settled
        if settled.outcome == .failed {
            errorKey = .waitFailed
        } else {
            errorKey = nil
        }
    }

    private func fail(_ code: AppErrorCode) {
        result = nil
        waitID = nil
        deadline = nil
        phase = .settled
        errorKey = L10nKey.errorKey(for: code)
    }
}
