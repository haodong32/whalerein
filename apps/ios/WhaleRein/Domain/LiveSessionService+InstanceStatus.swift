// Live selected-instance status and default-computer adapter.
//
// Every binding call is typed and read-mostly. Selecting a default Host asks
// Core to verify authorization first; Core persists it inside this instance's
// data directory, so another instance can never observe or overwrite it.

import Foundation
import WhaleReinMobile

extension LiveSessionService: InstanceStatusService {
    func instanceStatus() async throws -> InstanceStatus {
        let core = self.core
        let view = try await call { try core.status() }
        return Self.map(status: view)
    }

    func defaultHost() async throws -> String? {
        let core = self.core
        return try await call { try core.defaultHost() }
    }

    func selectDefaultHost(hostId: String) async throws {
        let core = self.core
        try await call { try core.selectDefaultHost(hostId: hostId) }
    }

    func currentCloudSessionId() async throws -> String? {
        let core = self.core
        return try await call { try core.currentCloudSessionId() }
    }

    private static func map(status view: ClientStatusView) -> InstanceStatus {
        InstanceStatus(
            service: InstanceServiceKind(rawValue: view.service) ?? .standalone,
            testingEnvironment: view.testingEnvironment,
            instanceId: view.instanceId,
            accountState: InstanceAccountState(rawValue: view.accountState) ?? .signedOut,
            remoteSignoutPending: view.remoteSignoutPending,
            defaultHost: view.defaultHost,
            deviceAccess: InstanceDeviceAccess(rawValue: view.deviceAccess) ?? .unavailable,
            configPath: view.configPath,
            dataDir: view.dataDir
        )
    }
}
