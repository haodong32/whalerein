"""ACC-08: an unavailable host must not consume an invitation's failure budget."""

import base64
import json
import urllib.error
import urllib.request

import pytest

from environment import Device, http

pytestmark = pytest.mark.chain

BASE = "http://127.0.0.1:18080"


def _claim(token, claim_id, timeout=30):
    body = json.dumps(
        {
            "token": token,
            "claimId": claim_id,
            "phase": "PAIRING_PHASE_HELLO",
            "noiseMessage": base64.b64encode(b"offline-probe-noise").decode(),
            "deviceId": "budget-probe",
            "protocolVersion": 1,
        }
    ).encode()
    request = urllib.request.Request(
        BASE + "/v1/pairing/claims",
        data=body,
        method="POST",
        headers={"Content-Type": "application/json"},
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


@pytest.mark.description(
    "Repeated HELLO claims while the host is offline answer unavailable without consuming the invitation failure budget, so pairing succeeds after the host returns."
)
def test_offline_host_does_not_lock_a_legitimate_invitation(scenario):
    invitation = http(
        "/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin
    )
    token = invitation["token"]

    scenario.plugin.close()

    # Wait for broker presence to observe the disconnect. A claim that still
    # races the presence cache forwards once and then answers unavailable; it
    # must not count either outcome as an authentication failure.
    offline_seen = False
    for attempt in range(12):
        status, body = _claim(token, "offline-probe-%d" % attempt)
        if status == 503:
            offline_seen = True
            break
        assert status != 429, body
    assert offline_seen, "host never became unavailable"

    # More attempts than the configured failure budget (5). Every one is a
    # transport outage, never rate_limited.
    for attempt in range(8):
        status, body = _claim(token, "offline-budget-%d" % attempt)
        assert status == 503, (attempt, status, body)
        assert body and body.get("code") == "unavailable", body

    # The invitation still works once the host is back: no legitimate user was
    # locked out by an outage.
    scenario.start_plugin()
    device = Device(scenario.directory, namespace="offline-recovery")
    identity = device.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation["pairingUri"])
    assert identity["device_id"]
    assert device.hosts(), "paired device cannot list its host"
