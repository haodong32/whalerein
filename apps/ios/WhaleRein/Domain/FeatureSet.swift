// Capabilities decided once per runtime build, so no view or model probes a
// service with `as?` at runtime.
//
// The set is computed from the materialized instance and the service the
// assembly could actually build. Feature models receive a concrete adapter or
// an explicitly unavailable implementation chosen from this value.

import Foundation

struct FeatureSet: Equatable, Sendable {
    /// The instance exposes a Cloud account surface (sign-in, directory).
    var cloudAccount: Bool
    /// The instance can list, select and manage Hosts and sessions.
    var sessionManagement: Bool
    /// The instance can page conversation history.
    var history: Bool
    /// The instance can transfer attachments.
    var attachments: Bool
    /// The instance reports Host/session status and connection state.
    var instanceStatus: Bool
    /// The instance persists retained operations that can be recovered.
    var operationRecovery: Bool

    /// Every capability the client currently implements. Used by fixtures and
    /// by a Live instance that opened successfully.
    static let all = FeatureSet(
        cloudAccount: true,
        sessionManagement: true,
        history: true,
        attachments: true,
        instanceStatus: true,
        operationRecovery: true
    )

    /// A service that could not be opened: onboarding still renders, but no
    /// paired surface claims a capability it cannot serve.
    static let unavailable = FeatureSet(
        cloudAccount: false,
        sessionManagement: false,
        history: false,
        attachments: false,
        instanceStatus: false,
        operationRecovery: false
    )

    /// Capabilities derive from the instance the runtime actually opened.
    /// A standalone Host has no Cloud account surface; every other capability
    /// follows the service in use.
    static func resolved(
        instance: AppInstance?,
        serviceIsAvailable: Bool
    ) -> FeatureSet {
        guard serviceIsAvailable else { return .unavailable }
        var set = FeatureSet.all
        set.cloudAccount = instance?.service == .cloud
        return set
    }
}
