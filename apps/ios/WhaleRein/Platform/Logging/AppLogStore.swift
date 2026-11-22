// Bounded, persistent, redacted app log storage.
//
// One store per process. Records are appended off the main thread, flushed
// periodically and on background, and pruned on launch. The store never
// uploads anything: export writes an app-private snapshot and the Settings
// screen opens the system share sheet for it.
//
// Bounds:
//   - 2 MB in-memory ring buffer, oldest records dropped with a counter
//   - app-log.jsonl (5 MB) plus app-log.1.jsonl (5 MB), 10 MB total
//   - 7-day age bound
// A failed append (including a locked device) drops the record, increments a
// visible counter and never crashes.

import Foundation

final class AppLogStore: @unchecked Sendable {
    struct Bounds: Equatable, Sendable {
        var memoryBytes: Int = 2 * 1024 * 1024
        var segmentBytes: Int = 5 * 1024 * 1024
        var retainedAge: TimeInterval = 7 * 24 * 60 * 60

        static let `default` = Bounds()
    }

    enum StoreError: Error, Equatable {
        case ioFailure
    }

    static let currentFileName = "app-log.jsonl"
    static let rotatedFileName = "app-log.1.jsonl"

    private let directory: URL
    private let bounds: Bounds
    private let fileManager: FileManager
    private let now: @Sendable () -> Date
    private let lock = NSLock()
    private let writer: DispatchQueue

    private var buffer: [String] = []
    private var bufferBytes = 0
    private var currentBytes = 0
    private var droppedRecords = 0
    private var writeFailures = 0
    private var lastPrune: Date

    init(
        directory: URL,
        bounds: Bounds = .default,
        fileManager: FileManager = .default,
        now: @escaping @Sendable () -> Date = { Date() },
        writer: DispatchQueue = DispatchQueue(label: "whalerein.applog.writer", qos: .utility)
    ) {
        self.directory = directory
        self.bounds = bounds
        self.fileManager = fileManager
        self.now = now
        self.writer = writer
        self.lastPrune = .distantPast
        prepareDirectory()
        currentBytes = fileSize(at: currentURL)
        pruneIfNeeded()
    }

    var currentURL: URL { directory.appendingPathComponent(Self.currentFileName) }
    var rotatedURL: URL { directory.appendingPathComponent(Self.rotatedFileName) }

    /// Number of records dropped by the memory bound or a failed write.
    var droppedCount: Int {
        lock.locked { droppedRecords + writeFailures }
    }

    /// Appends one record. Never blocks the caller on disk IO.
    func append(_ record: AppLogRecord) {
        guard record.level >= AppLogRecord.Level.floor else { return }
        guard var line = try? JSONEncoder().encode(record) else { return }
        line.append(0x0A)
        let text = String(decoding: line, as: UTF8.self)

        lock.lock()
        buffer.append(text)
        bufferBytes += line.count
        var dropped = 0
        while bufferBytes > bounds.memoryBytes, !buffer.isEmpty {
            let removed = buffer.removeFirst()
            bufferBytes -= removed.utf8.count
            dropped += 1
        }
        droppedRecords += dropped
        lock.unlock()

        writer.async { [weak self] in
            self?.drain()
        }
    }

    /// Synchronously drains buffered records. Called on background and before
    /// an export so the file reflects what the user just saw.
    func flush() {
        writer.sync { [weak self] in
            self?.drain()
        }
    }

    /// Deletes every segment and clears the in-memory buffer.
    func reset() {
        writer.sync { [weak self] in
            guard let self else { return }
            self.lock.locked {
                self.buffer.removeAll()
                self.bufferBytes = 0
            }
            try? self.fileManager.removeItem(at: self.currentURL)
            try? self.fileManager.removeItem(at: self.rotatedURL)
            self.currentBytes = 0
        }
    }

    /// Writes the bounded, redacted snapshot to an app-private staging file and
    /// returns its URL. Replaces any previous staging file.
    func exportSnapshot() throws -> URL {
        flush()
        let staging = fileManager.temporaryDirectory
            .appendingPathComponent("WhaleReinLogExport", isDirectory: true)
        try? fileManager.removeItem(at: staging)
        try fileManager.createDirectory(at: staging, withIntermediateDirectories: true)
        let target = staging.appendingPathComponent("whalerein-log.jsonl")
        try fileManager.copyItem(at: currentURL, to: target)
        // Keep a single file: a user exporting twice gets the latest snapshot,
        // not an ever-growing directory.
        return target
    }

