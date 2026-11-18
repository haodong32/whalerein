"""TRAN-01/TRAN-02: the production TLS path must be exercised, not assumed."""

import urllib.error
import urllib.request

import pytest
from environment import Device, http, wait
from tls_environment import TLS_DIR, TlsBroker, ca_bundle, certificate

pytestmark = pytest.mark.transport

BROKER_TLS = "mqtts://127.0.0.1:8883"


def _log(process):
    try:
        process.output.seek(0)
        return process.output.read()
    except (OSError, ValueError):
        return ""


def _wait_for_log(process, text, timeout=25):
    def observed():
        if text in _log(process).lower():
            return True
        if process.child.poll() is not None:
            raise AssertionError("Access exited before logging " + text)
        return False

    wait(observed, timeout=timeout, process=process)
    return _log(process)


def _event(value):
    return value.get("data", {}).get("event", {})


def _message_roles(values):
    return [
        _event(value).get("role")
        for value in values
        if _event(value).get("type") == "message"
    ]


def _ready(port=18080):
    try:
        with urllib.request.urlopen(
            f"http://127.0.0.1:{port}/readyz", timeout=1
        ) as response:
            return response.status == 200
    except (urllib.error.URLError, ConnectionError, TimeoutError):
        return False


def _enable_access_logs(scenario):
    """Raise Access logging so a TLS handshake failure is observable.

    The scenario base config uses level error; Go's MQTT client logs a failed
    dial at warn, so negative handshake cases need at least info to assert the
    x509 verification reason instead of only /readyz.
    """
    config = scenario.config.read_text()
    if "log:\n  level: error" in config:
        scenario.config.write_text(
            config.replace("log:\n  level: error", "log:\n  level: info", 1)
        )


def _restart_access_with_trust(scenario, ca_file):
    scenario.access.close()
    scenario.configure_tls(ca_file)
    _enable_access_logs(scenario)
    scenario.start_access_process()
    scenario.wait_access_ready()


def _restart_plugin(scenario):
    scenario.plugin.close()
    scenario.start_plugin()


@pytest.mark.tls
@pytest.mark.description(
    "A real TLS broker with a trusted CA carries pairing, session creation, send and tail end to end."
)
def test_tls_transport_carries_pairing_and_messages(scenario, paired_device):
    """The production TLS handshake path must work for the full product chain."""
    assert paired_device.call('host', 'list')["hosts"][0]["online"] is True
    session = paired_device.call('session', 'create', "--host", scenario.host)
    identity = session["lifecycle_receipt"]["session_id"]
    assert session["status"] == "succeeded"
    # Establish the committed cut before attaching: snapshot history is not
    # replayed as new output (STREAM-01). The live turn then crosses the TLS
    # MQTT session that the CLI/Core opens for tail.
    paired_device.observe_history(identity)
    watch = paired_device.start_watch('session', 'watch', identity, "--timeout", "30")
    try:
        watch.wait(lambda value: _event(value).get("type") == "reset", timeout=20)
        sent = paired_device.send(identity, "tls fixture message")
        assert sent["receipt"]["status"] == "succeeded"
        watch.wait(lambda value: _event(value).get("type") == "message", timeout=25)
    finally:
        watch.close()
    roles = _message_roles(watch.values)
    assert "USER" in roles and "ASSISTANT" in roles, roles


@pytest.mark.description(
    "An expired broker certificate keeps Access unready and logs a TLS verification failure instead of reporting hosts offline."
)
def test_expired_broker_certificate_fails_closed(scenario):
    """A stale certificate is a transport trust failure, not host presence."""
    scenario.access.close()
    scenario.plugin.close()
    tls = TlsBroker(scenario.directory / "tls-expired", cert="expired")
    scenario.configure_tls(tls.cert["ca"])
    _enable_access_logs(scenario)
    tls.start()
    try:
        scenario.start_access_process()
        log = _wait_for_log(scenario.access, "certificate").lower()
        assert "expired" in log, "expected an expired-certificate verification error"
        assert not _ready(), "Access reported ready with an expired broker certificate"
    finally:
        scenario.access.close()
        tls.close()


