"""Host presence-driven release of an abruptly disconnected question claimant."""

import json
import signal
import subprocess
import time

import pytest

from environment import ROOT, wait


pytestmark = pytest.mark.__getattr__("native-dsh")


def _answer_process(scenario, paired_device, session, batch, answer):
    return subprocess.Popen(paired_device.args("session", 'question', 'answer', "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer), "--command-id", "presence-kill-fixture"), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def test_native_question_simulated_presence_releases_claim(scenario, paired_device):
    """Observation loss does not cancel a live answer; its exact request lease expiry releases the claim once."""
    session = scenario.create()
    scenario.request("question-enable-remote-fixture")
    scenario.request("question-start-remote-fixture", sessionId=session)
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    scenario.request("question-hold-native-claim")
    process = _answer_process(scenario, paired_device, session, batch, answer)
    try:
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 1, timeout=15, process=scenario.native)
        started = time.monotonic()
        process.kill()
        try:
            process.communicate(timeout=5)
        except (subprocess.TimeoutExpired, OSError):
            pass
        assert scenario.request("simulate-device-offline", deviceId=paired_device.identity["device_id"])["simulated"]
        # Observation transport loss is not HTTP execution death. The killed
        # client's exact request lease must expire before its native claim is
        # released; the blanket device_offline path must not release it.
        time.sleep(3)
        assert scenario.request("question-native-claim-status")["held"] == 1
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 0, timeout=20, process=scenario.native)
        assert time.monotonic() - started < 25
        assert scenario.request("question-restore-native-claim")["restored"]
        current = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
        assert current["batch_id"] == batch["batch_id"]
        receipt = paired_device.call("session", "question", "answer", "--session", session, "--batch", batch["batch_id"],
            "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
            "--phase", batch["phase"], "--input", str(answer), "--command-id", "presence-after-kill")
        assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
    finally:
        if process.poll() is None:
            process.kill()
            try:
                process.communicate(timeout=5)
            except (subprocess.TimeoutExpired, OSError):
                pass




def test_native_question_abrupt_kill_releases_by_http_lease(scenario, paired_device):
    """An answer client that stops polling is released by Access request-claim sweep."""
    session = scenario.create()
    scenario.request("question-enable-remote-fixture")
    scenario.request("question-start-remote-fixture", sessionId=session)
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    scenario.request("question-hold-native-claim")
    process = _answer_process(scenario, paired_device, session, batch, answer)
    try:
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 1, timeout=15, process=scenario.native)
        started = time.monotonic()
        process.kill()
        try:
            process.communicate(timeout=5)
        except (subprocess.TimeoutExpired, OSError):
            pass
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 0, timeout=15, process=scenario.native)
        assert time.monotonic() - started < 15
        assert scenario.request("question-restore-native-claim")["restored"]
        current = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
        assert current["batch_id"] == batch["batch_id"]
        receipt = paired_device.call("session", "question", "answer", "--session", session, "--batch", batch["batch_id"],
            "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
            "--phase", batch["phase"], "--input", str(answer), "--command-id", "lease-after-kill")
        assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
    finally:
        if process.poll() is None:
            process.kill()
            try:
                process.communicate(timeout=5)
            except (subprocess.TimeoutExpired, OSError):
                pass


def _fixture_batch(scenario, paired_device):
    session = scenario.create()
    scenario.request("question-enable-remote-fixture")
    scenario.request("question-start-remote-fixture", sessionId=session)
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    return session, batch, answer


def test_native_question_half_open_releases_claim(scenario, paired_device):
    """A stopped (half-open) answer process stops polling and is released by the lease."""
    session, batch, answer = _fixture_batch(scenario, paired_device)
    scenario.request("question-hold-native-claim")
    process = _answer_process(scenario, paired_device, session, batch, answer)
    try:
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 1, timeout=15, process=scenario.native)
        started = time.monotonic()
        process.send_signal(signal.SIGSTOP)
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 0, timeout=15, process=scenario.native)
        assert time.monotonic() - started < 15
        assert scenario.request("question-restore-native-claim")["restored"]
    finally:
        if process.poll() is None:
            process.send_signal(signal.SIGCONT)
            process.kill()
            try:
                process.communicate(timeout=5)
            except (subprocess.TimeoutExpired, OSError):
                pass







