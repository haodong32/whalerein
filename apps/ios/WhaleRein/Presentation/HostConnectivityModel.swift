// App-level Host connectivity. One device connection belongs to a Host, not to
// a session, so this model owns the authoritative status every screen reads.
//
// The model seeds from `hostConnection(hostId:)` (which distinguishes "never
// opened" from "offline") and then follows the service's unscoped
// `hostConnection` events for as long as the app is in the foreground. The
// probe itself is started and stopped by the shell's lifecycle callbacks.

import Foundation
import Observation

/// Presentation view of one Host connection.
enum HostConnectionStatus: Equatable, Sendable {
    /// No snapshot exists yet; the connection was never opened or is about to
    /// be opened by the app-level monitor. Displays as "connecting".
    case unknown
    case disconnected
    case connecting
    case connected
    /// Forward-compatible: Core may report a limited connection.
    case degraded
    case reconnecting
    /// Terminal failure reported by Core (`revoked`, `unauthorized`,
    /// `not_paired`, `protocol_version_incompatible`, ...).
    case unavailable(String)
    case offline

    init(state: String, code: String?) {
        if let code, !code.isEmpty {
            self = .unavailable(code.lowercased())
            return
        }
        switch state.lowercased() {
        case "connected":
            self = .connected
        case "pairing", "connecting", "handshaking", "syncing":
            self = .connecting
        case "reconnecting":
            self = .reconnecting
        case "degraded", "limited":
            self = .degraded
        case "offline":
            self = .offline
        case "disconnected":
            self = .disconnected
        default:
            self = .unknown
        }
    }

    init(snapshot: HostConnectionSnapshot) {
        self.init(state: snapshot.state, code: snapshot.code)
    }

    var isOnline: Bool { self == .connected || self == .degraded }

}

@MainActor
@Observable
final class HostConnectivityModel {
    /// Latest snapshot per Host. A missing entry means "never opened", which is
    /// deliberately different from an `offline` snapshot.
    private(set) var snapshot: [String: HostConnectionSnapshot] = [:]

    private var service: (any SessionService)?
    private var consumeTask: Task<Void, Never>?
    private var generation = 0

    init(snapshot: [String: HostConnectionSnapshot] = [:]) {
        self.snapshot = snapshot
    }

    // MARK: - Reads

    func status(for hostId: String) -> HostConnectionStatus {
        guard let snapshot = snapshot[hostId] else { return .unknown }
        return HostConnectionStatus(snapshot: snapshot)
    }

    /// Authoritative status once a snapshot exists. Before the first snapshot,
    /// fall back to the directory's discovery/Cloud presence hint exactly once;
    /// after that, presence never overrides the Host connection.
    func status(for hostId: String, fallbackOnline: Bool) -> HostConnectionStatus {
        guard snapshot[hostId] != nil else {
            return fallbackOnline ? .connected : .offline
        }
        return status(for: hostId)
    }

    func isOnline(_ hostId: String) -> Bool {
        status(for: hostId).isOnline
    }

    func hasSnapshot(_ hostId: String) -> Bool {
        snapshot[hostId] != nil
    }

    // MARK: - Events

    func handle(_ envelope: SessionEventEnvelope) {
        guard case .hostConnection(let hostId, let state, let code, let sinceUnixMs) = envelope.event else {
            return
        }
        apply(
            HostConnectionSnapshot(
                hostId: hostId,
                state: state,
                code: code,
                sinceUnixMs: sinceUnixMs
            )
        )
    }

    // MARK: - Lifecycle

    /// Subscribes to the app-level event stream, seeds the initial snapshot from
    /// Core, then starts the broker probe for every Host the user can see.
    func startMonitoring(service: any SessionService, hostIds: [String]) {
        stopMonitoring()
        self.service = service
        generation &+= 1
        let generation = generation

        // Subscribe before starting the monitor so transitions are not lost.
        let stream = service.makeEventStream()
        consumeTask = Task { [weak self] in
            for await envelope in stream {
                guard let self, !Task.isCancelled, self.generation == generation else { return }
                self.handle(envelope)
            }
        }

        for hostId in hostIds {
            if let snapshot = service.hostConnection(hostId: hostId) {
                apply(snapshot)
            }
        }
        service.startHostMonitor(hostIds: hostIds)
    }

    /// Stops the probe and its event subscription. The last snapshot is kept so
    /// a foreground return can render it until the fresh probe reports.
    func stopMonitoring() {
        generation &+= 1
        consumeTask?.cancel()
        consumeTask = nil
        service?.stopHostMonitor()
        service = nil
    }

    func reset() {
        stopMonitoring()
        snapshot.removeAll()
    }

    private func apply(_ snapshot: HostConnectionSnapshot) {
        guard self.snapshot[snapshot.hostId] != snapshot else { return }
        self.snapshot[snapshot.hostId] = snapshot
    }
}
