"""Fixture-helper tests that need no container, Docker or Linux host.

These cover the pure logic the container benchmark fixture relies on: the
seeded slot plan and reconciliation, error-free measurement cuts, history
continuation verification, clock admission, typed-event allowlisting and cap
handling, and interval exposure math. One seam test builds a synthetic raw
case document and validates it against the current benchmark model.
"""

from __future__ import annotations

import sys
import types
from pathlib import Path

import pytest

ROOT = Path(__file__).resolve().parents[3]
sys.path.insert(0, str(ROOT / "tests" / "integration"))

from benchmark import clock, events, facts, history, intervals, network, sampler, slots, source  # noqa: E402


def mixed_configuration(**overrides):
    configuration = {
        "attempts": 4,
        "warmup_attempts": 2,
        "concurrency": 1,
        "profile": "warm-client",
        "delay_ms": 0,
        "cadence_ms": 100,
        "network": {
            "profile": "normal",
            "link": "host",
            "extra_rtt_ms": 0,
            "jitter_ms_per_direction": 0,
            "loss_percent_per_direction": 0.0,
        },
        "workflow": "mixed-load",
        "deadline_ms": 10000,
        "observation_window_ms": 1000,
        "seed": 0,
        "observation_mode": "trace",
    }
    configuration.update(overrides)
    return configuration


# ---------------------------------------------------------------------------
# Slot plan
# ---------------------------------------------------------------------------


def test_slot_plan_counterbalances_pairs_and_reverses_each_pair():
    schedule = slots.plan(mixed_configuration(seed=7))
    assert schedule["rule"] == "alternating"
    assert schedule["seed"] == "7"
    measured = [slot for slot in schedule["slots"] if slot["phase"] == "measured"]
    warmup = [slot for slot in schedule["slots"] if slot["phase"] == "warmup"]
    assert len(measured) == 4 and len(warmup) == 2
    pairs = {}
    for slot in measured:
        pairs.setdefault(slot["pair"], []).append(slot)
    orders = [
        tuple(entry["arm"] for entry in sorted(pair, key=lambda item: item["order"]))
        for pair in pairs.values()
    ]
    assert orders[0] != orders[1]
    assert set(orders[0]) == {"idle", "bulk"}
    for slot in measured:
        assert slot["order"] in (0, 1)


def test_slot_plan_rejects_odd_mixed_phase():
    with pytest.raises(ValueError):
        slots.plan(mixed_configuration(attempts=3))
    with pytest.raises(ValueError):
        slots.plan(mixed_configuration(warmup_attempts=1))


def test_slot_plan_uses_watch_arm_for_single_stream_workflows():
    schedule = slots.plan(mixed_configuration(workflow="watch", attempts=2, warmup_attempts=0))
    assert {slot["arm"] for slot in schedule["slots"]} == {"watch"}


def test_reconcile_accounts_for_every_planned_slot():
    schedule = slots.plan(mixed_configuration(warmup_attempts=0))
    attempts = [
        {"slot": 0, "status": "finished"},
        {"slot": 1, "status": "setup_failed"},
        {"slot": 2, "status": "finished"},
        # slot 3 intentionally absent
    ]
    result = slots.reconcile(schedule, attempts)
    assert result["missing"] == [3]
    assert result["states"][3] == "not_started"
    assert result["states"][1] == "setup_failed"
    assert not result["complete"]
    duplicate = attempts + [{"slot": 2, "status": "interrupted"}]
    result = slots.reconcile(schedule, duplicate)
    assert result["duplicates"] == [2]
    result = slots.reconcile(schedule, attempts + [{"slot": 99, "status": "finished"}])
    assert result["unknown"] == [99]


# ---------------------------------------------------------------------------
# Intervals
# ---------------------------------------------------------------------------


