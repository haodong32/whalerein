"""Independent CLI journeys through the real Linux product stack."""

import json
import os
import signal
import subprocess
from pathlib import Path
import pytest
import shutil

from environment import Device
from environment import ROOT, wait

pytestmark = pytest.mark.chain


def test_continued_question(scenario, paired_device, session_id):
    """A guarded explicit answer crosses the encrypted chain once and reports queued admission."""
    scenario.dsh.process.control("request_question", session_id=session_id)
    batches = paired_device.call("questions", "--session", session_id)["batches"]
    assert len(batches) == 1 and batches[0]["answerable"]
    batch = batches[0]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    result = paired_device.call("answer", "--session", session_id, "--batch", batch["batch_id"],
                                "--epoch", batch["source_epoch"], "--version", str(batch["version"]),
                                "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer))
    assert result["question_receipt"]["phase"] == "QUESTION_REPLY_QUEUED"
    assert paired_device.call("questions", "--session", session_id)["batches"] == []


def test_open_question(scenario, paired_device, session_id):
    """An explicit guarded open answer crosses the encrypted chain and reports a settled receipt."""
    scenario.dsh.process.control("request_question_open", session_id=session_id)
    batch = paired_device.call("questions", "--session", session_id)["batches"][0]
    assert batch["answerable"] and batch["phase"] == "QUESTION_OPEN"
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    result = paired_device.call("answer", "--session", session_id, "--batch", batch["batch_id"],
                                "--epoch", batch["source_epoch"], "--version", str(batch["version"]),
                                "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer))
    assert result["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert paired_device.call("questions", "--session", session_id)["batches"] == []


def test_open_question_client_cancel(scenario, paired_device, session_id):
    """An interrupted CLI explicitly cancels its in-flight open answer and leaves the question available."""
    scenario.dsh.process.control("request_question_open", session_id=session_id)
    scenario.dsh.process.control("hold_question_answer", session_id=session_id)
    batch = paired_device.call("questions", "--session", session_id)["batches"][0]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    command_id = "fixture-cancel-command"
    process = subprocess.Popen([
        str(ROOT / "target/debug/whalerein"), "--json", "--yes", "--data-dir", str(paired_device.directory / "client"),
        "answer", "--session", session_id, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
        "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"],
        "--input", str(answer), "--command-id", command_id,
    ], cwd=ROOT, env={**os.environ, "WHALEREIN_SECRET_BACKEND": "file:" + str(paired_device.directory / "secrets"),
                       "WHALEREIN_LOG": "error"}, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    diagnostic = {"stage": "starting"}
    try:
        wait(lambda: scenario.dsh.process.control("question_answer_waiting", session_id=session_id), timeout=30)
        diagnostic["stage"] = "in_flight"
        process.send_signal(signal.SIGINT)
        output, _ = process.communicate(timeout=30)
        diagnostic["stage"] = "cli_exited"
        diagnostic["returncode"] = process.returncode
        try:
            parsed = json.loads(output.strip())
            diagnostic["code"] = parsed.get("error", {}).get("code")
            diagnostic["has_command"] = command_id in parsed.get("error", {}).get("message", "")
        except ValueError:
            diagnostic["code"] = "invalid_output"
        assert process.returncode == 7
        envelope = json.loads(output.strip())
        assert envelope["error"]["code"] == "unknown" and command_id in envelope["error"]["message"]
        wait(lambda: not scenario.dsh.process.control("question_answer_waiting", session_id=session_id), timeout=5)
        diagnostic["stage"] = "claim_released"
        assert paired_device.call("questions", "--session", session_id)["batches"][0]["batch_id"] == batch["batch_id"]
    finally:
        Path("/reports/question-cancel-diagnostic.json").write_text(json.dumps(diagnostic))
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_answer_reads_stdin_and_rejects_unusable_input(scenario, paired_device, session_id):
    """An answer reads its payload from stdin, and empty or malformed stdin is a usage error that submits nothing."""
    scenario.dsh.process.control("request_question_open", session_id=session_id)
    batch = paired_device.call("questions", "--session", session_id)["batches"][0]
    guard = ["--session", session_id, "--batch", batch["batch_id"],
             "--epoch", batch["source_epoch"], "--version", str(batch["version"]),
             "--digest", batch["digest"], "--phase", batch["phase"], "--input", "-"]
    for invalid in ("", "{not json"):
        code, values = paired_device.invoke("answer", *guard, stdin_text=invalid, success=False)
        assert code == 2 and values[-1]["error"]["code"] == "invalid_argument"
        assert values[-1].get("data") is None
    still_open = paired_device.call("questions", "--session", session_id)["batches"]
    assert [entry["batch_id"] for entry in still_open] == [batch["batch_id"]]
    receipt = paired_device.invoke(
        "answer", *guard, stdin_text=json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]})
    )[1][-1]["data"]
    assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert paired_device.call("questions", "--session", session_id)["batches"] == []


def test_question_payload_pages_and_oversized_detail(scenario, paired_device, session_id):
    """Large complete question pages keep every batch and oversized detail is explicitly unavailable."""
    scenario.dsh.process.control("request_question_pages", session_id=session_id)
    first = paired_device.call("questions", "--session", session_id)
    assert len(first["batches"]) == 1 and first["next_cursor"]
    assert not first["batches"][0]["content_unavailable"]
    second = paired_device.call("questions", "--session", session_id, "--cursor", first["next_cursor"])
    assert len(second["batches"]) == 1 and not second["next_cursor"]
    assert first["batches"][0]["batch_id"] != second["batches"][0]["batch_id"]
    scenario.dsh.process.control("request_question_oversized", session_id=session_id)
    page = paired_device.call("questions", "--session", session_id)
    batch = page["batches"][0]
    assert batch["content_unavailable"] and not batch["answerable"] and batch["questions"] == []
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": []}]}))
    code, result = paired_device.invoke("answer", "--session", session_id, "--batch", batch["batch_id"],
        "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
        "--phase", batch["phase"], "--input", str(answer), success=False)
    assert code != 0 and result[-1]["error"]["code"] == "payload_too_large"


