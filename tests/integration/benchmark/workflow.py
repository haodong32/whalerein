"""Scheduled network disturbances and public CLI reads for the Watch benchmark.

Reads emit raw facts only: pages, logical and physical bytes, retries, verified
content and timing. Business classification of an attempt belongs to the
model. The history reader follows every continuation under one deadline and
verifies the full logical sequence; the attachment reader verifies length and
digest and records only bounded facts.
"""

import hashlib
import json
import shutil
import subprocess
import tempfile
import time
from dataclasses import dataclass
from pathlib import Path

from client_environment import CutoverClient
from environment import ROOT
from repo.benchmark import model

from benchmark import history, network, source


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


def recovery_observation(
    attempt, started_ns, restored_ns, origin_ns, outputs, sizes, deadline_ms
):
    """Keep the blackout separate; require a visible correct catch-up prefix."""
    times_ms = attempt["_source_times_ms"]
    return source.recovery_facts(
        blackout_start_ns=started_ns,
        restored_ns=restored_ns,
        origin_ns=origin_ns,
        outputs=outputs,
        times_ms=times_ms,
        sizes=sizes,
        deadline_ms=deadline_ms,
        timeout=outputs == [],
    )


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
    page_size: int = 3

    @property
    def expected_bytes(self):
        return sum(len(text.encode()) for text in self.expected)

    def read(self, scheduled_ns, deadline_ms, *, environment=None, on_spawn=None):
        wait_until(scheduled_ns)
        started_ns = time.monotonic_ns()

        def invoke(argv, timeout_ms):
            process = subprocess.Popen(
                argv,
                cwd=ROOT,
                env={**self.reader.env, **(environment or {})},
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                text=True,
            )
            if on_spawn is not None:
                on_spawn(process.pid)
            try:
                stdout, _ = process.communicate(timeout=timeout_ms / 1000)
            except subprocess.TimeoutExpired as error:
                process.kill()
                process.communicate()
                raise history.PageTimeout from error
            return process.returncode, stdout

        def fetch(page_size, continuation, timeout_ms):
            argv = self.reader.args(
                "session",
                "history",
                self.session,
                "--host",
                self.host,
                "--max-messages",
                str(page_size),
            )
            if continuation:
                argv.extend(["--continuation", continuation])
            returncode, stdout = invoke(argv, timeout_ms)
            response = self.reader.envelope(stdout)
            if returncode:
                code = (response.get("error") or {}).get("code")
                return {
                    "messages": [],
                    "continuation": None,
                    "complete": True,
                    "physical_bytes": len(stdout.encode()),
                    "error_code": code if model.valid_error_code(code) else "unknown",
                }
            data = response.get("data") or {}
            target = {"host_id": self.host, "session_id": self.session}
            if (
                response.get("command") != "session.history"
                or data.get("target") != target
                or data.get("source") != "online"
            ):
                return {
                    "messages": [],
                    "continuation": None,
                    "complete": True,
                    "physical_bytes": len(stdout.encode()),
                    "error_code": "invalid_envelope",
                }
            return {
                "messages": [text_of(message) for message in data.get("messages", [])],
                "continuation": data.get("continuation"),
                "complete": data.get("complete") is True,
                "physical_bytes": len(stdout.encode()),
            }

        facts = history.walk(
            fetch,
            expected=self.expected,
            page_size=self.page_size,
            deadline_ms=deadline_ms,
            clock_ms=lambda: time.monotonic_ns() / 1_000_000,
            text_of=lambda text: text,
        )
        finished_ns = time.monotonic_ns()
        facts["scheduled_ns"] = scheduled_ns
        facts["started_ns"] = started_ns
        facts["finished_ns"] = finished_ns
        return facts


def prepare_history(scenario, reader, specification):
    session = scenario.dsh.create_session()
    expected = model.history_texts(specification)
    scenario.dsh.process.control(
        "bench_seed_history", session_id=session, texts=expected
    )
    page_size = max(
        1, min(specification["messages"], specification.get("page_messages", 3))
    )
    return HistoryLoad(reader, scenario.host, session, expected, page_size=page_size)


