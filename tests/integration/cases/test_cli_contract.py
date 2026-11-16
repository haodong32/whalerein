"""CLI multi-process, signal and revocation contract acceptance.

These cases pin the contract the CLI promises to scripts. They assert specific
exit codes, envelopes and delivery outcomes instead of an "any documented code"
range, and they never accept a Rust panic.
"""

import signal
import time

import pytest

pytestmark = pytest.mark.chain


def _event(value):
    return value.get("data", {}).get("event", {})


def _texts(event):
    return [block.get("text", "") for block in event.get("blocks", [])]


def _assert_no_panic(watcher):
    stderr = "".join(watcher.stderr)
    assert "panicked" not in stderr, stderr[-500:]
    assert "failed printing to stdout" not in stderr, stderr[-500:]


@pytest.mark.description(
    "When SIGTERM reaches a running tail before any command submission, the CLI exits 2 with a cancelled envelope and no panic."
)
def test_sigterm_on_tail_is_a_cancelled_envelope(scenario, paired_device, session_id):
    """A pre-submission signal is a local cancellation, not an undefined death."""
    watcher = paired_device.start_watch(
        "tail", "--session", session_id, "--timeout", "30"
    )
    try:
        time.sleep(0.4)
        watcher.process.send_signal(signal.SIGTERM)
        code = watcher.wait_exit(timeout=10)
        assert code == 2, f"SIGTERM on tail exited {code}"
        assert watcher.values, "no envelope after SIGTERM"
        envelope = watcher.values[-1]
        assert envelope["schema"] == "whalerein/v1"
        assert envelope["command"] == "tail"
        assert envelope["error"]["code"] == "cancelled"
        assert envelope["error"]["action"] == "retry"
        _assert_no_panic(watcher)
    finally:
        watcher.close()


@pytest.mark.description(
    "A second tail for the same device profile is a deterministic conflict while a third process send succeeds and the attached tail still delivers the committed text."
)
def test_a_second_tail_is_a_conflict_while_a_third_process_send_succeeds(
    scenario, paired_device, session_id
):
    """One device profile owns one watch; unrelated commands keep working."""
    first = paired_device.start_watch("tail", "--session", session_id, "--timeout", "30")
    try:
        first.wait(lambda value: _event(value).get("type") == "reset", timeout=20)
        # The second viewer cannot silently replace the shared device channel.
        second = paired_device.start_watch(
            "tail", "--session", session_id, "--timeout", "10"
        )
        try:
            code = second.wait_exit(timeout=15)
            assert code == 8, f"the second tail exited {code}, expected conflict"
            assert second.values[-1]["error"]["code"] == "conflict"
            assert "another watch is active" in second.values[-1]["error"]["message"]
        finally:
            second.close()

        # A third process send is unaffected and the attached tail sees it.
        sent = paired_device.send(session_id, "channel replacement fixture")
        assert sent["status"] == "succeeded"
        assert sent["message_id"]
        first.wait(
            lambda value: any(
                "channel replacement fixture" in text for text in _texts(_event(value))
            ),
            timeout=30,
        )

        # Closing the viewer is the documented cancellation, not an undefined kill.
        first.process.send_signal(signal.SIGTERM)
        assert first.wait_exit(timeout=10) == 2
        assert first.values[-1]["error"]["code"] == "cancelled"
        _assert_no_panic(first)
    finally:
        first.close()


@pytest.mark.description(
    "When the device is revoked during a tail, the tail exits 4 with an authorization error envelope and the revoked token is denied."
)
def test_revoke_during_tail_ends_with_an_authorization_error(
    scenario, paired_device, session_id
):
    """Revocation is an authorization failure for the running viewer, never success."""
    watcher = paired_device.start_watch(
        "tail", "--session", session_id, "--timeout", "20"
    )
    try:
        watcher.wait(lambda value: _event(value).get("type") == "reset", timeout=20)
        paired_device.revoke()
        # The watch refresh (5 s) observes the denied token well inside 20 s.
        code = watcher.wait_exit(timeout=30)
        assert code == 4, f"revoked tail exited {code}, expected 4"
        envelope = watcher.values[-1]
        assert envelope["schema"] == "whalerein/v1"
        assert envelope["command"] == "tail"
        assert envelope["error"]["code"] in {"revoked", "unauthorized", "not_paired"}, envelope["error"]
        assert envelope["error"]["action"] == "reauth"
        assert "panicked" not in "".join(watcher.stderr)
        _assert_no_panic(watcher)
        assert paired_device.revoked_access_denied()
    finally:
        watcher.close()