def test_overlap_is_half_open_and_touching_is_zero():
    assert intervals.overlap_ms((0.0, 10.0), (10.0, 20.0)) == 0.0
    assert intervals.overlap_ms((5.0, 15.0), (0.0, 10.0)) == 5.0
    assert intervals.overlap_ms((20.0, 30.0), (0.0, 10.0)) == 0.0


def test_union_does_not_add_overlapping_reader_time():
    summary = intervals.exposure(
        [
            {"started_at_ms": 0.0, "finished_at_ms": 10.0},
            {"started_at_ms": 5.0, "finished_at_ms": 15.0},
            {"started_at_ms": None, "finished_at_ms": None},
        ],
        (0.0, 20.0),
    )
    assert summary["union_ms"] == 15.0
    # Summed reader time keeps the overlapping 5 ms twice; the union does not.
    assert summary["summed_overlap_ms"] == 20.0
    assert summary["overlapping_reads"] == 2
    assert summary["unknown_reads"] == 1
    assert not intervals.sufficient(summary, 16.0)


# ---------------------------------------------------------------------------
# Source cuts
# ---------------------------------------------------------------------------


def test_source_cut_is_named_per_workflow():
    assert source.origin("watch", first_emission_ns=500_000_000)[0] == 500_000_000
    assert source.origin("cold-connect", client_invocation_ns=300_000_000) == (
        300_000_000,
        source.CLIENT_INVOCATION,
    )
    assert source.origin("recovery", restored_ns=700_000_000) == (
        700_000_000,
        source.LINK_RESTORATION,
    )
    with pytest.raises(ValueError):
        source.origin("watch")
    with pytest.raises(ValueError):
        source.origin("recovery")


def test_source_series_is_relative_and_recovery_keeps_negative_offsets():
    assert source.source_series(
        [500_000_000, 600_000_000, 700_000_000], [3, 3, 3], 500_000_000
    ) == {
        "times_ms": [0.0, 100.0, 200.0],
        "sizes": [3, 3, 3],
    }
    assert source.source_series([500_000_000], [3], 700_000_000)["times_ms"] == [-200.0]


def test_recovery_catch_up_requires_covering_visible_prefix():
    sizes = [10, 10, 10]
    times = [0.0, 100.0, 200.0]
    outputs = [
        {"at_ms": 120.0, "prefix_bytes": 10, "correct": True, "kind": "preview"},
        {"at_ms": 260.0, "prefix_bytes": 20, "correct": True, "kind": "preview"},
    ]
    # Restoration at 250 ms: everything emitted by then is 30 bytes, so the
    # 20-byte prefix is not a catch-up yet.
    assert source.catch_up(outputs, times, sizes, 250.0) is None
    outputs.append({"at_ms": 300.0, "prefix_bytes": 30, "correct": True, "kind": "terminal"})
    matched = source.catch_up(outputs, times, sizes, 250.0)
    assert matched["at_ms"] == 300.0
    assert matched["via"] == "terminal"


def test_recovery_facts_keep_blackout_separate_from_restoration_cut():
    attempt = {"_source_times_ms": [0.0, 100.0, 200.0]}
    recovery = source.recovery_facts(
        blackout_start_ns=50_000_000,
        restored_ns=250_000_000,
        origin_ns=250_000_000,
        outputs=[
            {"at_ms": 30.0, "prefix_bytes": 30, "correct": True, "kind": "terminal"},
        ],
        times_ms=attempt["_source_times_ms"],
        sizes=[10, 10, 10],
        deadline_ms=1000,
    )
    assert recovery["blackout_start_ms"] == -200.0
    assert recovery["restored_ms"] == 0.0
    assert recovery["milestones"]["caught_up"]["at_ms"] == 30.0
    assert recovery["milestones"]["blackout_start"]["state"] == "observed"


# ---------------------------------------------------------------------------
# History traversal
# ---------------------------------------------------------------------------


def page(messages, continuation=None, complete=True, physical=10):
    return {
        "messages": messages,
        "continuation": continuation,
        "complete": complete,
        "physical_bytes": physical,
    }