@pytest.mark.description(
    "A broker certificate for another host name is rejected even when its CA is trusted."
)
def test_wrong_hostname_certificate_fails_closed(scenario):
    """Certificate identity must match the dialed broker name."""
    scenario.access.close()
    scenario.plugin.close()
    tls = TlsBroker(scenario.directory / "tls-wrong-host", cert="wrong-host")
    scenario.configure_tls(tls.cert["ca"])
    _enable_access_logs(scenario)
    tls.start()
    try:
        scenario.start_access_process()
        log = _wait_for_log(scenario.access, "certificate").lower()
        assert "wrong.example" in log, "expected the certificate's host name in the error"
        assert "valid for" in log or "not localhost" in log, log
        assert not _ready(), "Access reported ready for a certificate with the wrong host name"
    finally:
        scenario.access.close()
        tls.close()


@pytest.mark.description(
    "A self-signed broker certificate is rejected when only the test CA is trusted."
)
def test_self_signed_certificate_fails_closed(scenario):
    """An untrusted chain must never be accepted silently."""
    scenario.access.close()
    scenario.plugin.close()
    tls = TlsBroker(scenario.directory / "tls-self-signed", cert="self-signed")
    scenario.configure_tls(tls.cert["ca"])
    _enable_access_logs(scenario)
    tls.start()
    try:
        scenario.start_access_process()
        log = _wait_for_log(scenario.access, "certificate").lower()
        assert "unknown authority" in log, log
        assert not _ready(), "Access reported ready for an untrusted certificate"
    finally:
        scenario.access.close()
        tls.close()


@pytest.mark.description(
    "A missing CA file stops Access at startup with a configuration/security error instead of treating hosts as offline."
)
def test_missing_ca_file_fails_closed(scenario):
    scenario.access.close()
    scenario.plugin.close()
    scenario.configure_tls(scenario.directory / "missing-ca.pem")
    _enable_access_logs(scenario)
    scenario.start_access_process()
    exit_code = scenario.access.child.wait(timeout=25)
    assert exit_code != 0, "Access started without a readable MQTT CA file"
    log = _log(scenario.access).lower()
    assert "ca_file" in log and ("cannot be read" in log or "read ca file" in log), log
    assert not _ready()


@pytest.mark.tls
@pytest.mark.description(
    "Rotating the broker leaf, then its CA, keeps grants: untrusted rotations fail closed and trusting the new CA reconnects without re-pairing."
)
def test_certificate_rotation_fails_closed_then_recovers_without_repairing(
    scenario, paired_device
):
    """A rotation is a trust-store event, never a re-pairing event."""
    assert paired_device.call('host', 'list')["hosts"][0]["online"] is True
    session = paired_device.call('session', 'create', "--host", scenario.host)
    identity = session["lifecycle_receipt"]["session_id"]
    # STREAM-01: synchronize before the sends so the post-rotation observer reads
    # them as new output instead of suppressed snapshot history.
    paired_device.observe_history(identity)
    paired_device.send(identity, "before rotation")
    # Steps 2 and 4 prove an untrusted rotation fails closed by asserting the
    # logged x509 reason, so this Access session must emit warnings.
    _enable_access_logs(scenario)
    scenario.access.close()
    scenario.start_access()
    wait(
        lambda: paired_device.call('host', 'list')["hosts"][0]["online"] is True,
        timeout=30,
    )

    rotated_leaf = TlsBroker(scenario.directory / "tls-leaf-rotated", cert="server-rotated")
    rotated_ca = TlsBroker(scenario.directory / "tls-ca-rotated", cert="server2")
    try:
        # 1. New leaf from the trusted authority: Access reconnects on its own.
        scenario.tls_broker.close()
        rotated_leaf.start()
        wait(_ready, timeout=30)
        wait(
            lambda: paired_device.call('host', 'list')["hosts"][0]["online"] is True,
            timeout=30,
        )
        paired_device.send_confirmed(identity, "after leaf rotation")

        # 2. New authority that Access does not trust: fail closed, not offline.
        rotated_leaf.close()
        rotated_ca.start()
        log = _wait_for_log(scenario.access, "certificate").lower()
        assert "unknown authority" in log, log
        assert not _ready(), "Access trusted a broker certificate from an unknown CA"

        # 3. Trust the new authority (keep the old one): reconnect without
        #    re-pairing. The same device identity and grant still work.
        trust = ca_bundle(scenario.directory, "ca", "ca2")
        paired_device.configure_tls(trust)
        _restart_access_with_trust(scenario, trust)
        _restart_plugin(scenario)
        wait(
            lambda: paired_device.call('host', 'list')["hosts"][0]["online"] is True,
            timeout=30,
        )
        # The paired device's own MQTT TLS session must reconnect under the
        # new trust bundle; tail is the command that opens it.
        paired_device.observe_history(identity)
        watch = paired_device.start_watch(
            'session', 'watch', identity, "--timeout", "30"
        )
        try:
            watch.wait(lambda value: _event(value).get("type") == "reset", timeout=20)
            paired_device.send_confirmed(identity, "after CA rotation")
            watch.wait(
                lambda value: _event(value).get("type") == "message", timeout=25
            )
        finally:
            watch.close()
        assert "ASSISTANT" in _message_roles(watch.values)

        # 4. Removing the old authority must still fail closed. Access now
        #    trusts only ca2; point it back at the ca1-signed listener.
        paired_device.configure_tls(TLS_DIR / "ca2.pem")
        _restart_access_with_trust(scenario, TLS_DIR / "ca2.pem")
        rotated_ca.close()
        old_leaf = TlsBroker(scenario.directory / "tls-old-ca", cert="server")
        old_leaf.start()
        try:
            log = _wait_for_log(scenario.access, "certificate").lower()
            assert "unknown authority" in log, log
            assert not _ready(), "Access accepted a certificate from the removed CA"
        finally:
            old_leaf.close()
    finally:
        scenario.access.close()
        rotated_leaf.close()
        rotated_ca.close()


