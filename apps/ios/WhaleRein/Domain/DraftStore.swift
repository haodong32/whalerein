// Draft lifecycle, isolated per Host-qualified session. The file store keeps
// drafts inside Application Support/WhaleRein with complete file protection; the
// Rust Core encrypted-draft store replaces it when the binding lands.

import Foundation

@MainActor
protocol DraftStore: AnyObject {
    func save(_ text: String, hostId: String, sessionId: String) throws
    func load(hostId: String, sessionId: String) throws -> String?
    func clear(hostId: String, sessionId: String) throws
}

struct StoredDraft: Codable, Equatable {
    let hostId: String
    let sessionId: String
    var text: String
    var updatedAt: Date
}

@MainActor
final class FileDraftStore: DraftStore {
    private let directory: URL
    private let fileManager: FileManager
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    init(directory: URL, fileManager: FileManager = .default) {
        self.directory = directory
        self.fileManager = fileManager
        encoder.dateEncodingStrategy = .iso8601
        decoder.dateDecodingStrategy = .iso8601
    }

    convenience init(dataDirectory: URL, fileManager: FileManager = .default) throws {
        let directory = try AppStorageLocations.draftsDirectory(in: dataDirectory, fileManager: fileManager)
        self.init(directory: directory, fileManager: fileManager)
    }

    func save(_ text: String, hostId: String, sessionId: String) throws {
        guard !text.isEmpty else {
            try clear(hostId: hostId, sessionId: sessionId)
            return
        }
        let draft = StoredDraft(hostId: hostId, sessionId: sessionId, text: text, updatedAt: Date())
        let data = try encoder.encode(draft)
        let url = fileURL(hostId: hostId, sessionId: sessionId)
        try data.write(to: url, options: [.atomic, .completeFileProtection])
    }

    func load(hostId: String, sessionId: String) throws -> String? {
        let url = fileURL(hostId: hostId, sessionId: sessionId)
        guard fileManager.fileExists(atPath: url.path) else { return nil }
        let data = try Data(contentsOf: url)
        let draft = try decoder.decode(StoredDraft.self, from: data)
        guard draft.hostId == hostId, draft.sessionId == sessionId else { return nil }
        return draft.text
    }

    func clear(hostId: String, sessionId: String) throws {
        let url = fileURL(hostId: hostId, sessionId: sessionId)
        if fileManager.fileExists(atPath: url.path) {
            try fileManager.removeItem(at: url)
        }
    }

    private func fileURL(hostId: String, sessionId: String) -> URL {
        let name = AppStorageLocations.sessionKey(hostId: hostId, sessionId: sessionId)
        return directory.appendingPathComponent("\(name).json")
    }
}

@MainActor
final class InMemoryDraftStore: DraftStore {
    private(set) var values: [String: String] = [:]
    var failOnSave = false

    func save(_ text: String, hostId: String, sessionId: String) throws {
        if failOnSave { throw SessionServiceError(.draftUnavailable) }
        let key = AppStorageLocations.sessionKey(hostId: hostId, sessionId: sessionId)
        if text.isEmpty {
            values.removeValue(forKey: key)
        } else {
            values[key] = text
        }
    }

    func load(hostId: String, sessionId: String) throws -> String? {
        values[AppStorageLocations.sessionKey(hostId: hostId, sessionId: sessionId)]
    }

    func clear(hostId: String, sessionId: String) throws {
        values.removeValue(forKey: AppStorageLocations.sessionKey(hostId: hostId, sessionId: sessionId))
    }
}
