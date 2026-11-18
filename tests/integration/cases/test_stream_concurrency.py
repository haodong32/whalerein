"""One active watch per client instance; unrelated commands keep working."""

import pytest

pytestmark = pytest.mark.chain


@pytest.mark.description(
    "A second watch for the same client instance fails deterministically with conflict, and send still works while the first watch remains attached."
)
def test_a_second_watch_is_a_deterministic_conflict_while_send_works(
    paired_client, session_id
):
    paired_client.call("session", "show", session_id)
    first = paired_client.watch("session", "watch", session_id, "--timeout", "60")
    try:
        first.wait(
            lambda value: value.get("data", {}).get("event", {}).get("type")
            == "state",
            timeout=25,
        )
        competing = paired_client.watch("session", "watch", session_id, "--once", "--timeout", "3")
        try:
            assert competing.wait_exit() == 8
            assert competing.values[-1]["error"]["code"] == "conflict"
            assert "another watch is active" in competing.values[-1]["error"]["message"]
        finally:
            competing.close()

        # The competing watch must not have replaced the attached channel.
        paired_client.call("session", "send", session_id, "send after conflict fixture", "--detach")
        first.wait(
            lambda value: value.get("data", {}).get("event", {}).get("type")
            == "message"
            and any(
                "send after conflict fixture" in block.get("text", "")
                for block in value["data"]["event"].get("blocks", [])
            ),
            timeout=30,
        )
    finally:
        first.close()

    # Releasing the first watch releases the instance lease for the next watch.
    second = paired_client.watch("session", "watch", session_id, "--timeout", "15")
    try:
        second.wait(lambda value: value.get("data", {}).get("event", {}).get("type") == "state")
    finally:
        second.close()
