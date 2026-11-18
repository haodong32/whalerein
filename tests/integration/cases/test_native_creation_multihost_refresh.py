"""Native creation remains bound to its selected Host across refresh errors."""

import json
import os
from pathlib import Path
import sqlite3
import subprocess
from concurrent.futures import ThreadPoolExecutor

import pytest

from environment import ROOT, Device, http, wait
from native_environment import NativeScenario

pytestmark = pytest.mark.__getattr__("native-dsh")


def test_native_creation_refresh_failure_preserves_selected_host(scenario, paired_device):
    """A failed secondary Host refresh neither repeats creation nor changes primary sessions."""
    directory = scenario.directory / "creation-secondary"
    directory.mkdir()
    other = NativeScenario(directory, scenario.schema)
    other.host = "host-creation-secondary"
    other.http_port = 3110
    other.env = scenario.env.copy()
    try:
        other.start_native()
        invitation = http("/v1/pairing/invitations", {"hostId": other.host}, scenario.admin)
        paired_device.pair(invitation["pairingUri"])
        assert other.request("test-enable-create")["enabled"]
        primary_before = {item["session_id"] for item in paired_device.sessions(scenario.host)}
        secondary_before = {item["session_id"] for item in paired_device.sessions(other.host)}
        receipt = paired_device.call("session", "create", "--host", other.host)["lifecycle_receipt"]
        session = receipt["session_id"]
        assert receipt["disposition"] == "READY" and session
        assert other.request("gx-fail-session-list")["armed"]
        code, _ = paired_device.invoke("session", "list", "--host", other.host, success=False)
        assert code != 0
        assert {item["session_id"] for item in paired_device.sessions(scenario.host)} == primary_before
        assert other.request("gx-restore-session-list")["restored"]
        secondary_after = {item["session_id"] for item in paired_device.sessions(other.host)}
        assert secondary_after - secondary_before == {session}
        assert primary_before == {item["session_id"] for item in paired_device.sessions(scenario.host)}
        assert scenario.model.calls == other.model.calls == 0
    finally:
        other.close()


def test_native_creation_two_devices_same_workspace(scenario, paired_device):
    """Two authorized devices create independent native sessions in one workspace."""
    assert scenario.request("test-enable-create")["enabled"]
    other = Device(scenario.directory / "creation-second-device")
    invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    other.pair(invitation["pairingUri"])
    assert other.identity["device_id"] != paired_device.identity["device_id"]
    workspace = scenario.request("workspace-id")["workspaceId"]
    before = {item["session_id"] for item in paired_device.sessions(scenario.host)}
    first = paired_device.call("session", "create", "--host", scenario.host, "--workspace", workspace)["lifecycle_receipt"]
    second = other.call("session", "create", "--host", scenario.host, "--workspace", workspace)["lifecycle_receipt"]
    first_id, second_id = first["session_id"], second["session_id"]
    assert first["disposition"] == second["disposition"] == "READY"
    assert first_id and second_id and first_id != second_id
    assert {item["session_id"] for item in paired_device.sessions(scenario.host)} - before == {first_id, second_id}
    assert scenario.request("workspace-has-session", sessionId=first_id)["attached"]
    assert scenario.request("workspace-has-session", sessionId=second_id)["attached"]
    assert scenario.model.calls == 0


def test_native_remote_reuse_then_started_fallback(scenario, paired_device):
    """Remote reuse keeps a blank identity and falls back after the original session starts."""
    assert scenario.request("test-enable-create")["enabled"]
    original = paired_device.call("session", "create", "--host", scenario.host)["lifecycle_receipt"]
    session = original["session_id"]
    reused = paired_device.call("session", "create", "--host", scenario.host, "--session-id", session)["lifecycle_receipt"]
    assert reused["disposition"] == "READY" and reused["session_id"] == session
    scenario.model.plan()
    paired_device.send_confirmed(session, "synthetic fixture", delivery="start-if-idle")
    scenario.wait_idle(session)
    fallback = paired_device.call("session", "create", "--host", scenario.host, "--session-id", session)["lifecycle_receipt"]
    assert fallback["disposition"] == "READY" and fallback["session_id"] != session
    assert {session, fallback["session_id"]} <= {item["session_id"] for item in paired_device.sessions(scenario.host)}
    assert scenario.model.calls == 1


def test_native_remote_uses_host_defaults_not_client_location(scenario, paired_device):
    """Remote omitted-location creation follows native Host cwd and default preset."""
    assert scenario.request("test-enable-create")["enabled"]
    assert scenario.work.resolve() != ROOT.resolve()
    native = scenario.request("create-native-default")
    remote = paired_device.call("session", "create", "--host", scenario.host)["lifecycle_receipt"]
    assert remote["disposition"] == "READY"
    assert scenario.request("same-cwd", sessionId=native["sessionId"], config={"childId": remote["session_id"]})["same"]
    assert scenario.request("session-has-default-preset", sessionId=remote["session_id"])["matches"]
    assert scenario.model.calls == 0


def test_native_creation_rejects_revoked_selected_host(scenario, paired_device):
    """A revoked Host selection cannot create a native session or affect another Host."""
    directory = scenario.directory / "creation-revoked-secondary"
    directory.mkdir()
    other = NativeScenario(directory, scenario.schema)
    other.host = "host-creation-revoked"
    other.http_port = 3110
    other.env = scenario.env.copy()
    try:
        other.start_native()
        invitation = http("/v1/pairing/invitations", {"hostId": other.host}, scenario.admin)
        paired_device.pair(invitation["pairingUri"])
        assert other.request("test-enable-create")["enabled"]
        primary_count = scenario.request("session-count")["count"]
        secondary_count = other.request("session-count")["count"]
        assert other.request("gx-revoke-host-grant", deviceId=paired_device.identity["device_id"])["revoked"]
        wait(lambda: other.host not in {row["host_id"] for row in paired_device.hosts()}, timeout=20)
        code, values = paired_device.invoke("session", "create", "--host", other.host, success=False)
        assert code != 0
        assert all("lifecycle_receipt" not in value.get("data", {}) for value in values)
        assert other.request("session-count")["count"] == secondary_count
        assert scenario.request("session-count")["count"] == primary_count
        assert scenario.model.calls == other.model.calls == 0
    finally:
        other.close()


