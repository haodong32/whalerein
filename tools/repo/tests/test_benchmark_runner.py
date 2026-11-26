"""Benchmark runner: schema-2 facts, slot plan, trace export, replay and exit codes."""

from __future__ import annotations

import json
import subprocess
from pathlib import Path

import pytest

from repo.benchmark import model, runner
from repo.errors import RepoError, ToolMissingError, UsageError
from repo.process import ProcessResult


def ok(stdout="", returncode=0, missing=False):
    return ProcessResult(argv=(), cwd=".", returncode=returncode, stdout=stdout, missing=missing)


class Docker:
    """Records the docker calls the benchmark runner must own."""

    def __init__(
        self,
        document,
        container_code=0,
        build_code=0,
        missing_buildx=False,
        events=None,
        events_path=None,
        fail_cleanup=False,
        on_create=None,
    ):
        self.document = document
        self.container_code = container_code
        self.build_code = build_code
        self.missing_buildx = missing_buildx
        self.events = events
        self.events_path = events_path
        self.fail_cleanup = fail_cleanup
        self.on_create = on_create
        self.calls = []
        self.environ = {}
        self.created = None
        self.identity = "c" * 64
        self.out = self

    def info(self, _message):
        pass

    def which(self, tool):
        return None if (tool == "docker" and self.missing_buildx) else "/usr/bin/" + tool

    def require(self, tool, hint=""):
        path = self.which(tool)
        if path is None:
            raise ToolMissingError("required tool not found on PATH: " + tool)
        return path

    def run(self, argv, cwd, *, env=None, timeout=None, check=False, quiet=False):
        args = tuple(str(part) for part in argv)
        self.calls.append(args)
        if args[1] == "create":
            self.created = args
            if self.on_create:
                self.on_create(args)
            return ok(self.identity)
        if args[1:3] == ("container", "inspect"):
            if not self.created:
                return ok(returncode=1)
            label = self.created[self.created.index("--label") + 1].split("=", 1)[1]
            image = self.created[self.created.index("timeout") - 1]
            return ok(f"{self.identity} {label} {image} false")
        if args[1] == "start":
            mount = next(part for part in self.created if part.endswith("dst=/reports"))
            source = next(part[4:] for part in mount.split(",") if part.startswith("src="))
            target = Path(source)
            document = self.document() if callable(self.document) else self.document
            if document is not None:
                (target / "benchmark.json").write_text(json.dumps(document))
            if self.events is not None:
                path = target / (self.events_path or "events.ndjson")
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("\n".join(json.dumps(row) for row in self.events) + "\n")
            return ok(returncode=self.container_code)
        if args[1] == "ps":
            return ok("")
        if args[1] == "rm" and self.fail_cleanup:
            from repo.verification import VerificationError

            raise VerificationError("docker-cleanup-unverified")
        if args[1] in ("stop", "rm"):
            self.created = None
            return ok()
        raise AssertionError("unexpected docker call: " + repr(args))


@pytest.fixture(autouse=True)
def prepared_workspace(tmp_path, monkeypatch):
    from repo.verification import VerificationError

    def git(*args):
        subprocess.run(["git", "-C", tmp_path, *args], check=True, capture_output=True)

    git("init")
    git(
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "--allow-empty",
        "-m",
        "fixture",
    )

    def prepare(context, docker, target):
        assert target == "protocol-http"
        if docker.missing_buildx:
            raise VerificationError("docker-unavailable", 3)
        if docker.build_code:
            raise VerificationError("image-build-failed")
        return "sha256:" + "a" * 64, context.sources["public"].root

    monkeypatch.setattr(runner, "prepare", prepare)


def request(**changes):
    return runner.BenchRequest(**changes)