def test_question_answer_replay_across_plugin_restart(scenario, paired_device, session_id):
    """Repeated and changed-digest question controls never invoke the source a second time, even after Host restart."""
    scenario.dsh.process.control("request_question_open", session_id=session_id)
    batch = paired_device.call("questions", "--session", session_id)["batches"][0]
    answer = scenario.directory / "answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    receipt = paired_device.call("answer", "--session", session_id, "--batch", batch["batch_id"],
        "--epoch", batch["source_epoch"], "--version", str(batch["version"]), "--digest", batch["digest"],
        "--phase", batch["phase"], "--input", str(answer))
    assert receipt["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.dsh.process.control("question_answer_calls") == 1
    status, replay = scenario.replay_command(paired_device)
    assert status in (200, 202) and replay["commandId"] == receipt["command_id"]
    assert scenario.replay_command(paired_device, changed=True)[0] == 409
    scenario.plugin.close(); scenario.start_plugin()
    assert scenario.replay_command(paired_device)[0] in (200, 202)
    assert scenario.dsh.process.control("question_answer_calls") == 1


def test_pair(scenario):
    """When an unpaired device accepts a valid invitation, its host-bound credentials authorize a session query."""
    device = scenario.pair()
    assert device.identity["device_id"]
    assert any(
        host.get("host_id", host.get("hostId")) == scenario.host
        for host in device.hosts()
    ), "Pairing did not authorize the invited host"
    assert device.sessions(scenario.host) == []


def test_hosts(scenario, paired_device):
    """When a device is paired with a connected host, hosts lists that host as online."""
    assert any(
        host.get("host_id", host.get("hostId")) == scenario.host and host["online"]
        for host in paired_device.hosts()
    )


def test_sessions(scenario, paired_device, session_id):
    """When a paired device queries a host containing a session, the result includes that
    session ID.
    """
    assert any(
        session.get("session_id", session.get("sessionId")) == session_id
        for session in paired_device.sessions(scenario.host)
    )


def test_model_selection(scenario, paired_device, session_id):
    """When a paired CLI selects a FakeDSH model, a fresh read observes it and the receipt reports unconfirmed default persistence."""
    catalog = paired_device.call("models", "--host", scenario.host)
    assert catalog["groups"][0]["models"][0]["id"] == "fixture-model"
    initial = paired_device.call("model", "get", "--session", session_id)
    assert initial["pending"] is None
    receipt = paired_device.call("model", "set", "--session", session_id, "--provider", "fixture-provider", "--model", "fixture-model", "--effort", "high")
    assert receipt["status"] == "succeeded"
    assert receipt["model_selection"]["default_persistence"] == "unconfirmed"
    observed = paired_device.call("model", "get", "--session", session_id)
    assert observed["pending"]["effort"] == "high"
    assert scenario.dsh.snapshot(session_id)["events"] == []
    code, values = paired_device.invoke("model", "set", "--session", session_id, "--provider", "invalid", "--model", "invalid", success=False)
    assert code != 0 and values[-1]["error"]["code"] == "invalid_argument"
    assert paired_device.call("model", "get", "--session", session_id)["pending"]["effort"] == "high"
def test_wait_query(scenario, paired_device, session_id):
    """When a fake source is idle or running, the CLI reports typed wait observations and both timeout exit policies through the encrypted chain."""
    idle = paired_device.call("wait", "--session", session_id, "--timeout", "3")
    assert idle["reason"] == "idle" and not idle["state"]["running"]
    scenario.dsh.start_running(session_id)
    for strict, expected in [(False, 0), (True, 7)]:
        args = ["wait", "--session", session_id, "--timeout", "2"]
        if strict:
            args.append("--fail-on-timeout")
        exit_code, values = paired_device.invoke(*args, success=False)
        assert exit_code == expected
        assert values[-1]["data"]["reason"] == "timeout"
        assert values[-1]["data"]["state"]["running"]
def test_wait_oversized_result_recovers_over_encrypted_chain(scenario, paired_device, session_id):
    """An oversized source response fails explicitly; the next wait still works."""
    paired_device.sessions(scenario.host)
    (scenario.directory / "plugin" / "test-wait-oversized-result").write_text("1")
    code, values = paired_device.invoke("wait", "--session", session_id, "--timeout", "15", success=False)
    first = {
        "exit": code,
        "code": values[-1].get("error", {}).get("code"),
        "has_data": values[-1].get("data") is not None,
    }
    second = None
    try:
        assert code != 0 and values[-1]["error"]["code"] == "payload_too_large"
        assert values[-1].get("data") is None
        # The Host has just sealed and dropped an oversized result; allow it to
        # settle before bounding the recovery observation. The contract still
        # requires the next wait to observe idle.
        second = paired_device.call("wait", "--session", session_id, "--timeout", "15")
        assert second["reason"] == "idle"
    except AssertionError:
        Path("/reports/wait-oversized-diagnostic.json").write_text(
            json.dumps({"first": first, "second_reason": second and second.get("reason")})
        )
        raise


def test_session_create(scenario, paired_device):
    """A host-scoped create returns its source-owned identity without sending a prompt."""
    result = paired_device.call("session-create", "--host", scenario.host)
    assert result["status"] == "succeeded"
    receipt = result["lifecycle_receipt"]
    assert receipt["operation"] == "create" and receipt["disposition"] == "READY"
    identity = receipt["session_id"]
    assert identity and any(s["session_id"] == identity for s in paired_device.sessions(scenario.host))
    assert not scenario.dsh.snapshot(identity)["events"]


def test_session_create_offline_is_rejected_without_queueing(scenario, paired_device):
    """An offline create fails before any native change and leaves the session inventory unchanged."""
    paired_device.sessions(scenario.host)
    before = {item["session_id"] for item in paired_device.sessions(scenario.host)}
    scenario.access.close()
    code, values = paired_device.invoke("session-create", "--host", scenario.host, success=False)
    assert code != 0 and values[-1]["error"]["code"] in ("unavailable", "host_offline", "not_paired")
    scenario.start_access()
    after = {item["session_id"] for item in paired_device.sessions(scenario.host)}
    assert after == before


def test_session_fork_without_complete_turn(scenario, paired_device, session_id):
    """A source with no completed turn is rejected without creating a child."""
    before = {item["session_id"] for item in paired_device.sessions(scenario.host)}
    code, envelopes = paired_device.invoke("session-fork", "--session", session_id, success=False)
    assert code == 5 and envelopes[-1]["error"]["code"] == "operation_failed"
    assert envelopes[-1]["data"]["lifecycle_receipt"]["disposition"] == "FAILED"
    assert envelopes[-1]["data"]["lifecycle_receipt"]["reason_code"] == "PREFLIGHT_CONFLICT"
    assert {item["session_id"] for item in paired_device.sessions(scenario.host)} == before


def test_session_fork(scenario, paired_device, session_id):
    """Forking a completed FakeDSH turn returns independent parent and child identities."""
    paired_device.send(session_id, "synthetic fork fixture")
    source = scenario.dsh.snapshot(session_id)
    result = paired_device.call("session-fork", "--session", session_id)
    receipt = result["lifecycle_receipt"]
    assert receipt["operation"] == "fork" and receipt["parent_session_id"] == session_id
    child = receipt["session_id"]
    assert child != session_id
    assert [event["type"] for event in scenario.dsh.snapshot(child)["events"]] == [event["type"] for event in source["events"]]
    assert not scenario.dsh.snapshot(child)["summary"]["running"]


def test_model_extra_chain_revoked_device_cannot_reach_source(scenario, paired_device, session_id):
    """When an old device is revoked, catalog, session-model reads and selection are denied before the fake source is invoked."""
    paired_device.sessions(scenario.host)
    before = scenario.dsh.process.control("model_counts")
    attempts = [
        ("models", "--host", scenario.host),
        ("model", "get", "--session", session_id),
        ("model", "set", "--session", session_id, "--provider", "fixture-provider", "--model", "fixture-model"),
    ]
    stale_devices = []
    for index in range(len(attempts)):
        stale_directory = scenario.directory / f"stale-model-device-{index}"
        stale_directory.mkdir()
        shutil.copytree(paired_device.directory / "client", stale_directory / "client")
        shutil.copytree(paired_device.directory / "secrets", stale_directory / "secrets")
        stale = Device(stale_directory)
        stale.identity = paired_device.identity
        stale_devices.append(stale)
    paired_device.revoke()
    assert paired_device.revoked_access_denied()
    for stale, args in zip(stale_devices, attempts, strict=True):
        code, _ = stale.invoke(*args, success=False)
        assert code != 0
    assert scenario.dsh.process.control("model_counts") == before


def test_send(scenario, paired_device, session_id):
    """When a message is sent to an available session, the CLI succeeds and FakeDSH records
    exactly one user-message event.
    """
    assert paired_device.send(session_id, "fixture message")["status"] == "succeeded"
    events = scenario.dsh.snapshot(session_id)["events"]
    assert sum(event["type"] == "user/message" for event in events) == 1


def test_tail(scenario, paired_device, session_id):
    """When a new message is committed, tail returns its typed user and assistant Messages for the selected session, excluding stale and other-session content."""
    paired_device.send(session_id, "stale fixture message")
    paired_device.sync(session_id)
    other = scenario.dsh.create_session()
    paired_device.send(other, "other session fixture message")
    text = "selected fixture message — 你好"
    paired_device.send(session_id, text)
    source = scenario.dsh.snapshot(session_id)
    expected_times = [
        event["createdAt"] for event in source["events"]
        if event["type"] in ("user/message", "assistant/message")
        and event["data"].get("text") == text
    ]
    messages = [event for event in paired_device.events(session_id) if event["type"] == "message"]
    assert len(messages) == 2, "Tail did not deliver exactly the newly committed pair"
    assert [message["role"] for message in messages] == ["USER", "ASSISTANT"]
    matched = all(
        message["session"] == {"host_id": scenario.host, "session_id": session_id}
        and message["blocks"] == [{"block_id": "text-0", "type": "text", "text": text}]
        and message["authored_at_unix_ms"] == expected_time
        for message, expected_time in zip(messages, expected_times, strict=True)
    )
    assert matched, "Tail changed committed content, session scope, or authorship metadata"
    ids = [message["message_id"] for message in messages]
    assert len(set(ids)) == 2 and all(ids), "Tail lost distinct stable Message identities"
    _, cached_ids = paired_device.cached_history_state(scenario.host, session_id)
    assert set(ids).issubset(cached_ids), "Visible committed Messages were not persisted"


@pytest.mark.parametrize(
    "decision",
    [
        pytest.param(
            "approve",
            marks=pytest.mark.description(
                "When a valid pending approval is approved, FakeDSH records the approved decision and removes it from pending approvals."
            ),
        ),
        pytest.param(
            "reject",
            marks=pytest.mark.description(
                "When a valid pending approval is rejected, FakeDSH records the rejected decision and removes it from pending approvals."
            ),
        ),
    ],
)
def test_approval(decision, scenario, paired_device, session_id, pending_approval):
    """Resolves the pending approval to the requested decision."""
    approval = paired_device.approvals(session_id)[0]
    assert approval["approval_id"] == pending_approval["approvalId"]
    assert paired_device.resolve(decision, approval)["status"] in (
        "host_accepted",
        "succeeded",
    )
    expected = "approved" if decision == "approve" else "rejected"
    wait(lambda: scenario.dsh.approval(approval["approval_id"])["state"] == expected)
    assert not paired_device.approvals(session_id)


def test_interrupt(scenario, paired_device, session_id):
    """When interrupt is acknowledged, FakeDSH eventually becomes idle and commits one interruption for the same session."""
    scenario.dsh.start_running(session_id)
    assert paired_device.interrupt(session_id)["status"] in (
        "host_accepted",
        "succeeded",
    )
    wait(lambda: not scenario.dsh.snapshot(session_id)["summary"]["running"])
    snapshot = scenario.dsh.snapshot(session_id)
    assert not snapshot["summary"]["running"]
    assert sum(event["type"] == "session/interrupted" for event in snapshot["events"]) == 1
    assert snapshot["summary"]["sessionId"] == session_id


def test_sync(scenario, paired_device, session_id):
    """When one session gains two committed Messages, sync installs only that session's new identities and opaque cursor, and a repeat reports zero changes."""
    other = scenario.dsh.create_session()
    paired_device.sync(session_id)
    paired_device.sync(other)
    before = paired_device.cached_history_state(scenario.host, session_id)
    other_before = paired_device.cached_history_state(scenario.host, other)
    paired_device.send(other, "unselected fixture message")
    paired_device.send(session_id, "selected sync fixture message")
    result = paired_device.sync(session_id)
    assert result["session_id"] == session_id
    assert result["events"] == 2, "Sync did not install exactly the committed user/assistant pair"
    installed = paired_device.cached_history_state(scenario.host, session_id)
    assert installed[0] and installed[0] != before[0], "Sync did not advance the opaque committed cursor"
    assert len(installed[1]) == 2 and len(set(installed[1])) == 2
    assert paired_device.cached_history_state(scenario.host, other) == other_before, "Sync crossed the session boundary"
    repeated = paired_device.sync(session_id)
    assert repeated["session_id"] == session_id and repeated["events"] == 0
    assert paired_device.cached_history_state(scenario.host, session_id) == installed, "Idempotent sync changed committed identities or cursor"
    assert paired_device.sync(other)["events"] == 2


def test_revoke(scenario, paired_device):
    """When the paired device is revoked, Access rejects its old token and authenticated remote
    session queries fail.
    """
    paired_device.revoke()
    assert paired_device.revoked_access_denied()
    assert paired_device.sessions_exit(scenario.host) != 0


def test_host_offline(scenario, paired_device, session_id):
    """When the plugin is stopped, sending to its session returns host-offline exit code 5."""
    scenario.plugin.close()
    assert paired_device.send_exit(session_id) == 5


@pytest.mark.parametrize(
    "target",
    [
        pytest.param(
            "access",
            marks=pytest.mark.description(
                "When Access restarts without new source commits, session queries recover and the FakeDSH cursor is unchanged."
            ),
        ),
        pytest.param(
            "plugin",
            marks=pytest.mark.description(
                "When the plugin restarts without new source commits, session queries recover and the FakeDSH cursor is unchanged."
            ),
        ),
    ],
)
def test_restart(target, scenario, paired_device, session_id):
    """Preserves session history and serves queries after process restart."""
    paired_device.send(session_id, "fixture message")
    before = scenario.dsh.snapshot(session_id)["cursor"]
    getattr(scenario, target).close()
    getattr(scenario, "start_" + target)()
    assert any(session.get("session_id", session.get("sessionId")) == session_id for session in paired_device.sessions(scenario.host)), "Restart lost the selected session"
    assert scenario.dsh.snapshot(session_id)["cursor"] == before


def test_duplicate_command(scenario, paired_device, session_id):
    """When the same command ID, digest and body are submitted again, Access returns the
    original receipt without adding DSH events.
    """
    original = paired_device.send(session_id, "fixture message")
    before = scenario.dsh.snapshot(session_id)["cursor"]
    status, receipt = scenario.replay_command(paired_device)
    assert status in (200, 202)
    assert receipt["requestId"] == original["request_id"]
    assert receipt["commandId"] == original["command_id"]
    assert scenario.dsh.snapshot(session_id)["cursor"] == before


def test_digest_conflict(scenario, paired_device, session_id):
    """When an existing command ID has a changed body and digest, Access returns HTTP 409
    without adding DSH events.
    """
    paired_device.send(session_id, "fixture message")
    before = scenario.dsh.snapshot(session_id)["cursor"]
    status, conflict = scenario.replay_command(paired_device, changed=True, include_conflict_code=True)
    assert status == 409 and conflict["code"] == "payload_digest_mismatch"
    assert scenario.dsh.snapshot(session_id)["cursor"] == before


@pytest.mark.parametrize(
    "json_output,once,visible_text",
    [
        pytest.param(True, False, True, marks=pytest.mark.description(
            "When a live JSON CLI receives preview text, it arrives before final settlement and recovery contains only the durable message."
        )),
        pytest.param(False, False, True, marks=pytest.mark.description(
            "When a live human CLI receives previews and final settlement, it displays the final reply without repeating the visible prefix."
        )),
        pytest.param(True, True, True, marks=pytest.mark.description(
            "When JSON tail uses once, lifecycle markers do not stop it and the first visible preview completes before final settlement."
        )),
        pytest.param(False, True, True, marks=pytest.mark.description(
            "When human tail uses once, it waits beyond stream start and exits after printing the first visible preview."
        )),
        pytest.param(True, True, False, marks=pytest.mark.description(
            "When once tail receives only a stream-start marker, it waits for its deadline and reports timeout rather than empty success."
        )),
    ],
)
def test_live_preview(json_output, once, visible_text, scenario, paired_device, session_id):
    """Delivers authenticated preview text before DSH commits the final message."""
    import json
    import os
    import select
    import time

    from environment import ROOT, Process

    paired_device.sync(session_id)
    args = [
        str(ROOT / "target/debug/whalerein"),
        "--data-dir",
        str(paired_device.directory / "client"),
    ]
    if json_output:
        args.append("--json")
    args.extend(["tail", "--session", session_id, "--timeout", "15" if visible_text else "2"])
    if once:
        args.append("--once")
    live = Process(
        args,
        scenario.directory,
        {
            "WHALEREIN_SECRET_BACKEND": "file:"
            + str(paired_device.directory / "secrets"),
            "WHALEREIN_LOG": "info",
        },
        control=True,
    )
    buffered = bytearray()

    def subscribed():
        live.output.seek(0)
        return "watch_ready" in live.output.read()

    def read_until(predicate):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if predicate(bytes(buffered)):
                return
            ready, _, _ = select.select([live.child.stdout], [], [], 0.1)
            if ready:
                chunk = os.read(live.child.stdout.fileno(), 65536)
                assert chunk, "CLI exited before the expected stream output"
                buffered.extend(chunk)
        raise AssertionError("Expected live output was not received before settlement")

    def events(raw):
        # Only complete newline-delimited envelopes are parseable.
        values = [json.loads(line) for line in raw.split(b"\n")[:-1] if line]
        return [v["data"]["event"] for v in values if v.get("data", {}).get("event")]

    try:
        wait(subscribed, process=live)
        scenario.dsh.process.control("preview_start", session_id=session_id)
        if visible_text:
            scenario.dsh.process.control("preview_chunk", session_id=session_id)
            if json_output:
                read_until(
                    lambda raw: any(
                        e.get("type") == "preview" for e in events(raw)
                    )
                )
                assert all(e.get("type") != "message" for e in events(bytes(buffered)))
            else:
                read_until(lambda raw: raw == (b"x\n" if once else b"x"))
        else:
            read_until(lambda raw: any(e.get("type") == "reset" for e in events(raw)))
        # This assertion runs before the final message is even emitted.
        assert not scenario.dsh.snapshot(session_id)["events"]
        if once:
            assert live.child.wait(timeout=5) == (0 if visible_text else 7)
        scenario.dsh.process.control("preview_final", session_id=session_id)
        if not once:
            if json_output:
                read_until(lambda raw: any(e.get("type") == "message" for e in events(raw)))
            else:
                read_until(lambda raw: raw == b"xy\n")
    finally:
        live.close()
    # Restart the CLI and rebuild only this isolated device's disposable cache.
    # Transport credentials remain intact; history must come from DSH again.
    cache = paired_device.directory / "client" / "cache.sqlite3"
    for suffix in ("", "-wal", "-shm"):
        cache.with_name(cache.name + suffix).unlink(missing_ok=True)
    paired_device.sessions(scenario.host)
    _, values = paired_device.invoke("tail", "--session", session_id, "--once", "--timeout", "15")
    recovered = [v["data"]["event"] for v in values if v.get("data", {}).get("event")]
    assert all(event["type"] != "preview" for event in recovered)
    committed = [event for event in recovered if event["type"] == "message"]
    assert len(committed) == 1
    assert len(scenario.dsh.snapshot(session_id)["events"]) == 1


def test_probe_login(scenario, paired_device):
    """When a probe signs in, credentials are checked and topics denied without creating a
    host or displacing the existing host connection.
    """
    import json
    import os
    import subprocess

    from environment import ROOT

    def broker_clients():
        result = subprocess.run(
            ["node", "tests/integration/drivers/transport/broker-snapshot.mjs"],
            cwd=ROOT,
            env={
                **os.environ,
                "WHALEREIN_MQTT_URL": "mqtt://127.0.0.1:1883",
                "WHALEREIN_BROKER_PASSWORD": scenario.env["WHALEREIN_BROKER_PASSWORD"],
            },
            capture_output=True,
            text=True,
            timeout=15,
            check=False,
        )
        assert result.returncode == 0, "Broker snapshot query failed"
        return json.loads(result.stdout)["clients"]

    def live_connection():
        clients = broker_clients()
        match = [
            client
            for client in clients
            if client["client_id"] == "whalerein-host-" + scenario.host
        ]
        assert match, "Host connection missing from the broker registry"
        return match[0]["conn"]

    before = live_connection()
    result = subprocess.run(
        ["node", "tests/integration/drivers/transport/probe-login.mjs"],
        cwd=ROOT,
        env={**os.environ, **scenario.env},
        capture_output=True,
        text=True,
        timeout=25,
        check=False,
    )
    assert result.returncode == 0, "Probe login or authorization check failed"
    assert result.stdout.strip() == "probe-login-ok"
    assert live_connection() == before, "Probe displaced the existing host connection"
    assert any(host["online"] for host in paired_device.hosts())
    query = subprocess.run(
        [
            "psql", "-h", "127.0.0.1", "-p", "55432", "-U", "postgres",
            "-d", "whalerein_test", "-tAc",
            f"SELECT count(*) FROM {scenario.schema}.hosts WHERE host_id = 'probe-unregistered-host'",
        ],
        capture_output=True, text=True, timeout=5, check=True,
    )
    assert query.stdout.strip() == "0", "Probe provisioned a host"


def test_lost_final_notification_refreshes_without_subsequent_events(scenario, paired_device, session_id):
    """When the last committed push is lost and no subsequent event arrives, watch refresh repairs the source watermark and delivers the authoritative message."""
    import json

    from environment import ROOT, Process

    paired_device.sync(session_id)
    live = Process([
        str(ROOT / "target/debug/whalerein"), "--json", "--data-dir",
        str(paired_device.directory / "client"), "tail", "--session", session_id,
        "--once", "--timeout", "12",
    ], scenario.directory, {
        "WHALEREIN_SECRET_BACKEND": "file:" + str(paired_device.directory / "secrets"),
        "WHALEREIN_LOG": "info",
    }, control=True)
    try:
        def ready():
            live.output.seek(0)
            return "watch_ready" in live.output.read()
        wait(ready, process=live)
        scenario.dsh.process.control("emit_silent", session_id=session_id)
        stdout, _ = live.child.communicate(timeout=15)
        assert live.child.returncode == 0, "Watch did not recover the lost final notification"
        events = [json.loads(line).get("data", {}).get("event", {}) for line in stdout.splitlines()]
        assert sum(event.get("type") == "message" for event in events) == 1
        assert len(scenario.dsh.snapshot(session_id)["events"]) == 1
    finally:
        live.close()


def test_host_scoped_revocation_clears_the_committed_host_peer(scenario, paired_device):
    """When the host revokes its grant, Access denies the old token and the matching peer is removed by its bound notice."""
    identity = paired_device.identity["device_id"]
    assert scenario.host_peer_exists(identity)
    scenario.revoke_from_host(paired_device)
    wait(lambda: not scenario.host_peer_exists(identity))
    assert paired_device.revoked_access_denied()


def test_pairing_receipt_survives_lost_response_and_both_restarts(scenario):
    """When FINISH commits but its HTTP response is lost, a fresh CLI resumes through restarted Access and Host without re-pairing or leaking credentials into server receipts."""
    from http.client import HTTPConnection
    import json
    import socket
    import threading
    import urllib.parse
    from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
    from environment import Device, http

    lost = threading.Event()
    allow_finish = threading.Event()
    errors = []

    class Proxy(BaseHTTPRequestHandler):
        def log_message(self, *_args):
            pass

        def forward(self):
            upstream = HTTPConnection("127.0.0.1", 18080, timeout=25)
            try:
                body = self.rfile.read(int(self.headers.get("Content-Length", "0")))
                headers = {key: value for key, value in self.headers.items() if key.lower() not in {"host", "connection"}}
                upstream.request(self.command, self.path, body, headers)
                response = upstream.getresponse()
                payload = response.read()
                claim = json.loads(body) if self.path == "/v1/pairing/claims" else {}
                if claim.get("phase") == "PAIRING_PHASE_FINISH" and response.status == 200 and not allow_finish.is_set():
                    lost.set()
                    self.connection.shutdown(socket.SHUT_RDWR)
                    self.connection.close()
                    return
                self.send_response(response.status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)
            except (ConnectionError, OSError, ValueError):
                errors.append("proxy request failed")
                self.close_connection = True
            finally:
                upstream.close()

        do_POST = forward
        do_GET = forward

    proxy = ThreadingHTTPServer(("127.0.0.1", 0), Proxy)
    thread = threading.Thread(target=proxy.serve_forever, daemon=True)
    thread.start()
    try:
        invitation = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
        uri = urllib.parse.urlsplit(invitation["pairingUri"])
        query = dict(urllib.parse.parse_qsl(uri.query))
        query["access"] = f"http://127.0.0.1:{proxy.server_port}"
        invitation_uri = urllib.parse.urlunsplit(uri._replace(query=urllib.parse.urlencode(query)))
        device = Device(scenario.directory)
        code, _ = device.invoke("pair", invitation_uri, success=False)
        assert lost.is_set(), "FINISH did not reach committed activation"
        assert code != 0, "lost response did not preserve an uncertain client operation"
        scenario.access.close()
        scenario.plugin.close()
        scenario.start_access()
        scenario.start_plugin()
        allow_finish.set()
        device.identity = device.call("pair-resume")
        assert scenario.host_peer_exists(device.identity["device_id"])
        assert device.sessions(scenario.host) == []
        import subprocess
        stored = subprocess.run(["psql","-h","127.0.0.1","-p","55432","-U","postgres","-d","whalerein_test","-tAc",
            f"SELECT encode(hello_response,'hex'),encode(finish_response,'hex') FROM {scenario.schema}.pairing_operations"],
            check=True,capture_output=True,text=True,timeout=5)
        credential = device.transport_token().encode()
        for line in stored.stdout.splitlines():
            for blob in line.strip().split("|"):
                assert credential not in bytes.fromhex(blob), "plaintext credential persisted in pairing receipt"
        scenario.revoke_from_host(device)
        assert device.revoked_access_denied()
        assert not errors, "pairing proxy failed before the intended response loss"
    finally:
        proxy.shutdown()
        proxy.server_close()
        thread.join(timeout=5)


def test_delivery_queue_while_running(scenario, paired_device, session_id):
    """A remote queued command is accepted once while the current FakeDSH turn remains active."""
    scenario.dsh.start_running(session_id)
    receipt = paired_device.send(session_id, "synthetic queued fixture")
    assert receipt["status"] == "succeeded"
    assert receipt["delivery_id"]
    snapshot = scenario.dsh.snapshot(session_id)
    assert snapshot["summary"]["running"]
    assert snapshot["summary"]["queuedPrompts"] == 1
    assert not any(event["type"] == "user/message" for event in snapshot["events"])
def test_queue_edit_encoded_envelope_budget_and_complete_response(scenario, paired_device):
    """Queue edit enforces the encrypted request boundary and refuses an oversized complete response without mutating source."""
    import json
    import os
    import subprocess
    from environment import ROOT

    session="queue-budget-fixture"
    scenario.dsh.process.control("create_queue_budget_session",session_id=session)
    paired_device.sessions(scenario.host)
    queue=paired_device.call("queue","list","--session",session)
    assert len(queue["items"])==2
    first,second=[item["item_id"] for item in queue["items"]]
    estimate=subprocess.run(["node",str(ROOT / "tests/integration/drivers/delivery/budget-size.mjs"),scenario.host,paired_device.identity["device_id"],session,first],cwd=ROOT,capture_output=True,text=True,timeout=60,check=True)
    budget=json.loads(estimate.stdout)
    assert budget["sizes"] == [budget["wire_limit"]-1,budget["wire_limit"],budget["wire_limit"]+1]
    executable=ROOT / "target/debug/examples/delivery_budget_driver"
    env={**os.environ,"WHALEREIN_LOG":"error"}
    before=scenario.dsh.process.control("gx_effect_counts")["updatePendingQueue"]
    for offset,expected in [(-1,"succeeded"),(0,"succeeded"),(1,"payload_too_large")]:
        result=subprocess.run([str(executable),str(paired_device.directory / "client"),str(paired_device.directory / "secrets"),scenario.host,session,str(budget["at"]+offset),first],cwd=ROOT,env=env,capture_output=True,text=True,timeout=150,check=False)
        output=json.loads(result.stdout)
        assert output.get("status",output.get("error")) == expected
        assert (result.returncode == 0) == (offset <= 0)
    assert scenario.dsh.process.control("gx_effect_counts")["updatePendingQueue"] == before+2
    extra=subprocess.run([str(executable),str(paired_device.directory / "client"),str(paired_device.directory / "secrets"),scenario.host,session,str(2*1024*1024),second],cwd=ROOT,env=env,capture_output=True,text=True,timeout=150,check=False)
    assert extra.returncode == 0 and json.loads(extra.stdout)["status"] == "succeeded"
    source_before=scenario.dsh.process.control("queue_budget_state",session_id=session)
    assert [item["length"] for item in source_before] == [budget["at"],2*1024*1024]
    code,values=paired_device.invoke("queue","list","--session",session,success=False)
    assert code != 0 and values[-1]["error"]["code"] == "payload_too_large" and "data" not in values[-1]
    assert scenario.dsh.process.control("queue_budget_state",session_id=session) == source_before
    assert scenario.dsh.process.control("gx_effect_counts")["updatePendingQueue"] == before+3


def test_queue_complete_response_exact_encrypted_boundary(scenario, paired_device):
    """The full encrypted queue result is accepted through the transport limit without truncation."""
    import json

    session = "queue-response-budget-fixture"
    scenario.dsh.process.control("create_queue_budget_session", session_id=session)
    paired_device.sessions(scenario.host)
    marker = scenario.directory / "plugin" / "test-queue-wire-size"
    marker.touch()
    limit = 16 * 1024 * 1024

    def set_size(size):
        assert scenario.dsh.process.control("set_queue_budget_size", session_id=session, size=size)

    def observe():
        code, result = paired_device.invoke("queue", "list", "--session", session, success=False)
        wire = json.loads(marker.read_text())
        return code, result[-1], wire

    probe = limit - 4096
    set_size(probe)
    code, result, wire = observe()
    assert code == 0 and wire["status"] == "succeeded"
    assert len(result["data"]["items"][0]["blocks"][0]["text"]) == probe
    exact = limit - (wire["size"] - probe)
    for delta in (-1, 0, 1):
        set_size(exact + delta)
        code, result, wire = observe()
        source = scenario.dsh.process.control("queue_budget_state", session_id=session)
        assert source[0]["length"] == exact + delta
        if delta <= 0:
            assert code == 0 and wire["status"] == "succeeded" and wire["size"] == limit + delta
            assert len(result["data"]["items"][0]["blocks"][0]["text"]) == exact + delta
        else:
            assert code != 0 and result["error"]["code"] == "payload_too_large"
            assert "data" not in result and wire["status"] == "failed" and wire["bodySize"] == 0


def test_queue_denied_for_unpaired_and_revoked_token(scenario, paired_device):
    """Unpaired clients and a revoked transport token cannot execute pending queue mutations."""
    import base64
    import json
    import subprocess
    import time
    import uuid
    from environment import Device, http

    session = "queue-auth-fixture"
    scenario.dsh.process.control("create_queue_session", session_id=session)
    before = scenario.dsh.process.control("gx_effect_counts")["updatePendingQueue"]
    reads_before = scenario.dsh.process.control("queue_read_count")
    stranger = Device(scenario.directory / "unpaired-queue")
    for args in (("queue","list","--session",session),("queue","remove","--session",session,"--item","queue-fixture")):
        code, _ = stranger.invoke(*args,success=False)
        assert code != 0
    assert scenario.dsh.process.control("gx_effect_counts")["updatePendingQueue"] == before
    assert scenario.dsh.process.control("queue_read_count") == reads_before
    paired_device.sessions(scenario.host)
    queue = paired_device.call("queue","list","--session",session)
    item=queue["items"][0]["item_id"]
    query=f"""SELECT json_build_object(
      'requestId',request_id,'hostId',host_id,'deviceId',device_id,'commandId',command_id,
      'kind',kind,'protocolVersion',protocol_version,'channelId',channel_id,
      'clientTransportVersion',client_transport_version,'body',encode(request_body,'base64'),
      'payloadDigest',encode(payload_digest,'base64')) FROM {scenario.schema}.requests
      WHERE kind=2 AND device_id='{paired_device.identity["device_id"]}' ORDER BY created_at DESC LIMIT 1"""
    row=subprocess.run(["psql","-h","127.0.0.1","-p","55432","-U","postgres","-d","whalerein_test","-tAc",query],capture_output=True,text=True,timeout=5,check=True)
    saved=json.loads(row.stdout)
    for field in ("body","payloadDigest"):
        saved[field]=base64.b64encode(base64.b64decode(saved[field] or "")).decode()
    saved["requestId"]=uuid.uuid4().hex
    saved["expiresAtUnixMs"]=str(int((time.time()+60)*1000))
    result=paired_device.call("queue","edit","--session",session,"--item",item,"synthetic authorized edit")
    assert result["command"]["status"] == "succeeded"
    accepted=scenario.dsh.process.control("gx_effect_counts")["updatePendingQueue"]
    assert accepted == before + 1
    paired_device.revoke()
    reads=scenario.dsh.process.control("queue_read_count")
    with pytest.raises(RuntimeError,match="HTTP (401|403)"):
        http("/v1/hosts/"+scenario.host+"/requests",saved,paired_device._revoked_token)
    assert scenario.dsh.process.control("queue_read_count") == reads
    with pytest.raises(RuntimeError,match="HTTP (401|403)"):
        scenario.replay_command(paired_device,token_override=paired_device._revoked_token)
    assert scenario.dsh.process.control("gx_effect_counts")["updatePendingQueue"] == accepted


def test_pending_queue_chain(scenario, paired_device):
    """Queue observations and explicit edits use the encrypted remote path."""
    session = "queue-fixture-session"
    scenario.dsh.process.control("create_queue_session", session_id=session)
    paired_device.sessions(scenario.host)
    queue = paired_device.call("queue", "list", "--session", session)
    assert queue["available"] and len(queue["items"]) == 1
    item = queue["items"][0]["item_id"]
    edit = paired_device.call("queue", "edit", "--session", session, "--item", item, "replacement fixture")
    assert edit["command"]["status"] == "succeeded" and edit["refresh"] == "current"
    refreshed = paired_device.call("queue", "list", "--session", session)
    assert refreshed["items"][0]["item_id"] == item
    removed = paired_device.call("queue", "remove", "--session", session, "--item", item)
    assert removed["command"]["status"] == "succeeded"
    assert paired_device.call("queue", "list", "--session", session)["items"] == []


def test_delivery_encoded_envelope_budget(scenario, paired_device, session_id):
    """Encrypted delivery immediately below, at, and above the forwarded envelope budget reaches the source only within budget."""
    import json
    import os
    import subprocess
    from environment import ROOT

    assert scenario.dsh.process.control("enable_budget_probe") is True
    paired_device.sessions(scenario.host)
    estimate = subprocess.run(
        ["node", str(ROOT / "tests/integration/drivers/delivery/budget-size.mjs"),
         scenario.host, paired_device.identity["device_id"], session_id],
        cwd=ROOT, capture_output=True, text=True, timeout=60, check=True,
    )
    budget = json.loads(estimate.stdout)
    assert budget["sizes"] == [budget["wire_limit"] - 1, budget["wire_limit"], budget["wire_limit"] + 1]
    executable = ROOT / "target/debug/examples/delivery_budget_driver"
    env = {**os.environ, "WHALEREIN_LOG": "error"}
    for offset, expected in [(-1, "succeeded"), (0, "succeeded"), (1, "payload_too_large")]:
        result = subprocess.run(
            [str(executable), str(paired_device.directory / "client"),
             str(paired_device.directory / "secrets"), scenario.host, session_id,
             str(budget["at"] + offset)],
            cwd=ROOT, env=env, capture_output=True, text=True, timeout=150, check=False,
        )
        output = json.loads(result.stdout)
        assert output.get("status", output.get("error")) == expected
        if offset <= 0:
            assert result.returncode == 0 and output["has_delivery_id"]
        else:
            assert result.returncode != 0
    assert scenario.dsh.process.control("budget_count") == 2


def test_delivery_denied_for_unpaired_and_revoked_devices(scenario, paired_device, session_id):
    """Unpaired and revoked devices cannot deliver a message to the source."""
    from environment import Device

    before = scenario.dsh.snapshot(session_id)["cursor"]
    stranger = Device(scenario.directory / "unpaired")
    assert stranger.send_exit(session_id) != 0
    assert scenario.dsh.snapshot(session_id)["cursor"] == before
    paired_device.send(session_id, "synthetic authorized fixture")
    accepted = scenario.dsh.snapshot(session_id)["cursor"]
    paired_device.revoke()
    with pytest.raises(RuntimeError, match="HTTP (401|403)"):
        scenario.replay_command(paired_device, token_override=paired_device._revoked_token)
    assert scenario.dsh.snapshot(session_id)["cursor"] == accepted


def test_wait_two_sessions_cancellation_isolation(scenario, paired_device, session_id):
    """Two sessions wait alongside one tail through one Core; cancelling one leaves the other wait and tail active without interrupting Agents."""
    import json
    import subprocess
    from pathlib import Path
    from environment import ROOT

    second = scenario.dsh.create_session()
    scenario.dsh.start_running(session_id)
    scenario.dsh.start_running(second)
    observations = scenario.directory / "plugin" / "test-wait-observations.json"
    release = scenario.directory / "release-wait"
    process = subprocess.Popen([
        str(ROOT / "target/debug/examples/wait_concurrency_driver"),
        str(paired_device.directory / "client"), str(paired_device.directory / "secrets"),
        session_id, second, str(release),
    ], cwd=ROOT, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    def records():
        return json.loads(observations.read_text())
    try:
        wait(lambda: release.with_suffix(".tail-ready").exists()
             and {r["sessionId"] for r in records() if r["status"] == "active"} >= {session_id, second}, timeout=12)
        release.touch()
        wait(lambda: any(r["sessionId"] == session_id and r["status"] == "aborted" for r in records())
             and any(r["sessionId"] == second and r["status"] == "active" for r in records()), timeout=5)
        stdout, _ = process.communicate(timeout=15)
        assert process.returncode == 0
        assert json.loads(stdout) == {"first_cancelled": True, "tail_active": True, "tail_resets": 1, "second_reason": "Timeout", "second_running": True}
        for session in (session_id, second):
            assert not any(event["type"] == "session/interrupted" for event in scenario.dsh.snapshot(session)["events"])
    except Exception as error:
        if process.poll() is None:
            process.kill()
        stdout, _ = process.communicate(timeout=5)
        try:
            state = records()
            summary = {status: sum(row["status"] == status for row in state) for status in ("active", "aborted", "completed")}
        except (OSError, ValueError):
            summary = {}
        Path('/reports/wait-concurrency-diagnostic.json').write_text(json.dumps({
            'driver': stdout.strip(), 'tail_ready': release.with_suffix('.tail-ready').exists(), 'records': summary,
        }))
        raise AssertionError(f"wait fixture failed: driver={stdout.strip()}, tail_ready={release.with_suffix('.tail-ready').exists()}, records={summary}") from error
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_wait_revocation_aborts_only_observation(scenario, paired_device, session_id):
    """Revoking a device while its encrypted wait is active aborts that observation without interrupting its Agent."""
    import json
    import os
    import subprocess
    from pathlib import Path
    from environment import ROOT

    scenario.dsh.start_running(session_id)
    observations = scenario.directory / "plugin" / "test-wait-observations.json"
    process = subprocess.Popen([
        str(ROOT / "target/debug/whalerein"), "--json", "--yes", "--data-dir", str(paired_device.directory / "client"),
        "wait", "--session", session_id, "--timeout", "20",
    ], cwd=ROOT, env={**os.environ, "WHALEREIN_SECRET_BACKEND": "file:" + str(paired_device.directory / "secrets"), "WHALEREIN_LOG": "error"},
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    def records():
        return json.loads(observations.read_text())
    try:
        wait(lambda: any(row["status"] == "active" for row in records()), timeout=12)
        paired_device.revoke()
        wait(lambda: all(row["status"] != "active" for row in records()), timeout=5)
        stdout, _ = process.communicate(timeout=12)
        envelope = json.loads(stdout)
        assert process.returncode != 0 and envelope.get("error")
        Path('/reports/wait-revocation-diagnostic.json').write_text(json.dumps({'code': envelope['error']['code']}))
        assert envelope["error"]["code"] in {"revoked", "unauthorized", "forbidden", "not_paired", "unavailable", "expired"}
        assert any(row["status"] == "aborted" for row in records())
        assert not any(event["type"] == "session/interrupted" for event in scenario.dsh.snapshot(session_id)["events"])
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_wait_reconnect_after_host_restart(scenario, paired_device, session_id):
    """An active read-only wait reconnects after the Host restarts without restarting Agent work or its user deadline."""
    import json
    import os
    import subprocess
    import time
    from pathlib import Path
    from environment import ROOT

    scenario.dsh.start_running(session_id)
    observations = scenario.directory / "plugin" / "test-wait-observations.json"
    process = subprocess.Popen([
        str(ROOT / "target/debug/whalerein"), "--json", "--yes", "--data-dir", str(paired_device.directory / "client"),
        "wait", "--session", session_id, "--timeout", "20",
    ], cwd=ROOT, env={**os.environ, "WHALEREIN_SECRET_BACKEND": "file:" + str(paired_device.directory / "secrets"), "WHALEREIN_LOG": "error"},
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    started = time.monotonic()
    stages = {}
    def records():
        return json.loads(observations.read_text())
    try:
        wait(lambda: any(row["status"] == "active" for row in records()), timeout=10)
        stages['active'] = round(time.monotonic() - started, 3)
        stages['before_stop'] = [{key: row[key] for key in ('status', 'timeoutMs')} for row in records()]
        scenario.plugin.close()
        stages['stopped'] = round(time.monotonic() - started, 3)
        stages['after_stop'] = [{key: row[key] for key in ('status', 'timeoutMs')} for row in records()]
        observations.unlink(missing_ok=True)
        scenario.start_plugin()
        stages['restarted'] = round(time.monotonic() - started, 3)
        wait(lambda: any(row["status"] == "active" for row in records()), timeout=15)
        scenario.dsh.process.control("idle", session_id=session_id)
        stdout, _ = process.communicate(timeout=12)
        envelope = json.loads(stdout)
        assert process.returncode == 0 and envelope["data"]["reason"] == "idle"
        assert time.monotonic() - started < 20
        assert not any(event["type"] == "session/interrupted" for event in scenario.dsh.snapshot(session_id)["events"])
    except Exception as error:
        try:
            plugin_log = Path(scenario.plugin.output.name).read_text()
            stages['plugin_failure'] = [token for token in ('lock', 'conflict', 'sqlite', 'ENOENT', 'EADDRINUSE', 'unavailable') if token.lower() in plugin_log.lower()]
        except (OSError, ValueError):
            stages['plugin_failure'] = []
        try:
            stages['at_failure'] = [{key: row[key] for key in ('status', 'timeoutMs')} for row in records()]
        except (OSError, ValueError, KeyError):
            stages['at_failure'] = None
        try:
            stdout, _ = process.communicate(timeout=15)
        except subprocess.TimeoutExpired:
            process.kill()
            stdout, _ = process.communicate(timeout=5)
        rows = subprocess.run(['psql', '-h', '127.0.0.1', '-p', '55432', '-U', 'postgres', '-d', 'whalerein_test', '-t', '-A', '-F', ',', '-c',
            f'SELECT kind,status,error_code,round(extract(epoch from created_at)*1000),round(extract(epoch from updated_at)*1000) FROM "{scenario.schema}".requests ORDER BY created_at'], capture_output=True, text=True, timeout=5, check=True).stdout.splitlines()
        Path('/reports/wait-reconnect-diagnostic.json').write_text(json.dumps({'driver': stdout.strip(), 'status': process.returncode, 'elapsed_seconds': round(time.monotonic() - started, 3), 'stages': stages, 'request_states': rows}))
        raise AssertionError('wait reconnect fixture failed') from error
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)




def test_wait_recovers_after_another_client_replaces_channel(scenario, paired_device, session_id):
    """A wait recovers from a verified channel replacement with remaining time and never interrupts the Agent."""
    import json
    import os
    import subprocess
    import time
    from pathlib import Path
    from environment import ROOT

    scenario.dsh.start_running(session_id)
    observations = scenario.directory / "plugin" / "test-wait-observations.json"
    process = subprocess.Popen([
        str(ROOT / "target/debug/whalerein"), "--json", "--yes", "--data-dir", str(paired_device.directory / "client"),
        "wait", "--session", session_id, "--timeout", "20",
    ], cwd=ROOT, env={**os.environ, "WHALEREIN_SECRET_BACKEND": "file:" + str(paired_device.directory / "secrets"), "WHALEREIN_LOG": "error"},
        stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    started = time.monotonic()
    def records():
        return json.loads(observations.read_text())
    try:
        wait(lambda: any(row["status"] == "active" for row in records()), timeout=10)
        paired_device.sessions(scenario.host)
        wait(lambda: any(row["status"] == "aborted" for row in records())
             and any(row["status"] == "active" for row in records()), timeout=10)
        scenario.dsh.process.control("idle", session_id=session_id)
        stdout, _ = process.communicate(timeout=12)
        result = json.loads(stdout)
        assert process.returncode == 0 and result["data"]["reason"] == "idle"
        assert time.monotonic() - started < 20
        assert not any(event["type"] == "session/interrupted" for event in scenario.dsh.snapshot(session_id)["events"])
    except Exception as error:
        if process.poll() is None:
            process.kill()
        stdout, _ = process.communicate(timeout=5)
        try:
            statuses = [row['status'] for row in records()]
        except (OSError, ValueError):
            statuses = []
        Path('/reports/wait-replacement-diagnostic.json').write_text(json.dumps({'driver': stdout.strip(), 'status': process.returncode, 'observations': statuses}))
        raise AssertionError('wait channel replacement fixture failed') from error
    finally:
        if process.poll() is None:
            process.kill()
            process.communicate(timeout=5)


def test_wait_source_authorization_failure_is_not_retried(scenario, paired_device, session_id):
    """An authenticated source authorization failure is returned once without treating the device as revoked."""
    import json

    scenario.dsh.start_running(session_id)
    marker = scenario.directory / "plugin" / "test-wait-source-unauthorized"
    observations = scenario.directory / "plugin" / "test-wait-observations.json"
    marker.touch()
    code, values = paired_device.invoke("wait", "--session", session_id, "--timeout", "3", success=False)
    error = values[-1]["error"]
    assert code == 4 and error["code"] == "unauthorized" and error["layer"] == "source"
    assert "private fixture detail" not in json.dumps(values)
    assert not marker.exists()
    assert len(json.loads(observations.read_text())) == 1
    assert any(row["session_id"] == session_id for row in paired_device.sessions(scenario.host))
    assert not any(event["type"] == "session/interrupted" for event in scenario.dsh.snapshot(session_id)["events"])


def test_delivery_other_paired_device_cannot_replay_another_device_command(scenario, paired_device, session_id):
    """A separately paired device cannot replay another device's encrypted delivery."""
    from environment import Device, http

    second = Device(scenario.directory / "delivery-second-device")
    invite = http("/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin)
    second.identity = second.call("pair", invite["pairingUri"])
    assert second.identity["device_id"] != paired_device.identity["device_id"]
    paired_device.send(session_id, "synthetic first device delivery")
    before = scenario.dsh.snapshot(session_id)["cursor"]
    with pytest.raises(RuntimeError, match="HTTP (401|403)"):
        scenario.replay_command(paired_device, token_override=second.transport_token())
    assert scenario.dsh.snapshot(session_id)["cursor"] == before
    second.send(session_id, "synthetic second device delivery")
    assert scenario.dsh.snapshot(session_id)["cursor"] != before


def test_wait_offline_is_not_a_cached_observation(scenario, paired_device, session_id):
    """An offline Host cannot turn cached session state into a successful wait."""
    paired_device.sessions(scenario.host)
    scenario.plugin.close()
    code, values = paired_device.invoke("wait", "--session", session_id, "--timeout", "2", success=False)
    assert code == 5
    assert values[-1].get("data") is None
    assert values[-1]["error"]["code"] in {"host_offline", "unavailable"}
    assert any(item["session_id"] == session_id for item in paired_device.sessions(scenario.host))



def test_question_revoked_device_cannot_query_answer_or_cancel(scenario, paired_device, session_id):
    """A revoked device cannot access a pending question through list, answer or cancellation."""
    scenario.dsh.process.control("request_question_open", session_id=session_id)
    batch = paired_device.call("questions", "--session", session_id)["batches"][0]
    answer = scenario.directory / "revoked-answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    attempts = [
        ("questions", "--session", session_id),
        ("answer", "--session", session_id, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
         "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer)),
        ("question-cancel", "--session", session_id, "--command", "revoked-fixture"),
    ]
    devices = []
    for index in range(len(attempts)):
        directory = scenario.directory / f"stale-question-device-{index}"
        directory.mkdir()
        shutil.copytree(paired_device.directory / "client", directory / "client")
        shutil.copytree(paired_device.directory / "secrets", directory / "secrets")
        stale = Device(directory)
        stale.identity = paired_device.identity
        devices.append(stale)
    before = scenario.dsh.process.control("gx_effect_counts")
    paired_device.revoke()
    assert paired_device.revoked_access_denied()
    for device, args in zip(devices, attempts, strict=True):
        code, envelopes = device.invoke(*args, success=False)
        assert code != 0
        assert "Fixture question" not in json.dumps(envelopes)
    after = scenario.dsh.process.control("gx_effect_counts")
    assert after["answerOpenQuestion"] == before["answerOpenQuestion"]
    assert after["sendUserMessage"] == before["sendUserMessage"]


def test_question_foreign_session_batch_does_not_settle(scenario, paired_device, session_id):
    """A valid guard from another session neither settles nor exposes its original question."""
    other = "question-foreign-session"
    scenario.dsh.process.control("create_session", session_id=other)
    paired_device.sessions(scenario.host)
    scenario.dsh.process.control("request_question_open", session_id=session_id)
    batch = paired_device.call("questions", "--session", session_id)["batches"][0]
    answer = scenario.directory / "foreign-answer.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    def args(session):
        return ["answer", "--session", session, "--batch", batch["batch_id"], "--epoch", batch["source_epoch"],
                "--version", str(batch["version"]), "--digest", batch["digest"], "--phase", batch["phase"], "--input", str(answer)]
    before = scenario.dsh.process.control("question_answer_calls")
    code, response = paired_device.invoke(*args(other), success=False)
    assert code != 0 and "Fixture question" not in json.dumps(response)
    # A scoped adapter may be invoked, but must not settle the foreign question.
    current = paired_device.call("questions", "--session", session_id)["batches"]
    assert len(current) == 1 and current[0]["batch_id"] == batch["batch_id"]
    assert paired_device.call(*args(session_id))["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.dsh.process.control("question_answer_calls") >= before + 1