    /// Removes the staging directory after sharing completes or at next launch.
    func discardExportStaging() {
        let staging = fileManager.temporaryDirectory
            .appendingPathComponent("WhaleReinLogExport", isDirectory: true)
        try? fileManager.removeItem(at: staging)
    }

    // MARK: - Writer

    private func drain() {
        lock.lock()
        let lines = buffer
        buffer.removeAll(keepingCapacity: true)
        let bytes = bufferBytes
        bufferBytes = 0
        lock.unlock()
        guard !lines.isEmpty else {
            pruneIfNeeded()
            return
        }
        do {
            try appendToFile(lines.joined(), byteCount: bytes)
        } catch {
            lock.locked { writeFailures += lines.count }
        }
        pruneIfNeeded()
    }

    private func appendToFile(_ text: String, byteCount: Int) throws {
        if !fileManager.fileExists(atPath: currentURL.path) {
            guard fileManager.createFile(atPath: currentURL.path, contents: nil) else {
                throw StoreError.ioFailure
            }
            protect(currentURL)
        }
        if currentBytes + byteCount > bounds.segmentBytes {
            try rotate()
        }
        let handle = try FileHandle(forWritingTo: currentURL)
        defer { try? handle.close() }
        try handle.seekToEnd()
        try handle.write(contentsOf: Data(text.utf8))
        currentBytes += byteCount
    }

    private func rotate() throws {
        try? fileManager.removeItem(at: rotatedURL)
        if fileManager.fileExists(atPath: currentURL.path) {
            try fileManager.moveItem(at: currentURL, to: rotatedURL)
            protect(rotatedURL)
        }
        guard fileManager.createFile(atPath: currentURL.path, contents: nil) else {
            throw StoreError.ioFailure
        }
        protect(currentURL)
        currentBytes = 0
    }

    private func pruneIfNeeded() {
        let now = now()
        // Prune at most once an hour; the age bound is coarse by design.
        guard now.timeIntervalSince(lastPrune) > 3600 else { return }
        lastPrune = now
        let cutoff = now.addingTimeInterval(-bounds.retainedAge)
        for url in [rotatedURL, currentURL] {
            guard let attributes = try? fileManager.attributesOfItem(atPath: url.path),
                  let modified = attributes[.modificationDate] as? Date,
                  modified < cutoff else { continue }
            try? fileManager.removeItem(at: url)
            if url == currentURL { currentBytes = 0 }
        }
        // Enforce the total bound even if timestamps look recent.
        var total = fileSize(at: currentURL) + fileSize(at: rotatedURL)
        if total > bounds.segmentBytes * 2, fileManager.fileExists(atPath: rotatedURL.path) {
            try? fileManager.removeItem(at: rotatedURL)
            total = fileSize(at: currentURL)
        }
        _ = total
    }

    private func prepareDirectory() {
        guard !fileManager.fileExists(atPath: directory.path) else { return }
        try? fileManager.createDirectory(
            at: directory,
            withIntermediateDirectories: true,
            attributes: [.protectionKey: FileProtectionType.complete]
        )
        protect(directory)
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutable = directory
        try? mutable.setResourceValues(values)
    }

    private func protect(_ url: URL) {
        #if os(iOS)
        try? fileManager.setAttributes(
            [.protectionKey: FileProtectionType.complete],
            ofItemAtPath: url.path
        )
        #endif
        var values = URLResourceValues()
        values.isExcludedFromBackup = true
        var mutable = url
        try? mutable.setResourceValues(values)
    }

    private func fileSize(at url: URL) -> Int {
        (try? fileManager.attributesOfItem(atPath: url.path)[.size] as? Int) ?? 0
    }
}

private extension NSLock {
    func locked<T>(_ body: () throws -> T) rethrows -> T {
        lock()
        defer { unlock() }
        return try body()
    }
}