def test_history_walk_reverses_pages_and_counts_verified_bytes_once():
    expected = ["newest", "middle", "oldest"]
    pages = {
        None: page(expected[:1], "token-1", complete=False, physical=11),
        "token-1": page(expected[1:2], "token-2", complete=False, physical=12),
        "token-2": page(expected[2:], None, complete=True, physical=13),
    }
    clock_values = iter([0.0, 1.0, 2.0, 3.0, 4.0, 5.0])

    def fetch(size, continuation, timeout):
        assert timeout > 0
        return pages[continuation]

    # Pages arrive newest-page first; only reversing them reconstructs the
    # oldest-to-newest logical sequence.
    result = history.walk(
        fetch,
        expected=list(reversed(expected)),
        page_size=1,
        deadline_ms=1000,
        clock_ms=lambda: next(clock_values),
        text_of=lambda value: value,
    )
    assert result["outcome"] == "completed"
    assert result["verified"] is True
    assert result["pages"] == 3
    assert result["bytes_verified"] == sum(len(text.encode()) for text in reversed(expected))
    assert result["physical_bytes"] == 36


def test_history_walk_fails_repeated_tokens_and_corrupt_content():
    def repeated(size, continuation, timeout):
        return page(["a"], "same-token", complete=False)

    result = history.walk(
        repeated,
        expected=["a", "b"],
        page_size=1,
        deadline_ms=1000,
        clock_ms=lambda: 0.0,
        text_of=lambda value: value,
    )
    assert result["token_repeats"] == 1
    assert result["verified"] is False
    assert result["outcome"] == "incorrect_content"

    def corrupt(size, continuation, timeout):
        return page(["wrong"], None, complete=True)

    result = history.walk(
        corrupt,
        expected=["expected"],
        page_size=1,
        deadline_ms=1000,
        clock_ms=lambda: 0.0,
        text_of=lambda value: value,
    )
    assert result["corrupt"] is True
    assert result["outcome"] == "incorrect_content"


def test_history_walk_uses_one_deadline_across_continuations():
    current = {"value": 0.0}

    def tick():
        return current["value"]

    tokens = iter(["next-1", "next-2"])

    def slow_fetch(size, continuation, timeout):
        current["value"] += 600.0
        return page(["a"], next(tokens, None), complete=False)

    result = history.walk(
        slow_fetch,
        expected=["a", "b"],
        page_size=1,
        deadline_ms=1000,
        clock_ms=tick,
        text_of=lambda value: value,
    )
    assert result["outcome"] == "timeout"
    assert result["bytes_verified"] == 0


def test_attachment_verification_records_only_match_facts():
    good = history.attachment_facts(
        saved_bytes=8, expected_bytes=8, expected_sha256="a" * 64, actual_sha256="a" * 64
    )
    assert good["verified"] is True and good["bytes_verified"] == 8
    bad = history.attachment_facts(
        saved_bytes=8, expected_bytes=8, expected_sha256="a" * 64, actual_sha256="b" * 64
    )
    assert bad["verified"] is False
    assert bad["outcome"] == "incorrect_content"
    assert "sha256" not in bad  # the digest value is never published


# ---------------------------------------------------------------------------
# Clock admission
# ---------------------------------------------------------------------------


@pytest.fixture
def patched_clock(monkeypatch):
    monkeypatch.setattr(clock, "boot_id", lambda path=clock.BOOT_ID_PATH: "boot-a")
    monkeypatch.setattr(clock, "time_namespace", lambda pid, proc=None: "ns-a", raising=False)
    monkeypatch.setattr(
        clock, "process_lifetime", lambda pid, proc=None: f"lt-{pid}", raising=False
    )


