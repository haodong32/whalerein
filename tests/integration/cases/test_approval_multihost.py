"""APPR-07: approval routing across several authorized Hosts.

The approval identity is Host-scoped: it lives in one Host's native source and
must never be guessed. Without a Host selector the decision fails closed, and an
explicit `--host` routes to exactly that Host.
"""

import uuid
from pathlib import Path

import pytest

from environment import ROOT, Process, http, wait

pytestmark = pytest.mark.chain


def _resolve_args(approval):
    return (
        "--request",
        approval["approval_id"],
        "--digest",
        approval["digest"],
        "--version",
        str(approval["version"]),
    )


def _start_second_host(scenario):
    """Start one more independent FakeDSH authority and plugin for a new Host."""
    directory = scenario.directory / "second-host"
    directory.mkdir()
    socket = directory / "dsh.sock"
    authority = Process(
        ["node", str(ROOT / "tests/integration/fixtures/dsh.mjs")],
        directory,
        {"WHALEREIN_DSH_SOCKET": str(socket)},
    )
    scenario.processes.append(authority)
    wait(lambda: socket.exists(), process=authority)
    host = "host-" + uuid.uuid4().hex[:12]
    plugin = Process(
        ["node", str(ROOT / "tests/integration/fixtures/plugin.mjs")],
        directory,
        {
            "WHALEREIN_DSH_SOCKET": str(socket),
            "WHALEREIN_MQTT_URL": getattr(scenario, "tls_url", "mqtt://127.0.0.1:1883"),
            "WHALEREIN_HOST_ID": host,
            "WHALEREIN_HOST_TOKEN": scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"],
            "WHALEREIN_STATE_DIR": str(directory / "plugin"),
            "WHALEREIN_HOST_CREDENTIAL_FILE": str(directory / "credentials.json"),
        },
    )
    scenario.processes.append(plugin)
    wait(lambda: "plugin ready" in Path(plugin.output.name).read_text())
    return host


@pytest.mark.description(
    "With two authorized Hosts an approval decision without --host names both candidate Hosts and fails closed, and --host routes only to the named Host."
)
def test_multihost_approval_requires_and_honours_explicit_host(scenario, paired_device):
    second_host = _start_second_host(scenario)
    invitation = http("/v1/pairing/invitations", {"hostId": second_host}, scenario.admin)
    paired_device.call("pair", invitation["pairingUri"])
    device = paired_device
    session = scenario.dsh.create_session()
    device.sessions(scenario.host)
    device.call("sessions", "--host", second_host)
    pending = scenario.dsh.request_approval(session)
    card = device.approvals(session)[0]
    before = scenario.dsh.process.control("gx_effect_counts")["resolveApproval"]

    code, values = device.invoke("approve", *_resolve_args(card), success=False)
    assert code == 8
    assert values[-1]["error"]["code"] == "conflict"
    message = values[-1]["error"]["message"]
    assert scenario.host in message and second_host in message
    assert "--host" in message
    assert scenario.dsh.process.control("gx_effect_counts")["resolveApproval"] == before
    assert scenario.dsh.approval(pending["approval_id"])["state"] == "pending"

    # An authorized but wrong Host cannot resolve another Host's approval.
    code, values = device.invoke(
        "--host", second_host, "approve", *_resolve_args(card), success=False
    )
    assert code == 8
    assert values[-1]["error"]["code"] == "approval_unavailable"
    assert scenario.dsh.approval(pending["approval_id"])["state"] == "pending"

    resolved = device.call("--host", scenario.host, "approve", *_resolve_args(card))
    assert resolved["status"] in {"host_accepted", "succeeded"}
    assert scenario.dsh.process.control("gx_effect_counts")["resolveApproval"] == before + 1
    assert scenario.dsh.approval(pending["approval_id"])["state"] == "approved"
