// Stable signposts for launch and first content. Instrument.app and any
// Simulator trace can rely on these names; AC-20 verifies they exist.

import Foundation
import os

enum AppSignposts {
    static let signposter = OSSignposter(
        subsystem: Bundle.main.bundleIdentifier ?? "com.whalerein.app",
        category: "performance"
    )

    static let launch = signposter.makeSignpostID()
    static let firstConversation = signposter.makeSignpostID()

    /// Starts the cold-launch interval. Called from `AppRuntime.bootstrap`.
    static func beginLaunch() -> OSSignpostIntervalState {
        signposter.beginInterval("app.launch", id: launch)
    }

    static func endLaunch(_ state: OSSignpostIntervalState) {
        signposter.endInterval("app.launch", state)
    }

    /// First rendered conversation content. Emitted once per runtime group.
    static func beginFirstConversation() -> OSSignpostIntervalState {
        signposter.beginInterval("conversation.first_render", id: firstConversation)
    }

    static func endFirstConversation(_ state: OSSignpostIntervalState) {
        signposter.endInterval("conversation.first_render", state)
    }
}