def test_clock_register_brackets_reading_and_rejects_wrong_epoch(patched_clock):
    admission = clock.ClockAdmission(
        fixture_boot_id="boot-a", fixture_namespace="ns-a", clock=lambda: 1000
    )
    assert admission.register("core", pid=7, reading=1500, before_ns=1000, after_ns=2000)[
        "verified"
    ]
    rejected = admission.register("core", pid=8, reading=3000, before_ns=1000, after_ns=2000)
    assert rejected["verified"] is False
    assert rejected["reason"] == "reading_outside_bracket"
    assert admission.result()["producers"][0]["lifetime"] == "lt-7"


def test_clock_rejects_foreign_time_namespace(patched_clock, monkeypatch):
    monkeypatch.setattr(clock, "time_namespace", lambda pid, proc=None: "ns-b", raising=False)
    admission = clock.ClockAdmission(
        fixture_boot_id="boot-a", fixture_namespace="ns-a", clock=lambda: 1000
    )
    entry = admission.register("access", pid=9, reading=1500, before_ns=1000, after_ns=2000)
    assert entry["verified"] is False
    assert entry["reason"] == "foreign_time_namespace"
    assert admission.verified is False


def test_clock_membership_only_is_recorded_as_a_reason(patched_clock):
    admission = clock.ClockAdmission(
        fixture_boot_id="boot-a", fixture_namespace="ns-a", clock=lambda: 1000
    )
    entry = admission.register("host", pid=11)
    assert entry["verified"] is True
    assert entry["reason"] == "membership_only"


# ---------------------------------------------------------------------------
# Typed events: allowlist, merge, cap, coverage
# ---------------------------------------------------------------------------


@pytest.fixture(scope="module")
def manifest():
    return events.load_manifest(ROOT / "contracts" / "latency" / "observation.json")


def record(producer, event, ts, **data):
    return {
        "v": 1,
        "run": "run-1",
        "producer": producer,
        "lifetime": "lt-1",
        "seq": ts,
        "ts": ts,
        "event": event,
        "data": data,
    }


def test_sanitize_drops_forbidden_and_unknown_fields(manifest):
    clean, dropped = events.sanitize(
        record(
            "fixture",
            "fixture_observe",
            1,
            attempt="a-1",
            ordinal=1,
            kind="preview",
            bytes=10,
            prefix_bytes=10,
            correct=True,
            text="secret",
            mystery=1,
        ),
        manifest,
        run_id="run-1",
    )
    assert clean is not None
    assert "text" not in clean["data"] and "mystery" not in clean["data"]
    assert dropped["forbidden_fields"] == 1
    assert dropped["unknown_fields"] == 1


def test_sanitize_rejects_unknown_event_and_foreign_run(manifest):
    clean, dropped = events.sanitize(record("core", "made_up", 1), manifest, run_id="run-1")
    assert clean is None and dropped["unknown_event"] == 1
    clean, dropped = events.sanitize(
        record("core", "core_delivery", 1, frame="f", delivery=1, kind="preview", bytes=1),
        manifest,
        run_id="other-run",
    )
    assert clean is None and dropped["foreign_run"] == 1


def test_merge_is_deterministic_and_caps_with_loss(manifest, tmp_path):
    first = tmp_path / "fixture-a.ndjson"
    second = tmp_path / "core-b.ndjson"
    first.write_text(
        "\n".join(
            events.serialise(
                record(
                    "fixture",
                    "fixture_observe",
                    ts,
                    attempt="a-1",
                    ordinal=ts,
                    kind="preview",
                    bytes=1,
                    prefix_bytes=1,
                    correct=True,
                )
            )
            for ts in (3, 1)
        )
        + "\n{not-json}\n"
    )
    second.write_text(
        events.serialise(
            record("core", "core_delivery", 2, frame="f-1", delivery=1, kind="preview", bytes=1)
        )
        + "\n"
    )
    merged = events.merge(
        {str(first): "fixture", str(second): "core"},
        manifest,
        run_id="run-1",
        cap_bytes=2000,
        reserve_bytes=100,
    )
    assert [event["ts"] for event in merged["events"]] == [1, 2, 3]
    assert merged["losses"]["malformed_lines"] == 1
    assert merged["complete"] is True
    capped = events.merge(
        {str(first): "fixture", str(second): "core"},
        manifest,
        run_id="run-1",
        cap_bytes=200,
        reserve_bytes=100,
    )
    assert capped["truncated"] is True
    assert capped["losses"]["trace_bytes"] > 0
    assert capped["complete"] is False


