"""Native delivery capabilities through the ordinary production construction path."""

import pytest
from environment import wait

pytestmark = pytest.mark.__getattr__("native-dsh")


def test_native_delivery_production_construction_queue_and_steer(scenario, paired_device):
    """Without test enablement, default native queue and running steer return acceptance receipts."""
    session = scenario.create()
    sessions = paired_device.sessions(scenario.host)
    selected = next(item for item in sessions if item["session_id"] == session)
    assert selected["source_capabilities"]["delivery_modes"] == ["START_IF_IDLE", "ENQUEUE_AFTER_RUN", "STEER"]
    scenario.model.plan(hold=True)
    first = paired_device.send(session, "synthetic initial")["receipt"]
    assert first["delivery_id"]
    assert scenario.model.started.wait(timeout=15)
    queued = paired_device.send(session, "synthetic queued")["receipt"]
    steered = paired_device.send(session, "synthetic steer", delivery="steer")["receipt"]
    assert len({first["delivery_id"], queued["delivery_id"], steered["delivery_id"]}) == 3
    wait(lambda: queued["delivery_id"] in scenario.request("inbox-metadata", sessionId=session)["ids"], process=scenario.native)
    scenario.model.release.set()
    scenario.wait_idle(session, assistant_count=2)


def test_native_delivery_production_construction_idle_steer(scenario, paired_device):
    """Without test enablement, native steer on an idle agent may wake it."""
    session = scenario.create()
    paired_device.sessions(scenario.host)
    scenario.model.plan()
    receipt = paired_device.send(session, "synthetic idle steer", delivery="steer")["receipt"]
    assert receipt["delivery_id"]
    scenario.wait_idle(session)
    assert scenario.model.calls == 1


@pytest.mark.parametrize("kind", [
    pytest.param("missing", marks=pytest.mark.description(
        "A missing native queue receipt remains unknown across restart."
    )),
    pytest.param("negative", marks=pytest.mark.description(
        "A negative native queue receipt remains unknown across restart."
    )),
    pytest.param("empty", marks=pytest.mark.description(
        "An empty native queue receipt remains unknown across restart."
    )),
])
def test_native_delivery_unconfirmed_receipt_stays_unknown_after_restart(scenario, paired_device, kind):
    """After native entry, an unconfirmed return cannot create a successful receipt or cause replay."""
    import sqlite3

    session = scenario.create()
    paired_device.sessions(scenario.host)
    baseline = scenario.request("gx-user-delivery-count", sessionId=session)["count"]
    scenario.model.plan()
    assert scenario.request("arm-native-unconfirmed-after-admission", config={"kind": kind})["armed"]
    code, values = paired_device.invoke("session", "send", "--detach", session, "synthetic delivery", success=False)
    assert code != 0 and values[-1]["error"]["code"] == "unknown"
    scenario.wait_idle(session)
    assert scenario.model.calls == 1
    assert scenario.request("gx-user-delivery-count", sessionId=session)["count"] == baseline + 1
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        row = db.execute("SELECT command_id, status FROM commands ORDER BY created_at DESC LIMIT 1").fetchone()
    assert row and row[1] == "unknown"
    scenario.native.close()
    scenario.start_native()
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        assert db.execute("SELECT status FROM commands WHERE command_id=?", (row[0],)).fetchone() == ("unknown",)
    status, replay = scenario.replay_command(paired_device)
    assert status in (200, 202) and replay["commandId"] == row[0]
    assert scenario.request("gx-user-delivery-count", sessionId=session)["count"] == baseline + 1
    assert scenario.model.calls == 1


@pytest.mark.wrapper_launcher
def test_native_delivery_nonstandard_launcher_open(scenario, paired_device):
    """Delivery entry points are advertised from the native prompt service, not a launcher allowlist."""
    session = scenario.create()
    selected = next(item for item in paired_device.sessions(scenario.host) if item["session_id"] == session)
    assert selected["source_capabilities"]["delivery_modes"] == ["START_IF_IDLE", "ENQUEUE_AFTER_RUN", "STEER"]
    scenario.model.plan()
    assert paired_device.send(session, "synthetic queued fixture")["receipt"]["delivery_id"]
    scenario.wait_idle(session)
    assert scenario.model.calls == 1


@pytest.mark.external_launcher
def test_native_delivery_external_official_symlink(scenario, paired_device):
    """The standard CLI is supported when invoked through an external npm-style symlink."""
    assert scenario.request("delivery-entrypoint-symlink")["symlink"]
    session = scenario.create()
    selected = next(item for item in paired_device.sessions(scenario.host) if item["session_id"] == session)
    assert selected["source_capabilities"]["delivery_modes"] == ["START_IF_IDLE", "ENQUEUE_AFTER_RUN", "STEER"]
    scenario.model.plan()
    receipt = paired_device.send(session, "synthetic external symlink")["receipt"]
    assert receipt["delivery_id"]
    scenario.wait_idle(session)
    assert scenario.model.calls == 1


