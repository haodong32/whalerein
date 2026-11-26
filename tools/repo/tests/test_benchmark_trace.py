"""Native Perfetto export: canonical agreement, clock policy and failure modes."""

from __future__ import annotations

import hashlib

import pytest

from repo.benchmark import trace
from repo.errors import RepoError


def event(producer, ts, name, **data):
    return {
        "v": 1,
        "run": "run-trace",
        "producer": producer,
        "lifetime": f"lt-{producer}",
        "seq": ts,
        "ts": ts,
        "event": name,
        "data": data,
    }


def events():
    return [
        event("fixture", 1_000, "fixture_emit", stream="s-1", attempt="a-1", block="b-1",
              seq_start=0, seq_end=1, bytes=10),
        event("dsh", 1_100, "dsh_callback", stream="s-1", attempt="a-1", block="b-1",
              seq_start=0, seq_end=1, bytes=10, native="assistant-preview"),
        event("host", 1_200, "host_enqueue", stream="s-1", block="b-1", seq_start=0,
              seq_end=1, bytes=10, queue_depth=1, queue_bytes=10),
        event("core", 1_300, "core_receipt", frame="f-1", kind="event", bytes=10, decode="ok"),
        event("cli", 1_500, "cli_write", delivery=1, ordinal=1, bytes=10, outcome="success",
              flush=True),
        event("fixture", 1_600, "fixture_observe", attempt="a-1", ordinal=1, kind="preview",
              bytes=10, prefix_bytes=10, correct=True),
    ]


def exemplars():
    return [
        {"attempt": "a-1", "metric": "first_text_ms", "reason": "median",
         "coverage": "complete", "start_ns": 1_000, "end_ns": 1_600},
        {"attempt": "a-1", "metric": "max_display_delay_ms", "reason": "p95",
         "coverage": "complete", "start_ns": 1_050, "end_ns": 1_450},
    ]


def test_export_is_native_and_uses_one_monotonic_clock(tmp_path):
    target = tmp_path / "case.pftrace"
    metadata = trace.export(events(), exemplars(), target, origin_ns=900, run_id="run-trace")
    summary = trace.inspect(target)
    assert metadata["checksum"].startswith("sha256:")
    assert metadata["checksum"] == "sha256:" + hashlib.sha256(target.read_bytes()).hexdigest()
    assert summary["clock_ids"] == [trace.MONOTONIC_CLOCK_ID]
    assert summary["monotonic"] is True
    assert "producer/fixture" in summary["tracks"]
    assert "exemplar/first_text_ms" in summary["tracks"]
    assert summary["flows"] >= 1  # the stream flow joins fixture, dsh and host
    assert summary["slices"] >= len(events()) + len(exemplars()) * 2


def test_verify_agrees_with_canonical_ranges(tmp_path):
    target = tmp_path / "case.pftrace"
    metadata = trace.export(events(), exemplars(), target, origin_ns=900, run_id="run-trace")
    summary = trace.verify(
        target,
        events=len(events()),
        exemplars=len(exemplars()),
        minimum_ns=900,
        maximum_ns=metadata["bytes"] + 10_000,
    )
    assert summary["first_ns"] == 1_000
    assert summary["last_ns"] >= 1_600


def test_missing_edges_do_not_create_flows(tmp_path):
    target = tmp_path / "case.pftrace"
    single = [event("core", 1_000, "core_receipt", frame="f-1", kind="event", bytes=1, decode="ok")]
    metadata = trace.export(single, [], target, origin_ns=0, run_id="run-trace")
    assert metadata["exemplars"] == 0
    assert trace.inspect(target)["flows"] == 0


def test_reversed_exemplar_range_is_rejected(tmp_path):
    target = tmp_path / "case.pftrace"
    with pytest.raises(RepoError):
        trace.export(
            events(),
            [{"attempt": "a-1", "metric": "first_text_ms", "reason": "median",
              "coverage": "complete", "start_ns": 10, "end_ns": 5}],
            target,
            origin_ns=0,
            run_id="run-trace",
        )
    assert not target.exists()


def test_trace_byte_cap_is_enforced(tmp_path, monkeypatch):
    monkeypatch.setattr(trace, "MAX_TRACE_BYTES", 64)
    with pytest.raises(RepoError):
        trace.export(events(), exemplars(), tmp_path / "case.pftrace", origin_ns=0, run_id="r")


def test_unreadable_trace_is_a_clear_error(tmp_path):
    target = tmp_path / "case.pftrace"
    target.write_bytes(b"not a perfetto trace")
    with pytest.raises(RepoError):
        trace.inspect(target)

def test_boolean_annotations_are_not_encoded_as_integers(tmp_path):
    target = tmp_path / "case.pftrace"
    metadata = trace.export(
        events(),
        [{
            "attempt": "a-1", "metric": "first_text_ms", "reason": "median",
            "coverage": True, "start_ns": 1_000, "end_ns": 1_600,
        }],
        target,
        origin_ns=900,
        run_id="run-trace",
    )
    assert metadata["exemplars"] == 1
    assert trace.inspect(target)["clock_ids"] == [trace.MONOTONIC_CLOCK_ID]