def test_native_question_heartbeat_prevents_early_release(scenario, paired_device):
    """A live answer's heartbeat keeps the 10s lease from releasing it; killing it then releases."""
    session, batch, answer = _fixture_batch(scenario, paired_device)
    scenario.request("question-hold-native-claim")
    process = _answer_process(scenario, paired_device, session, batch, answer)
    try:
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 1, timeout=15, process=scenario.native)
        time.sleep(14)
        assert scenario.request("question-native-claim-status")["held"] == 1
        started = time.monotonic()
        process.kill()
        try:
            process.communicate(timeout=5)
        except (subprocess.TimeoutExpired, OSError):
            pass
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 0, timeout=15, process=scenario.native)
        assert time.monotonic() - started < 15
        assert scenario.request("question-restore-native-claim")["restored"]
    finally:
        if process.poll() is None:
            process.kill()
            try:
                process.communicate(timeout=5)
            except (subprocess.TimeoutExpired, OSError):
                pass



def test_native_question_reconnect_within_lease_keeps_claim(scenario, paired_device):
    """A client that resumes heartbeats within the lease is not released."""
    session, batch, answer = _fixture_batch(scenario, paired_device)
    scenario.request("question-hold-native-claim")
    process = _answer_process(scenario, paired_device, session, batch, answer)
    try:
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 1, timeout=15, process=scenario.native)
        process.send_signal(signal.SIGSTOP)
        time.sleep(5)
        process.send_signal(signal.SIGCONT)
        time.sleep(12)
        assert scenario.request("question-native-claim-status")["held"] == 1
        started = time.monotonic()
        process.kill()
        try:
            process.communicate(timeout=5)
        except (subprocess.TimeoutExpired, OSError):
            pass
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 0, timeout=15, process=scenario.native)
        assert time.monotonic() - started < 15
        assert scenario.request("question-restore-native-claim")["restored"]
    finally:
        if process.poll() is None:
            process.send_signal(signal.SIGCONT)
            process.kill()
            try:
                process.communicate(timeout=5)
            except (subprocess.TimeoutExpired, OSError):
                pass