def test_coverage_lists_observed_mandatory_boundaries(manifest):
    coverage = events.coverage_report(
        [
            record(
                "fixture",
                "fixture_emit",
                1,
                stream="s",
                attempt="a",
                block="b",
                seq_start=0,
                seq_end=0,
                bytes=1,
            ),
            record(
                "fixture",
                "fixture_observe",
                2,
                attempt="a",
                ordinal=1,
                kind="preview",
                bytes=1,
                prefix_bytes=1,
                correct=True,
            ),
        ],
        manifest,
    )
    assert "fixture_emit" in coverage["preview"]["observed"]
    assert coverage["preview"]["complete"] is False
    assert coverage["query"]["complete"] is False


# ---------------------------------------------------------------------------
# Environment sampling parsers
# ---------------------------------------------------------------------------


def test_parse_ss_extracts_rtt_retransmits_and_queue():
    """latency/AC-11.normal: real socket readings carry role and generation."""
    text = (
        "ESTAB 0 0 127.0.0.2:1883 127.0.0.1:45678\n"
        "\t cubic wscale:7,7 rtt:30.5/1.2 retrans:0/2 bytes_acked:123\n"
        "ESTAB 0 128 127.0.0.3:45679 127.0.0.1:1883\n"
        "\t cubic rtt:1.5/0.2 bytes_acked:9\n"
    )
    sockets = sampler.mqtt_sockets(sampler.parse_ss(text))
    assert len(sockets) == 2
    assert sockets[0]["rtt_us"] == 30500
    assert sockets[0]["retransmits"] == 2
    assert sockets[1]["send_queue_bytes"] == 128
    assert (
        sampler.socket_role(sockets[0], host_address="127.0.0.2", client_address="127.0.0.3")
        == "host"
    )
    assert (
        sampler.socket_role(sockets[1], host_address="127.0.0.2", client_address="127.0.0.3")
        == "client"
    )


def test_sampler_windows_record_gaps_and_replacement():
    """latency/AC-11.socket-replaced and latency/AC-11.idle.

    Replacement and idle are explicit states, not a measured zero.
    """
    samples = [
        {"role": "host", "generation": 1, "at_ms": 0.0, "state": "sampled", "rtt_us": 1000},
        {"role": "host", "generation": 1, "at_ms": 1000.0, "state": "sampled", "rtt_us": 1200},
        {"role": "host", "generation": 2, "at_ms": 4000.0, "state": "replaced", "rtt_us": 900},
    ]
    result = sampler.windows(samples, 1000)
    assert len(result) == 2
    assert result[0]["samples"] == 2
    assert result[0]["rtt_max_us"] == 1200
    assert result[1]["generation"] == 2
    assert result[1]["state"] == "replaced"
    assert sampler.sample_gap_ms(0.0, 4000.0, 1000) == 4000.0
    assert sampler.sample_gap_ms(0.0, 1200.0, 1000) is None
    idle = sampler.windows(
        [{"role": "client", "generation": 1, "at_ms": 0.0, "state": "idle"}], 1000
    )
    assert idle[0]["state"] == "idle"
    assert idle[0]["rtt_max_us"] is None


def test_shaped_network_declares_per_direction_impairment():
    """latency/AC-11.shaped: the shaped profile is attributable to the measured interval."""
    settings = {
        "profile": "high-rtt",
        "link": "host",
        "extra_rtt_ms": 280,
        "jitter_ms_per_direction": 40,
        "loss_percent_per_direction": 1.0,
    }
    options = network.netem_options(settings)
    assert options[:2] == ["delay", "140000us"]
    assert "40ms" in options and "loss" in options and "1.0%" in options
    assert network.shaped_links(settings) == ("host",)


