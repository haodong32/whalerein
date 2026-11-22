// Navigation state only. Routes never hold business state or a service handle;
// a screen derives what it shows from its own feature state.
//
// The legacy model still owns the sheet flags until each feature is rewritten
// (tasks 3.x and 4.x). New surfaces — the Debug state gallery, and the routes
// added by the rewrite — are owned here from the start.

import Foundation
import Observation

enum AppRoute: Equatable, Sendable {
    case settings
    case diagnostics
    case invitation
    case sessionSwitcher
    case serviceSelection
    case conversation
}

@MainActor
@Observable
final class AppRouter {
    /// The route presented as a sheet over the current root, if any.
    var presented: AppRoute?
    /// Root the app is currently showing.
    private(set) var root: AppRoute = .conversation
    #if DEBUG
    /// Debug-only entry into the scenario gallery. Release builds have no
    /// gallery symbol, so the property does not exist there.
    var showsStateGallery = false
    #endif

    func present(_ route: AppRoute) {
        presented = route
    }

    func dismiss() {
        presented = nil
    }

}
