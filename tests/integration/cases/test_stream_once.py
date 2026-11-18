"""Real-time output semantics for `watch --once` and `interrupt` termination."""

import pytest


pytestmark = pytest.mark.chain


@pytest.mark.description(
    "When watch --once runs with a cold cache, replayed snapshot history is not visible output and the command times out exit 7."
)
def test_watch_once_cold_cache_does_not_replay_history(scenario, paired_client, session_id):
    """Snapshot replay describes history; it is not new output for --once."""
    scenario.dsh.process.control("emit", session_id=session_id)
    watch = paired_client.watch("session", "watch", session_id, "--once", "--timeout", "3")
    try:
        code = watch.wait_exit()
        values = watch.values
    finally:
        watch.close()
    assert code == 7, f"a snapshot-only --once must time out, got {code}: {values[-1:]}"
    assert values[-1]["error"]["code"] == "timeout"
    assert all(
        value.get("data", {}).get("event", {}).get("type") != "message"
        for value in values
        if value.get("data")
    )


@pytest.mark.description(
    "When the cache is synchronized first, watch --once exits with only the newly committed turn and never replays older history."
)
def test_watch_once_after_sync_reports_only_new_output(paired_client, session_id):
    """An explicit sync moves the cursor, so subsequent watch output is new."""
    paired_client.call("session", "send", session_id, "old history fixture", "--detach")
    paired_client.call("session", "show", session_id)
    watch = paired_client.watch("session", "watch", session_id, "--once", "--timeout", "15")
    try:
        watch.wait(lambda value: value.get("data", {}).get("event", {}).get("type") == "state")
        paired_client.call("session", "send", session_id, "new turn fixture", "--detach")
        watch.wait(
            lambda value: value.get("data", {}).get("event", {}).get("type") == "message",
            timeout=20,
        )
        assert watch.wait_exit(timeout=10) == 0
    finally:
        watch.close()
    events = [
        value["data"]["event"]
        for value in watch.values
        if value.get("data", {}).get("event")
    ]
    messages = [event for event in events if event["type"] == "message"]
    assert messages, "watch --once produced no committed Message"
    texts = [
        block.get("text", "") for message in messages for block in message["blocks"]
    ]
    assert any("new turn fixture" in text for text in texts), texts
    assert all("old history fixture" not in text for text in texts), texts


@pytest.mark.description(
    "When interrupt is acknowledged and settled by the Host, the CLI reports terminal status succeeded rather than host_accepted."
)
def test_interrupt_reports_a_terminal_status(scenario, paired_client, session_id):
    """An accepted interrupt is followed to its terminal receipt."""
    scenario.dsh.start_running(session_id)
    result = paired_client.call("session", "interrupt", session_id)
    assert result["status"] == "succeeded"
    assert result["status"] != "host_accepted"
    assert paired_client.call("session", "wait", session_id, "--timeout", "10")[
        "reason"
    ] == "idle"
