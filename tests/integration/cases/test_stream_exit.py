"""Tail/wait exit-code matrix and not-found behavior for a single Host."""

import uuid

import pytest


from environment import wait
from journey_helpers import host_online

pytestmark = pytest.mark.chain


@pytest.mark.description(
    "A bounded tail that produces no output times out with exit 7, and --timeout 0 is rejected locally as invalid_argument before any watch starts."
)
def test_tail_timeout_matrix_reports_timeout_and_rejects_zero(
    scenario, paired_device, session_id
):
    paired_device.send(session_id, "timeout matrix fixture")
    paired_device.sync(session_id)
    code, values = paired_device.invoke(
        "tail", "--session", session_id, "--once", "--timeout", "2", success=False
    )
    assert code == 7
    assert values[-1]["error"]["code"] == "timeout"

    code, values = paired_device.invoke(
        "tail", "--session", session_id, "--once", "--timeout", "0", success=False
    )
    assert code == 2
    assert values[-1]["error"]["code"] == "invalid_argument"
    code, values = paired_device.invoke(
        "wait", "--session", session_id, "--timeout", "0", success=False
    )
    assert code == 2
    assert values[-1]["error"]["code"] == "invalid_argument"


@pytest.mark.description(
    "A tail on an offline single Host reports host_offline exit 5 instead of a silent deadline."
)
def test_tail_offline_reports_host_offline(scenario, paired_device, session_id):
    paired_device.send(session_id, "offline tail fixture")
    paired_device.sync(session_id)
    scenario.plugin.close()
    wait(lambda: host_online(paired_device, scenario.host) is False, timeout=45)
    code, values = paired_device.invoke(
        "tail", "--session", session_id, "--once", "--timeout", "3", success=False
    )
    assert code == 5, f"offline tail must report the transport state, got {values[-1:]}"
    assert values[-1]["error"]["code"] == "host_offline"


@pytest.mark.description(
    "For a single authorized Host, tail and wait on a nonexistent session report not_found exit 3 rather than a transport timeout."
)
def test_nonexistent_session_tail_and_wait_are_not_found(scenario, paired_device):
    missing = "session-" + uuid.uuid4().hex
    code, values = paired_device.invoke(
        "tail", "--session", missing, "--once", "--timeout", "3", success=False
    )
    assert code == 3, f"tail on a missing session must be not_found, got {values[-1:]}"
    assert values[-1]["error"]["code"] == "not_found"
    code, values = paired_device.invoke(
        "wait", "--session", missing, "--timeout", "3", success=False
    )
    assert code == 3, f"wait on a missing session must be not_found, got {values[-1:]}"
    assert values[-1]["error"]["code"] == "not_found"


@pytest.mark.description(
    "Ctrl-C during a wait cancels the Host observation, then the next wait completes instead of blocking on the stale claim."
)
def test_ctrl_c_during_wait_cleans_up_before_the_next_wait(
    scenario, paired_device, session_id
):
    import signal

    scenario.dsh.start_running(session_id)
    watcher = paired_device.start_watch(
        "wait", "--session", session_id, "--timeout", "30"
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
    observation = paired_device.call("wait", "--session", session_id, "--timeout", "10")
    assert observation["reason"] == "idle", observation
