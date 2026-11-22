"""Scheduled network disturbances and public CLI reads for the Watch benchmark."""

import hashlib
import json
import shutil
import subprocess
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

from benchmark import network
from client_environment import CutoverClient
from environment import ROOT
from repo.benchmark import model


def milliseconds(value_ns):
    return round(value_ns / 1_000_000, 3)


def wait_until(target_ns):
    delay = (target_ns - time.monotonic_ns()) / 1e9
    if delay > 0:
        time.sleep(delay)


def blackout(start_ns, duration_ms, settings):
    """Force the observed transport down and restore public endpoint traffic."""
    wait_until(start_ns)
    network.block_client()
    started = time.monotonic_ns()
    try:
        network.disconnect_watch()
        wait_until(started + duration_ms * 1_000_000)
    finally:
        network.restore_client(settings)
    return started, time.monotonic_ns()


def recovery_observation(attempt, started_ns, restored_ns, source_start_ns, sizes):
    started = milliseconds(started_ns - source_start_ns)
    restored = milliseconds(restored_ns - source_start_ns)
    missed_end = sum(
        size for emitted, size in zip(attempt.source_times_ms, sizes)
        if emitted <= restored
    )
    matched = next(
        (event["at_ms"] for event in attempt.outputs
         if event["at_ms"] >= restored and event["prefix_bytes"] >= missed_end),
        None,
    )
    return {
        "started_ms": started,
        "restored_ms": restored,
        "catch_up_ms": None if matched is None else round(matched - restored, 3),
    }


def text_of(event):
    return "".join(
        block.get("text") or ""
        for block in event.get("blocks", [])
        if block.get("type") == "text"
    )


@dataclass(frozen=True)
class HistoryLoad:
    reader: CutoverClient
    host: str
    session: str
    expected: list[str]

    @property
    def expected_bytes(self):
        return sum(len(text.encode()) for text in self.expected)

    def read(self, scheduled_ns, deadline_ms):
        wait_until(scheduled_ns)
        started = time.monotonic_ns()
        error_code = None
        measured_bytes = 0
        try:
            proc = subprocess.run(
                self.reader.args(
                    "session", "history", self.session, "--host", self.host,
                    "--max-messages", str(len(self.expected)),
                ),
                cwd=ROOT, env=self.reader.env, capture_output=True, text=True,
                timeout=deadline_ms / 1000, check=False,
            )
            response = self.reader.envelope(proc.stdout)
            if proc.returncode:
                outcome = "product_error"
                code = (response.get("error") or {}).get("code")
                if model.valid_error_code(code):
                    error_code = code
            else:
                data = response.get("data") or {}
                target = {"host_id": self.host, "session_id": self.session}
                texts = [text_of(message) for message in data.get("messages", [])]
                correct = (
                    response.get("command") == "session.history"
                    and data.get("target") == target
                    and data.get("source") == "online"
                    and texts == self.expected
                )
                if correct:
                    measured_bytes = sum(len(text.encode()) for text in texts)
                outcome = "completed" if correct else "incorrect_content"
        except subprocess.TimeoutExpired:
            outcome = "timeout"
        except (AssertionError, json.JSONDecodeError, OSError, TypeError, AttributeError):
            outcome = "product_error"
        duration = milliseconds(time.monotonic_ns() - started)
        if outcome == "completed" and duration > deadline_ms:
            outcome = "timeout"
        if outcome != "completed":
            measured_bytes = 0
        observation = {
            "scheduled_ns": scheduled_ns,
            "started_ns": started,
            "duration_ms": duration,
            "outcome": outcome,
            "bytes": measured_bytes,
            "expected_bytes": self.expected_bytes,
        }
        if error_code is not None:
            observation["error_code"] = error_code
        return observation


def prepare_history(scenario, reader, specification):
    session = scenario.dsh.create_session()
    expected = model.history_texts(specification)
    scenario.dsh.process.control("bench_seed_history", session_id=session, texts=expected)
    return HistoryLoad(reader, scenario.host, session, expected)


@dataclass(frozen=True)
class AttachmentLoad:
    reader: CutoverClient
    host: str
    session: str
    reference: str
    expected_bytes: int
    expected_sha256: str
    name: str

    def read(self, scheduled_ns, deadline_ms):
        wait_until(scheduled_ns)
        started = time.monotonic_ns()
        directory = Path(tempfile.mkdtemp(prefix="whalerein-bench-attachment-"))
        error_code = None
        saved_bytes = 0
        try:
            try:
                proc = subprocess.run(
                    self.reader.args(
                        "session", "download", self.reference,
                        "--to", str(directory),
                        "--session", self.session,
                        "--host", self.host,
                        "--name", self.name,
                    ),
                    cwd=ROOT, env=self.reader.env, capture_output=True, text=True,
                    timeout=deadline_ms / 1000, check=False,
                )
                response = self.reader.envelope(proc.stdout)
                if proc.returncode:
                    outcome = "product_error"
                    code = (response.get("error") or {}).get("code")
                    if model.valid_error_code(code):
                        error_code = code
                else:
                    data = response.get("data") or {}
                    path = Path(data.get("path")) if isinstance(data.get("path"), str) else None
                    actual = (
                        path.read_bytes()
                        if path is not None and path.parent == directory and path.is_file()
                        else b""
                    )
                    saved_bytes = len(actual)
                    correct = (
                        response.get("command") == "session.download"
                        and data.get("verified") is True
                        and data.get("bytes") == self.expected_bytes
                        and data.get("sha256") == self.expected_sha256
                        and hashlib.sha256(actual).hexdigest() == self.expected_sha256
                        and saved_bytes == self.expected_bytes
                        and data.get("reference") == self.reference
                    )
                    outcome = "completed" if correct else "incorrect_content"
            except subprocess.TimeoutExpired:
                outcome = "timeout"
            except (
                AssertionError,
                json.JSONDecodeError,
                OSError,
                TypeError,
                AttributeError,
                KeyError,
                ValueError,
            ):
                outcome = "product_error"
            duration = milliseconds(time.monotonic_ns() - started)
            if outcome == "completed" and duration > deadline_ms:
                outcome = "timeout"
            observation = {
                "scheduled_ns": scheduled_ns,
                "started_ns": started,
                "duration_ms": duration,
                "outcome": outcome,
                "bytes": saved_bytes if outcome == "completed" else 0,
                "expected_bytes": self.expected_bytes,
            }
            if error_code is not None:
                observation["error_code"] = error_code
            return observation
        finally:
            shutil.rmtree(directory, ignore_errors=True)


def prepare_attachment(scenario, reader, specification):
    """Commit one fixture attachment and resolve its public occurrence reference."""
    session = scenario.dsh.create_session()
    name = specification["name"]
    stored = scenario.dsh.save_attachment(
        kind="file",
        size=specification["bytes"],
        seed=specification["seed"],
        media_type=specification["media_type"],
        name=name,
    )
    scenario.dsh.commit_attachment(
        session, stored["attachment"], kind="file", role="tool",
        text="benchmark attachment",
    )
    history = reader.call("session", "history", session, "--host", scenario.host)
    rows = [row for row in history.get("attachments", []) if row.get("name") == name]
    if len(rows) != 1:
        raise RuntimeError("benchmark attachment is not discoverable through history")
    return AttachmentLoad(
        reader=reader,
        host=scenario.host,
        session=session,
        reference=rows[0]["reference"],
        expected_bytes=specification["bytes"],
        expected_sha256=stored["sha256"],
        name=name,
    )
