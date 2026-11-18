"""GX-05 partial rollout checks under the real isolated DSH adapter."""

import json
import os
import subprocess
from pathlib import Path

import pytest
from environment import ROOT, wait, Device, http

pytestmark = pytest.mark.__getattr__("native-dsh")


def test_gx05_native_default_open_capabilities_preserve_existing_operations(scenario, paired_device):
    """Default production advertisement exposes the optional capabilities without weakening send or the local question provider."""
    session = scenario.create()
    paired_device.sessions(scenario.host)
    created = paired_device.call('session', 'create', "--host", scenario.host)["lifecycle_receipt"]
    assert created["disposition"] == "READY"
    assert paired_device.call('session', 'models', "--host", scenario.host)["groups"]
    assert "available" in paired_device.call('session', 'queue', "list", "--session", session)
    assert "batches" in paired_device.call('session', 'question', 'list', "--session", session)
    assert paired_device.call('session', 'wait', session, "--timeout", "2")["reason"] in ("idle", "timeout", "input-required")
    code, values = paired_device.invoke('session', 'fork', session, success=False)
    assert code != 0 and values[-1]["error"]["code"] != "unsupported_capability"
    assert scenario.request("question-root-probe", sessionId=session)["answered"]
    scenario.model.plan()
    assert paired_device.send(session, "synthetic baseline", delivery="start-if-idle")["receipt"]["status"] == "succeeded"
    scenario.wait_idle(session)
    assert scenario.model.calls == 1


