"""Shared helpers for the JRNY cross-component journeys.

The cases in `cases/test_journeys*.py` exercise multi-step operator journeys.
Keeping the process, proxy, clock and database controls here leaves the case
bodies focused on the user-visible decision and expected outcome.

Nothing here exports message bodies, tokens or private keys. Proxy and browser
probes report only bounded stage codes and public identities.
"""

import json
import os
import signal
import sqlite3
import subprocess
import threading
import time
import urllib.parse
import uuid
from http.client import HTTPConnection
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

from environment import ROOT, Process, http, wait, approval_card

CLI = ROOT / "target/debug/whalerein"
DSH_FIXTURE = ROOT / "tests/integration/fixtures/dsh.mjs"
PLUGIN_FIXTURE = ROOT / "tests/integration/fixtures/plugin.mjs"
JOURNEY_BROWSER = ROOT / "tests/integration/native/journey-browser.mjs"


# --- PostgreSQL facts -------------------------------------------------------


def sql(schema, statement):
    """Run one statement in the case schema and return its last output line."""
    result = subprocess.run(
        [
            "psql",
            "-h",
            "127.0.0.1",
            "-p",
            "55432",
            "-U",
            "postgres",
            "-d",
            "whalerein_test",
            "-tAc",
            f"SET search_path TO {schema}; {statement}",
        ],
        check=True,
        capture_output=True,
        text=True,
        timeout=10,
    )
    lines = [line.strip() for line in result.stdout.splitlines() if line.strip()]
    return lines[-1] if lines else ""


def live_grants(schema, device_id):
    return int(
        sql(
            schema,
            "SELECT count(*) FROM grants "
            f"WHERE device_id='{device_id}' AND revoked_at IS NULL",
        )
    )


def row_count(schema, table):
    return int(sql(schema, f"SELECT count(*) FROM {table}"))


def command_request_rows(schema):
    """Control command identities retained for recovery (command_id <> '')."""
    return int(sql(schema, "SELECT count(*) FROM requests WHERE command_id <> ''"))


def rewind_for_next_day(schema):
    """Move every retained record 25 hours into the past.

    A real overnight gap is not schedulable in the suite. Rewinding the
    persisted expiry/creation times exercises the same 24 hour retention and
    recovery code on the next Access start without weakening it.
    """
    sql(
        schema,
        "UPDATE requests SET expires_at = now() - interval '25 hours', "
        "created_at = now() - interval '25 hours'",
    )
    sql(
        schema,
        "UPDATE pairing_tokens SET expires_at = now() - interval '25 hours', "
        "created_at = now() - interval '25 hours'",
    )


# --- host presence ----------------------------------------------------------


def host_online(device, host):
    """Read one Host's presence, or None while the listing itself is unavailable."""
    try:
        rows = device.hosts()
    except (AssertionError, RuntimeError, TimeoutError):
        return None
    return next((row["online"] for row in rows if row["host_id"] == host), None)


def wait_online(device, host, online=True, timeout=30):
    wait(lambda: host_online(device, host) is online, timeout=timeout)


# --- invitations and pairing interruption ----------------------------------


def invitation(scenario, host=None):
    return http(
        "/v1/pairing/invitations", {"hostId": host or scenario.host}, scenario.admin
    )["pairingUri"]


def rewrite_invitation(uri, access_url):
    parts = urllib.parse.urlsplit(uri)
    query = dict(urllib.parse.parse_qsl(parts.query))
    query["access"] = access_url
    return urllib.parse.urlunsplit(parts._replace(query=urllib.parse.urlencode(query)))


