// Keychain availability and credential recovery.
//
// A device that reboots and is opened before its first unlock cannot read
// Keychain items. The app must show a recoverable state, keep the retained
// account/pairing, and restore the exact same account and Host/session once the
// device is unlocked. The physical-device pre-first-unlock pass cannot run on
// the Simulator, so that case is explicitly disabled and is not reported as a
// pass.

import Foundation
import Testing
@testable import WhaleRein

private final class LockedKeychainState {
    var unlocked = false
    var factoryCalls = 0
}

@Suite("Keychain credential recovery")
@MainActor
struct KeychainCredentialRecoveryTests {
    private func cloudInstance(_ directory: URL) -> AppInstance {
        AppInstance(
            id: "cloud-production",
            service: .cloud,
            environment: .production,
            dataDirectory: directory,
            configPath: directory.appendingPathComponent("instance.toml"),
            hasStandaloneCA: false,
            standaloneCAPath: nil
        )
    }

    private func makeApp(
        state: LockedKeychainState,
        directory: URL,
        preferences: PreferencesStore,
        fixture: FixtureSessionService
    ) -> (AppShellModel, RecoverableSessionService) {
        let recoverable = RecoverableSessionService(instanceId: "cloud-production") {
            state.factoryCalls += 1
            guard state.unlocked else {
                throw SessionServiceError(.keychainUnavailable)
            }
            return fixture
        }
        let options = testLaunchOptions(["-ui-fixtures", "-ui-test-reset", "-ui-cloud-signed-in"])
        let app = AppShellModel(
            service: recoverable,
            preferences: preferences,
            dataDirectory: directory,
            options: options,
            activeInstance: cloudInstance(directory)
        )
        return (app, recoverable)
    }

    private func makeFixture() -> FixtureSessionService {
        FixtureSessionService(
            options: testLaunchOptions(["-ui-fixtures", "-ui-test-reset", "-ui-cloud-signed-in"]),
            draftStore: InMemoryDraftStore(),
            initiallyPaired: true,
            instanceId: "cloud-production",
            serviceKind: .cloud
        )
    }

    @Test("unavailable Keychain credentials keep the Cloud account surface recoverable")
    func unavailableKeychainKeepsAccountSurface() async {
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-001"
            $0.lastSessionTitle = "Release checklist"
        }
        let state = LockedKeychainState()
        let fixture = makeFixture()
        let (app, recoverable) = makeApp(
            state: state,
            directory: directory,
            preferences: preferences,
            fixture: fixture
        )

        // The recoverable wrapper must still expose the Cloud account boundary;
        // hiding it would strand the user on a signed-out surface after unlock.
        #expect(app.cloudSignIn != nil)
        await app.launchRestore()

        #expect(app.rootState == .failed(.keychainUnavailable))
        #expect(app.conversation == nil)
        #expect(app.isPaired, "a Keychain failure must not discard the pairing")
        #expect(app.cloudSignIn?.phase == .signedOut)
        #expect(app.cloudSignIn?.errorCode == .keychainUnavailable)
        #expect(app.cloudSignIn?.account == nil)
        #expect(preferences.value.paired)
        #expect(preferences.value.lastHostId == "studio-mac")
        #expect(!recoverable.isRecovered)

        let callsAfterLaunch = state.factoryCalls
        await pump(100)
        #expect(state.factoryCalls == callsAfterLaunch, "no background retry loop is allowed")
    }

    @Test("after unlock the same Cloud account and pairing are restored")
    func unlockRestoresSameAccountAndPairing() async {
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-001"
            $0.lastSessionTitle = "Release checklist"
        }
        let state = LockedKeychainState()
        let fixture = makeFixture()
        let (app, recoverable) = makeApp(
            state: state,
            directory: directory,
            preferences: preferences,
            fixture: fixture
        )
        await app.launchRestore()
        #expect(app.rootState == .failed(.keychainUnavailable))

        state.unlocked = true
        await app.retryLaunch()

        #expect(recoverable.isRecovered)
        #expect(app.rootState == .ready)
        #expect(app.conversation?.hostId == "studio-mac")
        #expect(app.conversation?.sessionId == "s-001")
        #expect(app.conversation?.sessionTitle == "Release checklist")
        #expect(app.cloudSignIn?.account == FixtureCatalog.cloudAccount)
        #expect(app.cloudSignIn?.errorCode == nil, "the recovered state must clear the stale Keychain error")
        #expect(app.cloudSignIn?.directory.hosts.map(\.hostId) == FixtureCatalog.cloudHosts.map(\.hostId))
        #expect(preferences.value.paired)
        #expect(preferences.value.lastHostId == "studio-mac")
        #expect(preferences.value.lastSessionId == "s-001")
    }

    @Test("foregrounding after unlock restores the Cloud account in the same activation")
    func foregroundRestoresCloudAccount() async {
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-001"
        }
        let state = LockedKeychainState()
        let (app, recoverable) = makeApp(
            state: state,
            directory: directory,
            preferences: preferences,
            fixture: makeFixture()
        )
        await app.launchRestore()
        #expect(app.rootState == .failed(.keychainUnavailable))

        state.unlocked = true
        await app.enterBackground()
        await app.becomeActive()

        #expect(recoverable.isRecovered)
        #expect(app.conversation?.sessionId == "s-001")
        #expect(app.cloudSignIn?.account == FixtureCatalog.cloudAccount)
        #expect(app.cloudSignIn?.errorCode == nil)
    }

    @Test(
        "a physical device restores credentials after first unlock",
        .disabled(Comment(
            rawValue: "Requires a physical iPhone: the Simulator Keychain is always "
                + "unlocked, so the pre-first-unlock unavailable state cannot be "
                + "reproduced here."
        ))
    )
    func physicalDeviceFirstUnlock() async {
        // Intentionally unexecuted on the Simulator. The injectable cases above
        // cover classification, recoverable state and restoration; only the OS
        // pre-first-unlock behaviour needs the device.
    }
}