def test_gx05_native_disable_keeps_existing_wait_scoped(scenario, paired_device):
    """Withdrawing wait blocks new observations while an existing scoped wait finishes and cleans up."""
    def stage(value):
        Path("/reports/gx05-stage.json").write_text(json.dumps({"stage": value}))
    stage("create")
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    assert scenario.request("gx-arm-wait-capability")["enabled"]
    process = subprocess.Popen(paired_device.args("session", "wait", session, "--timeout", "15"), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        stage("await-active")
        wait(lambda: scenario.request("gx-wait-active")["active"] == 1, timeout=10)
        stage("disable")
        assert scenario.request("gx-disable-wait-capability")["disabled"]
        scenario.model.release.set()
        stage("wait-result")
        output, _ = process.communicate(timeout=20)
        result = json.loads(output)
        assert process.returncode == 0 and result["data"]["reason"] == "idle"
        stage("cleanup")
        assert scenario.request("gx-wait-active")["active"] == 0
        stage("new-wait")
        code, values = paired_device.invoke('session', 'wait', session, "--timeout", "2", success=False)
        assert code != 0 and values[-1]["error"]["code"] == "unsupported_capability"
    finally:
        scenario.model.release.set()
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_gx05_native_disable_preserves_accepted_create(scenario, paired_device):
    """Withdrawing creation rejects new requests without rewriting an already accepted native operation."""
    assert scenario.request("test-enable-gx")["enabled"]
    before = {row["session_id"] for row in paired_device.sessions(scenario.host)}
    assert scenario.request("gx-hold-create")["armed"]
    process = subprocess.Popen(paired_device.args("session", "create", "--host", scenario.host), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    released = False
    try:
        wait(lambda: scenario.request("gx-held-create-status")["entered"], timeout=10)
        assert scenario.request("gx-disable-create-capability")["disabled"]
        assert scenario.request("gx-release-create")["released"]
        released = True
        output, _ = process.communicate(timeout=20)
        result = json.loads(output)
        assert process.returncode == 0 and result["data"]["lifecycle_receipt"]["disposition"] == "READY"
        identity = result["data"]["lifecycle_receipt"]["session_id"]
        after = {row["session_id"] for row in paired_device.sessions(scenario.host)}
        assert after - before == {identity}
        code, values = paired_device.invoke('session', 'create', "--host", scenario.host, success=False)
        assert code != 0 and values[-1]["error"]["code"] == "unsupported_capability"
        assert {row["session_id"] for row in paired_device.sessions(scenario.host)} == after
    finally:
        if not released:
            try:
                scenario.request("gx-release-create")
            except RuntimeError:
                pass
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


@pytest.mark.parametrize("operation", [
    pytest.param("send", marks=pytest.mark.description("Withdrawing queue delivery preserves an already accepted native send.")),
    pytest.param("steer", marks=pytest.mark.description("Withdrawing steer delivery preserves an already accepted native steer.")),
    pytest.param("model", marks=pytest.mark.description("Withdrawing model selection preserves an already accepted native selection.")),
    pytest.param("fork", marks=pytest.mark.description("Withdrawing fork preserves an already accepted native fork.")),
    pytest.param("queue_edit", marks=pytest.mark.description("Withdrawing queue edit preserves an already accepted native edit.")),
    pytest.param("queue_remove", marks=pytest.mark.description("Withdrawing queue removal preserves an already accepted native removal.")),
    pytest.param("queue_promote", marks=pytest.mark.description("Withdrawing queue promotion preserves an already accepted native promotion.")),
])
def test_gx05_native_disable_one_inflight_mutation(operation, scenario, paired_device):
    """A single capability withdrawal does not rewrite an accepted source operation or permit a new one."""
    def stage(name, **fields):
        Path(f"/reports/gx05-live-{operation}.json").write_text(json.dumps({"stage":name, **fields}))
    stage("enable")
    assert scenario.request("test-enable-gx")["enabled"]
    session = scenario.create()
    paired_device.sessions(scenario.host)
    if operation == "fork":
        scenario.model.plan()
        paired_device.send(session, "synthetic parent", delivery="start-if-idle")
        scenario.wait_idle(session)
    elif operation.startswith("queue_"):
        scenario.model.plan(hold=True)
        scenario.request("start", sessionId=session)
        assert scenario.model.started.wait(timeout=15)
        scenario.request("queue-add", sessionId=session)
        scenario.request("queue-add", sessionId=session)
        assert scenario.request("queue-read", sessionId=session)["available"]
        paired_device.sessions(scenario.host)
        queue = paired_device.call('session', 'queue', "list", "--session", session)
        item = queue["items"][0]["item_id"]
        independent_item = queue["items"][1]["item_id"]
    elif operation in ("send", "steer"):
        scenario.model.plan()
    if operation == "model":
        catalog = paired_device.call('session', 'models', "--host", scenario.host)
        group = next(group for group in catalog["groups"] if group["models"])
        selection = ["session", "model", "set", "--session", session, "--provider", group["id"], "--model", group["models"][0]["id"]]
    args = {
        "send": ["session", "send", session, "--detach", "--delivery", "enqueue-after-run", "synthetic turn"],
        "steer": ["session", "send", session, "--detach", "--delivery", "steer", "synthetic turn"],
        "model": selection if operation == "model" else [],
        "fork": ["session", "fork", session],
    }
    if operation.startswith("queue_"):
        action = {"queue_edit":"edit", "queue_remove":"remove", "queue_promote":"steer"}[operation]
        args[operation] = ["session", "queue", action, "--session", session, "--item", item] + (["synthetic replacement"] if action == "edit" else [])
    assert scenario.request("gx-hold-native-operation", config={"operation":operation})["armed"]
    process = subprocess.Popen(paired_device.args(*args[operation]), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    released = False
    try:
        stage("await-entry")
        wait(lambda: scenario.request("gx-held-native-status")["entered"], timeout=15)
        stage("disable")
        assert scenario.request("gx-disable-one-capability", config={"operation":operation})["disabled"]
        assert scenario.request("gx-release-native-operation")["released"]
        released = True
        stage("result")
        output, _ = process.communicate(timeout=30)
        result = json.loads(output)
        stage("accepted-result", exit=process.returncode, error_code=result.get("error",{}).get("code"))
        assert process.returncode == 0
        data = result["data"]
        if operation.startswith("queue_"):
            original = data["command"]
        elif operation in ("send", "steer"):
            original = data["receipt"]
        else:
            original = data
        assert original["status"] == "succeeded"
        assert scenario.request("gx-held-native-status")["calls"] == 1
        paired_device.sessions(scenario.host)
        stage("new-request")
        code, values = paired_device.invoke(*args[operation], success=False)
        stage("new-result", exit=code, error_code=values[-1].get("error",{}).get("code"))
        assert code != 0 and values[-1]["error"]["code"] == "unsupported_capability"
        assert scenario.request("gx-held-native-status")["calls"] == 1
        # An independently enabled operation remains usable after withdrawal.
        assert paired_device.call('session', 'models', "--host", scenario.host)["groups"]
        if operation in ("send", "steer"):
            alternative = "steer" if operation == "send" else "enqueue-after-run"
            assert paired_device.send(session, "synthetic independent", delivery=alternative)["receipt"]["status"] == "succeeded"
        elif operation.startswith("queue_"):
            if operation == "queue_remove":
                independent = paired_device.call('session', 'queue', "edit", "--session", session, "--item", independent_item, "synthetic independent")
            else:
                independent = paired_device.call('session', 'queue', "remove", "--session", session, "--item", independent_item)
            assert independent["command"]["status"] == "succeeded"
        else:
            assert paired_device.send(session, "synthetic independent", delivery="start-if-idle")["receipt"]["status"] == "succeeded"
        assert any(row["session_id"] == session for row in paired_device.sessions(scenario.host))
        stage("done")
    finally:
        if not released:
            try: scenario.request("gx-release-native-operation")
            except RuntimeError: pass
        try: scenario.request("gx-reset-native-operation")
        except RuntimeError: pass
        scenario.model.release.set()
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_gx05_native_disable_open_answer_during_claim(scenario, paired_device):
    """An accepted open answer completes once after its advertised capability is withdrawn."""
    assert scenario.request("test-enable-gx")["enabled"]
    session = scenario.create()
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    assert scenario.request("question-start-remote-fixture", sessionId=session)["started"]
    batch = paired_device.call('session', 'question', 'list', "--session", session)["batches"][0]
    assert batch["phase"] == "QUESTION_OPEN" and batch["answerable"]
    answer = scenario.directory / "gx-open-answer.json"
    answer.write_text(json.dumps({"answers":[{"id":"q1", "selected":["Yes"]}]}))
    args = ["session", "question", "answer", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer), "--command-id", "gx-open-answer"]
    assert scenario.request("gx-hold-question-claim")["armed"]
    process = subprocess.Popen(paired_device.args(*args), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    released = False
    try:
        wait(lambda: scenario.request("gx-held-question-status")["entered"], timeout=15)
        assert scenario.request("gx-disable-one-capability", config={"operation":"question_open"})["disabled"]
        assert scenario.request("gx-release-question-claim")["released"]
        released = True
        output, _ = process.communicate(timeout=20)
        result = json.loads(output)
        assert process.returncode == 0 and result["data"]["question_receipt"]["phase"] == "QUESTION_SETTLED"
        assert scenario.request("question-result-remote-fixture", sessionId=session) == {"selected":"Yes", "pending":False}
        # The broker remains active here; this case exercises admission and an
        # existing claim, not complete disable/teardown of the local provider.
        assert scenario.request("question-start-remote-fixture", sessionId=session)["started"]
        next_batch = paired_device.call('session', 'question', 'list', "--session", session)["batches"][0]
        next_args = ["session", "question", "answer", session, "--batch", next_batch["batch_id"], "--epoch", next_batch["source_epoch"], "--version", str(next_batch["version"]), "--digest", next_batch["digest"], "--phase", next_batch["phase"], "--input", str(answer)]
        code, values = paired_device.invoke(*next_args, success=False)
        assert code != 0 and values[-1]["error"]["code"] == "unsupported_capability"
    finally:
        try: scenario.request("gx-reset-question-claim")
        except RuntimeError: pass
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


@pytest.mark.timed_questions
def test_gx05_native_disable_continued_answer_during_admission(scenario, paired_device):
    """An accepted continued answer remains queued once after its advertised capability is withdrawn."""
    assert scenario.request("test-enable-gx")["enabled"]
    session = scenario.create()
    paired_device.sessions(scenario.host)
    other = Device(scenario.directory / "gx-other-device")
    invitation = http("/v1/pairing/invitations", {"hostId":scenario.host}, scenario.admin)
    other.identity = other.call('host', 'connect', '--invite-stdin', '--yes', stdin_text=invitation["pairingUri"])
    other.sessions(scenario.host)
    scenario.model.plan("question", hold=True)
    paired_device.send(session, "synthetic question", delivery="enqueue-after-run")
    wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"], timeout=15, process=scenario.native)
    batch = paired_device.call('session', 'question', 'list', "--session", session)["batches"][0]
    assert batch["phase"] == "QUESTION_CONTINUED" and batch["answerable"]
    answer = scenario.directory / "gx-continued-answer.json"
    answer.write_text(json.dumps({"answers":[{"id":"q1", "selected":["Yes"]}]}))
    args = ["session", "question", "answer", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer), "--command-id", "gx-continued-answer"]
    assert scenario.request("gx-hold-continued-answer")["armed"]
    process = subprocess.Popen(paired_device.args(*args), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(lambda: scenario.request("gx-held-continued-status")["entered"], timeout=15)
        assert scenario.request("gx-disable-one-capability", config={"operation":"question_continued"})["disabled"]
        next_args = ["session", "question", "answer", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer)]
        code, values = other.invoke(*next_args, success=False)
        assert code != 0 and values[-1]["error"]["code"] == "unsupported_capability"
        assert scenario.request("gx-held-continued-status")["calls"] == 0
        assert scenario.request("gx-release-continued-answer")["released"]
        output, _ = process.communicate(timeout=20)
        result = json.loads(output)
        assert process.returncode == 0 and result["data"]["question_receipt"]["phase"] == "QUESTION_REPLY_QUEUED"
        assert scenario.request("gx-held-continued-status")["calls"] == 1
        assert scenario.request("question-continued-status", sessionId=session)["queued"] == 1
    finally:
        try: scenario.request("gx-reset-continued-answer")
        except RuntimeError: pass
        scenario.model.release.set()
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)
