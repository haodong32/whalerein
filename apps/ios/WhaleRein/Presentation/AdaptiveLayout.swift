// Pure layout decisions shared by SwiftUI views and unit tests. Keeping the
// breakpoint out of the view body makes the compact/wide switch deterministic
// and easy to verify without a running window.

import Foundation

enum AdaptiveLayout {
    /// Width at which the persistent Host/session sidebar replaces the
    /// bottom-sheet switcher. 720 pt keeps compact iPhones (including the
    /// 430 pt portrait maximum) on the sheet while every iPhone landscape
    /// width, iPad split view, and reasonable Mac Catalyst window shows the
    /// sidebar.
    static let sidebarBreakpoint: Double = 720

    /// Fixed width of the persistent sidebar. The conversation takes the
    /// remaining width, so the user can always resize a Mac Catalyst window
    /// across the breakpoint without a layout jump in the sidebar itself.
    static let sidebarWidth: Double = 300

    /// True when the available width is wide enough for the persistent
    /// sidebar. The comparison is inclusive: exactly 720 pt shows the sidebar.
    static func showsSidebar(width: Double) -> Bool {
        width >= sidebarBreakpoint
    }
}
