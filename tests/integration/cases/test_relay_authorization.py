"""AC-02/03/07 authorization and exact-resolution evidence.

These cases use the real Access temporary ciphertext directory over its
case-scoped HTTP origin with a real Device bearer. The bearer is issued by the
case database exactly like the CLI's transport token, so authorization is the
same authority path the product uses; only the caller is a probe.
"""

import base64
import hashlib
import json
import os
import subprocess
import time

import pytest
from environment import ROOT, Device, RelayDirectory

pytestmark = pytest.mark.transport


def ciphertext(size=1024, seed=0):
    return bytes((seed * 131 + index * 17) & 0xFF for index in range(size))


def header(headers, name):
    for key, value in headers.items():
        if key.lower() == name.lower():
            return value
    raise AssertionError(f"missing relay response header {name}")


def reserve(directory, token, scenario, device, data, **overrides):
    scope = {
        "host_id": scenario.host,
        "device_id": device.identity["device_id"],
        "channel_id": "channel-auth",
        "purpose": "PAYLOAD_PURPOSE_RESULT",
        "ciphertext": data,
    }
    scope.update(overrides)
    return directory.reserve(token, **scope)


@pytest.mark.relay_directory
def test_ac03_foreign_host_is_denied(scenario, paired_device):
    """A Device grant for one Host cannot reserve ciphertext under another Host identity."""
    directory = scenario.relay_directory
    with scenario.fixture_bearer(paired_device) as bearer:
        status, _, body = reserve(directory, bearer, scenario, paired_device, ciphertext(), host_id="host-foreign")
        assert status == 403, body
        assert json.loads(body)["code"] == "forbidden"
        # The same scope under the granted Host is admitted.
        status, _, body = reserve(directory, bearer, scenario, paired_device, ciphertext())
        assert status == 200, body


@pytest.mark.relay_directory
def test_ac03_foreign_device_is_denied(scenario, paired_device):
    """One Device cannot fetch another Device's ciphertext even with the same Host grant."""
    second = scenario.pair_second()
    directory = scenario.relay_directory
    with scenario.fixture_bearer(paired_device) as owner, scenario.fixture_bearer(second) as other:
        data = ciphertext(512, 3)
        stored = directory.store(owner, ciphertext=data, host_id=scenario.host,
                                 device_id=paired_device.identity["device_id"], channel_id="channel-a")
        status, _, body = directory.fetch(owner, stored["objectId"])
        assert status == 200 and body == data
        status, _, payload = directory.fetch(other, stored["objectId"])
        assert status == 403, payload
        assert json.loads(payload)["code"] == "forbidden"


@pytest.mark.relay_directory
def test_ac03_session_and_channel_scope_are_required(scenario, paired_device):
    """A reservation outside the authenticated session, or without a channel identity, is denied before storage."""
    directory = scenario.relay_directory
    with scenario.fixture_bearer(paired_device) as bearer:
        status, _, body = reserve(
            directory, bearer, scenario, paired_device, ciphertext(), session_id="foreign-session"
        )
        assert status == 403, body
        status, _, body = reserve(
            directory, bearer, scenario, paired_device, ciphertext(), channel_id=""
        )
        assert status == 400, body
        assert json.loads(body)["code"] == "invalid_argument"


@pytest.mark.relay_directory
def test_ac02_reference_resolves_exact_bytes(scenario, paired_device):
    """A stored result object is fetched as the exact ciphertext with its declared length and digest."""
    directory = scenario.relay_directory
    data = ciphertext(200_000, 9)
    with scenario.fixture_bearer(paired_device) as bearer:
        stored = directory.store(
            bearer,
            ciphertext=data,
            host_id=scenario.host,
            device_id=paired_device.identity["device_id"],
            channel_id="channel-reference",
        )
        assert stored["purpose"] == "PAYLOAD_PURPOSE_RESULT"
        assert int(stored["ciphertextBytes"]) == len(data)
        status, headers, body = directory.fetch(bearer, stored["objectId"])
        assert status == 200
        assert body == data
        assert int(header(headers, "Content-Length")) == len(data)
        assert header(headers, "Digest") == "sha-256=" + base64.b64encode(hashlib.sha256(data).digest()).decode()
        assert int(header(headers, "WhaleRein-Object-Expires-At-Unix-Ms")) > int(time.time() * 1000) - 60_000


