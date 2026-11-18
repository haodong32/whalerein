"""GX-04 native interrupt recovery across isolated result faults."""

import json
import sqlite3
from pathlib import Path

import pytest
from environment import wait

pytestmark = pytest.mark.__getattr__("native-dsh")


@pytest.mark.parametrize("fault", [
    pytest.param("journal", marks=pytest.mark.description("A native interrupt with failed journal remains unknown without another cancel.")),
    pytest.param("notification", marks=pytest.mark.description("A native interrupt with lost notification recovers its original result without another cancel.")),
])
def test_gx04_native_interrupt_recovers_without_replay(fault, scenario, paired_device):
    """An accepted native interrupt is not repeated when its terminal Host result is lost."""
    def stage(value, **fields):
        Path(f"/reports/gx04-interrupt-{fault}.json").write_text(json.dumps({"stage":value, **fields}))
    assert scenario.request("test-enable-gx")["enabled"]
    session = scenario.create()
    paired_device.sessions(scenario.host)
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    try:
        scenario.request("queue-add", sessionId=session)
        assert scenario.request("gx-arm-interrupt-counter")["armed"]
        before = scenario.request("gx-interrupt-count")["count"]
        if fault == "journal":
            assert scenario.request("arm-fork-journal-fault")["armed"]
        else:
            assert scenario.request("gx-drop-next-command-success")["armed"]
        stage("interrupt")
        code, envelopes = paired_device.invoke("session", "interrupt", session, success=False)
        stage("initial", exit=code, status=envelopes[-1].get("data",{}).get("status"), error_code=envelopes[-1].get("error",{}).get("code"))
        if code == 0:
            initial = envelopes[-1]["data"]
            assert initial["status"] in ("host_accepted", "succeeded")
            request = initial["request_id"]
        else:
            assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
            pending = paired_device.call("operation", "list")["pending_commands"]
            assert len(pending) == 1
            request = pending[0]["request_id"]
        result = {}
        def terminal():
            status, values = paired_device.invoke("operation", "recover", request, success=False)
            if status == 0 and values[-1]["data"]["status"] == "host_accepted":
                return False
            result.update({"exit":status,"envelope":values[-1]})
            return True
        if code != 0 or initial["status"] != "succeeded":
            wait(terminal, timeout=15)
        else:
            result.update({"exit":0,"envelope":envelopes[-1]})
        stage("terminal", exit=result["exit"], error_code=result["envelope"].get("error",{}).get("code"))
        assert scenario.request("gx-interrupt-count")["count"] == before + 1
        if fault == "journal":
            assert result["exit"] == 7 and result["envelope"]["error"]["code"] == "unknown"
        else:
            assert result["exit"] == 0 and scenario.request("gx-drop-status")["count"] == 1
            with sqlite3.connect(paired_device.data_dir / "cache.sqlite3") as db:
                assert db.execute("UPDATE commands SET status='unknown' WHERE request_id=?", (request,)).rowcount == 1
        stage("restart")
        scenario.native.close()
        scenario.start_native()
        assert scenario.request("gx-arm-interrupt-counter")["armed"]
        assert scenario.request("gx-interrupt-count")["count"] == before + 1
        code, envelopes = paired_device.invoke("operation", "recover", request, success=False)
        if fault == "journal":
            assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
        else:
            assert code == 0 and envelopes[-1]["data"]["status"] == "succeeded"
        assert scenario.request("gx-interrupt-count")["count"] == before + 1
        stage("done")
    finally:
        scenario.model.release.set()
