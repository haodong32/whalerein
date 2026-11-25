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
    @State private var runtime: AppRuntime?
    @State private var bootstrapTask: Task<AppRuntime, Never>?
    @State private var pendingURLs: [URL] = []

    var body: some Scene {
        WindowGroup {
            Group {
                if let runtime {
                    RootView(app: runtime.legacy, runtime: runtime)
                        .environment(runtime.legacy)
                        .environment(runtime)
                        .id(runtime.activationGeneration)
                        .modifier(DynamicTypeOverride(
                            enabled: runtime.legacy.options.largeText
                        ))
                } else {
                    ProgressView()
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        .task { await bootstrap() }
                }
            }
            .onOpenURL { url in
                if let runtime {
                    Task { await runtime.handleOpenURL(url) }
                } else {
                    pendingURLs.append(url)
                }
            }
        }
    }

    private func bootstrap() async {
        guard runtime == nil else { return }
        let task = bootstrapTask ?? Task { await AppRuntime.bootstrap() }
        bootstrapTask = task
        let startedRuntime = await task.value
        guard runtime == nil else { return }
        runtime = startedRuntime
        bootstrapTask = nil
        let urls = pendingURLs
        pendingURLs.removeAll()
        for url in urls {
            await startedRuntime.handleOpenURL(url)
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
