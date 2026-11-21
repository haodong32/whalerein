"""Benchmark runner: owned container, validated export, failure and exit handling."""

from __future__ import annotations

import json
import subprocess

import pytest

from repo.benchmark import runner
from repo.errors import ToolMissingError, UsageError
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
        cleanup_stdout="",
        missing_buildx=False,
        export_bytes=None,
    ):
        self.document = document
        self.container_code = container_code
        self.build_code = build_code
        self.cleanup_stdout = cleanup_stdout
        self.missing_buildx = missing_buildx
        self.export_bytes = export_bytes
        self.calls = []
        self.progress = None
        self.interrupted = False
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
            from pathlib import Path

            target = Path(source)
            if self.export_bytes is not None:
                (target / "benchmark.json").write_text(self.export_bytes)
            elif self.document is not None:
                (target / "benchmark.json").write_text(json.dumps(self.document))
            return ok(returncode=self.container_code)
        if args[1] == "ps":
            return ok(self.cleanup_stdout)
        if args[1] == "rm":
            self.created = None
        if args[1] in ("stop", "rm"):
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


def attempt(identity="attempt-0000", outcome="completed", phase="measured", **extra):
    record = {
        "id": identity,
        "phase": phase,
        "outcome": outcome,
        "deadline_ms": 5000,
        "first_text_ms": 120.0,
        "final_ms": 300.0,
        "send_exit_ms": 340.0,
        "update_gaps_ms": [100.0],
        "update_count": 2,
        "duplicate_finals": 0,
    }
    record.update(extra)
    return record


def document(attempts=None, status=None):
    attempts = attempts or [attempt()]
    measured = [item for item in attempts if item["phase"] == "measured"]
    completed = [item for item in measured if item["outcome"] == "completed"]
    counts = {
        name: sum(item["outcome"] == name for item in measured)
        for name in (
            "completed",
            "product_error",
            "timeout_before_text",
            "timeout_after_text",
            "late_reply",
            "incorrect_content",
            "duplicate_final",
        )
    }
    firsts = sorted(
        item["first_text_ms"] for item in completed if item["first_text_ms"] is not None
    )
    finals = sorted(item["final_ms"] for item in completed if item["final_ms"] is not None)
    gaps = sorted(gap for item in completed for gap in item["update_gaps_ms"])
    return {
        "schema_version": 1,
        "scenario": {
            "id": "streaming-reply",
            "version": 1,
            "seed": "0",
            "deadline_ms": 5000,
            "observation_window_ms": 1000,
            "workload": {"updates": ["a", "b"], "gap_ms": 100, "delivery": "accepted"},
            "response": {
                "start_ms": 0,
                "final_ms": 200,
                "delay_ms": 0,
                "duplicate_final_ms": 500,
                "final_text": "ab",
            },
        },
        "run": {
            "id": "run-1",
            "started_at": "2026-11-14T00:00:00Z",
            "surface": "cli-stdout",
            "configuration": {
                "attempts": len(measured),
                "warmup_attempts": len([i for i in attempts if i["phase"] == "warmup"]),
                "concurrency": 1,
                "profile": "warm-client",
                "cadence_ms": 100,
            },
        },
        "environment": {
            "artifacts": {"cli": "sha256:" + "b" * 64},
            "topology": {},
            "limits": {},
            "versions": {},
            "fixture": {},
        },
        "attempts": attempts,
        "outcomes": {
            "counts": counts,
            "measured": len(measured),
            "correct_by_deadline": counts["completed"],
            "success_share": (counts["completed"] / len(measured)) if measured else None,
            "warmup": {
                "attempts": len([i for i in attempts if i["phase"] == "warmup"]),
                "completed": sum(
                    i["outcome"] == "completed" for i in attempts if i["phase"] == "warmup"
                ),
            },
        },
        "latency": {
            "samples": {
                "first_text_ms": entry(firsts),
                "final_ms": entry(finals),
                "update_gaps_ms": entry(gaps),
            },
        },
        "status": status or {"state": "complete", "reasons": [], "cleanup": {"result": "verified"}},
    }


