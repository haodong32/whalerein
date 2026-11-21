// Attachment transfer transitions: start, bounded progress, verification,
// completion, failure, cancellation and the ignored duplicate/unknown cases.

import Foundation
import XCTest
@testable import WhaleRein

final class AttachmentsReduceTests: XCTestCase {
    private let attachmentRef = "attachment-1"

    private func progress(_ received: Int64, total: Int64 = 100) -> AttachmentDownloadProgress {
        AttachmentDownloadProgress(received: received, total: total)
    }

    private func verifiedDownload() -> AttachmentDownload {
        AttachmentDownload(
            url: URL(fileURLWithPath: "/tmp/report.txt"),
            name: "report.txt",
            bytes: 100,
            sha256: String(repeating: "a", count: 64),
            mediaType: "text/plain",
            verified: true
        )
    }

    private func startedState() -> AttachmentsState {
        var state = AttachmentsState()
        state = AttachmentsReduce.reduce(
            state,
            .startRequested(attachmentRef: attachmentRef, total: 100)
        )
        state = AttachmentsReduce.reduce(
            state,
            .downloadStarted(attachmentRef: attachmentRef, downloadId: "download-1")
        )
        return state
    }

    func testStartPreparesAndRecordsTheDownloadIdentifier() {
        var state = AttachmentsState()
        state = AttachmentsReduce.reduce(
            state,
            .startRequested(attachmentRef: attachmentRef, total: 100)
        )
        var transfer = state.transfer(for: attachmentRef)
        XCTAssertEqual(transfer.phase, .preparing)
        XCTAssertEqual(transfer.progress, progress(0))
        XCTAssertNil(transfer.downloadId)
        XCTAssertNil(transfer.errorCode)

        state = AttachmentsReduce.reduce(
            state,
            .downloadStarted(attachmentRef: attachmentRef, downloadId: "download-1")
        )
        transfer = state.transfer(for: attachmentRef)
        XCTAssertEqual(transfer.phase, .transferring)
        XCTAssertEqual(transfer.downloadId, "download-1")
        XCTAssertTrue(transfer.isTransferring)
        XCTAssertEqual(state.activeAttachmentRefs, [attachmentRef])
    }

    func testRepeatedStartWhileRunningIsIgnored() {
        var state = startedState()
        state = AttachmentsReduce.reduce(
            state,
            .progressUpdated(attachmentRef: attachmentRef, progress: progress(25))
        )
        state = AttachmentsReduce.reduce(
            state,
            .startRequested(attachmentRef: attachmentRef, total: 999)
        )
        let transfer = state.transfer(for: attachmentRef)
        XCTAssertEqual(transfer.phase, .transferring)
        XCTAssertEqual(transfer.downloadId, "download-1")
        XCTAssertEqual(transfer.progress, progress(25))
    }

    func testProgressUpdateRecordsBytes() {
        var state = startedState()
        state = AttachmentsReduce.reduce(
            state,
            .progressUpdated(attachmentRef: attachmentRef, progress: progress(25))
        )
        XCTAssertEqual(state.transfer(for: attachmentRef).progress, progress(25))

        state = AttachmentsReduce.reduce(
            state,
            .progressUpdated(attachmentRef: attachmentRef, progress: progress(75))
        )
        let transfer = state.transfer(for: attachmentRef)
        XCTAssertEqual(transfer.progress, progress(75))
        XCTAssertEqual(transfer.progress?.fraction, 0.75)
    }

    func testCompletionPassesThroughVerificationAndKeepsTheDownload() {
        var state = startedState()
        state = AttachmentsReduce.reduce(
            state,
            .progressUpdated(attachmentRef: attachmentRef, progress: progress(100))
        )
        state = AttachmentsReduce.reduce(
            state,
            .verificationStarted(attachmentRef: attachmentRef)
        )
        XCTAssertEqual(state.transfer(for: attachmentRef).phase, .verifying)

        let download = verifiedDownload()
        state = AttachmentsReduce.reduce(
            state,
            .completed(attachmentRef: attachmentRef, download: download)
        )
        let transfer = state.transfer(for: attachmentRef)
        XCTAssertEqual(transfer.phase, .completed)
        XCTAssertEqual(transfer.download, download)
        XCTAssertTrue(transfer.isVerified)
        XCTAssertNil(transfer.errorCode)
        XCTAssertTrue(state.activeAttachmentRefs.isEmpty)
    }

