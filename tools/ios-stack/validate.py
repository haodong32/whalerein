#!/usr/bin/env python3
"""End-to-end validation for the disposable iOS loopback stack.

The validator starts a fresh stack, pairs the repository CLI through the real
Access/FakeDSH path, exercises history, idle/steer/queue sends, approval
resolution, stop, Access/broker recovery, and verified teardown. It writes
redacted evidence under `.local-docs/acceptance/add-ios-client/`.

Usage (from the repository root, after building the CLI):

    cargo build -p whalerein-cli
    python3 tools/ios-stack/validate.py
"""

from __future__ import annotations

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "tools" / "ios-stack" / "ios_stack.py"
EVIDENCE = ROOT / ".local-docs" / "acceptance" / "add-ios-client"
STATE = ROOT / ".data" / "ios-stack" / "current" / "state.json"
MESSAGE = "fixture validation message"
CLI_PATH = ROOT / "target" / "debug" / "whalerein"


def run(argv: list[str], *, env: dict[str, str] | None = None, timeout: float = 120.0):
    return subprocess.run(
        argv,
        cwd=str(ROOT),
        env=env,
        capture_output=True,
        text=True,
        timeout=timeout,
        check=False,
    )


def cli_envelope(output: str) -> dict:
    values = []
    for line in output.splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            values.append(json.loads(line))
        except ValueError:
            continue
    return values[-1] if values else {}


class Recorder:
    def __init__(self) -> None:
        self.steps: list[dict] = []
        self.cli_output: list[str] = []
        self.failures = 0

    def add(self, name: str, ok: bool, command: str, observed) -> None:
        if not ok:
            self.failures += 1
        self.steps.append(
            {"name": name, "ok": ok, "command": command, "observed": observed}
        )

    def finish(self) -> int:
        return 0 if self.failures == 0 else 1


def stack(*args: str, timeout: float = 300.0):
    return run([sys.executable, str(TOOL), *args], timeout=timeout)


def cli_call(
    recorder: Recorder,
    name: str,
    args: list[str],
    env: dict[str, str],
    *,
    display: str,
    check,
):
    completed = run(
        [str(CLI_PATH), "--json", "--yes", *args],
        env=env,
        timeout=120,
    )
    redacted = re.sub(r"whalerein://pair\?[^\s]+", "<redacted-invitation>", completed.stdout)
    recorder.cli_output.append(
        f"$ {display}\n{redacted.rstrip()}\n(exit {completed.returncode})\n"
    )
    envelope = cli_envelope(completed.stdout)
    observed = {"exit": completed.returncode}
    if envelope.get("error"):
        observed["error"] = envelope["error"].get("code")
    if envelope.get("data") is not None:
        observed["data"] = check(envelope.get("data") or {})
    ok = completed.returncode == 0 and not envelope.get("error")
    if ok:
        try:
            ok = bool(check(envelope.get("data") or {}, validate=True))
        except TypeError:
            pass
    recorder.add(name, ok, display, observed)
    return envelope.get("data") or {}


