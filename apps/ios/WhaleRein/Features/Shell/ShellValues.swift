// Shell-level values shared by the runtime and the root screen.

import Foundation

enum ConversationEmptyReason: Equatable, Sendable {
    case noHosts
    case noSessions
}

enum ConversationRootState: Equatable, Sendable {
    case restoring
    case ready
    case empty(ConversationEmptyReason)
    case failed(AppErrorCode)
    /// The authorized Host reports a protocol line this client cannot use.
    case incompatible(hostId: String, version: String)
    /// Core reported a non-retryable upgrade condition while loading. This is
    /// distinct from a generic unavailable error and never offers a blind retry
    /// as the primary route.
    case upgradeRequired(AppErrorCode)
}

enum AppScenePhase: Equatable, Sendable {
    case active
    case inactive
    case background
}

struct AppResumeMarker: Equatable, Sendable {
    let recordedAt: Date
    let hostId: String?
    let sessionId: String?
}
