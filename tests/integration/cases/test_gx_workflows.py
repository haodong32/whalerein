"""Cross-feature workflows; a passing case proves only its named variant."""

import json
import sqlite3
from pathlib import Path
import pytest
from environment import wait

pytestmark = pytest.mark.__getattr__("native-dsh")


def enable(scenario):
    assert scenario.request("test-enable-gx")["enabled"]


def select_model(paired_device, host, session):
    catalog = paired_device.call('session', 'models', "--host", host)
    group = next(group for group in catalog["groups"] if group["models"])
    model = group["models"][0]
    result = paired_device.call('session', 'model', "set", "--session", session, "--provider", group["id"], "--model", model["id"])
    assert result["status"] == "succeeded"
    assert result["model_selection"]["default_persistence"] == "unconfirmed"
    return group["id"], model["id"]


def answer(paired_device, directory, session, batch):
    path = directory / "gx-answer.json"
    path.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    return paired_device.call('session', 'question', 'answer', "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(path))


def test_gx01_native_default_create_model_queue_open_answer(scenario, paired_device):
    """A native session flows through model selection, queued send, input wait and explicit open answer."""
    enable(scenario)
    created = paired_device.call('session', 'create', "--host", scenario.host)
    session = created["lifecycle_receipt"]["session_id"]
    assert created["lifecycle_receipt"]["disposition"] == "READY"
    assert any(row["session_id"] == session for row in paired_device.sessions(scenario.host))
    select_model(paired_device, scenario.host, session)
    scenario.model.plan()
    sent = paired_device.send(session, "synthetic gx turn", delivery="enqueue-after-run")
    assert sent["receipt"]["status"] == "succeeded" and sent["receipt"]["delivery_id"]
    scenario.wait_idle(session)
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    assert scenario.request("question-start-remote-fixture", sessionId=session)["started"]
    observation = paired_device.call('session', 'wait', session, "--timeout", "3")
    assert observation["reason"] == "input_required" and "question" in observation["state"]["pending_input"]
    batches = paired_device.call('session', 'question', 'list', "--session", session)["batches"]
    assert len(batches) == 1 and batches[0]["phase"] == "QUESTION_OPEN"
    receipt = answer(paired_device, scenario.directory, session, batches[0])
    assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-result-remote-fixture", sessionId=session) == {"selected": "Yes", "pending": False}
    assert paired_device.call('session', 'wait', session, "--timeout", "3")["reason"] == "idle"


@pytest.mark.timed_questions
def test_gx01_native_continued_native_timeout_then_answer(scenario, paired_device):
    """A native continued question is answered explicitly after an authoritative wait timeout."""
    enable(scenario)
    session = paired_device.call('session', 'create', "--host", scenario.host)["lifecycle_receipt"]["session_id"]
    paired_device.sessions(scenario.host)
    select_model(paired_device, scenario.host, session)
    scenario.model.plan("question", hold=True)
    try:
        receipt = paired_device.send(session, "synthetic continued question", delivery="enqueue-after-run")
        assert receipt["receipt"]["status"] == "succeeded" and receipt["receipt"]["delivery_id"]
        wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"], timeout=15, process=scenario.native)
        batches = paired_device.call('session', 'question', 'list', "--session", session)["batches"]
        assert len(batches) == 1 and batches[0]["phase"] == "QUESTION_CONTINUED"
        native = scenario.request("native-wait", sessionId=session, timeoutMs=300)
        observed = paired_device.call('session', 'wait', session, "--timeout", "1")
        Path("/reports/gx01-continued-native-parity.json").write_text(json.dumps({"native_reason":native["reason"], "native_pending_input":native["pendingInput"], "reason":observed["reason"], "pending_input":observed["state"]["pending_input"]}))
        assert observed["reason"] == native["reason"] == "timeout"
        assert observed["state"]["pending_input"] == native["pendingInput"] == []
        Path("/reports/gx01-parity-stage.json").write_text(json.dumps({"stage":"answer"}))
        batch = batches[0]
        path = scenario.directory / "gx-answer.json"
        path.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
        native_status = scenario.request("question-continued-status", sessionId=session)
        code, envelopes = paired_device.invoke('session', 'question', 'answer', "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(path), success=False)
        Path("/reports/gx01-parity-stage.json").write_text(json.dumps({"stage":"answer-result", "exit":code, "error":envelopes[-1].get("error",{}).get("code"), "layer":envelopes[-1].get("error",{}).get("layer"), "answerable":batch.get("answerable"), "unavailable":batch.get("content_unavailable"), "question_count":len(batch.get("questions",[])), "has_expected_option":any(option.get("label") == "Yes" for question in batch.get("questions",[]) for option in question.get("options",[])), "continued":native_status["continued"], "queued":native_status["queued"]}))
        assert code == 0
        result = envelopes[-1]["data"]
        Path("/reports/gx01-parity-stage.json").write_text(json.dumps({"stage":"queued-receipt", "phase":result.get("question_receipt",{}).get("phase")}))
        assert result["question_receipt"]["phase"] == "QUESTION_REPLY_QUEUED"
        Path("/reports/gx01-parity-stage.json").write_text(json.dumps({"stage":"native-queued"}))
        assert scenario.request("question-continued-status", sessionId=session)["queued"] == 1
    finally:
        scenario.model.release.set()
    Path("/reports/gx01-parity-stage.json").write_text(json.dumps({"stage":"post-wait"}))
    assert paired_device.call('session', 'wait', session, "--timeout", "20")["reason"] == "idle"


def test_gx02_native_fork_model_and_independent_history(scenario, paired_device):
    """A native fork keeps parent and child identity and history independent after a model selection."""
    enable(scenario)
    parent = paired_device.call('session', 'create', "--host", scenario.host)["lifecycle_receipt"]["session_id"]
    paired_device.sessions(scenario.host)
    select_model(paired_device, scenario.host, parent)
    scenario.model.plan()
    assert paired_device.send(parent, "synthetic parent", delivery="enqueue-after-run")["receipt"]["status"] == "succeeded"
    scenario.wait_idle(parent)
    paired_device.observe_history(parent)
    before = scenario.cached(paired_device, parent)
    calls = scenario.model.calls
    receipt = paired_device.call('session', 'fork', parent)["lifecycle_receipt"]
    child = receipt["session_id"]
    assert receipt["parent_session_id"] == parent and child != parent
    assert scenario.request("same-cwd", sessionId=parent, config={"childId": child})["same"]
    assert scenario.model.calls == calls and not scenario.request("state", sessionId=child)["running"]
    paired_device.sessions(scenario.host)
    paired_device.observe_history(child)
    child_before = scenario.cached(paired_device, child)
    assert scenario.cached(paired_device, parent) == before
    scenario.model.plan()
    paired_device.send(child, "synthetic child", delivery="enqueue-after-run")
    scenario.wait_idle(child, assistant_count=2)
    paired_device.observe_history(child)
    assert scenario.cached(paired_device, child) != child_before
    assert scenario.cached(paired_device, parent) == before


def test_gx03_native_queue_edit_remove_promote_interrupt_and_wait(scenario, paired_device):
    """Native queue controls remain distinct from interrupt and a direct idle steer."""
    def stage(value):
        Path("/reports/gx03-stage.json").write_text(json.dumps({"stage": value}))
    stage("enable")
    enable(scenario)
    session = paired_device.call('session', 'create', "--host", scenario.host)["lifecycle_receipt"]["session_id"]
    paired_device.sessions(scenario.host)
    stage("plan")
    scenario.model.plan(hold=True)
    try:
        stage("start")
        paired_device.send(session, "synthetic active", delivery="enqueue-after-run")
        assert scenario.model.started.wait(timeout=15)
        stage("enqueue")
        for _ in range(4):
            assert paired_device.send(session, "synthetic pending", delivery="enqueue-after-run")["receipt"]["delivery_id"]
        stage("queue_available")
        assert scenario.request("queue-read", sessionId=session)["available"]
        paired_device.sessions(scenario.host)
        stage("initial_queue")
        initial = paired_device.call('session', 'queue', "list", "--session", session)
        ids = [item["item_id"] for item in initial["items"]]
        assert len(ids) == 4 and len(set(ids)) == 4
        stage("edit")
        edit = paired_device.call('session', 'queue', "edit", "--session", session, "--item", ids[0], "replacement fixture")
        assert edit["command"]["status"] == "succeeded"
        stage("changed_read")
        changed = paired_device.call('session', 'queue', "list", "--session", session)
        assert [item["item_id"] for item in changed["items"]] == ids
        assert scenario.request("queue-read", sessionId=session)["items"][0]["fixtureText"]
        stage("remove")
        removed = paired_device.call('session', 'queue', "remove", "--session", session, "--item", ids[1])
        assert removed["command"]["status"] == "succeeded"
        stage("remaining_read")
        remaining = paired_device.call('session', 'queue', "list", "--session", session)
        assert [item["item_id"] for item in remaining["items"]] == [ids[0], ids[2], ids[3]]
        stage("promote")
        promoted = paired_device.call('session', 'queue', "steer", "--session", session, "--item", ids[2])
        assert promoted["command"]["status"] == "succeeded"
        stage("interrupt")
        before_interrupt = scenario.request("events", sessionId=session)["types"].count("user/message")
        interruption = paired_device.interrupt(session)
        assert interruption["status"] in ("host_accepted", "succeeded")
        if interruption["status"] == "host_accepted":
            # Admission is not completion; query the same durable command until
            # a terminal receipt proves the native interrupt has finished.
            def interrupt_completed():
                code, envelopes = paired_device.invoke('operation', 'recover', interruption["request_id"], success=False)
                if code != 0:
                    raise AssertionError("interrupt recovery did not confirm success")
                status = envelopes[-1]["data"]["status"]
                assert status in ("host_accepted", "succeeded")
                return status == "succeeded"
            stage("interrupt_recovery")
            wait(interrupt_completed, timeout=15)
        stage("retention")
        current = paired_device.call('session', 'queue', "list", "--session", session)
        still_queued = {item["item_id"] for item in current["items"]} if current["available"] else set()
        seen = scenario.request("events", sessionId=session)["types"].count("user/message")
        assert ids[0] in still_queued or ids[3] in still_queued or seen > before_interrupt
    finally:
        scenario.model.release.set()
    stage("wait_idle")
    assert paired_device.call('session', 'wait', session, "--timeout", "20")["reason"] == "idle"
    stage("direct_steer")
    scenario.model.plan()
    direct = paired_device.send(session, "synthetic idle steer", delivery="steer")
    assert direct["receipt"]["status"] == "succeeded" and direct["receipt"]["delivery_id"]
    assert paired_device.call('session', 'wait', session, "--timeout", "20")["reason"] == "idle"


def test_gx03_native_interrupt_acceptance_requires_confirmation(scenario, paired_device):
    """A deliberately held native interrupt remains accepted until its original command is confirmed."""
    enable(scenario)
    session = paired_device.call('session', 'create', "--host", scenario.host)["lifecycle_receipt"]["session_id"]
    paired_device.sessions(scenario.host)
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    queued = paired_device.call('session', 'queue', "list", "--session", session)["items"]
    assert len(queued) == 1
    assert scenario.request("gx-hold-interrupt")["armed"]
    released = False
    try:
        receipt = paired_device.interrupt(session)
        assert receipt["status"] == "host_accepted"
        wait(lambda: scenario.request("gx-held-interrupt-status")["entered"], timeout=10)
        def journal_status():
            with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
                row = db.execute("SELECT status FROM commands WHERE device_id=? AND command_id=?", (paired_device.identity["device_id"], receipt["command_id"])).fetchone()
                return row[0] if row else None
        assert journal_status() == "accepted"
        assert scenario.request("gx-release-interrupt")["released"]
        released = True
        wait(lambda: journal_status() == "succeeded", timeout=15)
        def confirmed():
            code, envelopes = paired_device.invoke('operation', 'recover', receipt["request_id"], success=False)
            assert code == 0
            return envelopes[-1]["data"]["status"] == "succeeded"
        wait(confirmed, timeout=15)
        current = paired_device.call('session', 'queue', "list", "--session", session)
        seen = scenario.request("events", sessionId=session)["types"].count("user/message")
        assert any(item["item_id"] == queued[0]["item_id"] for item in current["items"]) or seen > 1
    finally:
        scenario.model.release.set()
        if not released:
            try:
                scenario.request("gx-release-interrupt")
            except RuntimeError:
                pass


def test_gx04_native_creation_journal_failure_remains_unknown(scenario, paired_device):
    """A native create with a failed Host journal remains unknown after restart and is never repeated."""
    enable(scenario)
    before = {row["session_id"] for row in paired_device.sessions(scenario.host)}
    assert scenario.request("arm-fork-journal-fault")["armed"]
    code, values = paired_device.invoke('session', 'create', "--host", scenario.host, success=False)
    assert code == 7 and values[-1]["error"]["code"] == "unknown"
    pending = paired_device.call('operation', 'list')["pending_commands"]
    assert len(pending) == 1
    after = {row["session_id"] for row in paired_device.sessions(scenario.host)}
    assert len(after - before) == 1
    scenario.native.close()
    scenario.start_native()
    code, values = paired_device.invoke('operation', 'recover', pending[0]["request_id"], success=False)
    assert code == 7 and values[-1]["error"]["code"] == "unknown"
    assert {row["session_id"] for row in paired_device.sessions(scenario.host)} == after
    assert scenario.model.calls == 0


def test_gx04_native_lost_creation_notification_recovers_identity(scenario, paired_device):
    """A dropped native creation notification is recovered with its actual identity and no second session."""
    enable(scenario)
    before = {row["session_id"] for row in paired_device.sessions(scenario.host)}
    assert scenario.request("gx-drop-next-command-success")["armed"]
    result = paired_device.call('session', 'create', "--host", scenario.host)
    assert scenario.request("gx-drop-status")["count"] == 1
    session = result["lifecycle_receipt"]["session_id"]
    after = {row["session_id"] for row in paired_device.sessions(scenario.host)}
    assert after - before == {session}
    with sqlite3.connect(paired_device.data_dir / "cache.sqlite3") as db:
        assert db.execute("UPDATE commands SET status='unknown' WHERE request_id=?", (result["request_id"],)).rowcount == 1
    scenario.native.close()
    scenario.start_native()
    recovered = paired_device.call('operation', 'recover', result["request_id"])
    assert recovered["lifecycle_receipt"]["session_id"] == session
    assert {row["session_id"] for row in paired_device.sessions(scenario.host)} == after
    assert scenario.model.calls == 0