def _cli_process(device, *commands):
    """Run one independent CLI invocation and keep its streams for the case."""
    return subprocess.Popen(
        device.args(*commands),
        cwd=ROOT,
        env=device.env(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )


def _stop_process(process):
    if process is None:
        return
    if process.poll() is None:
        process.kill()
    try:
        process.communicate(timeout=5)
    except (subprocess.TimeoutExpired, OSError):
        pass


def _answer_commands(session, batch, answer, command_id):
    return (
        "session", "question", "answer", "--session", session,
        "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
        "--version", str(batch["version"]), "--digest", batch["digest"],
        "--phase", batch["phase"], "--input", str(answer),
        "--command-id", command_id,
    )


def _answer_args(device, session, batch, answer, command_id):
    return device.args(*_answer_commands(session, batch, answer, command_id))


def test_native_killed_claim_release_keeps_sibling_watch_and_command(scenario, paired_device):
    """AC-28: a killed answer releases only its exact lease while a sibling command and Watch keep working."""
    session = scenario.create()
    scenario.request("question-enable-remote-fixture")
    scenario.request("question-start-remote-fixture", sessionId=session,
                     config={"questionMs": 120_000})
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    answer = scenario.directory / "sibling-lease-answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    # Hold the native wait so the killed client's exact Access request lease,
    # not a fabricated local completion, must release the claim.
    scenario.request("question-hold-native-claim")
    answer_process = subprocess.Popen(
        _answer_args(paired_device, session, batch, answer, "sibling-killed-claim"),
        cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE,
        stderr=subprocess.PIPE, text=True,
    )
    command_session = scenario.create()
    watch_session = scenario.create()
    paired_device.sessions(scenario.host)
    sender = watcher = None
    try:
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 1,
             timeout=15, process=scenario.native)

        # A sibling command observes unrelated work under the same Device and
        # stays in flight while the answer client dies.
        scenario.model.plan(hold=True)
        scenario.request("start", sessionId=command_session)
        assert scenario.model.started.wait(timeout=20), "sibling work did not start"
        sender = _cli_process(
            paired_device, "session", "wait", command_session,
            "--host", scenario.host, "--timeout", "30",
        )
        time.sleep(0.5)
        assert sender.poll() is None, f"sibling command exited at startup with {sender.returncode}"

        # A sibling Watch attaches to a different session and stays live.
        watcher = _cli_process(
            paired_device, "session", "watch", watch_session,
            "--host", scenario.host, "--once", "--timeout", "20",
        )
        wait(lambda: scenario.request("watches") > 0, timeout=10, process=scenario.native)
        assert sender.poll() is None and watcher.poll() is None

        started = time.monotonic()
        answer_process.kill()
        killed_output, _ = answer_process.communicate(timeout=5)
        assert "QUESTION_SETTLED" not in killed_output

        # Death alone is not lease expiry: the claim is still held shortly
        # after the client stops heartbeating.
        time.sleep(0.5)
        assert scenario.request("question-native-claim-status")["held"] == 1
        assert sender.poll() is None, f"sibling command exited early with {sender.returncode}"
        assert watcher.poll() is None, f"sibling watch exited early with {watcher.returncode}"

        # The sibling command finishes its unrelated work while the killed
        # answer's claim is still held, so the two leases are independent.
        scenario.model.release.set()
        sender_output, _ = sender.communicate(timeout=30)
        assert sender.returncode == 0, "sibling command lost its unrelated work"
        waited = json.loads(sender_output.strip().splitlines()[-1])["data"]
        assert waited["reason"] == "idle" and not waited["state"]["running"]
        scenario.wait_idle(command_session)
        assert scenario.request("question-native-claim-status")["held"] == 1

        # A sibling CLI invocation cannot steal the exact claim and does not
        # change the exact open batch.
        code, envelopes = paired_device.invoke(
            *_answer_commands(session, batch, answer, "sibling-steal-attempt"),
            success=False,
        )
        assert code != 0, "a sibling command must not take the held claim"
        assert envelopes[-1]["error"]["code"] == "conflict"
        assert scenario.request("question-native-claim-status")["held"] == 1

        time.sleep(max(0.0, 2.0 - (time.monotonic() - started)))
        assert scenario.request("question-native-claim-status")["held"] == 1
        assert watcher.poll() is None, f"sibling watch exited early with {watcher.returncode}"

        # Only the killed request's own lease elapses. The live sibling
        # command and observation traffic must not renew it, so release stays
        # bounded by the configured 10s lease plus one sweep.
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 0,
             timeout=20, process=scenario.native)
        elapsed = time.monotonic() - started
        assert 7.0 <= elapsed < 20.0, f"claim released outside the exact lease window: {elapsed:.1f}s"
        assert watcher.poll() is None, f"sibling watch exited during claim expiry with {watcher.returncode}"
        scenario.request("question-restore-native-claim")

        # The failed sibling attempt and expiry left the exact batch untouched.
        current = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
        assert current["batch_id"] == batch["batch_id"]
        assert current["source_epoch"] == batch["source_epoch"]
        assert current["version"] == batch["version"]
        assert current["digest"] == batch["digest"]
        assert current["phase"] == batch["phase"]
        assert current["answerable"] is True

        # The sibling Watch also stays usable and receives its own work.
        scenario.model.plan()
        paired_device.call(
            "session", "send", watch_session, "--detach",
            "--host", scenario.host, "--", "unrelated sibling watch fixture",
        )
        watch_output, _ = watcher.communicate(timeout=20)
        assert watcher.returncode == 0, "sibling Watch lost its unrelated work"
        assert "unrelated sibling watch fixture" in watch_output

        # A later authorized answer settles the same open question exactly once.
        receipt = paired_device.call(
            *_answer_commands(session, batch, answer, "sibling-after-release")
        )
        assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
        assert scenario.request("question-result-remote-fixture", sessionId=session) == {
            "selected": "Yes", "pending": False,
        }
        assert paired_device.call("session", "question", "list", "--session", session)["batches"] == []
        code, envelopes = paired_device.invoke(
            *_answer_commands(session, batch, answer, "sibling-after-release-again"),
            success=False,
        )
        assert code != 0 and envelopes[-1]["error"]["code"] == "conflict"
    finally:
        _stop_process(answer_process)
        _stop_process(sender)
        _stop_process(watcher)
        try:
            scenario.model.release.set()
        except Exception:
            pass
        try:
            scenario.request("question-restore-native-claim")
        except Exception:
            pass
