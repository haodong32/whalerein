"""The invitation URI contract shared by Access and mobile clients."""
import re
import urllib.parse

import pytest
from environment import Device, http

pytestmark = pytest.mark.chain

# Mobile clients keep the same narrow token alphabet and reject an expired link
# locally, so the URI must expose whole-second expiry and a URL-safe token.
TOKEN = re.compile(r"[A-Za-z0-9._~-]{8,}")


@pytest.mark.description(
    "The Access invitation URI carries version, access, host, token and whole-second "
    "expiry that the mobile parser accepts"
)
def test_invitation_uri_matches_the_cross_component_contract(scenario):
    invitation = http(
        "/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin
    )
    uri = urllib.parse.urlsplit(invitation["pairingUri"])
    assert uri.scheme == "whalerein"
    assert uri.netloc == "pair"
    query = urllib.parse.parse_qs(uri.query)
    assert query["v"] == ["1"]
    assert query["access"] == ["http://127.0.0.1:18080"]
    assert query["host"] == [scenario.host]
    assert query["token"] == [invitation["token"]]
    assert TOKEN.fullmatch(invitation["token"])
    # `exp` is Unix seconds (the iOS parser uses TimeInterval), not the
    # millisecond field returned in JSON.
    assert "exp" in query, "mobile local expiry needs an exp query field"
    expiry_seconds = int(query["exp"][0])
    assert expiry_seconds == int(invitation["expiresAtUnixMs"]) // 1000
    assert expiry_seconds > 0

    # The emitted URI is directly usable, so the contract is not cosmetic.
    device = Device(scenario.directory, "client")
    device.identity = device.call("pair", invitation["pairingUri"])
    assert scenario.host in {host["host_id"] for host in device.hosts()}