class PairingInterruption:
    """Loopback proxy that can lose one pairing claim reply.

    `drop_at="hello"` models a disconnect before any grant commit; the original
    invitation is not consumed. `drop_at="finish"` models a committed pairing
    whose reply never reached the client, which must then resume the retained
    FINISH instead of pairing again.
    """

    def __init__(self, drop_at=None, listen_port=0, upstream_port=18080):
        self.drop_at = drop_at
        self.upstream_port = upstream_port
        self.lost = threading.Event()
        self.errors = []
        owner = self

        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *_args):
                return

            def forward(self):
                upstream = HTTPConnection("127.0.0.1", owner.upstream_port, timeout=25)
                try:
                    length = int(self.headers.get("Content-Length", "0") or "0")
                    body = self.rfile.read(length)
                    headers = {
                        key: value
                        for key, value in self.headers.items()
                        if key.lower() not in {"host", "connection"}
                    }
                    upstream.request(self.command, self.path, body, headers)
                    response = upstream.getresponse()
                    payload = response.read()
                    claim = {}
                    if self.path == "/v1/pairing/claims":
                        try:
                            claim = json.loads(body)
                        except ValueError:
                            claim = {}
                    phase = claim.get("phase")
                    drops = (
                        response.status == 200
                        and owner.drop_at == "hello"
                        and phase == "PAIRING_PHASE_HELLO"
                    ) or (
                        response.status == 200
                        and owner.drop_at == "finish"
                        and phase == "PAIRING_PHASE_FINISH"
                    )
                    if drops:
                        # The request reached Access; only the reply is lost.
                        owner.lost.set()
                        self.close_connection = True
                        return
                    self.send_response(response.status)
                    self.send_header("Content-Type", "application/json")
                    self.send_header("Content-Length", str(len(payload)))
                    self.end_headers()
                    self.wfile.write(payload)
                except (ConnectionError, OSError, ValueError):
                    owner.errors.append("pairing proxy request failed")
                    self.close_connection = True
                finally:
                    upstream.close()

            do_POST = forward
            do_GET = forward

        self.server = ThreadingHTTPServer(("127.0.0.1", listen_port), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    @property
    def access_url(self):
        return f"http://127.0.0.1:{self.server.server_port}"

    def route(self, uri):
        return rewrite_invitation(uri, self.access_url)

    def close(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)


# --- additional Host --------------------------------------------------------


class SecondHost:
    """A second independently enrolled Host on the same Access and broker."""

    def __init__(self, scenario, suffix="2"):
        self.scenario = scenario
        self.directory = scenario.directory / f"host-{suffix}"
        self.directory.mkdir()
        self.socket = self.directory / "dsh.sock"
        self.host = "host-" + uuid.uuid4().hex[:16]
        self.authority = Process(
            ["node", str(DSH_FIXTURE)],
            self.directory,
            {"WHALEREIN_DSH_SOCKET": str(self.socket)},
            control=True,
        )
        wait(lambda: self.socket.exists(), process=self.authority)
        env = {
            "WHALEREIN_DSH_SOCKET": str(self.socket),
            "WHALEREIN_MQTT_URL": "mqtt://127.0.0.1:1883",
            "WHALEREIN_HOST_ID": self.host,
            "WHALEREIN_HOST_TOKEN": scenario.env["WHALEREIN_HOST_PROVISIONING_TOKEN"],
            "WHALEREIN_STATE_DIR": str(self.directory / "plugin"),
            "WHALEREIN_HOST_CREDENTIAL_FILE": str(self.directory / "credentials.json"),
        }
        if getattr(scenario, "tls_ca", None):
            env.update(
                {
                    "WHALEREIN_CA_FILE": scenario.tls_ca,
                    "WHALEREIN_ALLOW_INSECURE_MQTT": "0",
                }
            )
        self.plugin = Process(
            ["node", str(PLUGIN_FIXTURE)],
            self.directory,
            env,
            control=False,
        )
        wait(
            lambda: "plugin ready" in Path(self.plugin.output.name).read_text(),
            process=self.plugin,
        )

    def pair(self, device):
        device.pair(invitation(self.scenario, self.host))
        return self

    def create_session(self, session_id):
        self.authority.control("create_session", session_id=session_id)
        return session_id

    def start_running(self, session_id):
        return self.authority.control("running", session_id=session_id)

    def snapshot(self, session_id):
        return self.authority.control("snapshot", session_id=session_id)

    def effects(self, method):
        return self.authority.control("gx_effect_counts")[method]

    def peer_exists(self, device_id):
        return host_peer_exists(self.directory, device_id)

    def close(self):
        for process in (self.plugin, self.authority):
            try:
                process.close()
            except Exception:  # noqa: BLE001 - release every owned process
                pass


def host_peer_exists(directory, device_id):
    state = Path(directory) / "plugin/state.sqlite"
    if not state.exists():
        return False
    with sqlite3.connect(state) as connection:
        return (
            connection.execute(
                "SELECT 1 FROM peer_keys WHERE device_id=?", (device_id,)
            ).fetchone()
            is not None
        )


# --- fixture controls -------------------------------------------------------


def request_approval(scenario, session_id, ttl_ms=None):
    fields = {"session_id": session_id}
    if ttl_ms is not None:
        fields["ttl_ms"] = ttl_ms
    return approval_card(scenario.dsh.process.control("request_approval", **fields))


def effects(scenario, method):
    return scenario.dsh.process.control("gx_effect_counts")[method]


def approval_args(approval):
    return [
        "--request",
        approval["approval_id"],
        "--digest",
        approval["digest"],
        "--version",
        str(approval["version"]),
    ]


def invoke_approval(device, decision, approval, success=False):
    return device.invoke("session", "approval", decision, *approval_args(approval), success=success)


def answer_question(device, session, batch, payload=None):
    path = device.directory / f"journey-answer-{device.namespace}.json"
    path.write_text(
        json.dumps(
            payload
            or {"answers": [{"id": "q1", "selected": ["Yes"]}]}
        )
    )
    return device.call(
        "session", "question", "answer",
        "--session",
        session,
        "--batch",
        batch["batch_id"],
        "--epoch",
        batch["source_epoch"],
        "--version",
        str(batch["version"]),
        "--digest",
        batch["digest"],
        "--phase",
        batch["phase"],
        "--input",
        str(path),
    )


def event_messages(values):
    events = [
        value["data"]["event"]
        for value in values
        if value.get("data", {}).get("event")
    ]
    return [event for event in events if event["type"] == "message"]


def message_texts(event):
    return [
        block.get("text", "")
        for block in event.get("blocks", [])
        if block.get("type") == "text"
    ]


def assistant_texts(values):
    return [
        text
        for event in event_messages(values)
        if event.get("role") == "ASSISTANT"
        for text in message_texts(event)
    ]


def pending_commands(device):
    return device.call("operation", "list")["pending_commands"]


# --- human output -----------------------------------------------------------


def human_invoke(device, *args, timeout=60):
    """Run one CLI command without `--json` and return its exit/stdout/stderr."""
    result = subprocess.run(
        [str(CLI), "--config", str(device.config_path), *args],
        cwd=ROOT,
        env=device.env(),
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )
    return result.returncode, result.stdout, result.stderr


# --- clock, process and transport controls ---------------------------------


def _process_id(process):
    """Return the group leader for a fixture Process or a raw subprocess."""
    return process.child.pid if hasattr(process, "child") else process.pid


def stop_process(process):
    os.killpg(_process_id(process), signal.SIGSTOP)


def continue_process(process):
    os.killpg(_process_id(process), signal.SIGCONT)


def restart_access(scenario):
    scenario.access.close()
    scenario.start_access()


def relocate_access(scenario, upstream_port=18082):
    """Move Access' listener while keeping its advertised public URL.

    An already-paired client refuses an invitation whose Access URL differs from
    the stored one, so a transparent proxy must keep `public_url` (18080) and
    forward to the moved listener. The caller starts the proxy on 18080 and then
    starts Access, whose readiness probe travels through the proxy.
    """
    text = scenario.config.read_text()
    updated = text.replace(
        'listen: "127.0.0.1:18080"',
        f'listen: "127.0.0.1:{upstream_port}"',
        1,
    )
    assert updated != text, "the scenario must listen on the default port"
    scenario.config.write_text(updated)
    scenario.access.close()
    return upstream_port


def restart_plugin(scenario, extra_env=None):
    scenario.plugin.close()
    scenario.start_plugin(extra_env=extra_env)


def restart_broker(scenario):
    """Restart the broker used by this case, keeping its configuration."""
    broker = getattr(scenario, "broker", None)
    assert broker is not None, "the scenario fixture must expose its broker"
    broker.close()
    broker.ensure_started()
    return broker


def stop_broker(scenario):
    broker = getattr(scenario, "broker", None)
    assert broker is not None, "the scenario fixture must expose its broker"
    broker.close()
    return broker


def start_broker(scenario):
    broker = getattr(scenario, "broker", None)
    assert broker is not None, "the scenario fixture must expose its broker"
    broker.ensure_started()
    return broker


def rotate_tls_certificate(scenario):
    """Restart the case TLS listener with a different leaf signed by the same CA."""
    broker = getattr(scenario, "tls_broker", None)
    assert broker is not None, "certificate rotation requires the tls marker"
    broker.close()
    from tls_environment import certificate

    broker.cert = certificate("server-rotated")
    broker.start()
    return broker


def rewrite_config(device, mutate):
    """Rewrite the local pairing config through a mutation function."""
    path = device.data_dir / "regional.json"
    config = json.loads(path.read_text())
    mutate(config)
    path.write_text(json.dumps(config))


# --- native browser journeys ------------------------------------------------


def browser_journey(scenario, action, **values):
    """Run the JRNY browser script against the native Web launch URL."""
    data = {"action": action, "url": scenario.web_url(), **values}
    child = subprocess.Popen(
        ["node", str(JOURNEY_BROWSER)],
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        output, _ = child.communicate(json.dumps(data), timeout=90)
    except subprocess.TimeoutExpired:
        raise AssertionError("Journey browser deadline exceeded") from None
    finally:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.communicate(timeout=5)
    try:
        reply = json.loads(output)
    except ValueError:
        raise AssertionError("Journey browser failed during launch") from None
    assert child.returncode == 0, (
        "Journey browser stage failed: "
        + str(reply.get("stage", "unknown"))
    )
    assert reply.get("ok"), "Journey browser assertion failed"
    return reply


def sleep_until(predicate, timeout=15):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(0.02)
    return False
