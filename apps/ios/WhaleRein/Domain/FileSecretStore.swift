// Developer-only file secret store for the local Cloud instance.
//
// The local environment exists for development against the desktop test
// stack, where a Keychain namespace is not always usable (unsigned or
// freshly reset Simulator builds). This store keeps the same SecretStore
// contract, writes one file per key inside the instance directory, marks the
// directory 0700 and complete file protection, and excludes it from backup.
// It is selected only for `cloud-local` in DEBUG builds. Production and every
// other instance keep the Keychain store.

import CryptoKit
import Foundation
import WhaleReinMobile

final class FileSecretStore: SecretStore, @unchecked Sendable {
    private let directory: URL
    private let fileManager: FileManager
    private let lock = NSLock()

    init(directory: URL, fileManager: FileManager = .default) throws {
        self.directory = directory
        self.fileManager = fileManager
        try prepareDirectory()
    }

    func load(key: String) throws -> Data? {
        lock.lock()
        defer { lock.unlock() }
        let url = fileURL(for: key)
        do {
            return try Data(contentsOf: url)
        } catch let error as NSError
            where error.domain == NSCocoaErrorDomain
                && error.code == NSFileReadNoSuchFileError
        {
            return nil
        } catch {
            throw SecretStoreError.Failed
        }
    }

    func save(key: String, value: Data) throws {
        lock.lock()
        defer { lock.unlock() }
        var options: Data.WritingOptions = [.atomic]
        #if os(iOS)
        options.insert(.completeFileProtection)
        #endif
        do {
            try value.write(to: fileURL(for: key), options: options)
            try? fileManager.setAttributes(
                [.protectionKey: FileProtectionType.complete],
                ofItemAtPath: fileURL(for: key).path
            )
        } catch {
            throw SecretStoreError.Failed
        }
    }

    func delete(key: String) throws {
        lock.lock()
        defer { lock.unlock() }
        do {
            try fileManager.removeItem(at: fileURL(for: key))
        } catch let error as NSError
            where error.domain == NSCocoaErrorDomain
                && error.code == NSFileNoSuchFileError
        {
            return
        } catch {
            throw SecretStoreError.Failed
        }
    }

    /// The store never derives a file name from raw key material.
    func fileURL(for key: String) -> URL {
        let digest = SHA256.hash(data: Data(key.utf8))
        return directory.appendingPathComponent(
            digest.map { String(format: "%02x", $0) }.joined()
        )
    }

    private func prepareDirectory() throws {
        try fileManager.createDirectory(
            at: directory,
            withIntermediateDirectories: true
        )
        var values: URLResourceValues = URLResourceValues()
        values.isExcludedFromBackup = true
        var target = directory
        try target.setResourceValues(values)
        try? fileManager.setAttributes(
            [.posixPermissions: 0o700],
            ofItemAtPath: directory.path
        )
        #if os(iOS)
        try? fileManager.setAttributes(
            [.protectionKey: FileProtectionType.complete],
            ofItemAtPath: directory.path
        )
        #endif
    }
}