def test_native_creation_concurrent_command_replay_no_duplicate(scenario, paired_device):
    """Concurrent encrypted retries share one native create; a changed digest is rejected."""
    assert scenario.request("test-enable-create")["enabled"]
    before = scenario.request("session-count")["count"]
    assert scenario.request("gx-hold-create")["armed"]
    process = subprocess.Popen(
        paired_device.args("session", 'create', "--host", scenario.host),
        cwd=ROOT,
        env=paired_device.env(),
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
    )
    released = False
    try:
        wait(lambda: scenario.request("gx-held-create-status")["entered"], timeout=15)
        with ThreadPoolExecutor(max_workers=3) as executor:
            same = [executor.submit(scenario.replay_command, paired_device) for _ in range(2)]
            changed = executor.submit(scenario.replay_command, paired_device, changed=True)
            first, second = [result.result(timeout=15) for result in same]
            assert first[0] in (200, 202) and second[0] in (200, 202)
            assert first[1]["requestId"] == second[1]["requestId"]
            assert first[1]["commandId"] == second[1]["commandId"]
            assert changed.result(timeout=15)[0] == 409
        assert scenario.request("session-count")["count"] == before
        assert scenario.request("gx-release-create")["released"]
        released = True
        output, _ = process.communicate(timeout=30)
        assert process.returncode == 0
        result = json.loads(output)["data"]
        assert result["request_id"] == first[1]["requestId"]
        assert result["command_id"] == first[1]["commandId"]
        assert result["lifecycle_receipt"]["disposition"] == "READY"
        assert scenario.request("session-count")["count"] == before + 1
        assert scenario.model.calls == 0
    finally:
        if not released:
            try:
                scenario.request("gx-release-create")
            except RuntimeError:
                pass
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


@pytest.mark.privacy_debug_logging
def test_native_creation_debug_logs_and_receipt_omit_private_location(scenario, paired_device):
    """Debug diagnostics and persisted creation receipts exclude a synthetic cwd marker."""
    assert scenario.request("test-enable-create")["enabled"]
    secret = "creation-synthetic-private-directory-d1438"
    cwd = scenario.work / secret
    outputs = [scenario.access.output, scenario.native.output]
    positions = {stream.name: Path(stream.name).stat().st_size for stream in outputs}
    result = subprocess.run(
        paired_device.args("session", 'create', "--host", scenario.host, "--cwd", str(cwd)),
        cwd=ROOT,
        env=paired_device.env(),
        capture_output=True, text=True, timeout=90, check=False,
    )
    assert result.returncode == 0
    data = json.loads(result.stdout)["data"]
    assert data["lifecycle_receipt"]["disposition"] == "READY" and cwd.is_dir()
    assert all(secret not in output for output in (result.stdout, result.stderr))
    for stream in outputs:
        stream.flush()
        assert secret.encode() not in Path(stream.name).read_bytes()[positions[stream.name]:]
    native_log = Path(scenario.native.output.name).read_bytes()[positions[scenario.native.output.name]:]
    assert b"mqtt_publish_started" in native_log
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        receipts = db.execute("SELECT result_payload FROM commands WHERE request_id=?", (data["request_id"],)).fetchall()
    assert len(receipts) == 1 and secret.encode() not in (receipts[0][0] or b"")


def test_native_creation_history_refresh_failure_recovers_without_recreate(scenario, paired_device):
    """A failed history read preserves the created identity and recovers without creation replay."""
    assert scenario.request("test-enable-create")["enabled"]
    before = scenario.request("session-count")["count"]
    receipt = paired_device.call("session", "create", "--host", scenario.host)["lifecycle_receipt"]
    session = receipt["session_id"]
    assert receipt["disposition"] == "READY"
    scenario.model.plan()
    paired_device.send_confirmed(session, "synthetic fixture", delivery="start-if-idle")
    scenario.wait_idle(session)
    cached_before = paired_device.cached_history_state(scenario.host, session)
    assert scenario.request("creation-hold-history-failure", sessionId=session)["armed"]
    try:
        code, _ = paired_device.invoke("session", "watch", session, "--timeout", "1", success=False)
    finally:
        fault = scenario.request("creation-release-history-failure")
    assert fault["restored"] and fault["attempts"] > 0
    assert code != 0
    assert paired_device.cached_history_state(scenario.host, session) == cached_before
    assert scenario.request("session-count")["count"] == before + 1
    result = paired_device.observe_history(session, scenario.host)
    assert result["session"]["session_id"] == session
    assert len(paired_device.cached_history_state(scenario.host, session)[1]) >= 2
    assert scenario.request("session-count")["count"] == before + 1
    assert scenario.model.calls == 1


def test_native_creation_unpaired_has_no_source_effect(scenario, paired_device):
    """An unpaired CLI cannot create or obtain a session identity from a native Host."""
    assert scenario.request("test-enable-create")["enabled"]
    before = scenario.request("session-count")["count"]
    stranger = Device(scenario.directory / "creation-unpaired")
    code, values = stranger.invoke("session", "create", "--host", scenario.host, success=False)
    assert code != 0
    assert all("lifecycle_receipt" not in value.get("data", {}) for value in values)
    assert scenario.request("session-count")["count"] == before
    assert scenario.model.calls == 0
