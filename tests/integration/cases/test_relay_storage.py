"""AC-06/07 temporary ciphertext quota and lifetime evidence.

The cases use the real Access payload store with a case-scoped quota directory
and the Device bearer path. Physical filesystem free-space pressure and native
process restarts remain with their native procedures.
"""

import json
import time

import pytest

pytestmark = pytest.mark.storage


def data(size, seed=0):
    return bytes((seed + index * 13) & 0xFF for index in range(size))


def reserve(directory, token, scenario, device, size, **overrides):
    payload = data(size, 1)
    scope = {
        "host_id": scenario.host,
        "device_id": device.identity["device_id"],
        "channel_id": "channel-storage",
        "purpose": "PAYLOAD_PURPOSE_RESULT",
        "ciphertext": payload,
    }
    scope.update(overrides)
    return directory.reserve(token, **scope)


@pytest.mark.relay_directory(max_object_bytes=64 * 1024, max_host_bytes=256 * 1024, max_node_bytes=512 * 1024)
def test_ac06_object_limit_is_refused(scenario, paired_device):
    """A reservation above the per-object bound is refused with payload_too_large before any file is staged."""
    with scenario.fixture_bearer(paired_device) as bearer:
        status, _, body = reserve(scenario.relay_directory, bearer, scenario, paired_device, 64 * 1024 + 1)
    assert status == 413, body
    assert json.loads(body)["code"] == "payload_too_large"


@pytest.mark.relay_directory(max_object_bytes=64 * 1024, max_host_bytes=96 * 1024, max_node_bytes=512 * 1024)
def test_ac06_host_limit_is_refused(scenario, paired_device):
    """Two objects that individually fit are refused once their total would exceed the Host quota."""
    directory = scenario.relay_directory
    with scenario.fixture_bearer(paired_device) as bearer:
        first = directory.store(
            bearer, ciphertext=data(60 * 1024), host_id=scenario.host,
            device_id=paired_device.identity["device_id"], channel_id="channel-host-quota",
        )
        assert first["objectId"]
        status, _, body = reserve(directory, bearer, scenario, paired_device, 60 * 1024)
    assert status == 429, body
    payload = json.loads(body)
    assert payload["code"] == "rate_limited" and payload["reason"] == "quota_exceeded"


@pytest.mark.relay_directory(max_objects=2, max_active_uploads=1)
def test_ac06_object_and_active_upload_counts_are_bounded(scenario, paired_device):
    """The object count and concurrent upload count are enforced independently of byte totals."""
    directory = scenario.relay_directory
    with scenario.fixture_bearer(paired_device) as bearer:
        # One active, unfinished reservation consumes the only upload slot.
        status, _, body = reserve(directory, bearer, scenario, paired_device, 1024, channel_id="channel-active")
        assert status == 200, body
        status, _, body = reserve(directory, bearer, scenario, paired_device, 1024, channel_id="channel-active-2")
        assert status == 429, body
        assert json.loads(body)["reason"] == "quota_exceeded"


@pytest.mark.relay_directory(max_objects=2, max_active_uploads=2)
def test_ac06_object_count_is_bounded(scenario, paired_device):
    """A third completed object is refused after the bounded object count is reached."""
    directory = scenario.relay_directory
    with scenario.fixture_bearer(paired_device) as bearer:
        for index in range(2):
            directory.store(
                bearer, ciphertext=data(256, index), host_id=scenario.host,
                device_id=paired_device.identity["device_id"], channel_id=f"channel-count-{index}",
            )
        status, _, body = reserve(directory, bearer, scenario, paired_device, 256, channel_id="channel-count-3")
    assert status == 429, body


@pytest.mark.relay_directory(max_object_bytes=64 * 1024, max_host_bytes=256 * 1024)
def test_ac06_dishonest_length_is_refused(scenario, paired_device):
    """An upload longer than the reserved length is refused with conflict and never becomes readable."""
    directory = scenario.relay_directory
    with scenario.fixture_bearer(paired_device) as bearer:
        status, _, body = reserve(directory, bearer, scenario, paired_device, 1024, channel_id="channel-dishonest")
        assert status == 200, body
        reservation = json.loads(body)
        status, _, body = directory.upload(bearer, reservation, data(2048))
        assert status == 409, body
        status, _, body = directory.fetch(bearer, reservation["objectId"])
        assert status in {404, 409, 410}, body


@pytest.mark.relay_directory(retention="2s", upload_timeout="1s", cleanup_interval="1m", read_timeout="5s")
def test_ac07_hard_expiry_denies_a_late_fetch(scenario, paired_device):
    """An object whose fixed expiry has passed is denied with expired and never gains an extension."""
    directory = scenario.relay_directory
    with scenario.fixture_bearer(paired_device) as bearer:
        payload = data(512)
        expires = int((time.time() + 1) * 1000)
        status, _, body = directory.reserve(
            bearer, ciphertext=payload, host_id=scenario.host,
            device_id=paired_device.identity["device_id"], channel_id="channel-expiry",
            expires_at_ms=expires,
        )
        assert status == 200, body
        reservation = json.loads(body)
        status, _, body = directory.upload(bearer, reservation, payload)
        assert status == 204, body
        status, _, body = directory.finish(bearer, reservation["objectId"], payload)
        assert status == 200, body
        time.sleep(2.2)
        status, _, body = directory.fetch(bearer, reservation["objectId"])
        assert status == 410, body
        assert json.loads(body)["code"] == "expired"