def test_profiler_and_missing_metrics_are_explicitly_unsupported(monkeypatch):
    """latency/AC-11.unsupported: an unavailable optional capability is not a zero."""
    # The CI image may carry a profiler binary; force the unsupported branch so
    # the assertion is about the fallback, not about the host's toolchain.
    monkeypatch.setattr(sampler.shutil, "which", lambda name: None)
    capability = sampler.profiler_capability()
    assert capability["available"] is False
    assert capability["tool"] is None
    assert capability["reason"] == "unsupported"


def test_parse_metrics_and_cgroup_limits():
    metrics = sampler.parse_metrics(
        "# HELP go_goroutines\n"
        "go_goroutines 12\n"
        'go_gc_duration_seconds{quantile="0"} 0.1\n'
        "process_cpu_seconds_total 3.5\n"
        "unrelated_metric 9\n"
    )
    assert metrics == {"go_goroutines": 12.0, "process_cpu_seconds_total": 3.5}
    assert sampler.parse_cgroup_limits("200000 100000", "536870912") == {
        "cpus": 2.0,
        "memory_bytes": 536870912,
    }
    assert sampler.parse_cgroup_limits("max 100000", "max")["cpus"] is None


# ---------------------------------------------------------------------------
# Fixture document seam with the benchmark model
# ---------------------------------------------------------------------------


def test_synthetic_fixture_document_matches_the_model_contract():
    model = pytest.importorskip("repo.benchmark.model")
    configuration = mixed_configuration(attempts=2, warmup_attempts=0)
    scenario = facts.scenario_controls(model.scenario(configuration))
    schedule = slots.plan(configuration)

    class Capture:
        run_id = "run-synthetic"
        mode = configuration["observation_mode"]

    run = facts.run_block(
        capture=Capture(),
        configuration=configuration,
        started_at="2026-01-01T00:00:00Z",
        fingerprint={
            "build": {"revision": "synthetic", "fingerprint": "sha256:synthetic"},
            "fixture": {"name": "FakeDSH", "fingerprint": "sha256:fixture"},
            "schemas": {"case": 2, "event": 1, "workload": 8},
            "platform": {"os": "linux", "arch": "x86_64", "kernel": "test"},
            "limits": {"cpus": 1, "memory_bytes": 1024},
            "clock": {"source": "CLOCK_MONOTONIC", "unit": "ns", "verified": True},
            "sampling": {"socket_hz": 1, "runtime_hz": 1},
        },
    )
    environment = facts.environment_block(
        artifacts={"cli": "sha256:cli"},
        limits={"cpus": 1, "memory_bytes": 1024},
        versions={"python": "3.13"},
        fixture={"name": "FakeDSH", "fingerprint": "sha256:fixture"},
        clock={
            "source": "CLOCK_MONOTONIC",
            "unit": "ns",
            "boot_id_verified": True,
            "namespace_verified": True,
            "producers": [{"producer": "core", "lifetime": "lt-1", "verified": True}],
        },
        capture_origin_ns=1000,
    )
    attempts = []
    for slot in schedule["slots"]:
        reads = None
        if slot["arm"] == "bulk":
            reads = [
                {
                    "id": f"history-{index}",
                    "op": "history",
                    "scheduled_at_ms": 0.0,
                    "started_at_ms": 1.0,
                    "finished_at_ms": 2.0,
                    "deadline_ms": 10000,
                    "pages": 1,
                    "messages": 1,
                    "bytes_expected": 4,
                    "bytes_verified": 4,
                    "physical_bytes": 4,
                    "route": "inline",
                    "outcome": "completed",
                    "verified": True,
                    "token_repeats": 0,
                    "corrupt": False,
                }
                for index in range(4)
            ] + [
                {
                    "id": "attachment-0",
                    "op": "attachment",
                    "scheduled_at_ms": 0.0,
                    "started_at_ms": 1.0,
                    "finished_at_ms": 2.0,
                    "deadline_ms": 10000,
                    "pages": 0,
                    "messages": 0,
                    "bytes_expected": 8,
                    "bytes_verified": 8,
                    "physical_bytes": 8,
                    "route": "reference",
                    "outcome": "completed",
                    "verified": True,
                    "token_repeats": 0,
                    "corrupt": False,
                }
            ]
        attempts.append(
            facts.attempt(
                slot=slot["slot"],
                phase=slot["phase"],
                arm=slot["arm"],
                pair_id=slot["pair"],
                status="finished",
                source={"times_ms": [0.0, 100.0], "sizes": [4, 4]},
                outputs=[
                    facts.output_fact(
                        at_ms=0.0,
                        ordinal=1,
                        kind="preview",
                        bytes_=4,
                        prefix_bytes=4,
                        correct=True,
                    )
                ],
                observation=facts.observation_block(
                    started_ns=1000,
                    ended_ns=2000,
                    cutoff_ns=2000,
                    origin_ns=1000,
                    origin=source.FIRST_EMISSION,
                ),
                reads=reads,
            )
        )
    document = facts.document(
        run=run,
        scenario=scenario,
        environment=environment,
        schedule=schedule,
        attempts=attempts,
        trace=facts.trace_block(mode="off"),
        status=facts.status_block(state="complete"),
    )
    assert "derived" not in document
    model.validate_case(document)
    derived = model.derive(document)
    assert derived["populations"]["planned"]["measured"] == 2


