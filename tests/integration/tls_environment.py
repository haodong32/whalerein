"""TLS broker and scenario wiring for production-transport integration cases.

The default integration stack is plaintext loopback. These helpers start a
second, case-scoped Mosquitto listener with the same plugin configuration but a
real TLS handshake, and reconfigure Access, the plugin and the CLI to trust the
test CA. Certificate material lives in `fixtures/tls/` and is public test data.

Negative handshake cases do not use this module's broker: they start Access with
one certificate variant and assert `/readyz` fails closed with a transport
error, so a bad certificate can never be mistaken for a host being offline.

Rotation cases use two independent certificate authorities. `ca.pem` signs
`server.pem` (and the same-CA `server-rotated.pem`); `ca2.pem` signs
`server2.pem`. Removing the CA that signed the live listener must fail closed,
and trusting the new CA must recover without re-pairing.
"""

import os
import subprocess
from pathlib import Path

from environment import (
    BROKER_PASSWORD,
    INTERNAL_TOKEN,
    ROOT,
    Process,
    wait,
)

TLS_DIR = ROOT / "tests/integration/fixtures/tls"

_GENERATED = (
    "ca.pem", "ca.key", "ca2.pem", "ca2.key",
    "server.pem", "server.key", "expired.pem", "expired.key",
    "wrong-host.pem", "wrong-host.key", "self-signed.pem", "self-signed.key",
    "server-rotated.pem", "server-rotated.key", "server2.pem", "server2.key",
)


def _openssl(*arguments):
    completed = subprocess.run(
        ["openssl", *arguments], capture_output=True, text=True, check=False
    )
    if completed.returncode != 0:
        raise AssertionError(
            "TLS fixture generation failed: " + completed.stderr.strip()[-300:]
        )


def _ca(name, common_name):
    _openssl(
        "req", "-x509", "-newkey", "rsa:2048", "-nodes",
        "-keyout", str(TLS_DIR / f"{name}.key"),
        "-out", str(TLS_DIR / f"{name}.pem"),
        "-days", "3650", "-subj", f"/CN={common_name}",
        "-addext", "basicConstraints=critical,CA:TRUE",
        "-addext", "keyUsage=critical,keyCertSign,cRLSign",
    )


def _leaf(name, common_name, ca, san=None, not_before=None, not_after=None):
    key = TLS_DIR / f"{name}.key"
    csr = TLS_DIR / f"{name}.csr"
    cert = TLS_DIR / f"{name}.pem"
    _openssl(
        "req", "-new", "-newkey", "rsa:2048", "-nodes",
        "-keyout", str(key), "-out", str(csr), "-subj", f"/CN={common_name}",
    )
    arguments = [
        "x509", "-req", "-in", str(csr),
        "-CA", str(TLS_DIR / f"{ca}.pem"), "-CAkey", str(TLS_DIR / f"{ca}.key"),
        "-CAcreateserial", "-out", str(cert), "-sha256",
    ]
    if san is not None:
        extension = TLS_DIR / f"{name}.ext"
        extension.write_text("subjectAltName=" + san + "\n")
        arguments += ["-extfile", str(extension)]
    if not_before is not None:
        arguments += ["-not_before", not_before]
    if not_after is not None:
        arguments += ["-not_after", not_after]
    else:
        arguments += ["-days", "825"]
    _openssl(*arguments)
    csr.unlink(missing_ok=True)
    (TLS_DIR / f"{name}.ext").unlink(missing_ok=True)


def ensure_certificates():
    """Generate the public TLS fixtures once per checkout or container.

    The fixtures are deliberately not committed: private keys must never enter
    the repository, and the integration image regenerates them with openssl.
    """
    if all((TLS_DIR / name).is_file() for name in _GENERATED):
        return
    TLS_DIR.mkdir(parents=True, exist_ok=True)
    _ca("ca", "WhaleRein Test CA")
    _ca("ca2", "WhaleRein Rotation CA")
    _leaf("server", "localhost", "ca", "DNS:localhost,IP:127.0.0.1")
    _leaf(
        "expired", "localhost", "ca", "DNS:localhost,IP:127.0.0.1",
        not_before="20200101000000Z", not_after="20200201000000Z",
    )
    _leaf("wrong-host", "wrong.example", "ca", "DNS:wrong.example")
    _openssl(
        "req", "-x509", "-newkey", "rsa:2048", "-nodes",
        "-keyout", str(TLS_DIR / "self-signed.key"),
        "-out", str(TLS_DIR / "self-signed.pem"),
        "-days", "825", "-subj", "/CN=localhost",
        "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1",
    )
    _leaf("server-rotated", "localhost", "ca", "DNS:localhost,IP:127.0.0.1")
    _leaf("server2", "localhost", "ca2", "DNS:localhost,IP:127.0.0.1")


