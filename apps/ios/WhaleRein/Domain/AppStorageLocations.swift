// Local storage layout shared by the app and tests. Secrets do not belong in
// UserDefaults; the Keychain service names are recorded here for the adapter the
// mobile-bindings agent will add.

import CryptoKit
import Foundation

enum AppStorageLocations {
    static let dataDirectoryName = "WhaleRein"
    static let draftsDirectoryName = "drafts"
    static let preferencesFileName = "preferences.json"
    static let fixtureStateFileName = "fixture-state.json"

    static let keychainService = "com.whalerein.ios.secrets"
    static let stagingKeychainService = "com.whalerein.ios.staging.secrets"

    static func applicationSupportDirectory(fileManager: FileManager = .default) -> URL {
        let base = fileManager.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? fileManager.temporaryDirectory
        return base.appendingPathComponent(dataDirectoryName, isDirectory: true)
    }

    /// Creates the app data directory with complete file protection and returns it.
    @discardableResult
    static func prepareDataDirectory(fileManager: FileManager = .default) throws -> URL {
        let directory = applicationSupportDirectory(fileManager: fileManager)
        try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        #if os(iOS)
        try? fileManager.setAttributes(
            [.protectionKey: FileProtectionType.complete],
            ofItemAtPath: directory.path
        )
        #endif
        return directory
    }

    static func resetDataDirectory(fileManager: FileManager = .default) throws -> URL {
        let directory = applicationSupportDirectory(fileManager: fileManager)
        if fileManager.fileExists(atPath: directory.path) {
            try fileManager.removeItem(at: directory)
        }
        return try prepareDataDirectory(fileManager: fileManager)
    }

    static func draftsDirectory(in dataDirectory: URL, fileManager: FileManager = .default) throws -> URL {
        let directory = dataDirectory.appendingPathComponent(draftsDirectoryName, isDirectory: true)
        try fileManager.createDirectory(at: directory, withIntermediateDirectories: true)
        #if os(iOS)
        try? fileManager.setAttributes(
            [.protectionKey: FileProtectionType.complete],
            ofItemAtPath: directory.path
        )
        #endif
        return directory
    }

    /// Stable, filesystem-safe key for a Host-qualified session.
    static func sessionKey(hostId: String, sessionId: String) -> String {
        let raw = "\(hostId)\u{1F}\(sessionId)"
        let digest = SHA256.hash(data: Data(raw.utf8))
        return digest.map { String(format: "%02x", $0) }.joined()
    }
}