def facts_document(
    bench_request=None,
    *,
    completed=None,
    setup_failed=0,
    mode=None,
    build=("rev-a", "build-a"),
    artifacts=None,
    status=None,
    text_shift=0.0,
    workflow=None,
    network_profile=None,
    with_trace_coverage=False,
):
    """A current raw case document the model derives without invented verdicts."""
    bench_request = bench_request or request(attempts=1, warmup=0)
    if workflow is not None or network_profile is not None:
        bench_request = runner.BenchRequest(
            **{
                **bench_request.__dict__,
                "workflow": workflow or bench_request.workflow,
                "network_profile": network_profile or bench_request.network_profile,
                "link": "client"
                if (workflow or bench_request.workflow) == "recovery"
                else bench_request.link,
            }
        )
    mode = mode or bench_request.observation
    configuration = {**bench_request.as_configuration(), "observation_mode": mode}
    scenario = model.scenario(configuration)
    sizes = model.source_sizes(scenario)
    times = [index * bench_request.cadence_ms for index in range(len(sizes))]
    final_ms = times[-1] + bench_request.cadence_ms
    slots = runner.slot_plan(bench_request)["slots"]
    measured = [slot for slot in slots if slot["phase"] == "measured"]
    completed = bench_request.attempts if completed is None else completed
    attempts = []
    for index, slot in enumerate(measured):
        if index < completed:
            outputs = []
            for ordinal, (at_ms, size) in enumerate(zip(times, sizes, strict=True), 1):
                outputs.append(
                    {
                        "at_ms": at_ms + 20 + text_shift,
                        "ordinal": ordinal,
                        "kind": "preview",
                        "bytes": size,
                        "prefix_bytes": sum(sizes[:ordinal]),
                        "correct": True,
                    }
                )
            outputs.append(
                {
                    "at_ms": final_ms + 100 + text_shift,
                    "ordinal": len(outputs) + 1,
                    "kind": "terminal",
                    "bytes": sum(sizes),
                    "prefix_bytes": sum(sizes),
                    "correct": True,
                }
            )
            record = {
                "id": f"attempt-{index:04d}",
                "slot": slot["slot"],
                "phase": "measured",
                "arm": slot["arm"],
                "pair_id": slot["pair"],
                "status": "finished",
                "failure": {"attribution": None},
                "setup": {"errors": []},
                "source": {"times_ms": times, "sizes": sizes},
                "outputs": outputs,
                "observation": {
                    "started_ms": 0,
                    "ended_ms": final_ms + 100 + bench_request.observation_window_ms + text_shift,
                    "cutoff_ms": min(
                        bench_request.deadline_ms,
                        final_ms + 100 + bench_request.observation_window_ms + text_shift,
                    ),
                    "started_ns": 1_000_000_000,
                    "ended_ns": 1_000_000_000
                    + int(
                        round(
                            (final_ms + 100 + bench_request.observation_window_ms + text_shift)
                            * 1_000_000
                        )
                    ),
                    "cutoff_ns": 1_000_000_000
                    + min(
                        int(round(bench_request.deadline_ms * 1_000_000)),
                        int(
                            round(
                                (final_ms + 100 + bench_request.observation_window_ms + text_shift)
                                * 1_000_000
                            )
                        ),
                    ),
                    "early_stop": "terminal_window",
                },
            }
            if with_trace_coverage:
                record["trace"] = _trace_coverage()
            attempts.append(record)
        elif index < completed + setup_failed:
            attempts.append(
                {
                    "id": f"attempt-{index:04d}",
                    "slot": slot["slot"],
                    "phase": "measured",
                    "arm": slot["arm"],
                    "pair_id": slot["pair"],
                    "status": "setup_failed",
                    "failure": {"attribution": "fixture", "reason": "watch_setup"},
                    "setup": {"errors": ["watch_setup"]},
                    "source": {"times_ms": [], "sizes": []},
                    "outputs": [],
                    "observation": {
                        "started_ms": 0,
                        "ended_ms": 0,
                        "cutoff_ms": 0,
                        "early_stop": "error",
                    },
                }
            )
    document = {
        "schema_version": model.SCHEMA_VERSION,
        "kind": "benchmark-case",
        "scenario": scenario,
        "run": {
            "id": "run-1",
            "started_at": "2026-11-25T00:00:00Z",
            "surface": "cli-stdout",
            "endpoint": "cli_stdout",
            "observation_mode": mode,
            "configuration": configuration,
            "fingerprint": {
                "build": {"revision": build[0], "fingerprint": build[1]},
                "fixture": {"name": "fixture", "fingerprint": "fix-a"},
                "schemas": {"case": 2, "event": 1, "workload": 8},
                "platform": {"os": "linux", "arch": "amd64", "kernel": "6"},
                "limits": {"cpus": 2, "memory_bytes": 1024},
                "clock": {"source": "CLOCK_MONOTONIC", "unit": "ns", "verified": True},
                "sampling": {"socket_hz": 1, "runtime_hz": 1},
            },
        },
        "environment": {
            "artifacts": artifacts or {"cli": "img-a"},
            "topology": {"kind": "single-machine", "clock": "CLOCK_MONOTONIC"},
            "limits": {"cpus": 2, "memory_bytes": 1024},
            "versions": {},
            "fixture": {"name": "fixture", "fingerprint": "fix-a"},
            "clock": {
                "source": "CLOCK_MONOTONIC",
                "unit": "ns",
                "boot_id_verified": True,
                "namespace_verified": True,
                "producers": [
                    {"producer": "fixture", "lifetime": "lt-1", "verified": True},
                ],
            },
            "capture_origin_ns": 1_000_000_000,
        },
        "schedule": {"seed": str(bench_request.seed), "rule": "alternating", "slots": slots},
        "attempts": attempts,
        "trace": {"complete": mode != "trace"},
        "status": status
        or {"state": "complete", "reasons": [], "cleanup": {"result": "verified", "reason": None}},
    }
    if mode == "trace":
        document["trace"] = {
            "complete": True,
            "path": "case.pftrace",
            "losses": {},
        }
    return document


