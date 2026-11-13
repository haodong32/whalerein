"""Isolated native DSH journeys; no personal profile, external model or host runtime."""

import concurrent.futures
import http.server
import json
import os
import signal
import re
import socket
import sqlite3
import subprocess
import threading
from pathlib import Path

from environment import ROOT, Process, Scenario, wait


class Model:
    def __init__(self):
        self.kind = "text"
        self.calls = 0
        self.started = threading.Event()
        self.release = threading.Event()
        self.release.set()
        owner = self

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, *_args):
                pass

            def do_POST(self):
                # Request data is consumed privately and never exported or logged.
                self.rfile.read(int(self.headers.get("Content-Length", "0")))
                kind = owner.kind
                owner.calls += 1
                call = owner.calls
                owner.started.set()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()

                def emit(event, **fields):
                    frame = {"type": event, **fields}
                    self.wfile.write(
                        (f"event: {event}\ndata: {json.dumps(frame)}\n\n").encode()
                    )
                    self.wfile.flush()

                try:
                    emit(
                        "message_start",
                        message={
                            "id": "native-fixture",
                            "role": "assistant",
                            "type": "message",
                            "model": "deepseek-flash",
                            "content": [],
                            "usage": {"input_tokens": 10, "output_tokens": 0},
                        },
                    )
                    if kind in {"tool", "tool-sensitive", "question", "question-long", "question-multiple"} and call == 1:
                        emit(
                            "content_block_start",
                            index=0,
                            content_block={
                                "type": "tool_use",
                                "id": "native-fixture-call",
                                "name": "ask_user_question" if kind in {"question", "question-long", "question-multiple"} else "native_fixture",
                                "input": {},
                            },
                        )
                        emit(
                            "content_block_delta",
                            index=0,
                            delta={"type": "input_json_delta", "partial_json": json.dumps({
                                "questions": ([{"id": "q1", "question": "Fixture question", "multi_select": True, "options": [{"label": "A"}, {"label": "B"}]}, {"id": "q2", "question": "Second fixture", "options": [{"label": "Yes"}, {"label": "No"}]}] if kind == "question-multiple" else [{"id": "q1", "question": "Fixture question", "options": [{"label": "Yes"}, {"label": "No"}]}]),
                                "timeout": 3 if kind == "question-long" else 1,
                            }) if kind in {"question", "question-long", "question-multiple"} else (json.dumps({"note": "fork-synthetic-tool-parameter-b4821"}) if kind == "tool-sensitive" else "{}")},
                        )
                        reason = "tool_use"
                    else:
                        emit(
                            "content_block_start",
                            index=0,
                            content_block={"type": "text", "text": ""},
                        )
                        emit(
                            "content_block_delta",
                            index=0,
                            delta={"type": "text_delta", "text": "native "},
                        )
                        if not owner.release.wait(timeout=25):
                            return
                        emit(
                            "content_block_delta",
                            index=0,
                            delta={"type": "text_delta", "text": "fixture "},
                        )
                        emit(
                            "content_block_delta",
                            index=0,
                            delta={
                                "type": "text_delta",
                                "text": "x" * 80000 if kind == "large" else "response",
                            },
                        )
                        reason = "end_turn"
                    emit("content_block_stop", index=0)
                    emit(
                        "message_delta",
                        delta={"stop_reason": reason},
                        usage={"output_tokens": 3},
                    )
                    emit("message_stop")
                except (BrokenPipeError, ConnectionError):
                    pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def plan(self, kind="text", hold=False):
        self.kind = kind
        self.calls = 0
        self.started.clear()
        self.release.clear() if hold else self.release.set()

    def close(self):
        self.release.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(timeout=5)
        assert not self.thread.is_alive(), "Local model server did not stop"


