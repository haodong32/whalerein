// Inputs and intents the client-service selector needs.

import Foundation

/// Intents the selector needs from the shell.
struct ServiceSelectionActions {
    var switchToService: (AppServiceOption) async -> AppErrorCode?
    var enableLocal: (String) async -> AppErrorCode?
    var removeLocalCA: () async -> AppErrorCode?
    var importStandaloneCA: (String) async -> AppErrorCode?
    var removeStandaloneCA: () async -> AppErrorCode?
}

/// Everything the selector needs, bundled so screens can forward it.
struct ServiceSelectionContext {
    let configuration: AppConfigurationStore?
    let activeInstance: AppInstance?
    let actions: ServiceSelectionActions
}

@MainActor
extension ServiceSelectionContext {
        var options: [AppServiceOption] {
            guard let configuration = configuration else { return [] }
            return AppConfigurationStore.serviceOptions(
                for: configuration.instances,
                showsStaging: AppConfigurationStore.defaultShowsStaging,
                showsLocal: AppConfigurationStore.defaultShowsLocal
            )
        }

        var local: AppInstance? {
            configuration?.instances.first(where: { $0.isLocal })
        }

        /// Local can be created only together with its imported mkcert root.
        func needsLocalImport(_ option: AppServiceOption) -> Bool {
            option.kind == .createLocal && !(local?.hasCloudCA ?? false)
        }

        var standalone: AppInstance? {
            configuration?.instances.first(where: { $0.isStandalone })
        }
}
