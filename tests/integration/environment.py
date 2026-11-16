"""Concrete container-local process and product helpers."""

import json
import os
import queue
import select
import signal
import subprocess
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Shared broker bootstrap credentials. The plugin authenticates the Access
# identity locally against its password file; the internal token authenticates
# the Access callback the plugin makes for external principals.
BROKER_PASSWORD = "integration-broker-password"
INTERNAL_TOKEN = "compose-internal-token"


def wait(predicate, timeout=60, process=None):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if process is not None and process.child.poll() is not None:
            raise AssertionError("Required fixture process exited before readiness")
        try:
            if predicate():
                return
        except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired):
            pass
        # 20 ms matches the product's delta coalescing window: readiness and
        # command completion are observed promptly without busy-waiting.
        time.sleep(0.02)
    raise TimeoutError("Expected fixture condition was not observed")


class Process:
    def __init__(self, argv, directory, env=None, control=False, cwd=None):
        self.output = (directory / (uuid.uuid4().hex + ".log")).open("w+")
        self.child = subprocess.Popen(
            argv,
            cwd=cwd or ROOT,
            env={**os.environ, **(env or {})},
            stdin=subprocess.PIPE if control else subprocess.DEVNULL,
            stdout=subprocess.PIPE if control else self.output,
            stderr=self.output,
            text=True,
            start_new_session=True,
        )

    def close(self):
        try:
            if self.child.poll() is None:
                try:
                    os.killpg(self.child.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    self.child.wait(timeout=10)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(self.child.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    self.child.wait(timeout=5)
        finally:
            for stream in (self.child.stdin, self.child.stdout, self.output):
                if stream is not None:
                    stream.close()

    def control(self, method, **parameters):
        identity = uuid.uuid4().hex
        self.child.stdin.write(
            json.dumps({"id": identity, "method": method, **parameters}) + "\n"
        )
        self.child.stdin.flush()
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            ready, _, _ = select.select(
                [self.child.stdout], [], [], max(0, deadline - time.monotonic())
            )
            if not ready:
                break
            line = self.child.stdout.readline()
            try:
                result = json.loads(line)
            except ValueError:
                continue
            if result.get("id") == identity:
                if "value" not in result:
                    # The fixture rejected the control; surface the reason
                    # instead of failing later on a missing field.
                    raise RuntimeError(f"Fixture control {method} failed: {result.get('error')}")
                return result["value"]
        raise TimeoutError("Fixture control timed out")


def http(path, body=None, token=None, *, return_status=False, include_conflict_code=False):
    request = urllib.request.Request(
        "http://127.0.0.1:18080" + path,
        data=json.dumps(body).encode() if body is not None else None,
        headers={
            "Content-Type": "application/json",
            **({"Authorization": "Bearer " + token} if token else {}),
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=3) as response:
            payload = json.load(response)
            return (response.status, payload) if return_status else payload
    except urllib.error.HTTPError as error:
        if error.code == 409 and include_conflict_code:
            return 409, {"code": json.load(error).get("code")}
        raise RuntimeError(f"Access returned HTTP {error.code}") from None


class Device:
    """One independently paired CLI identity rooted below a case directory.

    `namespace` separates the data directory, secret store and cache for a
    second physical device. The default keeps every existing case on the
    original `client`/`secrets` paths.
    """

    def __init__(self, directory, namespace="client"):
        self.directory = directory
        self.namespace = namespace
        self.identity = None
        self.extra_env = {}

    @property
    def data_dir(self):
        return self.directory / self.namespace

    @property
    def secret_dir(self):
        return self.directory / (
            "secrets" if self.namespace == "client" else self.namespace + "-secrets"
        )

    def env(self, **overrides):
        return {
            **os.environ,
            "WHALEREIN_SECRET_BACKEND": "file:" + str(self.secret_dir),
            "WHALEREIN_LOG": "error",
            **self.extra_env,
            **overrides,
        }

    def invoke(self, *args, success=True, stdin_text=None, env=None):
        # `stdin_text` is an explicit pipe: passing nothing must not inherit the
        # terminal, and an empty string must still close the stream.
        streams = {} if stdin_text is None else {"input": stdin_text}
        try:
            result = subprocess.run(
                [
                    str(ROOT / "target/debug/whalerein"),
                    "--json",
                    "--yes",
                    "--data-dir",
                    str(self.data_dir),
                    *args,
                ],
                cwd=ROOT,
                env=self.env(**(env or {})),
                capture_output=True,
                text=True,
                timeout=90,
                check=False,
                **streams,
            )
        except subprocess.TimeoutExpired:
            raise TimeoutError("CLI operation timed out") from None
        if success and result.returncode != 0:
            raise AssertionError(f"CLI {args[0]} failed with exit {result.returncode}")
        values = [
            json.loads(line) for line in result.stdout.splitlines() if line.strip()
        ]
        assert values and all(v.get("schema") == "whalerein/v1" for v in values), (
            "Missing CLI envelope"
        )
        return result.returncode, values

    def call(self, *args):
        return self.invoke(*args)[1][-1].get("data", {})

    def hosts(self):
        return self.call("hosts")["hosts"]

    def sessions(self, host):
        return self.call("sessions", "--host", host)["sessions"]

    def send(self, session, message, delivery=None):
        options = ["--delivery", delivery] if delivery else []
        return self.call("send", "--session", session, *options, message)

    def approvals(self, session):
        return self.call("approvals", "--session", session)["approvals"]

    def resolve(self, decision, approval):
        return self.call(
            decision,
            "--request",
            approval["approval_id"],
            "--digest",
            approval["digest"],
            "--version",
            str(approval["version"]),
        )

    def interrupt(self, session):
        return self.call("interrupt", "--session", session)

    def sync(self, session):
        return self.call("sync", "--session", session)

    def cached_history_state(self, host, session):
        """Read committed identity/cursor metadata without decrypting or exporting payloads."""
        import sqlite3

        with sqlite3.connect(self.data_dir / "cache.sqlite3") as cache:
            row = cache.execute(
                "SELECT cursor FROM conversation_views WHERE host_id=? AND session_id=?",
                (host, session),
            ).fetchone()
            messages = cache.execute(
                "SELECT message_id FROM conversation_messages "
                "WHERE host_id=? AND session_id=? ORDER BY ordinal",
                (host, session),
            ).fetchall()
        return (row[0] if row else None, tuple(row[0] for row in messages))

    def revoke(self):
        self._revoked_token = self.transport_token()
        return self.call("revoke")

    def transport_token(self):
        key = "whalerein:v1:device-token:" + self.identity["device_id"]
        return (self.secret_dir / key.encode().hex()).read_text()

    def revoked_access_denied(self):
        try:
            http("/v1/hosts", token=self._revoked_token)
        except RuntimeError as error:
            return any(f"HTTP {status}" in str(error) for status in (401, 403))
        return False

    def events(self, session):
        _, values = self.invoke("tail", "--session", session, "--timeout", "3")
        return [
            value["data"]["event"]
            for value in values
            if value.get("data", {}).get("event")
        ]

    def send_exit(self, session):
        return self.invoke(
            "send", "--session", session, "fixture message", success=False
        )[0]

    def sessions_exit(self, host):
        return self.invoke("sessions", "--host", host, success=False)[0]

    def start_watch(self, *args, env=None):
        """Start a long-running command whose NDJSON output is observed live."""
        return Watcher(self, *args, env=env)


class Watcher:
    """A long-running CLI command (tail/wait) whose NDJSON output is read live.

    Cases that need two concurrent viewers or a send racing an approval use
    this instead of one-shot `Device.invoke`, which would serialize the
    observation behind command completion.
    """

    def __init__(self, device, *args, env=None):
        self.device = device
        self.values = []
        self._lines = queue.Queue()
        self.stderr = []
        self.process = subprocess.Popen(
            [
                str(ROOT / "target/debug/whalerein"),
                "--json",
                "--yes",
                "--data-dir",
                str(device.data_dir),
                *args,
            ],
            cwd=ROOT,
            env=device.env(**(env or {})),
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
            start_new_session=True,
        )
        self._threads = [
            threading.Thread(target=self._pump_stdout, daemon=True),
            threading.Thread(target=self._pump_stderr, daemon=True),
        ]
        for thread in self._threads:
            thread.start()

    def _pump_stdout(self):
        for line in self.process.stdout or ():
            try:
                value = json.loads(line)
            except ValueError:
                continue
            self.values.append(value)
            self._lines.put(value)
        self._lines.put(None)

    def _pump_stderr(self):
        for line in self.process.stderr or ():
            self.stderr.append(line)

    def next(self, timeout=10):
        try:
            value = self._lines.get(timeout=timeout)
        except queue.Empty:
            raise TimeoutError("CLI watcher produced no event") from None
        if value is None:
            raise AssertionError(
                "CLI watcher exited before an event: " + "".join(self.stderr)[-500:]
            )
        return value

    def wait(self, predicate, timeout=15):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            remaining = max(0.01, deadline - time.monotonic())
            value = self.next(timeout=remaining)
            if predicate(value):
                return value
        raise TimeoutError("Expected CLI watcher event was not observed")

    def wait_exit(self, timeout=15):
        return self.process.wait(timeout=timeout)

    def close(self):
        if self.process.poll() is None:
            try:
                os.killpg(self.process.pid, signal.SIGTERM)
            except ProcessLookupError:
                pass
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                try:
                    os.killpg(self.process.pid, signal.SIGKILL)
                except ProcessLookupError:
                    pass
                self.process.wait(timeout=5)
        for stream in (self.process.stdout, self.process.stderr):
            if stream is not None:
                stream.close()


def approval_card(value):
    """Normalize a FakeDSH approval snapshot to the public CLI card shape.

    The control channel returns the native adapter snapshot: camelCase fields
    with the digest as a serialized byte map. Cases pass cards back to the CLI,
    whose arguments use the public card shape (snake_case, hex digest).
    """
    digest = value.get("digest")
    if isinstance(digest, dict):
        try:
            digest = bytes(
                digest[str(index)] for index in range(len(digest))
            ).hex()
        except (KeyError, TypeError, ValueError):
            digest = None
    return {
        "approval_id": value.get("approvalId", ""),
        "session_id": value.get("sessionId", ""),
        "tool_name": value.get("toolName", ""),
        "call_id": value.get("callId", ""),
        "reason": value.get("reason", ""),
        "digest": digest,
        "version": value.get("version"),
        "expires_at_unix_ms": value.get("expiresAt"),
        "state": value.get("state", ""),
        "decision": value.get("decision", ""),
    }


class FakeDsh:
    def __init__(self, process):
        self.process = process

    def create_session(self):
        session_id = "session-" + uuid.uuid4().hex
        self.process.control("create_session", session_id=session_id)
        return session_id

    def request_approval(self, session_id, ttl_ms=None, reason=None, tool_name=None):
        extra = {}
        if ttl_ms is not None:
            extra["ttl_ms"] = int(ttl_ms)
        if reason is not None:
            extra["reason"] = reason
        if tool_name is not None:
            extra["tool_name"] = tool_name
        return approval_card(
            self.process.control("request_approval", session_id=session_id, **extra)
        )

    def snapshot(self, session_id):
        return self.process.control("snapshot", session_id=session_id)

    def approval(self, approval_id):
        return self.process.control("approval", approval_id=approval_id)

    def start_running(self, session_id):
        return self.process.control("running", session_id=session_id)


class Scenario:
    def __init__(self, directory, schema):
        self.directory = directory
        self.schema = schema
        self.processes = []
        self.admin = uuid.uuid4().hex
        # Case-unique namespace: no broker session, subscription or topic can
        # collide with another case even when a warm broker is shared.
        self.host = "host-" + uuid.uuid4().hex[:16]
        self.access_client_id = "access-" + uuid.uuid4().hex[:16]
        self.env = {
            "WHALEREIN_DATABASE_URL": f"postgres://postgres@127.0.0.1:55432/whalerein_test?sslmode=disable&search_path={schema}",
            "WHALEREIN_ADMIN_TOKEN": self.admin,
            "WHALEREIN_HOST_PROVISIONING_TOKEN": uuid.uuid4().hex,
            "WHALEREIN_BROKER_PASSWORD": BROKER_PASSWORD,
            "WHALEREIN_INTERNAL_TOKEN": INTERNAL_TOKEN,
        }
        self.socket = directory / "dsh.sock"
        self.config = directory / "access.yaml"
        self.config.write_text("""listen: "127.0.0.1:18080"
internal_listen: "127.0.0.1:18081"
public_url: "http://127.0.0.1:18080"
database_url: "${WHALEREIN_DATABASE_URL}"
admin_token: "${WHALEREIN_ADMIN_TOKEN}"
host_provisioning_token: "${WHALEREIN_HOST_PROVISIONING_TOKEN}"
broker:
  url: "mqtt://127.0.0.1:1883"
  public_url: "mqtt://127.0.0.1:1883"
  client_id: "__ACCESS_CLIENT_ID__"
  username: access
  password: "${WHALEREIN_BROKER_PASSWORD}"
line:
  policy_ttl: 10s
  reconcile_interval: 30s
  heartbeat_interval: 5s
  operation_timeout: 5s
presence:
  cache_ttl: 60s
admission:
  max_entries: 10000
  ttl: 5m
  load_timeout: 250ms
  max_background_loads: 64
internal:
  callback_token: "${WHALEREIN_INTERNAL_TOKEN}"
log:
  level: error
""".replace("__ACCESS_CLIENT_ID__", self.access_client_id))

    def configure_tls(self, ca_file, broker_url="mqtts://127.0.0.1:8883"):
        """Point Access, the plugin and future devices at the case TLS listener.

        Idempotent: cases re-run it with a different trust bundle after a
        certificate rotation, so an existing ca_file/server_name pair is
        replaced rather than duplicated (duplicate YAML keys fail validation).
        """
        ca_file = str(ca_file)
        lines = [
            line
            for line in self.config.read_text().splitlines()
            if not line.strip().startswith(("ca_file:", "server_name:"))
        ]
        updated = "\n".join(lines) + "\n"
        updated = updated.replace('url: "mqtt://127.0.0.1:1883"', f'url: "{broker_url}"', 1)
        updated = updated.replace(
            'public_url: "mqtt://127.0.0.1:1883"', f'public_url: "{broker_url}"', 1
        )
        updated = updated.replace(
            "broker:\n", f'broker:\n  ca_file: "{ca_file}"\n  server_name: "localhost"\n', 1
        )
        self.config.write_text(updated)
        self.tls_ca = ca_file
        self.tls_url = broker_url

    def start_dsh(self, extra_env=None):
        authority = Process(
            ["node", str(ROOT / "tests/integration/fixtures/dsh.mjs")],
            self.directory,
            {"WHALEREIN_DSH_SOCKET": str(self.socket), **(extra_env or {})},
            control=True,
        )
        self.processes.append(authority)
        self.dsh = FakeDsh(authority)
        wait(lambda: self.socket.exists(), process=authority)

    def start_access_process(self):
        """Start Access without waiting for readiness.

        The internal auth listener is up in well under a second, while the
        broker starts later. Starting Access first means the plugin's first
        deferred CONNECT callback already finds a live endpoint instead of
        failing admission and retrying.
        """
        self.access = Process(
            ["/usr/local/bin/whalerein-access", "--config", str(self.config)],
            self.directory,
            self.env,
        )
        self.processes.append(self.access)

    def wait_access_ready(self):
        wait(lambda: http("/readyz"), process=self.access)

    def start_access(self):
        self.start_access_process()
        self.wait_access_ready()

    def start_plugin(self, extra_env=None):
        self.plugin = Process(
            ["node", str(ROOT / "tests/integration/fixtures/plugin.mjs")],
            self.directory,
            {
                "WHALEREIN_DSH_SOCKET": str(self.socket),
                "WHALEREIN_MQTT_URL": getattr(self, "tls_url", "mqtt://127.0.0.1:1883"),
                "WHALEREIN_HOST_ID": self.host,
                "WHALEREIN_HOST_TOKEN": self.env["WHALEREIN_HOST_PROVISIONING_TOKEN"],
                "WHALEREIN_STATE_DIR": str(self.directory / "plugin"),
                "WHALEREIN_HOST_CREDENTIAL_FILE": str(
                    self.directory / "credentials.json"
                ),
                **(
                    {
                        "WHALEREIN_CA_FILE": self.tls_ca,
                        "WHALEREIN_ALLOW_INSECURE_MQTT": "0",
                    }
                    if getattr(self, "tls_ca", None)
                    else {}
                ),
                **(extra_env or {}),
            },
            control=False,
        )
        self.processes.append(self.plugin)
        # The fixture marker follows MQTT subscriptions and revocation replay.
        wait(
            lambda: "plugin ready" in Path(self.plugin.output.name).read_text(),
            process=self.plugin,
        )

    def pair(self, namespace="client"):
        device = Device(self.directory, namespace)
        if getattr(self, "tls_ca", None):
            device.extra_env = {"WHALEREIN_CA_FILE": self.tls_ca}
        invitation = http("/v1/pairing/invitations", {"hostId": self.host}, self.admin)
        device.identity = device.call("pair", invitation["pairingUri"])
        return device

    def pair_second(self):
        """Pair a second independent device to the same host identity."""
        return self.pair(namespace="client-2")

    def host_peer_exists(self, device_id):
        import sqlite3
        with sqlite3.connect(self.directory / "plugin/state.sqlite") as connection:
            return connection.execute("SELECT 1 FROM peer_keys WHERE device_id=?", (device_id,)).fetchone() is not None

    def revoke_from_host(self, device):
        device._revoked_token = device.transport_token()
        result = subprocess.run(
            ["node", str(ROOT / "tests/integration/drivers/transport/host-revoke.mjs")],
            cwd=ROOT, env={**os.environ,"WHALEREIN_DSH_SOCKET":str(self.socket),
                          "WHALEREIN_HOST_ID":self.host,"WHALEREIN_DEVICE_ID":device.identity["device_id"]},
            capture_output=True,text=True,timeout=20,check=False)
        if result.returncode != 0 or result.stdout.strip() != "host-revoke-ok":
            raise AssertionError("Signed host revocation failed")

    def replay_command(self, device, changed=False, token_override=None, route_host_override=None, include_conflict_code=False):
        import base64
        import hashlib

        query = f"""SELECT json_build_object(
            'requestId',request_id,'hostId',host_id,'deviceId',device_id,
            'commandId',command_id,'kind',kind,'protocolVersion',protocol_version,
            'channelId',channel_id,'clientTransportVersion',client_transport_version,
            'body',encode(request_body,'base64'),
            'payloadDigest',encode(payload_digest,'base64'))
            FROM {self.schema}.requests WHERE command_id <> '' ORDER BY created_at DESC LIMIT 1"""
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
                query,
            ],
            check=True,
            capture_output=True,
            text=True,
            timeout=5,
        )
        payload = json.loads(result.stdout)
        # PostgreSQL wraps base64 at 76 columns; proto3 JSON requires one line.
        for field in ("body", "payloadDigest"):
            payload[field] = base64.b64encode(base64.b64decode(payload[field])).decode()
        payload["requestId"] = uuid.uuid4().hex
        payload["expiresAtUnixMs"] = str(int((time.time() + 60) * 1000))
        if changed:
            body = base64.b64decode(payload["body"]) + b"changed"
            payload["body"] = base64.b64encode(body).decode()
            payload["payloadDigest"] = base64.b64encode(
                hashlib.sha256(body).digest()
            ).decode()
        token = token_override if token_override is not None else device.transport_token()
        try:
            return http(
                "/v1/hosts/" + (route_host_override or self.host) + "/requests",
                payload,
                token,
                return_status=True,
                include_conflict_code=include_conflict_code,
            )
        except RuntimeError as error:
            if "HTTP 409" in str(error):
                return 409, {}
            raise

    def close(self):
        errors = []
        for process in reversed(self.processes):
            try:
                process.close()
            except Exception as error:  # noqa: BLE001 - release all remaining owned processes
                errors.append(error)
        if errors:
            raise ExceptionGroup("Fixture cleanup failed", errors)


def _connect_policy(fields):
    """Render the whalerein/1 policy used by the transport policy stub."""
    username = fields.get("username", "")
    if username.startswith("host:"):
        host = username[5:]
        if not host or any(character.isspace() for character in host):
            return "whalerein/1\ndeny unauthorized\n"
        entries = [
            ("subscribe", f"whalerein/v1/h/{host}/request"),
            ("read", f"whalerein/v1/h/{host}/request"),
            ("subscribe", f"whalerein/v1/h/{host}/presence"),
            ("read", f"whalerein/v1/h/{host}/presence"),
            ("write", f"whalerein/v1/h/{host}/result"),
            ("write", f"whalerein/v1/h/{host}/event"),
        ]
        lines = [
            "whalerein/1",
            "ok",
            "kind host",
            f"host {host}",
            "revision 1",
            "ready 1",
        ]
        lines.extend(f"acl {access} 1 {host} {filter}" for access, filter in entries)
        return "\n".join(lines) + "\n"
    return "whalerein/1\ndeny unauthorized\n"


def start_connect_policy_stub(port=18081):
    """Serve the plugin's deferred CONNECT callback for broker-only cases.

    The transport Node.js case exercises the broker without an Access process.
    The plugin still requires a policy answer for its synthetic host, so the
    case owns this narrow loopback stub for the duration of its broker.
    """

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            if self.path != "/internal/broker/connect":
                self.send_error(404)
                return
            try:
                length = int(self.headers.get("Content-Length", "0") or "0")
            except ValueError:
                length = 0
            form = urllib.parse.parse_qs(self.rfile.read(length).decode("utf-8"))
            fields = {key: value[0] for key, value in form.items() if value}
            body = _connect_policy(fields).encode("utf-8")
            self.send_response(200)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *_args):
            return

    server = ThreadingHTTPServer(("127.0.0.1", port), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()

    class Stub:
        def close(self):
            server.shutdown()
            server.server_close()
            thread.join(timeout=5)

    return Stub()