def main() -> int:
    parser = argparse.ArgumentParser(description="Validate the iOS loopback stack")
    parser.add_argument("--cli", default=str(ROOT / "target" / "debug" / "whalerein"))
    parser.add_argument("--keep", action="store_true", help="leave the stack running")
    parser.add_argument("--timeout", type=float, default=300.0)
    args = parser.parse_args()

    global CLI_PATH
    CLI_PATH = Path(args.cli).resolve()
    if not CLI_PATH.exists():
        sys.stderr.write(
            f"missing CLI at {CLI_PATH}; run `cargo build -p whalerein-cli` first\n"
        )
        return 2

    recorder = Recorder()
    version = run([str(CLI_PATH), "--version"], timeout=15)
    recorder.cli_output.append(
        f"$ {CLI_PATH} --version\n{version.stdout.strip()}\n(exit {version.returncode})\n"
    )
    control_checked = False
    try:
        up = stack("up", "--fresh", "--timeout", str(int(args.timeout)), timeout=args.timeout + 60)
        up_ok = up.returncode == 0 and STATE.exists()
        recorder.add(
            "up",
            up_ok,
            "ios_stack.py up --fresh",
            {"exit": up.returncode, "state_exists": STATE.exists()},
        )
        if not up_ok:
            sys.stderr.write(up.stderr[-2000:])
            return recorder.finish()

        contract = json.loads(STATE.read_text(encoding="utf-8"))
        cli_env = {
            **os.environ,
            **contract["cli"]["env"],
            "WHALEREIN_LOG": "error",
        }

        invitation = json.loads(
            stack("invitation", "--refresh", "--json").stdout
        )["invitation_uri"]
        cli_call(
            recorder,
            "pair",
            ["pair", invitation],
            cli_env,
            display="whalerein pair <invitation>",
            check=lambda data, validate=False: (
                data.get("host_id") == contract["host_id"] if validate else {
                    "host_id": data.get("host_id"),
                    "fingerprint_present": bool(data.get("fingerprint")),
                }
            ),
        )

        cli_call(
            recorder,
            "list hosts",
            ["hosts"],
            cli_env,
            display="whalerein hosts",
            check=lambda data, validate=False: (
                any(
                    host["host_id"] == contract["host_id"] and host["online"]
                    for host in data.get("hosts", [])
                )
                if validate
                else {
                    "hosts": [
                        {"host_id": host["host_id"], "online": host["online"]}
                        for host in data.get("hosts", [])
                    ]
                }
            ),
        )

        cli_call(
            recorder,
            "list sessions",
            ["sessions", "--host", contract["host_id"]],
            cli_env,
            display="whalerein sessions --host <host-id>",
            check=lambda data, validate=False: (
                any(
                    session["session_id"] == contract["session_id"]
                    for session in data.get("sessions", [])
                )
                if validate
                else {
                    "sessions": [
                        {"session_id": session["session_id"], "running": session["running"]}
                        for session in data.get("sessions", [])
                    ]
                }
            ),
        )

        cli_call(
            recorder,
            "sync initial history",
            ["sync", "--session", contract["session_id"]],
            cli_env,
            display="whalerein sync --session <session-id>",
            check=lambda data, validate=False: (
                data.get("events") == contract["expected_message_count"]
                if validate
                else {"events": data.get("events")}
            ),
        )

        cli_call(
            recorder,
            "send while idle",
            [
                "send",
                "--session",
                contract["session_id"],
                "--delivery",
                "start-if-idle",
                MESSAGE,
            ],
            cli_env,
            display="whalerein send --delivery start-if-idle <fixture-message>",
            check=lambda data, validate=False: (
                data.get("status") == "succeeded"
                if validate
                else {"status": data.get("status")}
            ),
        )
        time.sleep(0.4)

        stream = stack("seed-event", "stream")
        recorder.add(
            "fixture stream",
            stream.returncode == 0,
            "ios_stack.py seed-event stream",
            {
                "exit": stream.returncode,
                "appended_message": json.loads(stream.stdout).get("appended_message")
                if stream.returncode == 0
                else None,
            },
        )
        time.sleep(0.4)
        cli_call(
            recorder,
            "sync streamed output",
            ["sync", "--session", contract["session_id"]],
            cli_env,
            display="whalerein sync --session <session-id>",
            check=lambda data, validate=False: (
                data.get("events", 0) >= 1
                if validate
                else {"events": data.get("events")}
            ),
        )

        running = stack("seed-event", "running")
        recorder.add(
            "fixture running",
            running.returncode == 0,
            "ios_stack.py seed-event running",
            {"exit": running.returncode},
        )
        for label, delivery in (("send steer", "steer"), ("send queue", "enqueue-after-run")):
            cli_call(
                recorder,
                label,
                [
                    "send",
                    "--session",
                    contract["session_id"],
                    "--delivery",
                    delivery,
                    MESSAGE,
                ],
                cli_env,
                display=f"whalerein send --delivery {delivery} <fixture-message>",
                check=lambda data, validate=False: (
                    data.get("status") == "succeeded"
                    if validate
                    else {"status": data.get("status")}
                ),
            )

        approval = stack("seed-event", "approval")
        recorder.add(
            "fixture approval",
            approval.returncode == 0,
            "ios_stack.py seed-event approval",
            {
                "exit": approval.returncode,
                "approval_id_present": bool(
                    json.loads(approval.stdout).get("approval", {}).get("approval_id")
                    if approval.returncode == 0
                    else False
                ),
            },
        )
        time.sleep(0.4)

        approvals = cli_call(
            recorder,
            "list approvals",
            ["approvals", "--session", contract["session_id"]],
            cli_env,
            display="whalerein approvals --session <session-id>",
            check=lambda data, validate=False: (
                len(data.get("approvals", [])) == 1
                if validate
                else {
                    "approvals": [
                        {
                            "approval_id": item["approval_id"],
                            "version": item["version"],
                            "tool_name": item["tool_name"],
                            "state": item["state"],
                        }
                        for item in data.get("approvals", [])
                    ]
                }
            ),
        )
        pending = approvals["approvals"][0]
        cli_call(
            recorder,
            "resolve approval",
            [
                "approve",
                "--request",
                pending["approval_id"],
                "--digest",
                pending["digest"],
                "--version",
                str(pending["version"]),
            ],
            cli_env,
            display="whalerein approve --request <id> --digest <hex> --version <n>",
            check=lambda data, validate=False: (
                data.get("status") == "succeeded"
                if validate
                else {"status": data.get("status")}
            ),
        )

        cli_call(
            recorder,
            "stop session",
            ["interrupt", "--session", contract["session_id"]],
            cli_env,
            display="whalerein interrupt --session <session-id>",
            check=lambda data, validate=False: (
                data.get("status") == "succeeded"
                if validate
                else {"status": data.get("status")}
            ),
        )
        time.sleep(0.4)
        status = json.loads(stack("status", "--json").stdout)
        recorder.add(
            "fixture stopped",
            status.get("ready") is True
            and status.get("fixture", {}).get("running") is False,
            "ios_stack.py status --json",
            {
                "ready": status.get("ready"),
                "session_running": status.get("fixture", {}).get("running"),
            },
        )

        control_url = contract["control_url"]
        control_token = contract["control_token"]
        unauthorized = 0
        try:
            urllib.request.urlopen(control_url + "/v1/status", timeout=3)
        except urllib.error.HTTPError as error:
            unauthorized = error.code
        request = urllib.request.Request(
            control_url + "/v1/status",
            headers={"Authorization": "Bearer " + control_token},
        )
        with urllib.request.urlopen(request, timeout=3) as response:
            control_status = json.load(response)
        recorder.add(
            "loopback control API",
            unauthorized == 401 and control_status.get("ok") is True,
            "GET <control-url>/v1/status with bearer token",
            {"unauthorized_status": unauthorized, "ok": control_status.get("ok")},
        )
        control_checked = True

        for target in ("access", "broker"):
            restarted = stack("restart", target, timeout=180)
            recorder.add(
                f"restart {target}",
                restarted.returncode == 0,
                f"ios_stack.py restart {target}",
                {"exit": restarted.returncode},
            )
            time.sleep(1.0)
            cli_call(
                recorder,
                f"list hosts after {target} restart",
                ["hosts"],
                cli_env,
                display="whalerein hosts",
                check=lambda data, validate=False: (
                    any(
                        host["host_id"] == contract["host_id"] and host["online"]
                        for host in data.get("hosts", [])
                    )
                    if validate
                    else {"host_count": len(data.get("hosts", []))}
                ),
            )
            time.sleep(0.5)

        cli_call(
            recorder,
            "send after recovery",
            [
                "send",
                "--session",
                contract["session_id"],
                "--delivery",
                "start-if-idle",
                MESSAGE,
            ],
            cli_env,
            display="whalerein send --delivery start-if-idle <fixture-message>",
            check=lambda data, validate=False: (
                data.get("status") == "succeeded"
                if validate
                else {"status": data.get("status")}
            ),
        )

        EVIDENCE.mkdir(parents=True, exist_ok=True)
        shutil.copy2(
            ROOT / ".data" / "ios-stack" / "current" / "supervisor.log",
            EVIDENCE / "ios-stack-supervisor.log",
        )
        for name in ("access.log", "plugin.log", "dsh.log"):
            source = ROOT / ".data" / "ios-stack" / "current" / name
            if source.exists():
                shutil.copy2(source, EVIDENCE / f"ios-stack-{name}")
        redacted = {
            key: value
            for key, value in contract.items()
            if key not in ("invitation_uri", "control_token")
        }
        (EVIDENCE / "ios-stack-state.redacted.json").write_text(
            json.dumps(redacted, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )

    finally:
        down = None
        if not args.keep:
            down = stack("down", timeout=120)
            if down.returncode == 0:
                payload = json.loads(down.stdout)
                recorder.add(
                    "down",
                    payload.get("cleaned") is True and not payload.get("remaining"),
                    "ios_stack.py down",
                    {
                        "cleaned": payload.get("cleaned"),
                        "remaining": payload.get("remaining"),
                    },
                )
            else:
                recorder.add(
                    "down",
                    False,
                    "ios_stack.py down",
                    {"exit": down.returncode},
                )
        EVIDENCE.mkdir(parents=True, exist_ok=True)
        (EVIDENCE / "ios-stack-validation.json").write_text(
            json.dumps(
                {
                    "schema": "whalerein/ios-stack-validation/v1",
                    "control_checked": control_checked,
                    "passed": recorder.finish() == 0,
                    "steps": recorder.steps,
                },
                indent=2,
                sort_keys=True,
            )
            + "\n",
            encoding="utf-8",
        )
        lines = [
            "# iOS loopback stack validation",
            "",
            "Generated by `python3 tools/ios-stack/validate.py`.",
            "Message text, invitation tokens, device tokens, digests, and control",
            "tokens are intentionally not recorded.",
            "",
            "| Step | Result | Command | Observed |",
            "|---|---|---|---|",
        ]
        for step in recorder.steps:
            observed = json.dumps(step["observed"], sort_keys=True)
            observed = observed.replace("|", "\\|")
            lines.append(
                f"| {step['name']} | {'PASS' if step['ok'] else 'FAIL'} | "
                f"`{step['command']}` | `{observed}` |"
            )
        lines += [
            "",
            f"Overall: {'PASS' if recorder.finish() == 0 else 'FAIL'}",
            "",
            "Raw state (redacted): `ios-stack-state.redacted.json`.",
            "Component logs: `ios-stack-supervisor.log`, `ios-stack-access.log`,",
            "`ios-stack-plugin.log`, `ios-stack-dsh.log`.",
            "",
        ]
        (EVIDENCE / "ios-stack-validation.md").write_text(
            "\n".join(lines), encoding="utf-8"
        )
        (EVIDENCE / "ios-stack-cli-output.log").write_text(
            "\n".join(recorder.cli_output), encoding="utf-8"
        )

    print(
        json.dumps(
            {
                "passed": recorder.finish() == 0,
                "steps": len(recorder.steps),
                "failures": recorder.failures,
                "evidence": str(EVIDENCE),
            },
            indent=2,
        )
    )
    return recorder.finish()


if __name__ == "__main__":
    sys.exit(main())
