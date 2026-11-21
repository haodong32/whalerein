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
from contextvars import ContextVar
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]

# Shared broker bootstrap credentials. The plugin authenticates the Access
# identity locally against its password file; the internal token authenticates
# the Access callback the plugin makes for external principals.
BROKER_PASSWORD = uuid.uuid4().hex
INTERNAL_TOKEN = uuid.uuid4().hex


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


def owns_listener(process, port):
    """Tie a loopback health response to the exact fixture process."""
    proc = Path("/proc") / str(process.child.pid)
    address = f"0100007F:{port:04X}"
    listeners = set()
    for line in (proc / "net/tcp").read_text().splitlines()[1:]:
        fields = line.split()
        if fields[1] == address and fields[3] == "0A":
            listeners.add(f"socket:[{fields[9]}]")
    return any(str(descriptor.readlink()) in listeners for descriptor in (proc / "fd").iterdir())


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


def relay_http(path, method="GET", body=None, token=None, headers=None, timeout=10):
    """One raw Access directory request; returns (status, headers, bytes).

    The exact ciphertext routes carry raw bytes, so the generic JSON helper is
    not sufficient. Error statuses are returned rather than raised so a case can
    assert the documented terminal code without parsing product logs.
    """
    request = urllib.request.Request(
        "http://127.0.0.1:18080" + path,
        data=body,
        method=method,
        headers={
            **({"Authorization": "Bearer " + token} if token else {}),
            **(headers or {}),
        },
    )
    try:
        with urllib.request.urlopen(request, timeout=timeout) as response:
            return response.status, dict(response.headers), response.read()
    except urllib.error.HTTPError as error:
        return error.code, dict(error.headers), error.read()


class RelayDirectory:
    """Client for the node-scoped temporary ciphertext directory.

    It uses the Device bearer path so a case can exercise reserve/upload/finish/
    fetch/delete and their stored scope without reading the Host signing key.
    The origin is the same loopback Access origin the product uses in this
    container; production uses HTTPS.
    """

    def __init__(self, origin="http://127.0.0.1:18080"):
        self.origin = origin.rstrip("/")

    def reserve(
        self,
        token,
        *,
        host_id,
        device_id,
        ciphertext,
        session_id="",
        channel_id="channel-fixture",
        purpose="PAYLOAD_PURPOSE_RESULT",
        part_id="",
        expires_at_ms=None,
        epoch=3,
        owner_node=None,
    ):
        import base64
        import hashlib
        import time

        digest = hashlib.sha256(ciphertext).digest()
        expiry = expires_at_ms if expires_at_ms is not None else int((time.time() + 60) * 1000)
        body = json.dumps(
            {
                "relayVersion": epoch,
                "hostId": host_id,
                "deviceId": device_id,
                "sessionId": session_id,
                "channelId": channel_id,
                "purpose": purpose,
                "partId": part_id,
                "ciphertextBytes": str(len(ciphertext)),
                "sha256": base64.b64encode(digest).decode(),
                "expiresAtUnixMs": str(expiry),
            }
        ).encode()
        headers = {"Content-Type": "application/json"}
        if owner_node is not None:
            headers["WhaleRein-Owner-Node"] = owner_node
        status, response_headers, payload = relay_http(
            "/v1/relay/objects", "POST", body, token, headers
        )
        return status, response_headers, payload

    def upload(self, token, reservation, ciphertext, epoch=3, headers=None):
        status, response_headers, payload = relay_http(
            reservation["uploadPath"],
            "PUT",
            ciphertext,
            token,
            {
                "Content-Type": "application/octet-stream",
                "WhaleRein-Relay-Epoch": str(epoch),
                **(headers or {}),
            },
        )
        return status, response_headers, payload

    def finish(self, token, object_id, ciphertext, *, digest=None, declared_bytes=None, epoch=3):
        import base64
        import hashlib

        sha = digest if digest is not None else hashlib.sha256(ciphertext).digest()
        body = json.dumps(
            {
                "relayVersion": epoch,
                "ciphertextBytes": str(
                    len(ciphertext) if declared_bytes is None else declared_bytes
                ),
                "sha256": base64.b64encode(sha).decode(),
            }
        ).encode()
        return relay_http(
            f"/v1/relay/objects/{object_id}/finish",
            "POST",
            body,
            token,
            {"Content-Type": "application/json"},
        )

    def fetch(self, token, object_id, epoch=3):
        return relay_http(
            f"/v1/relay/objects/{object_id}",
            "GET",
            None,
            token,
            {"WhaleRein-Relay-Epoch": str(epoch)},
        )

    def delete(self, token, object_id, epoch=3):
        return relay_http(
            f"/v1/relay/objects/{object_id}",
            "DELETE",
            None,
            token,
            {"WhaleRein-Relay-Epoch": str(epoch)},
        )

    def store(self, token, *, ciphertext, purpose="PAYLOAD_PURPOSE_RESULT", **scope):
        """Reserve, upload and finish one complete object; returns reference dict."""
        status, _, payload = self.reserve(
            token, ciphertext=ciphertext, purpose=purpose, **scope
        )
        assert status == 200, payload
        reservation = json.loads(payload)
        status, _, payload = self.upload(token, reservation, ciphertext)
        assert status == 204, payload
        status, _, payload = self.finish(token, reservation["objectId"], ciphertext)
        assert status == 200, payload
        return json.loads(payload)


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


