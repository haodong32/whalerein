"""Send validation, delivery-mode contracts and interrupt idempotence."""

import pytest


pytestmark = pytest.mark.chain


def _event_count(scenario, session_id):
    return len(scenario.dsh.snapshot(session_id)["events"])


@pytest.mark.description(
    "Empty and whitespace-only sends are rejected as invalid_argument and commit no user message."
)
def test_blank_send_is_invalid_argument_and_submits_nothing(
    scenario, paired_device, session_id
):
    before = _event_count(scenario, session_id)
    for text in ("", "   ", "\t\n"):
        code, values = paired_device.invoke(
            "send", "--session", session_id, text, success=False
        )
        assert code == 2, f"blank send must be invalid_argument, got {values[-1:]}"
        assert values[-1]["error"]["code"] == "invalid_argument"
    assert _event_count(scenario, session_id) == before, (
        "a rejected blank send must not create a user message"
    )


@pytest.mark.description(
    "start-if-idle on a running session reports conflict and queues nothing, while the same mode sends normally once the session is idle."
)
def test_start_if_idle_does_not_queue_while_running(
    scenario, paired_device, session_id
):
    scenario.dsh.start_running(session_id)
    before = _event_count(scenario, session_id)
    code, values = paired_device.invoke(
        "send",
        "--session",
        session_id,
        "--delivery",
        "start-if-idle",
        "must not queue",
        success=False,
    )
    assert code == 8, f"start-if-idle on a busy session must conflict, got {values[-1:]}"
    assert values[-1]["error"]["code"] == "conflict"
    assert _event_count(scenario, session_id) == before
    paired_device.invoke("wait", "--session", session_id, "--timeout", "10")

    # FakeDSH calls the idle control through the shared process API.
    scenario.dsh.process.control("idle", session_id=session_id)
    sent = paired_device.send(session_id, "starts only while idle", delivery="start-if-idle")
    assert sent["status"] == "succeeded"


@pytest.mark.description(
    "Interrupting an idle session succeeds, a repeated interrupt is accepted again, and wait still reports idle."
)
def test_repeated_interrupt_of_an_idle_session_remains_idle(
    scenario, paired_device, session_id
):
    first = paired_device.interrupt(session_id)
    assert first["status"] == "succeeded"
    second = paired_device.interrupt(session_id)
    assert second["status"] == "succeeded"
    assert paired_device.call("wait", "--session", session_id, "--timeout", "10")[
        "reason"
    ] == "idle"
