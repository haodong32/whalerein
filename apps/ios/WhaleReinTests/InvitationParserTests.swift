// Invitation parsing and validation: links, paste input, localized errors.

import Foundation
import Testing
@testable import WhaleRein

@Suite("Invitation parsing")
struct InvitationParserTests {
    @Test("parses a WhaleRein pair link with access, token and host hint")
    func parsesAppLink() {
        let url = FixtureCatalog.invitationURL(hostId: "studio-mac")
        let result = InvitationParser.parse(url.absoluteString)
        guard case .success(let invitation) = result else {
            Issue.record("expected success, got \(result)")
            return
        }
        #expect(invitation.token == FixtureCatalog.invitationToken)
        #expect(invitation.hostHint == "studio-mac")
        #expect(invitation.accessURL.host == "studio-mac.access.whalerein.test")
    }

    @Test("decodes a percent-encoded access URL")
    func decodesAccessURL() {
        let link = "whalerein://pair?access=https%3A%2F%2Fhost.example%2Fbase&token=abcdefgh12345&host=office"
        guard case .success(let invitation) = InvitationParser.parse(link) else {
            Issue.record("expected success")
            return
        }
        #expect(invitation.accessURL.absoluteString == "https://host.example/base")
        #expect(invitation.hostHint == "office")
    }

    @Test("accepts an HTTPS invitation URL with a token query")
    func parsesWebLink() {
        let link = "https://access.example/pair/start?token=abcdefgh12345&host=web-host"
        guard case .success(let invitation) = InvitationParser.parse(link) else {
            Issue.record("expected success")
            return
        }
        #expect(invitation.accessURL.absoluteString == "https://access.example/pair/start")
        #expect(invitation.hostHint == "web-host")
    }

    @Test("trims surrounding whitespace from pasted text")
    func trimsWhitespace() {
        let raw = "  \n" + FixtureCatalog.invitationURL(hostId: "studio-mac").absoluteString + "\n "
        guard case .success = InvitationParser.parse(raw) else {
            Issue.record("expected success")
            return
        }
    }

    @Test("rejects empty input")
    func rejectsEmpty() {
        #expect(InvitationParser.parse("   \n") == .failure(.empty))
    }

    @Test("rejects a non-URL string")
    func rejectsMalformed() {
        #expect(InvitationParser.parse("not a link") == .failure(.malformed))
    }

    @Test("rejects an unsupported scheme")
    func rejectsUnsupportedScheme() {
        #expect(InvitationParser.parse("ftp://pair?token=abcdefgh123") == .failure(.unsupportedScheme))
    }

    @Test("rejects a link without a token")
    func rejectsMissingToken() {
        #expect(
            InvitationParser.parse("whalerein://pair?access=https%3A%2F%2Fhost.example")
                == .failure(.missingToken)
        )
    }

    @Test("rejects an invalid access URL")
    func rejectsInvalidAccessURL() {
        #expect(
            InvitationParser.parse("whalerein://pair?access=not-a-url&token=abcdefgh123")
                == .failure(.invalidAccessURL)
        )
    }

    @Test("rejects an expired invitation")
    func rejectsExpired() {
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let link = InvitationParser.makeLink(
            accessURL: URL(string: "https://host.example")!,
            token: "abcdefgh12345",
            hostId: "studio-mac",
            expiresAt: now.addingTimeInterval(-60)
        )
        #expect(InvitationParser.parse(link, now: now) == .failure(.expired))
    }

    @Test("rejects an implausibly short token")
    func rejectsShortToken() {
        #expect(
            InvitationParser.parse("whalerein://pair?access=https%3A%2F%2Fhost.example&token=short")
                == .failure(.malformed)
        )
    }

    @Test("makeLink and parse round-trip the expiry")
    func roundTripsExpiry() {
        let expiry = Date(timeIntervalSince1970: 1_900_000_000)
        let link = InvitationParser.makeLink(
            accessURL: URL(string: "https://host.example/access")!,
            token: "abcdefgh12345",
            hostId: "studio-mac",
            expiresAt: expiry
        )
        guard case .success(let invitation) = InvitationParser.parse(link, now: Date(timeIntervalSince1970: 1_800_000_000)) else {
            Issue.record("expected success")
            return
        }
        #expect(invitation.expiresAt == expiry)
    }

    @Test("every parse error maps to a non-empty localized key")
    func parseErrorsAreLocalized() {
        let errors: [InvitationParseError] = [
            .empty, .malformed, .unsupportedScheme, .missingToken, .invalidAccessURL, .expired,
        ]
        for error in errors {
            let key = L10nKey.invitationErrorKey(for: error)
            #expect(!key.string.isEmpty)
            #expect(key.string != key.rawValue)
        }
    }
}
