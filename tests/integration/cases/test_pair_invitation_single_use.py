"""A pairing invitation authorizes exactly one device."""
import subprocess

import pytest
from environment import Device, http

pytestmark = pytest.mark.chain


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


@pytest.mark.description(
    "A second device reusing a consumed invitation gets a deterministic conflict "
    "and the first device keeps the only grant"
)
def test_second_device_cannot_reuse_a_consumed_invitation(scenario):
    invitation = http(
        "/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin
    )
    first = Device(scenario.directory, "client")
    first.identity = first.call("pair", invitation["pairingUri"])
    first_device_id = first.identity["device_id"]

    second = Device(scenario.directory, "client-2")
    code, envelopes = second.invoke("pair", invitation["pairingUri"], success=False)
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