class NativeScenario(Scenario):
    def __init__(self, directory, schema, timed_questions=False, debug_logging=False, wrapper_launcher=False, external_launcher=False):
        super().__init__(directory, schema)
        self.model = None
        self.native = None
        self.prepared = False
        self.timed_questions = timed_questions
        self.debug_logging = debug_logging
        self.wrapper_launcher = wrapper_launcher
        self.external_launcher = external_launcher
        self.socket = directory / "native.sock"
        self.home = directory / "dsh-home"
        self.http_port = 3109
        self.work = directory / "work"
        self.work.mkdir()
        self.overlay = directory / "native-overlay.yml"

    def request(self, method, **fields):
        connection = socket.socket(socket.AF_UNIX)
        connection.settimeout(25)
        try:
            connection.connect(str(self.socket))
            connection.sendall(
                (json.dumps({"method": method, **fields}) + "\n").encode()
            )
            data = b""
            while b"\n" not in data:
                chunk = connection.recv(65536)
                if not chunk:
                    break
                data += chunk
            response = json.loads(data)
            if not response.get("ok"):
                raise RuntimeError("Native fixture operation rejected")
            return response["result"]
        finally:
            connection.close()

    def _boot(self):
        launcher = ["/opt/native/node_modules/.bin/dsh"]
        if self.wrapper_launcher:
            wrapper = self.work / "wrapped-dsh.mjs"
            wrapper.write_text("const {runCli} = await import('/opt/native/node_modules/@deepseek-ai/dsh/lib/bin.js'); await runCli()\n")
            launcher = ["node", str(wrapper)]
        elif self.external_launcher:
            bin_dir = self.work / "external-bin"
            bin_dir.mkdir(exist_ok=True)
            link = bin_dir / "dsh"
            if not link.is_symlink():
                link.symlink_to("/opt/native/node_modules/.bin/dsh")
            launcher = [str(link)]
        self.native = Process(
            [
                *launcher,
                "--profile",
                "web",
                "--patch",
                str(self.overlay),
                "--port",
                str(self.http_port),
            ],
            self.work,
            {
                "DSH_HOME": str(self.home),
                "WHALEREIN_NATIVE_SOCKET": str(self.socket),
                "NATIVE_MODEL_KEY": "sk-" + "0" * 48,
                "DSH_PERMISSION_MODE": "workspace-write",
            },
            cwd=self.work,
        )
        self.processes.append(self.native)
        wait(
            lambda: self.request("booted").get("ready"), timeout=30, process=self.native
        )

    def start_native(self):
        if self.model is None:
            self.model = Model()
        # Invocation overlays configure only fixture infrastructure. The actual
        # connector is installed as a bundle and seeded through native Settings,
        # so browser form writes are not masked by invocation overrides.
        disabled = [
            "session-title-llm",
            "session-telemetry-otel",
            "otel",
            "llm-deepseek-account",
            "llm-pi-ai",
            "hmr",
        ]
        lines = [
            line for key in disabled for line in (f"- id: {key}", "  disabled: true")
        ]
        lines += [
            "- id: plugin-manager",
            "  config:",
            "    pnpmCommand: /opt/native/node_modules/.bin/pnpm",
            "- id: llm-deepseek",
            "  config:",
            f"    baseURL: http://127.0.0.1:{self.model.server.server_port}",
            "    apiKeyEnv: NATIVE_MODEL_KEY",
            "    thinking: disabled",
            "    reasoningEffort: off",
            "    maxTokens: 1024",
            "    streamIdleTimeoutMs: 30000",
            "- insert:",
            "    - id: native-integration-driver",
            f"      name: {json.dumps((ROOT / 'tests/integration/native/driver.mjs').as_uri())}",
        ]
        if self.timed_questions:
            lines += ["- id: preset-standard", "  config:", "    id: standard", "    order: 1",
                      "    plugins:", "      - id: tool-ask-user", "        name: '@deepseek-ai/dsh-tool-ask-user'",
                      "        config:", "          mode: timed"]
        self.overlay.write_text("\n".join(lines) + "\n")
        if not self.prepared:
            self._boot()
            installed = self.request("install")
            assert installed["application"] in {"applied", "restart-required"}, (
                "Local native bundle installation failed"
            )
            self.native.close()
            self._boot()
            config = {
                "server": "127.0.0.1",
                "access_port": "18080",
                "mqtt_port": "1883",
                "host_id": self.host,
                "host_token": self.env["WHALEREIN_HOST_PROVISIONING_TOKEN"],
                "state_dir": str(self.directory / "plugin"),
                "adapter": "real",
                "allow_insecure_mqtt": True,
                "log_level": "debug" if self.debug_logging else "silent",
            }
            assert self.request("seed", config=config)["seeded"]
            self.native.close()
            self.prepared = True
        self._boot()
        wait(
            lambda: (
                all(
                    self.request("ready").get(key)
                    for key in ("available", "connected", "source")
                )
                and not self.request("ready")["replayPending"]
            ),
            timeout=30,
            process=self.native,
        )

    def create(self):
        return self.request("create")["sessionId"]

    def wait_idle(self, session, assistant_count=1):
        wait(
            lambda: (
                self.request("events", sessionId=session)["types"].count(
                    "assistant/message"
                )
                >= assistant_count
                and not self.request("state", sessionId=session)["running"]
            ),
            timeout=30,
            process=self.native,
        )

    def tail(self, device, session, baseline=False):
        process = Process(
            [
                str(ROOT / "target/debug/whalerein"),
                "--json",
                "--yes",
                "--data-dir",
                str(device.directory / "client"),
                "tail",
                "--session",
                session,
                "--timeout",
                "12",
            ],
            self.directory,
            {
                "WHALEREIN_SECRET_BACKEND": "file:" + str(device.directory / "secrets"),
                "WHALEREIN_LOG": "error",
            },
        )
        self.processes.append(process)
        wait(lambda: self.request("watches") > 0, process=process)
        if baseline:
            # This case mutates history immediately after starting the watch;
            # wait for the initial snapshot so a late first reset cannot be
            # mistaken for a later reset. The barrier-based watch tests need
            # their baselines to remain in flight and pass baseline=False.
            def baseline_ready():
                try:
                    lines = Path(process.output.name).read_text().splitlines()
                except OSError:
                    return False
                for line in lines:
                    if not line.startswith('{"command"'):
                        continue
                    try:
                        event = json.loads(line).get("data", {}).get("event", {})
                    except ValueError:
                        continue
                    if event.get("type") == "reset":
                        return True
                return False
            wait(baseline_ready, process=process)
        return process

    def preview_ready(self, process):
        # The model stays held until the CLI has observed a real intact preview.
        # Reading a currently appended file may end in a partial JSON line.
        for line in Path(process.output.name).read_text().splitlines():
            if not line.startswith('{"command"'):
                continue
            try:
                event = json.loads(line).get("data", {}).get("event", {})
            except ValueError:
                continue
            if event.get("type") == "preview" and not event.get("incomplete"):
                return True
        return False

    def finish_tail(self, process):
        wait(lambda: process.child.poll() is not None, timeout=15)
        assert process.child.returncode == 0, "Native CLI watch failed"
        return [
            json.loads(line)
            for line in Path(process.output.name).read_text().splitlines()
            if line.startswith('{"command"')
        ]

    def cached(self, device, session):
        with sqlite3.connect(device.directory / "client/cache.sqlite3") as db:
            cursor = db.execute(
                "SELECT cursor FROM conversation_views WHERE session_id=?", (session,)
            ).fetchone()
            ids = tuple(
                row[0]
                for row in db.execute(
                    "SELECT message_id FROM conversation_messages WHERE session_id=? ORDER BY ordinal",
                    (session,),
                )
            )
        return (cursor[0] if cursor else None, ids)

    def web_url(self):
        text = Path(self.native.output.name).read_text()
        matches = re.findall(
            r"https?://(?:localhost|127\.0\.0\.1):3109[^\s\x1b]*", text
        )
        assert matches, "Native Web launch URL unavailable"
        return matches[-1]

    def browser_claim(self, device):
        self.request("workspace")
        paths = {
            name + "File": str(self.directory / (name + ".signal"))
            for name in ("ready", "pending", "checked", "done")
        }
        with concurrent.futures.ThreadPoolExecutor(max_workers=1) as executor:
            browser = executor.submit(self.browser, "claim-owner", **paths)
            try:
                wait(
                    lambda: Path(paths["readyFile"]).exists() or browser.done(),
                    timeout=30,
                )
                if browser.done():
                    browser.result()
                session = json.loads(Path(paths["readyFile"]).read_text())["sessionId"]
                self.model.plan("tool")
                device.send(session, "native browser ownership fixture", delivery="start-if-idle")
                wait(lambda: bool(device.approvals(session)), process=self.native)
                Path(paths["pendingFile"]).touch(mode=0o600)
                wait(
                    lambda: Path(paths["checkedFile"]).exists() or browser.done(),
                    timeout=25,
                )
                if browser.done():
                    browser.result()
                approval = device.approvals(session)[0]
                device.resolve("approve", approval)
                self.wait_idle(session)
            finally:
                Path(paths["doneFile"]).touch(mode=0o600)
            browser.result(timeout=30)
        assert self.request("events", sessionId=session)["decisions"] == [
            "allowed-once"
        ]
        assert self.request("effects")["count"] == 1

    def browser(self, action, **values):
        data = {"action": action, "url": self.web_url(), **values}
        child = subprocess.Popen(
            ["node", str(ROOT / "tests/integration/native/browser.mjs")],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            start_new_session=True,
        )
        try:
            output, _ = child.communicate(json.dumps(data), timeout=90)
        except subprocess.TimeoutExpired:
            raise AssertionError("Native browser deadline exceeded") from None
        finally:
            # Also stop Chromium descendants if the driver timed out or crashed.
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.communicate(timeout=5)
        try:
            reply = json.loads(output)
        except ValueError:
            raise AssertionError("Native browser failed during launch") from None
        assert child.returncode == 0, (
            "Native browser stage failed: "
            + str(reply.get("stage", "unknown"))
            + " "
            + json.dumps(reply.get("diagnostics", {}))
        )
        assert reply.get("ok"), "Native browser assertion failed"
        return reply

    def close(self):
        if self.model is not None:
            self.model.release.set()
        try:
            super().close()
        finally:
            if self.model is not None:
                self.model.close()
