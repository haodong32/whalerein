// Presentation classification for Cloud computers: per-device access is
// authoritative, so an enrolled host this device never paired is not openable.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Cloud directory access")
struct CloudDirectoryAccessTests {
    private func host(
        enrolled: Bool,
        eligible: Bool,
        access: String
    ) -> CloudDirectoryHost {
        CloudDirectoryHost(
            hostId: "host",
            displayName: "Host",
            enrolled: enrolled,
            online: true,
            presence: "online",
            observation: "",
            enrollmentState: enrolled ? "enrolled" : "eligible",
            pairingEligible: eligible,
            deviceAccess: access
        )
    }

    @Test("only a ready device grant opens directly")
    func readyGrantsOpen() {
        #expect(host(enrolled: true, eligible: true, access: "ready").access == .authorized(online: true))
        #expect(host(enrolled: true, eligible: true, access: "granted").access == .authorized(online: true))
    }

    @Test("an enrolled host without a device grant needs connect")
    func absentGrantNeedsConnect() {
        #expect(host(enrolled: true, eligible: true, access: "absent").access == .eligible)
        #expect(host(enrolled: true, eligible: true, access: "none").access == .eligible)
        #expect(host(enrolled: false, eligible: true, access: "pending").access == .eligible)
    }

    @Test("a revoked or unavailable device grant cannot be opened")
    func revokedGrantsAreUnavailable() {
        #expect(host(enrolled: true, eligible: true, access: "revoked").access == .unavailable)
        #expect(host(enrolled: true, eligible: true, access: "revocation_pending").access == .unavailable)
    }
}
