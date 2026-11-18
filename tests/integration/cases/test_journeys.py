"""Cross-component operator journeys (JRNY-01 .. JRNY-21).

Each case follows one user-visible journey through the real Linux product
stack. Unit behavior is covered elsewhere; these cases only assert outcomes
that appear when Access, the broker, the Host plugin, the CLI and PostgreSQL
are combined. A failing case that exposes a product defect is kept as written
and reported with its owner domain instead of being weakened.
"""

import json
import subprocess
import time
import uuid

import pytest

from environment import Device, ROOT, http, wait

from journey_helpers import (
    PairingInterruption,
    SecondHost,
    answer_question,
    assistant_texts,
    effects,
    host_online,
    human_invoke,
    invitation,
    invoke_approval,
    live_grants,
    pending_commands,
    request_approval,
    relocate_access,
    restart_access,
    restart_broker,
    restart_plugin,
    rewind_for_next_day,
    rewrite_config,
    rotate_tls_certificate,
    row_count,
    sql,
    start_broker,
    stop_broker,
    stop_process,
    continue_process,
    wait_online,
)

pytestmark = pytest.mark.chain


def _session(scenario):
    return scenario.dsh.create_session()


def _event(value):
    return value.get("data", {}).get("event") or {}


def _texts(event):
    return [block.get("text", "") for block in event.get("blocks", [])]


# --- JRNY-01 ----------------------------------------------------------------


@pytest.mark.description(
    "A fresh self-hosted installation follows the documented commands from an empty database through invitation, pairing, creation, send and tail to its first committed message."
)
def test_journey_first_self_hosted_message(scenario):
    """The first operator conversion works using only documented commands."""
    device = Device(scenario.directory)
    # The stack start enrolled the Host itself; no client grant or request exists.
    assert int(sql(scenario.schema, "SELECT count(*) FROM grants")) == 0
    assert row_count(scenario.schema, "requests") == 0
    assert row_count(scenario.schema, "devices") == 0
    code, values = device.invoke("host", "list", success=False)
    assert code == 4 and values[-1]["error"]["code"] == "not_paired"

    # Documented operator invitation endpoint, then the documented CLI flow.
    device.identity = device.pair(invitation(scenario))
    wait_online(device, scenario.host)
    hosts = device.hosts()
    assert [row["host_id"] for row in hosts] == [scenario.host]
    assert hosts[0]["online"] is True

    assert device.sessions(scenario.host) == []
    created = device.call("session", "create", "--host", scenario.host)
    receipt = created["lifecycle_receipt"]
    assert created["status"] == "succeeded" and receipt["disposition"] == "READY"
    session = receipt["session_id"]

    # The quickstart opens `tail` in one terminal before sending from another.
    # Sync first so the watcher attaches to a committed cut: snapshot history is
    # not replayed as new output (STREAM-01).
    device.observe_history(session)
    watch = device.start_watch("session", "watch", session, "--timeout", "30")
    try:
        watch.wait(lambda value: _event(value).get("type") == "state", timeout=20)
        sent = device.send(session, "first self-hosted message")["receipt"]
        assert sent["status"] == "succeeded" and sent["delivery_id"]
        watch.wait(
            lambda value: any(
                "first self-hosted message" in text for text in _texts(_event(value))
            ),
            timeout=25,
        )
    finally:
        watch.close()
    assert any("first self-hosted message" in text for text in assistant_texts(watch.values))


    doctor = device.call("doctor")
    assert doctor["address"].rstrip("/") == "http://127.0.0.1:18080"
    assert doctor["commands"] == {"non_terminal": 0, "unknown": 0}
    assert pending_commands(device) == []


# --- JRNY-03 ----------------------------------------------------------------


@pytest.mark.description(
    "When an approval expires while the operator is away, the stale card fails with approval_expired and wait never reports a phantom pending input."
)
def test_journey_approval_expires_while_away(scenario, paired_device):
    """An expired approval authorizes nothing and leaves no silent pending wait."""
    session = _session(scenario)
    scenario.dsh.start_running(session)
    request_approval(scenario, session, ttl_ms=1500)
    listed = paired_device.approvals(session)
    assert len(listed) == 1
    assert listed[0]["expires_at_unix_ms"] > 0

    # The operator is away (locked phone): no interaction until the lifetime passed.
    time.sleep(1.8)
    assert paired_device.approvals(session) == []

    code, values = invoke_approval(paired_device, "approve", listed[0])
    assert code == 8 and values[-1]["error"]["code"] == "approval_expired"
    assert values[-1].get("data") is None
    assert effects(scenario, "resolveApproval") == 0

    observation = paired_device.call("session", "wait", session, "--timeout", "1")
    # An expired approval releases the native turn: the session is idle, with no
    # phantom pending input. It must not report a stale running/input-required.
    assert observation["reason"] == "idle"
    assert observation["state"]["pending_input"] == []
    assert observation["state"]["running"] is False


