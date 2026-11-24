"""The new Standalone client grammar on real DSH and the platform keyring."""

import json
import select
import signal
import subprocess

import pytest

from client_environment import CutoverClient, isolated_keyring
from environment import ROOT, http, wait

pytestmark = pytest.mark.__getattr__("native-dsh")


@pytest.fixture
def platform_keyring(tmp_path):
    with isolated_keyring(tmp_path) as env:
        yield env


def test_standalone_pair_restart_and_follow_reply(scenario, platform_keyring):
    """Recover interrupted pairing and the original native reply across an Access restart."""
    client = CutoverClient(scenario.directory, "standalone-instance", platform_keyring)
    session_id = scenario.create()
    invitation = http(
        "/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin
    )["pairingUri"]
    assert scenario.request("gx-hold-pair-commit-result")["armed"]
    child = None
    released = False

    def at_commit_barrier():
        if child.poll() is not None:
            output, _ = child.communicate(timeout=5)
            result = client.envelope(output)
            code = result.get("error", {}).get("code", "unknown")
            raise AssertionError(f"CLI host connect failed with {code}")
        return scenario.request("gx-held-pair-commit-result")["entered"]

    try:
        child = subprocess.Popen(
            client.args("host", "connect", "--invite-stdin", "--yes"),
            cwd=ROOT,
            env=client.env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        child.stdin.write(invitation)
        child.stdin.close()
        child.stdin = None
        wait(at_commit_barrier, timeout=20)
        child.send_signal(signal.SIGTERM)
        stdout, stderr = child.communicate(timeout=15)
        assert child.returncode != 0, "Pairing should be interrupted before its confirmed response"
        assert invitation not in stdout and invitation not in stderr
        pending = client.call("operation", "list")["pending_pairing"]
        operation_id, host_id = pending
        assert host_id == scenario.host and operation_id
        assert scenario.request("gx-release-pair-commit-result")["released"]
        released = True
        recovered = client.call("operation", "recover", operation_id)
        assert recovered["operation_id"] == operation_id
        assert recovered["connection"]["host_id"] == scenario.host
        assert client.call("operation", "list")["pending_pairing"] is None

        sessions = client.call("session", "list", "--host", scenario.host)
        assert any(session["session_id"] == session_id for session in sessions["sessions"])
        scenario.model.plan()
        delivery = client.call(
            "session", "send", session_id, "Standalone fixture request", "--timeout", "25"
        )
        assert delivery["target"]["session_id"] == session_id
        assert delivery["reply"]["state"] == "committed"
        assert delivery["reply"]["attempt_id"]
        assert delivery["reply"]["text"] == "native fixture response"
        assert scenario.model.calls == 1
        send_id = delivery["operation_id"]
        message_id = delivery["receipt"]["message_id"]
        assert send_id and message_id
        before_events = scenario.request("events", sessionId=session_id)["types"]

        original_access = scenario.access
        original_access.close()
        status_during_outage = client.call("status")
        assert status_during_outage["device_access"] == "unavailable"
        assert status_during_outage["default_host"] == scenario.host
        exit_code, failed_read = client.invoke(
            "session", "model", "get", "--session", session_id, "--host", scenario.host
        )
        assert exit_code != 0
        code = failed_read["error"]["code"]
        assert code in ("unavailable", "timeout"), f"regional outage returned {code}"
        assert failed_read["data"]["target"]["host_id"] == scenario.host
        assert scenario.model.calls == 1
        scenario.start_access()
        assert scenario.access is not original_access and original_access.child.poll() is not None
        recovered_send = client.call("operation", "recover", send_id)
        assert recovered_send["operation_id"] == send_id
        assert recovered_send["target"] == delivery["target"]
        assert recovered_send["receipt"]["message_id"] == message_id
        assert recovered_send["reply"] == delivery["reply"]
        assert scenario.model.calls == 1
        assert scenario.request("events", sessionId=session_id)["types"] == before_events
    finally:
        if not released:
            scenario.request("gx-release-pair-commit-result")
        if child is not None and child.poll() is None:
            child.kill()
            child.communicate(timeout=5)



def test_native_dsh_advanced_controls(scenario, platform_keyring):
    """The new CLI creates and forks native sessions and preserves capability guards."""
    client = CutoverClient(scenario.directory, "advanced-controls", platform_keyring)
    invitation = http(
        "/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin
    )["pairingUri"]
    client.call("host", "connect", "--invite-stdin", "--yes", input_text=invitation)
    created = client.call(
        "session", "create", "--host", scenario.host, "--cwd", str(scenario.work)
    )
    receipt = created["lifecycle_receipt"]
    assert receipt["disposition"] in ("READY", "READY_WITH_WARNING")
    parent = receipt["session_id"]
    assert parent

    listing = client.call("session", "list", "--host", scenario.host)
    assert any(item["session_id"] == parent for item in listing["sessions"])
    model = client.call(
        "session", "model", "get", "--session", parent, "--host", scenario.host
    )
    native_model = scenario.request("model-read", sessionId=parent)
    assert model["default_is_fallback"] == native_model["default_is_fallback"]
    for key in ("next", "default_selection"):
        actual, expected = model[key], native_model[key]
        assert (actual is None) == (expected is None)
        if expected:
            assert actual["model"] == expected["model"]
            assert actual["provider"] == expected["provider"]
    assert model["default_selection"]
    assert scenario.model.calls == 0

    scenario.model.plan()
    sent = client.call(
        "session", "send", parent, "Advanced controls fixture", "--timeout", "25"
    )
    assert sent["reply"]["state"] == "committed"
    idle = client.call("session", "wait", parent, "--timeout", "10", "--fail-on-timeout")
    assert idle["session_id"] == parent and idle["reason"] == "idle"
    assert idle["state"]["running"] is False

    assert scenario.request("test-enable-fork")["enabled"]
    fork = client.call("session", "fork", parent, "--host", scenario.host)[
        "lifecycle_receipt"
    ]
    assert fork["disposition"] in ("READY", "READY_WITH_WARNING")
    child = fork["session_id"]
    assert child and child != parent
    assert fork["parent_session_id"] == parent
    assert scenario.request("same-cwd", sessionId=parent, config={"childId": child})["same"]
    assert scenario.model.calls == 1

    watcher = subprocess.Popen(
        client.args(
            "session", "watch", parent, "--host", scenario.host, "--once", "--timeout", "15"
        ),
        cwd=ROOT,
        env=client.env,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
    )
    try:
        wait(lambda: scenario.request("watches") > 0, timeout=6)
        readable, _, _ = select.select([watcher.stdout], [], [], 6)
        assert readable, "Native watch never emitted its initial scoped event"
        first = watcher.stdout.readline()
        assert first, "Native watch stopped before following output"
        scenario.model.plan()
        scenario.request("start", sessionId=parent)
        output, _ = watcher.communicate(timeout=20)
        assert watcher.returncode == 0, "Native one-shot watch did not complete"
        envelopes = [json.loads(line) for line in (first + output).splitlines()]
        events = [
            envelope["data"]["event"]
            for envelope in envelopes
            if "event" in envelope.get("data", {})
        ]
        assert any(event["type"] in ("message", "preview") for event in events)
        assert all(
            event["session"] == {"host_id": scenario.host, "session_id": parent}
            for event in events
        )
        assert all(envelope["schema"] == "whalerein/client/v1" for envelope in envelopes)
        assert envelopes[-1]["data"]["session_id"] == parent
        scenario.wait_idle(parent, assistant_count=2)
    finally:
        if watcher.poll() is None:
            watcher.kill()
            watcher.communicate(timeout=5)

    assert scenario.request("test-model-catalog-only")["enabled"]
    catalog = client.call("session", "models", "--host", scenario.host)
    group = next(group for group in catalog["groups"] if group["models"])
    model_id = group["models"][0]["id"]
    for args in [
        ("get", "--session", parent),
        ("set", "--session", parent, "--provider", group["id"], "--model", model_id),
    ]:
        code, envelope = client.invoke("session", "model", *args, "--host", scenario.host)
        assert code != 0 and envelope["error"]["code"] == "unsupported_capability"
    assert scenario.request("test-model-catalog-only-counts") == {"read": 0, "select": 0}


def test_native_dsh_queue_approval_question_and_interrupt(scenario, platform_keyring):
    """Exercise native pending work and guarded user input through the new CLI."""
    client = CutoverClient(scenario.directory, "guarded-controls", platform_keyring)
    invitation = http(
        "/v1/pairing/invitations", {"hostId": scenario.host}, scenario.admin
    )["pairingUri"]
    client.call("host", "connect", "--invite-stdin", "--yes", input_text=invitation)

    queue_session = scenario.create()
    scenario.request("enable-delivery-for-test")
    scenario.model.plan(hold=True)
    try:
        scenario.request("start", sessionId=queue_session)
        assert scenario.model.started.wait(timeout=15)
        queued = client.call(
            "session", "send", queue_session, "Queued controls fixture", "--detach",
            "--delivery", "enqueue-after-run",
        )
        delivery_id = queued["receipt"]["delivery_id"]
        assert delivery_id

        def queued_item():
            result = client.call("session", "queue", "list", "--session", queue_session)
            assert result["available"] and result["running"]
            return next(
                (item for item in result["items"] if item["delivery_id"] == delivery_id), None
            )

        wait(queued_item, timeout=12, process=scenario.native)
        item_id = queued_item()["item_id"]
        assert item_id and item_id != delivery_id
        client.call("session", "queue", "remove", "--session", queue_session, "--item", item_id)
        wait(lambda: queued_item() is None, timeout=10, process=scenario.native)
    finally:
        scenario.model.release.set()
    scenario.wait_idle(queue_session)

    approval_session = scenario.create()
    scenario.model.plan("tool")
    client.call(
        "session", "send", approval_session, "Approval guard fixture", "--detach",
        "--delivery", "start-if-idle",
    )

    def pending_approvals():
        return client.call("session", "approval", "list", "--session", approval_session)["approvals"]

    wait(pending_approvals, timeout=15, process=scenario.native)
    approval = pending_approvals()[0]
    bad_digest = ("0" if approval["digest"][0] != "0" else "1") + approval["digest"][1:]
    for digest, version in (
        (bad_digest, approval["version"]),
        (approval["digest"], approval["version"] + 1),
    ):
        code, result = client.invoke(
            "session", "approval", "approve", "--request", approval["approval_id"],
            "--digest", digest, "--version", str(version),
        )
        assert code != 0
        assert result["error"]["code"] in {"unknown", "conflict", "approval_version_mismatch", "payload_digest_mismatch"}
    assert pending_approvals()[0]["approval_id"] == approval["approval_id"]
    assert scenario.request("effects")["count"] == 0
    client.call("session", "interrupt", approval_session)
    wait(
        lambda: not scenario.request("state", sessionId=approval_session)["running"],
        process=scenario.native,
    )
    assert pending_approvals() == []
    assert scenario.request("events", sessionId=approval_session)["decisions"] == ["cancelled"]
    assert scenario.request("effects")["count"] == 0

    for decision, native_outcome in (("approve", "allowed-once"), ("reject", "rejected")):
        approval_session = scenario.create()
        scenario.model.plan("tool")
        client.call(
            "session", "send", approval_session, "Exact approval fixture", "--detach",
            "--delivery", "start-if-idle",
        )
        wait(pending_approvals, timeout=15, process=scenario.native)
        approval = pending_approvals()[0]
        client.call(
            "session", "approval", decision, "--request", approval["approval_id"],
            "--digest", approval["digest"], "--version", str(approval["version"]),
        )
        scenario.wait_idle(approval_session)
        decisions = scenario.request("events", sessionId=approval_session)["decisions"]
        assert decisions == [native_outcome]
        assert scenario.request("effects")["count"] == 1

    question_session = scenario.create()
    assert scenario.request("question-enable-remote-fixture")["enabled"]
    assert scenario.request("question-start-remote-fixture", sessionId=question_session)["started"]

    def current_questions():
        return client.call("session", "question", "list", "--session", question_session)["batches"]

    batch, = current_questions()
    assert batch["answerable"] and batch["phase"] == "QUESTION_OPEN"
    answer = json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]})

    def question_args(guard):
        return (
            "session", "question", "answer", "--session", question_session,
            "--batch", guard["batch_id"], "--epoch", guard["source_epoch"],
            "--version", str(guard["version"]), "--digest", guard["digest"],
            "--phase", guard["phase"], "--input", "-",
        )

    bad_digest = ("0" if batch["digest"][0] != "0" else "1") + batch["digest"][1:]
    for changes in (
        {"source_epoch": "stale-epoch"},
        {"version": batch["version"] + 1},
        {"digest": bad_digest},
    ):
        code, failure = client.invoke(*question_args({**batch, **changes}), input_text=answer)
        assert code != 0 and failure["error"]["code"] == "conflict"
        current, = current_questions()
        assert current["batch_id"] == batch["batch_id"] and current["answerable"]
    result = client.call(*question_args(batch), input_text=answer)
    assert result["question_receipt"]["phase"] == "QUESTION_SETTLED"
    assert scenario.request("question-result-remote-fixture", sessionId=question_session) == {
        "selected": "Yes", "pending": False,
    }
