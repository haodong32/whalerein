"""ACC-01: an unavailable presence view must not terminally fail a send."""

import socket
import subprocess

import pytest

from environment import wait

pytestmark = pytest.mark.chain


def _request_statuses(schema):
    result = subprocess.run(
        [
            "psql",
            "-h",
            "127.0.0.1",
            "-p",
            "55432",
            "-U",
            "postgres",
            "-d",
            "whalerein_test",
            "-tAc",
            f"SET search_path TO {schema}; SELECT status FROM requests",
        ],
        check=True,
        capture_output=True,
        text=True,
        timeout=5,
    )
    return [line.strip() for line in result.stdout.splitlines() if line.strip()]


def _listening(port=18080):
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=0.5):
            return True
    except OSError:
        return False


@pytest.mark.description(
    "A send submitted while Access presence is unavailable is queued and delivered after recovery instead of becoming host_offline."
)
def test_send_during_unavailable_presence_is_queued_and_delivered(
    scenario, paired_device
):
    """Access must distinguish 'presence not reconciled' from 'host offline'."""
    session = scenario.dsh.create_session()
    paired_device.sessions(scenario.host)
    before = scenario.dsh.process.control("gx_effect_counts")["sendUserMessage"]

    scenario.access.close()
    original = scenario.config.read_text()
    unreachable = original.replace(
        'url: "mqtt://127.0.0.1:1883"', 'url: "mqtt://127.0.0.1:19999"', 1
    )
    assert unreachable != original
    scenario.config.write_text(unreachable)
    try:
        scenario.start_access_process()
        wait(_listening, timeout=20, process=scenario.access)
        code, values = paired_device.invoke(
            'session', 'send', session, "presence-window message", "--detach", success=False
        )
        # The command cannot be confirmed while the broker is unreachable; it
        # must be reported as retryable (channel unavailable) or unknown, never
        # as a terminal host_offline that silently lost the work.
        assert code in (5, 7), values
        error = values[-1]["error"]
        assert error["code"] != "host_offline", error
        # A queue-window attempt is a retryable or recoverable failure
        # (unavailable/unknown/expired), never a terminal host_offline. An
        # uncertain outcome requires `operation recover` (action refresh); a
        # proven-unavailable outcome is a plain retry.
        if error.get("code") == "unknown":
            assert error.get("action") == "refresh", error
        else:
            assert error.get("action") == "retry", error
        statuses = _request_statuses(scenario.schema)
        assert statuses, "no request was recorded during the presence window"
        assert "host_offline" not in statuses, statuses
        assert "received" in statuses, statuses
    finally:
        scenario.access.close()
        scenario.config.write_text(original)
        scenario.start_access()

    # A retry after recovery must be accepted exactly once; the earlier
    # unconfirmed attempt must not produce a duplicate user message.
    paired_device.send(session, "presence-window message")
    wait(
        lambda: scenario.dsh.process.control("gx_effect_counts")["sendUserMessage"]
        == before + 1,
        timeout=20,
    )
    snapshot = scenario.dsh.snapshot(session)
    assert any(
        "presence-window message" in str(event.get("data", {}))
        for event in snapshot["events"]
    )
