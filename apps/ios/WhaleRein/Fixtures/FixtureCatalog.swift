// Deterministic synthetic data. Content, identifiers and ordering are fixed so
// unit tests and UI tests can assert exact rows without a network or a real Host.

import Foundation

struct FixtureTranscript: Hashable, Sendable {
    let messages: [ConversationMessage]
    let approvals: [Approval]
}

enum FixtureCatalog {
    static let seed: UInt64 = 0x5EED_1234
    static let transcriptCount = 320
    static let coreRevision = "fixture-add-ios-client.1"
    static let fingerprint = "SHA256:4f1d0a2c8b6e5719d3a4c0f5e8b17263405d9a1c7e2f8b46d05a3c9e17b2f4a8"
    static let fingerprintDisplay = "4F1D 0A2C 8B6E 5719 D3A4 C0F5 E8B1 7263"

    static let hosts: [HostSummary] = [
        HostSummary(
            hostId: "studio-mac",
            online: true,
            fingerprint: fingerprint,
            protocolVersion: "1"
        ),
        HostSummary(
            hostId: "travel-laptop",
            online: false,
            fingerprint: "SHA256:0b7c9e1d4a2f6385c1d9e7b0a4f2c6d8e0b3a5c7d9f1e3b5a7c9d0e2f4b6a8c1",
            protocolVersion: "1"
        ),
    ]

    static let cloudAccount = CloudAccount(
        accountId: "acct-fixture",
        email: "fixture@whalerein.test",
        issuer: "https://login.whalerein.test/oidc",
        subject: "subject-fixture",
        workspaceId: "ws-fixture"
    )

    /// The second account a fixture sign-in returns after a sign-out so UI
    /// tests can prove the sheet did not resume the previous account.
    static let cloudSecondAccount = CloudAccount(
        accountId: "acct-fixture-second",
        email: "second@whalerein.test",
        issuer: "https://login.whalerein.test/oidc",
        subject: "subject-fixture-second",
        workspaceId: "ws-fixture-second"
    )

    /// Deterministic Cloud directory covering authorized online/offline,
    /// eligible and unavailable computer states.
    static let cloudHosts: [CloudDirectoryHost] = [
        CloudDirectoryHost(
            hostId: "studio-mac",
            displayName: "Studio Mac",
            enrolled: true,
            online: true,
            presence: "online",
            observation: "active",
            enrollmentState: "enrolled",
            pairingEligible: false,
            deviceAccess: "granted"
        ),
        CloudDirectoryHost(
            hostId: "travel-laptop",
            displayName: "Travel Laptop",
            enrolled: true,
            online: false,
            presence: "offline",
            observation: "last seen yesterday",
            enrollmentState: "enrolled",
            pairingEligible: false,
            deviceAccess: "granted"
        ),
        CloudDirectoryHost(
            hostId: "new-laptop",
            displayName: "New Laptop",
            enrolled: false,
            online: true,
            presence: "online",
            observation: "ready to connect",
            enrollmentState: "eligible",
            pairingEligible: true,
            deviceAccess: "none"
        ),
        CloudDirectoryHost(
            hostId: "granted-mac",
            displayName: "Pending Grant Mac",
            enrolled: true,
            online: true,
            presence: "online",
            observation: "enrolled, device not connected",
            enrollmentState: "enrolled",
            pairingEligible: true,
            deviceAccess: "absent"
        ),
        CloudDirectoryHost(
            hostId: "old-server",
            displayName: "Old Server",
            enrolled: false,
            online: false,
            presence: "offline",
            observation: "not eligible",
            enrollmentState: "unavailable",
            pairingEligible: false,
            deviceAccess: "denied"
        ),
    ]

