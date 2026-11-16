"""APPR-20: claim versus delegate capability advertisement.

In delegate mode the native approval stays in the local DSH UI, so the Host has
no remote approval surface to offer. It must not advertise approvals and must
never claim or forward a remote decision.
"""

import pytest

from journey_helpers import restart_plugin, wait_online

pytestmark = pytest.mark.chain


def _resolve(device, approval, decision="approve"):
    return device.invoke(
        decision,
        "--request",
        approval["approval_id"],
        "--digest",
        approval["digest"],
        "--version",
        str(approval["version"]),
        success=False,
    )


@pytest.mark.description(
    "In delegate mode the Host advertises no approvals and a remote decision exits 8 without reaching the native approval."
)
def test_delegate_mode_withholds_approval_advertisement(scenario, paired_device):
    restart_plugin(scenario, extra_env={"WHALEREIN_APPROVAL_MODE": "delegate"})
    wait_online(paired_device, scenario.host)
    device = paired_device
    session = scenario.dsh.create_session()
    row = next(
        row for row in device.sessions(scenario.host) if row["session_id"] == session
    )
    assert row["source_capabilities"]["approvals"] is False

    pending = scenario.dsh.request_approval(session)
    assert device.approvals(session) == []
    before = scenario.dsh.process.control("gx_effect_counts")["resolveApproval"]
    code, values = _resolve(device, pending)
    assert code == 8
    assert values[-1]["error"]["code"] == "approval_unavailable"
    assert scenario.dsh.process.control("gx_effect_counts")["resolveApproval"] == before
    assert scenario.dsh.approval(pending["approval_id"])["state"] == "pending"