def test_native_delivery_precheck_race_rejects_before_source_entry(scenario, paired_device):
    """A local start observed by the precheck rejects before the remote native send is entered."""
    session = scenario.create()
    paired_device.sessions(scenario.host)
    scenario.model.plan(hold=True)
    assert scenario.request("arm-delivery-barrier")["armed"]
    assert scenario.request("arm-precheck-running-race")["armed"]
    try:
        code, result = paired_device.invoke(
            "session", "send", "--detach", session, "--delivery", "start-if-idle", "synthetic remote attempt", success=False,
        )
        assert code == 8 and result[-1]["error"]["code"] == "conflict"
        assert scenario.model.started.wait(timeout=15)
        assert not scenario.request("delivery-barrier-state")["entered"]
        assert scenario.model.calls == 1
    finally:
        scenario.request("release-delivery-barrier")
        scenario.model.release.set()
    scenario.wait_idle(session)


@pytest.mark.privacy_debug_logging
@pytest.mark.privacy_access_info_logging
def test_native_delivery_debug_cli_and_host_omit_private_message(scenario, paired_device):
    """Debug CLI and native adapter logs exclude a synthetic delivery body."""
    import hashlib
    import json
    import re
    import sqlite3
    import subprocess
    import uuid
    from pathlib import Path
    from environment import ROOT

    session = scenario.create()
    paired_device.sessions(scenario.host)
    scenario.model.plan()
    marker = 'synthetic-private-' + uuid.uuid4().hex
    access_log = Path(scenario.access.output.name)
    access_offset = access_log.stat().st_size
    result = subprocess.run(
        paired_device.args("session", 'send', '--detach', session, marker),
        cwd=ROOT, env=paired_device.env(), capture_output=True, text=True, timeout=90, check=False,
    )
    assert result.returncode == 0
    assert marker not in result.stdout and marker not in result.stderr
    receipt = json.loads(result.stdout.splitlines()[-1])['data']['receipt']
    assert receipt['delivery_id'] and receipt['command_id']
    scenario.wait_idle(session)
    with sqlite3.connect(scenario.directory / 'plugin/state.sqlite') as db:
        row = db.execute('SELECT result_payload FROM commands WHERE command_id=?', (receipt['command_id'],)).fetchone()
    assert row and marker.encode() not in (row[0] or b'')
    wait(lambda: 'host result stored' in access_log.read_text(errors='replace')[access_offset:], timeout=10, process=scenario.access)
    access_output = access_log.read_text(errors='replace')[access_offset:]
    assert 'host request accepted' in access_output and 'host result stored' in access_output
    outputs = [Path(process.output.name).read_text(errors='replace') for process in scenario.processes]
    assert all(marker not in output for output in outputs)
    assert any('mqtt_publish_started' in output for output in outputs)
    device_id = paired_device.identity["device_id"]
    assert re.fullmatch(r"[A-Za-z0-9_-]+", device_id)
    assert re.fullmatch(r"[A-Za-z0-9_]+", scenario.schema)
    hashes = subprocess.run(
        ["psql", "-h", "127.0.0.1", "-p", "55432", "-U", "postgres",
         "-d", "whalerein_test", "-Atc",
         f"SELECT encode(auth_hash,'hex') FROM {scenario.schema}.device_credentials "
         f"WHERE device_id='{device_id}'"],
        check=True, capture_output=True, text=True, timeout=5,
    ).stdout.splitlines()
    assert hashes
    host_token = scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"]
    for output in [result.stdout, result.stderr, *outputs]:
        # Device tokens are 32 random bytes encoded as 43 unpadded base64url characters.
        # Compare with the Access digest without reading the platform credential.
        candidates = re.findall(r"(?=([A-Za-z0-9_-]{43}))", output)
        if host_token in output or any(hashlib.sha256(value.encode()).hexdigest() in hashes for value in candidates):
            pytest.fail("credential appeared in diagnostic output", pytrace=False)
    for file in paired_device.data_dir.glob("cache.sqlite3*"):
        if marker.encode() in file.read_bytes():
            pytest.fail('message content appeared in local command cache', pytrace=False)


def test_native_delivery_controller_lifecycle_reconnect(scenario, paired_device):
    """A real Cordis controller reload restarts the host and restores default queue."""
    session = scenario.create()
    assert scenario.request("cycle-controller")["armed"]
    marker = scenario.socket.with_suffix(".sock.controller-cycle")
    wait(lambda: marker.exists() and marker.read_text() in ("recreated", "failed"), timeout=30, process=scenario.native)
    assert marker.read_text() == "recreated"
    wait(lambda: scenario.request("cycle-status") == {"newController": True, "newHost": True}, timeout=45, process=scenario.native)
    wait(lambda: scenario.request("ready").get("connected"), timeout=45, process=scenario.native)
    wait(lambda: any(item["session_id"] == session and "ENQUEUE_AFTER_RUN" in item["source_capabilities"]["delivery_modes"] for item in paired_device.sessions(scenario.host)), timeout=45, process=scenario.native)
    scenario.model.plan(hold=True)
    assert paired_device.send(session, "synthetic after controller lifecycle")["receipt"]["delivery_id"]
    assert scenario.model.started.wait(timeout=15)
    queued = paired_device.send(session, "synthetic queue after controller lifecycle")["receipt"]
    wait(lambda: queued["delivery_id"] in scenario.request("inbox-metadata", sessionId=session)["ids"], process=scenario.native)
    scenario.model.release.set()
    scenario.wait_idle(session, assistant_count=2)
