"""Revocation reaches a Host that was disconnected when the grant was revoked."""
import subprocess

import pytest
from environment import http, wait

pytestmark = pytest.mark.__getattr__("native-dsh")


def scalar(schema, statement):
    result = subprocess.run(
        [
            "psql",
            "-h",
            "127.0.0.1",
            "-p",
            "55432",
            "-U",
            "postgres",
            "-d",
            "whalerein_test",
            "-tAc",
            f"SET search_path TO {schema}; {statement}",
        ],
        check=True,
        capture_output=True,
        text=True,
        timeout=5,
    )
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    return lines[-1] if lines else ""


def pending_replays(schema, device_id):
    return scalar(
        schema,
        f"SELECT count(*) FROM grants WHERE device_id='{device_id}' "
        "AND revoked_at IS NOT NULL AND replayed_at IS NULL",
    )


def live_grants(schema, device_id):
    return scalar(
        schema,
        f"SELECT count(*) FROM grants WHERE device_id='{device_id}' AND revoked_at IS NULL",
    )


def revoked_token_is_rejected(token):
    with pytest.raises(RuntimeError, match="HTTP (401|403)"):
        http("/v1/hosts", token=token)


@pytest.fixture
def offline_broker(shared_broker):
    if shared_broker is None:
        pytest.skip("offline replay needs the controllable shared broker fixture")
    return shared_broker


@pytest.mark.description(
    "A device revocation committed while the Host is offline replays on reconnect, "
    "clears the Host peer and leaves the old token rejected"
)
def test_device_revocation_while_host_offline_replays_on_reconnect(
    scenario, paired_device, offline_broker
):
    device = paired_device
    device_id = device.identity["device_id"]
    token = device.transport_token()
    assert scenario.request("ready")["connected"]
    assert scenario.host_peer_exists(device_id)

    offline_broker.close()
    wait(lambda: not scenario.request("ready")["connected"], timeout=20)
    device.revoke()
    assert live_grants(scenario.schema, device_id) == "0"
    assert pending_replays(scenario.schema, device_id) == "1"
    assert scenario.request("ready")["connected"] is False

    offline_broker.ensure_started()
    wait(lambda: scenario.request("ready")["connected"], timeout=60)
    wait(
        lambda: pending_replays(scenario.schema, device_id) == "0",
        timeout=60,
    )
    wait(lambda: not scenario.host_peer_exists(device_id), timeout=30)
    assert scenario.request("ready")["replayPending"] is False
    revoked_token_is_rejected(token)


@pytest.mark.description(
    "A Host-signed revocation submitted while the Host has no broker session is "
    "accepted, queued and replayed when the Host reconnects"
)
def test_host_signed_revocation_while_disconnected_is_queued(
    scenario, paired_device, offline_broker
):
    device = paired_device
    device_id = device.identity["device_id"]
    token = device.transport_token()
    assert scenario.request("ready")["connected"]
    assert scenario.host_peer_exists(device_id)

    offline_broker.close()
    wait(lambda: not scenario.request("ready")["connected"], timeout=20)
    assert scenario.request("gx-revoke-host-grant", deviceId=device_id)["revoked"]
    assert live_grants(scenario.schema, device_id) == "0"
    assert pending_replays(scenario.schema, device_id) == "1"

    offline_broker.ensure_started()
    wait(lambda: scenario.request("ready")["connected"], timeout=60)
    wait(
        lambda: pending_replays(scenario.schema, device_id) == "0",
        timeout=60,
    )
    wait(lambda: not scenario.host_peer_exists(device_id), timeout=30)
    assert scenario.request("ready")["replayPending"] is False
    revoked_token_is_rejected(token)
