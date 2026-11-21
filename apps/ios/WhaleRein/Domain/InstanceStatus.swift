// Typed selected-instance status and the read-only Cloud session identity used
// by Settings account sections.
//
// These values mirror Core's `status` contract and the locally retained Cloud
// session identity. They never carry credentials, tokens or account secrets;
// presentation depends only on this domain boundary so the fixture and live
// adapters can supply the same typed states.

import Foundation

enum InstanceServiceKind: String, Equatable, Sendable {
    case cloud = "whalerein_cloud"
    case standalone

    var isCloud: Bool { self == .cloud }
}

enum InstanceAccountState: String, Equatable, Sendable {
    case notApplicable = "not_applicable"
    case signedIn = "signed_in"
    case signedOut = "signed_out"
}

enum InstanceDeviceAccess: String, Equatable, Sendable {
    case unpaired
    case ready
    case revocationPending = "revocation_pending"
    case revoked
    case unavailable
    case signInRequired = "sign_in_required"
}

struct InstanceStatus: Equatable, Sendable {
    let service: InstanceServiceKind
    let testingEnvironment: String?
    let instanceId: String?
    let accountState: InstanceAccountState
    let remoteSignoutPending: Bool
    let defaultHost: String?
    let deviceAccess: InstanceDeviceAccess
    let configPath: String
    let dataDir: String
}

/// Core-backed status, default Host and Cloud session identity for one
/// instance. `RecoverableSessionService` forwards these once construction
/// succeeds; an unavailable service keeps them typed failures.
@MainActor
protocol InstanceStatusService: AnyObject {
    func instanceStatus() async throws -> InstanceStatus
    func defaultHost() async throws -> String?
    func selectDefaultHost(hostId: String) async throws
    func currentCloudSessionId() async throws -> String?
}

/// Localized labels for typed status values. Keeping the mapping in the domain
/// layer lets fixtures and tests assert the exact presentation without a view.
extension InstanceAccountState {
    var labelKey: L10nKey {
        switch self {
        case .notApplicable: return .settingsAccountNotApplicable
        case .signedIn: return .settingsAccountSignedIn
        case .signedOut: return .settingsAccountSignedOut
        }
    }
}

extension InstanceDeviceAccess {
    var labelKey: L10nKey {
        switch self {
        case .unpaired: return .settingsDeviceUnpaired
        case .ready: return .settingsDeviceReady
        case .revocationPending: return .settingsDeviceRevocationPending
        case .revoked: return .settingsDeviceRevoked
        case .unavailable: return .settingsDeviceUnavailable
        case .signInRequired: return .settingsDeviceSignInRequired
        }
    }
}

extension InstanceServiceKind {
    var labelKey: L10nKey {
        switch self {
        case .cloud: return .settingsServiceCloud
        case .standalone: return .settingsServiceStandalone
        }
    }
}
