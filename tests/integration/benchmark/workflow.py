"""Scheduled network disturbances and public CLI reads for the Watch benchmark."""

import json
import subprocess
import time
from dataclasses import dataclass

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

    def read(self, scheduled_ns, deadline_ms):
        wait_until(scheduled_ns)
        started = time.monotonic_ns()
        error_code = None
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
                correct = (
                    response.get("command") == "session.history"
                    and data.get("target") == target
                    and data.get("source") == "online"
                    and [text_of(message) for message in data.get("messages", [])] == self.expected
                )
                outcome = "completed" if correct else "incorrect_content"
        except subprocess.TimeoutExpired:
            outcome = "timeout"
        except (AssertionError, json.JSONDecodeError, OSError, TypeError, AttributeError):
            outcome = "product_error"
        duration = milliseconds(time.monotonic_ns() - started)
        if outcome == "completed" and duration > deadline_ms:
            outcome = "timeout"
        observation = {"started_ns": started, "duration_ms": duration, "outcome": outcome}
        if error_code is not None:
            observation["error_code"] = error_code
        return observation


def prepare_history(scenario, reader, specification):
    session = scenario.dsh.create_session()
    expected = model.history_texts(specification)
    scenario.dsh.process.control("bench_seed_history", session_id=session, texts=expected)
    return HistoryLoad(reader, scenario.host, session, expected)
