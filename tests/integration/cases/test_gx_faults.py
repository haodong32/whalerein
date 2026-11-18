"""GX-04 isolated encrypted mutation recovery under a Host journal fault."""

import json
import sqlite3
import uuid
import pytest
from environment import wait

pytestmark = pytest.mark.chain


_OPERATIONS = [
    ("create", "executeHost"), ("fork", "forkSession"), ("send", "sendUserMessage"),
    ("interrupt", "interrupt"), ("model", "selectSessionModel"),
    ("queue_edit", "updatePendingQueue"), ("queue_remove", "updatePendingQueue"),
    ("queue_promote", "updatePendingQueue"), ("question_open", "answerOpenQuestion"),
    ("question_continued", "answerContinuedQuestion"), ("approval", "resolveApproval"),
]


@pytest.mark.parametrize("operation,source_method,fault", [
    pytest.param(operation, source_method, fault,
        marks=pytest.mark.description(f"A {operation} operation with a {fault} fault recovers without a second source mutation."))
    for operation, source_method in _OPERATIONS for fault in ("journal", "notification")
])
def test_gx04_fake_mutation_recovery(operation, source_method, fault, scenario, paired_device):
    """Inject one Host result fault after an encrypted command and recover without replay."""
    session = scenario.dsh.create_session()
    paired_device.sessions(scenario.host)
    if operation == "fork":
        paired_device.send(session, "synthetic completed parent")
    if operation == "interrupt":
        scenario.dsh.start_running(session)
    if operation.startswith("queue_"):
        session = "gx-queue-fixture"
        scenario.dsh.process.control("create_queue_session", session_id=session)
        paired_device.sessions(scenario.host)
        item = paired_device.call('session', 'queue', "list", "--session", session)["items"][0]["item_id"]
    if operation == "approval":
        scenario.dsh.request_approval(session)
        approval = paired_device.approvals(session)[0]
    if operation.startswith("question_"):
        method = "request_question_open" if operation == "question_open" else "request_question"
        scenario.dsh.process.control(method, session_id=session)
        batch = paired_device.call('session', 'question', 'list', "--session", session)["batches"][0]
        answer = scenario.directory / "gx-answer.json"
        answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    send_id = uuid.uuid4().hex
    args = {
        "create": ["session", "create", "--host", scenario.host],
        "fork": ["session", "fork", session],
        "send": ["session", "send", session, "synthetic input", "--detach", "--operation-id", send_id],
        "interrupt": ["session", "interrupt", session],
        "model": ["session", "model", "set", "--session", session, "--provider", "fixture-provider", "--model", "fixture-model"],
    }
    if operation.startswith("queue_"):
        action = {"queue_edit":"edit", "queue_remove":"remove", "queue_promote":"steer"}[operation]
        args[operation] = ["session", "queue", action, "--session", session, "--item", item] + (["synthetic replacement"] if action == "edit" else [])
    if operation.startswith("question_"):
        args[operation] = ["session", "question", "answer", "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer)]
    if operation == "approval":
        args[operation] = ["session", "approval", "approve", "--request", approval["approval_id"], "--digest", approval["digest"], "--version", str(approval["version"])]
    before = scenario.dsh.process.control("gx_effect_counts")[source_method]
    marker = scenario.directory / "plugin" / ("gx-fail-terminal" if fault == "journal" else "gx-drop-terminal")
    marker.write_text("armed")
    code, values = paired_device.invoke(*args[operation], success=False)
    wait(lambda: not marker.exists(), timeout=10)
    assert not marker.exists()
    assert scenario.dsh.process.control("gx_effect_counts")[source_method] == before + 1
    def receipt(data):
        if operation == "send":
            return data["receipt"]
        if operation.startswith("queue_"):
            return data["command"]
        return data

    if fault == "journal":
        if code == 0:
            assert receipt(values[-1]["data"])["status"] == "host_accepted"
        else:
            assert code == 7 and values[-1]["error"]["code"] == "unknown"
        retained = paired_device.call("operation", "list")
        if operation == "send":
            assert [item["operation_id"] for item in retained["sends"]] == [send_id]
            request = send_id
        else:
            pending = retained["pending_commands"]
            assert len(pending) == 1
            request = pending[0]["request_id"]
        original = None
    else:
        assert code == 0
        original = receipt(values[-1]["data"])
        request = original["request_id"]
        if operation == "send":
            assert request == send_id
        else:
            with sqlite3.connect(paired_device.data_dir / "cache.sqlite3") as db:
                assert db.execute("UPDATE commands SET status='unknown' WHERE request_id=?", (request,)).rowcount == 1
    scenario.plugin.close()
    scenario.start_plugin()
    code, values = paired_device.invoke("operation", "recover", request, success=False)
    if fault == "journal":
        assert code == 7 and values[-1]["error"]["code"] == "unknown"
    else:
        assert code == 0
        data = values[-1]["data"]
        recovered = data["receipt"] if operation == "send" else data
        assert recovered["status"] == "succeeded"
        assert recovered["command_id"] == original["command_id"]
        if operation == "send":
            assert data["operation_id"] == send_id
            assert data["reply"]["state"] == "committed"
        for key in ("lifecycle_receipt", "model_selection", "question_receipt", "delivery_id"):
            if original.get(key) is not None:
                assert recovered[key] == original[key]
    assert scenario.dsh.process.control("gx_effect_counts")[source_method] == before + 1


def test_gx04_fake_lost_create_receipt_recovers_same_identity(scenario, paired_device):
    """A lost terminal create notification is recovered from the journal without a duplicate native create."""
    marker = scenario.directory / "plugin" / "gx-drop-terminal"
    before = scenario.dsh.process.control("gx_effect_counts")["executeHost"]
    marker.write_text("armed")
    result = paired_device.call('session', 'create', "--host", scenario.host)
    assert not marker.exists()
    identity = result["lifecycle_receipt"]["session_id"]
    assert result["status"] == "succeeded" and identity
    assert scenario.dsh.process.control("gx_effect_counts")["executeHost"] == before + 1
    with sqlite3.connect(paired_device.data_dir / "cache.sqlite3") as db:
        assert db.execute("UPDATE commands SET status='unknown' WHERE request_id=?", (result["request_id"],)).rowcount == 1
    scenario.plugin.close()
    scenario.start_plugin()
    recovered = paired_device.call('operation', 'recover', result["request_id"])
    assert recovered["lifecycle_receipt"]["session_id"] == identity
    assert scenario.dsh.process.control("gx_effect_counts")["executeHost"] == before + 1