# --- JRNY-04 ----------------------------------------------------------------


@pytest.mark.description(
    "When the network path to Access and the broker changes, the Host leaves presence, a live watcher rebuilds its channel and later live output arrives exactly once."
)
def test_journey_network_path_change_rebuilds_the_channel(scenario, paired_device):
    """A broker/Access path change ends in a rebuilt channel, not a stuck watch."""
    session = _session(scenario)
    paired_device.send(session, "before path change")
    paired_device.observe_history(session)
    watch = paired_device.start_watch("session", "watch", session, "--timeout", "90")
    try:
        watch.wait(lambda value: _event(value).get("type") == "state", timeout=20)
        # The path disappears, Access notices the missing Host, and both sides restart.
        stop_broker(scenario)
        wait_online(paired_device, scenario.host, online=False, timeout=30)
        start_broker(scenario)
        restart_access(scenario)
        wait_online(paired_device, scenario.host, timeout=45)

        scenario.dsh.process.control("emit", session_id=session)
        try:
            watch.wait(
                lambda value: _event(value).get("type") == "message"
                and _event(value).get("role") == "ASSISTANT"
                and any("fixture output" in text for text in _texts(_event(value))),
                timeout=45,
            )
        except Exception as error:  # noqa: BLE001 - observation then re-raise
            raise
        later = paired_device.send(session, "after path change")["receipt"]
        assert later["status"] == "succeeded"
        watch.wait(
            lambda value: any("after path change" in text for text in _texts(_event(value))),
            timeout=30,
        )
    finally:
        watch.close()
    assert assistant_texts(watch.values).count("fixture output") == 1
    assert sum("after path change" in text for text in assistant_texts(watch.values)) == 1


# --- JRNY-05 ----------------------------------------------------------------


@pytest.mark.description(
    "Two watchers on one session see the mid-join baseline only as history, then both see each new live message exactly once."
)
def test_journey_two_watchers_baseline_and_live_boundary(scenario, paired_device):
    """A late watcher joins through a baseline and shares the live boundary."""
    session = _session(scenario)
    paired_device.send(session, "baseline fixture")
    first = paired_device.start_watch("session", "watch", session, "--timeout", "60")
    second = scenario.pair_second()
    other = None
    try:
        first.wait(lambda value: _event(value).get("type") == "reset", timeout=20)
        other = second.start_watch("session", "watch", session, "--timeout", "60")
        other.wait(lambda value: _event(value).get("type") == "reset", timeout=20)
        paired_device.send(session, "live boundary fixture")
        for watch in (first, other):
            watch.wait(
                lambda value: any(
                    "live boundary fixture" in text for text in _texts(_event(value))
                ),
                timeout=25,
            )
        time.sleep(0.3)
        for name, watch in (("first", first), ("second", other)):
            texts = assistant_texts(watch.values)
            assert sum("live boundary fixture" in text for text in texts) == 1, name
            assert all("baseline fixture" not in text for text in texts), name
    finally:
        first.close()
        if other is not None:
            other.close()


# --- JRNY-06 ----------------------------------------------------------------


@pytest.mark.description(
    "When one operator approves while a second operator sends, the second operator's stale approval card fails with approval_already_resolved and the source decides once."
)
def test_journey_stale_approval_card_after_a_second_operator(scenario, paired_device):
    """An approval card read before another operator's decision cannot be reused."""
    session = _session(scenario)
    second = scenario.pair_second()
    request_approval(scenario, session)
    first_card = paired_device.approvals(session)[0]
    stale_card = second.approvals(session)[0]
    assert stale_card["approval_id"] == first_card["approval_id"]

    while_running = second.send_confirmed(session, "operator B send fixture")
    assert while_running["status"] == "succeeded"
    decision = paired_device.resolve("approve", first_card)
    assert decision["status"] in {"host_accepted", "succeeded"}
    assert effects(scenario, "resolveApproval") == 1

    code, values = invoke_approval(second, "approve", stale_card)
    assert code == 8 and values[-1]["error"]["code"] == "approval_already_resolved"
    assert values[-1].get("data") is None
    assert effects(scenario, "resolveApproval") == 1
    assert second.approvals(session) == []


