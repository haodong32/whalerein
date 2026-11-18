"""Send validation, delivery-mode contracts and interrupt idempotence."""

import pytest


pytestmark = pytest.mark.chain


def _event_count(scenario, session_id):
    return len(scenario.dsh.snapshot(session_id)["events"])


@pytest.mark.description(
    "Empty and whitespace-only sends are rejected as invalid_argument and commit no user message."
)
def test_blank_send_is_invalid_argument_and_submits_nothing(
    scenario, paired_client, session_id
):
    before = _event_count(scenario, session_id)
    for text in ("", "   ", "\t\n"):
        code, value = paired_client.invoke("session", "send", session_id, text)
        assert code == 2, f"blank send must be invalid_argument, got {value}"
        assert value["error"]["code"] == "invalid_argument"
    assert _event_count(scenario, session_id) == before, (
        "a rejected blank send must not create a user message"
    )


@pytest.mark.description(
    "start-if-idle on a running session reports conflict and queues nothing, while the same mode sends normally once the session is idle."
)
def test_start_if_idle_does_not_queue_while_running(
    scenario, paired_client, session_id
):
    scenario.dsh.start_running(session_id)
    before = _event_count(scenario, session_id)
    code, value = paired_client.invoke(
        "session", "send", session_id, "must not queue", "--delivery", "start-if-idle"
    )
    assert code == 8, f"start-if-idle on a busy session must conflict, got {value}"
    assert value["error"]["code"] == "conflict"
    assert _event_count(scenario, session_id) == before
    # FakeDSH calls the idle control through the shared process API.
    scenario.dsh.process.control("idle", session_id=session_id)
    sent = paired_client.call(
        "session", "send", session_id, "starts only while idle", "--delivery", "start-if-idle", "--detach"
    )
    assert sent["receipt"]["status"] in ("host_accepted", "succeeded")
    recovered = paired_client.call("operation", "recover", sent["operation_id"])
    assert recovered["reply"]["state"] == "committed"


@pytest.mark.description(
    "Interrupting an idle session succeeds, a repeated interrupt is accepted again, and wait still reports idle."
)
def test_repeated_interrupt_of_an_idle_session_remains_idle(
    paired_client, session_id
):
    first = paired_client.call("session", "interrupt", session_id)
    assert first["status"] == "succeeded"
    second = paired_client.call("session", "interrupt", session_id)
    assert second["status"] == "succeeded"
    assert paired_client.call("session", "wait", session_id, "--timeout", "10")[
        "reason"
    ] == "idle"
