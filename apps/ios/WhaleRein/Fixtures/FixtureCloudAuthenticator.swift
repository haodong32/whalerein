// Deterministic stand-in for the system authentication sheet.
//
// UI tests cannot drive a hosted OIDC page, so the fixture build swaps the
// ASWebAuthenticationSession runner for a fake that completes the exact
// callback shape the real sheet produces after a short visible delay.

import AuthenticationServices
import Foundation

@MainActor
final class FixtureCloudAuthenticator: CloudWebAuthenticator {
    private var task: Task<Void, Never>?
    private(set) var lastEphemeral: Bool?
    let cancels: Bool

    init(cancels: Bool = false) {
        self.cancels = cancels
    }

    func authenticate(
        url: URL,
        callbackURLScheme: String,
        ephemeral: Bool,
        completion: @escaping @MainActor (URL?, Error?) -> Void
    ) {
        lastEphemeral = ephemeral
        task?.cancel()
        task = Task { @MainActor in
            try? await Task.sleep(for: .milliseconds(3000))
            guard !Task.isCancelled else { return }
            if cancels {
                completion(
                    nil,
                    ASWebAuthenticationSessionError(.canceledLogin)
                )
                return
            }
            completion(
                URL(string: "whalerein://auth/callback?code=fixture-code&state=fixture-state"),
                nil
            )
        }
    }

    func cancel() {
        task?.cancel()
        task = nil
    }
}
