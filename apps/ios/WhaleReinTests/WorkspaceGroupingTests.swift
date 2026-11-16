// Session grouping by workspace and the relative/absolute activity text rules.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Workspace grouping")
struct WorkspaceGroupingTests {
    private func session(
        _ id: String,
        _ title: String,
        cwd: String,
        at ms: Int64,
        running: Bool = false
    ) -> SessionSummary {
        SessionSummary(
            sessionId: id,
            title: title,
            updatedAtUnixMs: ms,
            cwd: cwd,
            running: running
        )
    }

    @Test("groups sessions by workspace, most recently active first")
    func groupsByWorkspace() {
        let groups = WorkspaceGrouping.groups(
            for: [
                session("a", "A", cwd: "/work/legacy", at: 100),
                session("b", "B", cwd: "/work/whalerein", at: 300),
                session("c", "C", cwd: "/work/whalerein", at: 200),
            ],
            otherName: "Other"
        )

        #expect(groups.map(\.name) == ["whalerein", "legacy"])
        #expect(groups[0].sessions.map(\.sessionId) == ["b", "c"])
        #expect(groups[1].sessions.map(\.sessionId) == ["a"])
        #expect(groups[0].path == "/work/whalerein")
    }

    @Test("sessions without a working directory fall into the Other group last")
    func emptyWorkingDirectory() {
        let groups = WorkspaceGrouping.groups(
            for: [
                session("a", "A", cwd: "", at: 400),
                session("b", "B", cwd: "/work/project", at: 100),
            ],
            otherName: "Other"
        )

        #expect(groups.map(\.name) == ["project", "Other"])
        #expect(groups[1].path == nil)
        #expect(groups[1].sessions.map(\.sessionId) == ["a"])
    }

    @Test("one workspace still produces exactly one group")
    func singleWorkspace() {
        let groups = WorkspaceGrouping.groups(
            for: [
                session("a", "A", cwd: "/work/only", at: 10),
                session("b", "B", cwd: "/work/only", at: 20),
            ],
            otherName: "Other"
        )

        #expect(groups.count == 1)
        #expect(groups[0].name == "only")
        #expect(groups[0].sessions.map(\.sessionId) == ["b", "a"])
    }

    @Test("ties break by session id and the Other group stays last")
    func tieBreaking() {
        let groups = WorkspaceGrouping.groups(
            for: [
                session("z", "Z", cwd: "", at: 500),
                session("a", "A", cwd: "/work/x", at: 500),
                session("b", "B", cwd: "/work/y", at: 500),
            ],
            otherName: "Other"
        )

        #expect(groups.last?.name == "Other")
        #expect(groups.map(\.name).dropLast() == ["x", "y"])
    }

    @Test("display names use the final path component")
    func displayName() {
        #expect(WorkspaceGrouping.displayName(forPath: "/work/whalerein") == "whalerein")
        #expect(WorkspaceGrouping.displayName(forPath: "/work/whalerein/core/") == "core")
        #expect(WorkspaceGrouping.displayName(forPath: "/") == "/")
    }

    @Test("activity text prefers relative time and falls back to a date")
    func activityText() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let recent = now.addingTimeInterval(-2 * 60 * 60)
        let old = now.addingTimeInterval(-30 * 24 * 60 * 60)

        #expect(!SessionActivityText.isOldEnough(recent, now: now))
        #expect(SessionActivityText.isOldEnough(old, now: now))
        #expect(!SessionActivityText.plainText(for: recent, now: now).isEmpty)
        #expect(!SessionActivityText.plainText(for: old, now: now).isEmpty)
        #expect(
            SessionActivityText.plainText(for: recent, now: now)
                != SessionActivityText.plainText(for: old, now: now)
        )
    }

    @Test("running state reaches the grouped session rows")
    func runningState() {
        let groups = WorkspaceGrouping.groups(
            for: [
                session("a", "A", cwd: "/work/w", at: 10, running: true),
                session("b", "B", cwd: "/work/w", at: 20),
            ],
            otherName: "Other"
        )

        #expect(groups[0].sessions.first { $0.sessionId == "a" }?.running == true)
        #expect(groups[0].sessions.first { $0.sessionId == "b" }?.running == false)
    }
}