    func testFailureKeepsOneErrorDimension() {
        var state = startedState()
        state = AttachmentsReduce.reduce(
            state,
            .failed(attachmentRef: attachmentRef, code: .security)
        )
        var transfer = state.transfer(for: attachmentRef)
        XCTAssertEqual(transfer.phase, .failed)
        XCTAssertEqual(transfer.errorCode, .security)
        XCTAssertNil(transfer.download)

        state = AttachmentsReduce.reduce(
            state,
            .failed(attachmentRef: attachmentRef, code: .conflict)
        )
        transfer = state.transfer(for: attachmentRef)
        XCTAssertEqual(transfer.errorCode, .security)
    }

    func testCancellationIsDistinctFromFailure() {
        var state = startedState()
        state = AttachmentsReduce.reduce(
            state,
            .cancelled(attachmentRef: attachmentRef)
        )
        let transfer = state.transfer(for: attachmentRef)
        XCTAssertEqual(transfer.phase, .cancelled)
        XCTAssertEqual(transfer.errorCode, .cancelled)
        XCTAssertNil(transfer.download)
        XCTAssertTrue(transfer.phase.isTerminal)
    }

    func testDuplicateTerminalEventsAreIgnored() {
        var state = startedState()
        let download = verifiedDownload()
        state = AttachmentsReduce.reduce(
            state,
            .completed(attachmentRef: attachmentRef, download: download)
        )
        state = AttachmentsReduce.reduce(
            state,
            .completed(
                attachmentRef: attachmentRef,
                download: AttachmentDownload(
                    url: URL(fileURLWithPath: "/tmp/other.txt"),
                    name: "other.txt",
                    bytes: 1,
                    sha256: String(repeating: "b", count: 64),
                    mediaType: "text/plain",
                    verified: true
                )
            )
        )
        state = AttachmentsReduce.reduce(
            state,
            .failed(attachmentRef: attachmentRef, code: .conflict)
        )
        state = AttachmentsReduce.reduce(
            state,
            .cancelled(attachmentRef: attachmentRef)
        )
        let transfer = state.transfer(for: attachmentRef)
        XCTAssertEqual(transfer.phase, .completed)
        XCTAssertEqual(transfer.download, download)
        XCTAssertNil(transfer.errorCode)
    }

    func testUnknownTransferIdIsIgnored() {
        let initial = AttachmentsState()
        var state = initial
        state = AttachmentsReduce.reduce(
            state,
            .downloadStarted(attachmentRef: "missing", downloadId: "download-x")
        )
        state = AttachmentsReduce.reduce(
            state,
            .progressUpdated(attachmentRef: "missing", progress: progress(50))
        )
        state = AttachmentsReduce.reduce(
            state,
            .completed(attachmentRef: "missing", download: verifiedDownload())
        )
        state = AttachmentsReduce.reduce(
            state,
            .failed(attachmentRef: "missing", code: .security)
        )
        state = AttachmentsReduce.reduce(
            state,
            .exportRecorded(attachmentRef: "missing", outcome: .exported)
        )
        state = AttachmentsReduce.reduce(
            state,
            .exportCleared(attachmentRef: "missing")
        )
        XCTAssertEqual(state, initial)
        XCTAssertEqual(state.transfer(for: "missing"), AttachmentTransferState())
    }

    func testExportOutcomeRequiresAVerifiedDownload() {
        var state = startedState()
        state = AttachmentsReduce.reduce(
            state,
            .verificationStarted(attachmentRef: attachmentRef)
        )
        state = AttachmentsReduce.reduce(
            state,
            .exportRecorded(attachmentRef: attachmentRef, outcome: .exported)
        )
        XCTAssertNil(state.transfer(for: attachmentRef).exportOutcome)

        state = AttachmentsReduce.reduce(
            state,
            .completed(attachmentRef: attachmentRef, download: verifiedDownload())
        )
        state = AttachmentsReduce.reduce(
            state,
            .exportRecorded(attachmentRef: attachmentRef, outcome: .cancelled)
        )
        XCTAssertEqual(state.transfer(for: attachmentRef).exportOutcome, .cancelled)

        state = AttachmentsReduce.reduce(
            state,
            .exportCleared(attachmentRef: attachmentRef)
        )
        XCTAssertNil(state.transfer(for: attachmentRef).exportOutcome)
    }

    func testRetryAfterFailureStartsACleanTransfer() {
        var state = startedState()
        state = AttachmentsReduce.reduce(
            state,
            .failed(attachmentRef: attachmentRef, code: .conflict)
        )
        state = AttachmentsReduce.reduce(
            state,
            .startRequested(attachmentRef: attachmentRef, total: 100)
        )
        let transfer = state.transfer(for: attachmentRef)
        XCTAssertEqual(transfer.phase, .preparing)
        XCTAssertEqual(transfer.progress, progress(0))
        XCTAssertNil(transfer.downloadId)
        XCTAssertNil(transfer.errorCode)
        XCTAssertNil(transfer.exportOutcome)
    }
}