# One entry per fixture identity. `ca` is the authority the listener's own
# certificate chains to, which is also what a trusting client needs.
CERTIFICATES = {
    "server": {"ca": "ca.pem", "cert": "server.pem", "key": "server.key"},
    "expired": {"ca": "ca.pem", "cert": "expired.pem", "key": "expired.key"},
    "wrong-host": {"ca": "ca.pem", "cert": "wrong-host.pem", "key": "wrong-host.key"},
    "self-signed": {"ca": "ca.pem", "cert": "self-signed.pem", "key": "self-signed.key"},
    # Same leaf identity as `server`, still signed by the original test CA.
    "server-rotated": {
        "ca": "ca.pem",
        "cert": "server-rotated.pem",
        "key": "server-rotated.key",
    },
    # Independent authority: trusting ca.pem alone cannot verify this leaf.
    "server2": {"ca": "ca2.pem", "cert": "server2.pem", "key": "server2.key"},
}


def certificate(name="server"):
    """Return the certificate/key/CA paths for one fixture identity."""
    ensure_certificates()
    try:
        entry = CERTIFICATES[name]
    except KeyError:
        raise ValueError(f"unknown TLS fixture certificate: {name}") from None
    return {
        "ca": TLS_DIR / entry["ca"],
        "cert": TLS_DIR / entry["cert"],
        "key": TLS_DIR / entry["key"],
    }


def ca_bundle(directory, *names):
    """Concatenate test CAs into one bundle for a rotation trust store."""
    ensure_certificates()
    target = Path(directory) / "trust-bundle.pem"
    target.parent.mkdir(parents=True, exist_ok=True)
    parts = [
        (TLS_DIR / (name if name.endswith(".pem") else f"{name}.pem")).read_bytes()
        for name in names
    ]
    target.write_bytes(b"".join(parts))
    target.chmod(0o644)
    return target


class TlsBroker:
    """One TLS MQTT listener with the WhaleRein plugin.

    The default port is the production MQTT TLS port inside the disposable
    container. A second instance can own another port so a case can point only
    the CLI/Core trust path at a different certificate while Access keeps a
    healthy listener.
    """

    def __init__(self, directory, cert="server", port=8883):
        self.directory = Path(directory)
        self.directory.mkdir(parents=True, exist_ok=True)
        self.cert = certificate(cert)
        self.port = port
        self.process = None

    @property
    def url(self):
        return f"mqtts://127.0.0.1:{self.port}"

    def config_text(self):
        plugin = self.directory / "broker"
        plugin.mkdir(parents=True, exist_ok=True)
        for directory in [plugin, *list(plugin.parents)[:3]]:
            try:
                directory.chmod(0o755)
            except OSError:
                pass
        material = (
            ("ca.pem", self.cert["ca"]),
            ("server.pem", self.cert["cert"]),
            ("server.key", self.cert["key"]),
        )
        for name, path in material:
            target = plugin / name
            target.write_bytes(Path(path).read_bytes())
            target.chmod(0o644)
        password_file = plugin / "access-password"
        token_file = plugin / "internal-token"
        password_file.write_text(BROKER_PASSWORD)
        token_file.write_text(INTERNAL_TOKEN)
        return "\n".join(
            [
                "# Generated per-case TLS listener for the integration container.",
                f"listener {self.port} 127.0.0.1",
                "protocol mqtt",
                "set_tcp_nodelay true",
                "allow_anonymous false",
                "persistence false",
                "max_packet_size 1048576",
                "log_dest stdout",
                "log_type error",
                "connection_messages false",
                "require_certificate false",
                f"cafile {plugin / 'ca.pem'}",
                f"certfile {plugin / 'server.pem'}",
                f"keyfile {plugin / 'server.key'}",
                "tls_version tlsv1.2",
                "plugin /usr/local/lib/mosquitto/whalerein.so",
                "plugin_opt_access_username access",
                f"plugin_opt_access_password_file {password_file}",
                "plugin_opt_access_auth_url http://127.0.0.1:18081/internal/broker/connect",
                f"plugin_opt_access_auth_token_file {token_file}",
                "plugin_opt_auth_timeout_ms 500",
                "plugin_opt_policy_ttl_ms 15000",
                "",
            ]
        )

    def start(self):
        if self.process is not None:
            return
        config = self.directory / "broker" / "mosquitto.conf"
        config.parent.mkdir(parents=True, exist_ok=True)
        config.write_text(self.config_text())
        self.process = Process(
            ["/usr/local/sbin/mosquitto", "-c", str(config)], self.directory
        )

        def ready():
            import socket

            # Readiness is "the listener bound". Negative certificate fixtures
            # (expired/wrong-host/self-signed) must still start so Access can be
            # tested against them; a certificate that Mosquitto cannot load
            # exits the process, which the readiness wait detects as a failure.
            with socket.create_connection(("127.0.0.1", self.port), timeout=1):
                return True

        wait(ready, process=self.process)

    def close(self):
        if self.process is not None:
            self.process.close()
            self.process = None
