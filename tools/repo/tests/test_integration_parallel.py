"""Parallel integration orchestration contracts with a fake Docker."""

import _thread
import json
import threading
import time
from pathlib import Path

from conftest import FakeRunner, ok

from repo import testing
from repo.runners import integration


def case(identity, status="passed"):
    return {"id": identity, "status": status, "kind": "integration", "node_role": "case"}


def export(
    cases,
    *,
    completeness="complete",
    selection="full",
    shard=None,
    code=0,
    checks=(),
    errors=(),
    duration_ms=1000,
):
    scope = {
        "suite_id": "integration",
        "project": "integration",
        "tests": list(cases),
        "coverage": [],
        "checks": list(checks),
        "completeness": completeness,
        "selection": selection,
        "errors": list(errors),
        "exit_code": code,
        "duration_ms": duration_ms,
    }
    if shard is not None:
        scope["shard"] = shard
    return {"version": 1, "scope": scope}


class Docker(FakeRunner):
    """Answer build/run/cleanup by argument shape; safe to call from threads."""

    def __init__(
        self,
        documents,
        *,
        build_code=0,
        invalid_image=False,
        verify_stdout="",
    ):
        super().__init__()
        self.documents = documents
        self.build_code = build_code
        self.invalid_image = invalid_image
        self.verify_stdout = verify_stdout
        self.lock = threading.Lock()
        self.builds = 0
        self.runs = []
        self.stops = []
        self.removes = []
        self.verifies = []

    def run(self, argv, cwd, **kwargs):
        args = tuple(map(str, argv))
        with self.lock:
            self.calls.append((args, str(cwd)))
        if args[1:3] == ("buildx", "version"):
            return ok()
        if args[1:3] == ("buildx", "build"):
            with self.lock:
                self.builds += 1
            if self.build_code:
                return ok(returncode=self.build_code)
            Path(args[args.index("--iidfile") + 1]).write_text(
                "not-an-image" if self.invalid_image else "sha256:" + "a" * 64
            )
            return ok()
        if args[1] == "run":
            name = args[args.index("--name") + 1]
            mount = args[args.index("--mount") + 1]
            source = next(part[4:] for part in mount.split(",") if part.startswith("src="))
            shard = args[args.index("--shard") + 1] if "--shard" in args else "0/1"
            with self.lock:
                self.runs.append((shard, name, source))
            document = self.documents.get(int(shard.split("/")[0]))
            if document is not None:
                target = Path(source)
                target.mkdir(parents=True, exist_ok=True)
                (target / "integration.json").write_text(json.dumps(document))
            return ok()
        if args[1] == "stop":
            with self.lock:
                self.stops.append(args[-1])
            return ok()
        if args[1] == "rm":
            with self.lock:
                self.removes.append(args[-1])
            return ok()
        if args[1] == "ps":
            with self.lock:
                self.verifies.append(args[args.index("--filter") + 1])
            return ok(stdout=self.verify_stdout)
        raise AssertionError("unexpected docker call: " + repr(args))


def collect(runner, tmp_path, out, parallel, **request):
    runner.out = out
    request.setdefault("suite", "integration")
    request.setdefault("parallel", parallel)
    return integration.collect(tmp_path, runner, testing.TestRequest(**request), tmp_path)


def test_parallel_builds_once_and_runs_unique_shards(tmp_path, out):
    documents = {
        0: export([case("cases/a.py::test_one"), case("cases/a.py::test_two")]),
        1: export([case("cases/b.py::test_three")], shard="1/2"),
    }
    documents[0]["scope"]["shard"] = "0/2"
    runner = Docker(documents)
    result = collect(runner, tmp_path, out, 2)
    assert runner.builds == 1
    assert [run[0] for run in runner.runs] == ["0/2", "1/2"]
    names = {run[1] for run in runner.runs}
    assert len(names) == 2
    sources = {run[2] for run in runner.runs}
    assert sources == {str(tmp_path / "container/0"), str(tmp_path / "container/1")}
    assert result["exit_code"] == 0
    assert result["completeness"] == "complete"
    assert result["selection"] == "full"
    assert not result["errors"]
    assert [entry["id"] for entry in result["tests"]] == [
        "cases/a.py::test_one",
        "cases/a.py::test_two",
        "cases/b.py::test_three",
    ]
    assert all(entry["status"] == "passed" for entry in result["tests"])
    assert result["coverage"] == [] and result["checks"] == []
    for name in names:
        assert name in runner.stops
        assert name in runner.removes
    assert {f"name=^/{name}$" for name in names} <= set(runner.verifies)


def test_parallel_combines_checks_and_partial_selection(tmp_path, out):
    documents = {
        0: export([case("cases/a.py::test_one")], selection="partial"),
        1: export(
            [case("cases/b.py::test_two")],
            checks=[{"name": "cross-language", "status": "passed"}],
        ),
    }
    result = collect(Docker(documents), tmp_path, out, 2)
    assert result["selection"] == "partial"
    assert result["checks"] == [{"name": "cross-language", "status": "passed"}]
    assert result["completeness"] == "complete"
    assert result["exit_code"] == 0


def test_empty_shard_exit_code_is_accepted(tmp_path, out):
    documents = {
        0: export([]),
        1: export([case("cases/b.py::test_two")]),
    }
    result = collect(Docker(documents), tmp_path, out, 2)
    assert result["completeness"] == "complete"
    assert result["exit_code"] == 0
    assert [entry["id"] for entry in result["tests"]] == ["cases/b.py::test_two"]


