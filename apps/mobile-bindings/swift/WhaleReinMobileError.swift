// Stable code and message accessors for the flat UniFFI error enum.
//
// This file is generated into the app's vendor directory by
// apps/mobile-bindings/scripts/build-xcframework.sh. Keep the switch in sync
// with `MobileError` in apps/mobile-bindings/src/error.rs.

import Foundation

public extension MobileError {
    /// Stable machine-readable code used for branching and logs.
    var code: String {
        switch self {
        case .InvalidInput: return "invalid_input"
        case .NotPaired: return "not_paired"
        case .NotFound: return "not_found"
        case .Revoked: return "revoked"
        case .HostOffline: return "host_offline"
        case .Unavailable: return "unavailable"
        case .Conflict: return "conflict"
        case .Timeout: return "timeout"
        case .Cancelled: return "cancelled"
        case .StaleHistory: return "stale_history"
        case .Security: return "security"
        case .Internal: return "internal"
        case .ProtocolVersionIncompatible: return "protocol_version_incompatible"
        case .UnsupportedCapability: return "unsupported_capability"
        }
    }

    /// Neutral, localized-neutral explanation. Never contains secrets,
    /// message content, invitation tokens or full URIs.
    var message: String {
        switch self {
        case let .InvalidInput(message): return message
        case let .NotPaired(message): return message
        case let .NotFound(message): return message
        case let .Revoked(message): return message
        case let .HostOffline(message): return message
        case let .Unavailable(message): return message
        case let .Conflict(message): return message
        case let .Timeout(message): return message
        case let .Cancelled(message): return message
        case let .StaleHistory(message): return message
        case let .Security(message): return message
        case let .Internal(message): return message
        case let .ProtocolVersionIncompatible(message): return message
        case let .UnsupportedCapability(message): return message
        }
    }
}
