"""Q03: a native question claim held across an Access restart still releases safely.

Access keeps HTTP-polling claims only in memory. After an Access restart the
claim is not reconstructed, so the documented request deadline is the fallback
release path. These cases do not require the 10-second lease to be restored and
do not prescribe a recovery data structure; they require a bounded safe release,
no implicit answer, and a later answer that settles exactly once.
"""

import json
import os
import signal
import subprocess
import time

import pytest

from environment import ROOT, wait
from journey_helpers import restart_access

pytestmark = pytest.mark.__getattr__("native-dsh")

# The documented HostRequest deadline is 30 seconds. Keep a generous margin for
# container scheduling while still proving the release is bounded.
DEADLINE_MARGIN = 15

# The native fixture question must outlive the request-deadline fallback so a
# later answer can still be accepted.
QUESTION_WINDOW_MS = 120_000


def _answer_process(paired_device, session, batch, answer, command_id):
    return subprocess.Popen(
        [
            str(ROOT / "target/debug/whalerein"), "--json", "--yes",
            "--data-dir", str(paired_device.directory / "client"),
            "answer", "--session", session,
            "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
            "--version", str(batch["version"]), "--digest", batch["digest"],
            "--phase", batch["phase"], "--input", str(answer),
            "--command-id", command_id,
        ],
        cwd=ROOT,
        env={
            **os.environ,
            "WHALEREIN_SECRET_BACKEND": "file:" + str(paired_device.directory / "secrets"),
            "WHALEREIN_LOG": "error",
        },
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _answer(scenario, paired_device, session, batch, answer, command_id):
    return paired_device.call(
        "answer", "--session", session,
        "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
        "--version", str(batch["version"]), "--digest", batch["digest"],
        "--phase", batch["phase"], "--input", str(answer),
        "--command-id", command_id,
    )


def _held_question(scenario, paired_device):
    session = scenario.create()
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    assert scenario.request(
        "question-start-remote-fixture", sessionId=session,
        config={"questionMs": QUESTION_WINDOW_MS},
    )["started"]
    wait(
        lambda: any(
            item["answerable"]
            for item in paired_device.call("questions", "--session", session)["batches"]
        ),
        timeout=20,
        process=scenario.native,
    )
    batch = paired_device.call("questions", "--session", session)["batches"][0]
    assert batch["answerable"] and batch["phase"] == "QUESTION_OPEN"
    answer = scenario.directory / "presence-restart-answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    assert scenario.request("question-hold-native-claim")["installed"]
    assert scenario.request("gx-arm-question-counter", config={"phase": "open"})["armed"]
    return session, batch, answer


def _assert_released_without_answer(scenario, paired_device, session, batch, started):
    wait(
        lambda: scenario.request("question-native-claim-status")["held"] == 0,
        timeout=30 + DEADLINE_MARGIN,
        process=scenario.native,
    )
    assert time.monotonic() - started < 30 + DEADLINE_MARGIN
    assert scenario.request("question-restore-native-claim")["restored"]
    # The first answer was never settled or fabricated: the Host's live
    # question page still has the same open, answerable batch. Only the request
    # scope ended.
    current = paired_device.call("questions", "--session", session)["batches"][0]
    assert current["batch_id"] == batch["batch_id"]
    assert current["answerable"] is True
    # One real native answer attempt was made; none of it settled the question.
    assert scenario.request("gx-question-count")["count"] == 1
    return current


def _later_answer_settles_once(scenario, paired_device, session, batch, answer):
    receipt = _answer(scenario, paired_device, session, batch, answer, "presence-restart-after")
    assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-result-remote-fixture", sessionId=session) == {
        "selected": "Yes",
        "pending": False,
    }
    assert paired_device.call("questions", "--session", session)["batches"] == []
    # Exactly one more native attempt, and replaying the same command id does
    # not invoke the source again.
    assert scenario.request("gx-question-count")["count"] == 2
    status, replay = scenario.replay_command(paired_device)
    assert status in (200, 202) and replay["commandId"] == receipt["command_id"]
    assert scenario.request("gx-question-count")["count"] == 2


@pytest.mark.description(
    "A native claim held across an Access restart releases by the bounded request deadline when the client then exits, fabricates no answer, and a later answer settles once."
)
def test_question_claim_held_across_access_restart_releases_after_client_exit(scenario, paired_device):
    session, batch, answer = _held_question(scenario, paired_device)
    process = _answer_process(paired_device, session, batch, answer, "presence-restart-held")
    try:
        wait(
            lambda: scenario.request("question-native-claim-status")["held"] == 1,
            timeout=15,
            process=scenario.native,
        )
        # Freeze the client so it cannot observe the restart, then let it die
        # after the restarted Access has already lost the in-memory claim.
        process.send_signal(signal.SIGSTOP)
        restart_access(scenario)
        assert scenario.request("question-native-claim-status")["held"] == 1
        started = time.monotonic()
        process.kill()
        process.communicate(timeout=15)
        current = _assert_released_without_answer(scenario, paired_device, session, batch, started)
        _later_answer_settles_once(scenario, paired_device, session, current, answer)
    finally:
        if process.poll() is None:
            process.send_signal(signal.SIGCONT)
            process.kill()
            try:
                process.communicate(timeout=5)
            except (subprocess.TimeoutExpired, OSError):
                pass
        try:
            scenario.request("question-restore-native-claim")
        except Exception:
            pass


@pytest.mark.description(
    "A native claim held while its client dies during an Access restart releases by the bounded request deadline, fabricates no answer, and a later answer settles once."
)
def test_question_claim_held_across_access_restart_releases_after_client_died_during_restart(scenario, paired_device):
    session, batch, answer = _held_question(scenario, paired_device)
    process = _answer_process(paired_device, session, batch, answer, "presence-restart-died")
    try:
        wait(
            lambda: scenario.request("question-native-claim-status")["held"] == 1,
            timeout=15,
            process=scenario.native,
        )
        started = time.monotonic()
        # Tear Access down, kill the polling client while Access is down, then
        # start Access again. The new process starts with no claims at all.
        scenario.access.close()
        process.kill()
        process.communicate(timeout=15)
        scenario.start_access()
        current = _assert_released_without_answer(scenario, paired_device, session, batch, started)
        _later_answer_settles_once(scenario, paired_device, session, current, answer)
    finally:
        if process.poll() is None:
            process.kill()
            try:
                process.communicate(timeout=5)
            except (subprocess.TimeoutExpired, OSError):
                pass
        try:
            scenario.request("question-restore-native-claim")
        except Exception:
            pass