def test_parallel_does_not_sum_shard_durations(tmp_path, out):
    documents = {
        0: export([case("cases/a.py::test_one")], duration_ms=10_000_000),
        1: export([case("cases/b.py::test_two")], duration_ms=10_000_000),
    }
    result = collect(Docker(documents), tmp_path, out, 2)
    assert result["duration_ms"] < 10_000_000


def test_parallel_duplicate_case_identity_is_an_error(tmp_path, out):
    documents = {
        0: export([case("cases/a.py::shared")]),
        1: export([case("cases/a.py::shared")]),
    }
    result = collect(Docker(documents), tmp_path, out, 2)
    assert len(result["tests"]) == 1
    assert result["completeness"] == "incomplete"
    assert result["exit_code"] == 1
    assert any("Duplicate integration case identity" in error for error in result["errors"])


def test_parallel_missing_shard_export_is_incomplete(tmp_path, out):
    documents = {0: export([case("cases/a.py::test_one")]), 1: None}
    result = collect(Docker(documents), tmp_path, out, 2)
    assert result["completeness"] == "incomplete"
    assert result["exit_code"] != 0
    assert any("Missing or invalid integration export" in error for error in result["errors"])
    assert [entry["id"] for entry in result["tests"]] == ["cases/a.py::test_one"]


def test_parallel_incomplete_shard_is_not_a_pass(tmp_path, out):
    documents = {
        0: export([case("cases/a.py::test_one")]),
        1: export([case("cases/b.py::test_two")], completeness="incomplete"),
    }
    result = collect(Docker(documents), tmp_path, out, 2)
    assert result["completeness"] == "incomplete"
    assert result["exit_code"] == 1


def test_parallel_cleanup_failure_is_an_error(tmp_path, out):
    documents = {0: export([case("cases/a.py::test_one")]), 1: export([])}
    result = collect(Docker(documents, verify_stdout="deadbeef"), tmp_path, out, 2)
    assert any("verify owned container cleanup" in error for error in result["errors"])
    assert result["completeness"] == "incomplete"
    assert result["exit_code"] == 1
    assert len(result["errors"]) == 2


def test_parallel_build_failure_launches_no_container(tmp_path, out):
    runner = Docker({}, build_code=1)
    result = collect(runner, tmp_path, out, 2)
    assert runner.runs == []
    assert result["completeness"] == "incomplete"
    assert result["exit_code"] == 1
    assert any("image build failed" in error for error in result["errors"])


def test_parallel_invalid_image_id_launches_no_container(tmp_path, out):
    runner = Docker({}, invalid_image=True)
    result = collect(runner, tmp_path, out, 2)
    assert runner.runs == []
    assert result["completeness"] == "incomplete"
    assert any("Invalid built image ID" in error for error in result["errors"])


def test_parallel_rejects_export_for_the_wrong_shard(tmp_path, out):
    documents = {
        0: export([case("cases/a.py::test_one")], shard="0/2"),
        1: export([case("cases/b.py::test_two")], shard="0/2"),
    }
    result = collect(Docker(documents), tmp_path, out, 2)
    assert result["completeness"] == "incomplete"
    assert any("unexpected shard identity" in error for error in result["errors"])


def test_parallel_single_container_path_has_no_shard_option(tmp_path, out):
    runner = Docker({})
    result = collect(runner, tmp_path, out, 1)
    assert runner.builds == 1
    assert len(runner.runs) == 1
    assert runner.runs[0][2] == str(tmp_path / "container")
    assert "--shard" not in runner.calls[2][0]
    # The single-container path keeps its historical exit code when only the
    # export is missing; completeness still marks the run incomplete.
    assert result["completeness"] == "incomplete"
    assert result["errors"]


class BlockingDocker(Docker):
    def __init__(self):
        super().__init__({})
        self.started = threading.Event()
        self.release = threading.Event()
        self.lock = threading.Lock()
        self.run_count = 0
        self.stop_calls = []
        self.removes = []
        self.verifies = []

    def run(self, argv, cwd, **kwargs):
        args = tuple(map(str, argv))
        if args[1] == "run":
            with self.lock:
                self.run_count += 1
                if self.run_count == 2:
                    self.started.set()
            self.release.wait(10)
            return ok()
        if args[1] == "stop":
            with self.lock:
                self.stop_calls.append(args[-1])
            self.release.set()
            return ok()
        if args[1] == "rm":
            with self.lock:
                self.removes.append(args[-1])
            return ok()
        if args[1] == "ps":
            with self.lock:
                self.verifies.append(args[args.index("--filter") + 1])
            return ok()
        if args[1:3] == ("buildx", "version"):
            return ok()
        if args[1:3] == ("buildx", "build"):
            Path(args[args.index("--iidfile") + 1]).write_text("sha256:" + "a" * 64)
            return ok()
        raise AssertionError("unexpected docker call: " + repr(args))


def test_parallel_interrupt_stops_every_launched_container(tmp_path, out):
    runner = BlockingDocker()
    runner.out = out
    sentinel = object()
    runner.progress = sentinel

    def interrupt():
        if runner.started.wait(10):
            time.sleep(0.1)
            _thread.interrupt_main()

    watcher = threading.Thread(target=interrupt, daemon=True)
    watcher.start()
    result = integration.collect(
        tmp_path,
        runner,
        testing.TestRequest(suite="integration", parallel=2),
        tmp_path,
    )
    watcher.join(10)
    assert result["exit_code"] == 130
    assert result["completeness"] == "incomplete"
    names = set(runner.stop_calls)
    assert len(names) == 2
    assert runner.progress is sentinel
    assert names <= set(runner.removes)
