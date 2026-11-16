// Pure accessibility text for the session switcher. Sessions are Host-scoped,
// so two Hosts can legitimately expose the same title or even the same session
// id. VoiceOver must hear which Host a row belongs to; the current Host alone
// is not enough context when the list is replaced in place.

import Foundation

enum SessionAccessibility {
    static func rowLabel(
        title: String,
        status: String,
        activity: String,
        hostId: String,
        isCurrent: Bool,
        selectedText: String
    ) -> String {
        var parts = [title, status, activity]
        if !hostId.isEmpty {
            parts.append("Host \(hostId)")
        }
        if isCurrent {
            parts.append(selectedText)
        }
        return parts.joined(separator: ", ")
    }
}
