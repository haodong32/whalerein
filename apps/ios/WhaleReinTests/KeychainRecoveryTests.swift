// Locked/unavailable Keychain recovery. A device that reboots and is opened
// before its first unlock cannot read Keychain items; the app must show an
// actionable state and retry only on an explicit action or foreground
// transition, never in a tight loop.

import Foundation
import Security
import Testing
@testable import WhaleRein

@Suite("Keychain recovery")
@MainActor
struct KeychainRecoveryTests {
    private final class FactoryState {
        var shouldSucceed = false
        var calls = 0
    }

    private func makeRecoverable(state: FactoryState, service: StubSessionService) -> RecoverableSessionService {
        RecoverableSessionService(failureCode: .keychainUnavailable) {
            state.calls += 1
            guard state.shouldSucceed else {
                throw SessionServiceError(.keychainUnavailable)
            }
            return service
        }
    }

    @Test("Keychain statuses classify as available, locked or failed")
    func availabilityClassification() {
        #expect(KeychainSecretStore.availability(for: errSecSuccess) == .available)
        #expect(KeychainSecretStore.availability(for: errSecItemNotFound) == .available)
        #expect(KeychainSecretStore.availability(for: errSecInteractionNotAllowed) == .locked)
        #expect(KeychainSecretStore.availability(for: errSecNotAvailable) == .locked)
        #expect(KeychainSecretStore.availability(for: errSecIO) == .failed)
    }

    @Test("a failing Keychain factory attempts once per call and recovers on a later call")
    func singleAttemptPerCall() async throws {
        let state = FactoryState()
        let service = makeRecoverable(state: state, service: StubSessionService())

        await #expect(throws: SessionServiceError.self) {
            _ = try await service.hosts()
        }
        #expect(state.calls == 1, "One operation must not retry in a loop")
        #expect(service.lastFailureCode == .keychainUnavailable)
        #expect(!service.isRecovered)

        await pump(100)
        #expect(state.calls == 1, "No background retry loop is allowed")

        state.shouldSucceed = true
        let hosts = try await service.hosts()
        #expect(hosts.count == 2)
        #expect(service.isRecovered)
        #expect(state.calls == 2)
    }

    @Test("a paired app shows the recoverable Keychain state and retries after unlock")
    func appStateRecovers() async {
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-001"
        }
        let state = FactoryState()
        let stub = StubSessionService()
        let service = makeRecoverable(state: state, service: stub)
        let app = AppModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )

        await app.launchRestore()

        #expect(app.rootState == .failed(.keychainUnavailable))
        #expect(app.conversation == nil)
        #expect(app.isPaired, "A Keychain failure must not discard the pairing")
        #expect(state.calls == 1)

        state.shouldSucceed = true
        await app.retryLaunch()

        #expect(app.conversation?.sessionId == "s-001")
        #expect(app.rootState == .ready)
        #expect(service.isRecovered)
    }

    @Test("foregrounding after unlock retries the locked Keychain once")
    func foregroundRecovery() async {
        let directory = makeTempDirectory()
        let preferences = PreferencesStore(directory: directory)
        preferences.update {
            $0.paired = true
            $0.lastHostId = "studio-mac"
            $0.lastSessionId = "s-001"
        }
        let state = FactoryState()
        let service = makeRecoverable(state: state, service: StubSessionService())
        let app = AppModel(
            service: service,
            preferences: preferences,
            dataDirectory: directory,
            options: testLaunchOptions()
        )

        await app.launchRestore()
        #expect(app.rootState == .failed(.keychainUnavailable))

        state.shouldSucceed = true
        await app.enterBackground()
        await app.becomeActive()

        #expect(app.conversation != nil)
        #expect(service.isRecovered)
    }

    @Test("the Keychain error is localized and distinct from a generic failure")
    func localizedError() {
        #expect(L10nKey.errorKey(for: .keychainUnavailable) == .errorKeychainUnavailable)
        #expect(!L10nKey.keychainLockedTitle.string.isEmpty)
        #expect(!L10nKey.keychainLockedBody.string.isEmpty)
        #expect(L10nKey.keychainLockedTitle.string != L10nKey.keychainLockedTitle.rawValue)
    }
}
