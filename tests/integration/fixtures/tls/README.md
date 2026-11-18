# TLS test fixtures

The integration TLS cases need a small certificate hierarchy: a trusted CA, a
second CA for rotation, a valid server leaf, and negative leaves (expired,
wrong host, self-signed). They are generated at test time by
`tests/integration/tls_environment.py` with the `openssl` CLI inside the
container’s temporary filesystem. They are never committed:
private keys must never enter the repository.

| File | Purpose |
| --- | --- |
| `ca.pem` / `ca.key` | Original test CA trusted by Access and Core. |
| `server.pem` / `server.key` | Server certificate for `localhost` and `127.0.0.1`, signed by `ca.pem`. |
| `expired.pem` / `expired.key` | Same identity as `server`, valid only in 2020. |
| `wrong-host.pem` / `wrong-host.key` | Valid `ca.pem` chain for `wrong.example`. |
| `self-signed.pem` / `self-signed.key` | Trusted by nobody. |
| `server-rotated.pem` / `server-rotated.key` | Replacement leaf for the same identity, still signed by `ca.pem`. |
| `ca2.pem` / `ca2.key` | Independent second authority for CA rotation. |
| `server2.pem` / `server2.key` | `localhost` identity signed by `ca2.pem`. |

The files are discarded with the test container.
