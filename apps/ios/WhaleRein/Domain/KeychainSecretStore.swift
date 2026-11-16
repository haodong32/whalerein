// Device-only Keychain implementation of the binding's SecretStore protocol.
//
// Secrets never enter UserDefaults, the app container, or logs. Items are
// created with `kSecAttrAccessibleWhenUnlockedThisDeviceOnly`, which excludes
// them from iCloud/backup migration to another phone. The service and optional
// access group are injected so tests can use a disposable namespace.

import Foundation
import Security
import WhaleReinMobile

enum KeychainAvailability: Equatable, Sendable {
    case available
    /// The device is locked or the Keychain is temporarily unavailable. The
    /// app must show a recoverable "unlock and retry" state, never loop.
    case locked
    /// A permanent Keychain failure for this device/namespace.
    case failed
}

final class KeychainSecretStore: SecretStore, @unchecked Sendable {
    /// Default app namespace. The staging app uses the staging suffix.
    static let defaultService = AppStorageLocations.keychainService
    static let stagingService = AppStorageLocations.stagingKeychainService

    private let service: String
    private let accessGroup: String?
    private let lock = NSLock()

    init(service: String = KeychainSecretStore.defaultService, accessGroup: String? = nil) {
        self.service = service
        self.accessGroup = accessGroup
    }

    /// Probes the Keychain without reading or writing a secret. Used once at
    /// launch (and on explicit retry) so a locked device shows actionable
    /// guidance instead of a generic failure or a retry loop.
    func availability() -> KeychainAvailability {
        lock.lock()
        defer { lock.unlock() }
        var query = baseQuery(key: nil)
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        query[kSecReturnAttributes as String] = true
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        return Self.availability(for: status)
    }

    static func availability(for status: OSStatus) -> KeychainAvailability {
        switch status {
        case errSecSuccess, errSecItemNotFound:
            return .available
        case errSecInteractionNotAllowed, errSecNotAvailable:
            return .locked
        default:
            return .failed
        }
    }

    func load(key: String) throws -> Data? {
        lock.lock()
        defer { lock.unlock() }
        var query = baseQuery(key: key)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var item: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &item)
        switch status {
        case errSecSuccess:
            guard let data = item as? Data else { throw SecretStoreError.Failed }
            return data
        case errSecItemNotFound:
            return nil
        case errSecInteractionNotAllowed, errSecNotAvailable:
            throw SecretStoreError.Unavailable
        default:
            throw SecretStoreError.Failed
        }
    }

    func save(key: String, value: Data) throws {
        lock.lock()
        defer { lock.unlock() }
        let query = baseQuery(key: key)
        let attributes: [String: Any] = [
            kSecValueData as String: value,
            kSecAttrAccessible as String: kSecAttrAccessibleWhenUnlockedThisDeviceOnly,
        ]
        let updateStatus = SecItemUpdate(query as CFDictionary, attributes as CFDictionary)
        if updateStatus == errSecSuccess { return }
        guard updateStatus == errSecItemNotFound else {
            throw Self.error(for: updateStatus)
        }
        var insert = query
        insert[kSecValueData as String] = value
        insert[kSecAttrAccessible as String] = kSecAttrAccessibleWhenUnlockedThisDeviceOnly
        let addStatus = SecItemAdd(insert as CFDictionary, nil)
        guard addStatus == errSecSuccess else {
            throw Self.error(for: addStatus)
        }
    }

    func delete(key: String) throws {
        lock.lock()
        defer { lock.unlock() }
        let status = SecItemDelete(baseQuery(key: key) as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw Self.error(for: status)
        }
    }

    /// Removes every item in this store's namespace. Test-only helper; the
    /// binding protocol never calls it.
    func removeAll() throws {
        lock.lock()
        defer { lock.unlock() }
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
        ]
        if let accessGroup {
            query[kSecAttrAccessGroup as String] = accessGroup
        }
        let status = SecItemDelete(query as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw Self.error(for: status)
        }
    }

    /// Number of items in this namespace, for isolated lifecycle assertions.
    func itemCount() throws -> Int {
        lock.lock()
        defer { lock.unlock() }
        var query = baseQuery(key: nil)
        query[kSecReturnAttributes as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitAll
        var result: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        switch status {
        case errSecSuccess:
            return (result as? [[String: Any]])?.count ?? 0
        case errSecItemNotFound:
            return 0
        default:
            throw Self.error(for: status)
        }
    }

    private func baseQuery(key: String?) -> [String: Any] {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
        ]
        if let key {
            query[kSecAttrAccount as String] = key
        }
        if let accessGroup {
            query[kSecAttrAccessGroup as String] = accessGroup
        }
        return query
    }

    private static func error(for status: OSStatus) -> SecretStoreError {
        switch status {
        case errSecInteractionNotAllowed, errSecNotAvailable:
            return .Unavailable
        default:
            return .Failed
        }
    }
}
