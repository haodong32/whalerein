"""A reconnecting watcher neither replays committed history nor drops new output."""

import pytest

from journey_helpers import restart_plugin, wait_online

pytestmark = pytest.mark.chain


def _events(watcher):
    return [
        value["data"]["event"]
        for value in watcher.values
        if value.get("data", {}).get("event")
    ]


def _messages_with(watcher, needle):
    return [
        event
        for event in _events(watcher)
        if event.get("type") == "message"
        and any(needle in block.get("text", "") for block in event.get("blocks", []))
    ]


@pytest.mark.description(
    "When the Host connection restarts, an attached tail reconnects without replaying committed history and receives exactly one copy of the next turn."
)
def test_reconnect_does_not_replay_committed_history(scenario, paired_device, session_id):
    paired_device.send(session_id, "baseline history fixture")
    paired_device.sync(session_id)
    tail = paired_device.start_watch(
        "tail", "--session", session_id, "--timeout", "60"
    )
    try:
        tail.wait(
            lambda value: value.get("data", {}).get("event", {}).get("type")
            == "state",
            timeout=25,
        )
        restart_plugin(scenario)
        wait_online(paired_device, scenario.host, timeout=45)
        paired_device.send(session_id, "after reconnect fixture")
        tail.wait(
            lambda value: value.get("data", {}).get("event", {}).get("type")
            == "message"
            and any(
                "after reconnect fixture" in block.get("text", "")
                for block in value["data"]["event"].get("blocks", [])
            ),
            timeout=45,
        )
    finally:
        tail.close()
    assert not _messages_with(tail, "baseline history fixture"), (
        "the reconnected watch must not replay committed history: "
        f"{_messages_with(tail, 'baseline history fixture')}"
    )
    messages = _messages_with(tail, "after reconnect fixture")
    message_ids = [message["message_id"] for message in messages]
    assert len(message_ids) == len(set(message_ids)), (
        "a committed message id was delivered more than once across reconnect: "
        f"{message_ids}"
    )
    # FakeDSH commits the user turn and its assistant echo as two distinct
    # messages; both must appear once, with distinct identities.
    assert len(message_ids) == 2, message_ids
    assert sorted(message["role"] for message in messages) == ["ASSISTANT", "USER"]