    static let sessionsByHost: [String: [SessionSummary]] = [
        "granted-mac": [
            SessionSummary(sessionId: "s-grant", title: "Granted workspace", updatedAtUnixMs: 1_761_000_500_000, cwd: "/work/whalerein"),
        ],
        "studio-mac": [
            SessionSummary(sessionId: "s-001", title: "Release checklist", updatedAtUnixMs: 1_761_000_300_000, cwd: "/work/whalerein"),
            SessionSummary(sessionId: "s-002", title: "Build triage", updatedAtUnixMs: 1_761_000_120_000, cwd: "/work/whalerein"),
            SessionSummary(sessionId: "s-003", title: "Nightly failure review", updatedAtUnixMs: 1_760_998_400_000, cwd: "/work/whalerein/core"),
            SessionSummary(sessionId: "s-legacy", title: "Legacy workspace", updatedAtUnixMs: 1_760_990_000_000, cwd: "/work/legacy"),
        ],
        "travel-laptop": [
            SessionSummary(sessionId: "s-101", title: "Travel notes", updatedAtUnixMs: 1_760_995_000_000, cwd: "/work/notes"),
            SessionSummary(sessionId: "s-102", title: "Offline cache review", updatedAtUnixMs: 1_760_994_000_000, cwd: "/work/cache"),
        ],
    ]

    static func sessions(hostId: String) -> [SessionSummary] {
        (sessionsByHost[hostId] ?? []).sorted { $0.updatedAtUnixMs > $1.updatedAtUnixMs }
    }

    static var invitationToken: String { "inv_7F3A9C2E5B8D1A4F6C0E" }

    static func invitationURL(hostId: String, expiresAt: Date? = nil) -> URL {
        let link = InvitationParser.makeLink(
            accessURL: URL(string: "https://\(hostId).access.whalerein.test")!,
            token: invitationToken,
            hostId: hostId,
            expiresAt: expiresAt
        )
        return URL(string: link)!
    }

    static func conversation(sessionId: String, hostId: String = "studio-mac", now: Date = Date()) -> [ConversationMessage] {
        let count = sessionId == "s-001" ? transcriptCount : 14
        return makeMessages(sessionId: sessionId, hostId: hostId, count: count)
    }

    static func approval(
        sessionId: String,
        now: Date = Date(),
        variant: FixtureApprovalVariant = .current
    ) -> Approval {
        let expiresAt: Int64
        let state: ApprovalState
        switch variant {
        case .expired:
            expiresAt = Int64(now.timeIntervalSince1970 * 1000) - 60_000
            state = .expired
        case .shortLived:
            expiresAt = Int64(now.timeIntervalSince1970 * 1000) + 1_500
            state = .pending
        case .fastClock:
            // The device clock is ten minutes ahead, so the local view of the
            // Host's absolute expiry is already in the past while the Host
            // still reports the request pending.
            expiresAt = Int64(now.timeIntervalSince1970 * 1000) - 600_000
            state = .pending
        case .unavailable:
            expiresAt = Int64(now.timeIntervalSince1970 * 1000) + 900_000
            state = .unavailable
        case .current, .changed:
            expiresAt = Int64(now.timeIntervalSince1970 * 1000) + 900_000
            state = .pending
        }
        return Approval(
            approvalId: "approval-release-\(sessionId)",
            sessionId: sessionId,
            toolName: "bash",
            callId: "call-0042",
            reason: "Run the release verification script",
            version: 3,
            expiresAtUnixMs: expiresAt,
            digestHex: "9f2c41d7b83a05e6c9d1f4a7b2e8c0d3a5f7b9c1e3d5f7a9b1c3d5e7f9a1b3c5",
            state: state
        )
    }

    static func questionBatch(sessionId: String) -> QuestionBatch {
        QuestionBatch(
            sessionID: sessionId,
            batchID: "question-release-\(sessionId)",
            sourceEpoch: "fixture-native-epoch",
            version: 2,
            digestHex: String(repeating: "a1", count: 32),
            phase: "QUESTION_CONTINUED",
            questions: [
                QuestionItem(
                    id: "question-channel",
                    question: "Which release channel should this session publish to?",
                    detail: "The native request is waiting for an explicit answer.",
                    header: "Release channel",
                    options: [
                        QuestionOption(label: "Stable", description: "Publish to the stable channel."),
                        QuestionOption(label: "Beta", description: "Publish to the beta channel."),
                    ],
                    multiSelect: false,
                    intent: QuestionIntent(
                        kind: "ask",
                        approve: "Answer",
                        reject: "Skip",
                        always: nil,
                        callID: "call-question-1",
                        subject: nil,
                        displayQuestion: nil,
                        displayDetail: nil,
                        displayApprove: nil
                    )
                )
            ],
            answerable: true,
            contentUnavailable: false
        )
    }

