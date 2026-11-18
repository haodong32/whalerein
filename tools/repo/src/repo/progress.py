"""Safe progress events and shared Rich presentation, independent of results."""

import json
import os
import re
import sys
import threading
import time

from rich.console import Console
from rich.progress import Progress, SpinnerColumn, TextColumn, TimeElapsedColumn
from rich.table import Column, Table

PREFIX = "WHALEREIN_PROGRESS "


class Display:
    def __init__(self, verbose=False, stream=None):
        self.verbose = verbose
        self.console = Console(
            stderr=stream is None,
            file=stream,
            markup=False,
            highlight=False,
            force_terminal=(
                False
                if os.environ.get("CI") or "NO_COLOR" in os.environ
                else True
                if os.environ.get("WHALEREIN_PROGRESS_TTY") == "1"
                else None
            ),
        )
        self.started = time.monotonic()
        self.report = None
        self.exit_code = None
        self.finished = 0
        self.failed = 0
        self.count_cases = True
        self.current = "Preparing tests"
        self.active = {}
        self.started_cases = {}
        self.scope = "tests"
        self.detail = ""
        self.total = None
        self.scope_finished = 0
        self.last_event = time.monotonic()
        self.last_print = self.last_event
        self.stop = threading.Event()
        self.progress = Progress(
            SpinnerColumn(),
            TextColumn("{task.fields[count]}"),
            TimeElapsedColumn(),
            TextColumn(
                "{task.description}",
                table_column=Column(ratio=1, no_wrap=True, overflow="ellipsis"),
            ),
            console=self.console,
            expand=True,
            disable=not self.console.is_terminal or verbose,
        )
        self.task = self.progress.add_task(
            "Preparing tests", total=None, count="0 finished · 0 failed"
        )
        self.progress.start()
        self.heartbeat = threading.Thread(target=self._heartbeat, daemon=True)
        self.heartbeat.start()

    def event(self, event):
        self.last_event = time.monotonic()
        kind = event["event"]
        self.count_cases = event.get("count_cases", self.count_cases)
        if kind in ("start", "finish") and not self.count_cases:
            return
        description = event.get("description") or event.get("id") or "Tests"
        if kind == "start":
            self.started_cases[event.get("id", description)] = description
            self.active[event.get("id", description)] = time.monotonic()
        if kind == "phase":
            self.scope = description
            self.total = event.get("total")
            self.scope_finished = 0
        if kind in ("phase", "start"):
            self.current = description
            self.detail = ""
            self.progress.update(self.task, description=description)
            if self.verbose:
                if kind == "phase":
                    self.console.print(description, style="bold cyan")
                else:
                    self.line("RUN", description, "cyan")
        elif kind == "finish":
            status = event.get("status", "interrupted")
            self.finished += 1
            self.scope_finished += 1
            self.failed += status == "failed"
            if self.verbose or status in ("failed", "blocked", "interrupted"):
                label = {
                    "passed": "PASS",
                    "failed": "FAIL",
                    "skipped": "SKIP",
                    "not_run": "NOT RUN",
                    "cancelled": "INTERRUPTED",
                }.get(status, status.upper())
                began = self.active.pop(event.get("id", description), None)
                elapsed = f" · {time.monotonic() - began:.2f}s" if began is not None else ""
                self.line(
                    label,
                    f"{description}{elapsed}",
                    "green" if status == "passed" else "red" if status == "failed" else "yellow",
                )
            self.progress.update(self.task, description=self.scope)
            if status == "failed":
                self.console.print(f"      Phase  {event.get('phase', 'test')}")
                self.console.print(
                    "      Reason Assertion or fixture failed; see report diagnostics"
                )
                self.console.print(f"      Case   {event.get('id', description)}", style="dim")
        elif kind == "unit_finish":
            status = "FAIL" if event.get("failed") or event.get("status") == "failed" else "PASS"
            parts = [f"{event.get('passed', 0)} passed"]
            if event.get("failed"):
                parts.append(f"{event['failed']} failed")
            if event.get("skipped"):
                parts.append(f"{event['skipped']} skipped")
            if status == "FAIL" and not event.get("failed"):
                parts.append("execution incomplete")
            text = description + " · " + " · ".join(parts)
            self.line(status, text, "red" if status == "FAIL" else "green")
        elif kind == "detail":
            if "total" in event:
                self.total = event["total"]
                self.scope_finished = 0
            self.detail = description
            self.progress.update(self.task, description=f"{self.current} · {description}")
            if self.verbose:
                self.console.print(f"      {description}", style="dim")
        elif kind == "waiting":
            self.console.print(f"WAIT  {description}", style="dim")
        count = (
            f"{self.scope_finished}/{self.total}"
            if self.total is not None
            else str(self.scope_finished)
        )
        self.progress.update(self.task, count=f"{count} finished · {self.failed} failed")

    def line(self, label, text, style):
        row = Table.grid(padding=(0, 1))
        row.add_column(width=11, no_wrap=True, style=style)
        row.add_column(ratio=1)
        row.add_row(label, text)
        self.console.print(row)

    def _heartbeat(self):
        while not self.stop.wait(10):
            if time.monotonic() - self.last_event >= 10:
                if not self.console.is_terminal or self.verbose:
                    self.line(
                        "WAIT",
                        (
                            f"{self.scope} · {self.detail or self.current} · "
                            f"{time.monotonic() - self.started:.0f}s"
                        ),
                        "dim",
                    )

    def result(self, document, exit_code):
        self.report = document
        self.exit_code = exit_code

    def close(self):
        self.stop.set()
        self.heartbeat.join(timeout=1)
        self.progress.stop()
        table = Table(
            "Suite",
            "Pass",
            "Fail",
            "Other",
            title="Test execution "
            + (
                "PASS"
                if self.exit_code == 0
                else "INTERRUPTED"
                if self.exit_code in (130, 143)
                else "INVALID REQUEST"
                if self.exit_code == 2
                else "BLOCKED"
                if self.exit_code == 3
                else "FAIL"
                if self.exit_code is not None
                else "progress"
            ),
        )
        if self.report:
            if self.verbose:
                for scope in self.report["scopes"]:
                    for case in scope["tests"]:
                        if case["status"] in ("not_run", "interrupted"):
                            label = "NOT RUN" if case["status"] == "not_run" else "INTERRUPTED"
                            self.console.print(
                                f"{label:<11} {case.get('description') or case['id']}",
                                style="yellow",
                            )
            for category in ("unit", "integration"):
                cases = [
                    case
                    for scope in self.report["scopes"]
                    if scope.get("kind", "unit") == category
                    for case in scope["tests"]
                    if case.get("node_role", "case") == "case"
                ]
                if cases:
                    passed = sum(c["status"] == "passed" for c in cases)
                    failed = sum(c["status"] == "failed" for c in cases)
                    table.add_row(
                        category, str(passed), str(failed), str(len(cases) - passed - failed)
                    )
            failed_coverage = sum(
                not c.get("ok", False)
                for scope in self.report["scopes"]
                for c in scope.get("coverage", [])
            )
            if failed_coverage:
                table.add_row("Coverage", "", str(failed_coverage), "packages below gate")
        else:
            table.add_row("Observed", str(self.finished - self.failed), str(self.failed), "")
        table.caption = f"Elapsed {time.monotonic() - self.started:.1f}s"
        self.console.print(table)


