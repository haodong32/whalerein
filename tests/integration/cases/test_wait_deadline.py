"""Controlled encrypted wait deadline and transport-failure acceptance."""

import json
import subprocess
import time
from pathlib import Path

import pytest
from environment import ROOT, wait

pytestmark = pytest.mark.chain


def start_wait(device, session, seconds=8):
    return subprocess.Popen(
        device.args("session", "wait", session, "--timeout", str(seconds)),
        cwd=ROOT, env=device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )


def read_records(path):
    return json.loads(path.read_text())


def test_wait_deadline_replacement_does_not_reset_total(scenario, paired_device, session_id):
    """Replacing a channel near an eight-second deadline keeps the original deadline and never interrupts native work."""
    scenario.dsh.start_running(session_id)
    records = scenario.directory / "plugin" / "test-wait-observations.json"
    process = start_wait(paired_device, session_id)
    started = time.monotonic()
    try:
        wait(lambda: any(row["status"] == "active" for row in read_records(records)), timeout=5)
        first = read_records(records)[0]["timeoutMs"]
        while time.monotonic() - started < 6.3:
            time.sleep(0.02)
        (records.parent / "test-wait-close-channel").touch()
        wait(lambda: any(row["status"] == "aborted" for row in read_records(records))
             and any(row["status"] == "active" for row in read_records(records)), timeout=2)
        active = [row["timeoutMs"] for row in read_records(records) if row["status"] == "active"]
        stdout, _ = process.communicate(timeout=5)
        elapsed = time.monotonic() - started
        result = json.loads(stdout)
        evidence = {"elapsed_seconds": round(elapsed, 3), "initial_slice_ms": first,
                    "reissued_slice_ms": min(active), "exit": process.returncode,
                    "reason": result.get("data", {}).get("reason")}
        Path("/reports/wait-near-deadline.json").write_text(json.dumps(evidence))
        assert process.returncode == 0 and result["data"]["reason"] == "timeout"
        assert 7.4 <= elapsed <= 9.0
        assert min(active) + 2000 < first
        assert not any(event["type"] == "session/interrupted" for event in scenario.dsh.snapshot(session_id)["events"])
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_wait_deadline_transport_expiry_after_valid_slice_is_error(scenario, paired_device, session_id):
    """A terminal transport expiry after a valid timeout slice remains an error before the overall deadline."""
    scenario.dsh.start_running(session_id)
    directory = scenario.directory / "plugin"
    records = directory / "test-wait-observations.json"
    force = directory / "test-wait-force-timeout"
    fault = directory / "test-wait-expire-result"
    force.touch()
    process = start_wait(paired_device, session_id)
    started = time.monotonic()
    try:
        wait(lambda: sum(row["status"] == "completed" for row in read_records(records)) >= 1
             and any(row["status"] == "active" for row in read_records(records)), timeout=5)
        fault.touch()
        force.touch()
        stdout, _ = process.communicate(timeout=6)
        elapsed = time.monotonic() - started
        result = json.loads(stdout)
        Path("/reports/wait-transport-expiry.json").write_text(json.dumps({
            "elapsed_seconds": round(elapsed, 3), "exit": process.returncode,
            "code": result.get("error", {}).get("code"), "has_data": "data" in result,
        }))
        assert not fault.exists()
        assert process.returncode == 7 and result["error"]["code"] == "expired" and result["data"]["target"]["session_id"] == session_id
        assert elapsed < 7
        assert not any(event["type"] == "session/interrupted" for event in scenario.dsh.snapshot(session_id)["events"])
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_wait_deadline_access_loss_after_valid_slice_is_error(scenario, paired_device, session_id):
    """Losing Access after a valid slice remains a transport error, never a successful business timeout."""
    scenario.dsh.start_running(session_id)
    directory = scenario.directory / "plugin"
    records = directory / "test-wait-observations.json"
    (directory / "test-wait-force-timeout").touch()
    process = start_wait(paired_device, session_id)
    started = time.monotonic()
    try:
        wait(lambda: sum(row["status"] == "completed" for row in read_records(records)) >= 1
             and any(row["status"] == "active" for row in read_records(records)), timeout=5)
        scenario.access.close()
        stdout, _ = process.communicate(timeout=12)
        elapsed = time.monotonic() - started
        result = json.loads(stdout)
        Path("/reports/wait-access-loss.json").write_text(json.dumps({
            "elapsed_seconds": round(elapsed, 3), "exit": process.returncode,
            "code": result.get("error", {}).get("code"), "reason": result.get("data", {}).get("reason"),
        }))
        assert process.returncode != 0 and "error" in result and result["data"]["target"]["session_id"] == session_id
        assert not any(event["type"] == "session/interrupted" for event in scenario.dsh.snapshot(session_id)["events"])
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)




def test_wait_deadline_transient_access_failure_recovers(scenario, paired_device, session_id):
    """A temporary Access outage followed by successful polling does not poison a later valid timeout observation."""
    scenario.dsh.start_running(session_id)
    directory = scenario.directory / "plugin"
    records = directory / "test-wait-observations.json"
    (directory / "test-wait-force-timeout").touch()
    process = start_wait(paired_device, session_id)
    started = time.monotonic()
    try:
        wait(lambda: sum(row["status"] == "completed" for row in read_records(records)) >= 1
             and any(row["status"] == "active" for row in read_records(records)), timeout=5)
        scenario.access.close()
        time.sleep(1)
        scenario.start_access()
        stdout, _ = process.communicate(timeout=12)
        elapsed = time.monotonic() - started
        result = json.loads(stdout)
        Path("/reports/wait-access-recovered.json").write_text(json.dumps({
            "elapsed_seconds": round(elapsed, 3), "exit": process.returncode,
            "code": result.get("error", {}).get("code"), "reason": result.get("data", {}).get("reason"),
        }))
        assert process.returncode == 0 and result["data"]["reason"] == "timeout"
        assert 7.4 <= elapsed <= 9.0
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)