def _trace_coverage():
    paths = {}
    for name, required in model.COVERAGE_PATHS.items():
        paths[name] = {"required": list(required), "observed": list(required), "complete": True}
    return {"coverage": paths, "loss": {}}


def current_report(bench_request=None, **options):
    document = facts_document(bench_request, **options)
    return model.case_document(document)


def run(tmp_path, docker, **changes):
    return runner.run(
        tmp_path,
        docker,
        runner.BenchRequest(**changes),
        tmp_path / "report",
    )


# latency/AC-08.current
def test_current_facts_publish_derived_rows_and_one_owned_container(tmp_path):
    docker = Docker(facts_document(request(attempts=2, warmup=0)))
    result = run(tmp_path, docker, attempts=2, warmup=0)
    assert result.exit_code == 0
    assert result.path == (tmp_path / "report" / "benchmark.json").resolve()
    saved = json.loads(result.path.read_text())
    assert saved["schema_version"] == model.SCHEMA_VERSION
    assert saved["derived"]["outcomes"]["correct_by_deadline"] == 2
    assert saved["status"]["cleanup"]["result"] == "verified"
    run_call = next(args for args in docker.calls if args[1] == "create")
    name = run_call[run_call.index("--name") + 1]
    assert name.startswith("wr-verify-benchmark-")
    assert run_call[run_call.index("--network") + 1] == "none"
    assert run_call[run_call.index("--cap-add") + 1] == "NET_ADMIN"
    assert any(part.startswith("/tmp:") for part in run_call)
    assert run_call[-1] == "tests/integration/benchmark/main.py"
    stopped = {args[-1] for args in docker.calls if args[1] in ("stop", "rm")}
    assert stopped == {docker.identity}


