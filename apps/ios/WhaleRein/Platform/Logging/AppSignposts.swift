// Stable signposts for launch and first content. Instrument.app and any
// Simulator trace can rely on these names; AC-20 verifies they exist.

import Foundation
import os

enum AppSignposts {
    static let signposter = OSSignposter(
        subsystem: Bundle.main.bundleIdentifier ?? "com.whalerein.app",
        category: "performance"
    )

    /// Starts the cold-launch interval. Called from `AppRuntime.bootstrap`.
    /// A fresh id each time keeps a runtime rebuild from reusing a finished
    /// interval.
    static func beginLaunch() -> OSSignpostIntervalState {
        signposter.beginInterval("app.launch", id: signposter.makeSignpostID())
    }

    static func endLaunch(_ state: OSSignpostIntervalState) {
        signposter.endInterval("app.launch", state)
    }

    /// Marks the first rendered conversation content as a point event.
    static func markFirstConversation() {
        signposter.emitEvent("conversation.first_render")
        AppLog.shared.log(.info, .conversation, "conversation.first_render")
    }

}
