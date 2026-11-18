"""Host presence-driven release of an abruptly disconnected question claimant."""

import json
import os
import signal
import subprocess
import time
from pathlib import Path

import pytest

from environment import ROOT, wait


pytestmark = pytest.mark.__getattr__("native-dsh")


def _answer_process(scenario, paired_device, session, batch, answer):
    return subprocess.Popen(paired_device.args("session", 'question', 'answer', "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer), "--command-id", "presence-kill-fixture"), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)


def test_native_question_simulated_presence_releases_claim(scenario, paired_device):
    """A Host device_offline event aborts the exact request scope and releases the native claim once."""
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
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 0, timeout=5, process=scenario.native)
        assert time.monotonic() - started < 8
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
