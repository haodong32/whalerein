"""GX-04 native delivery mode recovery across isolated result faults."""

import json
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
    text = "synthetic delivery"
    code, envelopes = paired_device.invoke(
        "session", "send", "--detach", session, "--delivery", delivery, text, success=False
    )
    stage("sent", exit=code, error_code=envelopes[-1].get("error",{}).get("code"))
    if fault == "journal":
        code, outcome = paired_device.settle_detached_delivery(
            code, envelopes[-1], session, text, delivery
        )
        stage("journal-settled", exit=code, error_code=outcome.get("error", {}).get("code"))
        assert code == 7 and outcome["error"]["code"] == "unknown"
        pending = paired_device.call("operation", "list")["sends"]
        stage("pending", count=len(pending))
        assert len(pending) == 1
        request = pending[0]["operation_id"]
        receipt = None
    else:
        assert code == 0
        operation = envelopes[-1]["data"]
        drop_count = scenario.request("gx-drop-status")["count"]
        stage("notification-submitted", receipt_status=operation["receipt"]["status"], drops=drop_count)
        assert drop_count == 1
        receipt = paired_device.confirm_detached_delivery(
            operation, session, text, delivery
        )
        stage("notification-receipt", has_delivery=True, drops=drop_count)
        request = operation["operation_id"]
    stage("wait-idle")
    scenario.wait_idle(session)
    before = scenario.request("gx-user-delivery-count", sessionId=session)["count"]
    stage("pre-restart-count", baseline_events=baseline, user_events=before, model_calls=scenario.model.calls)
    assert before == baseline + 1 and scenario.model.calls == 1
    stage("restart")
    scenario.native.close()
    scenario.start_native()
    code, envelopes = paired_device.invoke("operation", "recover", request, success=False)
    stage("recovered", exit=code, error_code=envelopes[-1].get("error",{}).get("code"))
    if fault == "journal":
        assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
    else:
        assert code == 0 and envelopes[-1]["data"]["receipt"]["delivery_id"] == receipt["delivery_id"]
    assert scenario.request("gx-user-delivery-count", sessionId=session)["count"] == before
    assert scenario.model.calls == 1
    stage("done")
