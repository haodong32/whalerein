"""QMQ native-DSH journeys: interrupt claims, observed model and queue state races."""

import json
import os
import subprocess

import pytest

from environment import ROOT, wait

pytestmark = pytest.mark.__getattr__("native-dsh")


def _cli(paired_device, *args):
    return subprocess.Popen(
        paired_device.args("--yes", *args),
        cwd=ROOT,
        env=paired_device.env(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _answer_file(scenario, name="qmq-native-answer.json"):
    path = scenario.directory / name
    path.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    return path


@pytest.mark.description("A native interrupt during an open answer releases the claim so a later answer settles.")
def test_native_interrupt_releases_an_open_question_claim(scenario, paired_device):
    session = scenario.create()
    paired_device.sessions(scenario.host)
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    assert scenario.request("question-start-remote-fixture", sessionId=session)["started"]
    wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_OPEN"], timeout=15, process=scenario.native)
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    answer = _answer_file(scenario)
    # Hold the native claim so the interrupt lands while the answer is in flight.
    assert scenario.request("gx-hold-question-claim")["armed"]
    process = _cli(paired_device, "session", "question", "answer", "--session", session, "--batch", batch["batch_id"],
        "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
        "--phase", batch["phase"], "--input", str(answer))
    try:
        wait(lambda: scenario.request("gx-held-question-status")["entered"], timeout=15, process=scenario.native)
        paired_device.interrupt(session)
        assert scenario.request("gx-release-question-claim")["released"]
        assert scenario.request("gx-reset-question-claim")["reset"]
        output, error = process.communicate(timeout=30)
        # The interrupted answer is definitive (conflict) or unconfirmed; it
        # must never look like the question stayed permanently busy.
        assert process.returncode in (7, 8), (process.returncode, output, error)
        # The same fixture call id can be answered again: a leaked claim would
        # make this second registration fail instead of settling.
        assert scenario.request("question-start-remote-fixture", sessionId=session)["started"]
        wait(lambda: any(batch["answerable"] for batch in paired_device.call("session", "question", "list", "--session", session)["batches"]), timeout=15)
        batch = [item for item in paired_device.call("session", "question", "list", "--session", session)["batches"] if item["answerable"]][-1]
        receipt = paired_device.call("session", "question", "answer", "--session", session, "--batch", batch["batch_id"],
            "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
            "--phase", batch["phase"], "--input", str(answer))
        assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
        assert scenario.request("question-result-remote-fixture", sessionId=session) == {"selected": "Yes", "pending": False}
    finally:
        if process.poll() is None:
            process.kill()
        try: scenario.request("gx-reset-question-claim")
        except Exception: pass


@pytest.mark.description("The next native turn after model set reports the selected model as used.")
def test_native_model_set_is_used_by_the_next_turn(scenario, paired_device):
    session = scenario.create()
    paired_device.sessions(scenario.host)
    catalog = scenario.request("model-catalog")
    group = next(group for group in catalog["groups"] if len(group.get("models", [])) >= 2)
    selected = group["models"][1]
    paired_device.call("session", "model", "set", "--session", session, "--provider", group["id"], "--model", selected["id"])
    scenario.model.plan()
    receipt = paired_device.send(session, "synthetic model turn", delivery="start-if-idle")["receipt"]
    assert receipt["status"] == "succeeded"
    scenario.wait_idle(session)
    observed = scenario.request("model-read", sessionId=session)
    assert observed["last_used"]["model"] == selected["id"]
    assert observed["pending"] is None
    assert observed["next"]["model"] == selected["id"]


@pytest.mark.description("A queue promotion whose session goes idle before the native update never claims a silent success.")
def test_native_queue_promotion_state_race_is_observable(scenario, paired_device):
    session = scenario.create()
    paired_device.sessions(scenario.host)
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    item = scenario.request("queue-read", sessionId=session)["items"][0]["id"]
    assert scenario.request("gx-hold-native-operation", config={"operation": "queue_promote"})["armed"]
    process = _cli(paired_device, "session", "queue", "steer", "--session", session, "--item", item)
    try:
        # The adapter observed a running session and reached the native update.
        wait(lambda: scenario.request("gx-held-native-status")["entered"], timeout=15, process=scenario.native)
        # The turn ends before the held native update is applied.
        scenario.model.release.set()
        scenario.wait_idle(session)
        assert scenario.request("gx-release-native-operation")["released"]
        output, error = process.communicate(timeout=30)
        assert scenario.request("gx-held-native-status")["calls"] == 1
        code = process.returncode
        result = json.loads(output) if output.strip() else {}
        assert code in (0, 7, 8), (code, output, error)
        items = paired_device.call("session", "queue", "list", "--session", session)["items"]
        placement = items[0]["placement"] if items else ""
        if code == 0:
            assert result["data"]["command"]["status"] == "succeeded"
            assert placement == "STEERING"
        else:
            # Unconfirmed or conflict, but never a success claim without the change.
            assert result.get("error", {}).get("code") in ("unknown", "conflict")
            assert placement in ("QUEUED", "STEERING", "")
    finally:
        if process.poll() is None:
            process.kill()
        try: scenario.request("gx-reset-native-operation")
        except Exception: pass