@dataclass(frozen=True)
class AttachmentLoad:
    reader: CutoverClient
    host: str
    session: str
    reference: str
    expected_bytes: int
    expected_sha256: str
    name: str

    def read(self, scheduled_ns, deadline_ms, *, environment=None, on_spawn=None):
        wait_until(scheduled_ns)
        started_ns = time.monotonic_ns()
        directory = Path(tempfile.mkdtemp(prefix="whalerein-bench-attachment-"))
        try:
            try:
                process = subprocess.Popen(
                    self.reader.args(
                        "session",
                        "download",
                        self.reference,
                        "--to",
                        str(directory),
                        "--session",
                        self.session,
                        "--host",
                        self.host,
                        "--name",
                        self.name,
                    ),
                    cwd=ROOT,
                    env={**self.reader.env, **(environment or {})},
                    stdout=subprocess.PIPE,
                    stderr=subprocess.DEVNULL,
                    text=True,
                )
                if on_spawn is not None:
                    on_spawn(process.pid)
                try:
                    stdout, _ = process.communicate(timeout=deadline_ms / 1000)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.communicate()
                    raise
                response = self.reader.envelope(stdout)
                if process.returncode:
                    code = (response.get("error") or {}).get("code")
                    observation = {
                        "outcome": "product_error",
                        "error_code": code if model.valid_error_code(code) else None,
                        "bytes_expected": self.expected_bytes,
                        "bytes_verified": 0,
                        "physical_bytes": 0,
                        "route": "reference",
                        "verified": False,
                        "token_repeats": 0,
                        "corrupt": False,
                        "length_match": False,
                        "digest_match": False,
                    }
                else:
                    data = response.get("data") or {}
                    path = (
                        Path(data.get("path"))
                        if isinstance(data.get("path"), str)
                        else None
                    )
                    actual = (
                        path.read_bytes()
                        if path is not None
                        and path.parent == directory
                        and path.is_file()
                        else b""
                    )
                    observation = history.attachment_facts(
                        saved_bytes=len(actual),
                        expected_bytes=self.expected_bytes,
                        expected_sha256=self.expected_sha256,
                        actual_sha256=hashlib.sha256(actual).hexdigest(),
                    )
                    if (
                        data.get("verified") is not True
                        or data.get("bytes") != self.expected_bytes
                        or data.get("reference") != self.reference
                    ):
                        observation["outcome"] = "incorrect_content"
                        observation["verified"] = False
                        observation["corrupt"] = True
            except subprocess.TimeoutExpired:
                observation = {
                    "outcome": "timeout",
                    "bytes_expected": self.expected_bytes,
                    "bytes_verified": 0,
                    "physical_bytes": 0,
                    "route": "reference",
                    "verified": False,
                    "token_repeats": 0,
                    "corrupt": False,
                    "length_match": False,
                    "digest_match": False,
                }
            except (
                AssertionError,
                json.JSONDecodeError,
                OSError,
                TypeError,
                AttributeError,
                KeyError,
                ValueError,
            ):
                observation = {
                    "outcome": "product_error",
                    "bytes_expected": self.expected_bytes,
                    "bytes_verified": 0,
                    "physical_bytes": 0,
                    "route": "reference",
                    "verified": False,
                    "token_repeats": 0,
                    "corrupt": False,
                    "length_match": False,
                    "digest_match": False,
                }
            finished_ns = time.monotonic_ns()
            observation["scheduled_ns"] = scheduled_ns
            observation["started_ns"] = started_ns
            observation["finished_ns"] = finished_ns
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
        session,
        stored["attachment"],
        kind="file",
        role="tool",
        text="benchmark attachment",
    )
    rows = reader.call("session", "history", session, "--host", scenario.host)
    matches = [row for row in rows.get("attachments", []) if row.get("name") == name]
    if len(matches) != 1:
        raise RuntimeError("benchmark attachment is not discoverable through history")
    return AttachmentLoad(
        reader=reader,
        host=scenario.host,
        session=session,
        reference=matches[0]["reference"],
        expected_bytes=specification["bytes"],
        expected_sha256=stored["sha256"],
        name=name,
    )
