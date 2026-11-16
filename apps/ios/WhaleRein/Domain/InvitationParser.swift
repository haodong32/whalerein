// Invitation link parsing and validation. Pure and deterministic so the same
// rules run in unit tests, the fixture service and a future stack-backed mode.

import Foundation

enum InvitationParser {
    static func parse(_ raw: String, now: Date = Date()) -> Result<Invitation, InvitationParseError> {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return .failure(.empty) }
        guard let components = URLComponents(string: trimmed), let scheme = components.scheme?.lowercased() else {
            return .failure(.malformed)
        }

        switch scheme {
        case "whalerein":
            return parseAppLink(components, now: now)
        case "https", "http":
            return parseWebLink(components, now: now)
        default:
            return .failure(.unsupportedScheme)
        }
    }

    private static func parseAppLink(_ components: URLComponents, now: Date) -> Result<Invitation, InvitationParseError> {
        let host = components.host?.lowercased()
        let path = components.path
        guard host == "pair" || path == "/pair" || path == "pair" else {
            return .failure(.malformed)
        }
        let query = queryItems(components)
        guard let access = query["access"], !access.isEmpty else {
            return .failure(.invalidAccessURL)
        }
        guard let accessURL = URL(string: access), let accessScheme = accessURL.scheme?.lowercased(),
              accessScheme == "https" || accessScheme == "http", accessURL.host != nil else {
            return .failure(.invalidAccessURL)
        }
        guard let token = query["token"], !token.isEmpty else {
            return .failure(.missingToken)
        }
        return finish(accessURL: accessURL, token: token, hostHint: query["host"], expiry: query["exp"], now: now)
    }

    private static func parseWebLink(_ components: URLComponents, now: Date) -> Result<Invitation, InvitationParseError> {
        guard components.path.lowercased().contains("pair") else {
            return .failure(.malformed)
        }
        let query = queryItems(components)
        guard let token = query["token"], !token.isEmpty else {
            return .failure(.missingToken)
        }
        var base = URLComponents(url: components.url!, resolvingAgainstBaseURL: false)
        base?.query = nil
        base?.fragment = nil
        guard let accessURL = base?.url, accessURL.host != nil else {
            return .failure(.invalidAccessURL)
        }
        return finish(accessURL: accessURL, token: token, hostHint: query["host"], expiry: query["exp"], now: now)
    }

    private static func finish(
        accessURL: URL,
        token: String,
        hostHint: String?,
        expiry: String?,
        now: Date
    ) -> Result<Invitation, InvitationParseError> {
        let allowed = CharacterSet.alphanumerics.union(CharacterSet(charactersIn: "-._~"))
        guard token.count >= 8, token.unicodeScalars.allSatisfy({ allowed.contains($0) }) else {
            return .failure(.malformed)
        }
        var expiresAt: Date?
        if let expiry, !expiry.isEmpty {
            guard let seconds = TimeInterval(expiry) else { return .failure(.malformed) }
            expiresAt = Date(timeIntervalSince1970: seconds)
        }
        if let expiresAt, expiresAt <= now {
            return .failure(.expired)
        }
        return .success(
            Invitation(
                accessURL: accessURL,
                token: token,
                hostHint: hostHint.flatMap { $0.isEmpty ? nil : $0 },
                expiresAt: expiresAt
            )
        )
    }

    private static func queryItems(_ components: URLComponents) -> [String: String] {
        var result: [String: String] = [:]
        for item in components.queryItems ?? [] {
            if result[item.name] == nil, let value = item.value {
                result[item.name] = value
            }
        }
        return result
    }

    /// Builds a canonical invitation link for fixtures and the paste affordance.
    static func makeLink(accessURL: URL, token: String, hostId: String, expiresAt: Date? = nil) -> String {
        var components = URLComponents()
        components.scheme = "whalerein"
        components.host = "pair"
        var items = [
            URLQueryItem(name: "access", value: accessURL.absoluteString),
            URLQueryItem(name: "token", value: token),
            URLQueryItem(name: "host", value: hostId),
        ]
        if let expiresAt {
            items.append(URLQueryItem(name: "exp", value: String(Int(expiresAt.timeIntervalSince1970))))
        }
        components.queryItems = items
        return components.url?.absoluteString ?? ""
    }
}
