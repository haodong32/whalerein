// App entry point. Bootstraps the authoritative client document, builds the
// service for the committed instance, honors launch arguments, and injects the
// composition root.
//
// Legacy `preferences.json`/`core/` at the old app root are never read,
// migrated or deleted. New per-instance state lives under
// `Application Support/WhaleRein/client/instances/<id>/`.

import SwiftUI

@main
struct WhaleReinApp: App {
    @State private var runtime: AppRuntime

    init() {
        _runtime = State(initialValue: AppRuntime.bootstrap())
    }

    var body: some Scene {
        WindowGroup {
            RootView(app: runtime.legacy, runtime: runtime)
                .environment(runtime.legacy)
                .environment(runtime)
                .id(runtime.activationGeneration)
                .modifier(DynamicTypeOverride(
                    enabled: runtime.legacy.options.largeText
                ))
        }
    }
}

private struct DynamicTypeOverride: ViewModifier {
    let enabled: Bool

    func body(content: Content) -> some View {
        if enabled {
            content.dynamicTypeSize(.accessibility1)
        } else {
            content.dynamicTypeSize(.xSmall ... .accessibility5)
        }
    }
}
