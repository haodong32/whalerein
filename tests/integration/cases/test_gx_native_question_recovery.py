"""GX-04 native question response recovery across isolated result faults."""

import json
import sqlite3
from pathlib import Path

import pytest
from environment import wait

pytestmark = pytest.mark.__getattr__("native-dsh")


@pytest.mark.timed_questions
@pytest.mark.parametrize("phase,fault", [
    pytest.param(phase, fault, marks=pytest.mark.description(
        f"A native {phase} answer under {fault} recovers without another source answer."
    ))
    for phase in ("open", "continued") for fault in ("journal", "notification")
])
def test_gx04_native_question_answers_recover_without_replay(phase, fault, scenario, paired_device):
    """A native answer remains single-shot when Host terminal persistence or notification fails."""
    def stage(value, **fields):
        Path(f"/reports/gx04-question-{phase}-{fault}.json").write_text(json.dumps({"stage":value, **fields}))
    assert scenario.request("test-enable-gx")["enabled"]
    session = scenario.create()
    paired_device.sessions(scenario.host)
    try:
        if phase == "open":
            assert scenario.request("question-enable-remote-fixture")["enabled"]
            assert scenario.request("question-start-remote-fixture", sessionId=session)["started"]
        else:
            scenario.model.plan("question", hold=True)
            paired_device.send(session, "synthetic question", delivery="enqueue-after-run")
            wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"], timeout=15, process=scenario.native)
        batches = paired_device.call("session", "question", "list", "--session", session)["batches"]
        assert len(batches) == 1 and batches[0]["answerable"]
        batch = batches[0]
        assert batch["phase"] == ("QUESTION_OPEN" if phase == "open" else "QUESTION_CONTINUED")
        answer = scenario.directory / "gx-question-answer.json"
        answer.write_text(json.dumps({"answers":[{"id":"q1", "selected":["Yes"]}]}))
        args = ["session", "question", "answer", "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer), "--command-id", "gx-question-answer"]
        assert scenario.request("gx-arm-question-counter", config={"phase":phase})["armed"]
        before = scenario.request("gx-question-count")["count"]
        if fault == "journal":
            assert scenario.request("arm-fork-journal-fault")["armed"]
        else:
            assert scenario.request("gx-drop-next-command-success")["armed"]
        stage("answer")
        code, envelopes = paired_device.invoke(*args, success=False)
        stage("answered", exit=code, error_code=envelopes[-1].get("error",{}).get("code"))
        assert scenario.request("gx-question-count")["count"] == before + 1
        if fault == "journal":
            assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
            pending = paired_device.call("operation", "list")["pending_commands"]
            assert len(pending) == 1
            request, receipt = pending[0]["request_id"], None
        else:
            assert code == 0 and scenario.request("gx-drop-status")["count"] == 1
            receipt = envelopes[-1]["data"]
            expected = "QUESTION_SETTLED" if phase == "open" else "QUESTION_REPLY_QUEUED"
            assert receipt["question_receipt"]["phase"] == expected
            request = receipt["request_id"]
            with sqlite3.connect(paired_device.data_dir / "cache.sqlite3") as db:
                assert db.execute("UPDATE commands SET status='unknown' WHERE request_id=?", (request,)).rowcount == 1
        if phase == "open":
            assert scenario.request("question-result-remote-fixture", sessionId=session) == {"selected":"Yes", "pending":False}
        else:
            assert scenario.request("question-continued-status", sessionId=session)["queued"] == 1
        stage("restart")
        scenario.native.close()
        scenario.start_native()
        assert scenario.request("gx-arm-question-counter", config={"phase":phase})["armed"]
        assert scenario.request("gx-question-count")["count"] == before + 1
        code, envelopes = paired_device.invoke("operation", "recover", request, success=False)
        stage("recovered", exit=code, error_code=envelopes[-1].get("error",{}).get("code"))
        if fault == "journal":
            assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
        else:
            assert code == 0 and envelopes[-1]["data"]["question_receipt"] == receipt["question_receipt"]
        assert scenario.request("gx-question-count")["count"] == before + 1
        stage("done")
    finally:
        scenario.model.release.set()
