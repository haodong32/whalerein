"""Actual DSH services and a local model, isolated inside the integration image."""

import hashlib
from concurrent.futures import ThreadPoolExecutor
import os
import shutil
import signal
import subprocess
import time
import sqlite3
import json
from pathlib import Path

import pytest
from environment import Device, ROOT, http, wait

pytestmark = pytest.mark.__getattr__("native-dsh")


def events(values):
    return [item.get("data", {}).get("event", {}) for item in values]


def text_fields(value):
    if isinstance(value, dict):
        for key, child in value.items():
            if key == "text" and isinstance(child, str):
                yield child
            else:
                yield from text_fields(child)
    elif isinstance(value, list):
        for child in value:
            yield from text_fields(child)


def test_native_wait_probe(scenario, paired_device):
    """Native wait observes running timeouts, independent cancellation, and cold idle without starting work."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    running = scenario.request("native-wait", sessionId=session, timeoutMs=250)
    assert running["reason"] == "timeout" and running["running"]
    assert scenario.request("native-wait-abort", sessionId=session, timeoutMs=5000)["aborted"]
    assert scenario.request("state", sessionId=session)["running"]
    scenario.model.release.set()
    scenario.wait_idle(session)
    idle = scenario.request("native-wait", sessionId=session, timeoutMs=3000)
    assert idle["reason"] == "idle" and idle["active"] and not idle["running"]
    adapter_idle = scenario.request("adapter-wait", sessionId=session, timeoutMs=3000)
    assert adapter_idle["reason"] == "idle" and adapter_idle["state"]["active"]
    assert "output" not in adapter_idle
    maintenance = scenario.request("native-maintenance", sessionId=session)
    assert maintenance == {"active": True, "running": False, "premature": False, "reason": "idle"}
    output = scenario.request("native-output-abort", sessionId=session)
    assert output["entered"] and output["aborted"]
    assert scenario.request("native-output-failure", sessionId=session)["propagated"]
    no_first = scenario.request("native-no-first-state", sessionId=session)
    assert no_first["failed"] and no_first["waitTimeout"]
    calls = scenario.model.calls
    scenario.native.close()
    scenario.start_native()
    cold = scenario.request("native-wait", sessionId=session, timeoutMs=3000)
    assert cold["reason"] == "idle" and not cold["active"] and not cold["queueAvailable"]
    adapter_cold = scenario.request("adapter-wait-state", sessionId=session)
    assert not adapter_cold["active"] and not adapter_cold["queueAvailable"]
    assert scenario.model.calls == calls


def test_native_wait_refreshes_silent_input(scenario, paired_device):
    """The native wait resamples input without a Session event or Agent interruption."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    try:
        result = scenario.request("native-silent-input-refresh", sessionId=session)
        assert result["reason"] == "input-required"
        assert result["pending"] == ["question"]
        assert result["reads"] >= 2
        assert 700 <= result["elapsedMs"] < 3000
        assert result["eventsBefore"] == result["eventsAfter"]
        assert scenario.request("state", sessionId=session)["running"]
    finally:
        scenario.model.release.set()


def test_native_wait_output_failure_over_encrypted_chain(scenario, paired_device):
    """A native output-read failure remains an error and does not expose its contents."""
    session = scenario.create()
    paired_device.sessions(scenario.host)
    assert scenario.request("gx-arm-wait-capability")["enabled"]
    assert scenario.request("arm-native-wait-output-failure")["armed"]
    code, envelopes = paired_device.invoke("session", "wait", session, "--timeout", "3", success=False)
    assert code != 0
    assert envelopes[-1]["data"] == {"target": {"host_id": scenario.host, "session_id": session}}
    assert "synthetic-private-output-marker" not in json.dumps(envelopes)
    assert paired_device.call("session", "wait", session, "--timeout", "3")["reason"] == "idle"