def emit(event, stream=None):
    print("\n" + PREFIX + json.dumps(event), file=stream or sys.stdout, flush=True)


def parse(line):
    if line.startswith(PREFIX):
        try:
            value = json.loads(line[len(PREFIX) :])
            if isinstance(value, dict) and value.get("event") in {
                "phase",
                "start",
                "finish",
                "waiting",
                "detail",
                "unit_finish",
            }:
                return value
            return None
        except ValueError:
            return None
    try:
        value = json.loads(line)
    except ValueError:
        match = re.fullmatch(r"test (.+) \.\.\. (ok|FAILED|ignored)(?:, .*)?", line)
        if match:
            return {
                "event": "finish",
                "id": match[1],
                "status": {"ok": "passed", "FAILED": "failed", "ignored": "skipped"}[match[2]],
            }
        return None
    if not isinstance(value, dict):
        return None
    action = value.get("Action")
    if value.get("Test") and action in ("run", "pass", "fail", "skip"):
        return {
            "event": "start" if action == "run" else "finish",
            "id": value.get("Package", "") + "::" + value["Test"],
            "status": {"pass": "passed", "fail": "failed", "skip": "skipped"}.get(action),
        }
    if value.get("name") == value.get("file") and value.get("status") == "passed":
        return None
    if value.get("type") == "suite":
        return None
    if value.get("status") in ("passed", "failed", "skipped", "cancelled") and value.get("name"):
        return {"event": "finish", "description": value["name"], "status": value["status"]}
    return None


def rust_start(partial):
    """Recognize libtest's unfinished pretty-format status line, not captured output."""
    match = re.fullmatch(r"test ([A-Za-z0-9_:]+) \.\.\. ", partial)
    if not match:
        return None
    return {"event": "start", "id": match[1], "description": match[1]}
