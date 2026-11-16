// Non-secret local preferences (paired flag and last selection). Secrets belong
// in Keychain; drafts live in the draft store. Stored under
// Application Support/WhaleRein.
//
// The file is not authoritative for pairing: Core owns the device identity. A
// corrupt or migrated payload therefore falls back to the safe onboarding state
// instead of leaving the app in a paired root with no usable Core session.

import Foundation

struct AppPreferences: Codable, Equatable {
    static let currentSchemaVersion = 2

    var schemaVersion: Int = AppPreferences.currentSchemaVersion
    var paired: Bool = false
    var lastHostId: String?
    var lastSessionId: String?
    var lastSessionTitle: String?

    init(
        schemaVersion: Int = AppPreferences.currentSchemaVersion,
        paired: Bool = false,
        lastHostId: String? = nil,
        lastSessionId: String? = nil,
        lastSessionTitle: String? = nil
    ) {
        self.schemaVersion = schemaVersion
        self.paired = paired
        self.lastHostId = lastHostId
        self.lastSessionId = lastSessionId
        self.lastSessionTitle = lastSessionTitle
    }

    enum CodingKeys: String, CodingKey {
        case schemaVersion
        case paired
        case lastHostId
        case lastSessionId
        case lastSessionTitle
    }

    /// Tolerant decoding with an explicit one-way migration from the original
    /// version-less payload. Unknown future versions are rejected so the store
    /// can surface a safe onboarding fallback rather than reading fields it does
    /// not understand.
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        let storedVersion = try container.decodeIfPresent(Int.self, forKey: .schemaVersion) ?? 1
        guard storedVersion <= Self.currentSchemaVersion else {
            throw PreferencesDecodingError.unsupportedVersion(storedVersion)
        }

        schemaVersion = Self.currentSchemaVersion
        paired = try container.decodeIfPresent(Bool.self, forKey: .paired) ?? false

        let host = try container.decodeIfPresent(String.self, forKey: .lastHostId)
        let session = try container.decodeIfPresent(String.self, forKey: .lastSessionId)
        let title = try container.decodeIfPresent(String.self, forKey: .lastSessionTitle)
        lastHostId = Self.nonEmpty(host)
        lastSessionId = Self.nonEmpty(session)
        lastSessionTitle = Self.nonEmpty(title)
        if lastHostId == nil {
            lastSessionId = nil
            lastSessionTitle = nil
        }
    }

    private static func nonEmpty(_ value: String?) -> String? {
        guard let value, !value.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            return nil
        }
        return value
    }
}

enum PreferencesDecodingError: Error, Equatable {
    case unsupportedVersion(Int)
}

@MainActor
final class PreferencesStore {
    enum Recovery: Equatable {
        case corrupt
        case unsupportedVersion(Int)
    }

    private let fileURL: URL
    private let fileManager: FileManager
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    private(set) var value: AppPreferences
    /// Why the stored payload was discarded, if it was. `nil` means the file was
    /// absent or decoded normally.
    private(set) var recovery: Recovery?

    init(directory: URL, fileManager: FileManager = .default) {
        let url = directory.appendingPathComponent(AppStorageLocations.preferencesFileName)
        self.fileURL = url
        self.fileManager = fileManager
        let loaded = Self.load(from: url, decoder: JSONDecoder(), fileManager: fileManager)
        self.value = loaded.value
        self.recovery = loaded.recovery
    }

    func update(_ mutate: (inout AppPreferences) -> Void) {
        var next = value
        mutate(&next)
        next.schemaVersion = AppPreferences.currentSchemaVersion
        guard next != value else { return }
        value = next
        persist()
    }

    private func persist() {
        do {
            let data = try encoder.encode(value)
            try data.write(to: fileURL, options: [.atomic, .completeFileProtection])
        } catch {
            // Losing the last-selection preference must not block the app.
        }
    }

    private static func load(
        from url: URL,
        decoder: JSONDecoder,
        fileManager: FileManager
    ) -> (value: AppPreferences, recovery: Recovery?) {
        guard fileManager.fileExists(atPath: url.path) else {
            return (AppPreferences(), nil)
        }
        guard let data = try? Data(contentsOf: url) else {
            return (AppPreferences(), .corrupt)
        }
        do {
            return (try decoder.decode(AppPreferences.self, from: data), nil)
        } catch let error as PreferencesDecodingError {
            switch error {
            case .unsupportedVersion(let version):
                return (AppPreferences(), .unsupportedVersion(version))
            }
        } catch {
            return (AppPreferences(), .corrupt)
        }
    }
}