def test_slot_plan_is_persisted_before_startup_and_private(tmp_path):
    captured = {}

    def on_create(args):
        mount = next(part for part in args if part.endswith("dst=/reports"))
        source = Path(next(part[4:] for part in mount.split(",") if part.startswith("src=")))
        captured["plan"] = json.loads((source / "schedule.json").read_text())
        captured["mode"] = source.stat().st_mode & 0o777
        env_file = Path(args[args.index("--env-file") + 1])
        captured["env"] = dict(
            line.split("=", 1) for line in env_file.read_text().splitlines() if "=" in line
        )
        captured["env_mode"] = env_file.stat().st_mode & 0o777
        captured["argv"] = " ".join(args)
        captured["env_args"] = [
            part.split("=", 1)[1].split("=", 1)[0] for part in args if part.startswith("WHALEREIN_")
        ]
        captured["env_values"] = {
            part.split("=", 1)[0]: part.split("=", 1)[1]
            for part in args
            if part.startswith("WHALEREIN_") and "=" in part
        }

    docker = Docker(facts_document(request(attempts=4, warmup=0)), on_create=on_create)
    result = run(tmp_path, docker, attempts=4, warmup=0)
    assert result.exit_code == 0
    plan = captured["plan"]
    assert plan["seed"] == "0" and plan["rule"] == "alternating"
    assert [slot["slot"] for slot in plan["slots"]] == [0, 1, 2, 3]
    assert captured["mode"] == 0o700
    assert captured["env_values"]["WHALEREIN_BENCH_MODE"] == "off"
    assert captured["env_values"]["WHALEREIN_BENCH_OBSERVATION"] == "off"
    assert captured["env_values"]["WHALEREIN_BENCH_OUTLET"] == "/reports/outlet"
    assert len(captured["env"]["WHALEREIN_BENCH_CAPTURE_KEY"]) >= 32
    assert captured["env_mode"] == 0o600
    assert "WHALEREIN_BENCH_CAPTURE_KEY=" not in captured["argv"]
    assert not (tmp_path / "report" / ".container.env").exists()


def test_mixed_load_slot_plan_is_counterbalanced_and_seeded(tmp_path):
    plan = runner.slot_plan(
        runner.BenchRequest(attempts=4, warmup=2, workflow="mixed-load", seed=0, observation="off")
    )["slots"]
    measured = [slot for slot in plan if slot["phase"] == "measured"]
    assert [(slot["arm"], slot["order"]) for slot in measured[:2]] == [
        ("idle", 0),
        ("bulk", 1),
    ]
    assert [(slot["arm"], slot["order"]) for slot in measured[2:]] == [
        ("bulk", 0),
        ("idle", 1),
    ]
    assert {slot["pair"] for slot in measured} == {"pair-0000", "pair-0001"}


@pytest.mark.parametrize(
    ("changes", "message"),
    [
        ({"workflow": "mixed-load", "attempts": 31, "warmup": 0}, "even measured"),
        ({"workflow": "mixed-load", "attempts": 30, "warmup": 3}, "even --warmup"),
        (
            {"workflow": "mixed-load", "attempts": 30, "warmup": 0, "p95_scope": "per-arm"},
            "at least 40 measured attempts",
        ),
        ({"attempts": 0}, "--attempts must be between"),
        ({"warmup": -1}, "--warmup must be between"),
        ({"observation": "maybe"}, "--observation must be"),
        ({"p95_scope": "worse"}, "--p95-scope must be"),
        ({"timeout": 0}, "--benchmark-timeout must be a positive"),
    ],
)
def test_invalid_requests_are_usage_errors(changes, message):
    with pytest.raises(UsageError, match=message):
        runner.BenchRequest(**changes).validate()


# latency/AC-09.defaults
def test_defaults_are_thirty_measured_four_warmup_and_zero_warmup_is_valid():
    default = runner.BenchRequest()
    assert default.attempts == 30
    assert default.warmup == 4
    default.validate()
    runner.BenchRequest(attempts=5, warmup=0).validate()
    runner.BenchRequest(
        attempts=40, warmup=0, workflow="mixed-load", p95_scope="per-arm"
    ).validate()


