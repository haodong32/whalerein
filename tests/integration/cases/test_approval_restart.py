"""APPR-08/09/11: approval decisions across Host, plugin and Access restarts.

FakeDSH is a durable native authority, so it can also simulate the opposite
case: the authority is fresh and the previously pending approval no longer
exists. The Host must then surface a definitive approval_* code, not unknown.
"""

import time

import pytest

from environment import wait
from journey_helpers import host_online, restart_access, restart_plugin, wait_online

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


def _resolve_count(scenario):
    return scenario.dsh.process.control("gx_effect_counts")["resolveApproval"]


def _card(device, session, approval_id):
    try:
        return next(
            (card for card in device.approvals(session) if card["approval_id"] == approval_id),
            None,
        )
    except AssertionError:
        # The CLI may briefly fail while Access restarts and the broker
        # subscription is rebuilt; the case waits for the recovered answer.
        return None


@pytest.mark.description(
    "A decision submitted while the Host is offline never reports approved, and the explicit retry after restart settles exactly once."
)
def test_offline_host_decision_is_not_approved_and_retry_is_explicit(scenario):
    device = scenario.pair()
    session = scenario.dsh.create_session()
    device.sessions(scenario.host)
    pending = scenario.dsh.request_approval(session)
    card = device.approvals(session)[0]
    before = _resolve_count(scenario)
    scenario.plugin.close()
    wait(lambda: host_online(device, scenario.host) is False, timeout=40)

    deadline = time.monotonic() + 30
    code, values = 0, []
    while time.monotonic() < deadline:
        code, values = _resolve(device, card)
        assert code != 0
        if code == 5:
            break
        time.sleep(0.5)
    assert code == 5
    assert values[-1]["error"]["code"] == "host_offline"
    assert scenario.dsh.approval(pending["approval_id"])["state"] == "pending"
    assert _resolve_count(scenario) == before

    restart_plugin(scenario)
    wait_online(device, scenario.host)
    code, values = _resolve(device, card)
    if code == 0:
        assert values[-1]["data"]["status"] == "succeeded"
    else:
        # If Access had already handed the offline submission to the restarted
        # Host, the retry must report the terminal already-resolved decision.
        assert code == 8
        assert values[-1]["error"]["code"] == "approval_already_resolved"
    assert _resolve_count(scenario) == before + 1
    assert scenario.dsh.approval(pending["approval_id"])["state"] == "approved"


@pytest.mark.description(
    "Restarting Access while an approval is pending keeps the approval visible and resolvable exactly once."
)
def test_access_restart_keeps_pending_approval_resolvable(scenario):
    device = scenario.pair()
    session = scenario.dsh.create_session()
    device.sessions(scenario.host)
    pending = scenario.dsh.request_approval(session)
    before = _resolve_count(scenario)
    restart_access(scenario)
    wait_online(device, scenario.host)
    # `environment.wait` only blocks until the condition holds; read the card
    # again afterwards to observe it.
    wait(
        lambda: _card(device, session, pending["approval_id"]),
        timeout=40,
    )
    recovered = _card(device, session, pending["approval_id"])
    assert recovered["state"] == "APPROVAL_STATE_PENDING"
    resolved = device.resolve("approve", recovered)
    assert resolved["status"] == "succeeded"
    assert _resolve_count(scenario) == before + 1
    assert scenario.dsh.approval(pending["approval_id"])["state"] == "approved"


@pytest.mark.description(
    "When the native approval is lost with the previous process, a late decision exits 8 approval_unavailable and never resolves twice."
)
def test_lost_pending_approval_surfaces_unavailable_not_unknown(scenario):
    device = scenario.pair()
    session = scenario.dsh.create_session()
    device.sessions(scenario.host)
    pending = scenario.dsh.request_approval(session)
    card = device.approvals(session)[0]
    # The plugin restart loses its in-memory claim; the native authority also
    # no longer holds the action (modelled explicitly, not by replacing the
    # authority, so the paired Host identity and fixture counters survive).
    assert scenario.dsh.process.control("forget_approval", approval_id=pending["approval_id"])
    assert scenario.dsh.approval(pending["approval_id"]) is None
    scenario.plugin.close()
    restart_plugin(scenario)
    wait_online(device, scenario.host)
    code, values = _resolve(device, card)
    assert code == 8
    assert values[-1]["error"]["code"] == "approval_unavailable"
    assert _resolve_count(scenario) == 0
    assert scenario.dsh.approval(pending["approval_id"]) is None
