"""Watch/wait exit-code matrix and not-found behavior for a single Host."""

import signal
import uuid

import pytest

from environment import wait

pytestmark = pytest.mark.chain


def _finish_watch(client, *args):
    stream = client.watch("session", "watch", *args)
    try:
        code = stream.wait_exit(timeout=15)
        assert stream.values, "watch produced no terminal envelope"
        return code, stream.values[-1]
    finally:
        stream.close()


@pytest.mark.description(
    "A bounded watch that produces no output times out with exit 7, and --timeout 0 is rejected locally as invalid_argument before any watch starts."
)
def test_watch_timeout_matrix_reports_timeout_and_rejects_zero(
    paired_client, session_id
):
    paired_client.call("session", "send", session_id, "timeout matrix fixture", "--detach")
    paired_client.call("session", "show", session_id)
    code, value = _finish_watch(
        paired_client, session_id, "--once", "--timeout", "2"
    )
    assert code == 7
    assert value["error"]["code"] == "timeout"

    code, value = _finish_watch(
        paired_client, session_id, "--once", "--timeout", "0"
    )
    assert code == 2
    assert value["error"]["code"] == "invalid_argument"
    code, value = paired_client.invoke(
        "session", "wait", session_id, "--timeout", "0"
    )
    assert code == 2
    assert value["error"]["code"] == "invalid_argument"


@pytest.mark.description(
    "A watch on an offline single Host reports host_offline exit 5 instead of a silent deadline."
)
def test_watch_offline_reports_host_offline(scenario, paired_client, session_id):
    paired_client.call("session", "show", session_id)
    scenario.plugin.close()

    def host_offline():
        code, value = paired_client.invoke("host", "list")
        return code == 0 and any(
            row["host_id"] == scenario.host and not row["online"]
            for row in value["data"]["hosts"]
        )

    wait(host_offline, timeout=45)
    code, value = _finish_watch(
        paired_client, session_id, "--once", "--timeout", "3"
    )
    assert code == 5, f"offline watch must report the transport state, got {value}"
    assert value["error"]["code"] == "host_offline"


@pytest.mark.description(
    "For a single authorized Host, watch and wait on a nonexistent session report not_found exit 3 rather than a transport timeout."
)
def test_nonexistent_session_watch_and_wait_are_not_found(paired_client):
    missing = "session-" + uuid.uuid4().hex
    code, value = _finish_watch(
        paired_client, missing, "--once", "--timeout", "3"
    )
    assert code == 3, f"watch on a missing session must be not_found, got {value}"
    assert value["error"]["code"] == "not_found"
    code, value = paired_client.invoke(
        "session", "wait", missing, "--timeout", "3"
    )
    assert code == 3, f"wait on a missing session must be not_found, got {value}"
    assert value["error"]["code"] == "not_found"


@pytest.mark.description(
    "Ctrl-C during a wait cancels the Host observation, then the next wait completes instead of blocking on the stale claim."
)
def test_ctrl_c_during_wait_cleans_up_before_the_next_wait(
    scenario, paired_client, session_id
):
    scenario.dsh.start_running(session_id)
    watcher = paired_client.watch(
        "session", "wait", session_id, "--timeout", "30"
    )
    try:
        # Wait until the Host is actually observing this wait before signalling:
        # a SIGINT delivered before the runtime installs its handler is the
        # default kill, not the cancelled envelope this case pins.
        wait(lambda: watcher.process.poll() is None, timeout=10)
        wait(
            lambda: (
                scenario.directory / "plugin" / "test-wait-observations.json"
            ).exists(),
            timeout=15,
        )
        watcher.process.send_signal(signal.SIGINT)
        code = watcher.wait_exit(timeout=15)
        assert code == 2, f"Ctrl-C on wait exited {code}"
        assert watcher.values[-1]["error"]["code"] == "cancelled"
    finally:
        watcher.close()
    scenario.dsh.process.control("idle", session_id=session_id)
    observation = paired_client.call("session", "wait", session_id, "--timeout", "10")
    assert observation["reason"] == "idle", observation
