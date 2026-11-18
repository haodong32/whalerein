"""APPR-10: a revoked device cannot deliver an approval decision."""

import pytest

from environment import wait

pytestmark = pytest.mark.chain



def _resolve(device, approval, decision="approve"):
    return device.invoke(
        "session", "approval", decision,
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


@pytest.mark.description(
    "A locally revoked device exits 4 not_paired for an approval decision and never reaches the Host."
)
def test_approval_after_local_revocation_is_not_paired(scenario):
    device = scenario.pair()
    session = scenario.dsh.create_session()
    device.sessions(scenario.host)
    scenario.dsh.request_approval(session)
    approval = device.approvals(session)[0]
    before = _resolve_count(scenario)
    device.revoke()
    code, values = _resolve(device, approval)
    assert code == 4
    assert values[-1]["error"]["code"] == "not_paired"
    assert _resolve_count(scenario) == before
    assert scenario.dsh.approval(approval["approval_id"])["state"] == "pending"


@pytest.mark.description(
    "When one device is revoked, the surviving device resolves the pending approval exactly once and the stale device stays local."
)
def test_revocation_and_approval_decision_order_keeps_one_winner(scenario):
    stale = scenario.pair()
    active = scenario.pair_second()
    session = scenario.dsh.create_session()
    stale.sessions(scenario.host)
    active.sessions(scenario.host)
    scenario.dsh.request_approval(session)
    approval = active.approvals(session)[0]
    before = _resolve_count(scenario)
    stale.revoke()
    resolved = active.resolve("approve", approval)
    assert resolved["status"] in {"host_accepted", "succeeded"}
    wait(lambda: scenario.dsh.approval(approval["approval_id"])["state"] == "approved")
    assert _resolve_count(scenario) == before + 1
    code, values = _resolve(stale, approval)
    assert code == 4
    assert values[-1]["error"]["code"] == "not_paired"
    assert _resolve_count(scenario) == before + 1
    assert scenario.dsh.approval(approval["approval_id"])["state"] == "approved"


@pytest.mark.description(
    "Repeating a resolved approval from the same stale guard reports approval_already_resolved instead of unknown."
)
def test_repeated_approval_decision_reports_already_resolved(scenario):
    device = scenario.pair()
    session = scenario.dsh.create_session()
    device.sessions(scenario.host)
    scenario.dsh.request_approval(session)
    approval = device.approvals(session)[0]
    device.resolve("reject", approval)
    code, values = _resolve(device, approval)
    assert code == 8
    assert values[-1]["error"]["code"] == "approval_already_resolved"
    assert scenario.dsh.process.control("gx_effect_counts")["resolveApproval"] == 1