    static func diagnostics(now: Date = Date()) -> DiagnosticsReport {
        let json = """
        {
          "schema": "whalerein.diagnostics/1",
          "generatedAtUnixMs": \(Int(now.timeIntervalSince1970 * 1000)),
          "appVersion": "0.1.0",
          "coreRevision": "\(coreRevision)",
          "connection": {
            "https": {"status": "ok", "correlationId": "corr-https-0001"},
            "mqtt": {"status": "failed", "failureCode": "mqtt_subscription_failed", "correlationId": "corr-mqtt-0007"}
          },
          "invitation": "whalerein://pair?token=inv_SENSITIVE_VALUE",
          "token": "inv_SENSITIVE_VALUE",
          "deviceKey": "PRIVATE_KEY_MATERIAL",
          "messageContent": "Do not export this sentence",
          "toolArguments": "echo SENSITIVE_ARGUMENT"
        }
        """
        return DiagnosticsReport(
            json: json,
            redactionFields: ["invitation", "token", "deviceKey", "messageContent", "toolArguments"],
            lastFailureCode: "mqtt_subscription_failed",
            appVersion: "0.1.0",
            coreVersion: coreRevision,
            stages: stages(),
            generatedAt: now
        )
    }

    static func stages() -> [DiagnosticStage] {
        [
            DiagnosticStage(hostId: "studio-mac", kind: .https, status: .ok, correlationId: "corr-https-0001", failureCode: nil),
            DiagnosticStage(hostId: "studio-mac", kind: .mqtt, status: .failed, correlationId: "corr-mqtt-0007", failureCode: "mqtt_subscription_failed"),
            DiagnosticStage(hostId: "travel-laptop", kind: .https, status: .ok, correlationId: "corr-https-0002", failureCode: nil),
            DiagnosticStage(hostId: "travel-laptop", kind: .mqtt, status: .idle, correlationId: nil, failureCode: nil),
        ]
    }

    // MARK: - Transcript generation

