"""CLI multi-process, signal and revocation contract acceptance."""

import signal
import subprocess

import pytest
from environment import wait

pytestmark = pytest.mark.chain


def _event(value):
    return value.get("data", {}).get("event", {})


def _texts(event):
    return [block.get("text", "") for block in event.get("blocks", [])]


def _attached(watcher):
    return watcher.wait(lambda value: _event(value).get("type") == "state", timeout=20)


def _assert_no_panic(watcher):
    stderr = "".join(watcher.stderr)
    assert "panicked" not in stderr, stderr[-500:]
    assert "failed printing to stdout" not in stderr, stderr[-500:]


@pytest.mark.description(
    "SIGTERM on an attached session watch returns exit 2 with a cancelled envelope and no panic."
)
def test_sigterm_on_session_watch_is_a_cancelled_envelope(paired_client, session_id):
    watcher = paired_client.watch("session", "watch", session_id, "--timeout", "30")
    try:
        _attached(watcher)
        watcher.process.send_signal(signal.SIGTERM)
        assert watcher.wait_exit(timeout=10) == 2
        envelope = watcher.values[-1]
        assert envelope["schema"] == "whalerein/client/v1"
        assert envelope["command"] == "session.watch"
        assert envelope["error"]["code"] == "cancelled"
        assert envelope["error"]["action"] == "retry"
        _assert_no_panic(watcher)
    finally:
        watcher.close()


@pytest.mark.description(
    "A second session watch conflicts while a third process sends successfully and the attached watch still delivers committed text."
)
def test_second_session_watch_conflicts_while_send_succeeds(paired_client, session_id):
    first = paired_client.watch("session", "watch", session_id, "--timeout", "30")
    try:
        _attached(first)
        second = paired_client.watch("session", "watch", session_id, "--timeout", "10")
        try:
            assert second.wait_exit(timeout=15) == 8
            assert second.values[-1]["error"]["code"] == "conflict"
            assert "another watch is active" in second.values[-1]["error"]["message"]
        finally:
            second.close()

        sent = paired_client.call(
            "session", "send", session_id, "concurrent send fixture", "--detach"
        )
        assert sent["state"] == "submission"
        assert sent["operation_id"]
        assert sent["target"] == {
            "host_id": paired_client.identity["host_id"],
            "session_id": session_id,
        }
        first.wait(
            lambda value: any(
                "concurrent send fixture" in text for text in _texts(_event(value))
            ),
            timeout=30,
        )
        first.process.send_signal(signal.SIGTERM)
        assert first.wait_exit(timeout=10) == 2
        assert first.values[-1]["error"]["code"] == "cancelled"
        _assert_no_panic(first)

        recovered = paired_client.call("operation", "recover", sent["operation_id"])
        assert recovered["operation_id"] == sent["operation_id"]
        assert recovered["target"] == sent["target"]
        assert recovered["receipt"]["status"] == "succeeded"
        assert recovered["receipt"]["message_id"]
    finally:
        first.close()


def test_watch_streams_while_same_device_send_follows_reply(scenario, paired_client, session_id):
    """An attached watch receives two live deltas before an ordinary send can finish."""
    fixture = scenario.dsh.process
    fixture.control("suppress_message_echo", session_id=session_id)
    watcher = paired_client.watch("session", "watch", session_id, "--timeout", "40")
    try:
        _attached(watcher)
        resets_before_send = sum(_event(value).get("type") == "reset" for value in watcher.values)
        sender = paired_client.watch("session", "send", session_id, "fixture", "--timeout", "30")
        try:
            def admitted_message():
                for event in scenario.dsh.snapshot(session_id)["events"]:
                    if event["type"] == "user/message" and event["data"].get("id"):
                        return event["data"]["id"]
                return None

            wait(admitted_message, timeout=10)
            message_id = admitted_message()
            assert message_id
            fixture.control("preview_start", session_id=session_id)
            for index, text in enumerate(("x", "y")):
                fixture.control("preview_chunk", session_id=session_id, index=index, text=text)
                watcher.wait(
                    lambda value: _event(value).get("type") == "preview" and _event(value).get("text") == text,
                    timeout=8,
                )
                assert sender.process.poll() is None

            assert sum(_event(value).get("type") == "reset" for value in watcher.values) == resets_before_send
            fixture.control("preview_final", session_id=session_id, message_id=message_id, count=2)
            assert sender.wait_exit(timeout=10) == 0
            assert sender.values[-1]["data"]["reply"]["state"] == "committed"
        finally:
            sender.close()
    finally:
        watcher.close()


@pytest.mark.description(
    "Revoking the Device during a session watch returns exit 4 and removes the persisted regional credential hash."
)
def test_revoke_during_session_watch_ends_with_authorization_error(
    scenario, paired_client, session_id
):
    watcher = paired_client.watch("session", "watch", session_id, "--timeout", "20")
    try:
        _attached(watcher)
        assert paired_client.call("device", "revoke", "--yes")["revoked"]
        assert watcher.wait_exit(timeout=30) == 4
        envelope = watcher.values[-1]
        assert envelope["schema"] == "whalerein/client/v1"
        assert envelope["command"] == "session.watch"
        assert envelope["error"]["code"] in {"revoked", "unauthorized", "not_paired"}
        assert envelope["error"]["action"] == "reauth"
        _assert_no_panic(watcher)

        # Query only the credential count from this case's database. The
        # transport credential is never extracted from the platform keyring.
        result = subprocess.run(
            [
                "psql", "-h", "127.0.0.1", "-p", "55432", "-U", "postgres",
                "-d", "whalerein_test", "-tAc",
                f"SET search_path TO {scenario.schema}; SELECT count(*) FROM device_credentials",
            ],
            check=True, capture_output=True, text=True, timeout=10,
        )
        assert result.stdout.strip().splitlines()[-1] == "0"
    finally:
        watcher.close()
