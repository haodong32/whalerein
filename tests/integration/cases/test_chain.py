"""Independent CLI journeys through the real Linux product stack."""

import pytest
from environment import wait

pytestmark = pytest.mark.chain


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
    status, _ = scenario.replay_command(paired_device, changed=True)
    assert status == 409
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
    import base64
    import json
    import os
    import subprocess
    import urllib.request

    from environment import ROOT

    def live_connection():
        request = urllib.request.Request(
            "http://127.0.0.1:18083/api/v5/clients/whalerein-host-" + scenario.host
        )
        credential = ":".join(
            scenario.env[key]
            for key in ("WHALEREIN_EMQX_API_KEY", "WHALEREIN_EMQX_API_SECRET")
        )
        request.add_header(
            "Authorization", "Basic " + base64.b64encode(credential.encode()).decode()
        )
        with urllib.request.urlopen(request, timeout=5) as response:
            return json.load(response)["connected_at"]

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
