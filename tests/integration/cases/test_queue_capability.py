"""QMQ-16: profile-level pending-queue opt-out is enforced for every queue subcommand."""

import pytest

pytestmark = pytest.mark.chain


def _restart_without_queue(scenario):
    scenario.plugin.close()
    scenario.start_plugin(extra_env={"WHALEREIN_PENDING_QUEUE_ENABLED": "false"})


def _queue_session(scenario):
    session = "queue-disabled-fixture"
    scenario.dsh.process.control("create_queue_session", session_id=session)
    return session


@pytest.mark.parametrize("subcommand", [
    pytest.param(value, marks=pytest.mark.description(
        f"With pending_queue_enabled=false, queue {value} exits 9 unsupported_capability."
    ))
    for value in ("list", "edit", "remove", "steer")
])
def test_pending_queue_disabled_rejects_every_queue_subcommand(scenario, subcommand):
    _restart_without_queue(scenario)
    device = scenario.pair()
    session = _queue_session(scenario)
    device.sessions(scenario.host)
    args = {
        "list": ["queue", "list", "--session", session],
        "edit": ["queue", "edit", "--session", session, "--item", "queue-fixture", "replacement"],
        "remove": ["queue", "remove", "--session", session, "--item", "queue-fixture"],
        "steer": ["queue", "steer", "--session", session, "--item", "queue-fixture"],
    }[subcommand]
    code, values = device.invoke(*args, success=False)
    assert code == 9
    assert values[-1]["error"]["code"] == "unsupported_capability"
    session_data = next(item for item in device.sessions(scenario.host) if item["session_id"] == session)
    capabilities = session_data["source_capabilities"]
    assert capabilities["pending_queue_read"] is False
    assert capabilities["pending_queue_edit"] is False
    assert capabilities["pending_queue_remove"] is False
    assert capabilities["pending_queue_steer"] is False
    assert scenario.dsh.process.control("gx_effect_counts")["updatePendingQueue"] == 0
    assert scenario.dsh.process.control("queue_read_count") == 0
