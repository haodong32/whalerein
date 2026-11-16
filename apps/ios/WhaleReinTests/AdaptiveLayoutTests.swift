// Breakpoint behavior for the adaptive compact/wide switcher.

import Testing
@testable import WhaleRein

@Suite("Adaptive layout")
struct AdaptiveLayoutTests {
    @Test("widths below the breakpoint keep the compact sheet switcher")
    func belowBreakpoint() {
        #expect(!AdaptiveLayout.showsSidebar(width: 719))
        #expect(!AdaptiveLayout.showsSidebar(width: 719.99))
        #expect(!AdaptiveLayout.showsSidebar(width: 393))
    }

    @Test("the breakpoint itself shows the persistent sidebar")
    func atBreakpoint() {
        #expect(AdaptiveLayout.showsSidebar(width: AdaptiveLayout.sidebarBreakpoint))
    }

    @Test("widths above the breakpoint show the persistent sidebar")
    func aboveBreakpoint() {
        #expect(AdaptiveLayout.showsSidebar(width: 721))
        #expect(AdaptiveLayout.showsSidebar(width: 844))
        #expect(AdaptiveLayout.showsSidebar(width: 1366))
    }

    @Test("the sidebar uses the documented fixed width")
    func sidebarWidth() {
        #expect(AdaptiveLayout.sidebarBreakpoint == 720)
        #expect(AdaptiveLayout.sidebarWidth == 300)
        #expect(AdaptiveLayout.sidebarWidth < AdaptiveLayout.sidebarBreakpoint)
    }

    @Test("representative device widths choose the expected switcher")
    func deviceWidths() {
        // iPhone 17 portrait and landscape, iPad mini portrait and iPad Pro
        // landscape, and a narrow Mac Catalyst window.
        #expect(!AdaptiveLayout.showsSidebar(width: 393))
        #expect(AdaptiveLayout.showsSidebar(width: 852))
        #expect(AdaptiveLayout.showsSidebar(width: 744))
        #expect(AdaptiveLayout.showsSidebar(width: 1376))
        #expect(!AdaptiveLayout.showsSidebar(width: 680))
    }
}
