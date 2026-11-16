"""QMQ queue lifecycle: cold vs unknown sessions, exact content round-trip and action availability."""

import pytest

pytestmark = pytest.mark.chain


@pytest.mark.description("Queue list distinguishes an unknown session (not_found) from a cold session.")
def test_queue_list_distinguishes_unknown_from_cold(scenario, paired_device):
    cold = "qmq-queue-cold"
    assert scenario.dsh.process.control("create_cold_session", session_id=cold)
    paired_device.sessions(scenario.host)
    code, values = paired_device.invoke("queue", "list", "--session", "qmq-queue-unknown", success=False)
    assert code == 3
    assert values[-1]["error"]["code"] == "not_found"
    assert values[-1].get("data") is None
    queue = paired_device.call("queue", "list", "--session", cold)
    assert queue["session_id"] == cold
    assert queue["available"] is False
    assert queue["items"] == []


@pytest.mark.description("Queue edit round-trips multi-byte, newline and whitespace content exactly.")
def test_queue_edit_round_trips_multibyte_content(scenario, paired_device):
    session = "qmq-queue-content"
    assert scenario.dsh.process.control("create_queue_session", session_id=session)
    paired_device.sessions(scenario.host)
    text = "第一行\n第二行\t😀 café  "
    paired_device.call("queue", "edit", "--session", session, "--item", "queue-fixture", text)
    item = paired_device.call("queue", "list", "--session", session)["items"][0]
    assert item["blocks"][0]["text"] == text
    # A large multi-byte replacement stays complete (no truncation or mojibake).
    large = ("数据行\n" * 5000).strip()
    paired_device.call("queue", "edit", "--session", session, "--item", "queue-fixture", large)
    assert paired_device.call("queue", "list", "--session", session)["items"][0]["blocks"][0]["text"] == large
    assert len(large.encode("utf-8")) > len(large) * 2
    # Whitespace-only content is invalid and leaves the source unchanged.
    code, values = paired_device.invoke(
        "queue", "edit", "--session", session, "--item", "queue-fixture", "  \n\t ", success=False
    )
    assert code == 2
    assert values[-1]["error"]["code"] == "invalid_argument"
    assert paired_device.call("queue", "list", "--session", session)["items"][0]["blocks"][0]["text"] == large


@pytest.mark.description("Queue promotion availability follows live state and a state flip is a definitive conflict.")
def test_queue_promotion_follows_live_state(scenario, paired_device):
    session = "qmq-queue-state"
    assert scenario.dsh.process.control("create_queue_session", session_id=session)
    paired_device.sessions(scenario.host)
    actions = paired_device.call("queue", "list", "--session", session)["items"][0]["actions"]
    assert "PROMOTE" in actions
    # The pre-read observes the idle state: promotion is not offered and the
    # command is a definitive conflict, not an unknown outcome.
    assert scenario.dsh.process.control("set_queue_idle", session_id=session)
    assert "PROMOTE" not in paired_device.call("queue", "list", "--session", session)["items"][0]["actions"]
    code, values = paired_device.invoke("queue", "steer", "--session", session, "--item", "queue-fixture", success=False)
    assert code == 8
    assert values[-1]["error"]["code"] == "conflict"
    # Re-reading after the session starts running makes the same action valid.
    assert scenario.dsh.process.control("running", session_id=session)
    assert "PROMOTE" in paired_device.call("queue", "list", "--session", session)["items"][0]["actions"]
    result = paired_device.call("queue", "steer", "--session", session, "--item", "queue-fixture")
    assert result["command"]["status"] == "succeeded"
    refreshed = paired_device.call("queue", "list", "--session", session)
    assert len(refreshed["items"]) == 1
    assert refreshed["items"][0]["placement"] == "STEERING"
