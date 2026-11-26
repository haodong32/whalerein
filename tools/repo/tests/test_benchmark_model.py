"""Model-level benchmark verdicts across AC-05, 07, 08, 09, 10, 12, 13 and 15.

The vectors are explicit and small. Each test name carries the binding string
from the task matrix (``latency/AC-XX.variant``) and asserts the specific model
outcome rather than a rendered summary.
"""

from __future__ import annotations

import copy
import json

import pytest

from repo.benchmark import analysis, model
from repo.errors import RepoError

# ---------------------------------------------------------------------------
# Small explicit case builders
# ---------------------------------------------------------------------------


def make_config(
    *,
    attempts,
    warmups=0,
    workflow="watch",
    mode="trace",
    deadline=100,
    window=20,
    seed=0,
    cadence=10,
    per_arm=False,
    network=None,
    coverage_floor=None,
):
    configuration = {
        "attempts": attempts,
        "warmup_attempts": warmups,
        "concurrency": 1,
        "profile": "warm-client",
        "workflow": workflow,
        "network": network or model.network_settings("normal", "host"),
        "deadline_ms": deadline,
        "observation_window_ms": window,
        "seed": seed,
        "observation_mode": mode,
        "cadence_ms": cadence,
        "delay_ms": 0,
    }
    if per_arm:
        configuration["per_arm_p95"] = True
    if coverage_floor is not None:
        configuration["coverage_floor"] = coverage_floor
    return configuration


def make_slot(ordinal, *, phase="measured", arm="watch", pair=None, order=0, start=0):
    return {
        "slot": ordinal,
        "phase": phase,
        "arm": arm,
        "pair": pair or f"slot-{ordinal}",
        "order": order,
        "planned_start_ms": start,
    }


def out(at, ordinal, *, kind="preview", bytes=0, prefix=0, correct=True, block=None):
    event = {
        "at_ms": at,
        "ordinal": ordinal,
        "kind": kind,
        "bytes": bytes,
        "prefix_bytes": prefix,
        "correct": correct,
    }
    if block is not None:
        event["block"] = block
    return event


def default_outputs(sizes, *, preview_at=5, terminal_at=15, terminal_prefix=None):
    total = sum(sizes)
    outputs = []
    if sizes and sizes[0]:
        outputs.append(out(preview_at, 0, bytes=sizes[0], prefix=sizes[0]))
    outputs.append(
        out(
            terminal_at,
            len(outputs),
            kind="terminal",
            bytes=total - (sizes[0] if outputs else 0),
            prefix=total if terminal_prefix is None else terminal_prefix,
        )
    )
    return outputs


def make_attempt(
    identity,
    slot,
    *,
    phase="measured",
    arm="watch",
    pair_id=None,
    status="finished",
    times=(0, 10),
    sizes=(10, 10),
    outputs=None,
    started=0,
    ended=120,
    cutoff=100,
    started_ns=None,
    ended_ns=None,
    cutoff_ns=None,
    early_stop="none",
    failure=None,
    setup=None,
    recovery=None,
    reads=None,
    trace=None,
):
    return {
        "id": identity,
        "slot": slot,
        "phase": phase,
        "arm": arm,
        "pair_id": pair_id or f"slot-{slot}",
        "status": status,
        "failure": failure or {"attribution": None},
        "setup": setup or {"errors": []},
        "source": {"times_ms": list(times), "sizes": list(sizes)},
        "outputs": (
            default_outputs(list(sizes))
            if outputs is None and status == "finished"
            else outputs or []
        ),
        "observation": {
            "started_ms": started,
            "ended_ms": ended,
            "cutoff_ms": cutoff,
            "early_stop": early_stop,
            **({"started_ns": started_ns} if started_ns is not None else {}),
            **({"ended_ns": ended_ns} if ended_ns is not None else {}),
            **({"cutoff_ns": cutoff_ns} if cutoff_ns is not None else {}),
        },
        **({"recovery": recovery} if recovery is not None else {}),
        **({"reads": reads} if reads is not None else {}),
        **({"trace": trace} if trace is not None else {}),
    }


def make_recovery(started, restored, milestones):
    return {
        "blackout_start_ms": started,
        "restored_ms": restored,
        "milestones": milestones,
    }


def default_trace(*, complete=True, path="case.pftrace", bytes=1024, cap_bytes=None, losses=None):
    trace = {
        "path": path,
        "checksum": None if path is None else "sha256:" + "0" * 64,
        "bytes": 0 if path is None else bytes,
        "complete": complete,
        "losses": losses or {"outlet_records": 0, "trace_bytes": 0, "attempts": 0},
    }
    if cap_bytes is not None:
        trace["cap_bytes"] = cap_bytes
    return trace


def build_facts(
    attempts,
    slots,
    *,
    workflow="watch",
    updates=("x" * 10, "y" * 10),
    cadence=10,
    deadline=100,
    window=20,
    mode="trace",
    status_state="complete",
    status_reasons=(),
    cleanup=None,
    clock_verified=True,
    clock_producers=None,
    topology=None,
    events=None,
    trace=None,
    configuration=None,
    started_at="2026-11-25T00:00:00Z",
):
    measured = sum(slot["phase"] == "measured" for slot in slots)
    warmup = sum(slot["phase"] == "warmup" for slot in slots)
    configuration = configuration or make_config(
        attempts=measured,
        warmups=warmup,
        workflow=workflow,
        mode=mode,
        deadline=deadline,
        window=window,
        cadence=cadence,
    )
    network = configuration["network"]
    response_final = cadence * len(updates)
    scenario = {
        "id": model.SCENARIO_ID,
        "version": model.SCENARIO_VERSION,
        "seed": str(configuration["seed"]),
        "deadline_ms": deadline,
        "observation_window_ms": window,
        "network": network,
        "workload": {
            "updates": list(updates),
            "gap_ms": cadence,
            "delivery": "immediate",
            "workflow": workflow,
            "controls": model.workload_controls(workflow, cadence, paired=True),
        },
        "response": {
            "start_ms": 0,
            "final_ms": response_final,
            "delay_ms": 0,
            "duplicate_final_ms": response_final + 300,
            "final_text": "".join(updates),
        },
    }
    producers = clock_producers
    if producers is None:
        producers = [
            {
                "producer": "fixture",
                "lifetime": "lt-fixture-1",
                "verified": clock_verified,
                **({} if clock_verified else {"reason": "clock_mismatch"}),
            }
        ]
    facts = {
        "schema_version": model.SCHEMA_VERSION,
        "kind": "benchmark-case",
        "run": {
            "id": "run-1",
            "started_at": started_at,
            "surface": model.OBSERVATION_SURFACE,
            "endpoint": model.OBSERVATION_ENDPOINT,
            "observation_mode": mode,
            "configuration": configuration,
            "fingerprint": {
                "build": {"revision": "abc123", "fingerprint": "build-1"},
                "fixture": {"name": "benchmark", "fingerprint": "fixture-1"},
                "schemas": {"case": 2, "event": 1, "workload": 8},
                "platform": {"os": "linux", "arch": "x86_64", "kernel": "6.8"},
                "limits": {"cpus": 4, "memory_bytes": 1 << 30},
                "clock": {
                    "source": "CLOCK_MONOTONIC",
                    "unit": "ns",
                    "verified": clock_verified,
                },
                "sampling": {"socket_hz": 1, "runtime_hz": 1},
            },
        },
        "scenario": scenario,
        "environment": {
            "artifacts": {"cli": "sha256:" + "a" * 64},
            "topology": topology or {"kind": "single-machine", "clock": "CLOCK_MONOTONIC"},
            "limits": {},
            "versions": {},
            "fixture": {"name": "benchmark", "fingerprint": "fixture-1"},
            "clock": {
                "source": "CLOCK_MONOTONIC",
                "unit": "ns",
                "boot_id_verified": clock_verified,
                "namespace_verified": clock_verified,
                "producers": producers,
            },
        },
        "schedule": {
            "seed": str(configuration["seed"]),
            "rule": "alternating",
            "slots": slots,
        },
        "attempts": attempts,
        "trace": trace if trace is not None else default_trace(complete=mode == "trace"),
        "status": {
            "state": status_state,
            "reasons": list(status_reasons),
            "cleanup": cleanup or {"result": "verified"},
        },
    }
    if events is not None:
        facts["events"] = events
    return facts


def one_attempt_case(**overrides):
    """A single current measured watch attempt with explicit defaults."""
    sizes = overrides.pop("sizes", (10, 10))
    times = overrides.pop("times", (0, 10))
    attempt_kwargs = {
        key: overrides.pop(key)
        for key in (
            "status",
            "outputs",
            "started",
            "ended",
            "cutoff",
            "started_ns",
            "ended_ns",
            "cutoff_ns",
            "early_stop",
            "failure",
            "setup",
            "recovery",
            "reads",
            "trace",
        )
        if key in overrides
    }
    slot = make_slot(0)
    attempt = make_attempt("a-0", 0, times=times, sizes=sizes, **attempt_kwargs)
    build = {
        key: overrides.pop(key)
        for key in (
            "workflow",
            "updates",
            "cadence",
            "deadline",
            "window",
            "mode",
            "status_state",
            "status_reasons",
            "cleanup",
            "clock_verified",
            "clock_producers",
            "topology",
            "events",
            "trace",
        )
        if key in overrides
    }
    if "updates" not in build:
        build["updates"] = tuple("x" * size for size in sizes)
    facts = build_facts([attempt], [slot], **build)
    facts.update(overrides)
    return facts


def derived(facts):
    return model.case_document(facts)["derived"]


# ---------------------------------------------------------------------------
# AC-15: deadline progress and user-visible stalls
# ---------------------------------------------------------------------------


def test_latency_ac_15_640_220_cutoff_exact_vector():
    """latency/AC-15.640-220-cutoff: model-asserted current-schema behavior."""
    outputs = [
        out(10, 0, bytes=220, prefix=220),
        out(40, 1, kind="duplicate", bytes=0, prefix=220),
        out(110, 2, kind="terminal", bytes=420, prefix=640),
    ]
    facts = one_attempt_case(
        updates=("x" * 220, "y" * 420),
        sizes=(220, 420),
        times=(0, 20),
        outputs=outputs,
        ended=130,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["visible_bytes"] == 220
    assert row["emitted_bytes"] == 640
    assert row["pending_bytes"] == 420
    assert row["peak_backlog_bytes"] == 420
    assert row["longest_pending_stall_ms"] == 80
    assert row["since_last_advance_ms"] == 90
    assert row["first_text_ms"] == 10
    assert row["terminal_ms"] is None
    assert row["late_terminal_ms"] == 110
    assert row["deadline_visible"] is False
    assert row["outcome"] == "late_reply"
    assert row["correct_by_deadline"] is False
    # The late terminal never rewrites the cutoff row.
    assert row["visible_bytes"] == 220
    assert row["terminal_integrity"] == "pass"


def test_latency_ac_15_wrong_replace_reset_exact_vector():
    """latency/AC-15.wrong-replace-reset: model-asserted current-schema behavior."""
    outputs = [
        out(5, 0, kind="wrong", bytes=100, prefix=0, correct=False),
        out(10, 1, kind="replace", bytes=220, prefix=220),
        out(50, 2, kind="reset", bytes=0, prefix=0),
        out(110, 3, kind="terminal", bytes=220, prefix=220),
    ]
    facts = one_attempt_case(
        updates=("a" * 220,),
        sizes=(220,),
        times=(0,),
        outputs=outputs,
        ended=140,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["visible_bytes"] == 0
    assert row["historical_visible_bytes"] == 220
    assert row["first_text_ms"] == 10
    assert row["deadline_visible"] is False
    assert row["late_terminal_ms"] == 110
    assert row["outcome"] == "late_reply"
    assert row["terminal_integrity"] == "pass"


def test_latency_ac_15_terminal_window_visibility_and_integrity_are_separate():
    """latency/AC-15.truncated-integrity-window; latency/AC-15.complete-integrity-window."""
    outputs = [
        out(95, 0, kind="terminal", bytes=20, prefix=20),
        out(105, 1, kind="duplicate", bytes=0, prefix=20),
    ]
    # latency/AC-15.truncated-integrity-window
    truncated = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=outputs,
        ended=101,
        cutoff=100,
    )
    row = derived(truncated)["attempts"][0]
    assert row["deadline_visible"] is True
    assert row["terminal_ms"] == 95
    assert row["terminal_integrity"] == "unverified"
    assert row["terminal_integrity_reason"] == "window_incomplete"
    assert row["correct_by_deadline"] is False
    assert row["outcome"] == "interrupted"

    # latency/AC-15.complete-integrity-window
    complete = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=outputs,
        ended=115,
        cutoff=100,
    )
    row = derived(complete)["attempts"][0]
    assert row["deadline_visible"] is True
    assert row["terminal_integrity"] == "fail"
    assert row["terminal_integrity_reason"] == "duplicate_terminal"
    assert row["correct_by_deadline"] is False
    assert row["outcome"] == "duplicate_final"