def test_trace_finalization_brackets_cli_and_computes_attempt_coverage():
    """The two run-level trace helpers stay testable without a container."""
    main = pytest.importorskip("benchmark.main")
    manifest = events.load_manifest(ROOT / "contracts" / "latency" / "observation.json")
    run = types.SimpleNamespace(
        manifest=manifest,
        attempts=[{"id": "attempt-0001"}],
        pending_clocks=[
            {
                "entry": {"producer": "cli", "lifetime": "lt-1", "verified": True},
                "outlet": "/reports/outlet/cli-1.ndjson",
                "before_ns": 100,
                "after_ns": 300,
            }
        ],
    )
    typed = [
        {
            "event": "fixture_emit",
            "ts": 200,
            "data": {"attempt": "attempt-0001"},
            "_outlet": "/reports/outlet/cli-1.ndjson",
        },
        {
            "event": "fixture_observe",
            "ts": 250,
            "data": {"attempt": "attempt-0001"},
            "_outlet": "/reports/outlet/cli-1.ndjson",
        },
    ]
    main.finalize_pending_clocks(run, typed)
    assert run.pending_clocks[0]["entry"]["verified"] is True
    coverage = main.per_attempt_coverage(run, typed)
    preview = coverage["attempt-0001"]["preview"]
    assert "fixture_emit" in preview["observed"]
    assert preview["complete"] is False


# ---------------------------------------------------------------------------
# AC-04 acceptance bindings for the fixture history reader
# ---------------------------------------------------------------------------


