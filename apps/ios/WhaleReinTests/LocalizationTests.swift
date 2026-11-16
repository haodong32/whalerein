// Localization completeness for English and Simplified Chinese, plus localized
// error/status selection used by presentation models.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Localization")
struct LocalizationTests {
    private func bundle(for language: String) -> Bundle? {
        guard let path = Bundle.main.path(forResource: language, ofType: "lproj") else {
            return nil
        }
        return Bundle(path: path)
    }

    private func value(_ key: L10nKey, in bundle: Bundle) -> String {
        bundle.localizedString(forKey: key.rawValue, value: nil, table: nil)
    }

    @Test("the app bundle contains English and Simplified Chinese tables")
    func tablesExist() {
        #expect(bundle(for: "en") != nil, "en.lproj missing from the app bundle")
        #expect(bundle(for: "zh-Hans") != nil, "zh-Hans.lproj missing from the app bundle")
    }

    @Test("every key is translated in both languages")
    func completeness() throws {
        guard let en = bundle(for: "en"), let zh = bundle(for: "zh-Hans") else {
            Issue.record("localization bundles missing")
            return
        }
        #expect(L10nKey.allCases.count >= 150)
        for key in L10nKey.allCases {
            let english = value(key, in: en)
            let chinese = value(key, in: zh)
            #expect(!english.isEmpty, "empty en value for \(key.rawValue)")
            #expect(!chinese.isEmpty, "empty zh-Hans value for \(key.rawValue)")
            #expect(english != key.rawValue, "missing en translation for \(key.rawValue)")
            #expect(chinese != key.rawValue, "missing zh-Hans translation for \(key.rawValue)")
        }
    }

    @Test("Simplified Chinese differs from English where text is translated")
    func chineseDiffers() throws {
        guard let en = bundle(for: "en"), let zh = bundle(for: "zh-Hans") else {
            Issue.record("localization bundles missing")
            return
        }
        #expect(value(.invitationTitle, in: zh) == "配对你的 iPhone")
        #expect(value(.pairingConfirm, in: zh) == "确认并配对")
        #expect(value(.invitationTitle, in: en) == "Pair your iPhone")
        #expect(value(.invitationTitle, in: en) != value(.invitationTitle, in: zh))
    }

    @Test("format strings keep their specifiers in both languages")
    func formatSpecifiers() throws {
        guard let en = bundle(for: "en"), let zh = bundle(for: "zh-Hans") else {
            Issue.record("localization bundles missing")
            return
        }
        for key in [L10nKey.sessionsUpdatedMinutes, .sessionsUpdatedHours, .sessionsUpdatedDays, .approvalVersion] {
            #expect(value(key, in: en).contains("%lld"), "en \(key.rawValue) lost %lld")
            #expect(value(key, in: zh).contains("%lld"), "zh \(key.rawValue) lost %lld")
        }
        #expect(value(.diagnosticsLastFailure, in: en).contains("%@"))
        #expect(value(.diagnosticsLastFailure, in: zh).contains("%@"))
    }

    @Test("connection status labels are localized and distinguish states")
    func connectionLabels() {
        let keys = ConnectionStatus.allCases.map { L10nKey.connectionKey(for: $0) }
        #expect(Set(keys).count == ConnectionStatus.allCases.count)
        for key in keys {
            #expect(!key.string.isEmpty)
            #expect(key.string != key.rawValue)
        }
    }

    @Test("command status labels are localized")
    func commandLabels() {
        for status in [CommandStatus.accepted, .uncertain, .confirmed, .rejected, .cancelled] {
            let key = L10nKey.commandKey(for: status)
            #expect(!key.string.isEmpty)
            #expect(key.string != key.rawValue)
        }
    }

    @Test("every error code maps to a localized message")
    func errorLabels() {
        for code in AppErrorCode.allCases {
            let key = L10nKey.errorKey(for: code)
            #expect(!key.string.isEmpty)
            #expect(key.string != key.rawValue)
        }
        #expect(L10nKey.errorKey(for: .notPaired) == .errorNotPaired)
        #expect(L10nKey.errorKey(for: .integrationUnavailable) == .errorIntegrationUnavailable)
    }

    @Test("keys are unique")
    func uniqueKeys() {
        let rawValues = L10nKey.allCases.map(\.rawValue)
        #expect(Set(rawValues).count == rawValues.count)
    }
}