@pytest.mark.relay_directory
def test_ac02_corrupt_and_substituted_objects_are_rejected(scenario, paired_device):
    """A complete-length upload with another digest, and a substituted body, are rejected before the object becomes readable."""
    directory = scenario.relay_directory
    good = ciphertext(4096, 5)
    with scenario.fixture_bearer(paired_device) as bearer:
        status, _, payload = directory.reserve(
            bearer, ciphertext=good, host_id=scenario.host,
            device_id=paired_device.identity["device_id"], channel_id="channel-corrupt",
        )
        assert status == 200, payload
        reservation = json.loads(payload)
        substituted = bytes([value ^ 0xFF for value in good])
        status, _, payload = directory.upload(bearer, reservation, substituted)
        # The store may detect the digest while streaming or at finish; either
        # way the object never becomes readable.
        assert status in {204, 409}, payload
        if status == 204:
            status, _, payload = directory.finish(bearer, reservation["objectId"], good)
        assert status == 409, payload

        # A truncated upload declares a shorter length and is refused too.
        status, _, payload = directory.reserve(
            bearer, ciphertext=good, host_id=scenario.host,
            device_id=paired_device.identity["device_id"], channel_id="channel-truncated",
        )
        assert status == 200, payload
        reservation = json.loads(payload)
        status, _, payload = directory.upload(bearer, reservation, good[:-1])
        assert status == 409, payload


@pytest.mark.relay_directory
def test_ac03_revocation_fences_an_existing_reference(scenario, paired_device):
    """Revoking the Device grant denies a fetch of an object reserved before revocation."""
    directory = scenario.relay_directory
    data = ciphertext(1024, 11)
    with scenario.fixture_bearer(paired_device) as bearer:
        stored = directory.store(
            bearer, ciphertext=data, host_id=scenario.host,
            device_id=paired_device.identity["device_id"], channel_id="channel-revoke",
        )
        paired_device.revoke()
        status, _, payload = directory.fetch(bearer, stored["objectId"])
        assert status in {401, 403}, payload


def run_query_peer(scenario, arguments):
    """Run one raw MQTT 5 peer and return only its printed routing facts."""
    result = subprocess.run(
        ["node", "tests/integration/drivers/transport/mqtt-query-peer.mjs", *arguments],
        cwd=ROOT,
        env={
            **os.environ,
            "WHALEREIN_TEST_MQTT_URL": "mqtt://127.0.0.1:1883",
        },
        capture_output=True,
        text=True,
        timeout=90,
        check=False,
    )
    assert result.returncode == 0, "raw MQTT query peer failed: " + result.stderr.strip()[-300:]
    lines = [line for line in result.stdout.splitlines() if line.strip()]
    assert lines, "raw MQTT query peer emitted no facts"
    return json.loads(lines[-1])


def test_forged_connection_property_is_rejected_by_the_broker(scenario, paired_device):
    """A Device-supplied whalerein-connection property is denied at PUBACK while the same query without it is accepted."""
    with scenario.fixture_bearer(paired_device) as bearer:
        facts = run_query_peer(
            scenario,
            ["forge", scenario.host, paired_device.identity["device_id"], bearer],
        )
    assert facts["forgedReason"] is not None and facts["forgedReason"] >= 0x80, facts
    assert facts["acceptedReason"] == 0, facts


def test_directed_reply_is_not_delivered_to_a_same_id_replacement(scenario, paired_device):
    """A reply targeted at a replaced Device connection is dropped, while the same reply targeted at the current connection is delivered."""
    access_password = scenario.env["WHALEREIN_BROKER_PASSWORD"]
    with scenario.fixture_bearer(paired_device) as bearer:
        facts = run_query_peer(
            scenario,
            [
                "target",
                scenario.host,
                paired_device.identity["device_id"],
                bearer,
                access_password,
            ],
        )
    assert facts["replaced"] is True, facts
    assert facts["epochMatches"] is True, facts
    assert facts["staleDelivered"] == 0, facts
    assert facts["currentDelivered"] >= 1, facts
