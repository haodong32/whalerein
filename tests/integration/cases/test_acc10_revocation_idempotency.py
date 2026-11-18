"""ACC-10: deterministic, idempotent device revocation for both callers."""

import json
import urllib.error
import urllib.request

import pytest

pytestmark = pytest.mark.chain

BASE = "http://127.0.0.1:18080"


def _delete(device_id, token=None):
    headers = {} if token is None else {"Authorization": "Bearer " + token}
    request = urllib.request.Request(
        BASE + "/v1/devices/" + device_id, method="DELETE", headers=headers
    )
    try:
        with urllib.request.urlopen(request, timeout=5) as response:
            return response.status, json.loads(response.read() or b"null")
    except urllib.error.HTTPError as error:
        return error.code, json.loads(error.read() or b"null")


@pytest.mark.description(
    "A device revokes itself once (204) and the same credential retry is a stable 401, while repeated admin revocation answers 204 and unknown or foreign devices stay 404 and 403."
)
def test_device_revocation_is_idempotent_for_admin_and_device_callers(scenario):
    device = scenario.pair()
    device_id = device.identity["device_id"]

    with scenario.fixture_bearer(device) as token:
        # The owning device revokes itself.
        status, body = _delete(device_id, token)
        assert status == 204, (status, body)
        # Its credential is gone after revocation. The retry is deterministic:
        # 401 unauthorized, never an ambiguous 403 or a 500.
        status, body = _delete(device_id, token)
        assert status == 401, (status, body)
        assert body and body.get("code") == "unauthorized", body

    # A repeated admin revoke reaches the same end state: 204 every time.
    for attempt in range(2):
        status, body = _delete(device_id, scenario.admin)
        assert status == 204, (attempt, status, body)

    # A different valid device token is forbidden, never unauthorized.
    other = scenario.pair_second()
    with scenario.fixture_bearer(other) as token:
        status, body = _delete(device_id, token)
    assert status == 403, (status, body)
    assert body and body.get("code") == "forbidden", body

    # An unknown but well-formed target is a stable not_found.
    status, body = _delete("unknown-device-id", scenario.admin)
    assert status == 404, (status, body)
    assert body and body.get("code") == "not_found", body