# --- JRNY-08 ----------------------------------------------------------------


@pytest.mark.description(
    "Across a simulated day boundary the 24 hour sweep removes expired invitations and query records while the uncertain command identity, its recovery and last night's cached history survive."
)
def test_journey_cross_day_cleanup_and_recovery(scenario, paired_device):
    """Retention cleans expired records but never the recovery identity."""
    session = _session(scenario)
    paired_device.send(session, "last night fixture")
    paired_device.observe_history(session)
    assert invitation(scenario)

    marker = scenario.directory / "plugin" / "gx-drop-terminal"
    marker.write_text("armed")
    request_id = uuid.uuid4().hex
    code, values = paired_device.invoke(
        "session", "send", session, "--detach", "--operation-id", request_id,
        "uncertain command fixture", success=False,
    )
    assert not marker.exists()
    assert code in (0, 7)
    assert values[-1]["data"]["operation_id"] == request_id
    records = paired_device.call("operation", "list")["sends"]
    assert any(record["operation_id"] == request_id for record in records)
    restart_plugin(scenario)
    wait_online(paired_device, scenario.host)
    assert any(record["operation_id"] == request_id for record in paired_device.call("operation", "list")["sends"])

    # Record the read-only requests that this case already created; the next
    # day's sweep must remove exactly those aged records. Requests created after
    # the sweep are current, not expired.
    stale_queries = [
        value
        for value in sql(
            scenario.schema,
            "SELECT coalesce(string_agg(request_id, ','), '') FROM requests "
            "WHERE command_id='' AND kind=2",
        ).split(",")
        if value
    ]
    assert stale_queries, "the case must retain read-only requests to expire"

    # The next day: every retained timestamp is more than a day old.
    rewind_for_next_day(scenario.schema)
    restart_access(scenario)
    wait_online(paired_device, scenario.host, timeout=45)
    restart_plugin(scenario)
    wait_online(paired_device, scenario.host)

    # The sweep ran: expired invitations and the aged read-only query records
    # are gone.
    stale_filter = ",".join("'" + value + "'" for value in stale_queries)
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        cleaned = (
            row_count(scenario.schema, "pairing_tokens") == 0
            and int(
                sql(
                    scenario.schema,
                    f"SELECT count(*) FROM requests WHERE request_id IN ({stale_filter})",
                )
            )
            == 0
        )
        if cleaned:
            break
        time.sleep(0.5)
    remaining = int(
        sql(
            scenario.schema,
            f"SELECT count(*) FROM requests WHERE request_id IN ({stale_filter})",
        )
    )
    assert row_count(scenario.schema, "pairing_tokens") == 0
    assert remaining == 0

    # The control identity and its terminal outcome are retained.
    assert int(
        sql(
            scenario.schema,
            f"SELECT count(*) FROM requests WHERE request_id='{request_id}'",
        )
    ) == 1
    assert sql(
        scenario.schema,
        f"SELECT bodies_cleared FROM requests WHERE request_id='{request_id}'",
    ) == "t"

    before = effects(scenario, "sendUserMessage")
    recovered = paired_device.call("operation", "recover", request_id)
    assert recovered["operation_id"] == request_id
    assert recovered["receipt"]["command_id"] == request_id
    assert recovered["reply"]["state"] == "committed"
    assert effects(scenario, "sendUserMessage") == before

    assert any(row["session_id"] == session for row in paired_device.sessions(scenario.host))
    cursor, messages = paired_device.cached_history_state(scenario.host, session)
    assert cursor is not None and len(messages) >= 2
    snapshot = scenario.dsh.snapshot(session)
    assert sum(
        "last night fixture" in json.dumps(event)
        for event in snapshot["events"]
        if event["type"] == "user/message"
    ) == 1


# --- JRNY-09 ----------------------------------------------------------------


