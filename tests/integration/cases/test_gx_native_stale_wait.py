"""GX-04 stale observation after an encrypted channel replacement."""

import json
import os
import subprocess
import time
from pathlib import Path

import pytest
from environment import ROOT, wait

pytestmark = pytest.mark.__getattr__("native-dsh")


def test_gx04_native_stale_wait_does_not_complete_new_channel(scenario, paired_device):
    """A deliberately late old-channel idle cannot end a new-channel wait while native work runs."""
    def stage(value, **fields):
        Path("/reports/gx04-stale-wait.json").write_text(json.dumps({"stage":value, **fields}))
    stage("enable")
    assert scenario.request("test-enable-gx")["enabled"]
    session = scenario.create()
    paired_device.sessions(scenario.host)
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    assert scenario.request("gx-arm-stale-wait")["armed"]
    process = subprocess.Popen(paired_device.args("session", 'wait', session, "--timeout", "12"), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    started = time.monotonic()
    try:
        stage("await-entry")
        wait(lambda: scenario.request("gx-stale-wait-status")["entered"], timeout=10)
        stage("replace-channel")
        assert any(row["session_id"] == session for row in paired_device.sessions(scenario.host))
        wait(lambda: scenario.request("gx-stale-wait-status")["aborted"], timeout=10)
        assert scenario.request("gx-release-stale-wait")["released"]
        wait(lambda: scenario.request("gx-stale-wait-status")["lateReturned"], timeout=5)
        time.sleep(0.2)
        alive = process.poll() is None
        native_running = scenario.request("state",sessionId=session)["running"]
        stage("late-returned", process_alive=alive, native_running=native_running, elapsed_seconds=round(time.monotonic()-started,3))
        if not alive:
            early_output, _ = process.communicate(timeout=5)
            try:
                early = json.loads(early_output)
                stage("early-exit", exit=process.returncode, error_code=early.get("error",{}).get("code"), reason=early.get("data",{}).get("reason"), elapsed_seconds=round(time.monotonic()-started,3))
            except ValueError:
                stage("early-exit", exit=process.returncode, error_code="invalid-json", elapsed_seconds=round(time.monotonic()-started,3))
        assert alive and native_running
        scenario.model.release.set()
        output, _ = process.communicate(timeout=15)
        result = json.loads(output)
        elapsed = time.monotonic() - started
        stage("complete", exit=process.returncode, reason=result.get("data",{}).get("reason"), elapsed_seconds=round(elapsed,3))
        assert process.returncode == 0 and result["data"]["reason"] == "idle"
        assert elapsed < 15 and scenario.model.calls == 1
        stage("done")
    finally:
        scenario.model.release.set()
        try: scenario.request("gx-reset-stale-wait")
        except RuntimeError: pass
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)