def entry(values):
    from repo.benchmark import model

    if not values:
        return {"count": 0, "quantiles": {}, "unavailable": {}, "reason": "no_samples"}
    computed = model.quantiles(values)
    return {
        "count": len(values),
        "quantiles": {
            name: computed[name] for name, minimum in model.TAILS if len(values) >= minimum
        },
        "unavailable": {
            name: model.UNAVAILABLE_TAIL for name, minimum in model.TAILS if len(values) < minimum
        },
        "reason": None,
    }


def run(tmp_path, docker, **request):

    return runner.run(
        tmp_path,
        docker,
        runner.BenchRequest(**request),
        tmp_path / "report",
    )


def test_successful_run_writes_validated_report_and_uses_one_owned_container(tmp_path):
    docker = Docker(document())
    result = run(tmp_path, docker)
    assert result.exit_code == 0
    assert result.path == (tmp_path / "report" / "benchmark.json").resolve()
    saved = json.loads(result.path.read_text())
    assert saved["outcomes"]["correct_by_deadline"] == 1
    assert saved["status"]["cleanup"]["result"] == "verified"
    run_call = next(args for args in docker.calls if args[1] == "create")
    name = run_call[run_call.index("--name") + 1]
    assert name.startswith("wr-verify-benchmark-")
    # Isolation: no network, private tmpfs, only this run's export mounted.
    assert run_call[run_call.index("--network") + 1] == "none"
    assert run_call[run_call.index("--cap-add") + 1] == "NET_ADMIN"
    assert any(part.startswith("/tmp:") for part in run_call)
    assert run_call[-1] == "tests/integration/benchmark/main.py"
    stopped = {args[-1] for args in docker.calls if args[1] in ("stop", "rm")}
    assert stopped == {docker.identity}


def test_benchmark_entry_point_is_distinct_from_the_correctness_suite(tmp_path):
    docker = Docker(document())
    run(tmp_path, docker)
    run_call = next(args for args in docker.calls if args[1] == "create")
    assert run_call[-1] == "tests/integration/benchmark/main.py"
    assert "-m" not in run_call and "pytest.ini" not in " ".join(run_call)


def test_failed_attempt_exits_nonzero_while_keeping_evidence(tmp_path):
    docker = Docker(
        document([attempt("a-1"), attempt("a-2", outcome="product_error", final_ms=None)])
    )
    result = run(tmp_path, docker)
    assert result.exit_code == 1
    saved = json.loads(result.path.read_text())
    assert saved["outcomes"]["measured"] == 2
    assert saved["outcomes"]["counts"]["product_error"] == 1
    assert saved["outcomes"]["correct_by_deadline"] == 1


def test_default_bench_runs_the_whole_matrix_with_one_build(tmp_path, monkeypatch):
    from repo import cli

    docker = Docker(document([attempt(outcome="product_error", final_ms=None)]), container_code=1)
    original_run = docker.run
    original_prepare = runner.prepare
    prepared = []

    def prepare(context, native, target):
        result = original_prepare(context, native, target)
        prepared.append(result)
        return result

    def fail_only_first(argv, cwd, **kwargs):
        result = original_run(argv, cwd, **kwargs)
        if argv[1] == "start":
            docker.document = document()
            docker.container_code = 0
        return result

    monkeypatch.setattr(runner, "prepare", prepare)
    monkeypatch.setattr(docker, "run", fail_only_first)
    args = cli.build_parser().parse_args(["bench", "--output", str(tmp_path / "matrix")])
    result, code = cli.dispatch(args, tmp_path, docker)
    assert code == 1
    assert result["status"] == "complete"
    assert len(prepared) == 1
    cases = result["cases"]
    assert [(row["workflow"], row["network"], row["link"]) for row in cases] == list(
        runner.model.DEFAULT_CASES
    )
    assert [row["exit_code"] for row in cases] == [1] + [0] * (len(cases) - 1)
    assert all((tmp_path / "matrix" / row["report"]).is_file() for row in cases)
    assert json.loads((tmp_path / "matrix" / "suite.json").read_text())["cases"] == cases
    created = [call for call in docker.calls if call[1] == "create"]
    assert len(created) == len(cases)
    [(_, workspace)] = prepared
    assert all(f"type=bind,src={workspace},dst=/workspace,readonly" in call for call in created)
    assert [
        json.loads(
            next(
                arg.split("=", 1)[1]
                for arg in call
                if arg.startswith("WHALEREIN_BENCHMARK_CONFIG=")
            )
        )["network"]
        for call in created
    ] == [runner.model.network_settings(row["network"], row["link"]) for row in cases]