@pytest.mark.description(
    "A plugin upgrade in the middle of a task changes advertised capabilities and still recovers the old command id from the Host journal without replaying it."
)
def test_journey_plugin_upgrade_mid_task(scenario, paired_device):
    """Capabilities change on upgrade; the old command identity is recovered, never replayed."""
    session = _session(scenario)
    scenario.dsh.start_running(session)
    paired_device.send(session, "queued during the long task")

    marker = scenario.directory / "plugin" / "gx-drop-terminal"
    marker.write_text("armed")
    request_id = uuid.uuid4().hex
    code, values = paired_device.invoke(
        "session", "send", session, "--detach", "--operation-id", request_id,
        "mid task upgrade command", success=False,
    )
    assert not marker.exists()
    assert code in (0, 7)
    assert values[-1]["data"]["operation_id"] == request_id
    records = paired_device.call("operation", "list")["sends"]
    assert any(record["operation_id"] == request_id for record in records)
    queued_before = scenario.dsh.snapshot(session)["summary"]["queuedPrompts"]
    assert queued_before == 2

    restart_plugin(scenario, extra_env={"WHALEREIN_PENDING_QUEUE_ENABLED": "false"})
    wait_online(paired_device, scenario.host)

    listed = next(
        row for row in paired_device.sessions(scenario.host) if row["session_id"] == session
    )
    assert listed["source_capabilities"]["pending_queue_read"] is False
    code, values = paired_device.invoke(
        "session", "queue", "list", "--session", session, success=False
    )
    assert code == 9 and values[-1]["error"]["code"] == "unsupported_capability"

    scenario.dsh.process.control("idle", session_id=session)
    wait(lambda: scenario.dsh.snapshot(session)["summary"]["queuedPrompts"] == 0)
    queued_before_recovery = scenario.dsh.snapshot(session)["summary"]["queuedPrompts"]
    before_recovery = effects(scenario, "sendUserMessage")
    recovered = paired_device.call("operation", "recover", request_id)
    assert recovered["operation_id"] == request_id
    assert recovered["receipt"]["command_id"] == request_id
    assert recovered["reply"]["state"] == "committed"
    assert effects(scenario, "sendUserMessage") == before_recovery
    assert scenario.dsh.snapshot(session)["summary"]["queuedPrompts"] == queued_before_recovery


# --- JRNY-10 ----------------------------------------------------------------