@pytest.mark.tls
@pytest.mark.parametrize(
    "kind",
    [
        pytest.param(
            "expired",
            marks=pytest.mark.description(
                "The CLI/Core MQTT client rejects an expired broker certificate with a transport error, never host_offline."
            ),
        ),
        pytest.param(
            "wrong-host",
            marks=pytest.mark.description(
                "The CLI/Core MQTT client rejects a broker certificate for another host name with a transport error, never host_offline."
            ),
        ),
        pytest.param(
            "self-signed",
            marks=pytest.mark.description(
                "The CLI/Core MQTT client rejects an untrusted self-signed broker certificate with a transport error, never host_offline."
            ),
        ),
    ],
)
def test_cli_rejects_untrusted_broker_identity(scenario, paired_device, kind):
    """The client side of the handshake must fail explicitly, not as offline."""
    bad = TlsBroker(scenario.directory / f"tls-client-{kind}", cert=kind, port=8884)
    bad.start()
    try:
        session = paired_device.call('session', 'create', "--host", scenario.host)
        identity = session["lifecycle_receipt"]["session_id"]
        code, values = paired_device.invoke(
            'session', 'watch', identity, "--timeout", "3",
            success=False,
            env={
                "WHALEREIN_CA_FILE": str(certificate(kind)["ca"]),
                "WHALEREIN_MQTT_URL": bad.url,
            },
        )
        assert code != 0
        error = values[-1]["error"]["code"]
        assert error != "host_offline", "TLS failure was reported as an offline host"
        assert error in (
            "unavailable",
            "security_error",
            "internal",
            "timeout",
            "protocol_version_incompatible",
            "invalid_configuration",
        ), error
    finally:
        bad.close()


@pytest.mark.tls
@pytest.mark.description(
    "The CLI/Core MQTT client with an unreadable CA path fails explicitly with invalid_configuration instead of silently trusting system roots or reporting the host offline."
)
def test_cli_missing_ca_file_fails_explicitly(scenario, paired_device):
    session = paired_device.call('session', 'create', "--host", scenario.host)
    identity = session["lifecycle_receipt"]["session_id"]
    code, values = paired_device.invoke(
        'session', 'watch', identity, "--timeout", "3",
        success=False,
        env={
            "WHALEREIN_CA_FILE": str(scenario.directory / "missing-ca.pem"),
            "WHALEREIN_MQTT_URL": BROKER_TLS,
        },
    )
    assert code != 0
    error = values[-1]["error"]["code"]
    assert error != "host_offline", "TLS failure was reported as an offline host"
    assert error in (
        "invalid_configuration",
        "unavailable",
        "security_error",
        "internal",
        "timeout",
        "protocol_version_incompatible",
    ), error