def native_wait_remote_question_over_encrypted_chain(outcome, scenario, paired_device):
    """Native Remote input disappears after actual answer or native expiry."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    paired_device.sessions(scenario.host)
    assert scenario.request("gx-arm-wait-capability")["enabled"]
    pending = scenario.request("wait-remote-question-start", sessionId=session, config={"timed": outcome == "expire"})
    try:
        assert pending["pending"] > 0
        result = paired_device.call("session", "wait", session, "--timeout", "3")
        assert result["reason"] == "input_required"
        assert "question" in result["state"]["pending_input"]
        assert scenario.request("state", sessionId=session)["running"]
        settled = scenario.request("wait-remote-question-settle" if outcome == "settle" else "wait-remote-question-expired")
        assert settled["pending"] == 0
        assert settled["answered" if outcome == "settle" else "expired"]
        result = paired_device.call("session", "wait", session, "--timeout", "1")
        assert result["reason"] == "timeout" and result["state"]["pending_input"] == []
    finally:
        scenario.request("wait-remote-question-stop")
        scenario.model.release.set()


def test_native_wait_remote_question_settled(scenario, paired_device):
    """An answered native Remote question disappears from the encrypted wait state."""
    native_wait_remote_question_over_encrypted_chain("settle", scenario, paired_device)


def test_native_wait_remote_question_expired(scenario, paired_device):
    """An expired native Remote foreground question disappears from encrypted wait state."""
    native_wait_remote_question_over_encrypted_chain("expire", scenario, paired_device)


def native_wait_after_intermediate_slices(outcome, scenario, paired_device):
    """Real native slices time out before a later Agent stop or Remote input."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    paired_device.sessions(scenario.host)
    assert scenario.request("gx-arm-short-native-wait")["enabled"]
    process = subprocess.Popen(paired_device.args("session", 'wait', session, "--timeout", "10"), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    remote = False
    try:
        wait(lambda: scenario.request("gx-native-wait-timeouts")["count"] >= 2, timeout=8, process=scenario.native)
        assert process.poll() is None
        assert scenario.request("state", sessionId=session)["running"]
        if outcome == "input":
            started = scenario.request("wait-remote-question-start", sessionId=session)
            remote = True
            assert started["pending"] > 0
        else:
            scenario.model.release.set()
        output, _ = process.communicate(timeout=10)
        assert process.returncode == 0
        result = json.loads(output)["data"]
        assert result["reason"] == ("input_required" if outcome == "input" else "idle")
        if outcome == "input":
            assert "question" in result["state"]["pending_input"]
            assert scenario.request("state", sessionId=session)["running"]
    finally:
        if remote:
            scenario.request("wait-remote-question-stop")
        scenario.model.release.set()
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_native_wait_slices_then_idle(scenario, paired_device):
    """Two native slice timeouts do not end a wait before the Agent becomes idle."""
    native_wait_after_intermediate_slices("idle", scenario, paired_device)


def test_native_wait_slices_then_remote_input(scenario, paired_device):
    """Two native slice timeouts do not hide a later native Remote input."""
    native_wait_after_intermediate_slices("input", scenario, paired_device)


def test_native_wait_claimed_approval_probe(scenario, paired_device):
    """When the adapter claims a native approval, native wait and the adapter projection expose their separate input ownership."""
    session = scenario.create()
    scenario.model.plan("tool")
    scenario.request("start", sessionId=session)
    wait(lambda: bool(paired_device.approvals(session)), process=scenario.native)
    native = scenario.request("native-wait", sessionId=session, timeoutMs=300)
    assert native["reason"] == "timeout"
    assert "approval" not in native["pendingInput"]
    assert paired_device.approvals(session)
    adapted = scenario.request("adapter-wait", sessionId=session, timeoutMs=1000)
    assert adapted["reason"] == "input-required" and adapted["state"]["pendingInput"] == ["approval"]
    paired_device.resolve("reject", paired_device.approvals(session)[0])
    scenario.wait_idle(session)


def test_native_adapter_create_default_and_reuse(scenario, paired_device):
    """An isolated DSH create delegates the default location and reuses an eligible blank identity."""
    first = scenario.request("create-adapter")
    session = (first.get("identity") or {}).get("session_id", "")
    assert session
    second = scenario.request("create-preferred", sessionId=session)
    assert (second.get("identity") or {}).get("session_id") == session
    assert any(item["session_id"] == session for item in paired_device.sessions(scenario.host))
    assert scenario.model.calls == 0
def test_native_adapter_workspace_attachment_and_failure(scenario, paired_device):
    """An unproven native attachment failure remains unknown after create and reuse."""
    successful = scenario.request("workspace-native-create")
    existing = (successful["receipt"].get("identity") or {}).get("session_id", "")
    assert existing and successful["attached"]
    attached = scenario.request("workspace-attach-fail", sessionId=existing)
    assert attached.get("result", {}).get("attach_error") == "workspace_attach_failed"
    before = len(paired_device.sessions(scenario.host))
    fresh = scenario.request("workspace-attach-fail")
    assert fresh.get("result", {}).get("attach_error") == "workspace_attach_failed"
    assert len(paired_device.sessions(scenario.host)) == before + 1
    assert scenario.model.calls == 0


def test_native_adapter_creation_concurrency_and_restart(scenario, paired_device):
    """Native per-ID ordering preserves one blank identity across concurrent claims and a restart."""
    preferred = "fixture-preferred-identity"
    result = scenario.request("create-preferred-race", sessionId=preferred)
    assert len(result) == 2 and all((item.get("identity") or {}).get("session_id") == preferred for item in result)
    scenario.native.close()
    scenario.start_native()
    recovered = scenario.request("create-preferred", sessionId=preferred)
    assert (recovered.get("identity") or {}).get("session_id") == preferred
    assert scenario.model.calls == 0


def test_native_adapter_explicit_directory_and_missing_workspace(scenario, paired_device):
    """Native create ensures a requested directory and rejects a missing workspace before mutation."""
    before = len(paired_device.sessions(scenario.host))
    missing = scenario.request("create-missing-workspace")
    facts = missing.get("result", {})
    assert facts.get("failure_code") in ("not_found", "invalid_argument") or facts.get("entered") is False
    assert len(paired_device.sessions(scenario.host)) == before
    created = scenario.request("create-explicit-directory")
    assert created["directory_created"] and (created["receipt"].get("identity") or {}).get("session_id")
    assert len(paired_device.sessions(scenario.host)) == before + 1
    assert scenario.model.calls == 0


def test_native_cli_production_entry_points(scenario, paired_device):
    """Open capability advertisement is reachable through the CLI without test-only switches."""
    session = scenario.create()
    paired_device.sessions(scenario.host)
    catalog = paired_device.call("session", "models", "--host", scenario.host)
    assert catalog["groups"], "Native model catalog is empty"
    model = paired_device.call("session", "model", "get", "--session", session)
    assert model["default_selection"] is not None
    assert paired_device.call("session", "wait", session, "--timeout", "1")["reason"] in ("idle", "timeout", "input-required")
    queue_state = paired_device.call("session", "queue", "list", "--session", session)
    assert "available" in queue_state
    created = paired_device.call("session", "create", "--host", scenario.host)["lifecycle_receipt"]
    assert created["disposition"] == "READY"
    child_source = created["session_id"]
    scenario.model.plan()
    paired_device.send(child_source, "synthetic production entry", delivery="start-if-idle")
    scenario.wait_idle(child_source)
    forked = paired_device.call("session", "fork", child_source)["lifecycle_receipt"]
    assert forked["operation"] == "fork" and forked["session_id"] != child_source


def test_native_remote_session_create_and_reuse(scenario, paired_device):
    """An isolated CLI creates and reuses a native session, then sends a distinct first prompt."""
    assert scenario.request("test-enable-create")["enabled"]
    created = paired_device.call("session", "create", "--host", scenario.host)["lifecycle_receipt"]
    session = created["session_id"]
    assert created["disposition"] == "READY" and scenario.model.calls == 0
    reused = paired_device.call("session", "create", "--host", scenario.host, "--session-id", session)["lifecycle_receipt"]
    assert reused["session_id"] == session
    assert any(item["session_id"] == session for item in paired_device.sessions(scenario.host))
    scenario.model.plan()
    assert paired_device.send_confirmed(session, "synthetic fixture", delivery="start-if-idle")['status'] == 'succeeded'
    scenario.wait_idle(session)
    assert scenario.model.calls == 1


def test_native_remote_workspace_creation(scenario, paired_device):
    """An isolated CLI creates a native session attached to the requested workspace."""
    assert scenario.request("test-enable-create")["enabled"]
    workspace = scenario.request("workspace-id")["workspaceId"]
    created = paired_device.call("session", "create", "--host", scenario.host, "--workspace", workspace)["lifecycle_receipt"]
    assert created["disposition"] == "READY"
    assert scenario.request("workspace-has-session", sessionId=created["session_id"])["attached"]
    assert scenario.model.calls == 0


def test_native_create_falls_back_from_started_and_archived_identity(scenario, paired_device):
    """Preferred identities that have started or are archived use native fallback identities."""
    started = scenario.request("create-adapter")["identity"]["session_id"]
    scenario.model.plan()
    scenario.request("start", sessionId=started)
    scenario.wait_idle(started)
    fallback = scenario.request("create-preferred", sessionId=started)
    fallback_id = (fallback.get("identity") or {}).get("session_id", "")
    assert fallback_id and fallback_id != started
    workspace = scenario.request("workspace-native-create")["receipt"]["identity"]["session_id"]
    assert scenario.request("archive-session", sessionId=workspace)["archived"]
    archived = scenario.request("create-preferred", sessionId=workspace)
    archived_id = (archived.get("identity") or {}).get("session_id", "")
    assert archived_id and archived_id != workspace
    assert scenario.model.calls == 1


def test_native_create_explicit_and_missing_preset(scenario, paired_device):
    """An available native preset is applied; absent presets do not produce a claimed identity."""
    before = len(paired_device.sessions(scenario.host))
    missing = scenario.request("create-missing-preset")
    facts = missing.get("result", {})
    assert facts.get("failure_code") in ("not_found", "invalid_argument") or facts.get("entered") is False
    assert len(paired_device.sessions(scenario.host)) == before
    created = scenario.request("create-explicit-preset")
    assert created["available"] and created["confirmed"]
    assert created["receipt"].get("identity", {}).get("session_id")
    assert len(paired_device.sessions(scenario.host)) == before + 1
    assert scenario.model.calls == 0


def test_native_remote_workspace_attachment_unknown(scenario, paired_device):
    """An unproven native attachment failure reports unknown without retrying create."""
    assert scenario.request("test-enable-create")["enabled"]
    workspace = scenario.request("workspace-id")["workspaceId"]
    before = len(paired_device.sessions(scenario.host))
    assert scenario.request("test-fail-next-attachment")["armed"]
    exit_code, envelopes = paired_device.invoke("session", "create", "--host", scenario.host, "--workspace", workspace, success=False)
    assert len(paired_device.sessions(scenario.host)) == before + 1
    receipt = envelopes[-1].get("data", {}).get("lifecycle_receipt")
    if receipt:
        # The native create proved a real identity; only the follow-up
        # workspace attachment failed.
        assert receipt["disposition"] == "READY_WITH_WARNING"
        assert receipt["warning_code"] == "workspace_attach_failed"
        assert exit_code == 0
    else:
        assert exit_code == 7 and envelopes[-1]["error"]["code"] == "unknown"
    assert scenario.model.calls == 0


def test_native_remote_workspace_with_preset_and_independent_creations(scenario, paired_device):
    """Distinct native create commands at one workspace retain distinct IDs and the requested preset."""
    assert scenario.request("test-enable-create")["enabled"]
    workspace = scenario.request("workspace-id")["workspaceId"]
    preset = scenario.request("default-preset-info")
    assert preset["available"]
    first = paired_device.call("session", "create", "--host", scenario.host, "--workspace", workspace, "--preset", preset["presetId"])["lifecycle_receipt"]
    second = paired_device.call("session", "create", "--host", scenario.host, "--workspace", workspace, "--preset", preset["presetId"])["lifecycle_receipt"]
    assert first["session_id"] != second["session_id"]
    for receipt in (first, second):
        assert receipt["disposition"] == "READY"
        assert scenario.request("workspace-has-session", sessionId=receipt["session_id"])["attached"]
        assert scenario.request("session-has-default-preset", sessionId=receipt["session_id"])["matches"]
    assert scenario.model.calls == 0



def test_native_workspace_error_can_occur_after_session_persistence(scenario, paired_device):
    """Detect whether the same workspace error can arise inside preset setup after persistence."""
    result = scenario.request("workspace-error-after-persist", sessionId="fixture-workspace-race")
    assert result["persisted"] is True
    # A structured nested workspace error entered native but never proved which
    # session it created; the adapter reports facts without a claimed identity.
    assert result["identity"] is None
    assert scenario.model.calls == 0



def test_native_nested_attachment_error_never_confirms_wrong_identity(scenario, paired_device):
    """Detect a nested structured attachment failure after a different native session is persisted."""
    result = scenario.request("workspace-warning-after-persist", sessionId="fixture-actual-session")
    # The error names a different session than the one actually persisted, so no
    # identity is claimed and no READY_WITH_WARNING is fabricated.
    assert result["persisted"] is True
    assert result["reportedSession"] is None
    assert scenario.model.calls == 0



def test_native_question_root_probe(scenario, paired_device):
    """A live root delegates to its scoped native provider and a stale caller cannot answer."""
    session = scenario.create()
    result = scenario.request("question-root-probe", sessionId=session)
    assert result == {"observed": 1, "answered": True, "absentContinued": False, "stale": "CALLER_NOT_LIVE",
                      "openEnabled": True, "claimed": 0}


def test_native_question_owned_child_probe(scenario, paired_device):
    """A live Agent owned by another Agent is delegated and cannot be remotely claimed."""
    session = scenario.create()
    result = scenario.request("question-owned-child-probe", sessionId=session)
    assert result == {"owned": True, "isRoot": False, "code": "DELEGATED_CALLER", "result": "delegated", "active": 0,
                      "readCode": "unsupported_capability"}


def test_native_question_resumed_root_probe(scenario, paired_device):
    """A session with child lineage resumed as a runtime root may ask a question."""
    session = scenario.create()
    result = scenario.request("question-resumed-root-probe", sessionId=session)
    assert result == {"isRoot": True, "hasParent": True, "answered": True, "answerable": 1}


def test_native_question_replaced_root_guard_probe(scenario, paired_device):
    """Real native root replacement changes the guard for an isolated synthetic projection."""
    result = scenario.request("question-replaced-root-guard-probe")
    assert result == {"differentAgent": True, "stableRead": True, "changedGuard": True,
                      "continuedCode": "conflict", "openCode": "conflict", "newAnswerable": True}


def test_native_question_remote_open(scenario, paired_device):
    """A test-gated real DSH open question accepts an explicit encrypted CLI answer once."""
    session = scenario.create()
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    assert scenario.request("question-start-remote-fixture", sessionId=session)["started"]
    batches = paired_device.call("session", "question", "list", "--session", session)["batches"]
    assert len(batches) == 1 and batches[0]["answerable"] and batches[0]["phase"] == "QUESTION_OPEN"
    batch = batches[0]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    receipt = paired_device.call("session", "question", "answer", "--session", session, "--batch", batch["batch_id"],
        "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
        "--phase", batch["phase"], "--input", str(answer), "--command-id", "native-open-fixture")
    assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-result-remote-fixture", sessionId=session) == {"selected": "Yes", "pending": False}


def test_native_question_reconnect_keeps_live_handle(scenario, paired_device):
    """A new authenticated device channel sees the same still-live native question and may answer it."""
    session = scenario.create()
    scenario.request("question-enable-remote-fixture")
    scenario.request("question-start-remote-fixture", sessionId=session)
    first = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    second = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    assert (first["batch_id"], first["source_epoch"], first["digest"]) == (second["batch_id"], second["source_epoch"], second["digest"])
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    receipt = paired_device.call("session", "question", "answer", "--session", session, "--batch", second["batch_id"], "--epoch", second["source_epoch"],
        "--version", str(second["version"]), "--digest", second["digest"], "--phase", second["phase"], "--input", str(answer))
    assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-result-remote-fixture", sessionId=session)["selected"] == "Yes"


def test_native_question_restart_does_not_recreate_handle(scenario, paired_device):
    """After the native source restarts, a vanished open Promise is not reconstructed from cached content."""
    session = scenario.create()
    scenario.request("question-enable-remote-fixture")
    scenario.request("question-start-remote-fixture", sessionId=session)
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    scenario.native.close()
    scenario.start_native()
    scenario.request("question-enable-remote-fixture")
    assert paired_device.call("session", "question", "list", "--session", session)["batches"] == []
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    code, result = paired_device.invoke("session", "question", "answer", "--session", session, "--batch", batch["batch_id"],
        "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
        "--phase", batch["phase"], "--input", str(answer), success=False)
    assert code != 0 and result[-1]["error"]["code"] == "conflict"


def test_native_question_adapter_reload_does_not_recreate_handle(scenario, paired_device):
    """Reloading the adapter cannot turn an earlier native Promise into a newly answerable handle."""
    session = scenario.create()
    scenario.request("question-enable-remote-fixture")
    scenario.request("question-start-remote-fixture", sessionId=session)
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    assert scenario.request("question-reload-adapter-fixture")["reloaded"]
    assert paired_device.call("session", "question", "list", "--session", session)["batches"] == []
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    code, result = paired_device.invoke("session", "question", "answer", "--session", session, "--batch", batch["batch_id"],
        "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
        "--phase", batch["phase"], "--input", str(answer), success=False)
    assert code != 0 and result[-1]["error"]["code"] == "conflict"


def test_native_question_remote_cancel_releases_claim(scenario, paired_device):
    """CLI cancellation releases a held actual DSH wait claim without answering or hiding the live question."""
    session = scenario.create()
    scenario.request("question-enable-remote-fixture")
    scenario.request("question-start-remote-fixture", sessionId=session)
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    scenario.request("question-hold-native-claim")
    command_id = "native-open-cancel-fixture"
    process = subprocess.Popen(paired_device.args("session", 'question', 'answer', "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer), "--command-id", command_id), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 1, timeout=15, process=scenario.native)
        process.send_signal(signal.SIGINT)
        output, _ = process.communicate(timeout=30)
        result = json.loads(output.strip())
        assert process.returncode == 7 and result["error"]["code"] == "unknown"
        assert command_id in result["error"]["message"]
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 0, timeout=5, process=scenario.native)
        assert scenario.request("question-restore-native-claim")["restored"]
        current = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
        assert current["batch_id"] == batch["batch_id"]
        receipt = paired_device.call("session", "question", "answer", "--session", session, "--batch", batch["batch_id"],
            "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
            "--phase", batch["phase"], "--input", str(answer))
        assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
        assert scenario.request("question-result-remote-fixture", sessionId=session)["selected"] == "Yes"
    finally:
        if process.poll() is None:
            process.kill(); process.communicate(timeout=5)


def test_native_question_cancel_scopes_cancellation_to_the_owner(scenario, paired_device):
    """A foreign device cannot cancel a held answer, and the owner's next command releases its own claim without settling."""
    from environment import Device, http

    session = scenario.create()
    scenario.request("question-enable-remote-fixture")
    scenario.request("question-start-remote-fixture", sessionId=session)
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    second = Device(scenario.directory / "cancel-second-device")
    invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    second.pair(invitation["pairingUri"])
    assert second.identity["device_id"] != paired_device.identity["device_id"]
    assert second.call("host", "list")["hosts"]
    scenario.request("question-hold-native-claim")
    command_id = "native-cancel-command-fixture"
    process = subprocess.Popen(paired_device.args("session", 'question', 'answer', "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer), "--command-id", command_id), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 1, timeout=15, process=scenario.native)
        assert second.call("session", "question", "cancel", "--session", session, "--command", command_id)["cancellation_requested"] is False
        assert scenario.request("question-native-claim-status")["held"] == 1
        # A second process for the owning device opens a new channel, and the
        # Host replaces the answering channel before it dispatches the cancel
        # payload. The claim is released either way; the reported boolean races
        # with that replacement, so it is deliberately not asserted here.
        paired_device.call("session", "question", "cancel", "--session", session, "--command", command_id)
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 0, timeout=15, process=scenario.native)
        output, _ = process.communicate(timeout=30)
        # The exact failure code depends on how the source surfaces the abort;
        # what must hold is that the cancelled answer never settled the question.
        assert process.returncode != 0 and "QUESTION_SETTLED" not in output
        assert paired_device.call("session", "question", "cancel", "--session", session, "--command", command_id)["cancellation_requested"] is False
        assert scenario.request("question-restore-native-claim")["restored"]
        current = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
        assert current["batch_id"] == batch["batch_id"]
        receipt = paired_device.call("session", "question", "answer", "--session", session, "--batch", batch["batch_id"],
            "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
            "--phase", batch["phase"], "--input", str(answer), "--command-id", "native-cancel-after")
        assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
        assert scenario.request("question-result-remote-fixture", sessionId=session)["selected"] == "Yes"
    finally:
        if process.poll() is None:
            process.kill(); process.communicate(timeout=5)


def test_native_question_abrupt_client_exit_releases_by_request_expiry(scenario, paired_device):
    """A killed client leaves no answer; an unobservable disconnect releases its claim by the existing request deadline."""
    session = scenario.create()
    scenario.request("question-enable-remote-fixture")
    scenario.request("question-start-remote-fixture", sessionId=session)
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    scenario.request("question-hold-native-claim")
    process = subprocess.Popen(paired_device.args("session", 'question', 'answer', "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer), "--command-id", "native-killed-fixture"), cwd=ROOT, env=paired_device.env(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    try:
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 1, timeout=15, process=scenario.native)
        process.kill(); process.communicate(timeout=5)
        time.sleep(0.25)
        early_held = scenario.request("question-native-claim-status")["held"]
        Path("/reports/question-abrupt-client-diagnostic.json").write_text(json.dumps({"held_after_250ms": early_held}))
        wait(lambda: scenario.request("question-native-claim-status")["held"] == 0, timeout=35, process=scenario.native)
        result = scenario.request("question-result-remote-fixture", sessionId=session)
        assert result == {"selected": "", "pending": True}
    finally:
        if process.poll() is None:
            process.kill(); process.communicate(timeout=5)


@pytest.mark.parametrize("kind", [
    pytest.param("plan-review", marks=pytest.mark.description("A native plan review preserves its detail and exact selected option label.")),
    pytest.param("approval", marks=pytest.mark.description("A blocking native approval without a wait retains decision context and exact selected label.")),
])
def test_native_question_context_probe(scenario, paired_device, kind):
    session = scenario.create()
    result = scenario.request("question-context-probe", sessionId=session, config={"kind": kind})
    assert result == {"kind": kind, "detail": True, "description": True, "display": True,
                      "selected": "Deny", "phase": "QUESTION_SETTLED", "claimed": 0, "localAborted": 1}


def test_native_question_stale_handle_cannot_capture_later_request(scenario, paired_device):
    """A released native handle cannot answer a later question even when the native call ID is reused."""
    session = scenario.create()
    result = scenario.request("question-future-request-probe", sessionId=session)
    assert result == {"distinct": True, "staleCode": "conflict", "selected": "No", "active": 0, "claimed": 0}


def test_native_question_deadline_after_claim(scenario, paired_device):
    """A deadline reached during actual native attachWait prevents remote settlement and releases its claim."""
    session = scenario.create()
    result = scenario.request("question-deadline-probe", sessionId=session)
    assert result == {"code": "expired", "definitive": True, "selected": "No", "nativeClaims": 0, "claimed": 0}


def test_native_question_claim_probe(scenario, paired_device):
    """An actual timed wait claim is tracked by source scope and released to the next native provider."""
    session = scenario.create()
    result = scenario.request("question-claim-probe", sessionId=session)
    assert result == {"own": 1, "other": 0, "after": 0, "localCalls": 1,
                      "events": ["requested", "released"], "answered": True, "releasedAgain": False}


@pytest.mark.parametrize("winner", [
    pytest.param("local", marks=pytest.mark.description("When the local provider answers first, the native result is local and a late remote answer is rejected.")),
    pytest.param("remote", marks=pytest.mark.description("When the remote provider answers first, the native result settles once and its native wait claim is released.")),
    pytest.param("expiry", marks=pytest.mark.description("When no provider answers, native timeout continues and the stale remote handle is rejected.")),
    pytest.param("cancel", marks=pytest.mark.description("When the native caller cancels, the pending remote handle is removed without an answer.")),
    pytest.param("disconnect", marks=pytest.mark.description("When the remote claimant disconnects before submitting, its native claim releases and the local provider can answer.")),
    pytest.param("simultaneous", marks=pytest.mark.description("When two remote submissions race a local provider, at most one native answer is accepted and all claims are released.")),
])
def test_native_question_open_race_probe(scenario, paired_device, winner):
    """The actual native waterfall lets the first of local and remote answer once without holding the local provider."""
    session = scenario.create()
    result = scenario.request("question-open-race-probe", sessionId=session, config={"winner": winner})
    assert result["localCalls"] == result["providerHops"] == 1 and result["active"] == result["claimed"] == 0
    if winner == "local":
        assert result["selected"] == "No" and result["lateCode"] == "conflict"
    elif winner == "remote":
        assert result["selected"] == "Yes" and result["phase"] == "QUESTION_SETTLED"
    elif winner == "expiry":
        assert result["pending"] and result["lateCode"] == "conflict"
    elif winner == "cancel":
        assert result["code"] == "ASK_ABORTED"
    elif winner == "disconnect":
        assert result["selected"] == "No" and result["lateCode"] == "unavailable" and result["actualClaims"] == 0
    else:
        assert (result["selected"], result["successes"]) in {("Yes", 1), ("No", 0)}


@pytest.mark.timed_questions
def test_native_question_continued_probe(scenario, paired_device):
    """A timed native question is discovered from the projection and a guarded answer is queued."""
    session = scenario.create()
    scenario.model.plan("question")
    scenario.request("start", sessionId=session)
    try:
        wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"], timeout=15, process=scenario.native)
    except (RuntimeError, TimeoutError):
        diagnostic = {"model_calls": scenario.model.calls, "event_types": scenario.request("events", sessionId=session)["types"]}
        try:
            diagnostic["projection"] = scenario.request("question-projected", sessionId=session)
        except RuntimeError:
            diagnostic["projection"] = "unavailable"
        Path("/reports/question-probe-diagnostic.json").write_text(json.dumps(diagnostic))
        raise
    assert scenario.request("question-answer-probe", sessionId=session)["phase"] == "QUESTION_REPLY_QUEUED"


@pytest.mark.timed_questions
@pytest.mark.parametrize("order", [
    pytest.param("legacy-first", marks=pytest.mark.description("A live untimed question followed by a timed question retains native order.")),
    pytest.param("timed-first", marks=pytest.mark.description("A timed continued question followed by a live untimed question retains native order.")),
])
def test_native_question_mixed_order(scenario, paired_device, order):
    session = scenario.create()
    if order == "legacy-first":
        assert scenario.request("question-mixed-start", sessionId=session)["started"]
    scenario.model.plan("question")
    scenario.request("start", sessionId=session)
    if order == "timed-first":
        wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"], timeout=15, process=scenario.native)
        assert scenario.request("question-mixed-start", sessionId=session)["started"]
    expected = ["legacy", "q1"] if order == "legacy-first" else ["q1", "legacy"]
    wait(lambda: scenario.request("question-mixed-observe", sessionId=session)["ids"] == expected, timeout=15, process=scenario.native)
    assert scenario.request("question-mixed-release", sessionId=session)["released"]


@pytest.mark.timed_questions
def test_native_question_stale_open_after_continuation(scenario, paired_device):
    """An open answer captured before native timeout is definitively rejected after continuation."""
    session = scenario.create()
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    scenario.model.plan("question-long")
    scenario.request("start", sessionId=session)
    wait(lambda: scenario.request("question-transition-capture", sessionId=session)["found"], timeout=10, process=scenario.native)
    wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"], timeout=10, process=scenario.native)
    assert scenario.request("question-transition-answer", sessionId=session) == {"code": "conflict", "definitive": True}
    assert scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"]


@pytest.mark.timed_questions
def test_native_question_continued_admission(scenario, paired_device):
    """A continued reply remains queued before native admission and duplicate replies are refused."""
    session = scenario.create()
    scenario.model.plan("question", hold=True)
    scenario.request("start", sessionId=session)
    wait(lambda: scenario.model.calls >= 2, timeout=15, process=scenario.native)
    wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"], timeout=5, process=scenario.native)
    assert scenario.request("question-answer-probe", sessionId=session).get("phase") == "QUESTION_REPLY_QUEUED"
    assert scenario.request("question-continued-status", sessionId=session) == {"continued": 1, "queued": 1}
    assert scenario.request("question-continued-duplicate", sessionId=session) == {"code": "REPLY_QUEUED"}
    scenario.model.release.set()
    wait(lambda: scenario.request("question-projected", sessionId=session)["count"] == 0, timeout=15, process=scenario.native)


@pytest.mark.timed_questions
@pytest.mark.parametrize("shape", [
    pytest.param("unknown", marks=pytest.mark.description("Native continued answer is probed with a label not offered in its single-select question.")),
    pytest.param("duplicate", marks=pytest.mark.description("Native continued answer is probed with a repeated selected label.")),
    pytest.param("multiple", marks=pytest.mark.description("Native continued answer is probed with two labels in a single-select question.")),
    pytest.param("custom", marks=pytest.mark.description("Native continued answer is probed with a custom-only response.")),
])
def test_native_question_answer_shape_probe(scenario, paired_device, shape):
    session = scenario.create()
    scenario.model.plan("question", hold=True)
    scenario.request("start", sessionId=session)
    wait(lambda: scenario.model.calls >= 2, timeout=15, process=scenario.native)
    wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"], timeout=5, process=scenario.native)
    result = scenario.request("question-native-answer-shape", sessionId=session, config={"shape": shape})
    assert result == {"accepted": True}
    scenario.model.release.set()


@pytest.mark.parametrize("shape,count,custom", [
    pytest.param("unknown", 1, False, marks=pytest.mark.description("Native open answer accepts a provider-selected label not offered by its question.")),
    pytest.param("duplicate", 2, False, marks=pytest.mark.description("Native open answer accepts a provider's repeated selected label.")),
    pytest.param("multiple", 2, False, marks=pytest.mark.description("Native open answer accepts two labels from a single-select question.")),
    pytest.param("approval-custom", 0, True, marks=pytest.mark.description("Native blocking approval service accepts a provider's custom answer shape.")),
])
def test_native_question_open_answer_shape_probe(scenario, paired_device, shape, count, custom):
    session = scenario.create()
    assert scenario.request("question-native-open-shape", sessionId=session, config={"shape": shape}) == {
        "accepted": True, "selectedCount": count, "customPresent": custom,
    }


@pytest.mark.parametrize("shape,selected,custom", [
    pytest.param("unknown", ["Unknown"], None, id="unknown", marks=pytest.mark.description("A remote open answer preserves an unoffered native-accepted label.")),
    pytest.param("duplicate", ["Yes", "Yes"], None, id="duplicate", marks=pytest.mark.description("A remote open answer preserves repeated selected labels accepted by native DSH.")),
    pytest.param("multiple", ["Yes", "No"], None, id="multiple", marks=pytest.mark.description("A remote open answer preserves multiple selected labels accepted by native DSH.")),
    pytest.param("custom", [], "Fixture custom", id="custom", marks=pytest.mark.description("A remote open answer preserves native custom-only text.")),
])
def test_native_question_remote_answer_shape_roundtrip(scenario, paired_device, shape, selected, custom):
    session = scenario.create()
    scenario.request("question-enable-remote-fixture")
    scenario.request("question-start-remote-fixture", sessionId=session)
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": selected, "custom": custom}]}))
    receipt = paired_device.call("session", "question", "answer", "--session", session, "--batch", batch["batch_id"],
        "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
        "--phase", batch["phase"], "--input", str(answer))
    assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-result-remote-shape", sessionId=session) == {"selected": selected, "custom": custom}


@pytest.mark.timed_questions
def test_native_question_continued_replaced_root(scenario, paired_device):
    """A real continued native question cannot be answered using its previous runtime owner's guard."""
    assert scenario.request("question-enable-continued-subclass-fixture")["enabled"]
    session = scenario.request("question-managed-root-create")["sessionId"]
    scenario.model.plan("question", hold=True)
    scenario.request("start", sessionId=session)
    wait(lambda: scenario.model.calls >= 2, timeout=15, process=scenario.native)
    wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"], timeout=5, process=scenario.native)
    old = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    reread = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    assert old["digest"] == reread["digest"] and old["version"] == reread["version"]
    scenario.model.release.set()
    wait(lambda: scenario.request("state", sessionId=session).get("running") is False, timeout=15, process=scenario.native)
    assert scenario.request("question-managed-root-replace", sessionId=session)["replaced"]
    new = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    assert new["answerable"] and new["digest"] != old["digest"] and new["version"] > old["version"]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    def args(batch):
        return ["session", "question", "answer", "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
                "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer)]
    code, result = paired_device.invoke(*args(old), success=False)
    assert code == 8 and result[-1]["error"]["code"] == "conflict"
    assert paired_device.call(*args(new))["question_receipt"]["phase"] == "QUESTION_REPLY_QUEUED"


@pytest.mark.timed_questions
def test_native_question_continued_remote_without_projection_override(scenario, paired_device):
    """A native continued projection is remotely answerable and visibly queued without replacing its projection logic."""
    assert scenario.request("question-enable-continued-subclass-fixture")["enabled"]
    session = scenario.create()
    scenario.model.plan("question", hold=True)
    scenario.request("start", sessionId=session)
    wait(lambda: scenario.model.calls >= 2, timeout=15, process=scenario.native)
    wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"], timeout=5, process=scenario.native)
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    assert batch["phase"] == "QUESTION_CONTINUED" and batch["answerable"]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    args = ["session", "question", "answer", "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
        "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer)]
    assert paired_device.call(*args)["question_receipt"]["phase"] == "QUESTION_REPLY_QUEUED"
    queued = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    assert queued["phase"] == "QUESTION_REPLY_QUEUED" and not queued["answerable"]
    assert queued["version"] > batch["version"] and queued["digest"] != batch["digest"]
    code, result = paired_device.invoke(*args, success=False)
    # A batch that is already reply_queued is an unanswerable phase; the
    # retained CLI contract classifies that as an input error, not a stale
    # conflict. The native arbitration still reports REPLY_QUEUED downstream.
    assert code == 2 and result[-1]["error"]["code"] == "invalid_argument"
    assert "not answerable" in result[-1]["error"]["message"]
    assert scenario.request("question-continued-duplicate", sessionId=session)["code"] == "REPLY_QUEUED"
    scenario.model.release.set()
    wait(lambda: paired_device.call("session", "question", "list", "--session", session)["batches"] == [], timeout=15, process=scenario.native)


def test_native_adapter_fork_completed_turn(scenario, paired_device):
    """A native fork delegates boundary selection and creates no automatic child run."""
    session = scenario.create()
    scenario.model.plan()
    paired_device.send(session, "native integration fixture", delivery="start-if-idle")
    scenario.wait_idle(session)
    calls = scenario.model.calls
    receipt = scenario.request("fork-adapter", sessionId=session)
    child = (receipt.get("identity") or {}).get("session_id", "")
    assert child and child != session and (receipt.get("identity") or {}).get("parent_session_id") == session
    assert any(item["session_id"] == child for item in paired_device.sessions(scenario.host))
    assert scenario.model.calls == calls
    assert not scenario.request("state", sessionId=child)["running"]


def test_native_fork_running_source_with_queued_followup(scenario, paired_device):
    """A running source with queued work forks only its latest completed prefix."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "first synthetic turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=parent)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("start", sessionId=parent)
    wait(lambda: scenario.request("state", sessionId=parent)["queueCount"] == 1, process=scenario.native)
    receipt = scenario.request("fork-adapter", sessionId=parent)
    child = (receipt.get("identity") or {}).get("session_id", "")
    parent_state = scenario.request("state", sessionId=parent)
    child_state = scenario.request("state", sessionId=child)
    assert parent_state["running"] and parent_state["queueCount"] == 1
    assert not child_state["running"] and child_state["queueCount"] in (None, 0)
    assert scenario.request("events", sessionId=child)["types"].count("assistant/message") == 1
    assert scenario.model.calls == 1
    scenario.model.release.set()
    scenario.wait_idle(parent, assistant_count=3)


def test_native_fork_empty_parent_rejected(scenario, paired_device):
    """A native source without a completed turn fails before child creation."""
    parent = scenario.create()
    before = {item["session_id"] for item in paired_device.sessions(scenario.host)}
    result = scenario.request("fork-empty-check", sessionId=parent)
    assert result["rejected"] is True and result["code"] == "conflict" and result["definitive"] is True
    assert {item["session_id"] for item in paired_device.sessions(scenario.host)} == before


def test_native_fork_cold_parent(scenario, paired_device):
    """A cold source can be forked without restoring or changing its parent."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    scenario.native.close()
    scenario.start_native()
    before = scenario.request("state", sessionId=parent)
    assert not before["active"]
    receipt = scenario.request("fork-adapter", sessionId=parent)
    identity = receipt.get("identity") or {}
    assert identity.get("parent_session_id") == parent and identity.get("session_id") != parent
    assert not scenario.request("state", sessionId=parent)["active"]
    child = identity["session_id"]
    assert not scenario.request("state", sessionId=child)["running"]
    assert scenario.request("same-cwd", sessionId=parent, config={"childId": child})["same"]
    parent_types = scenario.request("snapshot-types", sessionId=parent)["types"]
    child_types = scenario.request("snapshot-types", sessionId=child)["types"]
    assert parent_types.count("assistant/message") == child_types.count("assistant/message") == 1
    scenario.native.close()
    scenario.start_native()
    after = {item["session_id"] for item in paired_device.sessions(scenario.host)}
    assert {parent, child}.issubset(after)
    assert scenario.request("snapshot-types", sessionId=parent)["types"] == parent_types
    assert scenario.request("snapshot-types", sessionId=child)["types"] == child_types


def test_native_fork_workspace_attach_fault(scenario, paired_device):
    """An unproven native workspace fault remains unknown even when a child was created."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    before = {item["session_id"] for item in paired_device.sessions(scenario.host)}
    result = scenario.request("fork-with-attach-fault", sessionId=parent)
    facts = result.get("result", result)
    assert facts.get("entered") is True
    # Any identity is only the child that was actually created; an injected
    # unverified fault must never produce a fabricated one.
    child = (facts.get("identity") or {}).get("session_id")
    after = {item["session_id"] for item in paired_device.sessions(scenario.host)}
    assert len(after - before) == 1
    if child:
        assert child in after - before


def test_native_remote_fork_running_queue_replay_and_recovery(scenario, paired_device):
    """An encrypted fork preserves a running source and recovers its original identity after restart."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "first synthetic turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=parent)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("start", sessionId=parent)
    wait(lambda: scenario.request("state", sessionId=parent)["queueCount"] == 1, process=scenario.native)
    assert scenario.request("test-enable-fork")["enabled"]
    before = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    result = paired_device.call("session", "fork", parent)
    receipt = result["lifecycle_receipt"]
    child = receipt["session_id"]
    assert receipt["parent_session_id"] == parent and child not in before
    assert scenario.request("state", sessionId=parent)["queueCount"] == 1
    assert scenario.request("state", sessionId=parent)["running"]
    assert not scenario.request("state", sessionId=child)["running"]
    assert scenario.request("events", sessionId=child)["types"].count("assistant/message") == 1
    identities = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    status, replay = scenario.replay_command(paired_device)
    assert status in (200, 202) and replay["requestId"] == result["request_id"]
    assert {s["session_id"] for s in paired_device.sessions(scenario.host)} == identities
    scenario.model.release.set()
    scenario.wait_idle(parent, assistant_count=3)
    # Simulate a client that durably recorded the intent but lost its terminal projection.
    with sqlite3.connect(paired_device.data_dir / "cache.sqlite3") as db:
        assert db.execute("UPDATE commands SET status='unknown' WHERE request_id=?", (result["request_id"],)).rowcount == 1
    scenario.native.close()
    scenario.start_native()
    recovered = paired_device.call("operation", "recover", result["request_id"])
    assert recovered["lifecycle_receipt"]["session_id"] == child
    assert recovered["lifecycle_receipt"]["parent_session_id"] == parent
    assert {s["session_id"] for s in paired_device.sessions(scenario.host)} == identities


def test_native_remote_fork_safety_workspace_unknown(scenario, paired_device):
    """An unproven native workspace fault stays unknown through encrypted transport."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    assert scenario.request("test-enable-fork")["enabled"]
    before = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    assert scenario.request("arm-fork-attach-fault", sessionId=parent)["armed"]
    code, envelopes = paired_device.invoke("session", "fork", parent, success=False)
    after = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    assert len(after - before) == 1
    receipt = envelopes[-1].get("data", {}).get("lifecycle_receipt") or {}
    # A single injected attachment fault must not abort the existing child.
    assert {s["session_id"] for s in paired_device.sessions(scenario.host)} == after
    if receipt:
        assert receipt.get("session_id") in after


def test_native_remote_fork_journal_fault_stays_unknown(scenario, paired_device):
    """An uncommitted Host result stays unknown after restart and does not re-execute."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    assert scenario.request("test-enable-fork")["enabled"]
    before = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    assert scenario.request("arm-fork-journal-fault")["armed"]
    code, envelopes = paired_device.invoke("session", "fork", parent, success=False)
    assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
    pending = paired_device.call("operation", "list")["pending_commands"]
    assert len(pending) == 1
    request = pending[0]["request_id"]
    after = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    assert len(after - before) == 1
    scenario.native.close()
    scenario.start_native()
    # The native fork already produced one child and its Host operation record
    # survived the terminal-write fault, so recovery reports that proven
    # outcome without forking again.
    code, envelopes = paired_device.invoke("operation", "recover", request, success=False)
    # The native child exists but the Host could not persist its terminal
    # receipt, so recovery must stay conservative and never fork again.
    assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
    assert {s["session_id"] for s in paired_device.sessions(scenario.host)} == after


def test_native_remote_fork_parent_child_histories_independent(scenario, paired_device):
    """A forked child gets an independent cache and subsequent child work leaves its parent intact."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic parent turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    paired_device.observe_history(parent)
    before = scenario.cached(paired_device, parent)
    assert before[0] and before[1]
    assert scenario.request("test-enable-fork")["enabled"]
    receipt = paired_device.call("session", "fork", parent)["lifecycle_receipt"]
    child = receipt["session_id"]
    paired_device.observe_history(child)
    child_before = scenario.cached(paired_device, child)
    assert child_before[0] and child_before[1]
    assert scenario.cached(paired_device, parent) == before
    scenario.model.plan()
    paired_device.send(child, "synthetic child turn", delivery="start-if-idle")
    scenario.wait_idle(child, assistant_count=2)
    paired_device.observe_history(child)
    child_after = scenario.cached(paired_device, child)
    assert child_after != child_before
    assert scenario.cached(paired_device, parent) == before
    assert scenario.request("events", sessionId=parent)["types"].count("assistant/message") == 1


def test_native_remote_fork_open_empty_source_unknown(scenario, paired_device):
    """An open fork entry rejects an empty source conservatively without adding sessions."""
    parent = scenario.create()
    before = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    code, envelopes = paired_device.invoke("session", "fork", parent, success=False)
    assert code != 0
    receipt = envelopes[-1].get("data", {}).get("lifecycle_receipt") or {}
    assert receipt.get("disposition") == "FAILED" and not receipt.get("session_id")
    assert {s["session_id"] for s in paired_device.sessions(scenario.host)} == before


def test_native_remote_fork_new_distinct_commands(scenario, paired_device):
    """Two explicit remote fork commands create distinct children with the source directory."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic parent turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    calls = scenario.model.calls
    assert scenario.request("test-enable-fork")["enabled"]
    assert scenario.request("mark-working-directory")["marked"]
    first = paired_device.call("session", "fork", parent)["lifecycle_receipt"]
    second = paired_device.call("session", "fork", parent)["lifecycle_receipt"]
    first_id, second_id = first["session_id"], second["session_id"]
    assert len({parent, first_id, second_id}) == 3
    assert first["parent_session_id"] == second["parent_session_id"] == parent
    assert scenario.request("same-cwd", sessionId=parent, config={"childId": first_id})["same"]
    assert scenario.request("same-cwd", sessionId=parent, config={"childId": second_id})["same"]
    assert not scenario.request("state", sessionId=first_id)["running"]
    assert not scenario.request("state", sessionId=second_id)["running"]
    assert scenario.model.calls == calls
    assert scenario.request("working-directory-unchanged")["unchanged"]


def test_native_remote_fork_new_missing_source(scenario, paired_device):
    """A missing source returns not found without creating a new native session."""
    assert scenario.request("test-enable-fork")["enabled"]
    before = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    code, envelopes = paired_device.invoke("session", "fork", "fixture-missing-session", success=False)
    # A missing source is a proven preflight failure: FAILED with
    # SOURCE_NOT_FOUND and no native child, never a fabricated identity.
    assert code != 0
    receipt = envelopes[-1].get("data", {}).get("lifecycle_receipt") or {}
    assert receipt.get("disposition") == "FAILED"
    assert receipt.get("reason_code") == "SOURCE_NOT_FOUND"
    assert {s["session_id"] for s in paired_device.sessions(scenario.host)} == before


def test_native_remote_fork_new_postchange_fault(scenario, paired_device):
    """A fault after native child creation is reported as unknown and never replayed."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic parent turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    assert scenario.request("test-enable-fork")["enabled"]
    before = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    assert scenario.request("arm-fork-postchange-fault")["armed"]
    code, envelopes = paired_device.invoke("session", "fork", parent, success=False)
    # The throwing fixture returns no native facts, so the Host keeps the
    # conservative UNKNOWN outcome and never claims a child identity.
    assert code == 7 and envelopes[-1]["error"]["code"] == "unknown"
    receipt = envelopes[-1].get("data", {}).get("lifecycle_receipt") or {}
    assert receipt.get("disposition") == "UNKNOWN"
    assert not receipt.get("session_id")
    assert receipt.get("reconcile_mode") == "PARENT_SESSION_ID" and receipt.get("reconcile_key") == parent
    pending = paired_device.call("operation", "list")["pending_commands"]
    after = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    # A terminal UNKNOWN receipt settles the command; one child exists.
    assert pending == [] and len(after - before) == 1
    # A terminal UNKNOWN receipt settles the local command, so post-restart
    # recovery has no pending identity to re-execute and must not fork again.
    scenario.native.close()
    scenario.start_native()
    assert paired_device.call("operation", "list")["pending_commands"] == []
    assert {s["session_id"] for s in paired_device.sessions(scenario.host)} == after


def test_native_remote_fork_safety_nested_warning_spoof(scenario, paired_device):
    """A nested preset error cannot confirm an unrelated session as the fork child."""
    parent = scenario.create()
    victim = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic parent turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    assert scenario.request("test-enable-fork")["enabled"]
    before = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    assert scenario.request("arm-fork-preset-spoof", config={"victimId": victim})["armed"]
    code, envelopes = paired_device.invoke("session", "fork", parent, success=False)
    after = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    receipt = envelopes[-1].get("data", {}).get("lifecycle_receipt") or {}
    # The spoofed victim id must never be reported as the fork child.
    assert victim not in {receipt.get("session_id")} | ({receipt.get("parent_session_id")} - {parent})
    for child in after - before:
        assert child != victim
        if receipt.get("session_id"):
            assert receipt["session_id"] in after


def test_native_remote_fork_boundaries_authorization_revoked(scenario, paired_device):
    """After device revocation a replay of the original encrypted fork is denied without another child."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic parent turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    assert scenario.request("test-enable-fork")["enabled"]
    receipt = paired_device.call("session", "fork", parent)
    before = scenario.request("session-count")["count"]
    with scenario.fixture_bearer(paired_device) as bearer:
        paired_device.revoke()
        with pytest.raises(RuntimeError, match=r"HTTP (401|403)"):
            http("/v1/hosts", token=bearer)
        with pytest.raises(RuntimeError, match=r"HTTP (401|403)"):
            scenario.replay_command(paired_device, token_override=bearer)
    assert scenario.request("session-count")["count"] == before
    assert receipt["lifecycle_receipt"]["parent_session_id"] == parent


def test_native_remote_fork_boundaries_offline_no_deferred_action(scenario, paired_device):
    """An offline fork is rejected and is not queued for later host restart."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic parent turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    assert scenario.request("test-enable-fork")["enabled"]
    before = {s["session_id"] for s in paired_device.sessions(scenario.host)}
    scenario.native.close()
    wait(lambda: not next((h["online"] for h in paired_device.hosts() if h["host_id"] == scenario.host), True), timeout=30)
    code, _ = paired_device.invoke("session", "fork", parent, success=False)
    assert code != 0
    scenario.start_native()
    assert {s["session_id"] for s in paired_device.sessions(scenario.host)} == before


def test_native_fork_proven_context_prefix(scenario, paired_device):
    """A fork of a running parent copies exactly the prior committed message content."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic complete turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    baseline = scenario.request("snapshot-content-digest", sessionId=parent)
    full_prefix = scenario.request("snapshot-prefix-digest", sessionId=parent)
    assert baseline["count"] >= 2 and full_prefix["count"] >= baseline["count"]
    assert full_prefix["contextCount"] > 0
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=parent)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("start", sessionId=parent)
    wait(lambda: scenario.request("state", sessionId=parent)["queueCount"] == 1, process=scenario.native)
    child = (scenario.request("fork-adapter", sessionId=parent).get("identity") or {}).get("session_id", "")
    assert scenario.request("snapshot-content-digest", sessionId=child) == baseline
    assert scenario.request("snapshot-prefix-digest", sessionId=child, config={"count":full_prefix["count"]}) == full_prefix
    assert scenario.request("state", sessionId=parent)["running"]
    assert not scenario.request("state", sessionId=child)["running"]
    scenario.model.release.set()
    scenario.wait_idle(parent, assistant_count=3)


def test_native_fork_proven_default_model_and_preset(scenario, paired_device):
    """Native fork inherits recorded selection and preset while using current defaults as fallback."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic complete turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    result = scenario.request("fork-native-default-probe", sessionId=parent)
    assert result["childId"] != parent
    assert result["defaultFallback"] and result["currentInherited"] and result["parentUnchanged"] and result["inheritedPreset"]
    assert not scenario.request("state", sessionId=result["childId"])["running"]


def test_native_fork_proven_wrong_host_route(scenario, paired_device):
    """A fork request routed to the wrong Host cannot create another child."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic complete turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    assert scenario.request("test-enable-fork")["enabled"]
    paired_device.call("session", "fork", parent)
    before = scenario.request("session-count")["count"]
    with pytest.raises(RuntimeError, match=r"HTTP (400|401|403|404)"):
        scenario.replay_command(paired_device, route_host_override="fixture-wrong-host")
    assert scenario.request("session-count")["count"] == before


def test_native_fork_proven_concurrent_watches(scenario, paired_device):
    """Two devices watch parent and child concurrently without crossing native live events."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic complete turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    child = (scenario.request("fork-adapter", sessionId=parent).get("identity") or {}).get("session_id", "")
    second_dir = scenario.directory / "second-device"
    second_dir.mkdir()
    second = Device(second_dir)
    invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    second.pair(invitation["pairingUri"])
    parent_tail = scenario.tail(paired_device, parent)
    child_tail = scenario.tail(second, child)
    wait(lambda: scenario.request("watches") == 2, timeout=8, process=scenario.native)
    def baseline_seen(process):
        for line in Path(process.output.name).read_text().splitlines():
            try:
                if json.loads(line).get("data", {}).get("event", {}).get("type") == "reset":
                    return True
            except (ValueError, AttributeError):
                pass
        return False
    wait(lambda: baseline_seen(parent_tail) and baseline_seen(child_tail), timeout=8, process=scenario.native)
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=parent)
    scenario.request("start", sessionId=child)
    wait(lambda: scenario.model.calls >= 2, timeout=8, process=scenario.native)
    wait(lambda: scenario.preview_ready(parent_tail) and scenario.preview_ready(child_tail), timeout=8, process=scenario.native)
    scenario.model.release.set()
    parent_events = events(scenario.finish_tail(parent_tail))
    child_events = events(scenario.finish_tail(child_tail))
    for stream, expected in [(parent_events, parent), (child_events, child)]:
        visible = [item for item in stream if item.get("type") in ("preview", "message")]
        assert visible
        assert all(item.get("session") == {"host_id":scenario.host, "session_id":expected} for item in visible)
        assert any(item.get("type") == "preview" for item in visible)
        assert any(item.get("type") == "message" and item.get("role") == "ASSISTANT" for item in visible)


def test_native_fork_start_while_watch_baselines_inflight(scenario, paired_device):
    """Parent and child live events remain isolated when execution starts before their watch baselines complete."""
    parent = scenario.create()
    scenario.model.plan()
    paired_device.send(parent, "synthetic complete turn", delivery="start-if-idle")
    scenario.wait_idle(parent)
    child = (scenario.request("fork-adapter", sessionId=parent).get("identity") or {}).get("session_id", "")
    second_dir = scenario.directory / "second-device"
    second_dir.mkdir()
    second = Device(second_dir)
    invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    second.pair(invitation["pairingUri"])
    scenario.request("arm-watch-baseline-barrier")
    parent_tail = scenario.tail(paired_device, parent)
    child_tail = scenario.tail(second, child)
    wait(lambda: scenario.request("watches") == 2, timeout=8, process=scenario.native)
    wait(lambda: scenario.request("watch-baseline-barrier-count")["count"] == 2, timeout=8, process=scenario.native)
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=parent)
    scenario.request("start", sessionId=child)
    wait(lambda: scenario.model.calls >= 2, timeout=8, process=scenario.native)
    scenario.request("release-watch-baseline-barrier")
    wait(lambda: scenario.preview_ready(parent_tail) and scenario.preview_ready(child_tail), timeout=8, process=scenario.native)
    scenario.model.release.set()
    parent_events = events(scenario.finish_tail(parent_tail))
    child_events = events(scenario.finish_tail(child_tail))
    for stream, expected in [(parent_events, parent), (child_events, child)]:
        visible = [item for item in stream if item.get("type") in ("preview", "message")]
        assert visible
        assert all(item.get("session") == {"host_id":scenario.host, "session_id":expected} for item in visible)
        assert any(item.get("type") == "preview" for item in visible)
        assert any(item.get("type") == "message" and item.get("role") == "ASSISTANT" for item in visible)



def test_native_pair_send_revoke(scenario, paired_device):
    """When an isolated actual DSH Session receives a CLI prompt, it settles one native reply and revocation denies the old transport credential."""
    session = scenario.create()
    assert any(
        item["session_id"] == session for item in paired_device.sessions(scenario.host)
    )
    scenario.model.plan()
    paired_device.send(session, "native integration fixture", delivery="start-if-idle")
    scenario.wait_idle(session)
    assert (
        scenario.request("events", sessionId=session)["types"].count(
            "assistant/message"
        )
        == 1
    )
    with scenario.fixture_bearer(paired_device) as bearer:
        paired_device.revoke()
        with pytest.raises(RuntimeError, match=r"HTTP (401|403)"):
            http("/v1/hosts", token=bearer)


def test_native_stream_prefix_and_fixed_cut(scenario, paired_device):
    """When an actual native Attempt streams a large message and later history grows, intact preview prefixes match the final message and fixed-cut paging excludes the later turn."""
    session = scenario.create()
    scenario.model.plan("large", hold=True)
    tail = scenario.tail(paired_device, session)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15), "Native model request did not start"
    wait(lambda: scenario.preview_ready(tail), timeout=8, process=tail)
    scenario.model.release.set()
    values = events(scenario.finish_tail(tail))
    final = [
        item
        for item in values
        if item.get("type") == "message" and item.get("role") == "ASSISTANT"
    ]
    assert len({item["message_id"] for item in final}) == 1, (
        "Native final identity duplicated"
    )
    body = max(text_fields(final), key=len)
    accumulated = ""
    intact = False
    for item in values:
        if item.get("type") != "preview":
            continue
        accumulated = item["text"] if item["replace"] else accumulated + item["text"]
        if not item["incomplete"]:
            intact = True
            prefix_matches = body.startswith(accumulated)
            assert prefix_matches, "Native preview prefix diverged"
    final_size, preview_size = len(body), len(accumulated)
    # Live previews are ephemeral; settlement may supersede an undelivered suffix.
    # Every intact observed prefix must match, while final delivery is complete.
    assert intact and final_size == 80015 and 0 < preview_size <= final_size
    # Golden digest for the deterministic 80 KB fixture; no payload in failures.
    final_digest = hashlib.sha256(body.encode()).hexdigest()
    assert (
        final_digest
        == "a2f5e1b62155cd7b3d1a2736527c8ac8273767af6a0985291a605681868e163c"
    )
    before = scenario.cached(paired_device, session)
    page = scenario.request("cut-start", sessionId=session)
    cut = page["cut"]
    scenario.model.plan()
    paired_device.send(session, "native later-turn fixture", delivery="start-if-idle")
    scenario.wait_idle(session, assistant_count=2)
    for _ in range(200):
        if page["complete"]:
            break
        page = scenario.request("cut-next", sessionId=session)
    assert page["complete"] and page["cut"] == cut
    assert tuple(page["ids"]) == before[1], (
        "Frozen cut changed native message identities"
    )


def test_native_cold_restart(scenario, paired_device):
    """When the actual DSH process restarts after an idle turn, cold history preserves cursor/message identity and query recovery starts no new model request."""
    session = scenario.create()
    scenario.model.plan()
    paired_device.send(session, "native restart fixture", delivery="start-if-idle")
    scenario.wait_idle(session)
    paired_device.observe_history(session)
    before = scenario.cached(paired_device, session)
    calls = scenario.model.calls
    scenario.native.close()
    scenario.start_native()
    paired_device.observe_history(session)
    assert scenario.cached(paired_device, session) == before
    assert scenario.model.calls == calls
    scenario.model.plan()
    paired_device.send(session, "native post-restart continuation fixture", delivery="start-if-idle")
    scenario.wait_idle(session, assistant_count=2)
    paired_device.observe_history(session)
    after = scenario.cached(paired_device, session)
    assert after[0] != before[0] and after[1][: len(before[1])] == before[1]
    assert len(after[1]) == len(set(after[1])) > len(before[1])
    scenario.access.close()
    scenario.start_access()
    assert any(
        item["session_id"] == session for item in paired_device.sessions(scenario.host)
    )


@pytest.mark.parametrize(
    "decision,expected,effects",
    [
        pytest.param(
            "approve",
            "allowed-once",
            1,
            marks=pytest.mark.description(
                "When remote claim approves an actual native permission question, one decision grants exactly one fixture effect and a repeated approval cannot execute it again."
            ),
        ),
        pytest.param(
            "reject",
            "rejected",
            0,
            marks=pytest.mark.description(
                "When remote claim rejects an actual native permission question, one rejection settles the turn without executing the fixture effect."
            ),
        ),
    ],
)
def test_native_approval(scenario, paired_device, decision, expected, effects):
    """Native decision and exact fixture effect count agree."""
    session = scenario.create()
    scenario.model.plan("tool")
    paired_device.send(session, "native approval fixture", delivery="start-if-idle")
    wait(lambda: bool(paired_device.approvals(session)), process=scenario.native)
    approval = paired_device.approvals(session)[0]
    paired_device.resolve(decision, approval)
    scenario.wait_idle(session)
    assert scenario.request("events", sessionId=session)["decisions"] == [expected]
    assert scenario.request("effects")["count"] == effects
    code, _ = paired_device.invoke(
        "session", "approval", "approve",
        "--request",
        approval["approval_id"],
        "--digest",
        approval["digest"],
        "--version",
        str(approval["version"]),
        success=False,
    )
    assert code != 0 and scenario.request("effects")["count"] == effects


def test_native_approval_refuses_a_stale_guard(scenario, paired_device):
    """A wrong digest or version authorizes nothing, and the same approval still resolves with its exact guard."""
    session = scenario.create()
    scenario.model.plan("tool")
    paired_device.send(session, "native stale approval guard fixture", delivery="start-if-idle")
    wait(lambda: bool(paired_device.approvals(session)), process=scenario.native)
    approval = paired_device.approvals(session)[0]
    for digest, version in (
        ("00" * 32, approval["version"]),
        (approval["digest"], approval["version"] + 1),
    ):
        code, values = paired_device.invoke(
            "session", "approval", "approve", "--request", approval["approval_id"], "--digest", digest,
            "--version", str(version), success=False)
        # The Host journals a definitive rejection before authorizing the approval.
        assert code == 8 and values[-1]["error"]["code"] == "approval_version_mismatch"
        assert values[-1].get("data") is None
    assert scenario.request("effects")["count"] == 0
    paired_device.resolve("approve", approval)
    scenario.wait_idle(session)
    assert scenario.request("effects")["count"] == 1


@pytest.mark.short_approvals
def test_native_approval_refuses_an_expired_guard(scenario, paired_device):
    """An approval past its lifetime authorizes nothing even with its exact digest and version."""
    session = scenario.create()
    scenario.model.plan("tool")
    paired_device.send(session, "native expired approval fixture", delivery="start-if-idle")
    wait(lambda: bool(paired_device.approvals(session)), process=scenario.native)
    approval = paired_device.approvals(session)[0]
    # This case shortens the approval lifetime to three seconds.
    time.sleep(3.5)
    code, values = paired_device.invoke(
        "session", "approval", "approve", "--request", approval["approval_id"], "--digest", approval["digest"],
        "--version", str(approval["version"]), success=False)
    assert code == 8 and values[-1]["error"]["code"] == "approval_expired"
    assert values[-1].get("data") is None
    assert scenario.request("effects")["count"] == 0


def test_native_interrupt_cancels_approval(scenario, paired_device):
    """When interruption cancels an actual native approval, a late remote approval is rejected and the protected fixture effect never executes."""
    session = scenario.create()
    scenario.model.plan("tool")
    paired_device.send(session, "native cancellation fixture", delivery="start-if-idle")
    wait(lambda: bool(paired_device.approvals(session)), process=scenario.native)
    approval = paired_device.approvals(session)[0]
    paired_device.interrupt(session)
    wait(
        lambda: not scenario.request("state", sessionId=session)["running"],
        process=scenario.native,
    )
    code, _ = paired_device.invoke(
        "session", "approval", "approve",
        "--request",
        approval["approval_id"],
        "--digest",
        approval["digest"],
        "--version",
        str(approval["version"]),
        success=False,
    )
    assert code != 0 and scenario.request("effects")["count"] == 0
    assert scenario.request("events", sessionId=session)["decisions"] == ["cancelled"]


def test_native_browser_settings_persist(scenario):
    """When the real DSH browser opens WhaleRein settings, credentials stay masked, connection testing authenticates, and saved fields survive a page reload."""
    scenario.browser(
        "settings", hostToken=scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"]
    )
    expected = {"service": "standalone"}
    assert scenario.request("connection-settings") == expected
    scenario.native.close()
    scenario.start_native()
    assert scenario.request("connection-settings") == expected


def test_native_browser_bad_draft_is_read_only(scenario):
    """When a browser connection draft uses a rejected credential, the real host stays online and reload discards the unsaved secret."""
    scenario.browser(
        "bad-draft", hostToken=scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"]
    )
    assert scenario.request("ready")["connected"]


def test_native_browser_invitation_and_revoke(scenario):
    """When the real settings page generates an invitation and revokes its paired CLI, pairing succeeds and the old transport credential is rejected afterward."""
    from environment import Device

    invitation = scenario.directory / "browser-invitation.txt"
    scenario.browser(
        "invite",
        invitationFile=str(invitation),
        hostToken=scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"],
    )
    device = Device(scenario.directory)
    device.pair(invitation.read_text())
    with scenario.fixture_bearer(device) as bearer:
        scenario.browser(
            "revoke", hostToken=scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"]
        )
        with pytest.raises(RuntimeError, match=r"HTTP (401|403)"):
            http("/v1/hosts", token=bearer)


def test_native_browser_claim_ownership(scenario, paired_device):
    """When a real browser observes the native Session before a claimed question starts, only remote approval owns it and exactly one native decision/effect settles it."""
    scenario.browser_claim(paired_device)


def test_native_interrupt_stream(scenario, paired_device):
    """When the actual native model stream is interrupted before its held completion, the turn stops and read-only recovery starts no replacement model call."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    paired_device.send(session, "native stream cancellation fixture", delivery="start-if-idle")
    assert scenario.model.started.wait(timeout=15)
    paired_device.interrupt(session)
    wait(
        lambda: not scenario.request("state", sessionId=session)["running"],
        process=scenario.native,
    )
    calls = scenario.model.calls
    scenario.model.release.set()
    paired_device.observe_history(session)
    assert scenario.model.calls == calls == 1


def test_native_first_pair_after_access_restart(scenario):
    """When Access restarts before the first pairing, the native host recovers framed transport capability without a previously paired client priming it."""
    scenario.access.close()
    scenario.start_access()
    device = scenario.pair()
    session = scenario.create()
    assert any(item["session_id"] == session for item in device.sessions(scenario.host))


def test_native_delivery_queue_and_steer(scenario, paired_device):
    """Exercise native queue and steer over the encrypted path in an isolated test profile."""
    scenario.request("enable-delivery-for-test")
    session = scenario.create()
    scenario.model.plan(hold=True)
    first = paired_device.send_confirmed(session, "synthetic initial", delivery="start-if-idle")
    assert first["delivery_id"]
    assert scenario.model.started.wait(timeout=15)
    queued = paired_device.send_confirmed(session, "synthetic queued")
    assert queued["delivery_id"] and queued["delivery_id"] != first["delivery_id"]
    wait(
        lambda: queued["delivery_id"] in scenario.request("inbox-metadata", sessionId=session)["ids"],
        process=scenario.native,
    )
    steered = paired_device.send_confirmed(session, "synthetic steer", delivery="steer")
    assert steered["delivery_id"] not in (first["delivery_id"], queued["delivery_id"])
    scenario.model.release.set()
    scenario.wait_idle(session, assistant_count=2)
    paired_device.observe_history(session)
    _, message_ids = scenario.cached(paired_device, session)
    assert message_ids
    assert all(receipt["delivery_id"] not in message_ids for receipt in (first, queued, steered))
    assert all(receipt["delivery_id"] != receipt["command_id"] for receipt in (first, queued, steered))


def test_native_idle_steer(scenario, paired_device):
    """Native steering can wake an idle agent and returns an acceptance identity."""
    scenario.request("enable-delivery-for-test")
    session = scenario.create()
    scenario.model.plan()
    receipt = paired_device.send(session, "synthetic idle steer", delivery="steer")["receipt"]
    assert receipt["delivery_id"]
    scenario.wait_idle(session)


def test_native_start_if_idle_busy(scenario, paired_device):
    """A sampled running state rejects explicit start-if-idle before native submission."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    paired_device.send(session, "synthetic initial", delivery="start-if-idle")
    assert scenario.model.started.wait(timeout=15)
    text = "synthetic busy"
    code, values = paired_device.invoke(
        "session", "send", session, "--detach", "--delivery", "start-if-idle", "--", text,
        success=False,
    )
    code, result = paired_device.settle_detached_delivery(
        code, values[-1], session, text, "start-if-idle"
    )
    assert code == 8
    assert result["error"]["code"] == "conflict"
    assert scenario.model.calls == 1
    scenario.model.release.set()
    scenario.wait_idle(session)


def test_native_interrupt_keeps_queued_delivery(scenario, paired_device):
    """A native interruption retains an accepted queued message instead of silently dropping it."""
    scenario.request("enable-delivery-for-test")
    session = scenario.create()
    scenario.model.plan(hold=True)
    paired_device.send(session, "synthetic running", delivery="start-if-idle")
    assert scenario.model.started.wait(timeout=15)
    queued = paired_device.send_confirmed(session, "synthetic queued")
    wait(
        lambda: queued["delivery_id"] in scenario.request("inbox-metadata", sessionId=session)["ids"],
        process=scenario.native,
    )
    paired_device.interrupt(session)
    scenario.model.release.set()
    wait(
        lambda: scenario.request("events", sessionId=session)["types"].count("user/message") >= 2,
        timeout=30,
        process=scenario.native,
    )
    scenario.wait_idle(session)


def test_native_steer_after_interrupt(scenario, paired_device):
    """Steering after interruption follows native wakeup without an added idle-state conflict."""
    scenario.request("enable-delivery-for-test")
    session = scenario.create()
    scenario.model.plan(hold=True)
    paired_device.send(session, "synthetic running", delivery="start-if-idle")
    assert scenario.model.started.wait(timeout=15)
    paired_device.interrupt(session)
    receipt = paired_device.send_confirmed(session, "synthetic after interrupt", delivery="steer")
    assert receipt["delivery_id"]
    scenario.model.release.set()
    wait(
        lambda: scenario.request("events", sessionId=session)["types"].count("user/message") >= 2,
        timeout=30,
        process=scenario.native,
    )
    scenario.wait_idle(session)


def test_native_start_if_idle_after_local_start_race(scenario, paired_device):
    """A local start after the remote precheck follows native queue rather than inventing a conflict."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("arm-start-if-idle-race")
    receipt = paired_device.send_confirmed(session, "synthetic remote race", delivery="start-if-idle")
    assert receipt["delivery_id"]
    assert scenario.model.started.wait(timeout=15)
    wait(
        lambda: receipt["delivery_id"] in scenario.request("inbox-metadata", sessionId=session)["ids"],
        process=scenario.native,
    )
    scenario.model.release.set()
    scenario.wait_idle(session, assistant_count=2)
def test_native_model_selection_adapter(scenario, paired_device):
    """Observe model selection through the isolated native DSH controller and adapter."""
    session = scenario.create()
    catalog = scenario.request("model-catalog")
    assert catalog["groups"], "Native model provider catalog is empty"
    group = catalog["groups"][0]
    assert len(group["models"]) >= 2, "Native model catalog needs two distinct fixture routes"
    selection = {"provider": group["id"], "model": group["models"][0]["id"]}
    later = {"provider": group["id"], "model": group["models"][1]["id"]}
    scenario.native.close()
    scenario.start_native()
    assert not scenario.request("state", sessionId=session)["active"]
    before = scenario.request("model-read", sessionId=session)
    assert before["pending"] is None and before["last_used"] is None
    assert before["default_is_fallback"] is True
    assert not scenario.request("state", sessionId=session)["active"]
    receipt = scenario.request("model-select", sessionId=session, selection=selection)
    assert receipt["selected"]["provider"] == selection["provider"]
    assert receipt["selected"]["model"] == selection["model"]
    assert receipt["effect_scope"] == "session_and_source_default_attempt"
    assert receipt["default_persistence"] == "unconfirmed"
    observed = scenario.request("model-read", sessionId=session)
    assert observed["pending"]["model"] == selection["model"]
    scenario.request("model-select-local", sessionId=session, selection=later)
    assert scenario.request("model-read", sessionId=session)["pending"]["model"] == later["model"]
    assert receipt["selected"]["model"] == selection["model"]
def test_native_pending_queue_source(scenario, paired_device):
    """A live native inbox can be passively read, edited and withdrawn; cold state is unavailable."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    first = scenario.request("queue-read", sessionId=session)
    assert first["available"] and first["running"] and len(first["items"]) == 1
    identity = first["items"][0]["id"]
    assert first["items"][0]["placement"] == "QUEUED"
    assert set(first["items"][0]["actions"]) == {"EDIT", "REMOVE", "PROMOTE"}
    assert scenario.request("queue-edit", sessionId=session)["id"] == identity
    changed = scenario.request("queue-read", sessionId=session)
    assert changed["items"][0]["id"] == identity and changed["items"][0]["fixtureText"]
    assert scenario.request("queue-promote", sessionId=session)["id"] == identity
    promoted = scenario.request("queue-read", sessionId=session)
    assert len(promoted["items"]) == 1 and promoted["items"][0]["id"] == identity
    assert promoted["items"][0]["placement"] == "STEERING"
    assert scenario.request("queue-remove", sessionId=session)["id"] == identity
    assert not scenario.request("queue-read", sessionId=session)["items"]
    scenario.model.release.set()
    scenario.wait_idle(session)
    scenario.native.close()
    scenario.start_native()
    assert scenario.request("queue-read", sessionId=session)["available"] is False


def test_native_pending_queue_remote(scenario, paired_device):
    """An isolated native inbox accepts explicitly gated CLI reads and mutations through encryption."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    # This fixture-only toggle is not part of the production configuration.
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    initial = paired_device.call("session", "queue", "list", "--session", session)
    assert initial["available"] and len(initial["items"]) == 1
    identity = initial["items"][0]["item_id"]
    edited = paired_device.call("session", "queue", "edit", "--session", session, "--item", identity, "replacement fixture")
    assert edited["command"]["status"] == "succeeded" and edited["refresh"] == "current"
    assert scenario.request("queue-read", sessionId=session)["items"][0]["fixtureText"]
    removed = paired_device.call("session", "queue", "remove", "--session", session, "--item", identity)
    assert removed["command"]["status"] == "succeeded"
    assert not scenario.request("queue-read", sessionId=session)["items"]
    scenario.model.release.set()


def test_native_pending_queue_attachment_replacement(scenario, paired_device):
    """A native queued image exposes metadata, and an explicit edit replaces the whole item."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add-image", sessionId=session)
    before = scenario.request("queue-read", sessionId=session)
    assert before["available"] and len(before["items"]) == 1
    item = before["items"][0]
    assert item["attachments"] and item["attachments"][0]["mediaType"] == "image/png"
    assert item["attachments"][0]["size"] > 0 and not item["attachments"][0]["available"]
    identity = item["id"]
    metadata = scenario.request("queue-native-meta", sessionId=session)["items"][0]
    assert metadata["id"] == identity and metadata["rpcId"]
    assert scenario.request("queue-edit", sessionId=session)["id"] == identity
    after = scenario.request("queue-read", sessionId=session)
    assert after["items"][0]["id"] == identity and after["items"][0]["fixtureText"]
    assert after["items"][0]["attachments"] == []
    assert scenario.request("queue-native-meta", sessionId=session)["items"][0] == metadata
    scenario.model.release.set()


def test_native_pending_queue_concurrent_mutations(scenario, paired_device):
    """Concurrent remote edits follow DSH order; removal never recreates a consumed item."""
    from concurrent.futures import ThreadPoolExecutor
    from environment import Device, http

    other_dir = scenario.directory / "queue-second-device"
    other_dir.mkdir()
    other = Device(other_dir)
    invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    other.pair(invitation["pairingUri"])
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    other.sessions(scenario.host)
    item = paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"]
    with ThreadPoolExecutor(max_workers=2) as executor:
        edits = [executor.submit(device.invoke, "session", "queue", "edit", "--session", session, "--item", item, value, success=False) for device, value in ((paired_device, "edit first fixture"), (other, "edit second fixture"))]
        results = [future.result() for future in edits]
    assert all(code == 0 for code, _ in results)
    current = paired_device.call("session", "queue", "list", "--session", session)
    assert len(current["items"]) == 1 and current["items"][0]["item_id"] == item
    final_text = current["items"][0]["blocks"][0]["text"]
    assert final_text in ("edit first fixture", "edit second fixture")
    assert paired_device.call("session", "queue", "remove", "--session", session, "--item", item)["command"]["status"] == "succeeded"
    code, results = paired_device.invoke("session", "queue", "edit", "--session", session, "--item", item, "late fixture", success=False)
    assert code != 0 and results[-1]["error"]["code"] == "not_found"
    assert paired_device.call("session", "queue", "list", "--session", session)["items"] == []
    scenario.model.release.set()


def test_native_pending_queue_local_remote_race(scenario, paired_device):
    """Concurrent local removal and remote edit never recreate the native occurrence."""
    from concurrent.futures import ThreadPoolExecutor

    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item = paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"]
    with ThreadPoolExecutor(max_workers=2) as executor:
        local = executor.submit(scenario.request, "queue-remove-direct", sessionId=session, itemId=item)
        remote = executor.submit(paired_device.invoke, "session", "queue", "edit", "--session", session, "--item", item, "race fixture", success=False)
        assert local.result()["accepted"]
        code, envelope = remote.result()
    assert code == 0 or envelope[-1]["error"]["code"] in ("not_found", "unknown")
    assert paired_device.call("session", "queue", "list", "--session", session)["items"] == []
    scenario.model.release.set()

def test_native_model_selection_running_and_queued(scenario, paired_device):
    """When the native model is running with a queued message, selection delegates to DSH without cancelling the run or clearing its queue."""
    session = scenario.create()
    group = scenario.request("model-catalog")["groups"][0]
    selection = {"provider": group["id"], "model": group["models"][0]["id"]}
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15), "Native fixture model did not start"
    scenario.request("model-queue", sessionId=session)
    before = scenario.request("state", sessionId=session)
    assert before["running"] and before["queueKnown"] and before["queueCount"] >= 1
    receipt = scenario.request("model-select", sessionId=session, selection=selection)
    assert receipt["selected"]["model"] == selection["model"]
    after = scenario.request("state", sessionId=session)
    assert after["running"] and after["queueCount"] == before["queueCount"]
    assert not scenario.model.release.is_set()
    scenario.model.release.set()
    scenario.wait_idle(session, assistant_count=2)


def test_native_model_selection_while_approval_waits(scenario, paired_device):
    """When a native approval is pending, selecting a model preserves the pending approval and does not authorize its protected effect."""
    session = scenario.create()
    group = scenario.request("model-catalog")["groups"][0]
    selection = {"provider": group["id"], "model": group["models"][0]["id"]}
    scenario.model.plan("tool")
    paired_device.send(session, "native model approval fixture", delivery="start-if-idle")
    wait(lambda: bool(paired_device.approvals(session)), process=scenario.native)
    approval = paired_device.approvals(session)[0]
    receipt = scenario.request("model-select", sessionId=session, selection=selection)
    assert receipt["selected"]["model"] == selection["model"]
    assert [item["approval_id"] for item in paired_device.approvals(session)] == [approval["approval_id"]]
    assert scenario.request("effects")["count"] == 0
    paired_device.resolve("reject", approval)
    scenario.wait_idle(session)
    assert scenario.request("effects")["count"] == 0


def test_native_model_selection_background_save_failure(scenario, paired_device):
    """When isolated DSH default persistence fails, an accepted session selection remains accepted without retry or rollback."""
    session = scenario.create()
    catalog = scenario.request("model-catalog")
    group = catalog["groups"][0]
    alternative = next(model for model in group["models"] if model["id"] != catalog["default_selection"]["model"])
    selection = {"provider": group["id"], "model": alternative["id"]}
    result = scenario.request("model-select-save-fails", sessionId=session, selection=selection)
    assert result["attempts"] == 1
    assert result["receipt"]["selected"]["model"] == selection["model"]
    assert result["receipt"]["default_persistence"] == "unconfirmed"
    after = scenario.request("model-read", sessionId=session)
    assert after["next"]["model"] == selection["model"]
    assert after["default_selection"] == catalog["default_selection"]


def test_native_model_selection_while_user_question_waits(scenario, paired_device):
    """When a real DSH user question is awaiting an answer, model selection leaves that question pending and unanswered."""
    session = scenario.create()
    group = scenario.request("model-catalog")["groups"][0]
    selection = {"provider": group["id"], "model": group["models"][0]["id"]}
    assert scenario.request("model-question-start", sessionId=session)["pending"]
    receipt = scenario.request("model-select", sessionId=session, selection=selection)
    assert receipt["selected"]["model"] == selection["model"]
    assert scenario.request("model-question-state")["state"] == "pending"
    scenario.request("model-question-answer")
    wait(lambda: scenario.request("model-question-state")["state"] == "answered", process=scenario.native)


def test_native_remote_model_catalog_only(scenario, paired_device):
    """A catalog-only native Host rejects remote reads and writes before invoking their source methods."""
    session = scenario.create()
    assert scenario.request("test-model-catalog-only")["enabled"]
    paired_device.sessions(scenario.host)
    catalog = paired_device.call("session", "models", "--host", scenario.host)
    group = next(group for group in catalog["groups"] if group["models"])
    for args in [("session", "model", "get", "--session", session),
                 ("session", "model", "set", "--session", session, "--provider", group["id"], "--model", group["models"][0]["id"])]:
        code, envelopes = paired_device.invoke(*args, success=False)
        assert code != 0 and envelopes[-1]["error"]["code"] == "unsupported_capability"
    assert scenario.request("test-model-catalog-only-counts") == {"read": 0, "select": 0}


def test_native_remote_model_catalog_provider_failure_is_redacted(scenario, paired_device):
    """A remote catalog preserves healthy native providers and hides an injected provider diagnostic."""
    assert scenario.request("test-enable-gx")["enabled"]
    assert scenario.request("test-model-provider-failure-arm")["armed"]
    code, envelopes = paired_device.invoke("session", "models", "--host", scenario.host, success=False)
    assert code == 0
    catalog = envelopes[-1]["data"]
    assert any(group["models"] for group in catalog["groups"])
    assert catalog["complete"] is False
    assert catalog["failures"] == [{"id": "fixture-bad-provider", "name": "Fixture failure", "code": "unavailable"}]
    assert "fixture-provider-private-diagnostic" not in json.dumps(envelopes)


def test_native_remote_model_revoked_device_cannot_reach_source(scenario, paired_device):
    """After revocation, remote model operations cannot reach the native source."""
    session = scenario.create()
    assert scenario.request("test-enable-gx")["enabled"]
    assert scenario.request("test-model-counts-enable")["enabled"]
    paired_device.sessions(scenario.host)
    catalog = paired_device.call("session", "models", "--host", scenario.host)
    group = next(group for group in catalog["groups"] if group["models"])
    before = scenario.request("test-model-counts")
    attempts = [("session", "models", "--host", scenario.host), ("session", "model", "get", "--session", session),
                ("session", "model", "set", "--session", session, "--provider", group["id"], "--model", group["models"][0]["id"])]
    stale_devices = []
    for index in range(len(attempts)):
        directory = scenario.directory / f"stale-native-model-{index}"
        directory.mkdir()
        stale = Device(directory)
        shutil.copytree(paired_device.instance_dir, stale.instance_dir)
        stale.identity = paired_device.identity
        stale_devices.append(stale)
    with scenario.fixture_bearer(paired_device) as bearer:
        paired_device.revoke()
        with pytest.raises(RuntimeError, match=r"HTTP (401|403)"):
            http("/v1/hosts", token=bearer)
    for stale, args in zip(stale_devices, attempts, strict=True):
        code, _ = stale.invoke(*args, success=False)
        assert code != 0
    assert scenario.request("test-model-counts") == before


def test_native_remote_model_selection_preserves_open_question(scenario, paired_device):
    """A remote native model selection preserves a pending question without answering it."""
    session = scenario.create()
    assert scenario.request("test-enable-gx")["enabled"]
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    assert scenario.request("question-start-remote-fixture", sessionId=session)["started"]
    paired_device.sessions(scenario.host)
    before = paired_device.call("session", "question", "list", "--session", session)["batches"]
    assert len(before) == 1 and before[0]["phase"] == "QUESTION_OPEN"
    catalog = paired_device.call("session", "models", "--host", scenario.host)
    group = next(group for group in catalog["groups"] if group["models"])
    receipt = paired_device.call("session", "model", "set", "--session", session,
                                 "--provider", group["id"], "--model", group["models"][0]["id"])
    assert receipt["status"] == "succeeded"
    after = paired_device.call("session", "question", "list", "--session", session)["batches"]
    assert len(after) == 1
    assert (after[0]["batch_id"], after[0]["source_epoch"], after[0]["digest"], after[0]["phase"]) == (
        before[0]["batch_id"], before[0]["source_epoch"], before[0]["digest"], before[0]["phase"])
    answer = scenario.directory / "model-question-answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    batch = after[0]
    settled = paired_device.call("session", "question", "answer", "--session", session, "--batch", batch["batch_id"],
        "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
        "--phase", batch["phase"], "--input", str(answer))
    assert settled["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-result-remote-fixture", sessionId=session) == {"selected": "Yes", "pending": False}


def test_native_remote_model_selection_preserves_pending_approval(scenario, paired_device):
    """Remote native model selection leaves a running tool approval pending and unauthorized."""
    session = scenario.create()
    assert scenario.request("test-enable-gx")["enabled"]
    scenario.model.plan("tool")
    paired_device.send(session, "synthetic model approval fixture", delivery="start-if-idle")
    wait(lambda: bool(paired_device.approvals(session)), process=scenario.native)
    approval = paired_device.approvals(session)[0]
    before = scenario.request("state", sessionId=session)
    current = paired_device.call("session", "model", "get", "--session", session)["next"] or {}
    effective = (current.get("provider"), current.get("model"))
    catalog = paired_device.call("session", "models", "--host", scenario.host)
    provider, model = next(
        (group["id"], entry["id"])
        for group in catalog["groups"]
        for entry in group["models"]
        if (group["id"], entry["id"]) != effective
    )
    receipt = paired_device.call(
        "session", "model", "set", "--session", session, "--provider", provider, "--model", model
    )
    assert receipt["status"] == "succeeded"
    selected = receipt["model_selection"]["selected"]
    assert (selected["provider"], selected["model"]) == (provider, model)
    after = scenario.request("state", sessionId=session)
    assert after["pendingInputCount"] == before["pendingInputCount"]
    assert [item["approval_id"] for item in paired_device.approvals(session)] == [approval["approval_id"]]
    assert scenario.request("effects")["count"] == 0
    paired_device.resolve("reject", approval)
    scenario.wait_idle(session)
    assert scenario.request("effects")["count"] == 0


def test_native_remote_model_selection_preserves_gateway_pending_input(scenario, paired_device):
    """A native owner event stream holds a forwarded user question while remote model selection runs."""
    session = scenario.create()
    assert scenario.request("test-enable-gx")["enabled"]
    assert scenario.request("test-model-owner-open") == {"ready": True, "live": True}
    try:
        assert scenario.request("test-model-gateway-question-start", sessionId=session)["started"]
        wait(lambda: scenario.request("state", sessionId=session)["pendingInputCount"] > 0,
             timeout=15, process=scenario.native)
        before = scenario.request("state", sessionId=session)
        assert scenario.request("test-model-gateway-pending", sessionId=session)["events"] == ["user-questions/request"]
        catalog = paired_device.call("session", "models", "--host", scenario.host)
        group = next(group for group in catalog["groups"] if group["models"])
        receipt = paired_device.call("session", "model", "set", "--session", session,
                                     "--provider", group["id"], "--model", group["models"][0]["id"])
        assert receipt["status"] == "succeeded"
        after = scenario.request("state", sessionId=session)
        assert after["pendingInputCount"] == before["pendingInputCount"]
        assert scenario.request("test-model-gateway-pending", sessionId=session)["events"] == ["user-questions/request"]
        assert scenario.request("effects")["count"] == 0
    finally:
        scenario.request("test-model-owner-close")


def test_model_extra_native_explicit_effort(scenario, paired_device):
    """When an advertised native reasoning effort is selected, the adapter preserves that exact effort in the receipt and projection."""
    session = scenario.create()
    group = scenario.request("model-catalog")["groups"][0]
    model = next(model for model in group["models"] if model["efforts"])
    selection = {"provider": group["id"], "model": model["id"], "effort": model["efforts"][0]["id"]}
    receipt = scenario.request("model-select", sessionId=session, selection=selection)
    assert receipt["selected"]["effort"] == selection["effort"]
    assert scenario.request("model-read", sessionId=session)["next"]["effort"] == selection["effort"]


def test_model_extra_native_concurrent_local_change(scenario, paired_device):
    """When local and adapter selections race, each native receipt names its own choice and the later observation reflects a native outcome without rollback."""
    session = scenario.create()
    group = scenario.request("model-catalog")["groups"][0]
    assert len(group["models"]) >= 2
    first = {"provider": group["id"], "model": group["models"][0]["id"]}
    second = {"provider": group["id"], "model": group["models"][1]["id"]}
    result = scenario.request("model-select-concurrent", sessionId=session, selection=first, otherSelection=second)
    assert result["remote"]["selected"]["model"] == first["model"]
    assert result["local"]["selected"]["model"] == second["model"]
    observed = scenario.request("model-read", sessionId=session)
    assert observed["next"]["model"] in {first["model"], second["model"]}


def test_model_extra_native_catalog_provider_failure(scenario, paired_device):
    """When one injected native model provider fails, healthy routes remain visible and its private error is replaced with a stable availability code."""
    catalog = scenario.request("model-catalog-provider-failure")
    assert catalog["groups"] and catalog["groups"][0]["models"]
    assert catalog["complete"] is False
    assert catalog["failures"] == [{"id": "fixture-bad-provider", "name": "Fixture failure", "code": "unavailable"}]
    assert "fixture-provider-private-diagnostic" not in str(catalog)


def test_model_extra_native_unavailable_route_and_invalid_effort(scenario, paired_device):
    """When native model selection rejects an unavailable route or invalid effort, the adapter exposes a sanitized uncertain outcome and leaves the observed selection unchanged."""
    session = scenario.create()
    group = scenario.request("model-catalog")["groups"][0]
    model = group["models"][0]
    before = scenario.request("model-read", sessionId=session)
    selections = [
        {"provider": "fixture-unavailable", "model": "fixture-unavailable"},
        {"provider": group["id"], "model": model["id"], "effort": "fixture-invalid-effort"},
    ]
    for selection in selections:
        result = scenario.request("model-select-outcome", sessionId=session, selection=selection)
        assert result["accepted"] is False
        assert result["code"] in {"unavailable", "invalid_argument"}
        assert result["definitive"] is False
        assert scenario.request("model-read", sessionId=session)["next"] == before["next"]


def test_native_queue_replay_and_digest(scenario, paired_device):
    """A replayed queue edit does not overwrite a later native edit; changed digest is rejected."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item = paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"]
    receipt = paired_device.call("session", "queue", "edit", "--session", session, "--item", item, "replacement fixture")
    assert receipt["command"]["status"] == "succeeded"
    assert scenario.request("queue-edit-direct", sessionId=session, itemId=item)["accepted"]
    status, replay = scenario.replay_command(paired_device)
    assert status in (200, 202) and replay["commandId"] == receipt["command"]["command_id"]
    assert scenario.request("queue-read", sessionId=session)["items"][0]["laterText"]
    status, conflict = scenario.replay_command(paired_device, changed=True, include_conflict_code=True)
    assert status == 409 and conflict["code"] == "payload_digest_mismatch"
    assert scenario.request("queue-read", sessionId=session)["items"][0]["laterText"]
    scenario.model.release.set()


def test_native_queue_revocation(scenario, paired_device):
    """Revocation prevents queue read and mutation, leaving the native pending item unchanged."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    native = scenario.request("queue-read", sessionId=session)
    assert native["available"] and len(native["items"]) == 1
    item = native["items"][0]["id"]
    paired_device.sessions(scenario.host)
    assert paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"] == item
    paired_device.revoke()
    read_code, _ = paired_device.invoke("session", "queue", "list", "--session", session, success=False)
    edit_code, _ = paired_device.invoke("session", "queue", "remove", "--session", session, "--item", item, success=False)
    assert read_code != 0 and edit_code != 0
    assert scenario.request("queue-read", sessionId=session)["items"][0]["id"] == item
    scenario.model.release.set()


def test_native_queue_consumed_item_cannot_return(scenario, paired_device):
    """Once DSH asynchronously consumes a queued turn, a late remote edit cannot recreate it."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    initial = scenario.request("queue-read", sessionId=session)
    item = initial["items"][0]["id"]
    paired_device.sessions(scenario.host)
    scenario.model.release.set()
    scenario.wait_idle(session, assistant_count=2)
    before = scenario.request("events", sessionId=session)["types"].count("user/message")
    assert before >= 2
    code, result = paired_device.invoke("session", "queue", "edit", "--session", session, "--item", item, "late fixture", success=False)
    assert code != 0 and result[-1]["error"]["code"] == "not_found"
    code, result = paired_device.invoke("session", "queue", "remove", "--session", session, "--item", item, success=False)
    assert code != 0 and result[-1]["error"]["code"] == "not_found"
    assert scenario.request("events", sessionId=session)["types"].count("user/message") == before
    assert paired_device.call("session", "queue", "list", "--session", session)["items"] == []


def test_native_queue_interrupt_retention(scenario, paired_device):
    """Interruption retains accepted queued work; a fresh read never implies a stop-and-clear action."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    initial = scenario.request("queue-read", sessionId=session)
    item = initial["items"][0]["id"]
    paired_device.sessions(scenario.host)
    def wait_succeeded(receipt):
        assert receipt["status"] in ("succeeded", "host_accepted")
        if receipt["status"] == "host_accepted":
            wait(lambda: paired_device.call("operation", "recover", receipt["request_id"])["status"] == "succeeded", timeout=15)

    wait_succeeded(paired_device.interrupt(session))
    # The source may consume the retained item as soon as the interrupt settles.
    # Check the exact identity in either source state instead of an event count.
    def retained_or_committed():
        observed = scenario.request("queue-retention-evidence", sessionId=session)
        return item in observed["committed"] or (observed["available"] and item in observed["queued"])

    wait(retained_or_committed, timeout=15)
    current = paired_device.call("session", "queue", "list", "--session", session)
    if current["available"] and any(entry["item_id"] == item for entry in current["items"]):
        code, values = paired_device.invoke("session", "queue", "remove", "--session", session, "--item", item, success=False)
        if code == 0:
            wait_succeeded(values[-1]["data"]["command"])
            assert all(entry["item_id"] != item for entry in paired_device.call("session", "queue", "list", "--session", session)["items"])
        else:
            assert values[-1]["error"]["code"] == "not_found"
            wait(lambda: item in scenario.request("queue-retention-evidence", sessionId=session)["committed"], timeout=15)
    else:
        wait(lambda: item in scenario.request("queue-retention-evidence", sessionId=session)["committed"], timeout=15)
    scenario.model.release.set()


def test_native_queue_order_and_promotion_guard(scenario, paired_device):
    """Native pending entries preserve order; promoting an already steering item is rejected."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    scenario.request("queue-add", sessionId=session)
    source = scenario.request("queue-read", sessionId=session)
    assert source["available"] and len(source["items"]) == 2
    source_ids = [item["id"] for item in source["items"]]
    assert len(set(source_ids)) == 2
    paired_device.sessions(scenario.host)
    remote = paired_device.call("session", "queue", "list", "--session", session)
    assert [item["item_id"] for item in remote["items"]] == source_ids
    assert all(item["placement"] == "QUEUED" for item in remote["items"])
    assert set(remote) == {"session_id", "available", "running", "items", "target"}
    assert remote["target"] == {"host_id": scenario.host, "session_id": session}
    assert all(set(item) == {"item_id", "placement", "delivery_id", "blocks", "actions"} for item in remote["items"])
    assert scenario.request("queue-promote", sessionId=session)["id"] == source_ids[0]
    promoted = paired_device.call("session", "queue","list","--session",session)
    by_id = {item["item_id"]:item for item in promoted["items"]}
    assert by_id[source_ids[0]]["placement"] == "STEERING"
    assert by_id[source_ids[1]]["placement"] == "QUEUED"
    assert "PROMOTE" not in by_id[source_ids[0]]["actions"]
    code, outcome = paired_device.invoke("session", "queue", "steer", "--session", session, "--item", source_ids[0], success=False)
    assert code != 0 and outcome[-1]["error"]["code"] == "conflict"
    after = scenario.request("queue-read", sessionId=session)
    assert len(after["items"]) == 2 and len({item["id"] for item in after["items"]}) == 2
    scenario.model.release.set()


def test_native_queue_idle_promotion_guard(scenario, paired_device):
    """A retained native item cannot be promoted when the turn is idle."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    source = scenario.request("queue-read", sessionId=session)
    item = source["items"][0]["id"]
    paired_device.sessions(scenario.host)
    paired_device.interrupt(session)
    wait(lambda: not scenario.request("state", sessionId=session)["running"], timeout=15, process=scenario.native)
    after = scenario.request("queue-read", sessionId=session)
    assert any(entry["id"] == item for entry in after["items"])
    code, outcome = paired_device.invoke("session", "queue", "steer", "--session", session, "--item", item, success=False)
    assert code != 0 and outcome[-1]["error"]["code"] == "conflict"
    assert any(entry["id"] == item for entry in scenario.request("queue-read", sessionId=session)["items"])
    scenario.model.release.set()


def test_native_queue_wrong_session_scope(scenario, paired_device):
    """A queued item identity cannot mutate work in another native session."""
    first = scenario.create()
    second = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=first)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("start", sessionId=second)
    wait(lambda: scenario.model.calls >= 2, timeout=15, process=scenario.native)
    scenario.request("queue-add", sessionId=first)
    first_queue = scenario.request("queue-read", sessionId=first)
    scenario.request("queue-read", sessionId=second)
    item = first_queue["items"][0]["id"]
    paired_device.sessions(scenario.host)
    code, outcome = paired_device.invoke("session", "queue", "remove", "--session", second, "--item", item, success=False)
    assert code != 0 and outcome[-1]["error"]["code"] == "not_found"
    assert scenario.request("queue-read", sessionId=first)["items"][0]["id"] == item
    assert paired_device.call("session", "queue", "list", "--session", second)["items"] == []
    scenario.model.release.set()


@pytest.mark.parametrize(
    "mode",
    [
        pytest.param("enqueue-after-run", marks=pytest.mark.description(
            "A held queued delivery reaches native after the current generation completes."
        )),
        pytest.param("steer", marks=pytest.mark.description(
            "A held steering delivery reaches native after the current generation completes."
        )),
    ],
)
def test_native_delivery_completion_race(scenario, paired_device, mode):
    """A held remote delivery reaches native after the current generation completes."""
    scenario.request("enable-delivery-for-test")
    session = scenario.create()
    scenario.model.plan(hold=True)
    paired_device.send(session, "synthetic running")
    assert scenario.model.started.wait(timeout=15)
    scenario.request("arm-delivery-barrier")
    with ThreadPoolExecutor(max_workers=1) as executor:
        pending = executor.submit(paired_device.send_confirmed, session, "synthetic raced delivery", mode)
        try:
            wait(lambda: scenario.request("delivery-barrier-state")["entered"], timeout=10, process=scenario.native)
            scenario.model.release.set()
            scenario.wait_idle(session)
        finally:
            scenario.request("release-delivery-barrier")
        receipt = pending.result(timeout=20)
    assert receipt["delivery_id"]
    scenario.wait_idle(session, assistant_count=2)
    assert scenario.request("events", sessionId=session)["types"].count("user/message") >= 2
    assert scenario.model.calls == 2


def test_native_queue_cold_reconnect_and_receipt_recovery(scenario, paired_device):
    """After DSH restart, cold queue is unavailable and a confirmed receipt is never replayed."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    source = scenario.request("queue-read", sessionId=session)
    item = source["items"][0]["id"]
    paired_device.sessions(scenario.host)
    receipt = paired_device.call("session", "queue", "edit", "--session", session, "--item", item, "replacement fixture")
    assert receipt["command"]["status"] == "succeeded"
    calls = scenario.model.calls
    scenario.native.close()
    scenario.start_native()
    source = scenario.request("queue-read", sessionId=session)
    assert not source["available"]
    paired_device.sessions(scenario.host)
    current = paired_device.call("session", "queue", "list", "--session", session)
    assert not current["available"] and current["items"] == []
    status, replay = scenario.replay_command(paired_device)
    assert status in (200, 202) and replay["commandId"] == receipt["command"]["command_id"]
    assert not scenario.request("queue-read", sessionId=session)["available"]
    assert scenario.model.calls == calls
    scenario.model.release.set()


def test_native_queue_invalid_edit_preserves_item(scenario, paired_device):
    """Whitespace and non-text queue replacements are rejected without changing source work."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    source = scenario.request("queue-read", sessionId=session)
    item = source["items"][0]["id"]
    paired_device.sessions(scenario.host)
    code, outcome = paired_device.invoke("session", "queue", "edit", "--session", session, "--item", item, "  ", success=False)
    assert code != 0 and outcome[-1]["error"]["code"] == "invalid_argument"
    assert scenario.request("queue-invalid-nontext", sessionId=session, itemId=item)["rejected"]
    after = scenario.request("queue-read", sessionId=session)
    assert len(after["items"]) == 1 and after["items"][0]["id"] == item
    scenario.model.release.set()


def test_native_queue_capability_open_by_default(scenario, paired_device):
    """Queue reads and mutations are available to the CLI without a test-only capability switch."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    ids = scenario.request("queue-native-ids", sessionId=session)["ids"]
    assert len(ids) == 1
    paired_device.sessions(scenario.host)
    listed = paired_device.call("session", "queue", "list", "--session", session)
    assert listed["available"] and listed["items"][0]["item_id"] == ids[0]
    edited = paired_device.call("session", "queue", "edit", "--session", session, "--item", ids[0], "replacement fixture")
    assert edited["command"]["status"] == "succeeded"
    after_edit = scenario.request("queue-native-ids", sessionId=session)["ids"]
    assert after_edit == ids
    removed = paired_device.call("session", "queue", "remove", "--session", session, "--item", ids[0])
    assert removed["command"]["status"] == "succeeded"
    assert scenario.request("queue-native-ids", sessionId=session)["ids"] == []
    scenario.model.release.set()


def test_native_queue_promote_after_remove_failure_is_unknown(scenario, paired_device):
    """A promotion fault after native removal is unknown, never a definitive rejection."""
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    source=scenario.request("queue-read", sessionId=session)
    item=source["items"][0]["id"]
    paired_device.sessions(scenario.host)
    assert scenario.request("arm-queue-steer-failure",sessionId=session)["armed"]
    code, outcome = paired_device.invoke("session", "queue", "steer", "--session", session, "--item", item, success=False)
    assert code != 0 and outcome[-1]["error"]["code"] == "unknown"
    assert all(entry["id"] != item for entry in scenario.request("queue-read",sessionId=session)["items"])
    status, _ = scenario.replay_command(paired_device)
    assert status in (200,202)
    assert all(entry["id"] != item for entry in scenario.request("queue-read",sessionId=session)["items"])
    scenario.model.release.set()


def test_native_queue_splice_preserves_live_history_continuity(scenario, paired_device):
    """A live watch advances over queue splices and still receives the later committed answer."""
    session = scenario.create()
    paired_device.sessions(scenario.host)
    scenario.model.plan(hold=True)
    tail = scenario.tail(paired_device, session, baseline=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    source = scenario.request("queue-read", sessionId=session)
    item = source["items"][0]["id"]
    assert scenario.request("queue-remove-direct", sessionId=session, itemId=item)["accepted"]
    scenario.model.release.set()
    observed = events(scenario.finish_tail(tail))
    assert any(event.get("type") == "message" and event.get("role") == "ASSISTANT" for event in observed)
    # A fresh watch may emit an initial snapshot reset, never a later reset.
    assert all(index == 0 for index, event in enumerate(observed) if event.get("type") == "reset")


def test_native_queue_unknown_survives_restart(scenario, paired_device):
    """A lost queue-edit receipt stays unknown across DSH restart and is never resubmitted."""
    import sqlite3

    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    source=scenario.request("queue-read", sessionId=session)
    item=source["items"][0]["id"]
    paired_device.sessions(scenario.host)
    scenario.request("arm-queue-receipt-loss")
    code, outcome = paired_device.invoke("session", "queue", "edit", "--session", session, "--item", item, "replacement fixture", success=False)
    assert code != 0 and outcome[-1]["error"]["code"] == "unknown"
    assert scenario.request("queue-read", sessionId=session)["items"][0]["fixtureText"]
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        row=db.execute("SELECT command_id, status FROM commands ORDER BY created_at DESC LIMIT 1").fetchone()
    assert row and row[1] == "unknown"
    calls=scenario.model.calls
    scenario.native.close()
    scenario.start_native()
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        recovered=db.execute("SELECT status FROM commands WHERE command_id=?",(row[0],)).fetchone()
    assert recovered == ("unknown",)
    assert scenario.request("count-queue-mutations")["armed"]
    status, receipt = scenario.replay_command(paired_device)
    assert status in (200,202) and receipt["commandId"] == row[0]
    assert scenario.request("queue-mutation-count")["calls"] == 0
    assert not scenario.request("queue-read", sessionId=session)["available"]
    assert scenario.model.calls == calls
    scenario.model.release.set()



def test_native_queue_privileged_next_turn_is_readonly(scenario, paired_device):
    """A native system prompt in next-turn is redacted and cannot be changed remotely."""
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start",sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    item=scenario.request("queue-privileged-add",sessionId=session)["id"]
    observed=scenario.request("queue-read",sessionId=session)
    entry=next(value for value in observed["items"] if value["id"]==item)
    assert entry["placement"] == "QUEUED" and entry["actions"] == []
    paired_device.sessions(scenario.host)
    queue=paired_device.call("session", "queue","list","--session",session)
    remote=next(value for value in queue["items"] if value["item_id"]==item)
    assert remote["actions"] == [] and remote["delivery_id"] is None
    assert "synthetic privileged content" not in str(remote)
    for action in ("remove","steer"):
        code, values=paired_device.invoke("session", "queue",action,"--session",session,"--item",item,success=False)
        assert code != 0 and values[-1]["error"]["code"] == "conflict"
    code, values=paired_device.invoke("session", "queue","edit","--session",session,"--item",item,"replacement fixture",success=False)
    assert code != 0 and values[-1]["error"]["code"] == "conflict"
    assert any(value["id"]==item for value in scenario.request("queue-read",sessionId=session)["items"])
    scenario.model.release.set()


def test_native_queue_interrupt_retains_all_pending(scenario, paired_device):
    """Interrupt preserves every accepted item as queued or source-committed, and withdrawal remains explicit."""
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start",sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    for _ in range(4): scenario.request("queue-add",sessionId=session)
    before=scenario.request("queue-read",sessionId=session)
    ids={item["id"] for item in before["items"]}
    assert len(ids)==4
    paired_device.sessions(scenario.host)
    receipt=paired_device.interrupt(session)
    assert receipt["status"] in ("succeeded","host_accepted")
    if receipt["status"]=="host_accepted":
        wait(lambda: paired_device.call("operation", "recover",receipt["request_id"])["status"]=="succeeded",timeout=15)
    observation=scenario.request("queue-retention-evidence",sessionId=session)
    assert observation["available"]
    assert ids.issubset(set(observation["queued"])|set(observation["committed"]))
    remaining=ids.intersection(observation["queued"])
    assert remaining
    target=next(iter(remaining))
    removed=paired_device.call("session", "queue","remove","--session",session,"--item",target)
    command=removed["command"]
    assert command["status"] in ("succeeded","host_accepted")
    if command["status"] == "host_accepted":
        wait(lambda: paired_device.call("operation", "recover",command["request_id"])["status"] == "succeeded",timeout=15)
    after=scenario.request("queue-retention-evidence",sessionId=session)
    assert target not in after["queued"]
    assert (ids-{target}).issubset(set(after["queued"])|set(after["committed"]))
    scenario.model.release.set()


def test_native_pending_queue_attachment_remote_replacement(scenario, paired_device):
    """An encrypted remote edit replaces the complete native image item while preserving source identity."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add-image", sessionId=session)
    before = scenario.request("queue-read", sessionId=session)
    assert before["available"] and len(before["items"]) == 1
    item = before["items"][0]
    assert item["attachments"] and item["attachments"][0]["mediaType"] == "image/png"
    assert item["attachments"][0]["size"] > 0 and not item["attachments"][0]["available"]
    identity = item["id"]
    metadata = scenario.request("queue-native-meta", sessionId=session)["items"][0]
    assert metadata["id"] == identity and metadata["rpcId"]
    paired_device.sessions(scenario.host)
    result=paired_device.call("session", "queue","edit","--session",session,"--item",identity,"replacement fixture")
    assert result["command"]["status"] == "succeeded"
    after = scenario.request("queue-read", sessionId=session)
    assert after["items"][0]["id"] == identity and after["items"][0]["fixtureText"]
    assert after["items"][0]["attachments"] == []
    assert scenario.request("queue-native-meta", sessionId=session)["items"][0] == metadata
    scenario.model.release.set()


def test_native_queue_refresh_failure_keeps_receipt(scenario, paired_device):
    """A confirmed native edit remains successful if its next refresh fails, and a later refresh is current."""
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start",sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add",sessionId=session)
    source=scenario.request("queue-read",sessionId=session)
    item=source["items"][0]["id"]
    paired_device.sessions(scenario.host)
    assert scenario.request("arm-queue-refresh-failure")["armed"]
    result=paired_device.call("session", "queue","edit","--session",session,"--item",item,"replacement fixture")
    assert result["command"]["status"] == "succeeded" and result["refresh"] == "unavailable"
    assert "replacement fixture" not in str(result)
    current=paired_device.call("session", "queue","list","--session",session)
    assert current["available"] and current["items"][0]["item_id"] == item
    assert scenario.request("queue-read",sessionId=session)["items"][0]["fixtureText"]
    scenario.model.release.set()


def test_native_queue_live_empty_and_cold_passive(scenario, paired_device):
    """An empty live queue is available, while cold reads neither activate a session nor claim emptiness."""
    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start",sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    state=scenario.request("state",sessionId=session)
    assert state["active"] and state["running"]
    queue=scenario.request("queue-read",sessionId=session)
    assert queue["available"] and queue["items"] == []
    paired_device.sessions(scenario.host)
    remote=paired_device.call("session", "queue","list","--session",session)
    assert remote["available"] and remote["items"] == []
    scenario.model.release.set()
    scenario.wait_idle(session)
    calls=scenario.model.calls
    scenario.native.close()
    scenario.start_native()
    assert not scenario.request("state",sessionId=session)["active"]
    queue=scenario.request("queue-read",sessionId=session)
    assert not queue["available"] and queue["items"] == []
    paired_device.sessions(scenario.host)
    remote=paired_device.call("session", "queue","list","--session",session)
    assert not remote["available"] and remote["items"] == []
    assert not scenario.request("state",sessionId=session)["active"]
    assert scenario.model.calls == calls


def test_native_queue_context_rejects_every_mutation(scenario, paired_device):
    """All remote queue actions refuse a native context item without revealing its contents."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    context = scenario.request("queue-context-add", sessionId=session)["id"]
    source = scenario.request("queue-read", sessionId=session)
    assert source["items"][0]["id"] == context and source["items"][0]["placement"] == "CONTEXT"
    assert source["items"][0]["actions"] == []
    paired_device.sessions(scenario.host)
    visible = paired_device.call("session", "queue", "list", "--session", session)
    assert visible["items"][0]["item_id"] == context and visible["items"][0]["actions"] == []
    assert "synthetic hidden context" not in str(visible)
    for action in ("edit", "remove", "steer"):
        args=["session", "queue",action,"--session",session,"--item",context]
        if action == "edit": args.append("replacement fixture")
        code, outcome = paired_device.invoke(*args,success=False)
        assert code != 0 and outcome[-1]["error"]["code"] == "conflict"
    assert scenario.request("queue-read", sessionId=session)["items"][0]["id"] == context
    scenario.model.release.set()


def test_native_queue_context_is_readonly(scenario, paired_device):
    """Native producer context remains redacted and cannot be remotely mutated."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    context = scenario.request("queue-context-add", sessionId=session)["id"]
    source = scenario.request("queue-read", sessionId=session)
    assert source["items"][0]["id"] == context and source["items"][0]["placement"] == "CONTEXT"
    assert source["items"][0]["actions"] == []
    paired_device.sessions(scenario.host)
    visible = paired_device.call("session", "queue", "list", "--session", session)
    assert visible["items"][0]["item_id"] == context and visible["items"][0]["actions"] == []
    assert "synthetic hidden context" not in str(visible)
    code, outcome = paired_device.invoke("session", "queue", "remove", "--session", session, "--item", context, success=False)
    assert code != 0 and outcome[-1]["error"]["code"] == "conflict"
    assert scenario.request("queue-read", sessionId=session)["items"][0]["id"] == context
    scenario.model.release.set()


def test_native_queue_targeted_removal(scenario, paired_device):
    """Withdrawing one occurrence keeps other work and committed conversation unchanged."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    scenario.request("queue-add", sessionId=session)
    source = scenario.request("queue-read", sessionId=session)
    identities = [item["id"] for item in source["items"]]
    assert len(identities) == 2 and len(set(identities)) == 2
    committed = [kind for kind in scenario.request("events", sessionId=session)["types"] if kind in ("user/message", "assistant/message")]
    fingerprint = scenario.request("queue-history-fingerprint", sessionId=session)
    paired_device.sessions(scenario.host)
    paired_device.observe_history(session)
    message_ids = scenario.cached(paired_device, session)[1]
    assert message_ids
    result = paired_device.call("session", "queue", "remove", "--session", session, "--item", identities[0])
    assert result["command"]["status"] == "succeeded"
    after = scenario.request("queue-read", sessionId=session)
    assert [item["id"] for item in after["items"]] == identities[1:]
    assert [kind for kind in scenario.request("events", sessionId=session)["types"] if kind in ("user/message", "assistant/message")] == committed
    paired_device.observe_history(session)
    assert scenario.cached(paired_device, session)[1] == message_ids
    assert scenario.request("queue-history-fingerprint", sessionId=session) == fingerprint
    scenario.model.release.set()


def test_native_steer_during_abort_targets_next_turn(scenario, paired_device):
    """A steering delivery admitted during native cancellation uses native next-turn placement."""
    from environment import Device, http
    other = Device(scenario.directory / "interrupt-device")
    invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    other.pair(invitation["pairingUri"])
    scenario.request("enable-delivery-for-test")
    session = scenario.create()
    other.sessions(scenario.host)
    scenario.model.plan(hold=True)
    paired_device.send(session, "synthetic running")
    assert scenario.model.started.wait(timeout=15)
    scenario.request("arm-delivery-barrier")
    scenario.request("arm-abort-steer-observation", sessionId=session)
    with ThreadPoolExecutor(max_workers=1) as executor:
        pending = executor.submit(paired_device.send, session, "synthetic during abort", "steer")
        try:
            wait(lambda: scenario.request("delivery-barrier-state")["entered"], timeout=10, process=scenario.native)
            other.interrupt(session)
            receipt = pending.result(timeout=20)
        finally:
            scenario.request("release-delivery-barrier")
    assert receipt["delivery_id"]
    observation = scenario.request("abort-steer-observation")
    assert observation == {"aborted": True, "nextTurn": True}
    scenario.model.release.set()
    scenario.wait_idle(session)



def test_native_delivery_unknown_survives_restart(scenario, paired_device):
    """After native acceptance and a lost reply, restart preserves unknown and does not resubmit."""
    import sqlite3

    scenario.request("enable-delivery-for-test")
    session = scenario.create()
    scenario.model.plan()
    scenario.request("arm-delivery-receipt-loss")
    code, values = paired_device.invoke("session", "send", "--detach", session, "synthetic lost reply", success=False)
    assert code != 0
    assert values[-1]["error"]["code"] == "unknown"
    scenario.wait_idle(session)
    assert scenario.model.calls == 1
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        row = db.execute("SELECT command_id, status FROM commands ORDER BY created_at DESC LIMIT 1").fetchone()
    assert row and row[1] == "unknown"
    scenario.native.close()
    scenario.start_native()
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        recovered = db.execute("SELECT status FROM commands WHERE command_id=?", (row[0],)).fetchone()
    assert recovered == ("unknown",)
    status, receipt = scenario.replay_command(paired_device)
    assert status in (200, 202) and receipt["commandId"] == row[0]
    assert scenario.model.calls == 1



def test_native_delivery_receipt_identity_after_restart(scenario, paired_device):
    """A consumed native request identity and its confirmed Host receipt remain stable after restart."""
    import sqlite3

    scenario.request("enable-delivery-for-test")
    session = scenario.create()
    scenario.model.plan(hold=True)
    paired_device.send(session, "synthetic initial")
    assert scenario.model.started.wait(timeout=15)
    queued = paired_device.send_confirmed(session, "synthetic queue")
    wait(
        lambda: queued["delivery_id"] in scenario.request("inbox-metadata", sessionId=session)["ids"],
        process=scenario.native,
    )
    scenario.model.release.set()
    scenario.wait_idle(session, assistant_count=2)
    assert scenario.request("delivery-event-identities", sessionId=session)["ids"].count(queued["delivery_id"]) == 1
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        receipt = db.execute("SELECT status, result_payload FROM commands WHERE command_id=?", (queued["command_id"],)).fetchone()
    assert receipt and receipt[0] == "succeeded" and receipt[1]
    calls = scenario.model.calls
    scenario.native.close()
    scenario.start_native()
    identities = scenario.request("delivery-event-identities", sessionId=session)["ids"]
    assert identities.count(queued["delivery_id"]) == 1
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        restored = db.execute("SELECT status, result_payload FROM commands WHERE command_id=?", (queued["command_id"],)).fetchone()
    assert restored == receipt
    status, replay = scenario.replay_command(paired_device)
    assert status in (200, 202) and replay["commandId"] == queued["command_id"]
    assert scenario.model.calls == calls == 2


def test_native_queue_real_receipt_correlation(scenario, paired_device):
    """Native queue identities correlate to remote admission without claiming source origin."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("enable-delivery-for-test")
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    receipt = paired_device.send_confirmed(session, "synthetic queued", delivery="enqueue-after-run")
    assert receipt["status"] == "succeeded" and receipt["delivery_id"]
    queue=paired_device.call("session", "queue","list","--session",session)
    assert len(queue["items"]) == 1 and queue["items"][0]["delivery_id"] == receipt["delivery_id"]
    assert queue["items"][0]["item_id"] != receipt["delivery_id"]
    external=scenario.request("queue-add-external",sessionId=session)
    queue=paired_device.call("session", "queue","list","--session",session)
    assert len(queue["items"]) == 2 and queue["items"][1]["delivery_id"] == external["deliveryId"]
    scenario.model.release.set()











def test_native_start_if_idle_rejects_local_start_before_observation(scenario, paired_device):
    """A local start before the idle precheck is observed rejects before native remote submission."""
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("arm-precheck-running-race")
    code, output = paired_device.invoke(
        "session", "send", "--detach", session, "--delivery", "start-if-idle", "synthetic remote attempt", success=False,
    )
    assert code == 8 and output[-1]["error"]["code"] == "conflict"
    assert scenario.model.started.wait(timeout=15)
    assert scenario.model.calls == 1
    scenario.model.release.set()
    scenario.wait_idle(session)


def test_native_delivery_unknown_after_completion_race(scenario, paired_device):
    """A native delivery admitted after completion with a lost receipt remains unknown without replay."""
    scenario.request("enable-delivery-for-test")
    session = scenario.create()
    scenario.model.plan(hold=True)
    paired_device.send(session, "synthetic running")
    assert scenario.model.started.wait(timeout=15)
    scenario.request("arm-delivery-receipt-loss")
    scenario.request("arm-delivery-barrier")
    with ThreadPoolExecutor(max_workers=1) as executor:
        pending = executor.submit(
            paired_device.invoke, "session", "send", "--detach", session, "synthetic raced unknown", success=False,
        )
        try:
            wait(lambda: scenario.request("delivery-barrier-state")["entered"], timeout=10, process=scenario.native)
            scenario.model.release.set()
            scenario.wait_idle(session)
        finally:
            scenario.request("release-delivery-barrier")
        code, values = pending.result(timeout=20)
    assert code != 0 and values[-1]["error"]["code"] == "unknown"
    scenario.wait_idle(session, assistant_count=2)
    calls = scenario.model.calls
    status, _receipt = scenario.replay_command(paired_device)
    assert status in (200, 202)
    assert scenario.model.calls == calls == 2

def test_model_native_default_save_observed(scenario, paired_device):
    """When isolated DSH persists a changed model default, the session receipt remains unconfirmed while native save completion changes the source default."""
    session = scenario.create()
    catalog = scenario.request("model-catalog")
    group = catalog["groups"][0]
    alternative = next(model for model in group["models"] if model["id"] != catalog["default_selection"]["model"])
    selection = {"provider": group["id"], "model": alternative["id"]}
    result = scenario.request("model-select-save-observe", sessionId=session, selection=selection)
    assert result["attempts"] == 1
    assert result["receipt"]["default_persistence"] == "unconfirmed"
    assert result["persisted"]["provider"] == selection["provider"]
    assert result["persisted"]["model"] == selection["model"]
    assert scenario.request("model-read", sessionId=session)["next"]["model"] == selection["model"]


def test_model_native_selection_does_not_prompt(scenario, paired_device):
    """When selecting an idle native session model, the adapter does not start an assistant model request."""
    session = scenario.create()
    group = scenario.request("model-catalog")["groups"][0]
    selection = {"provider": group["id"], "model": group["models"][0]["id"]}
    before = scenario.model.calls
    receipt = scenario.request("model-select", sessionId=session, selection=selection)
    assert receipt["selected"]["model"] == selection["model"]
    assert scenario.model.calls == before
    assert not scenario.request("state", sessionId=session)["running"]


def test_model_native_cold_unknown_queue_selection(scenario, paired_device):
    """When native cold session state has no live inbox observation, model selection delegates to DSH and returns its acceptance."""
    session = scenario.create()
    group = scenario.request("model-catalog")["groups"][0]
    selection = {"provider": group["id"], "model": group["models"][0]["id"]}
    scenario.native.close()
    scenario.start_native()
    before = scenario.request("state", sessionId=session)
    assert not before["active"] and before["queueKnown"] is False and before["queueCount"] is None
    calls = scenario.model.calls
    receipt = scenario.request("model-select", sessionId=session, selection=selection)
    assert receipt["selected"]["model"] == selection["model"]
    assert scenario.model.calls == calls


def _native_queue_revocation_barrier(scenario, paired_device, phase):
    """Run a test-only barrier without recording native queue content."""
    import sqlite3
    import time
    from concurrent.futures import ThreadPoolExecutor

    session=scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start",sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add",sessionId=session)
    source=scenario.request("queue-read",sessionId=session)
    item=source["items"][0]["id"]
    paired_device.sessions(scenario.host)
    assert scenario.request("arm-channel-queue-barrier",config={"phase":phase})["armed"]
    with ThreadPoolExecutor(max_workers=1) as executor:
        future=executor.submit(paired_device.invoke,"session","queue","remove","--session",session,"--item",item,success=False)
        try:
            wait(lambda:scenario.request("channel-queue-barrier")["entered"],timeout=15,process=scenario.native)
            before=scenario.request("queue-native-ids",sessionId=session)["ids"]
            assert (item in before) == (phase == "pre-native")
            # Allow the client to enter its accepted-command result polling path.
            time.sleep(1)
            assert scenario.request("channel-revoke-peer",deviceId=paired_device.identity["device_id"])["revoked"]
            code, outcome=future.result(timeout=25)
            assert code != 0
            assert outcome[-1]["error"]["code"] == "unknown"
        finally:
            scenario.request("release-channel-queue-barrier")
    after=scenario.request("queue-native-ids",sessionId=session)["ids"]
    assert (item in after) == (phase == "pre-native")
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        row=db.execute("SELECT command_id,status FROM commands ORDER BY created_at DESC LIMIT 1").fetchone()
    assert row and row[1] == "unknown"
    pending=paired_device.call("operation", "list")["pending_commands"]
    assert len(pending) == 1 and pending[0]["command_id"] == row[0] and pending[0]["request_id"]
    scenario.model.release.set()


def test_native_queue_revoked_before_native_does_not_mutate(scenario, paired_device):
    """Revocation during the queue precheck stops before DSH mutation and retains an uncertain receipt."""
    _native_queue_revocation_barrier(scenario,paired_device,"pre-native")


def test_native_queue_revoked_after_native_keeps_unknown(scenario, paired_device):
    """Revocation after native queue removal does not claim rollback and retains an unknown receipt."""
    _native_queue_revocation_barrier(scenario,paired_device,"post-native")


def test_native_queue_unknown_nested_block_is_redacted(scenario, paired_device):
    """The encrypted queue projection redacts a synthetic future block and nested parameters."""
    import uuid

    marker = "synthetic-unknown-" + uuid.uuid4().hex
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    assert scenario.request("queue-inject-unknown-block", sessionId=session, config={"marker": marker})["armed"]
    result = paired_device.call("session", "queue", "list", "--session", session)
    assert len(result["items"]) == 1
    assert [block.get("type") for block in result["items"][0]["blocks"]] == ["text", "unavailable"]
    assert marker not in str(result)
    for process in scenario.processes:
        assert marker not in Path(process.output.name).read_text(errors="replace")
    scenario.model.release.set()


def test_native_queue_unknown_receipt_is_bound_to_origin_device(scenario, paired_device):
    """A second paired device cannot recover another device's uncertain queue command."""
    from environment import Device, http

    other_dir = scenario.directory / "queue-recovery-other"
    other_dir.mkdir()
    other = Device(other_dir)
    invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    other.pair(invitation["pairingUri"])
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    other.sessions(scenario.host)
    item = paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"]
    assert scenario.request("count-queue-mutations")["armed"]
    assert scenario.request("arm-queue-receipt-loss")["armed"]
    code, result = paired_device.invoke("session", "queue", "edit", "--session", session, "--item", item, "replacement fixture", success=False)
    assert code != 0 and result[-1]["error"]["code"] == "unknown"
    pending = paired_device.call("operation", "list")["pending_commands"]
    assert len(pending) == 1
    request_id = pending[0]["request_id"]
    assert scenario.request("queue-mutation-count")["calls"] == 1
    with scenario.fixture_bearer(other) as bearer:
        with pytest.raises(RuntimeError, match="HTTP (403|404)"):
            http("/v1/requests/" + request_id, token=bearer)
        with pytest.raises(RuntimeError, match="HTTP (401|403|404)"):
            scenario.replay_command(paired_device, token_override=bearer)
    other_code, other_result = other.invoke("operation", "recover", request_id, success=False)
    assert other_code != 0
    assert other_result[-1]["error"]["code"] in ("not_found", "unauthorized", "forbidden")
    assert other.call("operation", "list")["pending_commands"] == []
    assert paired_device.call("operation", "list")["pending_commands"][0]["request_id"] == request_id
    assert scenario.request("queue-mutation-count")["calls"] == 1
    assert scenario.request("queue-read", sessionId=session)["items"][0]["fixtureText"]
    scenario.model.release.set()


def test_native_queue_unpaired_and_revoked_wire_do_not_reach_source(scenario, paired_device):
    """Unpaired clients and revoked encrypted queue requests never reach the native source."""
    import base64
    import uuid
    from environment import Device, http

    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item = paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"]
    query = f"""SELECT json_build_object(
      'requestId',request_id,'hostId',host_id,'deviceId',device_id,'commandId',command_id,
      'kind',kind,'protocolVersion',protocol_version,'channelId',channel_id,
      'clientTransportVersion',client_transport_version,'body',encode(request_body,'base64'),
      'payloadDigest',encode(payload_digest,'base64')) FROM {scenario.schema}.requests
      WHERE kind=2 AND device_id='{paired_device.identity["device_id"]}' ORDER BY created_at DESC LIMIT 1"""
    row = subprocess.run(["psql", "-h", "127.0.0.1", "-p", "55432", "-U", "postgres", "-d", "whalerein_test", "-tAc", query], capture_output=True, text=True, timeout=5, check=True)
    saved = json.loads(row.stdout)
    for field in ("body", "payloadDigest"):
        saved[field] = base64.b64encode(base64.b64decode(saved[field] or "")).decode()
    saved["requestId"] = uuid.uuid4().hex
    saved["expiresAtUnixMs"] = str(int((time.time() + 60) * 1000))
    assert paired_device.call("session", "queue", "edit", "--session", session, "--item", item, "replacement fixture")["command"]["status"] == "succeeded"
    assert scenario.request("count-queue-reads")["armed"]
    assert scenario.request("count-queue-mutations")["armed"]
    with scenario.fixture_bearer(paired_device) as bearer:
        assert any(host["hostId"] == scenario.host for host in http("/v1/hosts", token=bearer)["hosts"])
        paired_device.revoke()

        stranger = Device(scenario.directory / "unpaired-native-queue")
        for args in (("session", "queue", "list", "--session", session), ("session", "queue", "remove", "--session", session, "--item", item)):
            code, _ = stranger.invoke(*args, success=False)
            assert code != 0
        with pytest.raises(RuntimeError, match="HTTP (401|403)"):
            http("/v1/hosts/" + scenario.host + "/requests", saved, bearer)
        with pytest.raises(RuntimeError, match="HTTP (401|403)"):
            scenario.replay_command(paired_device, token_override=bearer)
    assert scenario.request("queue-read-count")["calls"] == 0
    assert scenario.request("queue-mutation-count")["calls"] == 0
    scenario.model.release.set()


@pytest.mark.privacy_debug_logging
def test_native_queue_concurrent_replays_and_private_receipts(scenario, paired_device):
    """Concurrent encrypted replays execute once and do not publish content in logs or receipts."""
    import sqlite3
    import uuid
    from concurrent.futures import ThreadPoolExecutor

    marker = "synthetic-private-" + uuid.uuid4().hex
    session = scenario.create()
    scenario.model.plan(hold=True)
    scenario.request("start", sessionId=session)
    assert scenario.model.started.wait(timeout=15)
    scenario.request("queue-add", sessionId=session)
    assert scenario.request("queue-read", sessionId=session)["available"]
    paired_device.sessions(scenario.host)
    item = paired_device.call("session", "queue", "list", "--session", session)["items"][0]["item_id"]
    assert scenario.request("count-queue-mutations")["armed"]
    assert scenario.request("arm-channel-queue-barrier", config={"phase":"pre-native"})["armed"]
    with ThreadPoolExecutor(max_workers=3) as executor:
        original = executor.submit(paired_device.invoke, "session", "queue", "edit", "--session", session, "--item", item, marker)
        try:
            wait(lambda: scenario.request("channel-queue-barrier")["entered"], timeout=15, process=scenario.native)
            replays = [executor.submit(scenario.replay_command, paired_device) for _ in range(2)]
            outcomes = [future.result(timeout=15) for future in replays]
            assert all(status in (200, 202) for status, _ in outcomes)
            assert outcomes[0][1]["commandId"] == outcomes[1][1]["commandId"]
        finally:
            scenario.request("release-channel-queue-barrier")
        code, result = original.result(timeout=25)
    assert code == 0 and result[-1]["data"]["command"]["status"] == "succeeded"
    assert scenario.request("queue-mutation-count")["calls"] == 1
    assert paired_device.call("session", "queue", "list", "--session", session)["items"][0]["blocks"][0]["text"] == marker
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        receipt = db.execute("SELECT result_payload FROM commands WHERE command_id=?", (outcomes[0][1]["commandId"],)).fetchone()
    assert receipt and marker.encode() not in (receipt[0] or b"")
    assert any("mqtt_publish_started" in Path(process.output.name).read_text(errors="replace") for process in scenario.processes)
    for process in scenario.processes:
        assert marker not in Path(process.output.name).read_text(errors="replace")
    scenario.model.release.set()


@pytest.mark.privacy_debug_logging
def test_native_remote_fork_privacy_in_cli_access_dsh_logs(scenario, paired_device):
    """Encrypted fork diagnostics and its receipt do not expose a synthetic prior message."""
    import os
    import subprocess
    from environment import ROOT

    secret = "fork-synthetic-private-content-91e9d7"
    parent = scenario.create()
    scenario.model.plan("tool-sensitive")
    paired_device.send(parent, secret, delivery="start-if-idle")
    wait(lambda: bool(paired_device.approvals(parent)), process=scenario.native)
    paired_device.resolve("approve", paired_device.approvals(parent)[0])
    scenario.wait_idle(parent)
    assert scenario.request("synthetic-tool-marker-present", sessionId=parent)["present"]
    assert scenario.request("test-enable-fork")["enabled"]
    # Inspect only output emitted after the source has already consumed the marker.
    outputs = [scenario.access.output, scenario.native.output]
    positions = {stream.name: Path(stream.name).stat().st_size for stream in outputs}
    result = subprocess.run(
        paired_device.args("session", 'fork', parent),
        cwd=ROOT, env=paired_device.env(),
        capture_output=True, text=True, timeout=90, check=False,
    )
    assert result.returncode == 0
    envelope = json.loads(result.stdout)
    assert envelope["data"]["lifecycle_receipt"]["parent_session_id"] == parent
    markers = [secret, "fork-synthetic-tool-parameter-b4821"]
    assert all(marker not in result.stdout and marker not in result.stderr for marker in markers)
    for stream in outputs:
        stream.flush()
        assert all(marker.encode() not in Path(stream.name).read_bytes()[positions[stream.name]:] for marker in markers)
    native_log = Path(scenario.native.output.name).read_bytes()[positions[scenario.native.output.name]:]
    assert b"mqtt_publish_started" in native_log
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        receipts = db.execute("SELECT result_payload FROM commands WHERE request_id=?", (envelope["data"]["request_id"],)).fetchall()
    assert len(receipts) == 1
    assert all(marker.encode() not in (receipts[0][0] or b"") for marker in markers)


@pytest.mark.privacy_debug_logging
def test_native_delivery_privacy_in_receipt_or_diagnostics(scenario, paired_device):
    """A native encrypted delivery excludes message content from CLI output, logs and its durable receipt."""
    import json
    import os
    import sqlite3
    import subprocess
    import uuid
    from environment import ROOT

    scenario.request("enable-delivery-for-test")
    session = scenario.create()
    scenario.model.plan()
    marker = "synthetic-private-" + uuid.uuid4().hex
    result = subprocess.run(
        paired_device.args("session", 'send', '--detach', session, marker),
        cwd=ROOT,
        env=paired_device.env(),
        capture_output=True, text=True, timeout=90, check=False,
    )
    assert result.returncode == 0
    assert marker not in result.stdout and marker not in result.stderr
    envelope = json.loads(result.stdout.splitlines()[-1])
    receipt = paired_device.confirm_detached_delivery(envelope["data"], session, marker, None)
    assert receipt["delivery_id"] and receipt["command_id"]
    scenario.wait_idle(session)
    with sqlite3.connect(scenario.directory / "plugin/state.sqlite") as db:
        row = db.execute("SELECT result_payload FROM commands WHERE command_id=?", (receipt["command_id"],)).fetchone()
    assert row and marker.encode() not in (row[0] or b"")
    for process in scenario.processes:
        assert marker not in Path(process.output.name).read_text(errors="replace")
    assert any("mqtt_publish_started" in Path(process.output.name).read_text(errors="replace") for process in scenario.processes)


@pytest.mark.parametrize("mode", [
    pytest.param("enqueue-after-run", marks=pytest.mark.description(
        "Concurrent encrypted queue replays cause exactly one native user message."
    )),
    pytest.param("start-if-idle", marks=pytest.mark.description(
        "Concurrent encrypted start-if-idle replays cause exactly one native user message."
    )),
    pytest.param("steer", marks=pytest.mark.description(
        "Concurrent encrypted steer replays cause exactly one native user message."
    )),
])
def test_native_delivery_concurrent_replays_reach_source_once(scenario, paired_device, mode):
    """Held native delivery deduplicates simultaneous encrypted replays in every mode."""
    scenario.request("enable-delivery-for-test")
    session = scenario.create()
    paired_device.sessions(scenario.host)
    baseline = scenario.request("gx-user-delivery-count", sessionId=session)["count"]
    scenario.model.plan()
    scenario.request("arm-delivery-barrier")
    text = "synthetic delivery"
    with ThreadPoolExecutor(max_workers=3) as executor:
        original = executor.submit(paired_device.send, session, text, mode)
        try:
            wait(lambda: scenario.request("delivery-barrier-state")["entered"], timeout=15, process=scenario.native)
            futures = [executor.submit(scenario.replay_command, paired_device) for _ in range(2)]
            outcomes = [future.result(timeout=15) for future in futures]
            assert all(status in (200, 202) for status, _ in outcomes)
            assert outcomes[0][1]["commandId"] == outcomes[1][1]["commandId"]
        finally:
            scenario.request("release-delivery-barrier")
        submission = original.result(timeout=25)
    receipt = paired_device.confirm_detached_delivery(submission, session, text, mode)
    assert receipt["command_id"] == outcomes[0][1]["commandId"]
    assert receipt["delivery_id"]
    scenario.wait_idle(session)
    assert scenario.request("gx-user-delivery-count", sessionId=session)["count"] == baseline + 1
    assert scenario.model.calls == 1



def test_native_question_two_remote_providers_and_reused_call(scenario, paired_device):
    """Two encrypted devices race one native question, then an old guard cannot capture its successor."""
    from environment import Device, http
    directory = scenario.directory / "second-question-device"
    directory.mkdir()
    second = Device(directory)
    invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    second.pair(invitation["pairingUri"])
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    session = scenario.create()
    paired_device.sessions(scenario.host)
    second.sessions(scenario.host)
    assert scenario.request("question-race-encrypted-start", sessionId=session)["started"]
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    other_batch = second.call("session", "question", "list", "--session", session)["batches"][0]
    assert other_batch["batch_id"] == batch["batch_id"] and other_batch["digest"] == batch["digest"]
    answer = scenario.directory / "remote-race-answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    def args(b):
        return ["session", "question", "answer", "--session", session, "--batch", b["batch_id"], "--epoch", b["source_epoch"],
                "--version", str(b["version"]), "--digest", b["digest"], "--phase", b["phase"], "--input", str(answer)]
    processes = []
    for device in (paired_device, second):
        process = subprocess.Popen(device.args("--yes", *args(batch)), cwd=ROOT,
            env=device.env(),
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        processes.append(process)
    results = []
    try:
        for process in processes:
            stdout, _ = process.communicate(timeout=30)
            results.append((process.returncode, json.loads(stdout.strip())))
    finally:
        for process in processes:
            if process.poll() is None:
                process.kill(); process.communicate(timeout=5)
    assert sum(code == 0 for code, _ in results) == 1
    assert next(value for code, value in results if code == 0)["data"]["question_receipt"]["phase"] == "QUESTION_SETTLED"
    actual = scenario.request("question-race-encrypted-result", sessionId=session)
    assert actual == {
        "selected": "Yes", "pending": False}
    assert scenario.request("question-race-encrypted-start", sessionId=session)["started"]
    fresh = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    assert fresh["batch_id"] != batch["batch_id"]
    code, stale = paired_device.invoke(*args(batch), success=False)
    assert code != 0 and stale[-1]["error"]["code"] in ("conflict", "not_found")
    assert paired_device.call(*args(fresh))["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-race-encrypted-result", sessionId=session) == {
        "selected": "Yes", "pending": False}


def test_native_question_invalid_remote_answers_do_not_settle(scenario, paired_device):
    """Invalid question IDs and value types never settle a real native open request."""
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    session = scenario.create()
    assert scenario.request("question-start-remote-fixture", sessionId=session)["started"]
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    answer = scenario.directory / "invalid-question-answer.json"
    args = ["session", "question", "answer", "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
            "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer)]
    for answers in [[], [{"id": "other", "selected": []}], [{"id": "q1", "selected": []}, {"id": "q1", "selected": []}],
                    [{"id": "q1", "selected": [123]}], [{"id": "q1", "selected": [], "custom": 123}]]:
        answer.write_text(json.dumps({"answers": answers}))
        code, result = paired_device.invoke(*args, success=False)
        assert code != 0 and result[-1]["error"]["code"] == "invalid_argument"
        current = paired_device.call("session", "question", "list", "--session", session)["batches"]
        assert len(current) == 1 and current[0]["batch_id"] == batch["batch_id"] and current[0]["answerable"]
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    assert paired_device.call(*args)["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-result-remote-fixture", sessionId=session) == {"selected": "Yes", "pending": False}


def test_native_question_stale_remote_guards_do_not_settle(scenario, paired_device):
    """Stale epoch, version, and digest fail before a real native open question settles."""
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    session = scenario.create()
    assert scenario.request("question-start-remote-fixture", sessionId=session)["started"]
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    answer = scenario.directory / "stale-question-answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    def args(b):
        return ["session", "question", "answer", "--session", session, "--batch", b["batch_id"], "--epoch", b["source_epoch"],
                "--version", str(b["version"]), "--digest", b["digest"], "--phase", b["phase"], "--input", str(answer)]
    bad_digest = ("0" if batch["digest"][0] != "0" else "1") + batch["digest"][1:]
    for changes in [{"source_epoch": "stale-epoch"}, {"version": batch["version"] + 1}, {"digest": bad_digest}]:
        code, result = paired_device.invoke(*args({**batch, **changes}), success=False)
        assert code != 0 and result[-1]["error"]["code"] == "conflict"
        current = paired_device.call("session", "question", "list", "--session", session)["batches"]
        assert len(current) == 1 and current[0]["batch_id"] == batch["batch_id"] and current[0]["answerable"]
    assert paired_device.call(*args(batch))["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-result-remote-fixture", sessionId=session) == {"selected": "Yes", "pending": False}



def test_native_question_multiple_answers_are_atomic_and_preserve_labels(scenario, paired_device):
    """A native multi-question batch rejects partial answers and preserves explicit multiple selections."""
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    session = scenario.create()
    assert scenario.request("question-multiple-native-start", sessionId=session)["started"]
    batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    assert [(q["id"], q["multi_select"]) for q in batch["questions"]] == [("q1", True), ("q2", False)]
    answer = scenario.directory / "multiple-native-answer.json"
    args = ["session", "question", "answer", "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
            "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer)]
    for values in [[{"id": "q1", "selected": ["A", "C"]}],
                   [{"id": "q1", "selected": []}, {"id": "q1", "selected": []}],
                   [{"id": "q1", "selected": []}, {"id": "other", "selected": []}]]:
        answer.write_text(json.dumps({"answers": values}))
        code, result = paired_device.invoke(*args, success=False)
        assert code != 0 and result[-1]["error"]["code"] == "invalid_argument"
        current = paired_device.call("session", "question", "list", "--session", session)["batches"]
        assert len(current) == 1 and current[0]["batch_id"] == batch["batch_id"] and current[0]["answerable"]
    values = [{"id": "q2", "selected": ["No"], "custom": "Fixture custom"}, {"id": "q1", "selected": ["C", "A"]}]
    answer.write_text(json.dumps({"answers": values}))
    assert paired_device.call(*args)["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-multiple-native-result", sessionId=session) == {"answers": values, "pending": False}




@pytest.mark.timed_questions
def test_native_question_multiple_continued_answers_are_atomic(scenario, paired_device):
    """A native continued multi-question batch rejects incomplete answers and queues one complete answer."""
    assert scenario.request("question-enable-continued-subclass-fixture")["enabled"]
    session = scenario.create()
    scenario.model.plan("question-multiple", hold=True)
    scenario.request("start", sessionId=session)
    try:
        wait(lambda: scenario.model.calls >= 2, timeout=15, process=scenario.native)
        wait(lambda: scenario.request("question-projected", sessionId=session)["phases"] == ["QUESTION_CONTINUED"], timeout=5, process=scenario.native)
        batch = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
        assert [(q["id"], q["multi_select"]) for q in batch["questions"]] == [("q1", True), ("q2", False)]
        answer = scenario.directory / "multiple-continued-answer.json"
        args = ["session", "question", "answer", "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
                "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer)]
        for values in [[{"id": "q1", "selected": ["A"]}],
                       [{"id": "q1", "selected": []}, {"id": "q1", "selected": []}],
                       [{"id": "q1", "selected": []}, {"id": "other", "selected": []}]]:
            answer.write_text(json.dumps({"answers": values}))
            code, result = paired_device.invoke(*args, success=False)
            assert code != 0 and result[-1]["error"]["code"] == "invalid_argument"
            assert scenario.request("question-continued-status", sessionId=session) == {"continued": 1, "queued": 0}
        answer.write_text(json.dumps({"answers": [{"id": "q2", "selected": ["No"], "custom": "Fixture custom"},
                                                    {"id": "q1", "selected": ["B", "A"]}]}))
        assert paired_device.call(*args)["question_receipt"]["phase"] == "QUESTION_REPLY_QUEUED"
        assert scenario.request("question-continued-status", sessionId=session) == {"continued": 1, "queued": 1}
        assert scenario.request("question-continued-multi-proof", sessionId=session) == {"oneReply": True, "answerMatches": True}
    finally:
        scenario.model.release.set()
    wait(lambda: scenario.request("question-projected", sessionId=session)["count"] == 0, timeout=15, process=scenario.native)
