#!/usr/bin/env python3
"""Disposable loopback WhaleRein stack for the iOS Simulator Integration plan.

The stack runs the real components over loopback on one macOS host:

- WhaleRein Access built from this worktree (Go, stock server).
- PostgreSQL 18 in an isolated disposable Docker container.
- Mosquitto 2.1.2 with the repository broker plugin in a disposable Docker
  container (the pinned `whalerein-mosquitto:local` image).
- The repository FakeDSH fixture and the real Host plugin runtime (Node.js).

Commands:

    ios_stack.py up            start the stack and print the JSON contract
    ios_stack.py status        report readiness without secrets
    ios_stack.py invitation    print the current pairing invitation
    ios_stack.py seed-event    drive the fixture (output, stream, approval, ...)
    ios_stack.py restart       restart Access or the broker for recovery checks
    ios_stack.py down          stop everything and verify cleanup

Only Python's standard library is used. Runtime state lives under
`.data/ios-stack/` and is Git-ignored. Secrets are written with mode 0600 and
never logged.
"""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import hmac
import json
import os
import queue
import shlex
import shutil
import signal
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
DATA_DIR = ROOT / ".data" / "ios-stack"
BIN_DIR = DATA_DIR / "bin"
CURRENT_DIR = DATA_DIR / "current"
RUN_MARKER_ARG = "--ios-stack-run"

ACCESS_BIN = BIN_DIR / "whalerein-access"
ACCESS_SOURCE = "access/cmd/server"

POSTGRES_IMAGE = (
    "postgres:18.7-alpine3.24@sha256:"
    "75dcac3dfb1cd01ca3536910bbc84128759362f8b118bf8c55b38a94f68f3b73"
)
MOSQUITTO_IMAGE = os.environ.get("WHALEREIN_IOS_BROKER_IMAGE", "whalerein-mosquitto:local")
MOSQUITTO_DOCKERFILE = ROOT / "deploy" / "docker" / "mosquitto.Dockerfile"

WORKTREE_ID = hashlib.sha256(os.fsencode(ROOT)).hexdigest()[:12]
POSTGRES_CONTAINER = f"whalerein-ios-stack-{WORKTREE_ID}-pg"
MOSQUITTO_CONTAINER = f"whalerein-ios-stack-{WORKTREE_ID}-mqtt"
LABEL_STACK = "com.whalerein.ios-stack"
LABEL_RUN = "com.whalerein.ios-stack.run"
LABEL_WORKTREE = "com.whalerein.ios-stack.worktree"

SCHEMA = "whalerein/ios-stack/v1"
DATABASE_NAME = "whalerein_ios"
FIXTURE_SESSION_ID = "session-ios-1"
INITIAL_TRANSCRIPT = ("fixture output", "fixture output")
STREAM_PREVIEW_TEXT = "x"
STREAM_FINAL_TEXT = "xy"

DEFAULT_UP_TIMEOUT = 300.0
READY_TIMEOUT = 90.0
ACCESS_READY_TIMEOUT = 60.0
PLUGIN_READY_TIMEOUT = 45.0
CONTROL_TIMEOUT = 15.0


class StackError(Exception):
    """A bounded, user-facing stack failure."""

    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code
        self.message = message


def log_line(directory: Path, message: str) -> None:
    """Append one timestamped line; callers never pass secrets here."""
    try:
        path = directory / "supervisor.log"
        fd = os.open(path, os.O_CREAT | os.O_APPEND | os.O_WRONLY, 0o600)
        with os.fdopen(fd, "a", encoding="utf-8") as stream:
            stream.write(f"{time.strftime('%Y-%m-%dT%H:%M:%S%z')} {message}\n")
    except OSError:
        pass


def atomic_write(path: Path, text: str, mode: int = 0o600) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(path.name + ".tmp")
    fd = os.open(temporary, os.O_CREAT | os.O_TRUNC | os.O_WRONLY, mode)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        stream.write(text)
    os.replace(temporary, path)
    os.chmod(path, mode)


def atomic_write_json(path: Path, value, mode: int = 0o600) -> None:
    atomic_write(path, json.dumps(value, indent=2, sort_keys=True) + "\n", mode)


def read_json(path: Path):
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (OSError, ValueError):
        return None


def free_port() -> int:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def port_open(port: int, timeout: float = 0.5) -> bool:
    try:
        with socket.create_connection(("127.0.0.1", port), timeout=timeout):
            return True
    except OSError:
        return False


def wait_until(
    description: str,
    predicate,
    timeout: float,
    *,
    child=None,
    interval: float = 0.05,
) -> None:
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if child is not None and child.poll() is not None:
            raise StackError(
                "process_exited",
                f"{description}: process exited with status {child.returncode}",
            )
        try:
            if predicate():
                return
        except (OSError, ValueError, StackError):
            pass
        time.sleep(interval)
    raise StackError("timeout", f"timed out after {int(timeout)}s waiting for {description}")


def run(
    argv: list[str],
    *,
    cwd: Path | None = None,
    env: dict[str, str] | None = None,
    timeout: float = 60.0,
    check: bool = True,
) -> subprocess.CompletedProcess:
    try:
        completed = subprocess.run(
            argv,
            cwd=str(cwd or ROOT),
            env=env,
            capture_output=True,
            text=True,
            timeout=timeout,
            check=False,
        )
    except FileNotFoundError as error:
        raise StackError("missing_command", f"{argv[0]} was not found") from error
    except subprocess.TimeoutExpired as error:
        raise StackError("timeout", f"{argv[0]} timed out after {int(timeout)}s") from error
    if check and completed.returncode != 0:
        detail = (completed.stderr or completed.stdout or "").strip()[-2000:]
        raise StackError(
            "command_failed",
            f"{Path(argv[0]).name} exited {completed.returncode}: {detail}",
        )
    return completed


def docker(*args: str, timeout: float = 60.0, check: bool = True) -> subprocess.CompletedProcess:
    return run(["docker", *args], timeout=timeout, check=check)


def container_info(name: str):
    completed = docker(
        "container", "inspect", name, "--format", "{{.Id}}|{{.Image}}|{{json .State}}|{{json .Config.Labels}}",
        check=False,
    )
    if completed.returncode != 0:
        listing = docker(
            "ps", "-aq", "--no-trunc", "--filter", f"name=^/{name}$", check=False,
        )
        if listing.returncode == 0 and not listing.stdout.strip():
            return None
        raise StackError("container_unverified", f"Cannot verify container {name}")
    try:
        identity, image, state_text, labels_text = completed.stdout.strip().split("|", 3)
        state = json.loads(state_text)
        labels = json.loads(labels_text)
    except ValueError as error:
        raise StackError("container_unverified", f"Cannot verify container {name}") from error
    return {"id": identity, "image": image, "state": state, "labels": labels or {}}


def remove_owned_container(name: str, run_id: str | None) -> bool:
    """Remove the inspected container ID only when it belongs to this run."""
    info = container_info(name)
    if info is None:
        return False
    if (
        not run_id
        or info["labels"].get(LABEL_STACK) != "1"
        or info["labels"].get(LABEL_WORKTREE) != WORKTREE_ID
        or info["labels"].get(LABEL_RUN) != run_id
    ):
        raise StackError(
            "container_conflict",
            f"Docker container {name} does not belong to this run",
        )
    docker("rm", "-f", info["id"], timeout=60, check=False)
    if container_info(name) is not None:
        raise StackError("container_unverified", f"Cannot verify removal of {name}")
    return True


def ensure_image(image: str, build_args: list[str] | None = None, timeout: float = 1800.0) -> None:
    if docker("image", "inspect", image, check=False).returncode == 0:
        return
    if build_args is None:
        docker("pull", image, timeout=timeout)
        return
    docker("build", *build_args, timeout=timeout)


