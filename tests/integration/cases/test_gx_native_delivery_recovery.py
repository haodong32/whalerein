"""GX-04 native delivery mode recovery across isolated result faults."""

import json
import sqlite3
from pathlib import Path

import pytest

pytestmark = pytest.mark.__getattr__("native-dsh")


@pytest.mark.parametrize("delivery,fault", [
    pytest.param(delivery, fault, marks=pytest.mark.description(
        f"Native {delivery} delivery under {fault} preserves its outcome without another mutation."
    ))
    for delivery in ("enqueue-after-run", "start-if-idle", "steer")
    for fault in ("journal", "notification")
])
def test_gx04_native_delivery_modes_recover_without_replay(delivery, fault, scenario, paired_device):
    """A native delivery survives a controlled result fault without a second user event."""
    def stage(value, **fields):
        Path(f"/reports/gx04-delivery-{delivery}-{fault}.json").write_text(json.dumps({"stage":value, **fields}))
    stage("enable")
    assert scenario.request("test-enable-gx")["enabled"]
    session = scenario.create()
    paired_device.sessions(scenario.host)
    baseline = scenario.request("gx-user-delivery-count", sessionId=session)["count"]
    scenario.model.plan()
    if fault == "journal":
        assert scenario.request("arm-fork-journal-fault")["armed"]
    else:
        assert scenario.request("gx-drop-next-command-success")["armed"]
    stage("send")
    code, envelopes = paired_device.invoke("send", "--session", session, "--delivery", delivery, "synthetic delivery", success=False)
    stage("sent", exit=code, error_code=envelopes[-1].get("error",{}).get("code"))
    if fault == "journal":
        assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
        pending = paired_device.call("commands-pending")["commands"]
        stage("pending", count=len(pending))
        assert len(pending) == 1
        request = pending[0]["request_id"]
        receipt = None
    else:
        assert code == 0
        receipt = envelopes[-1]["data"]
        drop_count = scenario.request("gx-drop-status")["count"]
        stage("notification-receipt", has_delivery=bool(receipt.get("delivery_id")), drops=drop_count)
        assert receipt["delivery_id"] and drop_count == 1
        request = receipt["request_id"]
        with sqlite3.connect(paired_device.directory / "client/cache.sqlite3") as db:
            assert db.execute("UPDATE commands SET status='unknown' WHERE request_id=?", (request,)).rowcount == 1
    stage("wait-idle")
    scenario.wait_idle(session)
    before = scenario.request("gx-user-delivery-count", sessionId=session)["count"]
    stage("pre-restart-count", baseline_events=baseline, user_events=before, model_calls=scenario.model.calls)
    assert before == baseline + 1 and scenario.model.calls == 1
    stage("restart")
    scenario.native.close()
    scenario.start_native()
    code, envelopes = paired_device.invoke("command-recover", "--request", request, success=False)
    stage("recovered", exit=code, error_code=envelopes[-1].get("error",{}).get("code"))
    if fault == "journal":
        assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
    else:
        assert code == 0 and envelopes[-1]["data"]["delivery_id"] == receipt["delivery_id"]
    assert scenario.request("gx-user-delivery-count", sessionId=session)["count"] == before
    assert scenario.model.calls == 1
    stage("done")
