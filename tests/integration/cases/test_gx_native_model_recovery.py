"""GX-04 native model selection recovery across result faults."""

import json
import sqlite3
from pathlib import Path

import pytest
from environment import wait

pytestmark = pytest.mark.__getattr__("native-dsh")


@pytest.mark.parametrize("fault", [
    pytest.param("journal", marks=pytest.mark.description("A native model selection with failed journal remains unknown without another selection.")),
    pytest.param("notification", marks=pytest.mark.description("A native model selection with lost notification recovers the original receipt without another selection.")),
])
def test_gx04_native_model_recovery_without_replay(fault, scenario, paired_device):
    """A native model selection preserves its uncertain or confirmed result across restart without replay."""
    def stage(value, **fields):
        Path(f"/reports/gx04-model-{fault}.json").write_text(json.dumps({"stage":value, **fields}))
    stage("enable")
    assert scenario.request("test-enable-gx")["enabled"]
    session = scenario.create()
    paired_device.sessions(scenario.host)
    catalog = paired_device.call("session", "models", "--host", scenario.host)
    group = next(group for group in catalog["groups"] if group["models"])
    assert len(group["models"]) >= 2
    model = group["models"][0]
    assert scenario.request("gx-arm-model-counter")["armed"]
    before = scenario.request("gx-model-count")["count"]
    if fault == "journal":
        assert scenario.request("arm-fork-journal-fault")["armed"]
    else:
        assert scenario.request("gx-drop-next-command-success")["armed"]
    stage("select")
    code, envelopes = paired_device.invoke("session", "model", "set", "--session", session, "--provider", group["id"], "--model", model["id"], success=False)
    stage("selected", exit=code, error_code=envelopes[-1].get("error",{}).get("code"))
    assert scenario.request("gx-model-count")["count"] == before + 1
    if fault == "journal":
        assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
        pending = paired_device.call("operation", "list")["pending_commands"]
        assert len(pending) == 1
        request = pending[0]["request_id"]
        receipt = None
    else:
        assert code == 0 and scenario.request("gx-drop-status")["count"] == 1
        receipt = envelopes[-1]["data"]
        assert receipt["model_selection"]["default_persistence"] == "unconfirmed"
        request = receipt["request_id"]
        with sqlite3.connect(paired_device.data_dir / "cache.sqlite3") as db:
            assert db.execute("UPDATE commands SET status='unknown' WHERE request_id=?", (request,)).rowcount == 1
    stage("restart")
    scenario.native.close()
    scenario.start_native()
    assert scenario.request("gx-arm-model-counter")["armed"]
    assert scenario.request("gx-model-count")["count"] == before + 1
    if fault == "notification":
        later = {"provider": group["id"], "model": group["models"][1]["id"]}
        scenario.request("model-select-local", sessionId=session, selection=later)
        assert scenario.request("model-read", sessionId=session)["pending"]["model"] == later["model"]
    code, envelopes = paired_device.invoke("operation", "recover", request, success=False)
    stage("recovered", exit=code, error_code=envelopes[-1].get("error",{}).get("code"))
    if fault == "journal":
        assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
    else:
        assert code == 0 and envelopes[-1]["data"]["model_selection"] == receipt["model_selection"]
        assert scenario.request("model-read", sessionId=session)["pending"]["model"] == later["model"]
    assert scenario.request("gx-model-count")["count"] == before + 1
    stage("done")


def test_native_model_failed_default_save_is_not_retried_after_restart(scenario, paired_device):
    """A failed native default save is attempted once despite command replay and Host restart."""
    def stage(name): Path('/reports/model-save-failure-stage.json').write_text(json.dumps({'stage': name}))
    stage('enable')
    assert scenario.request("test-enable-gx")["enabled"]
    session = scenario.create()
    paired_device.sessions(scenario.host)
    catalog = paired_device.call("session", "models", "--host", scenario.host)
    stage('catalog')
    group = next(group for group in catalog["groups"] if len(group["models"]) >= 2)
    default = scenario.request("model-catalog")["default_selection"]
    alternative = next(model for model in group["models"] if model["id"] != default["model"] or group["id"] != default["provider"])
    assert scenario.request("test-model-default-failure-arm")["armed"]
    assert scenario.request("gx-arm-model-counter")["armed"]
    stage('set')
    before = scenario.request("gx-model-count")["count"]
    receipt = paired_device.call("session", "model", "set", "--session", session, "--provider", group["id"], "--model", alternative["id"])
    stage('receipt')
    assert receipt["status"] == "succeeded" and receipt["model_selection"]["default_persistence"] == "unconfirmed"
    wait(lambda: scenario.request("test-model-default-failure-count")["count"] == 1, process=scenario.native)
    stage('default')
    assert scenario.request("model-read", sessionId=session)["default_selection"] == default
    assert scenario.request("gx-model-count")["count"] == before + 1
    scenario.native.close()
    stage('restart')
    scenario.start_native()
    assert scenario.request("test-model-default-failure-arm")["armed"]
    assert scenario.request("gx-arm-model-counter")["armed"]
    status, replay = scenario.replay_command(paired_device)
    stage('replay')
    assert status in (200, 202) and replay["commandId"] == receipt["command_id"]
    assert scenario.request("gx-model-count")["count"] == before + 1
    assert scenario.request("test-model-default-failure-count")["count"] == 1
    assert scenario.request("model-read", sessionId=session)["default_selection"] == default
