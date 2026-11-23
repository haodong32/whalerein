// Deterministic instance status and default-computer fixtures.
//
// The fixture mirrors Core's typed status mapping without a network: Cloud and
// Standalone services, signed-in/out and pending remote sign-out, paired,
// offline/unavailable and revoked device access, and the selected default Host.

import Foundation

@MainActor
extension FixtureSessionService: InstanceStatusService {
    func instanceStatus() async throws -> InstanceStatus {
        let accountState: InstanceAccountState
        let signedIn: Bool
        switch serviceKind {
        case .standalone:
            accountState = .notApplicable
            signedIn = true
        case .cloud:
            signedIn = cloudState.account != nil
            accountState = signedIn ? .signedIn : .signedOut
        }

        let deviceAccess: InstanceDeviceAccess
        if serviceKind == .cloud && !signedIn {
            deviceAccess = .signInRequired
        } else if revoked {
            deviceAccess = .revoked
        } else if !paired {
            deviceAccess = .unpaired
        } else if options.offlineOnOpen || hasOfflineHostConnection {
            deviceAccess = .unavailable
        } else {
            deviceAccess = .ready
        }

        return InstanceStatus(
            service: serviceKind == .cloud ? .cloud : .standalone,
            testingEnvironment: options.staging ? "staging" : nil,
            instanceId: instanceId,
            accountState: accountState,
            remoteSignoutPending: serviceKind == .cloud && cloudState.logoutPending,
            defaultHost: signedIn ? defaultHostId : nil,
            deviceAccess: deviceAccess,
            configPath: "/fixtures/\(instanceId)/instance.toml",
            dataDir: "/fixtures/\(instanceId)/data"
        )
    }

    func defaultHost() async throws -> String? {
        defaultHostId
    }

    func selectDefaultHost(hostId: String) async throws {
        guard paired, !revoked else { throw SessionServiceError(.notPaired) }
        guard FixtureCatalog.hosts.contains(where: { $0.hostId == hostId }) else {
            throw SessionServiceError(.conflict)
        }
        defaultHostId = hostId
    }

    func currentCloudSessionId() async throws -> String? {
        guard serviceKind == .cloud, cloudState.account != nil else { return nil }
        return FixtureSessionService.fixtureCurrentSessionId
    }
}