@pytest.mark.parametrize(
    ("selectors", "profile", "link"),
    [
        (["--network", "jitter"], "jitter", "host"),
        (["--link", "client"], "normal", "client"),
        (["--scenario", "recovery"], "normal", "client"),
        (["--scenario", "mixed-load"], "normal", "host"),
    ],
)
def test_a_bench_selector_keeps_single_case_execution(tmp_path, selectors, profile, link):
    from repo import cli

    docker = Docker(document())
    args = cli.build_parser().parse_args(
        ["bench", "--output", str(tmp_path / "single"), *selectors]
    )
    result, code = cli.dispatch(args, tmp_path, docker)
    assert code == 0
    assert result["report"].endswith("/benchmark.json")
    [created] = [call for call in docker.calls if call[1] == "create"]
    config = next(
        arg.split("=", 1)[1] for arg in created if arg.startswith("WHALEREIN_BENCHMARK_CONFIG=")
    )
    assert json.loads(config)["network"] == runner.model.network_settings(profile, link)


def test_matrix_interruption_keeps_unstarted_cases_pending(tmp_path):
    docker = Docker(document(), container_code=130)
    result = runner.run_matrix(tmp_path, docker, runner.BenchRequest(), tmp_path / "matrix")
    assert result.exit_code == 130
    assert result.document["status"] == "incomplete"
    assert result.document["cases"][0]["exit_code"] == 130
    assert all(row["status"] == "pending" for row in result.document["cases"][1:])
    assert len([call for call in docker.calls if call[1] == "create"]) == 1


def test_complete_run_with_a_late_reply_is_never_a_success(tmp_path):
    docker = Docker(document([attempt("a-1", outcome="late_reply", final_ms=None)]))
    result = run(tmp_path, docker)
    assert result.exit_code == 1
    saved = json.loads(result.path.read_text())
    assert saved["outcomes"]["correct_by_deadline"] == 0
    assert saved["attempts"][0]["first_text_ms"] == 120.0


def test_existing_report_in_the_output_directory_is_rejected(tmp_path):
    output = tmp_path / "report"
    output.mkdir()
    (output / "benchmark.json").write_text("{}")
    with pytest.raises(UsageError, match="output directory already exists"):
        runner.run(tmp_path, Docker(document()), runner.BenchRequest(), output)


@pytest.mark.parametrize(
    ("changes", "message"),
    [
        ({"attempts": 0}, "--attempts must be between"),
        ({"warmup": -1}, "--warmup must be between"),
        ({"delay_ms": -1}, "--delay must be between"),
        ({"deadline_ms": 0}, "--deadline must be between"),
        ({"cadence_ms": 5}, "--cadence must be between"),
        ({"cadence_ms": 70000}, "--cadence must be between"),
        ({"network_profile": "unknown"}, "--network must be a named"),
        ({"link": "unknown"}, "--link must be"),
        ({"concurrency": 2}, "--concurrency must be 1"),
        ({"timeout": 0}, "--benchmark-timeout must be a positive"),
    ],
)
def test_invalid_requests_are_usage_errors(changes, message):
    with pytest.raises(UsageError, match=message):
        runner.BenchRequest(**changes).validate()


def test_a_cadence_that_cannot_settle_before_the_deadline_is_a_usage_error():
    with pytest.raises(UsageError, match="needs a deadline above"):
        runner.BenchRequest(cadence_ms=2000, deadline_ms=5000).validate()