    private static func makeMessages(sessionId: String, hostId: String, count: Int) -> [ConversationMessage] {
        var rng = SeededRandom(seed: seed &+ UInt64(sessionId.unicodeScalars.reduce(0) { $0 &+ UInt64($1.value) }))
        var messages: [ConversationMessage] = []
        for index in 1...count {
            let messageId = String(format: "msg-%04d", index)
            let base = Int64(1_800_000_000_000) + Int64(index) * 4_000
            let blocks: [ConversationBlock]
            let role: MessageRole

            if index == count - 2 {
                role = .assistant
                blocks = [
                    ConversationBlock(
                        blockId: "\(messageId)-block-0",
                        kind: .text,
                        text: """
                        ### Release verification table

                        | Check | Result |
                        | --- | --- |
                        | Unit suites | Passed |
                        | UI plan | Passed |

                        Everything above is synthetic fixture data.
                        """
                    )
                ]
            } else if index == count {
                role = .assistant
                blocks = [
                    ConversationBlock(
                        blockId: "\(messageId)-block-0",
                        kind: .text,
                        text: """
                        ## Release summary

                        The final verification run **passed**. Notes:

                        - All unit suites passed.
                        - The simulator UI plan completed.

                        ```swift
                        let revision = "\(coreRevision)"
                        print("WhaleRein", revision)
                        ```
                        Review the full log before closing the session.
                        """
                    )
                ]
            } else if index % 11 == 0 {
                role = .tool
                let exitCode = rng.int(in: 0...1)
                blocks = [
                    ConversationBlock(
                        blockId: "\(messageId)-block-0",
                        kind: .tool,
                        text: """
                        $ ./repo test core
                        compiling whalerein-core v0.1.0
                        running 148 tests
                        test result: \(exitCode == 0 ? "ok" : "FAILED"). 148 passed; 0 failed
                        finished in 2.41s
                        exit code \(exitCode)
                        """,
                        toolName: "bash",
                        toolActivity: ToolActivity(
                            callId: "call-\(index)",
                            name: "bash",
                            summary: "./repo test core",
                            state: exitCode == 0 ? .succeeded : .failed,
                            stages: ["preparation", "execution", "settlement"],
                            progress: nil,
                            failureCode: exitCode == 0 ? nil : "test_failed",
                            result: [
                                .text("exit code \(exitCode)")
                            ],
                            callMissing: false
                        )
                    )
                ]
            } else if index % 13 == 0 {
                role = .notice
                blocks = [
                    ConversationBlock(
                        blockId: "\(messageId)-block-0",
                        kind: .notice,
                        text: "Checkpoint \(index) saved. History is complete through this point."
                    )
                ]
            } else if index % 9 == 0 {
                role = .user
                blocks = [
                    ConversationBlock(
                        blockId: "\(messageId)-block-0",
                        kind: .text,
                        text: "Synthetic request \(index): check the \(index % 3 == 0 ? "release" : "build") output and summarize the failures."
                    )
                ]
            } else if index % 4 == 1 {
                role = .assistant
                blocks = [
                    ConversationBlock(
                        blockId: "\(messageId)-block-0",
                        kind: .text,
                        text: """
                        ### Synthetic message \(index)

                        I checked the **build log** for run \(index). Everything relevant is in order.

                        ```bash
                        swift build --configuration debug
                        swift test --parallel
                        ```

                        Next step \(index + 1) is ready.
                        """
                    )
                ]
            } else if index % 17 == 0 {
                role = .assistant
                blocks = [
                    ConversationBlock(
                        blockId: "\(messageId)-block-0",
                        kind: .reasoning,
                        text: "I considered the failing checks and decided to verify the release artifact before answering."
                    ),
                    ConversationBlock(
                        blockId: "\(messageId)-block-1",
                        kind: .text,
                        text: "Reasoning sample \(index): the release artifact is ready."
                    )
                ]
            } else if index % 19 == 0 {
                role = .assistant
                blocks = [
                    ConversationBlock(
                        blockId: "\(messageId)-block-0",
                        kind: .attachment,
                        text: "build-report-\(index).txt",
                        attachment: AttachmentBlock(
                            mediaType: "text/plain",
                            name: "build-report-\(index).txt",
                            size: 96,
                            available: true,
                            attachmentRef: "fixture-attachment-\(index)"
                        )
                    )
                ]
            } else if index % 7 == 0 {
                role = .assistant
                blocks = [
                    ConversationBlock(
                        blockId: "\(messageId)-block-0",
                        kind: .question,
                        text: "Synthetic question \(index): should the verifier include the simulator run?"
                    )
                ]
            } else {
                role = .assistant
                blocks = [
                    ConversationBlock(
                        blockId: "\(messageId)-block-0",
                        kind: .text,
                        text: "Synthetic message \(index): analyzed the output for \(hostId), session \(sessionId). No further action is required."
                    )
                ]
            }
            messages.append(
                ConversationMessage(messageId: messageId, role: role, blocks: blocks, createdAtUnixMs: base)
            )
        }
        return messages
    }

    // MARK: - Synthetic attachments

    static func attachmentBytes(for reference: String) -> Data {
        Data("Synthetic verified attachment for \(reference).\n".utf8)
    }

    static func attachmentName(for reference: String) -> String {
        let safe = reference.unicodeScalars.map { scalar in
            CharacterSet.alphanumerics.contains(scalar) ? Character(String(scalar)) : "-"
        }
        return "fixture-\(String(safe)).txt"
    }

    static func attachmentDigest(for reference: String) -> String {
        // Deterministic SHA-256-shaped value; fixture transfers are explicitly
        // synthetic and never represent Host verification.
        let seed = attachmentBytes(for: reference)
        return seed.map { String(format: "%02x", $0) }.joined().padding(
            toLength: 64,
            withPad: "0",
            startingAt: 0
        )
    }
}

private extension String {
    func padding(toLength length: Int, withPad pad: String, startingAt index: Int) -> String {
        var value = self
        while value.count < length {
            value += pad
        }
        return String(value.prefix(length))
    }
}