device_keyring = ContextVar("device_keyring", default=None)


class Device:
    """One selected Standalone configuration in a private test keyring."""

    def __init__(self, directory, namespace="client", *, ca_certificate=None):
        if not namespace or Path(namespace).name != namespace or namespace in {".", ".."}:
            raise ValueError("Device fixture namespace must name one directory")
        self.directory = directory
        self.namespace = namespace
        self.config_path = directory / f"{namespace}.toml"
        self.identity = None
        self.extra_env = {}
        self._keyring = device_keyring.get()
        directory.mkdir(parents=True, exist_ok=True)
        if not self.config_path.exists():
            self.configure_tls(ca_certificate)

    @property
    def instance_dir(self):
        return self.directory / self.namespace

    @property
    def data_dir(self):
        """The runtime state directory below the instance manifest."""
        return self.instance_dir / "state"

    def configure_tls(self, ca_certificate):
        """Write the selected fixture TOML with optional Standalone CA trust."""
        data = (
            'version = 1\nservice = "standalone"\n'
            f'data_dir = {json.dumps(self.namespace, ensure_ascii=False)}\n'
        )
        if ca_certificate is not None:
            data += f'standalone_ca_certificate = {json.dumps(str(ca_certificate), ensure_ascii=False)}\n'
        self.config_path.write_text(data, encoding="utf-8")

    def args(self, *commands):
        return [
            str(ROOT / "target/debug/whalerein"),
            "--config", str(self.config_path),
            "--json",
            *commands,
        ]

    def keyring_env(self):
        if self._keyring is None:
            raise RuntimeError("Device requires a private integration keyring")
        return {**os.environ, **self._keyring()}

    def env(self, **overrides):
        return {**self.keyring_env(), **self.extra_env, **overrides}

    def has_secret(self, role):
        from client_environment import fixture_has_secret

        return fixture_has_secret(self, role)

    def invoke(self, *commands, success=True, stdin_text=None, env=None):
        streams = {"stdin": subprocess.DEVNULL} if stdin_text is None else {"input": stdin_text}
        try:
            result = subprocess.run(
                self.args(*commands),
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
        values = [
            json.loads(line) for line in result.stdout.splitlines() if line.strip()
        ]
        assert values and all(v.get("schema") == "whalerein/client/v1" for v in values), (
            "Missing selected-client CLI envelope"
        )
        if success and result.returncode != 0:
            code = values[-1].get("error", {}).get("code", "unknown")
            raise AssertionError(f"CLI {commands[:2]} failed with exit {result.returncode} ({code})")
        return result.returncode, values

    def call(self, *commands, **kwargs):
        return self.invoke(*commands, **kwargs)[1][-1].get("data", {})

    def pair(self, invitation, *options):
        self.identity = self.call(
            "host", "connect", "--invite-stdin", "--yes", *options, stdin_text=invitation
        )
        return self.identity

    def hosts(self):
        return self.call("host", "list")["hosts"]

    def sessions(self, host):
        return self.call("session", "list", "--host", host)["sessions"]

    def send(self, session, message, delivery=None):
        options = ["--delivery", delivery] if delivery else []
        return self.call("session", "send", session, "--detach", *options, "--", message)

    def send_confirmed(self, session, message, delivery=None):
        """Wait for a terminal acceptance from the original detached send."""
        submission = self.send(session, message, delivery=delivery)
        return self.confirm_detached_delivery(submission, session, message, delivery)

    def settle_detached_delivery(self, code, envelope, session, text, delivery):
        """Read the exact recorded send's terminal effect without waiting for a reply."""
        if code != 0:
            return code, envelope
        submission = envelope["data"]
        operation = submission["operation_id"]
        target = submission["target"]
        assert isinstance(operation, str) and operation.strip() == operation and operation
        deadline = time.monotonic() + 20
        delivery_args = () if delivery is None else ("--delivery", delivery)
        while True:
            submission = envelope["data"]
            assert submission["state"] == "submission"
            assert submission["operation_id"] == operation and submission["target"] == target
            if submission["receipt"]["status"] not in {"received", "host_accepted"}:
                return code, envelope
            remaining = deadline - time.monotonic()
            if remaining < 1:
                raise TimeoutError("Detached delivery did not reach a terminal receipt")
            time.sleep(0.1)
            # Core resolves a saved operation ID and checks the original
            # intent before reading its receipt. Detach avoids waiting for
            # a model reply while the case holds the active run.
            code, envelopes = self.invoke(
                "session", "send", session, "--host", target["host_id"],
                "--detach", *delivery_args, "--operation-id", operation,
                "--timeout", str(int(remaining)),
                "--", text, success=False,
            )
            envelope = envelopes[-1]
            if code != 0:
                return code, envelope

    def confirm_detached_delivery(self, submission, session, text, delivery):
        code, envelope = self.settle_detached_delivery(
            0, {"data": submission}, session, text, delivery
        )
        assert code == 0, "Delivery did not succeed"
        receipt = envelope["data"]["receipt"]
        identity = receipt.get("delivery_id")
        assert receipt["status"] == "succeeded"
        assert isinstance(identity, str) and identity and identity.strip() == identity
        return receipt

    def approvals(self, session):
        return self.call("session", "approval", "list", "--session", session)["approvals"]

    def resolve(self, decision, approval):
        return self.call(
            "session", "approval", decision,
            "--request",
            approval["approval_id"],
            "--digest",
            approval["digest"],
            "--version",
            str(approval["version"]),
        )

    def interrupt(self, session):
        return self.call("session", "interrupt", session)

    def invoke_captured(self, *commands, success=True, stdin_text=None, env=None):
        """Run one CLI command and keep stdout/stderr separate.

        Attachment downloads put progress on stderr and exactly one result
        envelope on stdout; the ordinary invoke helper only reads stdout.
        """
        streams = {"stdin": subprocess.DEVNULL} if stdin_text is None else {"input": stdin_text}
        try:
            result = subprocess.run(
                self.args(*commands),
                cwd=ROOT,
                env=self.env(**(env or {})),
                capture_output=True,
                text=True,
                timeout=120,
                check=False,
                **streams,
            )
        except subprocess.TimeoutExpired:
            raise TimeoutError("CLI operation timed out") from None
        values = [
            json.loads(line) for line in result.stdout.splitlines() if line.strip()
        ]
        assert values and all(v.get("schema") == "whalerein/client/v1" for v in values), (
            "Missing selected-client CLI envelope"
        )
        if success and result.returncode != 0:
            code = values[-1].get("error", {}).get("code", "unknown")
            raise AssertionError(f"CLI {commands[:2]} failed with exit {result.returncode} ({code})")
        return result.returncode, values, result.stderr

    def history(self, session, *, cache=False, message=None):
        args = ["session", "history", session]
        if cache:
            args.append("--cache")
        if message is not None:
            args.extend(["--message", message])
        return self.call(*args)

    def download(self, reference, destination, session, *, host=None, force=False,
                 name=None, timeout=None, success=True):
        args = ["session", "download", reference, "--to", str(destination),
                "--session", session]
        if host is not None:
            args.extend(["--host", host])
        if force:
            args.append("--force")
        if name is not None:
            args.extend(["--name", name])
        if timeout is not None:
            args.extend(["--timeout", str(timeout)])
        return self.invoke_captured(*args, success=success)

    def observe_history(self, session, host=None):
        """Wait for the selected watch to finish syncing its committed baseline."""
        scope = ["--host", host] if host is not None else []
        watcher = self.start_watch("session", "watch", session, "--timeout", "20", *scope)
        try:
            def is_scoped_state(value):
                event = value.get("data", {}).get("event", {})
                target = event.get("session", {})
                return (
                    event.get("type") == "state"
                    and target.get("session_id") == session
                    and bool(target.get("host_id"))
                    and (host is None or target["host_id"] == host)
                )

            return watcher.wait(is_scoped_state, timeout=20)["data"]["event"]
        finally:
            watcher.close()

    def cached_history_state(self, host, session):
        """Read committed identity/cursor metadata without decrypting payloads."""
        import sqlite3
        from contextlib import closing

        path = self.data_dir / "cache.sqlite3"
        if not path.is_file():
            return None, ()
        with closing(sqlite3.connect(f"{path.absolute().as_uri()}?mode=ro", uri=True)) as cache:
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
        return self.call("device", "revoke", "--yes")

    def cli_access_denied(self):
        exit_code, values = self.invoke("host", "list", success=False)
        return exit_code == 4 and values[-1].get("error", {}).get("code") in {
            "revoked", "not_paired", "unauthorized", "forbidden",
        }

    def events(self, session):
        _, values = self.invoke("session", "watch", session, "--timeout", "3")
        return [
            value["data"]["event"]
            for value in values
            if value.get("data", {}).get("event")
        ]

    def send_exit(self, session):
        return self.invoke(
            "session", "send", session, "fixture message", "--detach", success=False
        )[0]

    def sessions_exit(self, host):
        return self.invoke("session", "list", "--host", host, success=False)[0]

    def start_watch(self, *commands, env=None):
        """Start a command whose NDJSON output is observed live."""
        return Watcher(self.args(*commands), self.env(**(env or {})))


class Watcher:
    """A long-running CLI command (tail/wait) whose NDJSON output is read live.

    Cases that need two concurrent viewers or a send racing an approval use
    this instead of one-shot `Device.invoke`, which would serialize the
    observation behind command completion.
    """

    def __init__(self, argv, env):
        self.values = []
        self._lines = queue.Queue()
        self.stderr = []
        self.process = subprocess.Popen(
            argv,
            cwd=ROOT,
            env=env,
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
        code = self.process.wait(timeout=timeout)
        for reader in self._threads:
            reader.join(timeout=1)
        return code

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

    def save_attachment(self, *, kind="file", size=0, seed=0, media_type="", name=""):
        """Store deterministic bytes in the authority and return the native ref."""
        return self.process.control(
            "save_attachment",
            attachment_kind=kind,
            size=int(size),
            seed=int(seed),
            media_type=media_type,
            name=name,
        )

    def commit_attachment(self, session_id, attachment, *, kind="file", role="user", text=""):
        """Emit one committed message carrying the occurrence."""
        return self.process.control(
            "commit_attachment",
            session_id=session_id,
            attachment=attachment,
            attachment_kind=kind,
            role=role,
            text=text,
        )

    def attachment_pull_state(self):
        return self.process.control("attachment_pull_state")

    def reset_attachment_stats(self):
        return self.process.control("reset_attachment_stats")

    def set_attachment_pull_hold(self, hold=True):
        return self.process.control("set_attachment_pull_hold", hold=hold)

    def release_attachment_pull(self):
        return self.process.control("release_attachment_pull")


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
        # Test-only Host attachment policy overrides, injected before start.
        self.attachment_policy = None
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

    def configure_relay_directory(self, **limits):
        """Enable the node-scoped ciphertext directory for one relay case.

        The storage root stays inside the case directory. Limits default to a
        small, fast directory; a case can lower one bound through the marker
        keyword without changing product defaults.
        """
        import json as _json

        root = self.directory / "payloads"
        root.mkdir(parents=True, exist_ok=True)
        values = {
            "max_object_bytes": limits.get("max_object_bytes", 20 << 20),
            "max_host_bytes": limits.get("max_host_bytes", 64 << 20),
            "max_node_bytes": limits.get("max_node_bytes", 128 << 20),
            "max_objects": limits.get("max_objects", 8),
            "max_active_uploads": limits.get("max_active_uploads", 4),
            "free_space_reserve_bytes": limits.get("free_space_reserve_bytes", 1 << 20),
            "retention": limits.get("retention", "10m"),
            "cleanup_interval": limits.get("cleanup_interval", "2s"),
            "cleanup_batch": limits.get("cleanup_batch", 16),
            "upload_timeout": limits.get("upload_timeout", "10s"),
            "read_timeout": limits.get("read_timeout", "10s"),
        }
        # A production-shaped absolute root is required; values are YAML scalars.
        self.config.write_text(
            self.config.read_text().rstrip("\n")
            + "\nstorage:\n  enabled: true\n"
            + f'  root_dir: {_json.dumps(str(root))}\n'
            + '  node_id: "node-integration"\n'
            + '  owner_id: "node-integration"\n'
            + "".join(f"  {key}: {value}\n" for key, value in values.items())
        )
        self.access_url = "http://127.0.0.1:18080"
        self.relay_directory = RelayDirectory()

    def configure_access_transport(self, profile):
        """Select one packaged transport profile for the Access hop."""
        assert profile in {"default", "compact", "roomy"}
        text = self.config.read_text().rstrip("\n")
        assert "\ntransport:\n" not in text
        self.config.write_text(text + f"\ntransport:\n  profile: {profile}\n")

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
            {"WHALEREIN_DSH_SOCKET": str(self.socket), "WHALEREIN_HOST_ID": self.host, **(extra_env or {})},
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
            ["/workspace/.verification/bin/whalerein-access", "--config", str(self.config)],
            self.directory,
            self.env,
        )
        self.processes.append(self.access)

    def wait_access_ready(self, *, listener_port=18080):
        binary = ROOT / ".verification/bin/whalerein-access"
        executable = Path(f"/proc/{self.access.child.pid}/exe")
        wait(
            lambda: executable.samefile(binary)
            and owns_listener(self.access, listener_port)
            and http("/readyz"),
            process=self.access,
        )

    def start_access(self, *, listener_port=18080):
        self.start_access_process()
        self.wait_access_ready(listener_port=listener_port)

    def start_plugin(self, extra_env=None):
        policy_env = {}
        policy = self.attachment_policy or {}
        if policy.get("total") is not None:
            policy_env["WHALEREIN_ATTACHMENT_MAX_TOTAL_BYTES"] = str(int(policy["total"]))
        if policy.get("chunk") is not None:
            policy_env["WHALEREIN_ATTACHMENT_MAX_CHUNK_BYTES"] = str(int(policy["chunk"]))
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
                "WHALEREIN_LOG_LEVEL": getattr(self, "plugin_log_level", "warn"),
                **(
                    {"WHALEREIN_ACCESS_URL": self.access_url}
                    if getattr(self, "access_url", "")
                    else {}
                ),
                **policy_env,
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
        device = Device(self.directory, namespace, ca_certificate=getattr(self, "tls_ca", None))
        invitation = http("/v1/pairing/invitations", {"hostId": self.host}, self.admin)
        device.pair(invitation["pairingUri"])
        return device

    def pair_second(self):
        """Pair a second independent device to the same host identity."""
        return self.pair(namespace="client-2")

    def host_peer_exists(self, device_id):
        import sqlite3
        with sqlite3.connect(self.directory / "plugin/state.sqlite") as connection:
            return connection.execute("SELECT 1 FROM peer_keys WHERE device_id=?", (device_id,)).fetchone() is not None

    def revoke_from_host(self, device):
        result = subprocess.run(
            ["node", str(ROOT / "tests/integration/drivers/transport/host-revoke.mjs")],
            cwd=ROOT, env={**os.environ,"WHALEREIN_DSH_SOCKET":str(self.socket),
                          "WHALEREIN_HOST_ID":self.host,"WHALEREIN_DEVICE_ID":device.identity["device_id"]},
            capture_output=True,text=True,timeout=20,check=False)
        if result.returncode != 0 or result.stdout.strip() != "host-revoke-ok":
            raise AssertionError("Signed host revocation failed")

    def fixture_bearer(self, device):
        """Issue a synthetic HTTP credential in this case's isolated database."""
        import hashlib
        import re
        from contextlib import contextmanager

        device_id = device.identity["device_id"]
        if not re.fullmatch(r"case_[0-9a-f]{32}", self.schema):
            raise ValueError("Invalid fixture schema")
        if not re.fullmatch(r"[A-Za-z0-9_-]+", device_id):
            raise ValueError("Invalid fixture Device identity")

        def sql(statement):
            subprocess.run(
                ["psql", "-h", "127.0.0.1", "-p", "55432", "-U", "postgres",
                 "-d", "whalerein_test", "-v", "ON_ERROR_STOP=1", "-c", statement],
                check=True, capture_output=True, timeout=5,
            )

        @contextmanager
        def credential():
            token = uuid.uuid4().hex + uuid.uuid4().hex
            digest = hashlib.sha256(token.encode()).hexdigest()
            table = self.schema + ".device_credentials"
            sql(f"INSERT INTO {table}(auth_hash,device_id) VALUES (decode('{digest}','hex'),'{device_id}')")
            try:
                yield token
            finally:
                sql(f"DELETE FROM {table} WHERE auth_hash = decode('{digest}','hex')")

        return credential()

    def replay_command(self, device, changed=False, token_override=None, route_host_override=None, include_conflict_code=False):
        import base64
        import hashlib

        query = f"""SELECT json_build_object(
            'requestId',request_id,'hostId',host_id,'deviceId',device_id,
            'commandId',command_id,'kind',kind,'protocolVersion',protocol_version,
            'channelId',channel_id,'relayVersion',relay_version,
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
        from contextlib import nullcontext

        credential = (
            nullcontext(token_override) if token_override is not None else self.fixture_bearer(device)
        )
        with credential as token:
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
