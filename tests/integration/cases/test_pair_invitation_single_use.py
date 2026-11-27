"""A pairing invitation authorizes exactly one device."""
import pytest
from environment import Device, http
from journey_helpers import sql as scalar

pytestmark = pytest.mark.chain


@pytest.mark.description(
    "A second device reusing a consumed invitation gets a deterministic conflict "
    "and the first device keeps the only grant"
)
def test_second_device_cannot_reuse_a_consumed_invitation(scenario):
    invitation = http(
        "/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin
    )
    first = Device(scenario.directory, "client")
    first.identity = first.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation["pairingUri"])
    first_device_id = first.identity["device_id"]

    second = Device(scenario.directory, "client-2")
    code, envelopes = second.invoke('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation["pairingUri"], success=False)
    assert code == 8, "a consumed invitation must report a deterministic conflict"
    error = envelopes[-1]["error"]
    assert error["code"] == "conflict"
    assert "used" in error["message"].lower()

    # A is still paired and can use its grant.
    assert scenario.host in {host["host_id"] for host in first.hosts()}
    assert first.sessions(scenario.host) == []

    # Exactly one committed grant, one device row and one consumed invitation.
    assert scalar(scenario.schema, "SELECT count(*) FROM grants") == "1"
    assert (
        scalar(
            scenario.schema,
            f"SELECT count(*) FROM grants WHERE device_id='{first_device_id}' AND revoked_at IS NULL",
        )
        == "1"
    )
    assert scalar(scenario.schema, "SELECT count(*) FROM devices") == "1"
    assert (
        scalar(
            scenario.schema,
            "SELECT count(*) FROM pairing_tokens WHERE used_at IS NOT NULL",
        )
        == "1"
    )
    assert (
        scalar(
            scenario.schema,
            f"SELECT count(*) FROM pairing_tokens WHERE host_id='{scenario.host}' AND used_at IS NULL",
        )
        == "0"
    )
