// Grouping sessions by the workspace they were opened in. `cwd` is the only
// workspace signal the Source reports, so this stays a pure projection: no
// network, no state, and no SwiftUI dependency, which keeps it unit-testable.

import Foundation

struct WorkspaceGroup: Identifiable, Hashable, Sendable {
    /// Exact `cwd` for a real workspace, or a stable sentinel for sessions whose
    /// Source reported no working directory.
    let id: String
    let name: String
    /// Full path for accessibility labels and diagnostics; nil for the fallback
    /// group.
    let path: String?
    let sessions: [SessionSummary]
}

enum WorkspaceGrouping {
    static let otherGroupId = "workspace.other"

    /// Groups sessions by workspace and orders them for the switcher:
    /// groups by most recent activity, sessions by most recent activity,
    /// sessions with no `cwd` last. Grouping is skipped when there is only one
    /// workspace so the list is not wrapped in a redundant folder header.
    static func groups(for sessions: [SessionSummary], otherName: String) -> [WorkspaceGroup] {
        var order: [String] = []
        var grouped: [String: [SessionSummary]] = [:]

        for session in sessions {
            let key = workspaceKey(for: session.cwd)
            if grouped[key] == nil {
                order.append(key)
            }
            grouped[key, default: []].append(session)
        }

        let result: [WorkspaceGroup] = order.map { key in
            let members = (grouped[key] ?? []).sorted(by: newerFirst)
            return WorkspaceGroup(
                id: key,
                name: key == otherGroupId ? otherName : displayName(forPath: key),
                path: key == otherGroupId ? nil : key,
                sessions: members
            )
        }

        return result.sorted { lhs, rhs in
            // The fallback group always sorts last, regardless of its recency.
            if lhs.id == otherGroupId { return false }
            if rhs.id == otherGroupId { return true }
            let lhsLatest = lhs.sessions.first?.updatedAtUnixMs ?? Int64.min
            let rhsLatest = rhs.sessions.first?.updatedAtUnixMs ?? Int64.min
            if lhsLatest == rhsLatest {
                return lhs.name.localizedStandardCompare(rhs.name) == .orderedAscending
            }
            return lhsLatest > rhsLatest
        }
    }

    /// Last path component, falling back to the whole path when it has no
    /// component (for example "/").
    static func displayName(forPath path: String) -> String {
        let trimmed = path.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        guard let component = trimmed.split(separator: "/").last else { return path }
        return String(component)
    }

    private static func workspaceKey(for cwd: String) -> String {
        let trimmed = cwd.trimmingCharacters(in: .whitespacesAndNewlines)
        return trimmed.isEmpty ? otherGroupId : trimmed
    }

    private static func newerFirst(_ lhs: SessionSummary, _ rhs: SessionSummary) -> Bool {
        if lhs.updatedAtUnixMs == rhs.updatedAtUnixMs {
            return lhs.sessionId < rhs.sessionId
        }
        return lhs.updatedAtUnixMs > rhs.updatedAtUnixMs
    }
}