# latency/AC-09.setup-failure
def test_slot_accounting_six_successes_one_setup_failure_and_23_unstarted(tmp_path):
    docker = Docker(facts_document(request(attempts=30, warmup=0), completed=6, setup_failed=1))
    result = run(tmp_path, docker, attempts=30, warmup=0)
    assert result.exit_code == 1
    saved = json.loads(result.path.read_text())
    populations = saved["derived"]["populations"]
    assert populations["planned"]["measured"] == 30
    assert populations["started"]["measured"] == 7
    assert populations["measurement_entered"]["measured"] == 6
    assert populations["on_time_correct"]["measured"] == 6
    assert populations["setup_failures"] == {"fixture": 1}
    assert populations["not_started"] == 23
    assert saved["derived"]["outcomes"]["counts"]["not_started"] == 23
    assert len(saved["derived"]["attempts"]) == 30


# latency/AC-09.interrupted-slot
def test_interruption_keeps_available_partial_facts_and_pending_slots(tmp_path):
    partial = facts_document(request(attempts=30, warmup=0), completed=4)
    partial["status"] = {
        "state": "complete",
        "reasons": [],
        "cleanup": {"result": "verified", "reason": None},
    }
    docker = Docker(partial, container_code=130)
    result = run(tmp_path, docker, attempts=30, warmup=0)
    assert result.exit_code == 130
    saved = json.loads(result.path.read_text())
    assert saved["status"]["state"] == "incomplete"
    assert saved["derived"]["populations"]["on_time_correct"]["measured"] == 4
    assert saved["derived"]["populations"]["not_started"] == 26


# latency/AC-08.missing
def test_missing_evidence_publishes_an_explicit_incomplete_report(tmp_path):
    docker = Docker(None)
    result = run(tmp_path, docker, attempts=5, warmup=0)
    assert result.exit_code == 1
    saved = json.loads(result.path.read_text())
    assert saved["status"]["state"] == "incomplete"
    assert "Missing benchmark export" in saved["execution"]["errors"]
    assert saved["derived"]["populations"]["planned"]["measured"] == 5
    assert saved["derived"]["populations"]["not_started"] == 5


@pytest.mark.parametrize(
    ("mutation", "message"),
    [
        ({"schema_version": 1}, "case schema 1"),
        ({"schema_version": 3}, "case schema 3"),
        ({"kind": "benchmark-suite"}, "benchmark-suite"),
    ],
)
# latency/AC-08.old latency/AC-08.future latency/AC-08.mixed
def test_retired_and_future_case_versions_are_rejected(tmp_path, mutation, message):
    document = facts_document(request(attempts=1, warmup=0))
    document.update(mutation)
    docker = Docker(document)
    result = run(tmp_path, docker, attempts=1, warmup=0)
    assert result.exit_code == 1
    saved = json.loads(result.path.read_text())
    assert any(message in error for error in saved["execution"]["errors"])
    assert saved["derived"]["outcomes"]["measured"] == 1


# latency/AC-12.cleanup-after-success
def test_cleanup_error_after_a_correct_attempt_keeps_the_business_result(tmp_path):
    docker = Docker(facts_document(request(attempts=2, warmup=0)), fail_cleanup=True)
    result = run(tmp_path, docker, attempts=2, warmup=0)
    assert result.exit_code == 1
    saved = json.loads(result.path.read_text())
    assert saved["status"]["cleanup"]["result"] == "unverified"
    assert saved["derived"]["outcomes"]["correct_by_deadline"] == 2
    assert saved["derived"]["attempts"][0]["outcome"] == "completed"
    assert saved["execution"]["cleanup"]["result"] == "unverified"


