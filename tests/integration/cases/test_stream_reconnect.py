"""A reconnecting watcher neither replays committed history nor drops new output."""

import pytest

from environment import wait
from journey_helpers import restart_plugin

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
    "When the Host connection restarts, an attached watcher reconnects without replaying committed history and receives exactly one copy of the next turn."
)
def test_reconnect_does_not_replay_committed_history(scenario, paired_client, session_id):
    paired_client.call("session", "send", session_id, "baseline history fixture", "--detach")
    paired_client.call("session", "show", session_id)
    watcher = paired_client.watch("session", "watch", session_id, "--timeout", "60")
    try:
        watcher.wait(
            lambda value: value.get("data", {}).get("event", {}).get("type")
            == "state",
            timeout=25,
        )
        restart_plugin(scenario)

        def host_online():
            code, value = paired_client.invoke("host", "list")
            return code == 0 and any(
                row["host_id"] == scenario.host and row["online"]
                for row in value["data"]["hosts"]
            )

        wait(host_online, timeout=45)
        paired_client.call("session", "send", session_id, "after reconnect fixture", "--detach")
        watcher.wait(
            lambda _: len(_messages_with(watcher, "after reconnect fixture")) >= 2,
            timeout=45,
        )
    finally:
        watcher.close()
    assert not _messages_with(watcher, "baseline history fixture"), (
        "the reconnected watch must not replay committed history: "
        f"{_messages_with(watcher, 'baseline history fixture')}"
    )
    messages = _messages_with(watcher, "after reconnect fixture")
    message_ids = [message["message_id"] for message in messages]
    assert len(message_ids) == len(set(message_ids)), (
        "a committed message id was delivered more than once across reconnect: "
        f"{message_ids}"
    )
    # FakeDSH commits the user turn and its assistant echo as two distinct
    # messages; both must appear once, with distinct identities.
    assert len(message_ids) == 2, message_ids
    assert sorted(message["role"] for message in messages) == ["ASSISTANT", "USER"]
