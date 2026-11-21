// Minimal local OIDC provider for the real ASWebAuthenticationSession path.
//
// The authorization endpoint redirects straight to the app's registered
// callback, so the test exercises the actual system sheet and callback
// interception without a hosted identity provider or credentials.

import Network
import XCTest

final class MockOIDCProvider {
    private let listener: NWListener
    private let queue = DispatchQueue(label: "MockOIDCProvider")
    let baseURL: String

    init() throws {
        let parameters = NWParameters.tcp
        parameters.requiredLocalEndpoint = .hostPort(
            host: .ipv4(.loopback),
            port: .any
        )
        let listener = try NWListener(using: parameters)
        let queue = self.queue
        let ready = DispatchSemaphore(value: 0)
        listener.stateUpdateHandler = { state in
            if case .ready = state { ready.signal() }
        }
        listener.newConnectionHandler = { connection in
            Self.serve(connection, queue: queue)
        }
        listener.start(queue: queue)
        guard ready.wait(timeout: .now() + 5) == .success,
              let port = listener.port?.rawValue else {
            throw NSError(domain: "MockOIDCProvider", code: 1)
        }
        self.listener = listener
        baseURL = "http://127.0.0.1:\(port)"
    }

    func stop() {
        listener.cancel()
    }

    private static func serve(_ connection: NWConnection, queue: DispatchQueue) {
        connection.start(queue: queue)
        connection.receive(minimumIncompleteLength: 1, maximumLength: 16_384) {
            data, _, _, _ in
            guard let data else {
                connection.cancel()
                return
            }
            let request = String(decoding: data, as: UTF8.self)
            let response: String
            if request.contains("GET /authorize") {
                response = """
                HTTP/1.1 302 Found\r
                Location: whalerein://auth/callback?code=fixture-code&state=fixture-state\r
                Content-Length: 0\r
                Connection: close\r
                \r

                """
            } else {
                response = """
                HTTP/1.1 200 OK\r
                Content-Type: text/plain\r
                Content-Length: 2\r
                Connection: close\r
                \r
                ok
                """
            }
            connection.send(
                content: Data(response.utf8),
                completion: .contentProcessed { _ in connection.cancel() }
            )
        }
    }
}