def test_workload_and_network_delay_reach_the_container(tmp_path):
    docker = Docker(document())
    run(tmp_path, docker, cadence_ms=20, network_profile="jitter", link="client")
    run_call = next(args for args in docker.calls if args[1] == "create")
    probe = next(part for part in run_call if part.startswith("WHALEREIN_BENCHMARK_CONFIG="))
    configuration = json.loads(probe.split("=", 1)[1])
    assert configuration["cadence_ms"] == 20
    assert configuration["network"] == runner.model.network_settings("jitter", "client")


def test_default_cadence_matches_the_streaming_scenario():
    assert runner.BenchRequest().cadence_ms == 100
    assert len(runner.model.UPDATES) == 32


def test_missing_buildx_is_a_prerequisite_failure(tmp_path):
    with pytest.raises(ToolMissingError, match="docker-unavailable"):
        run(tmp_path, Docker(document(), missing_buildx=True))


def test_build_failure_is_invalid_execution_with_no_invented_attempts(tmp_path):
    docker = Docker(document(), build_code=1)
    result = run(tmp_path, docker)
    assert result.exit_code == 1
    assert result.document["status"]["state"] == "invalid"
    assert any("image-build" in reason for reason in result.document["status"]["reasons"])
    assert not any(args[1] == "create" for args in docker.calls)


def test_missing_export_is_invalid_and_explicit(tmp_path):
    docker = Docker(None)
    result = run(tmp_path, docker)
    assert result.exit_code == 1
    assert result.document["status"]["state"] == "invalid"
    assert "Missing benchmark export" in result.errors


def test_malformed_export_is_never_published_as_a_measurement(tmp_path):
    docker = Docker(document(), export_bytes='{"schema_version": 1}')
    result = run(tmp_path, docker)
    assert result.exit_code == 1
    assert result.document["status"]["state"] == "invalid"
    assert any("invalid benchmark export" in reason for reason in result.errors)


def test_unverified_cleanup_invalidates_the_run(tmp_path):
    docker = Docker(document(), cleanup_stdout="deadbeef\n")
    result = run(tmp_path, docker)
    assert result.exit_code == 1
    saved = json.loads(result.path.read_text())
    assert saved["status"]["cleanup"]["result"] == "unverified"
    assert saved["status"]["state"] == "invalid"
    assert "cleanup_unverified" in saved["status"]["reasons"]


def test_interruption_keeps_partial_evidence_and_reports_130(tmp_path, monkeypatch):
    docker = Docker(document())

    original = docker.run

    def interrupt_on_run(argv, cwd, **kwargs):
        if tuple(map(str, argv))[1] == "start":
            raise KeyboardInterrupt
        return original(argv, cwd, **kwargs)

    monkeypatch.setattr(docker, "run", interrupt_on_run)
    result = run(tmp_path, docker)
    assert result.exit_code == 130
    assert result.document["status"]["state"] in ("incomplete", "invalid")
    assert result.path is not None


def test_failed_attempts_exit_1_but_keep_the_run_status_honest(tmp_path):
    """Exit 1 from the entry point means "ran, with failures" and the export says so."""
    docker = Docker(
        document(
            [attempt("a-1", outcome="timeout_before_text", first_text_ms=None, final_ms=None)]
        ),
        container_code=1,
    )
    result = run(tmp_path, docker)
    assert result.exit_code == 1
    assert result.document["status"]["state"] == "complete"
    assert result.document["outcomes"]["counts"]["timeout_before_text"] == 1


def test_a_crashed_entry_point_invalidates_the_run(tmp_path):
    docker = Docker(document(), container_code=137)
    result = run(tmp_path, docker)
    assert result.exit_code == 1
    assert result.document["status"]["state"] == "invalid"
    assert any("exited 137" in reason for reason in result.document["status"]["reasons"])


def test_incomplete_status_from_the_container_is_preserved(tmp_path):
    incomplete = document(
        status={
            "state": "incomplete",
            "reasons": ["run_interrupted"],
            "cleanup": {"result": "verified", "reason": None},
        }
    )
    docker = Docker(incomplete)
    result = run(tmp_path, docker)
    assert result.exit_code == 1
    assert result.document["status"]["state"] == "incomplete"
    assert "run_interrupted" in result.document["status"]["reasons"]
