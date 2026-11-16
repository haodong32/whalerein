// Localization depth: count-aware English plural selection, Simplified Chinese
// coverage for error/empty states, and the strings added for lifecycle,
// revocation, incompatibility and approval-recovery states.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Localization depth")
struct LocalizationDepthTests {
    private func bundle(for language: String) -> Bundle? {
        guard let path = Bundle.main.path(forResource: language, ofType: "lproj") else {
            return nil
        }
        return Bundle(path: path)
    }

    private func value(_ key: L10nKey, in bundle: Bundle) -> String {
        bundle.localizedString(forKey: key.rawValue, value: nil, table: nil)
    }

    @Test("message and unread counts select a singular or plural key")
    func pluralSelection() throws {
        #expect(L10nKey.messageCountKey(for: 0) == .conversationMessageCountPlural)
        #expect(L10nKey.messageCountKey(for: 1) == .conversationMessageCount)
        #expect(L10nKey.messageCountKey(for: 2) == .conversationMessageCountPlural)
        #expect(L10nKey.unreadCountKey(for: 1) == .conversationUnreadCount)
        #expect(L10nKey.unreadCountKey(for: 3) == .conversationUnreadCountPlural)

        guard let en = bundle(for: "en"), let zh = bundle(for: "zh-Hans") else {
            Issue.record("localization bundles missing")
            return
        }
        let singular = value(.conversationMessageCount, in: en)
        let plural = value(.conversationMessageCountPlural, in: en)
        #expect(singular.contains("%lld"))
        #expect(plural.contains("%lld"))
        #expect(singular != plural)
        #expect(String(format: singular, 1) == "1 message")
        #expect(String(format: plural, 2) == "2 messages")

        let unreadOne = value(.conversationUnreadCount, in: en)
        let unreadMany = value(.conversationUnreadCountPlural, in: en)
        #expect(String(format: unreadOne, 1) == "1 unread message")
        #expect(String(format: unreadMany, 2) == "2 unread messages")
        #expect(value(.conversationUnreadCount, in: zh).contains("%lld"))
        #expect(value(.conversationUnreadCountPlural, in: zh).contains("%lld"))
    }

    @Test("Simplified Chinese covers error and empty states with real translations")
    func chineseStates() throws {
        guard let en = bundle(for: "en"), let zh = bundle(for: "zh-Hans") else {
            Issue.record("localization bundles missing")
            return
        }
        let keys: [L10nKey] = [
            .errorRevoked,
            .errorNotPaired,
            .errorHostOffline,
            .errorUnavailable,
            .errorTimeout,
            .sessionsEmpty,
            .hostsEmpty,
            .conversationNoHostsTitle,
            .conversationNoSessionsTitle,
            .conversationEmpty,
            .revokedTitle,
            .revokedBody,
            .conversationIncompatibleTitle,
            .conversationIncompatibleBody,
            .approvalUnavailable,
            .approvalUnavailableBody,
            .approvalOffline,
            .approvalBusy,
            .privacyShieldTitle,
        ]
        for key in keys {
            let english = value(key, in: en)
            let chinese = value(key, in: zh)
            #expect(!english.isEmpty, "empty en value for \(key.rawValue)")
            #expect(!chinese.isEmpty, "empty zh value for \(key.rawValue)")
            #expect(english != key.rawValue, "missing en translation for \(key.rawValue)")
            #expect(chinese != key.rawValue, "missing zh translation for \(key.rawValue)")
        }
        // Spot-check that the Chinese states are real translations, not copied
        // English placeholders.
        #expect(value(.errorRevoked, in: zh) != value(.errorRevoked, in: en))
        #expect(value(.sessionsEmpty, in: zh) != value(.sessionsEmpty, in: en))
        #expect(value(.conversationIncompatibleBody, in: zh).contains("%@"))
        #expect(value(.conversationIncompatibleBody, in: en).contains("%@"))
    }

    @Test("new user-facing feature strings are translated in both languages")
    func newFeatureStrings() throws {
        guard let en = bundle(for: "en"), let zh = bundle(for: "zh-Hans") else {
            Issue.record("localization bundles missing")
            return
        }
        let keys: [L10nKey] = [
            .privacyShieldTitle,
            .privacyShieldBody,
            .hostsAdd,
            .settingsAddHost,
            .revokedTitle,
            .revokedBody,
            .revokedPair,
            .revokedForget,
            .conversationIncompatibleTitle,
            .conversationIncompatibleBody,
            .conversationReconnect,
            .conversationWatchEndedHint,
            .conversationCodePlain,
            .conversationToolOutput,
            .pairingLinkQueued,
        ]
        for key in keys {
            #expect(value(key, in: en) != key.rawValue, "missing en \(key.rawValue)")
            #expect(value(key, in: zh) != key.rawValue, "missing zh \(key.rawValue)")
        }
    }
}
