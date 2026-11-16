"""One active watch per device profile; unrelated commands keep working."""

import pytest

from environment import wait

pytestmark = pytest.mark.chain


@pytest.mark.description(
    "A second tail for the same device profile fails deterministically with conflict, and send still works while the first watch remains attached."
)
def test_a_second_tail_is_a_deterministic_conflict_while_send_works(
    scenario, paired_device, session_id
):
    paired_device.send(session_id, "watch concurrency fixture")
    paired_device.sync(session_id)
    first = paired_device.start_watch(
        "tail", "--session", session_id, "--timeout", "60"
    )
    try:
        first.wait(
            lambda value: value.get("data", {}).get("event", {}).get("type")
            == "state",
            timeout=25,
        )
        code, values = paired_device.invoke(
            "tail",
            "--session",
            session_id,
            "--once",
            "--timeout",
            "3",
            success=False,
        )
        assert code == 8, f"expected a deterministic conflict, got {code}: {values[-1:]}"
        assert values[-1]["error"]["code"] == "conflict"
        assert "another watch is active" in values[-1]["error"]["message"]

        # The competing tail must not have replaced the attached channel.
        paired_device.send(session_id, "send after conflict fixture")
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

    # Releasing the first watch releases the profile lease for the next tail.
    second = paired_device.start_watch("tail", "--session", session_id, "--timeout", "15")
    try:
        wait(lambda: second.process.poll() is None, timeout=5)
        second.close()
    finally:
        second.close()
