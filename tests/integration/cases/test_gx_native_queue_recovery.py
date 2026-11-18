"""GX-04 native queue control recovery across isolated result faults."""

import json
import sqlite3
from pathlib import Path

import pytest

pytestmark = pytest.mark.__getattr__("native-dsh")


@pytest.mark.parametrize("action,fault", [
    pytest.param(action, fault, marks=pytest.mark.description(
        f"Native queue {action} under {fault} recovers without a second source mutation."
    ))
    for action in ("edit", "remove", "steer") for fault in ("journal", "notification")
])
def test_gx04_native_queue_actions_recover_without_replay(action, fault, scenario, paired_device):
    """An accepted native queue mutation is never reissued after Host result recovery."""
    def stage(value, **fields):
        Path(f"/reports/gx04-queue-{action}-{fault}.json").write_text(json.dumps({"stage":value, **fields}))
    assert scenario.request("test-enable-gx")["enabled"]
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    try:
        scenario.request("queue-add", sessionId=session)
        assert scenario.request("queue-read", sessionId=session)["available"]
        paired_device.sessions(scenario.host)
        queue = paired_device.call("session", "queue", "list", "--session", session)
        item = queue["items"][0]["item_id"]
        assert scenario.request("gx-arm-queue-counter")["armed"]
        before = scenario.request("gx-queue-count")["count"]
        if fault == "journal":
            assert scenario.request("arm-fork-journal-fault")["armed"]
        else:
            assert scenario.request("gx-drop-next-command-success")["armed"]
        stage("mutate")
        args = ["session", "queue", action, "--session", session, "--item", item] + (["replacement fixture"] if action == "edit" else [])
        code, envelopes = paired_device.invoke(*args, success=False)
        stage("mutated", exit=code, error_code=envelopes[-1].get("error",{}).get("code"))
        assert scenario.request("gx-queue-count")["count"] == before + 1
        if fault == "journal":
            assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
            pending = paired_device.call("operation", "list")["pending_commands"]
            assert len(pending) == 1
            request, receipt = pending[0]["request_id"], None
        else:
            assert code == 0 and scenario.request("gx-drop-status")["count"] == 1
            receipt = envelopes[-1]["data"]["command"]
            request = receipt["request_id"]
            with sqlite3.connect(paired_device.data_dir / "cache.sqlite3") as db:
                assert db.execute("UPDATE commands SET status='unknown' WHERE request_id=?", (request,)).rowcount == 1
        stage("restart")
        scenario.native.close()
        scenario.start_native()
        assert scenario.request("gx-arm-queue-counter")["armed"]
        assert scenario.request("gx-queue-count")["count"] == before + 1
        code, envelopes = paired_device.invoke("operation", "recover", request, success=False)
        stage("recovered", exit=code, error_code=envelopes[-1].get("error",{}).get("code"))
        if fault == "journal":
            assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
        else:
            assert code == 0 and envelopes[-1]["data"]["command_id"] == receipt["command_id"]
        assert scenario.request("gx-queue-count")["count"] == before + 1
        stage("done")
    finally:
        scenario.model.release.set()