def test_history_reader_follows_all_pages_under_one_deadline():
    """latency/AC-04.paginated: every continuation is followed once, in order."""
    expected = ["page-1", "page-2", "page-3"]
    # The CLI returns the newest page first; continuations walk older history.
    pages = {
        None: page(["page-3"], "cursor-a", complete=False, physical=40),
        "cursor-a": page(["page-2"], "cursor-b", complete=False, physical=50),
        "cursor-b": page(["page-1"], None, complete=True, physical=60),
    }
    calls = []

    def fetch(size, continuation, timeout):
        calls.append((size, continuation, timeout))
        return pages[continuation]

    clock_values = iter([0.0, 10.0, 20.0, 30.0, 40.0, 50.0])
    result = history.walk(
        fetch,
        expected=expected,
        page_size=1,
        deadline_ms=1000,
        clock_ms=lambda: next(clock_values),
        text_of=lambda value: value,
    )
    assert [call[1] for call in calls] == [None, "cursor-a", "cursor-b"]
    assert result["pages"] == 3
    assert result["messages"] == 3
    assert result["verified"] is True
    assert result["outcome"] == "completed"
    # Logical bytes are counted once even though three physical pages moved.
    assert result["bytes_verified"] == sum(len(text.encode()) for text in expected)
    assert result["physical_bytes"] == 150
    assert result["physical_bytes"] > result["bytes_verified"]


def test_history_reader_rejects_repeated_continuation_token():
    """latency/AC-04.repeated-token: a repeated cursor is rejected, not followed."""
    calls = []

    def fetch(size, continuation, timeout):
        calls.append(continuation)
        # The second page repeats the token that produced it.
        return page(["page-1"], "cursor-a", complete=False, physical=10)

    result = history.walk(
        fetch,
        expected=["page-1", "page-2"],
        page_size=1,
        deadline_ms=1000,
        clock_ms=lambda: 0.0,
        text_of=lambda value: value,
    )
    assert calls == [None, "cursor-a"]
    assert result["token_repeats"] == 1
    assert result["verified"] is False
    assert result["outcome"] == "incorrect_content"
    assert result["bytes_verified"] == 0


def test_history_reader_fails_corrupt_or_incomplete_content():
    """latency/AC-04.corrupt: wrong order/content or a truncated traversal fails."""

    def corrupt_fetch(size, continuation, timeout):
        return page(["page-2", "page-1"], None, complete=True, physical=20)

    result = history.walk(
        corrupt_fetch,
        expected=["page-1", "page-2"],
        page_size=2,
        deadline_ms=1000,
        clock_ms=lambda: 0.0,
        text_of=lambda value: value,
    )
    assert result["verified"] is False
    assert result["corrupt"] is True
    assert result["outcome"] == "incorrect_content"
    assert result["bytes_verified"] == 0

    def incomplete_fetch(size, continuation, timeout):
        # Claims completeness but carries no continuation after a short page.
        return page(["page-1"], None, complete=False, physical=10)

    result = history.walk(
        incomplete_fetch,
        expected=["page-1", "page-2"],
        page_size=2,
        deadline_ms=1000,
        clock_ms=lambda: 0.0,
        text_of=lambda value: value,
    )
    assert result["corrupt"] is True
    assert result["verified"] is False


def test_history_reader_timeout_keeps_verified_bytes_separate():
    """latency/AC-04.timeout: deadline exhaustion fails while bytes stay separate."""
    current = {"value": 0.0}
    tokens = iter(["cursor-a", "cursor-b"])

    def slow_fetch(size, continuation, timeout):
        current["value"] += 700.0
        return page(["page-1"], next(tokens, None), complete=False, physical=30)

    result = history.walk(
        slow_fetch,
        expected=["page-1", "page-2"],
        page_size=1,
        deadline_ms=1000,
        clock_ms=lambda: current["value"],
        text_of=lambda value: value,
    )
    assert result["outcome"] == "timeout"
    assert result["verified"] is False
    assert result["bytes_verified"] == 0
    # Physical bytes already transferred remain visible for the report.
    assert result["physical_bytes"] == 60
    assert result["pages"] == 2


def test_clock_deduplicates_repeated_membership_for_one_lifetime(patched_clock):
    admission = clock.ClockAdmission(
        fixture_boot_id="boot-a", fixture_namespace="ns-a", clock=lambda: 1000
    )
    first = admission.register("cli", pid=21)
    second = admission.register("cli", pid=21)
    assert first == second
    assert len(admission.result()["producers"]) == 1
    assert admission.result()["producers"][0]["verified"] is True