# latency/AC-05.outer-clock-excluded
def test_outer_runner_timing_does_not_change_business_intervals(tmp_path, monkeypatch):
    document = facts_document(request(attempts=2, warmup=0))
    first = run(tmp_path / "first", Docker(document), attempts=2, warmup=0)
    ticks = iter([100.0, 100.001])
    monkeypatch.setattr(runner, "monotonic", lambda: next(ticks, 100.001))
    second = run(tmp_path / "second", Docker(document), attempts=2, warmup=0)
    assert first.exit_code == second.exit_code == 0
    assert (
        first.document["derived"]["attempts"][0]["first_text_ms"]
        == second.document["derived"]["attempts"][0]["first_text_ms"]
    )
    assert (
        first.document["derived"]["attempts"][0]["terminal_ms"]
        == second.document["derived"]["attempts"][0]["terminal_ms"]
    )
    assert first.document["execution"]["duration_ms"] != second.document["execution"]["duration_ms"]


# latency/AC-12.offline-replay
def test_online_and_offline_replay_publish_identical_business_rows(tmp_path):
    docker = Docker(facts_document(request(attempts=3, warmup=0), completed=2, setup_failed=1))
    online = run(tmp_path, docker, attempts=3, warmup=0)
    saved = json.loads(online.path.read_text())
    replayed = runner.replay(tmp_path / "report", output=tmp_path / "replay")
    assert replayed.exit_code == 1
    assert replayed.document["derived"] == saved["derived"]
    assert replayed.document["execution"]["replay"] is True
    assert (tmp_path / "replay" / "benchmark.json").is_file()
    saved_replay = json.loads((tmp_path / "replay" / "benchmark.json").read_text())
    assert saved_replay["derived"]["outcomes"] == saved["derived"]["outcomes"]


# latency/AC-10.export-failure
def test_trace_export_failure_preserves_the_verdict_and_exits_nonzero(tmp_path, monkeypatch):
    document = facts_document(
        request(attempts=2, warmup=0, observation="trace"), with_trace_coverage=True
    )

    def fail(*_args, **_kwargs):
        raise RepoError("perfetto unavailable")

    monkeypatch.setattr(runner.trace, "export", fail)
    result = run(tmp_path, Docker(document), attempts=2, warmup=0, observation="trace")
    assert result.exit_code == 1
    saved = json.loads(result.path.read_text())
    assert saved["derived"]["outcomes"]["correct_by_deadline"] == 2
    assert saved["execution"]["trace_export"]["state"] == "failed"
    assert result.document["derived"] == saved["derived"]


# latency/AC-10.trace-artifact
def test_trace_export_publishes_a_provenance_artifact_and_exemplar_index(tmp_path, monkeypatch):
    document = facts_document(
        request(attempts=2, warmup=0, observation="trace"), with_trace_coverage=True
    )
    seen = {}

    def fake_export(events, exemplars, path, *, origin_ns=None, run_id="unknown"):
        seen["exemplars"] = exemplars
        Path(path).write_bytes(b"trace")
        return {
            "checksum": "sha256:" + "c" * 64,
            "bytes": 5,
            "events": len(events),
            "exemplars": len(exemplars),
        }

    monkeypatch.setattr(runner.trace, "export", fake_export)
    result = run(tmp_path, Docker(document), attempts=2, warmup=0, observation="trace")
    assert result.exit_code == 0
    saved = json.loads(result.path.read_text())
    assert saved["trace"]["path"] == "case.pftrace"
    assert saved["trace"]["checksum"] == "sha256:" + "c" * 64
    assert saved["execution"]["trace_export"]["state"] == "complete"
    assert seen["exemplars"]
    assert all(item["start_ns"] == 1_000_000_000 for item in seen["exemplars"])
    assert all(item["end_ns"] > item["start_ns"] for item in seen["exemplars"])