@pytest.mark.description(
    "An Access upgrade in the middle of pairing and of an in-flight request never leaves a half grant, a false success or a duplicated source call."
)
def test_journey_access_upgrade_during_pairing_and_requests(scenario):
    """An Access restart during pairing or a command is explicit and non-duplicating."""
    device = Device(scenario.directory)
    proxy = PairingInterruption(drop_at="hello")
    try:
        link = invitation(scenario)
        code, _ = device.invoke("host", "connect", "--invite-stdin", "--yes", stdin_text=proxy.route(link), success=False)
        assert code != 0 and proxy.lost.is_set()
        assert row_count(scenario.schema, "grants") == 0
    finally:
        proxy.close()

    # Operator-driven upgrade: rotate the deployment credentials with the restart.
    scenario.env["WHALEREIN_ADMIN_TOKEN"] = uuid.uuid4().hex
    scenario.admin = scenario.env["WHALEREIN_ADMIN_TOKEN"]
    scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"] = uuid.uuid4().hex
    restart_access(scenario)
    restart_plugin(scenario)

    # The interrupted invitation was never consumed, so the same link still pairs.
    device.identity = device.pair(link)
    assert live_grants(scenario.schema, device.identity["device_id"]) == 1
    session = _session(scenario)
    device.sessions(scenario.host)

    scenario.dsh.process.control("hang_next", method_name="sendUserMessage")
    before = effects(scenario, "sendUserMessage")
    attempt = subprocess.Popen(
        [*device.args(),
            "session", "send", session,
            "in-flight during upgrade",
        ],
        cwd=ROOT,
        env=device.env(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        wait(lambda: effects(scenario, "sendUserMessage") == before + 1, timeout=20)
        # The command reached the Host and is hung at the source; the upgrade starts.
        scenario.access.close()
        output, _ = attempt.communicate(timeout=40)
        assert attempt.returncode != 0
        envelopes = [json.loads(line) for line in output.splitlines() if line.strip()]
        assert envelopes and envelopes[-1].get("error")
        assert "reply" not in (envelopes[-1].get("data") or {})
    finally:
        if attempt.poll() is None:
            attempt.kill()
            attempt.communicate(timeout=5)

    assert effects(scenario, "sendUserMessage") == before + 1
    scenario.start_access()
    wait_online(device, scenario.host, timeout=45)
    assert effects(scenario, "sendUserMessage") == before + 1
    for pending in pending_commands(device):
        device.invoke("operation", "recover", pending["request_id"], success=False)
    assert effects(scenario, "sendUserMessage") == before + 1

    # Releasing the hung command requires a Host restart; afterwards a new send works once.
    restart_plugin(scenario)
    wait_online(device, scenario.host)
    after = device.send(session, "after the Access upgrade")["receipt"]
    assert after["status"] == "succeeded"
    assert effects(scenario, "sendUserMessage") == before + 2


# --- JRNY-11 ----------------------------------------------------------------


@pytest.mark.description(
    "After a Host identity change the pinned fingerprint stops the channel with host_fingerprint_mismatch, keeps local authorization, and the documented local recovery path still works."
)
def test_journey_host_identity_loss_guidance(scenario, paired_device):
    """A changed Host identity is not a dead end, but its guidance is incomplete."""
    session = _session(scenario)
    paired_device.send(session, "before the identity change")
    before = effects(scenario, "sendUserMessage")

    rewrite_config(
        paired_device, lambda config: config.__setitem__("host_fingerprint", "00" * 32)
    )
    code, values = paired_device.invoke(
        "session", "send", session, "--detach", "after the identity change", success=False
    )
    assert code == 8 and values[-1]["error"]["code"] == "host_fingerprint_mismatch"
    assert values[-1]["data"]["target"] == {"host_id": scenario.host, "session_id": session}
    assert effects(scenario, "sendUserMessage") == before
    mismatch_message = values[-1]["error"]["message"]
    # Local authorization is preserved, so recovery does not silently log the user out.
    assert scenario.host in {row["host_id"] for row in paired_device.hosts()}
    assert live_grants(scenario.schema, paired_device.identity["device_id"]) == 1

    # The documented last-resort local recovery is available and explicit.
    paired_device.call("device", "forget", "--local-only", "--yes")
    code, values = paired_device.invoke("host", "list", success=False)
    assert code == 4 and values[-1]["error"]["code"] == "not_paired"

    assert any(
        hint in mismatch_message
        for hint in ("whalerein host connect", "whalerein device forget", "revoke", "re-pair")
    ), "a changed Host fingerprint must name the explicit revoke/forget/re-pair path"


# --- JRNY-13 ----------------------------------------------------------------


@pytest.mark.description(
    "An approval click while the Host is offline is not applied, the approval stays pending after reconnect, and exactly one later decision settles it."
)
def test_journey_approval_click_while_host_offline(scenario, paired_device):
    """An offline approval click cannot be mistaken for an approval."""
    session = _session(scenario)
    request_approval(scenario, session)
    card = paired_device.approvals(session)[0]
    scenario.plugin.close()
    wait_online(paired_device, scenario.host, online=False, timeout=30)

    offline_code, offline_values = invoke_approval(paired_device, "approve", card)
    offline_status = (offline_values[-1].get("data") or {}).get("status")

    restart_plugin(scenario)
    wait_online(paired_device, scenario.host)
    still_pending = paired_device.approvals(session)
    assert [item["approval_id"] for item in still_pending] == [card["approval_id"]]

    decided = paired_device.resolve("approve", still_pending[0])
    assert decided["status"] in {"host_accepted", "succeeded"}
    wait(
        lambda: scenario.dsh.approval(card["approval_id"])["state"] == "approved",
        timeout=15,
    )
    assert effects(scenario, "resolveApproval") == 1
    code, values = invoke_approval(paired_device, "approve", card)
    assert code == 8 and values[-1]["error"]["code"] == "approval_already_resolved"
    assert effects(scenario, "resolveApproval") == 1

    assert offline_code != 0, (
        "an approval click while the Host is offline must not exit 0; "
        f"it exited {offline_code} with status {offline_status!r}"
    )
    assert offline_status != "succeeded"


# --- JRNY-14 ----------------------------------------------------------------


@pytest.mark.description(
    "After an overnight disconnect with both Access and the plugin restarted, a previously committed message appears once, the pending approval resolves once and a new send works."
)
def test_journey_overnight_disconnect_and_both_restarts(scenario, paired_device):
    """Both control-plane restarts preserve one decision and one copy of history."""
    session = _session(scenario)
    paired_device.send(session, "before the overnight gap")
    scenario.dsh.start_running(session)
    request_approval(scenario, session)
    approval = paired_device.approvals(session)[0]
    before = effects(scenario, "sendUserMessage")

    scenario.plugin.close()
    scenario.access.close()
    time.sleep(2)
    code, values = paired_device.invoke(
        "session", "send", session, "--detach", "during the gap", success=False
    )
    assert code in {5, 7}
    assert values[-1]["data"]["target"] == {"host_id": scenario.host, "session_id": session}
    assert "reply" not in values[-1]["data"]
    assert effects(scenario, "sendUserMessage") == before

    scenario.start_access()
    scenario.start_plugin()
    wait_online(paired_device, scenario.host, timeout=45)
    pending = paired_device.approvals(session)
    assert [item["approval_id"] for item in pending] == [approval["approval_id"]]
    decided = paired_device.resolve("approve", pending[0])
    # The same non-terminal ack can be returned right after both services
    # restart; the native approval must still settle exactly once.
    assert decided["status"] in {"host_accepted", "succeeded"}
    wait(
        lambda: scenario.dsh.approval(approval["approval_id"])["state"] == "approved",
        timeout=15,
    )
    assert effects(scenario, "resolveApproval") == 1
    code, values = invoke_approval(paired_device, "approve", approval)
    assert code == 8 and values[-1]["error"]["code"] == "approval_already_resolved"
    assert effects(scenario, "resolveApproval") == 1

    scenario.dsh.process.control("idle", session_id=session)
    after = paired_device.send(session, "after the gap")["receipt"]
    assert after["status"] == "succeeded"
    events = scenario.dsh.snapshot(session)["events"]
    for text in ("before the overnight gap", "after the gap"):
        assert sum(
            text in json.dumps(event)
            for event in events
            if event["type"] == "user/message"
        ) == 1, text


# --- JRNY-15 ----------------------------------------------------------------


@pytest.mark.description(
    "A CLI killed while its send is in flight is recovered the next day through operation list and operation recover without resending the message."
)
def test_journey_cli_crash_then_next_day_recovery(scenario, paired_device):
    """A crashed CLI leaves a recoverable identity, never a silent resend."""
    session = _session(scenario)
    scenario.dsh.process.control("hang_next", method_name="sendUserMessage")
    before = effects(scenario, "sendUserMessage")
    attempt = subprocess.Popen(
        [*paired_device.args(),
            "session", "send", session,
            "crashed command fixture",
        ],
        cwd=ROOT,
        env=paired_device.env(),
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        wait(lambda: effects(scenario, "sendUserMessage") == before + 1, timeout=20)
        # The command is hung at the source, so the CLI is still waiting: kill it.
        attempt.kill()
        attempt.communicate(timeout=10)
    finally:
        if attempt.poll() is None:
            attempt.kill()
            attempt.communicate(timeout=5)
    assert effects(scenario, "sendUserMessage") == before + 1

    pending = pending_commands(paired_device)
    assert len(pending) == 1
    request_id = pending[0]["request_id"]
    command_id = pending[0]["command_id"]

    # The next day: restart both services and age the retained records.
    rewind_for_next_day(scenario.schema)
    restart_access(scenario)
    wait_online(paired_device, scenario.host, timeout=45)
    restart_plugin(scenario)
    wait_online(paired_device, scenario.host)
    recover_code, recover_values = paired_device.invoke(
        "operation", "recover", request_id, success=False
    )
    if recover_code == 0:
        # The Host completed the source call before the crash: the recovered
        # copy is the accepted/terminal outcome and the identity must match.
        recovered = recover_values[-1]["data"]
        assert recovered["operation_id"] == request_id
        assert recovered["receipt"]["command_id"] == command_id
        assert recovered["reply"]["state"] == "committed"
    else:
        # The hung source call was aborted by the Host restart. Its durable
        # outcome is explicitly unknown; that is never a false success.
        assert recover_code == 7
        assert recover_values[-1]["error"]["code"] == "unknown"
    assert effects(scenario, "sendUserMessage") == before + 1
    events = scenario.dsh.snapshot(session)["events"]
    assert all("crashed command fixture" not in json.dumps(event) for event in events)

    sent = paired_device.send(session, "after the crash")["receipt"]
    assert sent["status"] == "succeeded"
    assert effects(scenario, "sendUserMessage") == before + 2


# --- JRNY-16 ----------------------------------------------------------------


@pytest.mark.description(
    "A first pairing interrupted before any commit consumes no invitation and no grant, and retrying the same link pairs exactly once."
)
def test_journey_pairing_interrupted_before_commit(scenario):
    """A pre-commit interruption is retryable with the same invitation."""
    device = Device(scenario.directory)
    proxy = PairingInterruption(drop_at="hello")
    try:
        link = invitation(scenario)
        code, _ = device.invoke("host", "connect", "--invite-stdin", "--yes", stdin_text=proxy.route(link), success=False)
        assert code != 0 and proxy.lost.is_set()
        assert row_count(scenario.schema, "grants") == 0
        assert row_count(scenario.schema, "devices") == 0
    finally:
        proxy.close()

    device.identity = device.pair(link)
    assert live_grants(scenario.schema, device.identity["device_id"]) == 1
    assert scenario.host_peer_exists(device.identity["device_id"])
    assert device.sessions(scenario.host) == []


@pytest.mark.description(
    "A second-Host pairing interrupted after Access committed resumes from the retained FINISH into exactly one new grant and no pending command."
)
def test_journey_pairing_interrupted_after_commit_resumes(scenario, paired_device):
    """The retained FINISH completes without a second grant."""
    other = SecondHost(scenario)
    link = invitation(scenario, other.host)
    upstream_port = relocate_access(scenario)
    # The proxy owns the public URL so the already-paired client accepts the
    # invitation; Access moves behind it and keeps the same advertised address.
    proxy = PairingInterruption(
        drop_at="finish", listen_port=18080, upstream_port=upstream_port
    )
    try:
        scenario.start_access(listener_port=upstream_port)
        code, values = paired_device.invoke("host", "connect", "--invite-stdin", "--yes", stdin_text=proxy.route(link), success=False)
        assert code != 0 and proxy.lost.is_set()
        assert live_grants(scenario.schema, paired_device.identity["device_id"]) == 2
        proxy.drop_at = None
        resumed = paired_device.call("operation", "recover", values[-1]["data"]["operation_id"])["connection"]
        assert resumed["host_id"] == other.host
        assert live_grants(scenario.schema, paired_device.identity["device_id"]) == 2
        assert paired_device.call("operation", "list")["pending_commands"] == []
        wait(
            lambda: other.peer_exists(paired_device.identity["device_id"]),
            timeout=15,
            process=other.plugin,
        )
        assert paired_device.sessions(other.host) == []
    finally:
        proxy.close()
        other.close()


# --- JRNY-17 ----------------------------------------------------------------


@pytest.mark.tls("server")
@pytest.mark.description(
    "Rotating the deployment credentials and the broker leaf certificate keeps an existing device grant working without re-pairing while the old administrator token is rejected."
)
def test_journey_token_and_certificate_rotation_keeps_grants(scenario, paired_device):
    """Rotation replaces operator material, not user grants."""
    session = _session(scenario)
    paired_device.send(session, "before rotation")
    device_id = paired_device.identity["device_id"]
    assert live_grants(scenario.schema, device_id) == 1
    old_admin = scenario.admin

    scenario.env["WHALEREIN_ADMIN_TOKEN"] = uuid.uuid4().hex
    scenario.admin = scenario.env["WHALEREIN_ADMIN_TOKEN"]
    scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"] = uuid.uuid4().hex
    rotate_tls_certificate(scenario)
    restart_access(scenario)
    restart_plugin(scenario)
    wait_online(paired_device, scenario.host, timeout=45)

    # The existing grant and its encrypted channel keep working.
    assert live_grants(scenario.schema, device_id) == 1
    assert host_online(paired_device, scenario.host) is True
    # STREAM-01: synchronize the committed cut before observing new output.
    paired_device.observe_history(session)
    watch = paired_device.start_watch("session", "watch", session, "--timeout", "30")
    try:
        watch.wait(lambda value: _event(value).get("type") == "state", timeout=20)
        sent = paired_device.send_confirmed(session, "after rotation")
        assert sent["status"] == "succeeded"
        watch.wait(
            lambda value: any("after rotation" in text for text in _texts(_event(value))),
            timeout=25,
        )
    finally:
        watch.close()
    assert any("after rotation" in text for text in assistant_texts(watch.values))

    # The rotated operator credential replaced the old one.
    with pytest.raises(RuntimeError, match="HTTP (401|403)"):
        http("/v1/pairing/invitations", {"hostId": scenario.host}, old_admin)
    fresh = http(
        "/v1/pairing/invitations",
        {"hostId": scenario.host},
        scenario.env["WHALEREIN_ADMIN_TOKEN"],
    )
    assert fresh["pairingUri"].startswith("whalerein://pair?")


# --- JRNY-18 ----------------------------------------------------------------


@pytest.mark.description(
    "A Host frozen through a network change reconnects without waiting out a full backoff schedule and an approval whose lifetime elapsed during the gap is expired instead of resurrected."
)
def test_journey_frozen_host_clock_and_backoff(scenario, paired_device):
    """A long gap expires approvals and bounds the reconnect delay."""
    session = _session(scenario)
    request_approval(scenario, session, ttl_ms=1200)
    card = paired_device.approvals(session)[0]

    stop_process(scenario.plugin)
    try:
        restart_broker(scenario)
        wait_online(paired_device, scenario.host, online=False, timeout=30)
        # The approval lifetime passes while the Host process timers are frozen.
        time.sleep(1.5)
    finally:
        continue_process(scenario.plugin)

    started = time.monotonic()
    wait_online(paired_device, scenario.host, timeout=25)
    elapsed = time.monotonic() - started
    assert elapsed < 20, f"reconnect after a long gap took {elapsed:.1f}s"

    assert paired_device.approvals(session) == []
    code, values = invoke_approval(paired_device, "approve", card)
    assert code == 8 and values[-1]["error"]["code"] == "approval_expired"
    assert effects(scenario, "resolveApproval") == 0

    fresh = request_approval(scenario, session)
    decided = paired_device.resolve("approve", fresh)
    # A decision acknowledged during a reconnect can still be reported as the
    # non-terminal host_accepted copy; the native approval must nevertheless
    # settle exactly once.
    assert decided["status"] in {"host_accepted", "succeeded"}
    wait(
        lambda: scenario.dsh.approval(fresh["approval_id"])["state"] == "approved",
        timeout=15,
    )
    assert effects(scenario, "resolveApproval") == 1


# --- JRNY-19 ----------------------------------------------------------------


@pytest.mark.description(
    "An empty client state names a concrete next command instead of ending with a generic pairing error."
)
def test_journey_empty_state_guidance(scenario):
    """First-run guidance must tell the operator what to run next."""
    device = Device(scenario.directory)
    code, stdout, stderr = human_invoke(device, "host", "list")
    text = stdout + stderr
    assert code == 4 and "not_paired" in text
    # The empty authorized-host projection already names the pair command; the
    # reachable unpaired error must not be shorter than that projection.
    assert "whalerein host connect" in text or "whalerein device forget" in text, (
        "an unpaired empty state must name the next command, got: " + text.strip()
    )


# --- JRNY-20 ----------------------------------------------------------------


@pytest.mark.description(
    "With two Hosts exposing the same session id, explicit selection routes to each Host and an unqualified operation uses the selected default."
)
def test_journey_multihost_same_named_session_selection(scenario, paired_device):
    """Default and explicit targets remain Host-qualified."""
    other = SecondHost(scenario)
    try:
        other.pair(paired_device)
        shared = "journey-shared-session"
        scenario.dsh.process.control("create_session", session_id=shared)
        other.create_session(shared)
        assert any(
            row["session_id"] == shared for row in paired_device.sessions(scenario.host)
        )
        assert any(
            row["session_id"] == shared for row in paired_device.sessions(other.host)
        )

        # Explicit routing selects the intended Host.
        first = paired_device.call(
            "session", "interrupt", shared, "--host", scenario.host
        )
        assert first["status"] == "succeeded"
        second = paired_device.call(
            "session", "interrupt", shared, "--host", other.host
        )
        assert second["status"] == "succeeded"

        assert paired_device.call("status")["default_host"] == other.host
        default = paired_device.call("session", "interrupt", shared)
        assert default["target"] == {"host_id": other.host, "session_id": shared}
    finally:
        other.close()


# --- JRNY-21 ----------------------------------------------------------------


@pytest.mark.description(
    "Two operators answer the same open question: the first receipt settles it, the second receives a definitive conflict and the source consumes one answer."
)
def test_journey_two_operators_answer_one_question(scenario, paired_device):
    """Only one of two identical answers is consumed."""
    session = _session(scenario)
    second = scenario.pair_second()
    scenario.dsh.process.control("request_question_open", session_id=session)
    first_card = paired_device.call("session", "question", "list", "--session", session)["batches"][0]
    second_card = second.call("session", "question", "list", "--session", session)["batches"][0]
    assert first_card["batch_id"] == second_card["batch_id"]
    assert first_card["digest"] == second_card["digest"]
    before = effects(scenario, "answerOpenQuestion")

    settled = answer_question(paired_device, session, first_card)
    assert settled["question_receipt"]["phase"] == "QUESTION_SETTLED"
    answer = second.directory / f"journey-answer-{second.namespace}.json"
    answer.write_text(json.dumps({"answers": [{"id": "q1", "selected": ["Yes"]}]}))
    code, values = second.invoke(
        "session", "question", "answer",
        "--session",
        session,
        "--batch",
        second_card["batch_id"],
        "--epoch",
        second_card["source_epoch"],
        "--version",
        str(second_card["version"]),
        "--digest",
        second_card["digest"],
        "--phase",
        second_card["phase"],
        "--input",
        str(answer),
        success=False,
    )
    assert code == 8 and values[-1]["error"]["code"] == "conflict"
    assert effects(scenario, "answerOpenQuestion") == before + 1
    assert paired_device.call("session", "question", "list", "--session", session)["batches"] == []