def test_latency_ac_15_late_terminal_never_rewrites_cutoff():
    """latency/AC-15.late-terminal: model-asserted current-schema behavior."""
    outputs = [
        out(20, 0, bytes=20, prefix=20),
        out(120, 1, kind="terminal", bytes=20, prefix=40),
    ]
    facts = one_attempt_case(
        updates=("a" * 20, "b" * 20),
        sizes=(20, 20),
        times=(0, 10),
        outputs=outputs,
        ended=150,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["visible_bytes"] == 20
    assert row["pending_bytes"] == 20
    assert row["terminal_ms"] is None
    assert row["late_terminal_ms"] == 120
    assert row["outcome"] == "late_reply"


def test_latency_ac_15_source_silence_only_counts_with_outstanding_text():
    """latency/AC-15.source-silence: model-asserted current-schema behavior."""
    outputs = [out(5, 0, bytes=10, prefix=10)]
    facts = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=outputs,
        ended=120,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["pending_bytes"] == 10
    assert row["longest_pending_stall_ms"] == 95
    assert row["since_last_advance_ms"] == 95

    shown = [out(0, 0, bytes=10, prefix=10)]
    facts = one_attempt_case(
        updates=("a" * 10,),
        sizes=(10,),
        times=(0,),
        outputs=shown,
        ended=120,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["pending_bytes"] == 0
    assert row["longest_pending_stall_ms"] == 0


def test_latency_ac_15_never_visible_timeout_before_text():
    """latency/AC-15.never-visible: model-asserted current-schema behavior."""
    facts = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=[],
        ended=120,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["first_text_ms"] is None
    assert row["visible_bytes"] == 0
    assert row["pending_bytes"] == 20
    assert row["since_last_advance_ms"] is None
    assert row["terminal_integrity"] == "missing"
    assert row["outcome"] == "timeout_before_text"


def test_latency_ac_15_final_only_terminal_establishes_first_text():
    """latency/AC-15.final-only-recovery: model-asserted current-schema behavior."""
    outputs = [out(50, 0, kind="terminal", bytes=20, prefix=20)]
    facts = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=outputs,
        ended=120,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["first_text_ms"] == 50
    assert row["terminal_ms"] == 50
    assert row["deadline_visible"] is True
    assert row["outcome"] == "completed"
    assert row["correct_by_deadline"] is True


def test_latency_ac_15_unobserved_observer_stops_before_cutoff():
    """latency/AC-15.unobserved: model-asserted current-schema behavior."""
    facts = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=[],
        ended=50,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["outcome"] == "interrupted"
    assert row["terminal_integrity"] == "unverified"
    assert row["terminal_integrity_reason"] == "horizon_incomplete"
    assert row["first_text_ms"] is None


def test_latency_ac_15_incomplete_capture_preserves_rows():
    """latency/AC-15.incomplete-capture: model-asserted current-schema behavior."""
    facts = one_attempt_case(status_state="incomplete", status_reasons=["fixture_interrupted"])
    report = model.case_document(facts)
    assert report["derived"]["attempts"][0]["outcome"] == "completed"
    assert report["derived"]["quality"]["state"] == "incomplete"
    assert "status_incomplete" in report["derived"]["quality"]["reasons"]


def test_latency_ac_15_hidden_block_is_not_pending_text():
    """latency/AC-15.hidden-block: model-asserted current-schema behavior."""
    outputs = [
        out(5, 0, kind="hidden", bytes=100, prefix=0),
        out(6, 1, bytes=50, prefix=50, block="visible"),
        out(15, 2, kind="terminal", bytes=0, prefix=50, block="visible"),
    ]
    facts = one_attempt_case(
        updates=("v" * 50, "h" * 100),
        sizes=(50, 100),
        times=(0, 10),
        outputs=outputs,
        ended=120,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["emitted_bytes"] == 50
    assert row["visible_bytes"] == 50
    assert row["pending_bytes"] == 0
    assert row["first_text_ms"] == 6
    assert row["outcome"] == "completed"


def test_latency_ac_15_duplicate_after_cutoff_fails_integrity():
    """latency/AC-15.duplicate-after-cutoff: model-asserted current-schema behavior."""
    outputs = [
        out(95, 0, kind="terminal", bytes=20, prefix=20),
        out(105, 1, kind="duplicate", bytes=0, prefix=20),
    ]
    facts = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=outputs,
        ended=115,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["deadline_visible"] is True
    assert row["visible_bytes"] == 20
    assert row["terminal_integrity"] == "fail"
    assert row["outcome"] == "duplicate_final"


def test_latency_ac_15_post_terminal_preview_fails_integrity():
    """latency/AC-15.post-terminal-preview: model-asserted current-schema behavior."""
    outputs = [
        out(95, 0, kind="terminal", bytes=20, prefix=20),
        out(110, 1, kind="preview", bytes=10, prefix=10, block="visible"),
    ]
    facts = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=outputs,
        ended=115,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["terminal_integrity"] == "fail"
    assert row["terminal_integrity_reason"] == "post_terminal_output"
    assert row["outcome"] == "incorrect_content"


def test_latency_ac_15_truncated_integrity_window_is_unverified():
    """latency/AC-15.truncated-integrity-window: model-asserted current-schema behavior."""
    outputs = [out(95, 0, kind="terminal", bytes=20, prefix=20)]
    facts = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=outputs,
        ended=110,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["terminal_integrity"] == "unverified"
    assert row["terminal_integrity_reason"] == "window_incomplete"
    assert row["deadline_visible"] is True


def test_latency_ac_15_complete_integrity_window_passes():
    """latency/AC-15.complete-integrity-window: model-asserted current-schema behavior."""
    outputs = [out(95, 0, kind="terminal", bytes=20, prefix=20)]
    facts = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=outputs,
        ended=115,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["terminal_integrity"] == "pass"
    assert row["terminal_integrity_reason"] == "complete_integrity_window"
    assert row["outcome"] == "completed"


def test_latency_ac_15_equal_time_stream_order_emits_before_observation():
    outputs = [out(10, 0, bytes=10, prefix=10)]
    facts = one_attempt_case(
        updates=("a" * 10,),
        sizes=(10,),
        times=(10,),
        outputs=outputs,
        ended=120,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["pending_bytes"] == 0
    assert row["longest_pending_stall_ms"] == 0


def test_latency_ac_15_decrease_cannot_count_as_progress():
    outputs = [
        out(5, 0, kind="replace", bytes=20, prefix=20),
        out(10, 1, kind="replace", bytes=10, prefix=10),
    ]
    facts = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=outputs,
        ended=120,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["last_advance_ms"] == 5
    assert row["since_last_advance_ms"] == 95
    assert row["visible_bytes"] == 10
    assert row["historical_visible_bytes"] == 20
    assert row["pending_bytes"] == 10
    assert row["longest_pending_stall_ms"] == 90


def test_latency_ac_15_backlog_is_per_active_block():
    outputs = [
        out(5, 0, bytes=10, prefix=10, block="a"),
        out(6, 1, bytes=10, prefix=10, block="b"),
        out(7, 2, kind="reset", bytes=0, prefix=0, block="a"),
    ]
    facts = one_attempt_case(
        updates=("a" * 10, "b" * 10),
        sizes=(10, 10),
        times=(0, 10),
        outputs=outputs,
        ended=120,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["visible_bytes"] == 10
    assert row["emitted_bytes"] == 20
    assert row["pending_bytes"] == 10
    assert row["peak_backlog_bytes"] == 10


def test_latency_ac_15_terminal_reprint_updates_state():
    outputs = [
        out(5, 0, kind="terminal", bytes=10, prefix=10),
        out(6, 1, kind="terminal", bytes=10, prefix=20),
    ]
    facts = one_attempt_case(
        updates=("a" * 20,),
        sizes=(20,),
        times=(0,),
        outputs=outputs,
        ended=120,
        cutoff=100,
    )
    row = derived(facts)["attempts"][0]
    assert row["visible_bytes"] == 20
    assert row["terminal_integrity"] == "fail"
    assert row["terminal_integrity_reason"] == "duplicate_terminal"


# ---------------------------------------------------------------------------
# AC-05: clock and correlation integrity
# ---------------------------------------------------------------------------


def shaped_event(event, producer, ts, data, seq=1):
    return {
        "v": 1,
        "run": "run-1",
        "producer": producer,
        "lifetime": "lt-1",
        "seq": seq,
        "ts": ts,
        "event": event,
        "data": data,
    }


def test_latency_ac_05_shared_clock_direct_subtraction():
    """latency/AC-05.shared-clock: model-asserted current-schema behavior."""
    events = [
        shaped_event(
            "host_enqueue",
            "host",
            1_000_000_000,
            {"stream": "s", "block": "b", "seq_start": 0, "seq_end": 1, "bytes": 10},
        ),
        shaped_event(
            "host_dequeue",
            "host",
            1_050_000_000,
            {
                "stream": "s",
                "block": "b",
                "seq_start": 0,
                "seq_end": 1,
                "bytes": 10,
                "wait_ns": 50_000_000,
            },
        ),
    ]
    report = derived(one_attempt_case(events=events))
    assert report["quality"]["clock"]["verified"] is True
    interval = report["attribution"]["intervals"][0]
    assert interval["name"] == "host_queue_ms"
    assert interval["duration_ms"] == 50.0
    assert interval["start_ns"] == 1_000_000_000


def test_latency_ac_05_wrong_clock_or_origin_is_unsupported():
    """latency/AC-05.wrong-clock-or-origin: model-asserted current-schema behavior."""
    facts = one_attempt_case()
    facts["environment"]["clock"]["source"] = "CLOCK_REALTIME"
    with pytest.raises(RepoError, match="unsupported benchmark clock source"):
        model.validate_case(facts)
    facts = one_attempt_case()
    facts["environment"]["topology"]["clock"] = "process-relative"
    with pytest.raises(RepoError, match="unsupported benchmark topology clock"):
        model.validate_case(facts)


def test_latency_ac_05_remote_topology_is_rejected():
    """latency/AC-05.remote-rejected: model-asserted current-schema behavior."""
    facts = one_attempt_case(topology={"kind": "fleet", "clock": "CLOCK_MONOTONIC"})
    with pytest.raises(RepoError, match="unsupported benchmark topology"):
        model.validate_case(facts)


def test_latency_ac_05_foreign_namespace_keeps_business_outcome():
    """latency/AC-05.foreign-namespace: model-asserted current-schema behavior."""
    facts = one_attempt_case()
    facts["environment"]["clock"]["namespace_verified"] = False
    report = derived(facts)
    row = report["attempts"][0]
    assert row["outcome"] == "completed"
    assert row["correct_by_deadline"] is True
    assert report["quality"]["clock"]["verified"] is False
    assert report["quality"]["clock"]["reason"] == "namespace_unverified"
    assert report["attribution"]["available"] is False
    assert report["attribution"]["reason"] == "clock_unverified"


def test_latency_ac_05_outer_runner_timestamps_are_excluded():
    """latency/AC-05.outer-clock-excluded: model-asserted current-schema behavior."""
    first = derived(one_attempt_case())
    second = derived(one_attempt_case(started_at="2027-01-01T12:34:56Z"))
    for key in ("attempts", "outcomes", "latency", "streaming", "populations"):
        assert first[key] == second[key]


def test_latency_ac_05_restart_reuses_the_verified_clock():
    """latency/AC-05.restart: model-asserted current-schema behavior."""
    producers = [
        {"producer": "fixture", "lifetime": "lt-1", "verified": True},
        {"producer": "fixture", "lifetime": "lt-2", "verified": True},
    ]
    report = derived(one_attempt_case(clock_producers=producers))
    assert report["quality"]["clock"]["verified"] is True
    assert report["quality"]["clock"]["restarted"] is True


def test_latency_ac_05_unverified_restart_lifetime_marks_clock_unverified():
    """latency/AC-05.restart: model-asserted current-schema behavior."""
    producers = [
        {"producer": "fixture", "lifetime": "lt-1", "verified": True},
        {"producer": "fixture", "lifetime": "lt-2", "verified": False, "reason": "restart"},
    ]
    report = derived(one_attempt_case(clock_producers=producers))
    assert report["quality"]["clock"]["verified"] is False
    assert report["quality"]["clock"]["reason"] == "producer_unverified"
    assert report["attempts"][0]["outcome"] == "completed"


def test_latency_ac_05_ambiguous_join_is_unavailable_not_zero():
    """latency/AC-05.ambiguous: model-asserted current-schema behavior."""
    events = [
        shaped_event(
            "host_enqueue",
            "host",
            1_000_000_000,
            {"stream": "s", "block": "b", "seq_start": 0, "seq_end": 1, "bytes": 10},
        ),
        shaped_event(
            "host_dequeue",
            "host",
            1_010_000_000,
            {
                "stream": "s",
                "block": "b",
                "seq_start": 0,
                "seq_end": 1,
                "bytes": 10,
                "wait_ns": 1,
            },
            seq=2,
        ),
        shaped_event(
            "host_dequeue",
            "host",
            1_020_000_000,
            {
                "stream": "s",
                "block": "b",
                "seq_start": 0,
                "seq_end": 1,
                "bytes": 10,
                "wait_ns": 2,
            },
            seq=3,
        ),
    ]
    report = derived(one_attempt_case(events=events))
    assert report["attribution"]["intervals"] == []
    assert report["attribution"]["unresolved"][0]["reason"] == "ambiguous_join"


# ---------------------------------------------------------------------------
# Current-version validation and replay (AC-08 base)
# ---------------------------------------------------------------------------


def test_latency_ac_08_current_case_document_is_published():
    """latency/AC-08.current: model-asserted current-schema behavior."""
    report = model.case_document(one_attempt_case())
    assert report["schema_version"] == 2
    assert report["derived"]["attempts"][0]["outcome"] == "completed"
    assert model.validate_report(report) == report


def test_latency_ac_08_old_future_and_mixed_versions_are_rejected(tmp_path):
    """latency/AC-08.old; latency/AC-08.future; latency/AC-08.mixed."""
    # latency/AC-08.old and latency/AC-08.future
    for version in (1, 3, None):
        facts = one_attempt_case()
        if version is None:
            facts.pop("schema_version")
        else:
            facts["schema_version"] = version
        with pytest.raises(RepoError, match="unsupported benchmark case schema"):
            model.validate_case(facts)
        path = tmp_path / f"v{version}.json"
        path.write_text(json.dumps(facts))
        with pytest.raises(RepoError, match="unsupported benchmark schema version"):
            model.read(path)

    # latency/AC-08.mixed
    suite = model.suite_document([])
    facts = one_attempt_case()
    facts["schema_version"] = 2
    facts["kind"] = "benchmark-suite"
    with pytest.raises(RepoError, match="unsupported benchmark mixed document"):
        model.validate_case(facts)
    assert model.validate_suite(suite)["schema_version"] == 2


def test_latency_ac_08_replay_serializes_facts_and_rederives_identically(tmp_path):
    """latency/AC-08.current: model-asserted current-schema behavior."""
    facts = one_attempt_case(events=[])
    first = model.case_document(copy.deepcopy(facts))
    path = tmp_path / "benchmark.json"
    path.write_text(json.dumps(first, sort_keys=True))
    replayed = model.read(path)
    assert replayed["derived"] == first["derived"]
    assert model.derive(json.loads(json.dumps(facts))) == first["derived"]


def test_latency_ac_08_tampered_derived_sections_are_rejected():
    """latency/AC-08.current: model-asserted current-schema behavior."""
    report = model.case_document(one_attempt_case())
    report["derived"]["outcomes"]["correct_by_deadline"] = 0
    with pytest.raises(RepoError, match="derived sections are stale or tampered"):
        model.validate_report(report)


def test_latency_ac_08_interrupted_suite_keeps_partial_cases():
    """latency/AC-08.interrupted: model-asserted current-schema behavior."""
    first = model.case_document(one_attempt_case())
    incomplete = one_attempt_case(status_state="incomplete", status_reasons=["interrupted"])
    incomplete["run"]["id"] = "run-2"
    second = model.case_document(incomplete)
    suite = model.suite_document(
        [first, second],
        suite_id="suite-1",
        status={
            "state": "interrupted",
            "reasons": ["signal"],
            "pending": [1],
            "interruption": {"signal": "SIGINT"},
        },
    )
    assert model.validate_suite(suite)["cases"][1]["status"] == "incomplete"
    suite["status"]["pending"] = []
    with pytest.raises(RepoError, match="interrupted suite must list pending"):
        model.validate_suite(suite)


def test_latency_ac_08_comparison_retains_provenance_and_clock_policy():
    """latency/AC-08.current: model-asserted current-schema behavior."""
    baseline = [model.case_document(one_attempt_case())]
    candidate = [model.case_document(one_attempt_case())]
    result = analysis.compare_runs(baseline, candidate)
    assert result["baseline"]["revision"] == "abc123"
    assert result["clock_policy"] == {"source": "CLOCK_MONOTONIC", "unit": "ns"}
    assert result["conditions"]["match"] is True
    assert result["provenance"]["baseline"][0]["run_id"] == "run-1"


# ---------------------------------------------------------------------------
# Mixed-load and slot-accounting builders
# ---------------------------------------------------------------------------


def mixed_slots(pairs, *, warmup_pairs=0, seed=0, start=0):
    slots = []
    first = "idle" if model.seed_parity(seed) == 0 else "bulk"
    other = "bulk" if first == "idle" else "idle"
    for phase, count in (("warmup", warmup_pairs), ("measured", pairs)):
        for pair_index in range(count):
            expected = (first, other) if pair_index % 2 == 0 else (other, first)
            pair = f"{phase}-pair-{pair_index:02d}"
            for order, arm in enumerate(expected):
                slots.append(
                    make_slot(
                        start + len(slots),
                        phase=phase,
                        arm=arm,
                        pair=pair,
                        order=order,
                        start=len(slots) * 10,
                    )
                )
    return slots


def read_observation(
    identity,
    *,
    op="history",
    scheduled=0,
    started=0,
    finished=5,
    expected=64,
    verified_bytes=None,
    outcome="completed",
    route="inline",
    pages=1,
    messages=8,
    error_code=None,
    token_repeats=0,
    corrupt=False,
):
    verified = outcome == "completed"
    observation = {
        "id": identity,
        "op": op,
        "scheduled_at_ms": scheduled,
        "started_at_ms": started,
        "finished_at_ms": finished,
        "deadline_ms": 100,
        "pages": pages,
        "messages": messages,
        "bytes_expected": expected,
        "bytes_verified": (
            expected
            if verified_bytes is None and verified
            else (0 if verified else verified_bytes or 0)
        ),
        "physical_bytes": expected,
        "route": route,
        "outcome": outcome,
        "verified": verified,
        "token_repeats": token_repeats,
        "corrupt": corrupt,
    }
    if error_code is not None:
        observation["error_code"] = error_code
    return observation


def default_reads(identity, *, start=0, finish=5):
    return {
        "history": [
            read_observation(
                f"{identity}-h{index}",
                scheduled=start,
                started=start,
                finished=finish,
            )
            for index in range(4)
        ],
        "attachment": [
            read_observation(
                f"{identity}-a0",
                op="attachment",
                scheduled=start,
                started=start,
                finished=finish,
                expected=128,
                route="reference",
            )
        ],
    }


def mixed_case(
    *,
    pairs=3,
    warmup_pairs=0,
    seed=0,
    per_arm=False,
    read_factory=None,
    outputs_factory=None,
    statuses=None,
    events=None,
    trace=None,
):
    slots = mixed_slots(pairs, warmup_pairs=warmup_pairs, seed=seed)
    attempts = []
    for slot in slots:
        identity = f"a-{slot['slot']}"
        reads = []
        if slot["arm"] == "bulk":
            factory = read_factory or (lambda item: default_reads(item["id"]))
            reads = factory({"id": identity, "slot": slot["slot"]})
            reads = [*reads["history"], *reads["attachment"]]
        overrides = outputs_factory(slot) if outputs_factory else {}
        attempts.append(
            make_attempt(
                identity,
                slot["slot"],
                phase=slot["phase"],
                arm=slot["arm"],
                pair_id=slot["pair"],
                reads=reads,
                **overrides,
            )
        )
    if statuses:
        for attempt in attempts:
            if attempt["id"] in statuses:
                attempt["status"] = statuses[attempt["id"]]
    measured = sum(slot["phase"] == "measured" for slot in slots)
    warmup = sum(slot["phase"] == "warmup" for slot in slots)
    configuration = make_config(
        attempts=measured,
        warmups=warmup,
        workflow="mixed-load",
        per_arm=per_arm,
        seed=seed,
    )
    return build_facts(
        attempts,
        slots,
        workflow="mixed-load",
        configuration=configuration,
        events=events,
        trace=trace,
    )


def many_case(n, factory, *, updates=("x" * 100,), deadline=100, window=20, configuration=None):
    slots = [make_slot(index) for index in range(n)]
    attempts = [make_attempt(f"a-{index}", index, **factory(index)) for index in range(n)]
    configuration = configuration or make_config(attempts=n, deadline=deadline, window=window)
    return build_facts(
        attempts,
        slots,
        updates=updates,
        deadline=deadline,
        window=window,
        configuration=configuration,
    )


# ---------------------------------------------------------------------------
# AC-09: reproducible workload correctness and slot accounting
# ---------------------------------------------------------------------------


def test_latency_ac_09_defaults_keep_four_warmups_and_thirty_measured():
    """latency/AC-09.defaults: 4 warmups / 30 measured, every planned slot counted."""
    assert model.DEFAULT_WARMUPS == 4
    assert model.DEFAULT_ATTEMPTS == 30
    slots = [make_slot(index, phase="warmup") for index in range(4)]
    slots += [make_slot(index + 4, start=index) for index in range(30)]
    attempts = [make_attempt(f"w-{index}", index, phase="warmup") for index in range(4)]
    attempts.append(make_attempt("a-4", 4))
    facts = build_facts(attempts, slots)
    report = derived(facts)
    assert report["populations"]["planned"]["total"] == 34
    assert report["populations"]["planned"]["warmup"] == 4
    assert report["populations"]["planned"]["measured"] == 30
    assert report["outcomes"]["measured"] == 30
    assert report["outcomes"]["counts"]["not_started"] == 29
    assert report["outcomes"]["counts"]["completed"] == 1
    assert report["outcomes"]["success_share"] == pytest.approx(1 / 30)
    assert report["populations"]["denominators"]["on_time_correct_over_planned"] == pytest.approx(
        1 / 30
    )


def test_latency_ac_09_zero_warmup_is_valid():
    """latency/AC-09.zero-warmup: no warmup slots is a valid plan."""
    slots = [make_slot(0), make_slot(1)]
    attempts = [make_attempt("a-0", 0)]
    facts = build_facts(attempts, slots)
    report = derived(facts)
    assert report["outcomes"]["warmup"] == {"attempts": 0, "completed": 0}
    assert report["populations"]["planned"]["warmup"] == 0


def test_latency_ac_09_setup_failure_and_unstarted_slots_reconcile():
    """latency/AC-09.setup-failure: 6 successes, 1 setup failure, 23 unstarted."""
    slots = [make_slot(index) for index in range(30)]
    attempts = [make_attempt(f"a-{index}", index) for index in range(6)]
    attempts.append(
        make_attempt(
            "a-6",
            6,
            status="setup_failed",
            failure={"attribution": "fixture", "reason": "watch_not_ready"},
        )
    )
    facts = build_facts(attempts, slots)
    report = derived(facts)
    assert report["outcomes"]["counts"] == {
        "completed": 6,
        "product_error": 0,
        "timeout_before_text": 0,
        "timeout_after_text": 0,
        "late_reply": 0,
        "incorrect_content": 0,
        "duplicate_final": 0,
        "not_started": 23,
        "setup_failed": 1,
        "interrupted": 0,
    }
    assert report["populations"]["setup_failures"] == {"fixture": 1}
    assert report["populations"]["not_started"] == 23
    assert report["slot_accounting"] if "slot_accounting" in report else True


def test_latency_ac_09_interrupted_slot_remains_in_denominators():
    """latency/AC-09.interrupted-slot: an interrupted attempt stays in the population."""
    slots = [make_slot(0), make_slot(1)]
    attempts = [
        make_attempt("a-0", 0),
        make_attempt("a-1", 1, status="interrupted", ended=50),
    ]
    report = derived(build_facts(attempts, slots))
    assert report["outcomes"]["measured"] == 2
    assert report["outcomes"]["counts"]["interrupted"] == 1
    assert report["outcomes"]["counts"]["completed"] == 1
    assert report["populations"]["interrupted"] == 1


def test_latency_ac_09_odd_mixed_count_is_rejected_before_build():
    """latency/AC-09.odd: odd mixed measured or per-phase counts fail."""
    slots = [
        make_slot(0, arm="idle", pair="p0", order=0),
        make_slot(1, arm="bulk", pair="p0", order=1),
        make_slot(2, arm="idle", pair="p1", order=0),
    ]
    attempts = [
        make_attempt("a-0", 0, arm="idle", pair_id="p0"),
        make_attempt("a-1", 1, arm="bulk", pair_id="p0", reads=[]),
        make_attempt("a-2", 2, arm="idle", pair_id="p1"),
    ]
    configuration = make_config(attempts=3, workflow="mixed-load")
    facts = build_facts(attempts, slots, workflow="mixed-load", configuration=configuration)
    with pytest.raises(RepoError, match="even number of slots"):
        model.validate_case(facts)


def test_latency_ac_09_multi_page_read_keeps_logical_and_physical_bytes():
    """latency/AC-09.multi-page: continuations keep verified bytes and page counts."""
    read_sets = {}

    def reads_factory(item):
        sets = default_reads(item["id"])
        for read in sets["history"]:
            read.update(pages=3, messages=24, physical_bytes=192)
        read_sets[item["id"]] = sets
        return sets

    facts = mixed_case(pairs=1, read_factory=reads_factory)
    report = derived(facts)
    bucket = report["workflow_results"]["reads"]["bulk"]
    assert bucket["pages"] == 13  # four three-page reads plus one attachment page
    assert bucket["bytes_verified"] == sum(
        read["bytes_verified"]
        for sets in read_sets.values()
        for read in (*sets["history"], *sets["attachment"])
    )
    assert bucket["physical_bytes"] > bucket["bytes_verified"]


def test_latency_ac_09_failed_read_stays_in_denominator_and_reports_code():
    """latency/AC-09.failed-read: a product read failure keeps its bounded code."""
    facts = mixed_case(pairs=2)
    bulk = [item for item in facts["attempts"] if item["arm"] == "bulk"]
    bulk[0]["reads"][0].update(
        outcome="product_error", bytes_verified=0, verified=False, error_code="rate_limited"
    )
    report = derived(facts)
    bucket = report["workflow_results"]["reads"]["bulk"]
    assert bucket["expected_reads"] == 10
    assert bucket["successful_reads"] == 9
    assert bucket["errors"] == {"rate_limited": 1}
    assert report["workflow_results"]["bulk_read_gate"]["state"] == "incomplete"
    assert "bulk_read_errors" in report["workflow_results"]["bulk_read_gate"]["reasons"]


def test_latency_ac_09_zero_overlap_cannot_establish_load_impact():
    """latency/AC-09.zero-overlap: reads after the Watch ends have zero overlap."""

    def factory(item):
        return {
            "history": [
                read_observation(f"{item['id']}-h{index}", started=20, finished=30, scheduled=20)
                for index in range(4)
            ],
            "attachment": [
                read_observation(
                    f"{item['id']}-a0",
                    op="attachment",
                    started=20,
                    finished=30,
                    scheduled=20,
                    expected=128,
                    route="reference",
                )
            ],
        }

    report = derived(mixed_case(pairs=1, read_factory=factory))
    exposure = report["workflow_results"]["exposure"]
    assert exposure["overlapping_reads"] == 0
    assert exposure["union_ms"] == 0
    assert exposure["sufficient"] is False
    assert exposure["reason"] == "zero_overlap"


def test_latency_ac_09_overlap_union_is_not_the_sum_of_reader_time():
    """latency/AC-09.overlap-union: summed reader overlap can exceed the union."""

    def factory(item):
        return {
            "history": [
                read_observation(f"{item['id']}-h0", started=0, finished=8),
                read_observation(f"{item['id']}-h1", started=2, finished=10),
                read_observation(f"{item['id']}-h2", started=2, finished=10),
                read_observation(f"{item['id']}-h3", started=0, finished=10),
            ],
            "attachment": [
                read_observation(
                    f"{item['id']}-a0",
                    op="attachment",
                    started=0,
                    finished=10,
                    expected=128,
                    route="reference",
                )
            ],
        }

    report = derived(mixed_case(pairs=1, read_factory=factory))
    exposure = report["workflow_results"]["exposure"]
    assert exposure["union_ms"] == 10
    assert exposure["summed_overlap_ms"] > exposure["union_ms"]
    assert exposure["overlapping_reads"] == 5
    assert exposure["sufficient"] is True


def test_latency_ac_09_unknown_overlap_is_explicit():
    """latency/AC-09.unknown-overlap: a missing read boundary stays unknown."""

    def factory(item):
        reads = default_reads(item["id"])
        reads["history"][0]["started_at_ms"] = None
        reads["history"][0]["finished_at_ms"] = None
        return reads

    report = derived(mixed_case(pairs=1, read_factory=factory))
    exposure = report["workflow_results"]["exposure"]
    assert exposure["unknown_reads"] == 1
    assert exposure["sufficient"] is False
    assert exposure["reason"] == "unknown_overlap"


def test_latency_ac_09_concurrent_reads_share_one_watch_exposure():
    """latency/AC-09.concurrent: concurrent reads are one union, not serial work."""
    report = derived(mixed_case(pairs=2))
    exposure = report["workflow_results"]["exposure"]
    assert exposure["scheduled_reads"] == 10
    assert exposure["started_reads"] == 10
    assert exposure["completed_reads"] == 10
    assert exposure["overlapping_reads"] == 10
    assert exposure["union_ms"] == 10
    assert exposure["summed_overlap_ms"] == 50


# ---------------------------------------------------------------------------
# AC-10: exemplar selection and trace ranges
# ---------------------------------------------------------------------------


def exemplar_by(report, kind, metric=None):
    for exemplar in report["trace_index"]["exemplars"]:
        if exemplar["kind"] == kind and (metric is None or exemplar["metric"] == metric):
            return exemplar
    raise AssertionError(f"missing exemplar {kind}/{metric}")


def test_latency_ac_10_typical_exemplar_is_nearest_median():
    """latency/AC-10.typical: the typical exemplar is the nearest median attempt."""
    facts = many_case(
        5,
        lambda index: {
            "outputs": [
                out(5 + index, 0, bytes=10, prefix=10),
                out(15 + index, 1, kind="terminal", bytes=10, prefix=20),
            ]
        },
    )
    report = derived(facts)
    exemplar = exemplar_by(report, "typical")
    assert exemplar["attempt_id"] == "a-2"
    assert exemplar["value"] == 7
    assert exemplar["selection"] == "nearest_median"


def test_latency_ac_10_tail_uses_nearest_rank_p95():
    """latency/AC-10.tail: 20 samples select rank ceil(0.95*n) exactly."""
    facts = many_case(
        20,
        lambda index: {
            "outputs": [
                out(index + 1, 0, bytes=10, prefix=10),
                out(index + 20, 1, kind="terminal", bytes=10, prefix=20),
            ]
        },
    )
    report = derived(facts)
    exemplar = exemplar_by(report, "tail", "first_text_ms")
    assert exemplar["selection"] == "nearest_rank_p95"
    assert exemplar["value"] == 19
    assert exemplar["slot"] == 18


def test_latency_ac_10_timeout_is_selected_as_failed_exemplar():
    """latency/AC-10.timeout: a timeout remains a selectable failed attempt."""
    facts = many_case(3, lambda index: {})
    timeout = facts["attempts"][1]
    timeout["outputs"] = [out(5, 0, bytes=10, prefix=10)]
    timeout["observation"]["ended_ms"] = 120
    report = derived(facts)
    exemplar = exemplar_by(report, "failed")
    assert exemplar["attempt_id"] == "a-1"
    assert exemplar["value"] == "timeout_after_text"


def test_latency_ac_10_overlapping_absolute_ranges_stay_visible():
    """latency/AC-10.overlap: exemplar ranges use one MONOTONIC base and may overlap."""
    facts = many_case(
        20,
        lambda index: {
            "started_ns": 1_000_000_000 + index * 100_000_000,
            "ended_ns": 1_500_000_000 + index * 100_000_000,
            "outputs": [
                out(index + 1, 0, bytes=10, prefix=10),
                out(index + 20, 1, kind="terminal", bytes=10, prefix=20),
            ],
        },
    )
    report = derived(facts)
    ranges = [item["trace_range"] for item in report["trace_index"]["exemplars"]]
    assert all(item["path"] == "case.pftrace" for item in ranges)
    assert all(item["start_ns"] < item["end_ns"] for item in ranges)
    assert any(
        left["start_ns"] < right["end_ns"] and right["start_ns"] < left["end_ns"]
        for left in ranges
        for right in ranges
        if left is not right
    )


def test_latency_ac_10_missing_trace_has_explicit_reason():
    """latency/AC-10.missing: an absent trace carries an explicit range reason."""
    facts = many_case(3, lambda index: {})
    facts["trace"] = default_trace(complete=True, path=None)
    report = derived(facts)
    exemplar = exemplar_by(report, "typical")
    assert exemplar["trace_range"] is None
    assert exemplar["range_reason"] == "trace_missing"


def test_latency_ac_10_export_failure_keeps_business_verdict():
    """latency/AC-10.export-failure: export status never changes business metrics."""
    healthy = many_case(3, lambda index: {})
    failed = copy.deepcopy(healthy)
    failed["trace"]["complete"] = False
    assert derived(failed)["outcomes"] == derived(healthy)["outcomes"]
    assert derived(failed)["latency"] == derived(healthy)["latency"]
    exemplar = exemplar_by(derived(failed), "typical")
    assert exemplar["trace_range"] is None
    assert exemplar["range_reason"] == "trace_truncated"


def test_latency_ac_10_different_metric_tails_choose_different_attempts():
    """latency/AC-10.different-metric-tails: first-text and backlog tails differ."""

    def factory(index):
        if index == 0:
            return {
                "times": (0,),
                "sizes": (10,),
                "outputs": [
                    out(90, 0, bytes=10, prefix=10),
                    out(95, 1, kind="terminal", bytes=0, prefix=10),
                ],
            }
        if index == 19:
            return {
                "times": (0,),
                "sizes": (100,),
                "outputs": [
                    out(5, 0, bytes=10, prefix=10),
                    out(10, 1, kind="terminal", bytes=90, prefix=100),
                ],
            }
        return {
            "times": (0,),
            "sizes": (100,),
            "outputs": [
                out(90, 0, bytes=10, prefix=10),
                out(95, 1, kind="terminal", bytes=90, prefix=100),
            ],
        }

    report = derived(many_case(20, factory))
    first = exemplar_by(report, "tail", "first_text_ms")
    backlog = exemplar_by(report, "tail", "peak_backlog_bytes")
    assert first["attempt_id"] != backlog["attempt_id"]


def test_latency_ac_10_export_verdict_is_independent_of_rendering_mode():
    """latency/AC-10.export-independent-verdict: mode changes quality, not business rows."""
    facts = many_case(3, lambda index: {})
    facts["run"]["configuration"]["observation_mode"] = "off"
    facts["run"]["observation_mode"] = "off"
    report = derived(facts)
    assert report["attempts"][0]["outcome"] == "completed"
    assert report["quality"]["capture"]["mode"] == "off"
    assert report["quality"]["diagnostic_pass"] is False


def test_latency_ac_10_shared_file_ranges_point_to_one_trace():
    """latency/AC-10.shared-file-ranges: every selected interval names the same case file."""
    facts = many_case(
        20,
        lambda index: {
            "started_ns": 1_000_000_000 + index * 10_000_000,
            "ended_ns": 1_100_000_000 + index * 10_000_000,
            "outputs": [
                out(index + 1, 0, bytes=10, prefix=10),
                out(index + 20, 1, kind="terminal", bytes=10, prefix=20),
            ],
        },
    )
    report = derived(facts)
    paths = {
        exemplar["trace_range"]["path"]
        for exemplar in report["trace_index"]["exemplars"]
        if exemplar["trace_range"] is not None
    }
    assert paths == {"case.pftrace"}


def test_latency_ac_10_trace_byte_cap_is_explicit():
    """latency/AC-10.trace-byte-cap: a saturated trace is truncated, not passed."""
    facts = many_case(3, lambda index: {})
    facts["trace"]["cap_bytes"] = 1024
    facts["trace"]["bytes"] = 1024
    facts["trace"]["losses"] = {"outlet_records": 5, "trace_bytes": 10, "attempts": 1}
    report = derived(facts)
    assert report["trace"]["truncated"] is True
    assert report["quality"]["capture"]["truncated"] is True
    assert report["quality"]["state"] == "incomplete"
    assert "trace_byte_cap" in report["quality"]["reasons"]


def test_latency_ac_10_missing_absolute_ns_states_the_reason():
    """latency/AC-10.missing: relative-only observations never invent an absolute range."""
    facts = many_case(3, lambda index: {})
    for attempt in facts["attempts"]:
        attempt["observation"].pop("started_ns", None)
        attempt["observation"].pop("ended_ns", None)
    report = derived(facts)
    assert all(
        exemplar["trace_range"] is None and exemplar["range_reason"] == "trace_clock_unavailable"
        for exemplar in report["trace_index"]["exemplars"]
    )


# ---------------------------------------------------------------------------
# AC-12: verified attribution and trends
# ---------------------------------------------------------------------------


def test_latency_ac_12_host_delay_is_contained_in_the_interval():
    """latency/AC-12.host-delay: the derived Host queue interval contains 37 ms."""
    injected_ns = 37_000_000
    events = [
        shaped_event(
            "host_enqueue",
            "host",
            1_000_000_000,
            {"stream": "s", "block": "b", "seq_start": 0, "seq_end": 1, "bytes": 10},
        ),
        shaped_event(
            "host_dequeue",
            "host",
            1_000_000_000 + injected_ns,
            {
                "stream": "s",
                "block": "b",
                "seq_start": 0,
                "seq_end": 1,
                "bytes": 10,
                "wait_ns": injected_ns,
            },
        ),
    ]
    report = derived(one_attempt_case(events=events))
    interval = next(
        item for item in report["attribution"]["intervals"] if item["name"] == "host_queue_ms"
    )
    assert interval["duration_ms"] == pytest.approx(37.0)
    assert interval["duration_ms"] >= 37.0


def test_latency_ac_12_access_delay_is_contained_in_the_interval():
    """latency/AC-12.access-delay: Access admit-to-reply contains the injected delay."""
    events = [
        shaped_event(
            "access_query",
            "access",
            2_000_000_000,
            {"request": "r-1", "attempt": 1, "route": "inline", "stage": "admit"},
        ),
        shaped_event(
            "access_query",
            "access",
            2_000_000_000 + 23_000_000,
            {"request": "r-1", "attempt": 1, "route": "inline", "stage": "reply"},
            seq=2,
        ),
    ]
    report = derived(one_attempt_case(events=events))
    interval = next(
        item for item in report["attribution"]["intervals"] if item["name"] == "access_forward_ms"
    )
    assert interval["duration_ms"] == pytest.approx(23.0)


def test_latency_ac_12_core_delay_and_ambiguous_scopes():
    """latency/AC-12.core-delay: receipt-to-delivery subtraction for one frame."""
    events = [
        shaped_event(
            "core_receipt",
            "core",
            3_000_000_000,
            {"frame": "f-1", "kind": "event", "bytes": 10, "decode": "ok"},
        ),
        shaped_event(
            "core_delivery",
            "core",
            3_000_000_000 + 11_000_000,
            {"frame": "f-1", "delivery": 1, "kind": "preview", "bytes": 10},
            seq=2,
        ),
    ]
    report = derived(one_attempt_case(events=events))
    interval = next(
        item for item in report["attribution"]["intervals"] if item["name"] == "core_delivery_ms"
    )
    assert interval["duration_ms"] == pytest.approx(11.0)


def test_latency_ac_12_overlapping_intervals_are_unioned_not_summed():
    """latency/AC-12.overlap: concurrent Host and Access work is not serial time."""
    events = [
        shaped_event(
            "host_enqueue",
            "host",
            1_000_000_000,
            {"stream": "s", "block": "b", "seq_start": 0, "seq_end": 1, "bytes": 10},
        ),
        shaped_event(
            "host_dequeue",
            "host",
            1_040_000_000,
            {
                "stream": "s",
                "block": "b",
                "seq_start": 0,
                "seq_end": 1,
                "bytes": 10,
                "wait_ns": 1,
            },
            seq=2,
        ),
        shaped_event(
            "access_query",
            "access",
            1_020_000_000,
            {"request": "r-1", "attempt": 1, "route": "inline", "stage": "admit"},
            seq=3,
        ),
        shaped_event(
            "access_query",
            "access",
            1_060_000_000,
            {"request": "r-1", "attempt": 1, "route": "inline", "stage": "reply"},
            seq=4,
        ),
    ]
    report = derived(one_attempt_case(events=events))
    attribution = report["attribution"]
    assert attribution["summed_ms"] == pytest.approx(80.0)
    assert attribution["union_ms"] == pytest.approx(60.0)
    assert attribution["critical_path_ms"] == pytest.approx(60.0)
    assert attribution["overlap_ms"] == pytest.approx(20.0)


def test_latency_ac_12_mismatched_conditions_make_a_comparison_inconclusive():
    """latency/AC-12.mismatch: changed workload/deadline is never compared silently."""
    baseline = model.case_document(one_attempt_case())
    changed = one_attempt_case(deadline=200)
    changed["scenario"]["deadline_ms"] = 200
    changed["run"]["configuration"]["deadline_ms"] = 200
    candidate = model.case_document(changed)
    conditions = analysis.compare_conditions(baseline, candidate)
    assert conditions["match"] is False
    assert "configuration" in conditions["mismatches"]
    result = analysis.compare_runs([baseline], [candidate])
    assert result["verdict"] == "inconclusive"
    assert "conditions_mismatch" in result["reasons"]


def test_latency_ac_12_small_sample_p95_is_unavailable():
    """latency/AC-12.small-sample: a P95 below its 20-sample floor is labelled."""
    small = model.sample_entry([float(index) for index in range(19)])
    assert "p95" not in small["quantiles"]
    assert small["unavailable"]["p95"] == model.UNAVAILABLE_TAIL
    enough = model.sample_entry([float(index) for index in range(20)])
    assert enough["quantiles"]["p95"] == 18.0


def test_latency_ac_12_watch_and_segment_weighted_ids_are_distinct():
    """latency/AC-12.watch-vs-segment: Watch and segment percentiles keep separate IDs."""
    assert model.METRIC_CATALOG["max_display_delay_ms"]["weighting"] == "watch"
    assert model.METRIC_CATALOG["segment_display_delay_ms"]["weighting"] == "segment"
    facts = one_attempt_case()
    report = derived(facts)
    assert report["streaming"]["max_display_delay_ms"]["count"] == 1
    assert report["diagnostics"]["segment_display_delay_ms"]["count"] == 2


def test_latency_ac_12_offline_replay_matches_online(tmp_path):
    """latency/AC-12.offline-replay: serialized facts reproduce every business row."""
    facts = mixed_case(pairs=2)
    online = model.case_document(copy.deepcopy(facts))
    path = tmp_path / "replay.json"
    path.write_text(json.dumps(online))
    offline = model.read(path)
    assert offline["derived"] == online["derived"]


def test_latency_ac_12_cleanup_failure_preserves_a_passing_business_result():
    """latency/AC-12.cleanup-after-success: cleanup status never rewrites the outcome."""
    facts = one_attempt_case(cleanup={"result": "unverified", "reason": "staging_cleanup_failed"})
    report = derived(facts)
    assert report["attempts"][0]["outcome"] == "completed"
    assert report["outcomes"]["correct_by_deadline"] == 1


def test_latency_ac_12_mixed_arm_p95_requires_twenty_per_arm_and_forty_planned():
    """latency/AC-12.mixed-arm-floor: per-arm P95 plans 40 slots and needs 20/arm."""
    with pytest.raises(RepoError, match="at least 40 planned measured slots"):
        model.validate_case(mixed_case(pairs=15, per_arm=True))
    report = derived(mixed_case(pairs=20, per_arm=True))
    assert report["latency"]["per_arm_p95"] == {"idle": True, "bulk": True}
    assert report["latency"]["arms"]["idle"]["measured"] == 20
    assert report["latency"]["arms"]["bulk"]["measured"] == 20


def test_latency_ac_12_counterbalanced_pair_order_is_recorded_and_reproducible():
    """latency/AC-12.counterbalanced-order: seed parity and pair reversal survive."""
    report = derived(mixed_case(pairs=4, seed=0))
    workflow = report["workflow_results"]
    assert workflow["pair_counts"] == {"pairs": 4, "idle_first": 2, "bulk_first": 2}
    orders = [pair["order"] for pair in workflow["pairs"]]
    assert orders[0] == ["idle", "bulk"]
    assert orders[1] == ["bulk", "idle"]
    reversed_report = derived(mixed_case(pairs=4, seed=1))
    assert reversed_report["workflow_results"]["pair_counts"] == {
        "pairs": 4,
        "idle_first": 2,
        "bulk_first": 2,
    }
    assert reversed_report["workflow_results"]["pairs"][0]["order"] == ["bulk", "idle"]


def test_latency_ac_12_bulk_minus_idle_effect_keeps_its_sign_in_both_orders():
    """latency/AC-12.bulk-minus-idle: every delta is bulk minus idle, even bulk-first."""

    def outputs_factory(slot):
        if slot["arm"] != "bulk":
            return {}
        return {
            "outputs": [
                out(5, 0, bytes=10, prefix=10),
                out(40, 1, kind="terminal", bytes=10, prefix=20),
            ]
        }

    report = derived(mixed_case(pairs=2, outputs_factory=outputs_factory))
    deltas = report["workflow_results"]["deltas"]
    metric = "max_display_delay_ms"
    assert deltas["by_order"]["idle_first"][metric]["mean"] == pytest.approx(25.0)
    assert deltas["by_order"]["bulk_first"][metric]["mean"] == pytest.approx(25.0)
    assert deltas["pooled"][metric]["mean"] == pytest.approx(25.0)
    assert all(
        pair["deltas"][metric] == pytest.approx(25.0)
        for pair in report["workflow_results"]["pairs"]
    )


# ---------------------------------------------------------------------------
# AC-13: reviewed observer cost
# ---------------------------------------------------------------------------


def budget_evidence(differences, *, successes=20, coverage=1.0):
    return {
        "first_text_ms": {
            "pairs": [{"baseline": 100.0, "candidate": 100.0 + value} for value in differences],
            "baseline_successes": successes,
            "candidate_successes": successes,
            "coverage": {"baseline": coverage, "candidate": coverage},
        }
    }


def test_latency_ac_13_freeze_creates_a_versioned_budget():
    """latency/AC-13.freeze: a frozen budget declares units, floors and revision."""
    budget = analysis.budget_declaration(
        {"first_text_ms": {"upper_bound": 5.0, "unit": "ms"}},
        revision="budget-1",
        conditions_required={"workload": 8},
    )
    assert budget["revision"] == "budget-1"
    assert budget["metrics"]["first_text_ms"]["direction"] == "upper"
    assert budget["metrics"]["first_text_ms"]["sample_floor"] == 20
    assert analysis.validate_budget(budget)["state"] == "current"


def test_latency_ac_13_paired_upper_bound_within_budget():
    """latency/AC-13.paired: five matched pairs and 20 successes pass a loose bound."""
    budget = analysis.budget_declaration({"first_text_ms": 2.0}, revision="b-1")
    result = analysis.evaluate_budget(budget, budget_evidence([1.0] * 5))
    entry = result["metrics"]["first_text_ms"]
    assert result["state"] == "within"
    assert entry["verdict"] == "within"
    assert entry["upper_bound_observed"] == pytest.approx(1.0)
    assert entry["pairs"] == 5


def test_latency_ac_13_breach_uses_a_one_sided_upper_bound():
    """latency/AC-13.breach: an upper bound beyond the frozen tolerance fails."""
    budget = analysis.budget_declaration({"first_text_ms": 2.0}, revision="b-1")
    result = analysis.evaluate_budget(budget, budget_evidence([10.0] * 5))
    assert result["state"] == "breach"
    assert result["metrics"]["first_text_ms"]["verdict"] == "breach"
    assert result["metrics"]["first_text_ms"]["upper_bound_observed"] > 2.0


def test_latency_ac_13_stale_budget_is_inconclusive():
    """latency/AC-13.stale: an expired or condition-changed budget cannot pass."""
    budget = analysis.budget_declaration(
        {"first_text_ms": 5.0}, revision="b-1", expires_at="2020-01-01"
    )
    result = analysis.evaluate_budget(budget, budget_evidence([1.0] * 5), now="2026-01-01")
    assert result["state"] == "inconclusive"
    assert "budget_expired" in result["reasons"]
    budget = analysis.budget_declaration(
        {"first_text_ms": 5.0}, revision="b-1", conditions_required={"workload": 8}
    )
    result = analysis.evaluate_budget(
        budget, budget_evidence([1.0] * 5), current_conditions={"workload": 7}
    )
    assert result["state"] == "inconclusive"
    assert "conditions_changed" in result["reasons"]


def test_latency_ac_13_inconclusive_for_insufficient_pairs_or_successes():
    """latency/AC-13.inconclusive: missing pairs, samples or coverage never pass."""
    budget = analysis.budget_declaration(
        {"first_text_ms": {"upper_bound": 5.0, "coverage_floor": 0.99}}, revision="b-1"
    )
    too_few_pairs = analysis.evaluate_budget(budget, budget_evidence([1.0] * 3))
    assert too_few_pairs["metrics"]["first_text_ms"]["reason"] == "insufficient_pairs"
    too_few_samples = analysis.evaluate_budget(budget, budget_evidence([1.0] * 5, successes=10))
    assert too_few_samples["metrics"]["first_text_ms"]["reason"] == "insufficient_successes"
    bad_coverage = analysis.evaluate_budget(budget, budget_evidence([1.0] * 5, coverage=0.5))
    assert bad_coverage["metrics"]["first_text_ms"]["reason"] == "insufficient_coverage"
    assert bad_coverage["state"] == "inconclusive"


def test_latency_ac_13_disabled_path_regression_is_judged_separately():
    """latency/AC-13.disabled-path-regression: off-mode cost is its own scope."""
    budget = analysis.budget_declaration(
        {"first_text_ms": {"upper_bound": 2.0, "scope": "disabled_path"}},
        revision="b-1",
        scope="disabled_path",
    )
    result = analysis.evaluate_budget(budget, budget_evidence([8.0] * 5))
    assert result["state"] == "breach"
    assert result["metrics"]["first_text_ms"]["scope"] == "disabled_path"


def test_latency_ac_13_unmatched_predecessor_is_inconclusive():
    """latency/AC-13.unmatched-predecessor: no baseline identity means no verdict."""
    budget = analysis.budget_declaration({"first_text_ms": 5.0}, revision="b-1")
    result = analysis.release_comparison(
        None,
        {"revision": "candidate"},
        budget,
        budget_evidence([1.0] * 5),
    )
    assert result["state"] == "inconclusive"
    assert "unmatched_predecessor" in result["reasons"]


def test_latency_ac_13_release_comparison_records_the_baseline_identity():
    """latency/AC-13.paired: release verdict names predecessor and candidate revisions."""
    budget = analysis.budget_declaration({"first_text_ms": 5.0}, revision="b-1")
    result = analysis.release_comparison(
        {"revision": "before", "fingerprint": "f-before"},
        {"revision": "after", "fingerprint": "f-after"},
        budget,
        budget_evidence([1.0] * 5),
    )
    assert result["baseline"]["revision"] == "before"
    assert result["candidate"]["revision"] == "after"
    assert result["state"] == "within"


def test_latency_ac_13_aa_calibration_uses_the_noise_bound():
    """latency/AC-13.aa: A/A noise inside the declared bound calibrates."""
    pairs = [{"baseline": 100.0, "candidate": 101.0} for _ in range(5)]
    assert analysis.aa_comparison(pairs, tolerance_ms=2.0)["state"] == "calibrated"
    noisy = [{"baseline": 100.0, "candidate": 110.0} for _ in range(5)]
    result = analysis.aa_comparison(noisy, tolerance_ms=2.0)
    assert result["state"] == "inconclusive"
    assert result["reason"] == "noise_bound_exceeded"


def test_latency_ac_13_missing_required_boundary_blocks_diagnostic_pass():
    """latency/AC-13.missing-required-boundary: coverage gates the diagnostic verdict."""
    trace = {
        "coverage": {
            "preview": {
                "required": ["fixture_emit", "host_enqueue"],
                "observed": ["fixture_emit"],
                "complete": False,
            }
        },
        "loss": {"trace_bytes": 8},
    }
    report = derived(one_attempt_case(trace=trace))
    preview = report["coverage"]["paths"]["preview"]
    assert preview["covered"] == 0
    assert preview["complete"] is False
    assert preview["reason"] == "missing_required_boundary"
    assert report["quality"]["diagnostic_pass"] is False


def test_latency_ac_13_whole_trace_loss_counts_in_the_denominator():
    """latency/AC-13.whole-trace-loss: a lost trace attempt is eligible but uncovered."""
    facts = many_case(2, lambda index: {})
    for attempt in facts["attempts"]:
        attempt.pop("trace", None)
    report = derived(facts)
    preview = report["coverage"]["paths"]["preview"]
    assert preview["eligible"] == 2
    assert preview["covered"] == 0
    assert preview["missing"]["whole_trace_loss"] == 2
    assert report["quality"]["diagnostic_pass"] is False


def test_latency_ac_13_inapplicable_stage_is_exempt_only_with_evidence():
    """latency/AC-13.inapplicable-stage: a reasoned inapplicable stage is exempt."""
    trace = {
        "coverage": {
            "preview": {
                "required": ["fixture_emit", "access_query"],
                "observed": ["fixture_emit"],
                "inapplicable": {"access_query": "direct preview has no Access interval"},
                "complete": True,
            }
        },
        "loss": {},
    }
    report = derived(one_attempt_case(trace=trace))
    assert report["coverage"]["paths"]["preview"]["covered"] == 1
    assert report["coverage"]["paths"]["preview"]["complete"] is True


# ---------------------------------------------------------------------------
# AC-07: operational aggregation and mode availability
# ---------------------------------------------------------------------------


def test_latency_ac_07_aggregate_mode_keeps_external_metrics():
    """latency/AC-07.aggregate: aggregate mode reports external rows, no attribution."""
    report = derived(one_attempt_case(mode="aggregate"))
    assert report["attempts"][0]["outcome"] == "completed"
    assert report["attribution"]["available"] is False
    assert report["attribution"]["mode"] == "aggregate"
    assert report["quality"]["capture"]["attribution_available"] is False
    assert "attribution_unavailable_aggregate" in report["quality"]["limitations"]


def test_latency_ac_07_off_mode_keeps_external_metrics():
    """latency/AC-07.off: off mode still publishes business outcomes and latency."""
    report = derived(one_attempt_case(mode="off"))
    assert report["outcomes"]["correct_by_deadline"] == 1
    assert report["latency"]["samples"]["first_text_ms"]["count"] == 1
    assert report["attribution"]["available"] is False
    assert "attribution_unavailable_off" in report["quality"]["limitations"]


def test_latency_ac_07_mode_availability_requires_trace_capture():
    """latency/AC-07.mode-availability: only trace capture enables attribution."""
    trace = derived(one_attempt_case(mode="trace"))
    aggregate = derived(one_attempt_case(mode="aggregate"))
    off = derived(one_attempt_case(mode="off"))
    assert trace["quality"]["capture"]["attribution_available"] is True
    assert aggregate["quality"]["capture"]["attribution_available"] is False
    assert off["quality"]["capture"]["attribution_available"] is False


def test_latency_ac_07_cardinality_counts_bounded_identities():
    """latency/AC-07.cardinality: distinct streams, blocks, requests and watches are counted."""
    events = [
        shaped_event(
            "host_enqueue",
            "host",
            1_000_000_000,
            {"stream": "s-1", "block": "b-1", "seq_start": 0, "seq_end": 1, "bytes": 10},
        ),
        shaped_event(
            "host_enqueue",
            "host",
            1_000_000_001,
            {"stream": "s-2", "block": "b-2", "seq_start": 0, "seq_end": 1, "bytes": 10},
            seq=2,
        ),
        shaped_event(
            "core_join",
            "core",
            1_000_000_002,
            {"watch": "w-1", "connection": "c-1", "generation": 1, "milestone": "watch_ready"},
            seq=3,
        ),
        shaped_event(
            "core_join",
            "core",
            1_000_000_003,
            {"watch": "w-2", "connection": "c-1", "generation": 1, "milestone": "watch_ready"},
            seq=4,
        ),
        shaped_event(
            "access_query",
            "access",
            1_000_000_004,
            {"request": "r-1", "attempt": 1, "route": "inline", "stage": "admit"},
            seq=5,
        ),
    ]
    report = derived(one_attempt_case(events=events))
    assert report["attribution"]["cardinality"] == {
        "events": 5,
        "streams": 2,
        "blocks": 2,
        "requests": 1,
        "watches": 2,
    }


def test_latency_ac_07_restart_is_explicit_in_quality():
    """latency/AC-07.restart: a verified producer replacement stays explicit."""
    producers = [
        {"producer": "fixture", "lifetime": "lt-1", "verified": True},
        {"producer": "fixture", "lifetime": "lt-2", "verified": True},
    ]
    report = derived(one_attempt_case(clock_producers=producers))
    assert report["quality"]["clock"]["restarted"] is True
    assert [item["lifetime"] for item in report["quality"]["clock"]["producers"]] == [
        "lt-1",
        "lt-2",
    ]


def test_latency_ac_07_concurrency_is_bounded_by_distinct_watches():
    """latency/AC-07.concurrency: concurrent watches are counted once each."""
    events = [
        shaped_event(
            "core_join",
            "core",
            1_000_000_000 + index,
            {
                "watch": f"w-{index % 2}",
                "connection": "c-1",
                "generation": 1,
                "milestone": "watch_ready",
            },
            seq=index + 1,
        )
        for index in range(4)
    ]
    report = derived(one_attempt_case(events=events))
    assert report["attribution"]["cardinality"]["watches"] == 2
    assert report["attribution"]["cardinality"]["events"] == 4


def test_latency_ac_05_concurrent_scopes_use_their_own_join_keys():
    """latency/AC-05.concurrent: model-asserted current-schema behavior."""
    events = [
        shaped_event(
            "access_query",
            "access",
            1_000_000_000,
            {"request": "r-1", "attempt": 1, "route": "inline", "stage": "admit"},
        ),
        shaped_event(
            "access_query",
            "access",
            1_005_000_000,
            {"request": "r-1", "attempt": 1, "route": "inline", "stage": "reply"},
            seq=2,
        ),
        shaped_event(
            "access_query",
            "access",
            1_000_000_000,
            {"request": "r-2", "attempt": 2, "route": "inline", "stage": "admit"},
            seq=3,
        ),
        shaped_event(
            "access_query",
            "access",
            1_007_000_000,
            {"request": "r-2", "attempt": 2, "route": "inline", "stage": "reply"},
            seq=4,
        ),
    ]
    report = derived(one_attempt_case(events=events))
    intervals = [
        item for item in report["attribution"]["intervals"] if item["name"] == "access_forward_ms"
    ]
    assert sorted(item["duration_ms"] for item in intervals) == [5.0, 7.0]
    assert report["attribution"]["unresolved"] == []


# ---------------------------------------------------------------------------
# Structural validation: every malformed branch fails before derivation
# ---------------------------------------------------------------------------


def _set(facts, *path, value):
    target = facts
    for key in path[:-1]:
        target = target[key]
    target[path[-1]] = value
    return facts


def _drop(facts, *path):
    target = facts
    for key in path[:-1]:
        target = target[key]
    target.pop(path[-1], None)
    return facts


STRUCTURAL_REJECTIONS = [
    ("not-an-object", lambda f: [], "document is not an object"),
    ("unknown-kind", lambda f: _set(f, "kind", value="other"), "unknown document kind"),
    ("raw-derived", lambda f: _set(f, "derived", value={}), "must not contain derived"),
    ("missing-run", lambda f: _drop(f, "run"), "missing section run"),
    ("run-not-object", lambda f: _set(f, "run", value=[]), "run is not an object"),
    ("run-id", lambda f: _set(f, "run", "id", value=""), "run id must be text"),
    ("surface", lambda f: _set(f, "run", "surface", value="x"), "unknown observation surface"),
    ("endpoint", lambda f: _set(f, "run", "endpoint", value="x"), "unknown observation endpoint"),
    (
        "run-mode",
        lambda f: _set(f, "run", "observation_mode", value="x"),
        "unknown observation mode",
    ),
    (
        "config-object",
        lambda f: _set(f, "run", "configuration", value=[]),
        "configuration is not an object",
    ),
    (
        "config-missing",
        lambda f: _drop(f, "run", "configuration", "cadence_ms"),
        "missing cadence_ms",
    ),
    (
        "concurrency",
        lambda f: _set(f, "run", "configuration", "concurrency", value=2),
        "sequential execution",
    ),
    (
        "attempts-zero",
        lambda f: _set(f, "run", "configuration", "attempts", value=0),
        "attempts must be a positive integer",
    ),
    (
        "warmup-negative",
        lambda f: _set(f, "run", "configuration", "warmup_attempts", value=-1),
        "warmup_attempts must be a non-negative integer",
    ),
    (
        "profile",
        lambda f: _set(f, "run", "configuration", "profile", value=""),
        "profile must be text",
    ),
    (
        "workflow",
        lambda f: _set(f, "run", "configuration", "workflow", value="other"),
        "unknown workflow",
    ),
    (
        "config-mode",
        lambda f: _set(f, "run", "configuration", "observation_mode", value="other"),
        "unknown configuration observation mode",
    ),
    (
        "mode-disagrees",
        lambda f: _set(f, "run", "configuration", "observation_mode", value="off"),
        "observation mode disagrees",
    ),
    (
        "deadline-zero",
        lambda f: _set(f, "run", "configuration", "deadline_ms", value=0),
        "deadline_ms must be a positive number",
    ),
    (
        "window-negative",
        lambda f: _set(f, "run", "configuration", "observation_window_ms", value=-1),
        "observation_window_ms must be a number",
    ),
    (
        "cadence-zero",
        lambda f: _set(f, "run", "configuration", "cadence_ms", value=0),
        "cadence_ms must be a positive integer",
    ),
    (
        "seed-type",
        lambda f: _set(f, "run", "configuration", "seed", value=[]),
        "seed must be an integer or text",
    ),
    (
        "network-profile",
        lambda f: _set(f, "run", "configuration", "network", "profile", value="other"),
        "unknown benchmark network settings",
    ),
    (
        "network-canonical",
        lambda f: _set(f, "run", "configuration", "network", "extra_rtt_ms", value=5),
        "network settings are not canonical",
    ),
    (
        "fingerprint-object",
        lambda f: _set(f, "run", "fingerprint", value=[]),
        "fingerprint is not an object",
    ),
    (
        "build-identity",
        lambda f: _drop(f, "run", "fingerprint", "build", "revision"),
        "requires a build identity",
    ),
    (
        "schemas-object",
        lambda f: _set(f, "run", "fingerprint", "schemas", value=[]),
        "requires schemas",
    ),
    (
        "schemas-version",
        lambda f: _set(f, "run", "fingerprint", "schemas", "case", value=1),
        "fingerprint schemas",
    ),
    (
        "fingerprint-section",
        lambda f: _drop(f, "run", "fingerprint", "fixture"),
        "fixture is not an object",
    ),
    (
        "fingerprint-clock",
        lambda f: _drop(f, "run", "fingerprint", "clock", "source"),
        "clock is missing source",
    ),
    (
        "scenario-id",
        lambda f: _set(f, "scenario", "id", value="other"),
        "unknown scenario identity",
    ),
    ("scenario-version", lambda f: _set(f, "scenario", "version", value=7), "scenario version"),
    ("scenario-seed", lambda f: _set(f, "scenario", "seed", value="9"), "scenario seed disagrees"),
    (
        "scenario-deadline",
        lambda f: _set(f, "scenario", "deadline_ms", value=99),
        "scenario deadline_ms disagrees",
    ),
    (
        "scenario-window",
        lambda f: _set(f, "scenario", "observation_window_ms", value=99),
        "observation_window_ms disagrees",
    ),
    (
        "scenario-network",
        lambda f: _set(f, "scenario", "network", value=model.network_settings("normal", "client")),
        "scenario network disagrees",
    ),
    (
        "workload-object",
        lambda f: _set(f, "scenario", "workload", value=[]),
        "workload is not an object",
    ),
    (
        "workload-missing",
        lambda f: _drop(f, "scenario", "workload", "gap_ms"),
        "workload is missing gap_ms",
    ),
    (
        "workload-updates",
        lambda f: _set(f, "scenario", "workload", "updates", value=[]),
        "updates must be a nonempty list",
    ),
    (
        "workload-gap",
        lambda f: _set(f, "scenario", "workload", "gap_ms", value=-1),
        "gap_ms must be a number",
    ),
    (
        "workload-delivery",
        lambda f: _set(f, "scenario", "workload", "delivery", value="accepted"),
        "requires immediate delivery",
    ),
    (
        "workload-workflow",
        lambda f: _set(f, "scenario", "workload", "workflow", value="recovery"),
        "workflow disagrees with the run",
    ),
    (
        "workload-controls",
        lambda f: _set(f, "scenario", "workload", "controls", value=[]),
        "workload controls must be an object",
    ),
    (
        "response-object",
        lambda f: _set(f, "scenario", "response", value=[]),
        "response is not an object",
    ),
    (
        "response-missing",
        lambda f: _drop(f, "scenario", "response", "start_ms"),
        "response is missing start_ms",
    ),
    (
        "response-number",
        lambda f: _set(f, "scenario", "response", "start_ms", value=-1),
        "response start_ms must be a number",
    ),
    (
        "response-text",
        lambda f: _set(f, "scenario", "response", "final_text", value=""),
        "final_text must be text",
    ),
    (
        "response-mismatch",
        lambda f: _set(f, "scenario", "response", "final_text", value="nope"),
        "must equal the scripted updates",
    ),
    (
        "response-order",
        lambda f: _set(f, "scenario", "response", "final_ms", value=0),
        "final_ms precedes its last update",
    ),
    (
        "environment-object",
        lambda f: _set(f, "environment", value=[]),
        "environment is not an object",
    ),
    (
        "environment-missing",
        lambda f: _drop(f, "environment", "artifacts"),
        "environment is missing artifacts",
    ),
    (
        "artifacts-empty",
        lambda f: _set(f, "environment", "artifacts", value={}),
        "artifacts must map names",
    ),
    (
        "topology-kind",
        lambda f: _set(f, "environment", "topology", "kind", value="remote"),
        "only a single-machine capture",
    ),
    (
        "topology-clock",
        lambda f: _set(f, "environment", "topology", "clock", value="realtime"),
        "one CLOCK_MONOTONIC timeline",
    ),
    (
        "clock-missing",
        lambda f: _drop(f, "environment", "clock", "source"),
        "clock is missing source",
    ),
    (
        "clock-unit",
        lambda f: _set(f, "environment", "clock", "unit", value="us"),
        "only integer nanoseconds",
    ),
    (
        "clock-flags",
        lambda f: _set(f, "environment", "clock", "boot_id_verified", value="yes"),
        "verification flags must be booleans",
    ),
    (
        "clock-producers",
        lambda f: _set(f, "environment", "clock", "producers", value={}),
        "clock producers must be a list",
    ),
    (
        "clock-producer-role",
        lambda f: _set(f, "environment", "clock", "producers", value=[{}]),
        "need a producer role",
    ),
    (
        "clock-producer-lifetime",
        lambda f: _set(f, "environment", "clock", "producers", value=[{"producer": "fixture"}]),
        "need a lifetime",
    ),
    (
        "clock-producer-verified",
        lambda f: _set(
            f,
            "environment",
            "clock",
            "producers",
            value=[{"producer": "fixture", "lifetime": "l", "verified": "yes"}],
        ),
        "verified must be a boolean",
    ),
    (
        "clock-producer-clock",
        lambda f: _set(
            f,
            "environment",
            "clock",
            "producers",
            value=[
                {
                    "producer": "fixture",
                    "lifetime": "l",
                    "verified": True,
                    "clock": "CLOCK_REALTIME",
                }
            ],
        ),
        "capture accepts one clock",
    ),
    (
        "clock-producer-unit",
        lambda f: _set(
            f,
            "environment",
            "clock",
            "producers",
            value=[{"producer": "fixture", "lifetime": "l", "verified": True, "unit": "us"}],
        ),
        "capture accepts one unit",
    ),
    (
        "clock-producer-duplicate",
        lambda f: _set(
            f,
            "environment",
            "clock",
            "producers",
            value=[
                {"producer": "fixture", "lifetime": "l", "verified": True},
                {"producer": "fixture", "lifetime": "l", "verified": True},
            ],
        ),
        "lifetime is duplicated",
    ),
    (
        "clock-producer-reason",
        lambda f: _set(
            f,
            "environment",
            "clock",
            "producers",
            value=[{"producer": "fixture", "lifetime": "l", "verified": True, "reason": 1}],
        ),
        "reason must be text",
    ),
    (
        "probe-object",
        lambda f: _set(f, "environment", "network_probe", value=[]),
        "network probe is not an object",
    ),
    (
        "probe-samples",
        lambda f: _set(f, "environment", "network_probe", value={"reference_ms": [1.0]}),
        "requires positive TCP RTT samples",
    ),
    (
        "counters",
        lambda f: _set(f, "environment", "network_counters", value=[]),
        "network_counters must be an object",
    ),
    (
        "sampler",
        lambda f: _set(f, "environment", "sampler", value=[]),
        "environment sampler must be an object",
    ),
    (
        "origin",
        lambda f: _set(f, "environment", "capture_origin_ns", value=-1),
        "capture_origin_ns must be a non-negative integer",
    ),
]


@pytest.mark.parametrize(
    ("name", "mutate", "match"),
    STRUCTURAL_REJECTIONS,
    ids=[item[0] for item in STRUCTURAL_REJECTIONS],
)
def test_structural_rejections(name, mutate, match):
    """latency/AC-08.current: malformed raw facts never reach derivation."""
    facts = mutate(one_attempt_case())
    with pytest.raises(RepoError, match=match):
        model.validate_case(facts)


SCHEDULE_REJECTIONS = [
    ("object", lambda f: _set(f, "schedule", value=[]), "schedule is not an object"),
    ("seed-missing", lambda f: _drop(f, "schedule", "seed"), "schedule is missing seed"),
    ("seed-mismatch", lambda f: _set(f, "schedule", "seed", value="9"), "schedule seed disagrees"),
    (
        "rule",
        lambda f: _set(f, "schedule", "rule", value="random"),
        "schedule rule must be alternating",
    ),
    (
        "slots-empty",
        lambda f: _set(f, "schedule", "slots", value=[]),
        "slots must be a nonempty list",
    ),
    (
        "slot-object",
        lambda f: _set(f, "schedule", "slots", value=[[]]),
        "schedule slot is not an object",
    ),
    (
        "slot-missing",
        lambda f: _set(f, "schedule", "slots", value=[{"slot": 0}]),
        "schedule slot is missing phase",
    ),
    (
        "slot-negative",
        lambda f: _set(f, "schedule", "slots", 0, "slot", value=-1),
        "ordinal must be a non-negative integer",
    ),
    (
        "slot-phase",
        lambda f: _set(f, "schedule", "slots", 0, "phase", value="other"),
        "unknown phase",
    ),
    ("slot-arm", lambda f: _set(f, "schedule", "slots", 0, "arm", value="other"), "unknown arm"),
    ("slot-pair", lambda f: _set(f, "schedule", "slots", 0, "pair", value=""), "pair must be text"),
    (
        "slot-order",
        lambda f: _set(f, "schedule", "slots", 0, "order", value=2),
        "order must be 0 or 1",
    ),
    (
        "slot-start",
        lambda f: _set(f, "schedule", "slots", 0, "planned_start_ms", value=-1),
        "planned_start_ms must be a number",
    ),
    (
        "measured-count",
        lambda f: _set(f, "run", "configuration", "attempts", value=2),
        "measured slots disagree",
    ),
    (
        "warmup-count",
        lambda f: _set(f, "run", "configuration", "warmup_attempts", value=1),
        "warmup slots disagree",
    ),
    (
        "nonmixed-arm",
        lambda f: _set(f, "schedule", "slots", 0, "arm", value="bulk"),
        "use the watch arm",
    ),
]


@pytest.mark.parametrize(
    ("name", "mutate", "match"),
    SCHEDULE_REJECTIONS,
    ids=[item[0] for item in SCHEDULE_REJECTIONS],
)
def test_schedule_rejections(name, mutate, match):
    """latency/AC-09.defaults: the persisted schedule is validated strictly."""
    facts = one_attempt_case()
    mutate(facts)
    with pytest.raises(RepoError, match=match):
        model.validate_case(facts)


ATTEMPT_REJECTIONS = [
    ("not-list", lambda f: _set(f, "attempts", value={}), "attempts must be a list"),
    ("not-object", lambda f: _set(f, "attempts", value=[[]]), "attempt is not an object"),
    ("missing", lambda f: _set(f, "attempts", value=[{"id": "a-0"}]), "attempt is missing slot"),
    (
        "duplicate-id",
        lambda f: _set(f, "attempts", value=[f["attempts"][0], copy.deepcopy(f["attempts"][0])]),
        "duplicated",
    ),
    (
        "slot-unknown",
        lambda f: _set(f, "attempts", 0, "slot", value=9),
        "not in the persisted schedule",
    ),
    (
        "phase",
        lambda f: _set(f, "attempts", 0, "phase", value="warmup"),
        "disagrees with its scheduled slot",
    ),
    (
        "arm",
        lambda f: _set(f, "attempts", 0, "arm", value="idle"),
        "disagrees with its scheduled slot",
    ),
    (
        "pair",
        lambda f: _set(f, "attempts", 0, "pair_id", value="other"),
        "disagrees with its scheduled slot",
    ),
    ("status", lambda f: _set(f, "attempts", 0, "status", value="other"), "unknown attempt status"),
    ("failure", lambda f: _set(f, "attempts", 0, "failure", value=[]), "failure must be an object"),
    (
        "attribution",
        lambda f: _set(f, "attempts", 0, "failure", "attribution", value="other"),
        "unknown failure attribution",
    ),
    (
        "reason",
        lambda f: _set(f, "attempts", 0, "failure", "reason", value=1),
        "failure reason must be text",
    ),
    ("setup", lambda f: _set(f, "attempts", 0, "setup", value=[]), "setup requires an errors list"),
    (
        "errors",
        lambda f: _set(f, "attempts", 0, "setup", "errors", value="x"),
        "setup requires an errors list",
    ),
    (
        "error-text",
        lambda f: _set(f, "attempts", 0, "setup", "errors", value=[1]),
        "setup errors must be text",
    ),
    ("source", lambda f: _set(f, "attempts", 0, "source", value=[]), "source is not an object"),
    (
        "times",
        lambda f: _set(f, "attempts", 0, "source", "times_ms", value=["x"]),
        "times_ms must be numbers",
    ),
    (
        "times-order",
        lambda f: _set(f, "attempts", 0, "source", "times_ms", value=[10, 0]),
        "source times must be ordered",
    ),
    (
        "sizes",
        lambda f: _set(f, "attempts", 0, "source", "sizes", value=[-1, 1]),
        "sizes must be non-negative",
    ),
    ("length", lambda f: _set(f, "attempts", 0, "source", "sizes", value=[1]), "equal length"),
    ("outputs", lambda f: _set(f, "attempts", 0, "outputs", value={}), "outputs must be a list"),
    (
        "output-object",
        lambda f: _set(f, "attempts", 0, "outputs", value=[[]]),
        "output is not an object",
    ),
    (
        "output-missing",
        lambda f: _set(f, "attempts", 0, "outputs", value=[{"at_ms": 1}]),
        "output is missing ordinal",
    ),
    (
        "output-time",
        lambda f: _set(f, "attempts", 0, "outputs", 0, "at_ms", value=-1),
        "output at_ms must be a number",
    ),
    (
        "output-ordinal",
        lambda f: _set(f, "attempts", 0, "outputs", 0, "ordinal", value=-1),
        "ordinal must be a non-negative integer",
    ),
    (
        "output-kind",
        lambda f: _set(f, "attempts", 0, "outputs", 0, "kind", value="other"),
        "unknown output kind",
    ),
    (
        "output-bytes",
        lambda f: _set(f, "attempts", 0, "outputs", 0, "bytes", value=-1),
        "bytes must be a non-negative integer",
    ),
    (
        "output-prefix",
        lambda f: _set(f, "attempts", 0, "outputs", 0, "prefix_bytes", value=-1),
        "prefix_bytes must be a non-negative integer",
    ),
    (
        "output-correct",
        lambda f: _set(f, "attempts", 0, "outputs", 0, "correct", value="yes"),
        "correct must be a boolean",
    ),
    (
        "output-block",
        lambda f: _set(f, "attempts", 0, "outputs", 0, "block", value=""),
        "block must be text",
    ),
    (
        "output-order",
        lambda f: (
            _set(f, "attempts", 0, "outputs", 0, "at_ms", value=15),
            _set(f, "attempts", 0, "outputs", 0, "ordinal", value=2),
        ),
        "ordered by time and ordinal",
    ),
    (
        "output-prefix-high",
        lambda f: _set(f, "attempts", 0, "outputs", 0, "prefix_bytes", value=999),
        "prefix exceeds",
    ),
    (
        "observation",
        lambda f: _set(f, "attempts", 0, "observation", value=[]),
        "observation is not an object",
    ),
    (
        "observation-missing",
        lambda f: _drop(f, "attempts", 0, "observation", "started_ms"),
        "observation is missing started_ms",
    ),
    (
        "observation-started",
        lambda f: _set(f, "attempts", 0, "observation", "started_ms", value=None),
        "started_ms is required after start",
    ),
    (
        "observation-started-negative",
        lambda f: _set(f, "attempts", 0, "observation", "started_ms", value=-1),
        "started_ms must be a number",
    ),
    (
        "observation-end",
        lambda f: (
            _set(f, "attempts", 0, "observation", "started_ms", value=10),
            _set(f, "attempts", 0, "observation", "ended_ms", value=5),
        ),
        "end precedes its start",
    ),
    (
        "observation-cutoff-start",
        lambda f: (
            _set(f, "attempts", 0, "observation", "started_ms", value=10),
            _set(f, "attempts", 0, "observation", "cutoff_ms", value=5),
        ),
        "cutoff precedes its start",
    ),
    (
        "observation-cutoff-deadline",
        lambda f: _set(f, "attempts", 0, "observation", "cutoff_ms", value=999),
        "must not exceed the deadline",
    ),
    (
        "observation-early",
        lambda f: _set(f, "attempts", 0, "observation", "early_stop", value="other"),
        "unknown observation early_stop",
    ),
    (
        "observation-ns",
        lambda f: _set(f, "attempts", 0, "observation", "started_ns", value=-1),
        "started_ns must be a non-negative integer",
    ),
    (
        "observation-ns-order",
        lambda f: (
            _set(f, "attempts", 0, "observation", "started_ns", value=10),
            _set(f, "attempts", 0, "observation", "ended_ns", value=1),
        ),
        "absolute observation end precedes",
    ),
    (
        "observation-ns-cutoff-start",
        lambda f: (
            _set(f, "attempts", 0, "observation", "started_ns", value=10),
            _set(f, "attempts", 0, "observation", "cutoff_ns", value=5),
        ),
        "absolute cutoff precedes",
    ),
    (
        "observation-ns-cutoff-end",
        lambda f: (
            _set(f, "attempts", 0, "observation", "started_ns", value=0),
            _set(f, "attempts", 0, "observation", "ended_ns", value=10),
            _set(f, "attempts", 0, "observation", "cutoff_ns", value=999),
        ),
        "absolute cutoff exceeds",
    ),
]


@pytest.mark.parametrize(
    ("name", "mutate", "match"),
    ATTEMPT_REJECTIONS,
    ids=[item[0] for item in ATTEMPT_REJECTIONS],
)
def test_attempt_rejections(name, mutate, match):
    """latency/AC-15.640-220-cutoff: malformed attempt facts are rejected."""
    facts = one_attempt_case()
    mutate(facts)
    with pytest.raises(RepoError, match=match):
        model.validate_case(facts)


READ_REJECTIONS = [
    ("reads", lambda f: _set(f, "attempts", 0, "reads", value={}), "reads must be a list"),
    (
        "read-object",
        lambda f: _set(f, "attempts", 0, "reads", value=[[]]),
        "read observation is not an object",
    ),
    (
        "read-missing",
        lambda f: _set(f, "attempts", 0, "reads", value=[{"id": "r"}]),
        "read observation is missing op",
    ),
    (
        "read-id",
        lambda f: _set(f, "attempts", 0, "reads", value=[{**read_observation("r-1"), "id": ""}]),
        "read identity",
    ),
    (
        "read-op",
        lambda f: _set(
            f, "attempts", 0, "reads", value=[{**read_observation("r-1"), "op": "other"}]
        ),
        "unknown read operation",
    ),
    (
        "read-started",
        lambda f: _set(
            f, "attempts", 0, "reads", value=[{**read_observation("r-1"), "started_at_ms": -1}]
        ),
        "started_at_ms must be a number",
    ),
    (
        "read-early",
        lambda f: _set(
            f,
            "attempts",
            0,
            "reads",
            value=[{**read_observation("r-1"), "scheduled_at_ms": 10, "started_at_ms": 5}],
        ),
        "cannot start before",
    ),
    (
        "read-finish",
        lambda f: _set(
            f,
            "attempts",
            0,
            "reads",
            value=[{**read_observation("r-1"), "started_at_ms": 10, "finished_at_ms": 5}],
        ),
        "finish before",
    ),
    (
        "read-pages",
        lambda f: _set(f, "attempts", 0, "reads", value=[{**read_observation("r-1"), "pages": -1}]),
        "pages must be a non-negative integer",
    ),
    (
        "read-route",
        lambda f: _set(
            f, "attempts", 0, "reads", value=[{**read_observation("r-1"), "route": "other"}]
        ),
        "unknown read route",
    ),
    (
        "read-outcome",
        lambda f: _set(
            f, "attempts", 0, "reads", value=[{**read_observation("r-1"), "outcome": "other"}]
        ),
        "unknown read outcome",
    ),
    (
        "read-verified",
        lambda f: _set(
            f, "attempts", 0, "reads", value=[{**read_observation("r-1"), "verified": "yes"}]
        ),
        "verified and corrupt must be booleans",
    ),
    (
        "read-repeats",
        lambda f: _set(
            f, "attempts", 0, "reads", value=[{**read_observation("r-1"), "token_repeats": -1}]
        ),
        "token_repeats must be a non-negative integer",
    ),
    (
        "read-bytes",
        lambda f: _set(
            f, "attempts", 0, "reads", value=[{**read_observation("r-1"), "bytes_verified": 1}]
        ),
        "must verify the expected bytes",
    ),
    (
        "read-code",
        lambda f: _set(
            f,
            "attempts",
            0,
            "reads",
            value=[
                {**read_observation("r-1"), "outcome": "product_error", "error_code": "Bad/Code"}
            ],
        ),
        "invalid read error code",
    ),
]


@pytest.mark.parametrize(
    ("name", "mutate", "match"),
    READ_REJECTIONS,
    ids=[item[0] for item in READ_REJECTIONS],
)
def test_read_rejections(name, mutate, match):
    """latency/AC-09.failed-read: read facts are validated before aggregation."""
    facts = one_attempt_case()
    mutate(facts)
    with pytest.raises(RepoError, match=match):
        model.validate_case(facts)


TRACE_REJECTIONS = [
    ("trace", lambda f: _set(f, "trace", value=[]), "trace is not an object"),
    ("complete", lambda f: _drop(f, "trace", "complete"), "trace complete must be a boolean"),
    ("path", lambda f: _set(f, "trace", "path", value=1), "trace path must be text or null"),
    (
        "bytes",
        lambda f: _set(f, "trace", "bytes", value=-1),
        "trace bytes must be a non-negative integer",
    ),
    ("losses", lambda f: _set(f, "trace", "losses", value=[]), "trace losses must be an object"),
    (
        "loss-value",
        lambda f: _set(f, "trace", "losses", value={"x": -1}),
        "loss counters must be named non-negative integers",
    ),
    (
        "coverage",
        lambda f: _set(f, "trace", "coverage", value=[]),
        "trace coverage must be an object",
    ),
    ("status", lambda f: _set(f, "status", value=[]), "status needs a known state"),
    (
        "status-state",
        lambda f: _set(f, "status", "state", value="other"),
        "status needs a known state",
    ),
    ("reasons", lambda f: _set(f, "status", "reasons", value=[1]), "status reasons must be text"),
    (
        "incomplete-reason",
        lambda f: _set(f, "status", "state", value="incomplete"),
        "must state its reasons",
    ),
    (
        "cleanup",
        lambda f: _set(f, "status", "cleanup", value=[]),
        "cleanup needs a verified or unverified result",
    ),
    (
        "cleanup-reason",
        lambda f: _set(f, "status", "cleanup", value={"result": "unverified"}),
        "unverified cleanup must state a reason",
    ),
]


@pytest.mark.parametrize(
    ("name", "mutate", "match"),
    TRACE_REJECTIONS,
    ids=[item[0] for item in TRACE_REJECTIONS],
)
def test_trace_and_status_rejections(name, mutate, match):
    """latency/AC-10.missing: trace and status facts are validated before selection."""
    facts = one_attempt_case()
    mutate(facts)
    with pytest.raises(RepoError, match=match):
        model.validate_case(facts)


def test_attempt_trace_and_events_are_validated():
    """latency/AC-05.ambiguous: typed events and per-attempt coverage are strict."""
    facts = one_attempt_case()
    facts["attempts"][0]["trace"] = []
    with pytest.raises(RepoError, match="attempt trace is not an object"):
        model.validate_case(facts)
    for coverage, match in (
        ([], "attempt trace coverage must be an object"),
        ({"other": {}}, "unknown coverage path"),
        ({"preview": []}, "coverage path entry must be an object"),
        (
            {"preview": {"required": "x", "observed": [], "complete": True}},
            "coverage required must be a list",
        ),
        (
            {"preview": {"required": [], "observed": [], "complete": "yes"}},
            "coverage complete must be a boolean",
        ),
        (
            {
                "preview": {
                    "required": [],
                    "observed": [],
                    "complete": True,
                    "inapplicable": {"x": ""},
                }
            },
            "inapplicable stages require",
        ),
    ):
        facts = one_attempt_case()
        facts["attempts"][0]["trace"] = {"coverage": coverage}
        with pytest.raises(RepoError, match=match):
            model.validate_case(facts)
    facts = one_attempt_case()
    facts["attempts"][0]["trace"] = {"loss": []}
    with pytest.raises(RepoError, match="attempt trace loss must be an object"):
        model.validate_case(facts)
    for events, match in (
        ({}, "typed events must be a list"),
        ([[]], "typed event is not an object"),
        ([{"producer": "host"}], "typed event is missing event"),
        ([{"producer": "", "event": "host_enqueue", "ts": 1, "data": {}}], "producer must be text"),
        ([{"producer": "host", "event": "other", "ts": 1, "data": {}}], "unknown typed event"),
        (
            [{"producer": "core", "event": "host_enqueue", "ts": 1, "data": {}}],
            "does not own its boundary",
        ),
        (
            [{"producer": "host", "event": "host_enqueue", "ts": -1, "data": {}}],
            "ts must be a non-negative integer",
        ),
        (
            [{"producer": "host", "event": "host_enqueue", "ts": 1, "data": []}],
            "data must be an object",
        ),
        (
            [{"producer": "host", "event": "host_enqueue", "ts": 1, "data": {"text": "x"}}],
            "forbidden field",
        ),
        (
            [{"v": 2, "producer": "host", "event": "host_enqueue", "ts": 1, "data": {}}],
            "event schema",
        ),
    ):
        facts = one_attempt_case()
        facts["events"] = events
        with pytest.raises(RepoError, match=match):
            model.validate_case(facts)


# ---------------------------------------------------------------------------
# Public helpers, recovery summaries and remaining derivation branches
# ---------------------------------------------------------------------------


def test_workload_and_network_helpers_are_stable():
    """latency/AC-09.defaults: workload and network helper surfaces stay stable."""
    assert model.seed_parity("2") == 0
    assert model.seed_parity("beta") == sum(ord(char) for char in "beta") % 2
    with pytest.raises(ValueError, match="unknown benchmark network link"):
        model.network_settings("normal", "other")
    assert model.shaped_links(model.network_settings("normal", "both")) == ("host", "client")
    assert model.shaped_links(model.network_settings("normal", "client")) == ("client",)
    assert model.workload_controls("watch", 10) == {}
    assert "outage" in model.workload_controls("recovery", 10)
    assert "pairing" in model.workload_controls("mixed-load", 10, paired=True)
    assert "pairing" not in model.workload_controls("mixed-load", 10)
    assert len(model.history_texts({"messages": 2, "bytes_per_message": 8})) == 2
    configuration = make_config(attempts=1)
    built = model.scenario(configuration)
    assert built["version"] == 8
    assert model.source_sizes(built) == [
        len(part.encode()) for part in built["workload"]["updates"]
    ]
    probe = {
        "reference_ms": [1.0] * 5,
        "host_broker_ms": [281.0] * 5,
        "host_ms": [281.0] * 5,
        "client_ms": [1.0] * 5,
    }
    assert model.observed_extra_rtt(probe) == pytest.approx(280.0)
    assert model.network_probe_matches(probe, 280) is True
    assert (
        model.network_probe_matches({"reference_ms": [1.0], "host_broker_ms": [1.0]}, 500) is False
    )
    assert model.link_probe_matches(probe, model.network_settings("high-rtt", "host")) is True
    assert model.link_probe_matches(probe, model.network_settings("high-rtt", "client")) is False
    assert model.largest_burst([{"at_ms": 0, "bytes": 4}, {"at_ms": 60, "bytes": 6}], 50) == 6
    values = model.streaming_values(
        {
            "source_times_ms": [0, 10],
            "outputs": [
                out(5, 0, bytes=10, prefix=10),
                out(15, 1, kind="terminal", bytes=10, prefix=20),
            ],
        },
        20,
        sizes=[10, 10],
        burst_window_ms=50,
        display_delay=True,
    )
    assert values["max_display_delay_ms"] == 5
    assert "largest_burst_share" in values
    summary = model.streaming_summary(
        [
            {
                "phase": "measured",
                "source_times_ms": [0, 10],
                "outputs": [
                    out(5, 0, bytes=10, prefix=10),
                    out(15, 1, kind="terminal", bytes=10, prefix=20),
                ],
            }
        ],
        20,
        [10, 10],
        50,
        display_delay=True,
    )
    assert summary["zero_preview"] == 0
    diagnostics = model.diagnostic_summary(
        [
            {
                "phase": "measured",
                "source_times_ms": [0, 10],
                "outputs": [
                    out(5, 0, bytes=10, prefix=10),
                    out(15, 1, kind="terminal", bytes=10, prefix=20),
                ],
            }
        ],
        [10, 10],
    )
    assert diagnostics["segment_display_delay_ms"]["count"] == 2
    outputs = [
        {
            "at_ms": 5,
            "bytes": 10,
            "prefix_bytes": 10,
            "kind": "preview",
            "correct": True,
        }
    ]
    assert model.longest_pending_gap(outputs, [0], [10]) == 5
    assert model.max_display_delay(outputs, [0], [10]) == 5
    assert list(model.segment_display_delays(outputs, [0], [10])) == [5]


def test_recovery_summary_counts_only_assessable_recoveries():
    """latency/AC-09.concurrent: recovery catch-up keeps its own denominator."""
    slots = [make_slot(index) for index in range(6)]
    attempts = []
    milestones = [
        {
            "link_restored": {"at_ms": 20, "state": "observed"},
            "caught_up": {"at_ms": 50, "state": "observed", "via": "terminal"},
        },
        {
            "link_restored": {"at_ms": 20, "state": "observed"},
            "caught_up": {"at_ms": 40, "state": "observed", "via": "preview"},
        },
        {
            "link_restored": {"at_ms": 20, "state": "observed"},
            "caught_up": {"at_ms": 60, "state": "observed"},
        },
        {
            "link_restored": {"at_ms": 20, "state": "observed"},
            "timeout": {"at_ms": 100, "state": "observed"},
        },
        {
            "link_restored": {"at_ms": 20, "state": "observed"},
            "first_update": {"at_ms": 10, "state": "observed"},
        },
        {
            "link_restored": {"at_ms": 20, "state": "observed"},
        },
    ]
    for index, marks in enumerate(milestones):
        attempts.append(
            make_attempt(
                f"a-{index}",
                index,
                recovery=make_recovery(10, 20, marks),
            )
        )
    configuration = make_config(attempts=6, workflow="recovery")
    facts = build_facts(attempts, slots, workflow="recovery", configuration=configuration)
    report = derived(facts)
    workflow = report["workflow_results"]
    assert workflow["assessable"] == 4
    assert workflow["terminal_first"] == 1
    assert workflow["other_first"] == 1
    assert workflow["unknown_path"] == 1
    assert workflow["verified_timeouts"] == 1
    assert workflow["already_current"] == 1
    assert workflow["unassessable"] == 1
    assert workflow["terminal_first_share"] == pytest.approx(0.25)


def test_mixed_load_exposure_reasons_cover_insufficient_and_absent_rows():
    """latency/AC-09.overlap-union: short exposure and absent rows stay explicit."""

    def short_factory(item):
        return {
            "history": [
                read_observation(f"{item['id']}-h{index}", finished=2) for index in range(4)
            ],
            "attachment": [
                read_observation(
                    f"{item['id']}-a0",
                    op="attachment",
                    finished=2,
                    expected=128,
                    route="reference",
                )
            ],
        }

    report = derived(mixed_case(pairs=1, read_factory=short_factory))
    assert report["workflow_results"]["exposure"]["reason"] == "insufficient_exposure"
    slots = mixed_slots(1)
    configuration = make_config(attempts=2, workflow="mixed-load")
    facts = build_facts([], slots, workflow="mixed-load", configuration=configuration)
    report = derived(facts)
    assert report["workflow_results"]["exposure"]["reason"] == "zero_overlap"
    assert report["workflow_results"]["bulk_read_gate"]["reasons"] == ["no_bulk_reads"]


def test_mixed_load_incomplete_pair_outcome_is_not_compared():
    """latency/AC-09.interrupted-slot: a failed bulk arm leaves its pair unpaired."""
    facts = mixed_case(pairs=1)
    bulk = next(item for item in facts["attempts"] if item["arm"] == "bulk")
    bulk["status"] = "interrupted"
    report = derived(facts)
    assert report["workflow_results"]["pairs"][0]["reason"] == "incomplete_outcome"
    assert report["workflow_results"]["pairs"][0]["values"] is None


def test_classification_covers_product_fixture_incorrect_and_not_started():
    """latency/AC-15.never-visible: each terminal branch keeps a distinct outcome."""
    product = one_attempt_case(failure={"attribution": "product", "reason": "write_failed"})
    row = derived(product)["attempts"][0]
    assert (row["outcome"], row["failure_class"]) == ("product_error", "product")

    fixture = one_attempt_case(
        status="setup_failed",
        failure={"attribution": "fixture", "reason": "watch_not_ready"},
    )
    row = derived(fixture)["attempts"][0]
    assert (row["outcome"], row["failure_class"]) == ("setup_failed", "fixture")

    incorrect = one_attempt_case(
        outputs=[out(10, 0, kind="terminal", bytes=20, prefix=20, correct=False)],
        ended=120,
    )
    row = derived(incorrect)["attempts"][0]
    assert row["outcome"] == "incorrect_content"
    assert row["terminal_integrity"] == "missing"
    assert row["terminal_integrity_reason"] == "incorrect_terminal"

    planned = one_attempt_case(status="planned", started=None, ended=None, cutoff=None)
    row = derived(planned)["attempts"][0]
    assert row["outcome"] == "not_started"
    assert row["snapshot_state"] == "inapplicable"


def test_pending_evidence_uses_absolute_cutoff_and_sequence_range():
    """latency/AC-15.640-220-cutoff: outstanding text names its last owner and range."""
    events = [
        shaped_event(
            "fixture_emit",
            "fixture",
            1_000_000_000,
            {
                "stream": "s-1",
                "attempt": "a-0",
                "block": "b-1",
                "seq_start": 0,
                "seq_end": 4,
                "bytes": 40,
            },
        ),
        shaped_event(
            "fixture_emit",
            "fixture",
            1_100_000_000,
            {
                "stream": "s-1",
                "attempt": "a-0",
                "block": "b-1",
                "seq_start": 5,
                "seq_end": 9,
                "bytes": 60,
            },
            seq=2,
        ),
    ]
    facts = one_attempt_case(
        events=events,
        started_ns=900_000_000,
        ended_ns=1_200_000_000,
        cutoff_ns=1_050_000_000,
    )
    row = derived(facts)["attempts"][0]
    assert row["last_evidenced_stage"] == "fixture_emit"
    assert row["last_evidenced_owner"] == "fixture"
    assert row["pending_range"]["seq_start"] == 0
    assert row["pending_range"]["seq_end"] == 4


def test_attribution_reports_reversed_boundaries_and_no_events():
    """latency/AC-05.ambiguous: reversed joins are unavailable, not negative."""
    events = [
        shaped_event(
            "host_enqueue",
            "host",
            2_000_000_000,
            {"stream": "s", "block": "b", "seq_start": 0, "seq_end": 1, "bytes": 10},
        ),
        shaped_event(
            "host_dequeue",
            "host",
            1_000_000_000,
            {
                "stream": "s",
                "block": "b",
                "seq_start": 0,
                "seq_end": 1,
                "bytes": 10,
                "wait_ns": 1,
            },
            seq=2,
        ),
    ]
    report = derived(one_attempt_case(events=events))
    assert report["attribution"]["intervals"] == []
    assert report["attribution"]["unresolved"][0]["reason"] == "reversed_boundary"
    empty = derived(one_attempt_case(events=[]))
    assert empty["attribution"]["reason"] == "no_events"
    assert empty["attribution"]["critical_path_ms"] == 0.0


def test_suite_validation_rejects_malformed_entries():
    """latency/AC-08.interrupted: suite schema 2 rejects malformed case indexes."""
    suite = model.suite_document([model.case_document(one_attempt_case())])
    for mutate, match in (
        (lambda item: item.update(schema_version=1), "unsupported benchmark suite schema"),
        (lambda item: item.update(kind="other"), "suite kind"),
        (lambda item: item.update(id=""), "suite id must be text"),
        (lambda item: item.update(cases={}), "suite cases must be a list"),
        (lambda item: item.update(cases=[[]]), "case entry is not an object"),
        (
            lambda item: item["cases"][0].update(index=-1),
            "case index must be a non-negative integer",
        ),
        (lambda item: item["cases"][0].update(index=3), "indexes must be contiguous"),
        (lambda item: item["cases"][0].update(status="other"), "unknown status"),
        (
            lambda item: item["cases"][0].update(case_schema=1),
            "unsupported benchmark suite case schema",
        ),
        (lambda item: item["status"].update(state="other"), "suite status needs a known state"),
        (lambda item: item["status"].update(reasons=[1]), "reasons must be text"),
        (lambda item: item["status"].update(pending=["x"]), "pending must list case indexes"),
        (lambda item: item["status"].update(pending=[9]), "pending index is outside"),
        (
            lambda item: item["status"].update(state="incomplete", reasons=[], pending=[]),
            "must state its reasons",
        ),
    ):
        broken = copy.deepcopy(suite)
        mutate(broken)
        with pytest.raises(RepoError, match=match):
            model.validate_suite(broken)


def test_read_and_report_error_paths(tmp_path):
    """latency/AC-08.current: readers reject unreadable and unknown documents."""
    with pytest.raises(RepoError, match="unreadable file"):
        model.read(tmp_path / "missing.json")
    path = tmp_path / "array.json"
    path.write_text("[]")
    with pytest.raises(RepoError, match="document is not an object"):
        model.read(path)
    path = tmp_path / "kind.json"
    path.write_text(json.dumps({"schema_version": 2, "kind": "other"}))
    with pytest.raises(RepoError, match="unsupported benchmark document kind"):
        model.read(path)
    with pytest.raises(RepoError, match="missing derived sections"):
        model.validate_report({"schema_version": 2})
    with pytest.raises(RepoError, match="missing derived sections"):
        model.validate_report([])


def test_analysis_helpers_and_run_comparison_branches():
    """latency/AC-12.mismatch: legacy comparison helpers stay deterministic."""
    assert analysis.mean([]) is None
    assert analysis.mean([1.0, 3.0]) == 2.0
    assert analysis.t_critical(0) is None
    assert analysis.t_critical(31) == 1.96
    assert analysis.paired([1.0]) == (1.0, None, None, None)
    assert analysis.paired_upper_bound([1.0]) == (1.0, None, None)
    assert analysis.classify(None, None, None, 1.0) == ("inconclusive", "insufficient_pairs")
    assert analysis.classify(0.0, 5.0, 10.0, 2.0) == (
        "regression",
        "interval_beyond_upper_tolerance",
    )
    assert analysis.classify(0.0, -10.0, -5.0, 2.0) == (
        "improvement",
        "interval_beyond_lower_tolerance",
    )
    assert analysis.classify(0.0, 1.0, 2.0, 2.0) == (
        "within_tolerance",
        "interval_within_tolerance",
    )
    assert analysis.classify(0.0, -30.0, 5.0, 25.0) == (
        "inconclusive",
        "interval_crosses_tolerance",
    )
    assert analysis.aa_noise([1.0, 4.0, 6.0]) == [3.0, 2.0]
    attempts = [{"outcome": "completed", "first_text_ms": 10.0, "final_ms": 20.0}]
    assert analysis.samples(attempts, "first_text_ms") == [10.0]
    assert analysis.samples(attempts, "update_gaps_ms") == []
    baseline = [
        [{"outcome": "completed", "first_text_ms": 100.0, "final_ms": 100.0}] for _ in range(5)
    ]
    candidate = [
        [{"outcome": "completed", "first_text_ms": 50.0, "final_ms": 50.0}] for _ in range(5)
    ]
    compared = analysis.compare(baseline, candidate)
    assert compared["metrics"]["first_text_ms"]["verdict"] == "improvement"
    empty = analysis.compare([], [])
    assert empty["verdict"] == "inconclusive"
    assert "insufficient_pairs" in empty["reasons"]
    healthy = analysis.run_checks(
        {
            "outcomes": {"measured": 25, "success_share": 0.99},
            "latency": {
                "samples": {
                    "first_text_ms": {"quantiles": {"p95": 10.0}, "count": 25},
                    "final_ms": {"quantiles": {"p95": 20.0}, "count": 25},
                }
            },
        }
    )
    assert healthy["state"] == "complete"


def test_analysis_condition_and_budget_validation_paths():
    """latency/AC-13.freeze: malformed budgets are rejected before any verdict."""
    with pytest.raises(ValueError, match="unsupported budget declaration"):
        analysis.validate_budget({"schema": 99})
    with pytest.raises(ValueError, match="revision is required"):
        analysis.validate_budget({"schema": 1, "metrics": {"x": {"upper_bound": 1}}})
    with pytest.raises(ValueError, match="metrics are required"):
        analysis.validate_budget({"schema": 1, "revision": "r", "metrics": {}})
    with pytest.raises(ValueError, match="needs a numeric upper bound"):
        analysis.validate_budget({"schema": 1, "revision": "r", "metrics": {"x": {}}})
    with pytest.raises(ValueError, match="needs an integer sample floor"):
        analysis.validate_budget(
            {
                "schema": 1,
                "revision": "r",
                "metrics": {"x": {"upper_bound": 1, "sample_floor": "x"}},
            }
        )
    with pytest.raises(ValueError, match="budget upper_bound must be numeric"):
        analysis.budget_declaration({"x": {"upper_bound": "x"}}, revision="r")
    budget = analysis.budget_declaration({"first_text_ms": 5.0}, revision="r")
    no_evidence = analysis.evaluate_budget(budget, {})
    assert no_evidence["state"] == "inconclusive"
    assert no_evidence["metrics"]["first_text_ms"]["reason"] == "insufficient_pairs"
    one_pair = analysis.evaluate_budget(
        budget, {"first_text_ms": {"pairs": [{"baseline": 1.0, "candidate": 2.0}]}}
    )
    assert one_pair["metrics"]["first_text_ms"]["reason"] == "insufficient_pairs"
    missing_run = analysis.compare_runs([], [])
    assert "missing_run" in missing_run["reasons"]
    same = model.case_document(one_attempt_case())
    no_gain = analysis.compare_runs([same] * 5, [same] * 5)
    assert no_gain["verdict"] == "complete"


def test_analysis_bulk_gate_and_paired_arm_comparison():
    """latency/AC-12.bulk-minus-idle: the legacy mixed-load gate stays covered."""
    complete = {
        "expected_reads": 2,
        "successful_reads": 2,
        "bytes_expected": 10,
        "bytes_verified": 10,
        "errors": {},
    }
    assert analysis.bulk_read_gate(complete)["state"] == "complete"
    assert analysis.bulk_read_gate({**complete, "bytes_verified": 9})["reasons"] == [
        "bulk_bytes_not_verified"
    ]
    assert analysis.bulk_read_gate({"expected_reads": 0})["reasons"] == [
        "no_bulk_reads",
    ]
    pairs = [
        {
            "pair_id": f"p-{index}",
            "values": {
                "idle": {
                    "max_display_delay_ms": 5,
                    "longest_pending_stall_ms": 10 + index,
                    "largest_output_share": 0.1,
                    "final_output_share": 0.1,
                },
                "bulk": {
                    "max_display_delay_ms": 6,
                    "longest_pending_stall_ms": 12 + index,
                    "largest_output_share": 0.1,
                    "final_output_share": 0.1,
                },
            },
            "reason": None,
        }
        for index in range(6)
    ]
    result = analysis.paired_arm_comparison(pairs)
    assert result["gate"]["state"] == "complete"
    assert result["gate"]["metrics"]["longest_pending_stall_ms"]["mean_delta"] == 2
    incomplete = analysis.paired_arm_comparison(pairs[:2])
    assert incomplete["gate"]["state"] == "incomplete"
    missing = analysis.paired_arm_comparison(
        [{"pair_id": "p-0", "values": None, "reason": "incomplete_pair"}]
    )
    assert missing["pairs"][0]["deltas"]["longest_pending_stall_ms"] is None


def test_mixed_load_pair_order_validation_errors_are_explicit():
    """latency/AC-12.counterbalanced-order: malformed pair schedules fail clearly."""
    slots = [
        make_slot(0, arm="idle", pair="p0", order=0),
        make_slot(1, arm="bulk", pair="p0", order=0),
    ]
    configuration = make_config(attempts=2, workflow="mixed-load")
    facts = build_facts([], slots, workflow="mixed-load", configuration=configuration)
    with pytest.raises(RepoError, match="pair slots need order 0 and 1"):
        model.validate_case(facts)
    slots = [
        make_slot(0, arm="bulk", pair="p0", order=0),
        make_slot(1, arm="idle", pair="p0", order=1),
    ]
    facts = build_facts([], slots, workflow="mixed-load", configuration=configuration)
    with pytest.raises(RepoError, match="pair order is not counterbalanced"):
        model.validate_case(facts)
    slots = [
        make_slot(0, arm="idle", pair="p0", order=0),
        make_slot(1, arm="bulk", pair="p0", order=1),
        make_slot(2, arm="idle", pair="p0", order=0),
        make_slot(3, arm="bulk", pair="p0", order=1),
    ]
    configuration = make_config(attempts=4, workflow="mixed-load")
    facts = build_facts([], slots, workflow="mixed-load", configuration=configuration)
    with pytest.raises(RepoError, match="exactly two slots per pair"):
        model.validate_case(facts)


def test_mixed_read_count_validation_is_explicit():
    """latency/AC-09.setup-failure: every scheduled bulk read needs an outcome."""
    facts = mixed_case(pairs=1)
    bulk = next(item for item in facts["attempts"] if item["arm"] == "bulk")
    bulk["reads"].pop(0)
    with pytest.raises(RepoError, match="every scheduled history read"):
        model.validate_case(facts)
    facts = mixed_case(pairs=1)
    bulk = next(item for item in facts["attempts"] if item["arm"] == "bulk")
    bulk["reads"] = [read for read in bulk["reads"] if read["op"] == "history"]
    with pytest.raises(RepoError, match="every scheduled attachment read"):
        model.validate_case(facts)
    facts = mixed_case(pairs=1)
    bulk = next(item for item in facts["attempts"] if item["arm"] == "bulk")
    bulk.pop("reads")
    with pytest.raises(RepoError, match="requires read observations"):
        model.validate_case(facts)


def test_mixed_scenario_read_declarations_are_read_from_the_workload():
    """latency/AC-09.concurrent: scenario() declares reads directly on workload."""
    configuration = make_config(attempts=2, workflow="mixed-load")
    scenario = model.scenario(configuration)
    assert "history" in scenario["workload"]
    assert "attachment" in scenario["workload"]
    assert "controls" not in scenario["workload"]
    slots = mixed_slots(1)
    attempts = []
    for slot in slots:
        reads = []
        if slot["arm"] == "bulk":
            declared = default_reads(f"a-{slot['slot']}")
            reads = [*declared["history"], *declared["attachment"]]
        attempts.append(
            make_attempt(
                f"a-{slot['slot']}",
                slot["slot"],
                arm=slot["arm"],
                pair_id=slot["pair"],
                reads=reads,
            )
        )
    facts = build_facts(attempts, slots, workflow="mixed-load", configuration=configuration)
    facts["scenario"] = scenario
    model.validate_case(facts)
    report = derived(facts)
    assert report["workflow_results"]["exposure"]["scheduled_reads"] == 5
    assert report["workflow_results"]["reads"]["bulk"]["expected_reads"] == 5


def test_latency_ac_12_waterfall_matches_every_edge_and_leaves_zero_residual():
    """latency/AC-12.offline-replay; latency/AC-01.batch-join."""
    base = 1_000_000_000
    step = 10_000_000

    def ev(name, index, producer, data):
        return shaped_event(name, producer, base + step * index, data)

    common = {"stream": "s", "block": "b", "seq_start": 0, "seq_end": 0, "bytes": 10}
    batch = {"batch": "batch-1"}
    events = [
        ev(
            "fixture_emit",
            0,
            "fixture",
            {
                "stream": "s",
                "attempt": "a-0",
                "block": "b",
                "seq_start": 0,
                "seq_end": 0,
                "bytes": 10,
            },
        ),
        ev(
            "dsh_callback",
            1,
            "dsh",
            {**common, **batch, "attempt": "a-0", "native": "assistant-preview"},
        ),
        ev("dsh_handoff", 2, "dsh", {**common, **batch, "attempt": "a-0"}),
        ev("host_enqueue", 3, "host", {**common, **batch, "queue_depth": 1, "queue_bytes": 10}),
        ev("host_dequeue", 4, "host", {**common, **batch, "wait_ns": step}),
        ev(
            "host_publish",
            5,
            "host",
            {
                **common,
                **batch,
                "request": "",
                "attempt": 1,
                "kind": "event",
                "outcome": "accepted",
            },
        ),
        ev(
            "core_receipt",
            6,
            "core",
            {**common, **batch, "frame": "f-1", "kind": "event", "decode": "ok"},
        ),
        ev(
            "core_enqueue",
            7,
            "core",
            {"frame": "f-1", "queue_depth": 1, "queue_bytes": 10, "accepted": True},
        ),
        ev("core_dequeue", 8, "core", {"frame": "f-1", "wait_ns": step, "disposition": "applied"}),
        ev(
            "core_delivery",
            9,
            "core",
            {
                "frame": "f-1",
                "delivery": 1,
                "kind": "preview",
                "bytes": 10,
                "stream": "s",
                "block": "b",
                "seq_start": 0,
                "seq_end": 0,
                "batch": "batch-1",
            },
        ),
        ev(
            "cli_write",
            10,
            "cli",
            {
                "delivery": 1,
                "ordinal": 1,
                "bytes": 10,
                "outcome": "success",
                "flush": True,
                "stream": "s",
                "block": "b",
                "seq_start": 0,
                "seq_end": 0,
            },
        ),
        ev(
            "fixture_observe",
            11,
            "fixture",
            {
                "attempt": "a-0",
                "ordinal": 1,
                "record": 1,
                "kind": "preview",
                "bytes": 10,
                "prefix_bytes": 10,
                "correct": True,
                "block": "b",
            },
        ),
    ]
    report = derived(
        one_attempt_case(events=events, ended_ns=base + step * 11, cutoff_ns=base + step * 11)
    )
    waterfall = report["attempts"][0]["waterfall"]
    assert waterfall["complete"] is True
    assert waterfall["residual_ms"] == 0
    assert waterfall["total_ms"] == 110.0
    assert len(waterfall["edges"]) == 8
    assert report["coverage"]["paths"]["preview"]["edges"]["complete"] is True


def test_dropped_publish_and_suppressed_preview_are_terminal_not_missing():
    """latency/AC-12.terminal-disposition."""
    dropped = shaped_event(
        "host_publish",
        "host",
        1_000_000_000,
        {
            "request": "",
            "attempt": 1,
            "kind": "event",
            "bytes": 10,
            "outcome": "dropped",
            "stream": "s",
            "seq_start": 2,
            "seq_end": 2,
            "batch": "batch-1",
        },
    )
    _, missing = model._pair_edge(
        [dropped],
        "host_publish",
        "core_receipt",
        ("stream", "seq_start", "seq_end"),
    )
    assert missing[0]["reason"] == "dropped"

    suppressed = shaped_event(
        "dsh_callback",
        "dsh",
        1_000_000_000,
        {
            "stream": "s",
            "attempt": "a-0",
            "block": "b",
            "seq_start": 2,
            "seq_end": 2,
            "bytes": 10,
            "native": "assistant-preview",
            "disposition": "suppressed",
        },
    )
    _, missing = model._pair_edge(
        [suppressed],
        "dsh_callback",
        "dsh_handoff",
        ("stream", "block", "seq_start", "seq_end", "batch"),
    )
    assert missing[0]["reason"] == "inapplicable"