def test_trace_mode_merges_the_typed_event_sidecar(tmp_path, monkeypatch):
    document = facts_document(
        request(attempts=1, warmup=0, observation="trace"), with_trace_coverage=True
    )
    document["events"] = None
    document["trace"]["events_path"] = "events.ndjson"
    events = [
        {
            "v": 1,
            "run": "run-1",
            "producer": "fixture",
            "lifetime": "lt-1",
            "seq": 1,
            "ts": 1_000_000_000,
            "event": "fixture_observe",
            "data": {
                "attempt": "attempt-0000",
                "ordinal": 1,
                "kind": "preview",
                "bytes": 1,
                "prefix_bytes": 1,
                "correct": True,
            },
        }
    ]
    seen = {}

    def fake_export(events_arg, exemplars, path, **_kwargs):
        seen["events"] = events_arg
        Path(path).write_bytes(b"trace")
        return {"checksum": "sha256:" + "d" * 64, "bytes": 5}

    monkeypatch.setattr(runner.trace, "export", fake_export)
    result = run(
        tmp_path,
        Docker(document, events=events),
        attempts=1,
        warmup=0,
        observation="trace",
    )
    assert result.exit_code == 0
    assert len(seen["events"]) == 1
    saved = json.loads((tmp_path / "report" / "benchmark.json").read_text())
    assert len(saved["events"]) == 1
    assert saved["events"][0]["event"] == "fixture_observe"


def test_matrix_persists_current_suite_index_and_case_evidence(tmp_path, monkeypatch):
    def fake_run(root, docker, scenario, directory, *, prepared=None):
        directory.mkdir(parents=True)
        document = current_report(runner.BenchRequest(attempts=2, warmup=0))
        path = directory / "benchmark.json"
        path.write_text(json.dumps(document))
        return runner.BenchResult(document, 0, path)

    monkeypatch.setattr(runner, "run", fake_run)
    result = runner.run_matrix(
        tmp_path,
        Docker(None),
        runner.BenchRequest(attempts=2, warmup=0),
        tmp_path / "suite",
    )
    assert result.exit_code == 0
    suite = json.loads((tmp_path / "suite" / "suite.json").read_text())
    assert suite["schema_version"] == model.SUITE_SCHEMA_VERSION
    assert suite["kind"] == "benchmark-suite"
    assert suite["status"]["state"] == "complete"
    assert len(suite["cases"]) == len(model.DEFAULT_CASES)
    for entry in suite["cases"]:
        assert entry["status"] == "complete"
        assert entry["case_schema"] == model.SCHEMA_VERSION
        assert entry["observation_mode"] == "off"
        assert entry["endpoint"] == "cli_stdout"
        assert entry["capture_quality"] == "complete"
        assert entry["artifacts"] == {"cli": "img-a"}
        assert entry["path"].endswith("/benchmark.json")
        assert entry["slot_plan"].endswith("/schedule.json")
    model.validate_suite(suite)


def test_matrix_interruption_keeps_pending_cases_and_partial_facts(tmp_path, monkeypatch):
    calls = {"count": 0}

    def fake_run(root, docker, scenario, directory, *, prepared=None):
        calls["count"] += 1
        directory.mkdir(parents=True)
        document = current_report(runner.BenchRequest(attempts=2, warmup=0), completed=0)
        path = directory / "benchmark.json"
        path.write_text(json.dumps(document))
        if calls["count"] == 2:
            return runner.BenchResult(document, runner.EXIT_INTERRUPTED, path)
        return runner.BenchResult(document, 0, path)

    monkeypatch.setattr(runner, "run", fake_run)
    result = runner.run_matrix(
        tmp_path,
        Docker(None),
        runner.BenchRequest(attempts=2, warmup=0),
        tmp_path / "suite",
    )
    assert result.exit_code == 130
    suite = json.loads((tmp_path / "suite" / "suite.json").read_text())
    assert suite["status"]["state"] == "interrupted"
    assert suite["status"]["pending"]
    assert suite["cases"][0]["status"] == "complete"
    assert suite["cases"][1]["status"] == "interrupted"
    assert all(case["status"] == "planned" for case in suite["cases"][2:])
    model.validate_suite(suite)
    assert (tmp_path / "suite" / suite["cases"][1]["path"]).is_file()
