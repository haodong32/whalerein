// Native system authentication sheet for the hosted Cloud page.
//
// ASWebAuthenticationSession keeps the credential experience in an isolated
// system browser and returns the exact callback URL to the app. The model only
// sees completion; it never builds credential UI.

import AuthenticationServices
import UIKit

@MainActor
protocol CloudWebAuthenticator: AnyObject {
    /// `ephemeral` ignores the shared browser session so the next sign-in can
    /// choose a different account. The shared session is kept for the ordinary
    /// path, which lets an already signed-in provider account return in one tap.
    func authenticate(
        url: URL,
        callbackURLScheme: String,
        ephemeral: Bool,
        completion: @escaping @MainActor (URL?, Error?) -> Void
    )
    func cancel()
}

func cloudAuthenticationWasCancelled(_ error: Error) -> Bool {
    let error = error as NSError
    return error.domain == ASWebAuthenticationSessionErrorDomain
        && error.code == ASWebAuthenticationSessionError.canceledLogin.rawValue
}

@MainActor
final class WebAuthenticationSessionRunner: NSObject, CloudWebAuthenticator,
    ASWebAuthenticationPresentationContextProviding
{
    private var session: ASWebAuthenticationSession?

    func authenticate(
        url: URL,
        callbackURLScheme: String,
        ephemeral: Bool,
        completion: @escaping @MainActor (URL?, Error?) -> Void
    ) {
        session?.cancel()
        let session = ASWebAuthenticationSession(
            url: url,
            callbackURLScheme: callbackURLScheme
        ) { callbackURL, error in
            Task { @MainActor in
                completion(callbackURL, error)
            }
        }
        session.presentationContextProvider = self
        // The shared session resumes the provider's signed-in account. An
        // explicit "use another account" request starts from a clean sheet.
        session.prefersEphemeralWebBrowserSession = ephemeral
        self.session = session
        session.start()
    }

    func cancel() {
        session?.cancel()
        session = nil
    }

    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let windows = scenes.flatMap(\.windows)
        if let key = windows.first(where: \.isKeyWindow) {
            return key
        }
        return scenes.first?.windows.first ?? ASPresentationAnchor()
    }
}
