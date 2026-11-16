// Small platform shims for controls whose SwiftUI/UIKit spelling is not
// available on every supported SDK. Each shim keeps the existing iOS behavior
// and degrades gracefully on Mac Catalyst instead of removing the feature.

import SwiftUI

#if canImport(UIKit)
import UIKit
#endif

#if canImport(AppKit) && !targetEnvironment(macCatalyst)
import AppKit
#endif

extension View {
    /// Inline navigation titles on iOS/iPadOS. The modifier is not available
    /// on Mac Catalyst, where the platform supplies its own window title bar.
    @ViewBuilder
    func inlineNavigationTitle() -> some View {
        #if targetEnvironment(macCatalyst)
        self
        #else
        self.navigationBarTitleDisplayMode(.inline)
        #endif
    }
}

/// Clipboard access used by invitation paste and code-block copy. Catalyst and
/// iOS use UIKit; a future plain-macOS target falls back to AppKit.
enum PlatformPasteboard {
    static var string: String? {
        #if canImport(UIKit)
        return UIPasteboard.general.string
        #elseif canImport(AppKit)
        return NSPasteboard.general.string(forType: .string)
        #else
        return nil
        #endif
    }

    static func setString(_ value: String) {
        #if canImport(UIKit)
        UIPasteboard.general.string = value
        #elseif canImport(AppKit)
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(value, forType: .string)
        #endif
    }
}


extension View {
    /// iOS 26 Liquid Glass for floating controls and composer surfaces, with a
    /// native material fallback on the iOS 18 deployment target and Catalyst
    /// versions that predate the glass APIs.
    @ViewBuilder
    func liquidGlass<S: Shape>(in shape: S) -> some View {
        if #available(iOS 26.0, *) {
            self.glassEffect(.regular.interactive(), in: shape)
        } else {
            self.background(.regularMaterial, in: shape)
        }
    }

    /// Native glass button styles on iOS 26; bordered styles keep the same
    /// hierarchy on older systems.
    @ViewBuilder
    func glassButtonStyle(prominent: Bool = false, controlSize: ControlSize = .regular) -> some View {
        if #available(iOS 26.0, *) {
            if prominent {
                self.buttonStyle(.glassProminent).controlSize(controlSize)
            } else {
                self.buttonStyle(.glass).controlSize(controlSize)
            }
        } else {
            if prominent {
                self.buttonStyle(.borderedProminent).controlSize(controlSize)
            } else {
                self.buttonStyle(.bordered).controlSize(controlSize)
            }
        }
    }
}
