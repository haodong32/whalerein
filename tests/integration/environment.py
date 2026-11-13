"""Concrete container-local process and product helpers."""

import json
import os
import select
import signal
import subprocess
import time
import urllib.error
import urllib.request
import uuid
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]


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
        time.sleep(0.1)
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
    def __init__(self, directory):
        self.directory = directory
        self.identity = None

    def invoke(self, *args, success=True):
        try:
            result = subprocess.run(
                [
                    str(ROOT / "target/debug/whalerein"),
                    "--json",
                    "--yes",
                    "--data-dir",
                    str(self.directory / "client"),
                    *args,
                ],
                cwd=ROOT,
                env={
                    **os.environ,
                    "WHALEREIN_SECRET_BACKEND": "file:"
                    + str(self.directory / "secrets"),
                    "WHALEREIN_LOG": "error",
                },
                capture_output=True,
                text=True,
                timeout=90,
                check=False,
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

        with sqlite3.connect(self.directory / "client" / "cache.sqlite3") as cache:
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
        return (self.directory / "secrets" / key.encode().hex()).read_text()

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


class FakeDsh:
    def __init__(self, process):
        self.process = process

    def create_session(self):
        session_id = "session-" + uuid.uuid4().hex
        self.process.control("create_session", session_id=session_id)
        return session_id

    def request_approval(self, session_id):
        return self.process.control("request_approval", session_id=session_id)

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
            "WHALEREIN_BROKER_PASSWORD": uuid.uuid4().hex,
            "WHALEREIN_INTERNAL_TOKEN": "compose-internal-token",
            "WHALEREIN_EMQX_API_KEY": "compose-api-key",
            "WHALEREIN_EMQX_API_SECRET": "compose-api-secret",
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
  url: "mqtt://127.0.0.1:1884"
  public_url: "mqtt://127.0.0.1:1883"
  client_id: "__ACCESS_CLIENT_ID__"
  username: access
  password: "${WHALEREIN_BROKER_PASSWORD}"
emqx:
  rest_url: "http://127.0.0.1:18083"
  api_key: "${WHALEREIN_EMQX_API_KEY}"
  api_secret: "${WHALEREIN_EMQX_API_SECRET}"
  disconnect_mode: rest
internal:
  callback_token: "${WHALEREIN_INTERNAL_TOKEN}"
log:
  level: error
""".replace("__ACCESS_CLIENT_ID__", self.access_client_id))

    def start_dsh(self):
        authority = Process(
            ["node", str(ROOT / "tests/integration/fixtures/dsh.mjs")],
            self.directory,
            {"WHALEREIN_DSH_SOCKET": str(self.socket)},
            control=True,
        )
        self.processes.append(authority)
        self.dsh = FakeDsh(authority)
        wait(lambda: self.socket.exists(), process=authority)

    def start_access_process(self):
        """Start Access without waiting for readiness.

        The internal auth listener is up in well under a second, while the
        broker starts later. Starting Access first lets EMQX's HTTP
        authenticator initialize against a live endpoint instead of marking
        the resource unhealthy and waiting for its 15 second health check.
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

    def start_plugin(self):
        self.plugin = Process(
            ["node", str(ROOT / "tests/integration/fixtures/plugin.mjs")],
            self.directory,
            {
                "WHALEREIN_DSH_SOCKET": str(self.socket),
                "WHALEREIN_MQTT_URL": "mqtt://127.0.0.1:1883",
                "WHALEREIN_HOST_ID": self.host,
                "WHALEREIN_HOST_TOKEN": self.env["WHALEREIN_HOST_PROVISIONING_TOKEN"],
                "WHALEREIN_STATE_DIR": str(self.directory / "plugin"),
                "WHALEREIN_HOST_CREDENTIAL_FILE": str(
                    self.directory / "credentials.json"
                ),
            },
            control=False,
        )
        self.processes.append(self.plugin)
        # The fixture marker follows MQTT subscriptions and revocation replay.
        wait(
            lambda: "plugin ready" in Path(self.plugin.output.name).read_text(),
            process=self.plugin,
        )

    def pair(self):
        device = Device(self.directory)
        invitation = http("/v1/pairing/invitations", {"hostId": self.host}, self.admin)
        device.identity = device.call("pair", invitation["pairingUri"])
        return device

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
