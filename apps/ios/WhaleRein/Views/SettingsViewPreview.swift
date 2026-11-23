// Settings preview kept out of the screen body so the view stays within the
// client architecture file budget.

import SwiftUI

#if DEBUG
#Preview("Settings fixture") {
    if let scenario = ScenarioCatalog.scenario(id: "settings.baseline") {
        let directory = ScenarioBuilder.temporaryDirectory()
        let service = ScenarioBuilder.service(for: scenario, directory: directory)
        SettingsView(
            settings: SettingsModel(
                service: service,
                preferences: PreferencesStore(directory: directory),
                dataDirectory: directory,
                isStaging: false,
                isFixtureBacked: true,
                onDeviceReset: {}
            ),
            diagnostics: DiagnosticsModel(
                service: service,
                stagingDirectory: directory
            ),
            account: nil,
            operations: nil,
            cloud: nil,
            dataPath: directory.path,
            serviceSelection: ServiceSelectionContext(
                configuration: nil,
                activeInstance: nil,
                actions: ServiceSelectionActions(
                    switchToService: { _ in nil },
                    enableLocal: { _ in nil },
                    removeLocalCA: { nil },
                    importStandaloneCA: { _ in nil },
                    removeStandaloneCA: { nil }
                )
            ),
            hostConnectivity: HostConnectivityModel(),
            reasoningVisible: false,
            setReasoningVisible: { _ in nil },
            onSignOutCloud: {},
            onAddHost: {}
        )
    }
}
#endif
