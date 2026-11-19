"""GX-04 native fork recovery across isolated result faults."""

import json
import sqlite3
from pathlib import Path

import pytest

pytestmark = pytest.mark.__getattr__("native-dsh")


@pytest.mark.parametrize("fault", [
    pytest.param("journal", marks=pytest.mark.description("A native fork with failed journal remains unknown without a second child.")),
    pytest.param("notification", marks=pytest.mark.description("A native fork with lost notification recovers its original child without a second fork.")),
])
def test_gx04_native_fork_recovers_without_replay(fault, scenario, paired_device):
    """A native fork is not invoked again after its terminal Host result is lost."""
    def stage(value, **fields):
        Path(f"/reports/gx04-fork-{fault}.json").write_text(json.dumps({"stage":value, **fields}))
    assert scenario.request("test-enable-gx")["enabled"]
    parent = scenario.create()
    paired_device.sessions(scenario.host)
    scenario.model.plan()
    paired_device.send_confirmed(parent, "synthetic parent", delivery="enqueue-after-run")
    scenario.wait_idle(parent)
    before_sessions = {row["session_id"] for row in paired_device.sessions(scenario.host)}
    assert scenario.request("gx-arm-fork-counter")["armed"]
    before = scenario.request("gx-fork-count")["count"]
    if fault == "journal":
        assert scenario.request("arm-fork-journal-fault")["armed"]
    else:
        assert scenario.request("gx-drop-next-command-success")["armed"]
    stage("fork")
    code, envelopes = paired_device.invoke("session", "fork", parent, success=False)
    stage("forked", exit=code, error_code=envelopes[-1].get("error",{}).get("code"))
    assert scenario.request("gx-fork-count")["count"] == before + 1
    after_sessions = {row["session_id"] for row in paired_device.sessions(scenario.host)}
    assert len(after_sessions - before_sessions) == 1
    if fault == "journal":
        assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
        pending = paired_device.call("operation", "list")["pending_commands"]
        assert len(pending) == 1
        request, receipt = pending[0]["request_id"], None
    else:
        assert code == 0 and scenario.request("gx-drop-status")["count"] == 1
        receipt = envelopes[-1]["data"]
        assert receipt["lifecycle_receipt"]["parent_session_id"] == parent
        assert receipt["lifecycle_receipt"]["session_id"] in after_sessions - before_sessions
        request = receipt["request_id"]
        with sqlite3.connect(paired_device.data_dir / "cache.sqlite3") as db:
            assert db.execute("UPDATE commands SET status='unknown' WHERE request_id=?", (request,)).rowcount == 1
    stage("restart")
    scenario.native.close()
    scenario.start_native()
    assert scenario.request("gx-arm-fork-counter")["armed"]
    assert scenario.request("gx-fork-count")["count"] == before + 1
    code, envelopes = paired_device.invoke("operation", "recover", request, success=False)
    if fault == "journal":
        assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
    else:
        assert code == 0 and envelopes[-1]["data"]["lifecycle_receipt"] == receipt["lifecycle_receipt"]
    assert scenario.request("gx-fork-count")["count"] == before + 1
    assert {row["session_id"] for row in paired_device.sessions(scenario.host)} == after_sessions
    assert scenario.model.calls == 1
    stage("done")
