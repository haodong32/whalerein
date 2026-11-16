"""APPR-02/05/06/13/14: approval lifecycle across devices, TTL and the wire.

These cases use the real CLI, Access, encrypted control path, plugin Host and
FakeDSH. FakeDSH is the durable native authority; the plugin is the contributor
that owns claim/decision semantics. Real-DSH native behaviour is validated in
the native suite.
"""

import pytest

from environment import wait

pytestmark = pytest.mark.chain


def _resolve(device, approval, decision="approve", digest=None, version=None):
    return device.invoke(
        decision,
        "--request",
        approval["approval_id"],
        "--digest",
        digest if digest is not None else approval["digest"],
        "--version",
        str(version if version is not None else approval["version"]),
        success=False,
    )


def _resolve_count(scenario):
    return scenario.dsh.process.control("gx_effect_counts")["resolveApproval"]


def _cards(device, session):
    return device.approvals(session)


@pytest.mark.description(
    "Two devices that decide the same pending approval produce exactly one native resolution and the loser exits 8 approval_already_resolved."
)
def test_two_devices_decide_one_approval_once(scenario):
    first = scenario.pair()
    second = scenario.pair_second()
    session = scenario.dsh.create_session()
    first.sessions(scenario.host)
    second.sessions(scenario.host)
    pending = scenario.dsh.request_approval(session)
    first_card = _cards(first, session)[0]
    second_card = _cards(second, session)[0]
    assert (first_card["approval_id"], first_card["digest"], first_card["version"]) == (
        second_card["approval_id"],
        second_card["digest"],
        second_card["version"],
    )
    before = _resolve_count(scenario)
    assert second.resolve("approve", second_card)["status"] == "succeeded"
    code, values = _resolve(first, first_card)
    assert code == 8
    assert values[-1]["error"]["code"] == "approval_already_resolved"
    assert _resolve_count(scenario) == before + 1
    assert scenario.dsh.approval(pending["approval_id"])["state"] == "approved"
    assert _cards(first, session) == []
    assert _cards(second, session) == []


@pytest.mark.description(
    "Stale digest, stale version, a repeated decision and TTL expiry all exit 8 with an approval_* code, never a generic unknown."
)
def test_stale_approval_guards_surface_approval_codes(scenario):
    device = scenario.pair()
    session = scenario.dsh.create_session()
    device.sessions(scenario.host)
    pending = scenario.dsh.request_approval(session)
    card = _cards(device, session)[0]
    before = _resolve_count(scenario)
    code, values = _resolve(device, card, digest="00" * 32)
    assert code == 8
    assert values[-1]["error"]["code"] == "approval_version_mismatch"
    code, values = _resolve(device, card, version=card["version"] + 7)
    assert code == 8
    assert values[-1]["error"]["code"] == "approval_version_mismatch"
    assert _resolve_count(scenario) == before
    assert _cards(device, session)[0]["approval_id"] == card["approval_id"]
    assert device.resolve("approve", card)["status"] == "succeeded"
    code, values = _resolve(device, card)
    assert code == 8
    assert values[-1]["error"]["code"] == "approval_already_resolved"
    assert _resolve_count(scenario) == before + 1

    expiring = scenario.dsh.request_approval(session, ttl_ms=1000)
    wait(lambda: not _cards(device, session), timeout=20)
    code, values = _resolve(device, expiring)
    assert code == 8
    assert values[-1]["error"]["code"] == "approval_expired"
    assert _resolve_count(scenario) == before + 1
    assert scenario.dsh.approval(pending["approval_id"])["state"] == "approved"


@pytest.mark.description(
    "After an approval TTL expiry the session is idle and a later turn can request and approve a new approval."
)
def test_ttl_expiry_returns_idle_and_next_approval_works(scenario):
    device = scenario.pair()
    session = scenario.dsh.create_session()
    device.sessions(scenario.host)
    scenario.dsh.request_approval(session, ttl_ms=1000)
    expired_card = _cards(device, session)[0]
    wait(lambda: not _cards(device, session), timeout=20)
    assert device.call("wait", "--session", session, "--timeout", "5")["reason"] == "idle"

    device.send(session, "fixture follow-up turn")
    scenario.dsh.request_approval(session, ttl_ms=30000)
    next_card = _cards(device, session)[0]
    assert next_card["approval_id"] != expired_card["approval_id"]
    resolved = device.resolve("approve", next_card)
    # An approval decision acknowledged during a restart/channel rebuild can be
    # reported as the non-terminal host_accepted copy; the native decision must
    # still settle exactly once.
    assert resolved["status"] in {"host_accepted", "succeeded"}
    wait(
        lambda: scenario.dsh.approval(next_card["approval_id"])["state"] == "approved",
        timeout=15,
    )
    code, values = _resolve(device, expired_card)
    assert code == 8
    assert values[-1]["error"]["code"] == "approval_expired"


@pytest.mark.description(
    "Two concurrent approvals keep independent versions and a stable list order while only the earlier TTL expires."
)
def test_concurrent_approvals_keep_independent_ttls_and_stable_order(scenario):
    device = scenario.pair()
    session = scenario.dsh.create_session()
    device.sessions(scenario.host)
    first = scenario.dsh.request_approval(session, ttl_ms=1500)
    second = scenario.dsh.request_approval(session, ttl_ms=30000)
    assert second["version"] > first["version"]
    for _ in range(3):
        assert [card["approval_id"] for card in _cards(device, session)] == [
            first["approval_id"],
            second["approval_id"],
        ]
    wait(
        lambda: [card["approval_id"] for card in _cards(device, session)]
        == [second["approval_id"]],
        timeout=20,
    )
    assert device.resolve("approve", second)["status"] == "succeeded"
    assert scenario.dsh.approval(first["approval_id"])["state"] == "expired"


@pytest.mark.description(
    "A long native approval reason is bounded on the wire and no tool arguments or parameter fields are exposed."
)
def test_approval_reason_is_bounded_and_arguments_never_enter_the_wire(scenario):
    device = scenario.pair()
    session = scenario.dsh.create_session()
    device.sessions(scenario.host)
    secret = "tool-argument-secret-marker"
    scenario.dsh.request_approval(
        session,
        ttl_ms=30000,
        reason=("escalate sandbox " * 400) + secret,
        tool_name="Bash",
    )
    card = _cards(device, session)[0]
    assert 0 < len(card["reason"]) <= 1024
    assert secret not in card["reason"]
    assert set(card) == {
        "approval_id",
        "session_id",
        "tool_name",
        "call_id",
        "reason",
        "digest",
        "version",
        "expires_at_unix_ms",
        "state",
        "decision",
    }
    serialized = repr(card)
    assert "arguments" not in serialized
    assert "parameters" not in serialized
    assert len(card["tool_name"]) <= 256