class Child:
    """One owned process in its own process group."""

    def __init__(
        self,
        name: str,
        argv: list[str],
        *,
        env: dict[str, str],
        log_path: Path,
        control: bool = False,
    ):
        self.name = name
        self.argv = argv
        self.control = control
        self.log_path = log_path
        self._log = None
        self._lines: queue.Queue | None = None
        materialized = [str(value) for value in argv]
        materialized[0] = shutil.which(materialized[0]) or materialized[0]
        self.argv = materialized
        self._log = os.fdopen(
            os.open(self.log_path, os.O_CREAT | os.O_APPEND | os.O_WRONLY, 0o600),
            "a",
            encoding="utf-8",
        )
        stdout = subprocess.PIPE if control else self._log
        self.process = subprocess.Popen(
            self.argv,
            cwd=str(ROOT),
            env=env,
            stdin=subprocess.PIPE if control else subprocess.DEVNULL,
            stdout=stdout,
            stderr=self._log,
            text=True,
            start_new_session=True,
            bufsize=1,
        )
        if control:
            self._lines = queue.Queue()
            self._reader = threading.Thread(target=self._read_lines, daemon=True)
            self._reader.start()

    def _read_lines(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            stripped = line.strip()
            if not stripped:
                continue
            try:
                value = json.loads(stripped)
            except ValueError:
                self._lines.put({"error": "non-json fixture output"})
                continue
            self._lines.put(value)

    def alive(self) -> bool:
        return self.process.poll() is None

    def call(self, method: str, timeout: float = CONTROL_TIMEOUT, **fields):
        if not self.control or self._lines is None or self.process.stdin is None:
            raise StackError("internal", f"{self.name} is not a control process")
        if not self.alive():
            raise StackError("process_exited", f"{self.name} exited before control {method}")
        identity = uuid.uuid4().hex
        payload = json.dumps({"id": identity, "method": method, **fields}) + "\n"
        self.process.stdin.write(payload)
        self.process.stdin.flush()
        deadline = time.monotonic() + timeout
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise StackError("timeout", f"{self.name} control {method} timed out")
            try:
                value = self._lines.get(timeout=remaining)
            except queue.Empty as error:
                raise StackError("timeout", f"{self.name} control {method} timed out") from error
            if "error" in value:
                raise StackError(
                    "fixture_control_failed",
                    f"{self.name} control {method} failed: {value.get('error')}",
                )
            if value.get("id") != identity:
                continue
            return value.get("value")

    def close(self, grace: float = 8.0) -> None:
        try:
            if self.process.poll() is None:
                try:
                    os.killpg(self.process.pid, signal.SIGTERM)
                except ProcessLookupError:
                    pass
                try:
                    self.process.wait(timeout=grace)
                except subprocess.TimeoutExpired:
                    try:
                        os.killpg(self.process.pid, signal.SIGKILL)
                    except ProcessLookupError:
                        pass
                    try:
                        self.process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        pass
        finally:
            if self.process.stdin is not None:
                self.process.stdin.close()
            if self.process.stdout is not None:
                self.process.stdout.close()
            if self._log is not None:
                self._log.close()

    def tail(self, limit: int = 1200) -> str:
        try:
            return self.log_path.read_text(encoding="utf-8", errors="replace").strip()[-limit:]
        except OSError:
            return ""


class Supervisor:
    """Owns one stack: one Access, one fixture Host, two containers."""

    def __init__(self, directory: Path):
        self.directory = directory
        self.run_id = uuid.uuid4().hex
        self.secrets: dict[str, str] = {}
        self.ports: dict[str, int] = {}
        self.control_port = 0
        self.http_server: ThreadingHTTPServer | None = None
        self.host_id = f"host-ios-stack-{self.run_id[:10]}"
        self.expected: list[str] = list(INITIAL_TRANSCRIPT)
        self.last_approval: dict | None = None
        self.access: Child | None = None
        self.dsh: Child | None = None
        self.plugin: Child | None = None
        self.ready = False
        self.stopping = threading.Event()
        self.lock = threading.RLock()
        self.startup_error: str | None = None
        self.state: dict | None = None

    # ---- configuration and secrets -------------------------------------

    def _secret_path(self, name: str) -> Path:
        return self.directory / "secrets" / f"{name}.txt"

    def _load_or_create_secrets(self) -> None:
        directory = self.directory / "secrets"
        directory.mkdir(parents=True, exist_ok=True)
        os.chmod(directory, 0o700)
        for name in (
            "admin-token",
            "host-token",
            "broker-password",
            "internal-token",
            "control-token",
        ):
            path = self._secret_path(name)
            if not path.exists():
                atomic_write(path, uuid.uuid4().hex + uuid.uuid4().hex)
            os.chmod(path, 0o600)
            self.secrets[name] = path.read_text(encoding="utf-8").strip()

    def _access_config(self) -> Path:
        path = self.directory / "access.yaml"
        text = f"""mode: standalone
listen: "127.0.0.1:{self.ports['access']}"
internal_listen: "127.0.0.1:{self.ports['internal']}"
public_url: "http://127.0.0.1:{self.ports['access']}"
database_url: "${{WHALEREIN_DATABASE_URL}}"
admin_token: "${{WHALEREIN_ADMIN_TOKEN}}"
host_provisioning_token: "${{WHALEREIN_HOST_PROVISIONING_TOKEN}}"
broker:
  url: "mqtt://127.0.0.1:{self.ports['mqtt']}"
  public_url: "mqtt://127.0.0.1:{self.ports['mqtt']}"
  client_id: "whalerein-ios-access-{self.run_id[:8]}"
  username: access
  password: "${{WHALEREIN_BROKER_PASSWORD}}"
line:
  policy_ttl: 10s
  reconcile_interval: 30s
  heartbeat_interval: 5s
  operation_timeout: 5s
admission:
  max_entries: 10000
  ttl: 5m
  load_timeout: 250ms
  max_background_loads: 64
internal:
  callback_token: "${{WHALEREIN_INTERNAL_TOKEN}}"
pairing:
  invitation_ttl: 30m
  claim_timeout: 15s
  claim_ttl: 5m
  max_failures: 5
log:
  level: error
"""
        atomic_write(path, text, 0o600)
        return path

    def _mosquitto_config(self) -> Path:
        path = self.directory / "mosquitto.conf"
        text = f"""# Generated per-run by tools/ios-stack. Mirrors deploy/mosquitto/mosquitto.conf.
allow_anonymous false
persistence false
max_packet_size 1048576
sys_interval 0

listener 1883 0.0.0.0
protocol mqtt
set_tcp_nodelay true
max_connections -1

log_dest stdout
log_type error
log_type warning
log_type notice
connection_messages false

plugin /usr/lib/mosquitto/whalerein.so
plugin_opt_access_username access
plugin_opt_access_password_file /run/whalerein/mqtt-access-password
plugin_opt_access_auth_url http://host.docker.internal:{self.ports['internal']}/internal/broker/connect
plugin_opt_access_auth_token_file /run/whalerein/internal-token
plugin_opt_auth_timeout_ms 500
plugin_opt_auth_workers 8
plugin_opt_policy_ttl_ms 15000
"""
        atomic_write(path, text, 0o644)
        return path

    # ---- container lifecycle -------------------------------------------

    def _ensure_no_foreign_container(self, name: str) -> None:
        info = container_info(name)
        if info is not None:
            remove_owned_container(name, self.run_id)

    def start_postgres(self) -> None:
        self._ensure_no_foreign_container(POSTGRES_CONTAINER)
        docker(
            "run",
            "-d",
            "--name",
            POSTGRES_CONTAINER,
            "--label",
            f"{LABEL_STACK}=1",
            "--label",
            f"{LABEL_WORKTREE}={WORKTREE_ID}",
            "--label",
            f"{LABEL_RUN}={self.run_id}",
            "-e",
            "POSTGRES_USER=postgres",
            "-e",
            f"POSTGRES_DB={DATABASE_NAME}",
            "-e",
            "POSTGRES_HOST_AUTH_METHOD=trust",
            "-p",
            f"127.0.0.1:{self.ports['postgres']}:5432",
            POSTGRES_IMAGE,
            timeout=120,
        )
        log_line(self.directory, "postgres container started")

        def database_ready() -> bool:
            completed = docker(
                "exec",
                POSTGRES_CONTAINER,
                "psql",
                "-U",
                "postgres",
                "-d",
                DATABASE_NAME,
                "-tAc",
                "select 1",
                timeout=10,
                check=False,
            )
            return completed.returncode == 0 and completed.stdout.strip() == "1"

        try:
            wait_until("PostgreSQL readiness", database_ready, READY_TIMEOUT)
        except StackError:
            logs = docker("logs", "--tail", "40", POSTGRES_CONTAINER, check=False).stdout
            raise StackError("postgres_failed", f"PostgreSQL did not become ready: {logs.strip()}")
        log_line(self.directory, "postgres ready")

    def start_broker(self) -> None:
        self._ensure_no_foreign_container(MOSQUITTO_CONTAINER)
        config = self._mosquitto_config()
        environment = {
            **os.environ,
            "WHALEREIN_BROKER_PASSWORD": self.secrets["broker-password"],
            "WHALEREIN_INTERNAL_TOKEN": self.secrets["internal-token"],
        }
        run(
            [
                "docker",
                "run",
                "-d",
                "--name",
                MOSQUITTO_CONTAINER,
                "--label",
                f"{LABEL_STACK}=1",
                "--label",
                f"{LABEL_WORKTREE}={WORKTREE_ID}",
                "--label",
                f"{LABEL_RUN}={self.run_id}",
                "--add-host",
                "host.docker.internal:host-gateway",
                "-e",
                "WHALEREIN_BROKER_PASSWORD",
                "-e",
                "WHALEREIN_INTERNAL_TOKEN",
                "-p",
                f"127.0.0.1:{self.ports['mqtt']}:1883",
                "-v",
                f"{config}:/mosquitto/config/mosquitto.conf:ro",
                MOSQUITTO_IMAGE,
            ],
            env=environment,
            timeout=120,
        )
        log_line(self.directory, "mosquitto container started")
        try:
            wait_until(
                "Mosquitto readiness",
                lambda: port_open(self.ports["mqtt"]),
                READY_TIMEOUT,
            )
        except StackError:
            logs = docker("logs", "--tail", "40", MOSQUITTO_CONTAINER, check=False).stdout
            raise StackError("broker_failed", f"Mosquitto did not become ready: {logs.strip()}")
        log_line(self.directory, "mosquitto ready")

    def _access_environment(self) -> dict[str, str]:
        return {
            **os.environ,
            "WHALEREIN_DATABASE_URL": (
                f"postgres://postgres@127.0.0.1:{self.ports['postgres']}/{DATABASE_NAME}"
                "?sslmode=disable"
            ),
            "WHALEREIN_ADMIN_TOKEN": self.secrets["admin-token"],
            "WHALEREIN_HOST_PROVISIONING_TOKEN": self.secrets["host-token"],
            "WHALEREIN_BROKER_PASSWORD": self.secrets["broker-password"],
            "WHALEREIN_INTERNAL_TOKEN": self.secrets["internal-token"],
        }

    def start_access(self) -> None:
        config = self.directory / "access.yaml"
        if not config.exists():
            config = self._access_config()
        self.access = Child(
            "access",
            [str(ACCESS_BIN), "--config", str(config)],
            env=self._access_environment(),
            log_path=self.directory / "access.log",
        )
        self._write_pids()
        log_line(self.directory, "access process started")

        def health() -> bool:
            return self._http_status(f"http://127.0.0.1:{self.ports['access']}/healthz") == 200

        try:
            wait_until("Access health", health, ACCESS_READY_TIMEOUT, child=self.access.process)
            wait_until(
                "Access readiness",
                lambda: self._http_status(f"http://127.0.0.1:{self.ports['access']}/readyz")
                == 200,
                ACCESS_READY_TIMEOUT,
                child=self.access.process,
            )
        except StackError as error:
            raise StackError("access_failed", f"{error.message}: {self.access.tail()}") from error
        log_line(self.directory, "access ready")

    def _fixture_environment(self) -> dict[str, str]:
        return {
            **os.environ,
            "WHALEREIN_DSH_SOCKET": str(self.directory / "dsh.sock"),
            "WHALEREIN_MQTT_URL": f"mqtt://127.0.0.1:{self.ports['mqtt']}",
            "WHALEREIN_HOST_ID": self.host_id,
            "WHALEREIN_HOST_TOKEN": self.secrets["host-token"],
            "WHALEREIN_STATE_DIR": str(self.directory / "plugin"),
            "WHALEREIN_ALLOW_INSECURE_MQTT": "1",
            "WHALEREIN_LOG_LEVEL": "warn",
        }

    def start_fixture(self) -> None:
        socket_path = self.directory / "dsh.sock"
        try:
            socket_path.unlink()
        except FileNotFoundError:
            pass
        self.dsh = Child(
            "dsh",
            [
                "node",
                str(ROOT / "tests/integration/fixtures/dsh.mjs"),
                RUN_MARKER_ARG,
                self.run_id,
                "--ios-stack-state",
                str(self.directory),
            ],
            env=self._fixture_environment(),
            log_path=self.directory / "dsh.log",
            control=True,
        )
        self._write_pids()
        wait_until("FakeDSH control socket", socket_path.exists, READY_TIMEOUT, child=self.dsh.process)
        self.dsh.call("create_session", session_id=FIXTURE_SESSION_ID)
        for _ in INITIAL_TRANSCRIPT:
            self.dsh.call("emit", session_id=FIXTURE_SESSION_ID)
        log_line(self.directory, "fixture session seeded")

    def start_plugin(self) -> None:
        self.plugin = Child(
            "plugin",
            [
                "node",
                str(ROOT / "tests/integration/fixtures/plugin.mjs"),
                RUN_MARKER_ARG,
                self.run_id,
                "--ios-stack-state",
                str(self.directory),
            ],
            env=self._fixture_environment(),
            log_path=self.directory / "plugin.log",
        )
        self._write_pids()
        log_path = self.directory / "plugin.log"

        def ready() -> bool:
            return "plugin ready" in log_path.read_text(encoding="utf-8", errors="replace")

        try:
            wait_until("Host plugin readiness", ready, PLUGIN_READY_TIMEOUT, child=self.plugin.process)
        except StackError as error:
            raise StackError("host_failed", f"{error.message}: {self.plugin.tail()}") from error
        log_line(self.directory, "host plugin ready")

    # ---- invitation and contract ---------------------------------------

    def _http_status(self, url: str, timeout: float = 2.0) -> int:
        try:
            with urllib.request.urlopen(url, timeout=timeout) as response:
                return response.status
        except (urllib.error.URLError, OSError, ValueError):
            return 0

    def _post_invitation(self) -> dict:
        body = json.dumps({"hostId": self.host_id}).encode("utf-8")
        request = urllib.request.Request(
            f"http://127.0.0.1:{self.ports['access']}/v1/pairing/invitations",
            data=body,
            headers={
                "Content-Type": "application/json",
                "Authorization": "Bearer " + self.secrets["admin-token"],
            },
        )
        try:
            with urllib.request.urlopen(request, timeout=10) as response:
                return json.load(response)
        except urllib.error.HTTPError as error:
            raise StackError(
                "invitation_failed",
                f"Access rejected the invitation with HTTP {error.code}",
            ) from error
        except (urllib.error.URLError, OSError, ValueError) as error:
            raise StackError("invitation_failed", "Access invitation request failed") from error

    def create_invitation(self) -> None:
        value = self._post_invitation()
        uri = value.get("pairingUri")
        if not uri:
            raise StackError("invitation_failed", "Access returned no pairing URI")
        atomic_write(self.directory / "invitation.txt", uri + "\n", 0o600)
        self.invitation = {
            "invitation_uri": uri,
            "invitation_expires_at_unix_ms": value.get("expiresAtUnixMs"),
        }
        log_line(self.directory, "pairing invitation created")

    def _start_http_control(self) -> None:
        supervisor = self
        token = self.secrets["control-token"]

        class Handler(BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"

            def log_message(self, _format, *_args):
                log_line(supervisor.directory, f"http control {self.command} {self.path.split('?')[0]}")

            def _send(self, status: int, value) -> None:
                body = json.dumps(value).encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)

            def _authorized(self) -> bool:
                header = self.headers.get("Authorization", "")
                expected = "Bearer " + token
                return hmac.compare_digest(header, expected)

            def _read_body(self) -> dict:
                try:
                    length = int(self.headers.get("Content-Length", "0") or "0")
                except ValueError:
                    length = 0
                if length <= 0 or length > 2048:
                    return {}
                try:
                    value = json.loads(self.rfile.read(length))
                except ValueError:
                    return {}
                return value if isinstance(value, dict) else {}

            def do_GET(self):
                if not self._authorized():
                    self._send(401, {"ok": False, "error": {"code": "unauthorized"}})
                    return
                path = self.path.split("?")[0]
                try:
                    with supervisor.lock:
                        if path == "/v1/status":
                            self._send(200, {"ok": True, "data": supervisor.status()})
                            return
                        if path == "/v1/invitation":
                            if "refresh=1" in self.path:
                                data = supervisor.refresh_invitation()
                            else:
                                data = {
                                    "invitation_uri": supervisor.invitation["invitation_uri"],
                                    "invitation_expires_at_unix_ms": supervisor.invitation[
                                        "invitation_expires_at_unix_ms"
                                    ],
                                }
                            self._send(200, {"ok": True, "data": data})
                            return
                    self._send(404, {"ok": False, "error": {"code": "not_found"}})
                except StackError as error:
                    self._send(500, {"ok": False, "error": {"code": error.code, "message": error.message}})

            def do_POST(self):
                if not self._authorized():
                    self._send(401, {"ok": False, "error": {"code": "unauthorized"}})
                    return
                path = self.path.split("?")[0]
                body = self._read_body()
                try:
                    with supervisor.lock:
                        if path == "/v1/seed-event":
                            data = supervisor.seed_event(body.get("kind"))
                        elif path == "/v1/restart":
                            data = supervisor.restart(body.get("target"))
                        elif path == "/v1/broker":
                            data = supervisor.broker_action(body.get("action"))
                        else:
                            self._send(404, {"ok": False, "error": {"code": "not_found"}})
                            return
                    self._send(200, {"ok": True, "data": data})
                except StackError as error:
                    self._send(400, {"ok": False, "error": {"code": error.code, "message": error.message}})

        self.http_server = ThreadingHTTPServer(("127.0.0.1", self.control_port), Handler)
        self.http_server.daemon_threads = True
        thread = threading.Thread(target=self.http_server.serve_forever, daemon=True)
        thread.start()
        log_line(self.directory, "http control endpoint started")

    def _contract(self) -> dict:
        state_path = self.directory / "state.json"
        client_config = self.directory / "client.toml"
        if not client_config.exists():
            atomic_write(
                client_config,
                'version = 1\nservice = "standalone"\ndata_dir = "./client"\n',
            )
        expected = [
            {"role": "assistant", "text": text, "source": "seed"} for text in self.expected
        ]
        return {
            "schema": SCHEMA,
            "ready": True,
            "run_id": self.run_id,
            "state_path": str(state_path),
            "state_dir": str(self.directory),
            "access_url": f"http://127.0.0.1:{self.ports['access']}",
            "mqtt_url": f"mqtt://127.0.0.1:{self.ports['mqtt']}",
            "invitation_uri": self.invitation["invitation_uri"],
            "invitation_expires_at_unix_ms": self.invitation[
                "invitation_expires_at_unix_ms"
            ],
            "control_url": f"http://127.0.0.1:{self.control_port}",
            "control_token": self.secrets["control-token"],
            "host_id": self.host_id,
            "session_id": FIXTURE_SESSION_ID,
            "expected_transcript": list(self.expected),
            "expected_transcript_detail": expected,
            "expected_message_count": len(self.expected),
            "stream_expectation": {
                "stream_id": "fixture-attempt",
                "preview_text": STREAM_PREVIEW_TEXT,
                "final_text": STREAM_FINAL_TEXT,
            },
            "teardown_marker": self.run_id,
            "teardown_marker_file": str(self.directory / "run.marker"),
            "cleanup_command": [
                sys.executable,
                str(Path(__file__).resolve()),
                "down",
                "--state",
                str(state_path),
            ],
            "cli": {"config": str(client_config)},
            "containers": {
                "postgres": POSTGRES_CONTAINER,
                "broker": MOSQUITTO_CONTAINER,
            },
            "ports": dict(self.ports),
            "known_limitations": [
                "Loopback plaintext HTTP and MQTT only; no TLS listener.",
                "Docker Desktop (or host-gateway support) is required for the pinned broker image.",
                "The fixture is FakeDSH, not real DSH; it does not certify native persistence.",
                "Only one ios-stack instance is active at a time.",
                "A physical device cannot reach this host loopback stack.",
            ],
        }

    def write_contract(self) -> None:
        contract = self._contract()
        atomic_write_json(self.directory / "state.json", contract, 0o600)
        lines = [
            f"WHALEREIN_IOS_STACK_STATE={shlex.quote(contract['state_path'])}",
            f"WHALEREIN_IOS_STACK_ACCESS_URL={shlex.quote(contract['access_url'])}",
            f"WHALEREIN_IOS_STACK_MQTT_URL={shlex.quote(contract['mqtt_url'])}",
            f"WHALEREIN_IOS_STACK_INVITATION_URI={shlex.quote(contract['invitation_uri'])}",
            f"WHALEREIN_IOS_STACK_CONTROL_URL={shlex.quote(contract['control_url'])}",
            f"WHALEREIN_IOS_STACK_CONTROL_TOKEN={shlex.quote(contract['control_token'])}",
            f"WHALEREIN_IOS_STACK_HOST_ID={contract['host_id']}",
            f"WHALEREIN_IOS_STACK_SESSION_ID={contract['session_id']}",
            f"WHALEREIN_IOS_STACK_TEARDOWN_MARKER={contract['teardown_marker']}",
            f"WHALEREIN_IOS_STACK_STATE_DIR={shlex.quote(contract['state_dir'])}",
        ]
        atomic_write(self.directory / "contract.env", "\n".join(lines) + "\n", 0o600)
        atomic_write(self.directory / "expected.json", json.dumps(
            {"messages": contract["expected_transcript_detail"]}, indent=2
        ) + "\n", 0o600)
        atomic_write(self.directory / "run.marker", self.run_id + "\n", 0o600)
        self.state = contract

    def _write_pids(self) -> None:
        pids = {
            "supervisor": os.getpid(),
            "access": self.access.process.pid if self.access else None,
            "dsh": self.dsh.process.pid if self.dsh else None,
            "plugin": self.plugin.process.pid if self.plugin else None,
        }
        pids["starts"] = {
            str(pid): _process_start(pid) for pid in pids.values() if pid is not None
        }
        atomic_write_json(self.directory / "pids.json", pids, 0o600)

    # ---- lifecycle ------------------------------------------------------

    def start(self) -> None:
        os.makedirs(self.directory, exist_ok=True)
        os.chmod(self.directory, 0o711)
        atomic_write(self.directory / "run.marker", self.run_id + "\n")
        self._write_pids()
        self._load_or_create_secrets()
        self.ports = {
            "access": free_port(),
            "internal": free_port(),
            "mqtt": free_port(),
            "postgres": free_port(),
        }
        self.control_port = free_port()
        self._access_config()
        for name in ("client", "client-secrets", "plugin"):
            path = self.directory / name
            path.mkdir(exist_ok=True)
            os.chmod(path, 0o700)
        self.start_postgres()
        self.start_broker()
        self.start_access()
        self.start_fixture()
        self.start_plugin()
        self.create_invitation()
        self._start_http_control()
        self.ready = True
        self.write_contract()
        self._write_pids()
        log_line(self.directory, f"stack ready run={self.run_id}")

    def health(self) -> bool:
        containers = [
            container_info(MOSQUITTO_CONTAINER),
            container_info(POSTGRES_CONTAINER),
        ]
        return (
            self.ready
            and bool(self.access and self.access.alive())
            and bool(self.dsh and self.dsh.alive())
            and bool(self.plugin and self.plugin.alive())
            and all(
                info is not None and info["state"].get("Status") == "running"
                for info in containers
            )
        )

    def status(self) -> dict:
        fixture = None
        if self.dsh is not None and self.dsh.alive():
            try:
                snapshot = self.dsh.call("snapshot", session_id=FIXTURE_SESSION_ID)
                fixture = {
                    "running": bool(snapshot.get("summary", {}).get("running")),
                    "history_events": len(snapshot.get("events", [])),
                }
            except StackError as error:
                fixture = {"error": error.code}
        pending_approval = None
        if self.last_approval and self.dsh is not None and self.dsh.alive():
            try:
                current = self.dsh.call(
                    "approval", approval_id=self.last_approval["approval_id"]
                )
            except StackError:
                current = None
            if current is not None and current.get("state") == "pending":
                pending_approval = self.last_approval
        containers = {}
        artifacts = {}
        for key, name in (("postgres", POSTGRES_CONTAINER), ("broker", MOSQUITTO_CONTAINER)):
            info = container_info(name)
            containers[key] = None if info is None else info["state"].get("Status")
            if info is not None:
                artifacts[key] = info["image"]
        if ACCESS_BIN.is_file():
            with ACCESS_BIN.open("rb") as binary:
                artifacts["access"] = hashlib.file_digest(binary, "sha256").hexdigest()
        return {
            "schema": SCHEMA,
            "ready": self.health(),
            "run_id": self.run_id,
            "state_path": str(self.directory / "state.json"),
            "access_url": f"http://127.0.0.1:{self.ports.get('access')}",
            "mqtt_url": f"mqtt://127.0.0.1:{self.ports.get('mqtt')}",
            "control_url": f"http://127.0.0.1:{self.control_port}",
            "host_id": self.host_id,
            "session_id": FIXTURE_SESSION_ID,
            "teardown_marker": self.run_id,
            "processes": {
                "access": bool(self.access and self.access.alive()),
                "dsh": bool(self.dsh and self.dsh.alive()),
                "plugin": bool(self.plugin and self.plugin.alive()),
            },
            "containers": containers,
            "artifacts": artifacts,
            "fixture": fixture,
            "pending_approval": pending_approval,
            "invitation_expires_at_unix_ms": getattr(self, "invitation", {}).get(
                "invitation_expires_at_unix_ms"
            ),
            "expected_transcript": list(self.expected),
        }

    def seed_event(self, kind: str) -> dict:
        if self.dsh is None:
            raise StackError("not_ready", "the fixture is not running")
        session = FIXTURE_SESSION_ID
        result: dict = {"kind": kind}
        if kind == "output":
            value = self.dsh.call("emit", session_id=session)
            self.expected.append("fixture output")
            result["appended_message"] = "fixture output"
            result["seq"] = value.get("seq") if isinstance(value, dict) else None
        elif kind == "stream":
            self.dsh.call("preview_start", session_id=session)
            self.dsh.call("preview_chunk", session_id=session)
            # Hold the provisional preview long enough for a live watch to
            # observe it before the committed final message arrives.
            try:
                preview_pause_ms = int(os.environ.get("WHALEREIN_IOS_STACK_PREVIEW_PAUSE_MS", "600"))
            except ValueError:
                preview_pause_ms = 600
            time.sleep(max(0, min(preview_pause_ms, 5000)) / 1000)
            self.dsh.call("preview_final", session_id=session)
            self.expected.append(STREAM_FINAL_TEXT)
            result["appended_message"] = STREAM_FINAL_TEXT
            result["preview_text"] = STREAM_PREVIEW_TEXT
        elif kind == "preview":
            # Split streaming: emit the provisional preview only, so a test can
            # observe it before the committed final message exists.
            self.dsh.call("preview_start", session_id=session)
            self.dsh.call("preview_chunk", session_id=session)
            result["preview_text"] = STREAM_PREVIEW_TEXT
        elif kind == "final":
            self.dsh.call("preview_final", session_id=session)
            self.expected.append(STREAM_FINAL_TEXT)
            result["appended_message"] = STREAM_FINAL_TEXT
        elif kind == "approval":
            value = self.dsh.call("request_approval", session_id=session)
            approval = {
                "approval_id": value.get("approvalId"),
                "version": value.get("version"),
                "tool_name": value.get("toolName"),
                "expires_at_unix_ms": value.get("expiresAt"),
            }
            self.last_approval = approval
            result["approval"] = approval
        elif kind == "running":
            self.dsh.call("running", session_id=session)
            result["session_running"] = True
        elif kind == "idle":
            self.dsh.call("idle", session_id=session)
            result["session_running"] = False
        else:
            raise StackError("invalid_argument", f"unknown seed event {kind}")
        self.write_contract()
        result["expected_transcript"] = list(self.expected)
        log_line(self.directory, f"seed event {kind}")
        return result

    def refresh_invitation(self) -> dict:
        self.create_invitation()
        self.write_contract()
        return {
            "invitation_uri": self.invitation["invitation_uri"],
            "invitation_expires_at_unix_ms": self.invitation[
                "invitation_expires_at_unix_ms"
            ],
        }

    def restart(self, target: str) -> dict:
        if target == "access":
            if self.access is not None:
                self.access.close()
            self.start_access()
            log_line(self.directory, "access restarted")
            return {"restarted": "access", "ready": self.ready}
        if target == "broker":
            docker("restart", MOSQUITTO_CONTAINER, timeout=120)
            wait_until("Mosquitto readiness", lambda: port_open(self.ports["mqtt"]), READY_TIMEOUT)
            wait_until(
                "Access readiness after broker restart",
                lambda: self._http_status(f"http://127.0.0.1:{self.ports['access']}/readyz")
                == 200,
                ACCESS_READY_TIMEOUT,
            )
            time.sleep(0.5)
            log_line(self.directory, "broker restarted")
            return {"restarted": "broker", "ready": self.ready}
        raise StackError("invalid_argument", f"unknown restart target {target}")

    def broker_action(self, action: str) -> dict:
        """Stop or start the broker for deterministic transport-loss tests.

        This is additive to `restart broker`: `stop` leaves the broker down so
        a test can inspect HTTPS-backed operations and diagnostics while MQTT
        is unavailable, then `start` restores it.
        """
        if action == "stop":
            # A graceful stop can outlive the control request timeout and leaves
            # the device MQTT socket half-open; kill the broker so transport
            # loss is immediate and deterministic.
            docker("kill", MOSQUITTO_CONTAINER, timeout=120)
            log_line(self.directory, "broker stopped")
            return {"action": "stop", "ready": self.health()}
        if action == "start":
            docker("start", MOSQUITTO_CONTAINER, timeout=120)
            wait_until("Mosquitto readiness", lambda: port_open(self.ports["mqtt"]), READY_TIMEOUT)
            wait_until(
                "Access readiness after broker start",
                lambda: self._http_status(f"http://127.0.0.1:{self.ports['access']}/readyz")
                == 200,
                ACCESS_READY_TIMEOUT,
            )
            time.sleep(0.5)
            log_line(self.directory, "broker started")
            return {"action": "start", "ready": self.ready}
        raise StackError("invalid_argument", f"unknown broker action {action}")

    def shutdown(self) -> dict:
        errors: list[str] = []
        if self.http_server is not None:
            try:
                self.http_server.shutdown()
                self.http_server.server_close()
            except OSError as error:
                errors.append(f"http control: {error}")
            self.http_server = None
        removed: list[str] = []
        for child in (self.plugin, self.dsh, self.access):
            if child is None:
                continue
            try:
                child.close()
            except OSError as error:
                errors.append(f"{child.name}: {error}")
        for name in (MOSQUITTO_CONTAINER, POSTGRES_CONTAINER):
            try:
                if remove_owned_container(name, self.run_id):
                    removed.append(name)
            except StackError as error:
                errors.append(error.message)
        result = {
            "run_id": self.run_id,
            "removed_containers": removed,
            "processes_stopped": [
                name
                for name, child in (
                    ("plugin", self.plugin),
                    ("dsh", self.dsh),
                    ("access", self.access),
                )
                if child is not None
            ],
            "errors": errors,
        }
        atomic_write_json(self.directory / "down-result.json", result, 0o600)
        log_line(self.directory, "stack stopped")
        return result


# ---- supervisor socket and entry point --------------------------------


def _control_server(supervisor: Supervisor) -> None:
    path = supervisor.directory / "control.sock"
    try:
        path.unlink()
    except FileNotFoundError:
        pass
    server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    server.bind(str(path))
    os.chmod(path, 0o600)
    server.listen(8)
    server.settimeout(0.5)

    def handle(connection: socket.socket) -> None:
        try:
            with connection, connection.makefile("rwb") as stream:
                line = stream.readline()
                if not line:
                    return
                try:
                    request = json.loads(line)
                except ValueError:
                    response = {"ok": False, "error": {"code": "invalid_request"}}
                else:
                    response = _dispatch(supervisor, request)
                stream.write((json.dumps(response) + "\n").encode("utf-8"))
                stream.flush()
        except (OSError, ValueError):
            pass

    while not supervisor.stopping.is_set():
        try:
            connection, _ = server.accept()
        except TimeoutError:
            continue
        except OSError:
            break
        thread = threading.Thread(target=handle, args=(connection,), daemon=True)
        thread.start()
    server.close()
    try:
        path.unlink()
    except FileNotFoundError:
        pass


def _dispatch(supervisor: Supervisor, request: dict) -> dict:
    command = request.get("command")
    try:
        with supervisor.lock:
            if command == "ping":
                return {
                    "ok": True,
                    "data": {
                        "ready": supervisor.health(),
                        "run_id": supervisor.run_id,
                    },
                }
            if command == "status":
                return {"ok": True, "data": supervisor.status()}
            if command == "invitation":
                if request.get("refresh"):
                    return {"ok": True, "data": supervisor.refresh_invitation()}
                return {
                    "ok": True,
                    "data": {
                        "invitation_uri": getattr(supervisor, "invitation", {}).get(
                            "invitation_uri"
                        ),
                        "invitation_expires_at_unix_ms": getattr(
                            supervisor, "invitation", {}
                        ).get("invitation_expires_at_unix_ms"),
                    },
                }
            if command == "seed-event":
                return {"ok": True, "data": supervisor.seed_event(request.get("kind"))}
            if command == "restart":
                return {"ok": True, "data": supervisor.restart(request.get("target"))}
            if command == "shutdown":
                supervisor.stopping.set()
                return {"ok": True, "data": {"stopping": True}}
            return {"ok": False, "error": {"code": "unknown_command"}}
    except StackError as error:
        return {"ok": False, "error": {"code": error.code, "message": error.message}}
    except Exception as error:  # noqa: BLE001 - report a bounded failure to the caller
        return {"ok": False, "error": {"code": "internal", "message": str(error)}}


def supervise(directory: Path) -> int:
    supervisor = Supervisor(directory.resolve())

    def request_stop(_signum, _frame):
        supervisor.stopping.set()

    signal.signal(signal.SIGTERM, request_stop)
    signal.signal(signal.SIGINT, request_stop)
    server_thread = threading.Thread(
        target=_control_server, args=(supervisor,), daemon=True
    )
    server_thread.start()
    try:
        supervisor.start()
    except Exception as error:  # noqa: BLE001 - startup must always clean up
        supervisor.startup_error = str(error)
        message = error.message if isinstance(error, StackError) else str(error)
        code = error.code if isinstance(error, StackError) else "internal"
        atomic_write_json(
            directory / "error.json",
            {"schema": SCHEMA, "code": code, "message": message},
            0o600,
        )
        log_line(directory, f"startup failed: {code}")
        supervisor.shutdown()
        return 1
    try:
        while not supervisor.stopping.wait(1.0):
            pass
    finally:
        supervisor.shutdown()
    return 0


# ---- client helpers ----------------------------------------------------


def socket_request(directory: Path, request: dict, timeout: float = 20.0) -> dict:
    path = directory / "control.sock"
    try:
        with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as client:
            client.settimeout(timeout)
            client.connect(str(path))
            client.sendall((json.dumps(request) + "\n").encode("utf-8"))
            stream = client.makefile("rb")
            line = stream.readline()
    except (OSError, ValueError) as error:
        raise StackError("not_running", "the ios-stack supervisor is not running") from error
    if not line:
        raise StackError("not_running", "the ios-stack supervisor closed the control socket")
    try:
        response = json.loads(line)
    except ValueError as error:
        raise StackError("internal", "invalid control response") from error
    if not response.get("ok"):
        error = response.get("error") or {}
        raise StackError(error.get("code", "internal"), error.get("message", "control failed"))
    return response.get("data") or {}


def resolve_state(value: str | None) -> tuple[Path, Path]:
    if value is None:
        directory = CURRENT_DIR
    else:
        candidate = Path(value).expanduser()
        if candidate.is_dir():
            directory = candidate
        elif candidate.name == "state.json":
            directory = candidate.parent
        else:
            directory = candidate
    directory = directory.resolve()
    return directory, directory / "state.json"


def print_json(value) -> None:
    sys.stdout.write(json.dumps(value, indent=2, sort_keys=True) + "\n")


def _pid_alive(pid: int) -> bool:
    try:
        os.kill(pid, 0)
        return True
    except (ProcessLookupError, PermissionError):
        return False


def _pid_matches(pid: int, needles: list[str]) -> bool:
    completed = run(["ps", "-p", str(pid), "-o", "command="], check=False, timeout=5)
    command = completed.stdout
    return bool(command) and any(needle and needle in command for needle in needles)


def _process_start(pid: int) -> str:
    completed = run(["ps", "-p", str(pid), "-o", "lstart="], check=False, timeout=5)
    return completed.stdout.strip() if completed.returncode == 0 else ""


def _terminate_pid(pid: int, start: str, needles: list[str], grace: float = 5.0) -> bool:
    if not start or start != _process_start(pid) or not _pid_matches(pid, needles):
        return False
    try:
        if os.getpgid(pid) == pid:
            os.killpg(pid, signal.SIGTERM)
        else:
            os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return False
    deadline = time.monotonic() + grace
    while time.monotonic() < deadline:
        if not _pid_alive(pid) or _process_start(pid) != start:
            return True
        time.sleep(0.1)
    if _process_start(pid) != start or not _pid_matches(pid, needles):
        return False
    try:
        if os.getpgid(pid) == pid:
            os.killpg(pid, signal.SIGKILL)
        else:
            os.kill(pid, signal.SIGKILL)
    except ProcessLookupError:
        pass
    return True


def find_owned_pids(directory: Path, run_id: str | None) -> list[int]:
    """Return PIDs whose command line carries this run's unique markers."""
    completed = run(["ps", "-axo", "pid=,command="], check=False, timeout=10)
    found = []
    for line in completed.stdout.splitlines():
        stripped = line.strip()
        if not stripped:
            continue
        pid_text, _, command = stripped.partition(" ")
        if not pid_text.isdigit():
            continue
        pid = int(pid_text)
        if pid == os.getpid():
            continue
        markers = [
            f"_supervise --dir {directory}",
            f"{RUN_MARKER_ARG} {run_id}" if run_id else "",
            f"--ios-stack-state {directory}",
            f"--config {directory / 'access.yaml'}",
        ]
        if any(marker and marker in command for marker in markers):
            found.append(pid)
    return found


def cleanup_run(directory: Path, state: dict | None = None) -> dict:
    """Best-effort cleanup that only touches this run's labelled resources."""
    removed: list[str] = []
    errors: list[str] = []
    pids = read_json(directory / "pids.json") or {}
    run_id = (state or {}).get("run_id") or (read_json(directory / "state.json") or {}).get(
        "run_id"
    )
    if not run_id:
        try:
            run_id = (directory / "run.marker").read_text().strip()
        except FileNotFoundError:
            pass
    markers = [
        f"_supervise --dir {directory}",
        f"{RUN_MARKER_ARG} {run_id}" if run_id else "",
        f"--ios-stack-state {directory}",
        f"--config {directory / 'access.yaml'}",
    ]
    supervisor_pid = pids.get("supervisor")
    starts = pids.get("starts", {})
    candidates = find_owned_pids(directory, run_id)
    if supervisor_pid in candidates:
        candidates.remove(supervisor_pid)
        candidates.insert(0, supervisor_pid)
    for pid in candidates:
        start = starts.get(str(pid), "")
        if _pid_alive(pid) and not _terminate_pid(pid, start, markers) and _pid_alive(pid):
            errors.append(f"process ownership unverified: {pid}")
    for name in (MOSQUITTO_CONTAINER, POSTGRES_CONTAINER):
        try:
            if remove_owned_container(name, run_id):
                removed.append(name)
        except StackError as error:
            errors.append(error.message)
    return {"removed_containers": removed, "errors": errors}


def wait_supervisor_exit(pid: int | None, timeout: float) -> bool:
    if not isinstance(pid, int):
        return True
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if not _pid_alive(pid):
            return True
        time.sleep(0.1)
    return not _pid_alive(pid)


# ---- commands ----------------------------------------------------------


def lock_stack():
    DATA_DIR.mkdir(parents=True, exist_ok=True)
    handle = os.fdopen(
        os.open(DATA_DIR / "update.lock", os.O_CREAT | os.O_WRONLY, 0o600),
        "w",
        encoding="utf-8",
    )
    try:
        fcntl.flock(handle.fileno(), fcntl.LOCK_EX | fcntl.LOCK_NB)
    except OSError as error:
        handle.close()
        raise StackError("busy", "another ios-stack up/down command is running") from error
    return handle


def ensure_access_binary() -> None:
    BIN_DIR.mkdir(parents=True, exist_ok=True)
    go = shutil.which("go")
    if go is None:
        raise StackError("missing_command", "go is required to build WhaleRein Access")
    run(
        [go, "build", "-buildvcs=false", "-trimpath", "-o", str(ACCESS_BIN), "./" + ACCESS_SOURCE],
        timeout=900,
    )
    if not ACCESS_BIN.exists():
        raise StackError("build_failed", "Access binary was not produced")
    os.chmod(ACCESS_BIN, 0o755)


def ensure_dependencies() -> None:
    if shutil.which("docker") is None:
        raise StackError("missing_command", "docker is required for PostgreSQL and Mosquitto")
    if shutil.which("node") is None:
        raise StackError("missing_command", "node is required for the fixture Host")
    if docker("info", timeout=30, check=False).returncode != 0:
        raise StackError("docker_unavailable", "the Docker engine is not reachable")
    ensure_image(POSTGRES_IMAGE, timeout=1800)
    if docker("image", "inspect", MOSQUITTO_IMAGE, check=False).returncode != 0:
        sys.stderr.write(
            f"building {MOSQUITTO_IMAGE} from {MOSQUITTO_DOCKERFILE.relative_to(ROOT)}...\n"
        )
        docker(
            "build",
            "-f",
            str(MOSQUITTO_DOCKERFILE),
            "-t",
            MOSQUITTO_IMAGE,
            ".",
            timeout=1800,
        )


def healthy_state(directory: Path) -> dict | None:
    state = read_json(directory / "state.json")
    if not state or state.get("schema") != SCHEMA:
        return None
    try:
        ping = socket_request(directory, {"command": "ping"}, timeout=5)
    except StackError:
        return None
    if not ping.get("ready"):
        return None
    return state


def cmd_up(args: argparse.Namespace) -> int:
    directory, _state_path = resolve_state(args.state)
    handle = lock_stack()
    try:
        if not args.fresh:
            existing = healthy_state(directory)
            if existing is not None:
                sys.stderr.write("ios-stack already running; reusing the existing stack\n")
                if args.env:
                    sys.stdout.write((directory / "contract.env").read_text(encoding="utf-8"))
                else:
                    print_json(existing)
                return 0
        if directory.exists():
            sys.stderr.write("cleaning stale ios-stack state...\n")
            cleanup = cleanup_run(directory)
            if cleanup["errors"]:
                raise StackError("stale_cleanup_failed", "Cannot verify cleanup of the old stack")
            shutil.rmtree(directory)
        sys.stderr.write("building WhaleRein Access from the current worktree...\n")
        ensure_access_binary()
        ensure_dependencies()
        directory.mkdir(parents=True, exist_ok=True)
        os.chmod(directory, 0o711)
        log_path = directory / "supervisor.log"
        fd = os.open(log_path, os.O_CREAT | os.O_APPEND | os.O_WRONLY, 0o600)
        stream = os.fdopen(fd, "a", encoding="utf-8")
        process = subprocess.Popen(
            [sys.executable, str(Path(__file__).resolve()), "_supervise", "--dir", str(directory)],
            cwd=str(ROOT),
            stdin=subprocess.DEVNULL,
            stdout=stream,
            stderr=subprocess.STDOUT,
            start_new_session=True,
            text=True,
        )
        stream.close()
        if not (directory / "pids.json").exists():
            atomic_write_json(directory / "pids.json", {
                "supervisor": process.pid,
                "starts": {str(process.pid): _process_start(process.pid)},
            })
        deadline = time.monotonic() + args.timeout
        while time.monotonic() < deadline:
            error = read_json(directory / "error.json")
            if error is not None:
                cleanup_run(directory)
                raise StackError(error.get("code", "startup_failed"), error.get("message", ""))
            if process.poll() is not None:
                tail = ""
                try:
                    tail = log_path.read_text(encoding="utf-8", errors="replace").strip()[-1500:]
                except OSError:
                    pass
                raise StackError(
                    "startup_failed",
                    f"the ios-stack supervisor exited with status {process.returncode}. {tail}",
                )
            state = healthy_state(directory)
            if state is not None:
                if args.env:
                    sys.stdout.write((directory / "contract.env").read_text(encoding="utf-8"))
                else:
                    print_json(state)
                return 0
            time.sleep(0.1)
        cleanup_run(directory)
        raise StackError("timeout", f"ios-stack did not become ready within {int(args.timeout)}s")
    except StackError as error:
        sys.stderr.write(f"ios-stack up failed ({error.code}): {error.message}\n")
        return 1
    finally:
        handle.close()


def cmd_status(args: argparse.Namespace) -> int:
    directory, state_path = resolve_state(args.state)
    state = read_json(state_path)
    if state is None:
        if args.json:
            print_json({"schema": SCHEMA, "ready": False, "state_path": str(state_path)})
        else:
            sys.stderr.write("ios-stack is not running (no state file)\n")
        return 1
    try:
        status = socket_request(directory, {"command": "status"})
    except StackError as error:
        if args.json:
            print_json(
                {
                    "schema": SCHEMA,
                    "ready": False,
                    "state_path": str(state_path),
                    "error": {"code": error.code, "message": error.message},
                }
            )
        else:
            sys.stderr.write(f"ios-stack is not running: {error.message}\n")
        return 1
    if args.json:
        print_json(status)
    else:
        print(f"ready: {status['ready']}")
        print(f"access: {status['access_url']}")
        print(f"mqtt: {status['mqtt_url']}")
        print(f"host: {status['host_id']}")
        print(f"session: {status['session_id']} running={status['fixture']['running']}")
        print(f"state: {status['state_path']}")
    return 0 if status.get("ready") else 1


def cmd_invitation(args: argparse.Namespace) -> int:
    directory, _ = resolve_state(args.state)
    request = {"command": "invitation"}
    if args.refresh:
        request["refresh"] = True
    try:
        data = socket_request(directory, request)
    except StackError as error:
        sys.stderr.write(f"ios-stack invitation failed ({error.code}): {error.message}\n")
        return 1
    if args.json:
        print_json(data)
    else:
        print(data["invitation_uri"])
    return 0


def cmd_seed(args: argparse.Namespace) -> int:
    directory, _ = resolve_state(args.state)
    try:
        data = socket_request(
            directory, {"command": "seed-event", "kind": args.kind}, timeout=30
        )
    except StackError as error:
        sys.stderr.write(f"ios-stack seed-event failed ({error.code}): {error.message}\n")
        return 1
    print_json(data)
    return 0


def cmd_restart(args: argparse.Namespace) -> int:
    directory, _ = resolve_state(args.state)
    try:
        data = socket_request(
            directory, {"command": "restart", "target": args.target}, timeout=180
        )
    except StackError as error:
        sys.stderr.write(f"ios-stack restart failed ({error.code}): {error.message}\n")
        return 1
    print_json(data)
    return 0


def cmd_down(args: argparse.Namespace) -> int:
    directory, state_path = resolve_state(args.state)
    handle = lock_stack()
    try:
        state = read_json(state_path)
        pids = read_json(directory / "pids.json") or {}
        if (directory / "control.sock").exists():
            try:
                socket_request(directory, {"command": "shutdown"}, timeout=10)
            except StackError:
                pass
        wait_supervisor_exit(pids.get("supervisor"), 30)
        cleanup = cleanup_run(directory, state)
        result = {
            "schema": SCHEMA,
            "cleaned": not cleanup["errors"],
            "removed_containers": cleanup["removed_containers"],
            "errors": cleanup["errors"],
            "state_path": str(state_path),
        }
        remaining = []
        for name in (MOSQUITTO_CONTAINER, POSTGRES_CONTAINER):
            info = container_info(name)
            if info is not None:
                remaining.append(name)
        run_id = (state or {}).get("run_id")
        for pid in find_owned_pids(directory, run_id):
            remaining.append(f"pid:{pid}")
        result["remaining"] = remaining
        if remaining or cleanup["errors"]:
            result["cleaned"] = False
        else:
            shutil.rmtree(directory, ignore_errors=True)
            result["state_path_removed"] = not directory.exists()
        DATA_DIR.mkdir(parents=True, exist_ok=True)
        atomic_write_json(DATA_DIR / "last-down.json", result, 0o600)
        print_json(result)
        return 0 if result["cleaned"] else 1
    finally:
        handle.close()


def build_parser() -> argparse.ArgumentParser:
    parser = argparse.ArgumentParser(
        prog="ios-stack",
        description="Disposable loopback WhaleRein stack for iOS Integration tests.",
    )
    parser.add_argument(
        "--state",
        default=None,
        help="path to state.json or its run directory (default: .data/ios-stack/current)",
    )
    subparsers = parser.add_subparsers(dest="command", required=True)

    def add_state(parser: argparse.ArgumentParser) -> None:
        parser.add_argument(
            "--state",
            default=None,
            help="path to state.json or its run directory",
        )

    up = subparsers.add_parser("up", help="start the stack and print the JSON contract")
    add_state(up)
    up.add_argument("--fresh", action="store_true", help="replace any running stack")
    up.add_argument("--env", action="store_true", help="print the shell env contract")
    up.add_argument("--timeout", type=float, default=DEFAULT_UP_TIMEOUT)
    up.set_defaults(handler=cmd_up)

    status = subparsers.add_parser("status", help="report stack readiness")
    add_state(status)
    status.add_argument("--json", action="store_true")
    status.set_defaults(handler=cmd_status)

    invitation = subparsers.add_parser("invitation", help="print a pairing invitation")
    add_state(invitation)
    invitation.add_argument("--json", action="store_true")
    invitation.add_argument("--refresh", action="store_true", help="mint a fresh invitation")
    invitation.set_defaults(handler=cmd_invitation)

    seed = subparsers.add_parser("seed-event", help="drive the fixture source")
    add_state(seed)
    seed.add_argument(
        "kind",
        choices=["output", "stream", "approval", "running", "idle"],
        help="fixture event to produce",
    )
    seed.set_defaults(handler=cmd_seed)

    restart = subparsers.add_parser("restart", help="restart Access or the broker")
    add_state(restart)
    restart.add_argument("target", choices=["access", "broker"])
    restart.set_defaults(handler=cmd_restart)

    down = subparsers.add_parser("down", help="stop the stack and verify cleanup")
    add_state(down)
    down.set_defaults(handler=cmd_down)

    supervise_parser = subparsers.add_parser("_supervise")
    add_state(supervise_parser)
    supervise_parser.add_argument("--dir", required=True)
    supervise_parser.set_defaults(handler=lambda args: supervise(Path(args.dir)))
    return parser


def main(argv: list[str] | None = None) -> int:
    parser = build_parser()
    args = parser.parse_args(argv)
    return int(args.handler(args))


if __name__ == "__main__":
    sys.exit(main())
