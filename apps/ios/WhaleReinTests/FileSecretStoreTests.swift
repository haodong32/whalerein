// Developer file store used by the local Cloud instance in Debug builds.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Local file secret store")
@MainActor
struct FileSecretStoreTests {
    private func makeStore() throws -> (FileSecretStore, URL) {
        let directory = FileManager.default.temporaryDirectory
            .appendingPathComponent("WhaleReinSecrets-\(UUID().uuidString)")
        return (try FileSecretStore(directory: directory), directory)
    }

    @Test("values round-trip and survive a new store instance")
    func roundTripAndPersistence() throws {
        let (store, directory) = try makeStore()
        defer { try? FileManager.default.removeItem(at: directory) }
        try store.save(key: "device-key", value: Data("secret".utf8))

        #expect(try store.load(key: "device-key") == Data("secret".utf8))
        let reopened = try FileSecretStore(directory: directory)
        #expect(try reopened.load(key: "device-key") == Data("secret".utf8))
        #expect(try reopened.load(key: "missing") == nil)
    }

    @Test("delete removes the value and is idempotent")
    func deleteIsIdempotent() throws {
        let (store, directory) = try makeStore()
        defer { try? FileManager.default.removeItem(at: directory) }
        try store.save(key: "k", value: Data("v".utf8))
        try store.delete(key: "k")
        #expect(try store.load(key: "k") == nil)
        try store.delete(key: "k")
    }

    @Test("file names are digests and never the raw key")
    func fileNamesAreDigests() throws {
        let (store, directory) = try makeStore()
        defer { try? FileManager.default.removeItem(at: directory) }
        try store.save(key: "private-key-name", value: Data("v".utf8))
        let files = try FileManager.default.contentsOfDirectory(atPath: directory.path)
        #expect(files.count == 1)
        #expect(!files[0].contains("private-key-name"))
        #expect(files[0].count == 64)
    }

    @Test("debug builds select the file store only for the local instance")
    func fileStoreSelection() throws {
        let document = URL(fileURLWithPath: "/tmp/WhaleRein/client/app.toml")
        let production = AppConfiguration.fixture(documentURL: document).instances[0]
        let local = AppInstance(
            id: "cloud-local",
            service: .cloud,
            environment: .local,
            dataDirectory: URL(fileURLWithPath: "/tmp/WhaleRein/client/instances/cloud-local/data"),
            configPath: URL(fileURLWithPath: "/tmp/WhaleRein/client/instances/cloud-local/instance.toml"),
            hasStandaloneCA: false,
            standaloneCAPath: nil
        )
        #expect(!AppComposition.usesFileSecrets(for: production))
        #expect(AppComposition.usesFileSecrets(for: local))
    }
}
