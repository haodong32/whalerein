"""Exercise Linux verification's actual adapter and report through fake Docker."""

import json
import subprocess
from pathlib import Path
from types import SimpleNamespace

import pytest
from conftest import FakeRunner, ok

from repo import testing
from repo.integration_targets import collection_paths, selected_artifacts, validate_inventory
from repo.runners import integration

ONE = "cases/test_observation_artifact.py::test_example"
TWO = "cases/test_proto_compat.py::test_example"
IMAGE = "sha256:" + "a" * 64


def _case(identity, status="passed"):
    return {"id": identity, "status": status, "kind": "integration", "node_role": "case"}


def _export(cases, shard=None):
    scope = {
        "suite_id": "integration",
        "project": "integration",
        "tests": [_case(identity) for identity in cases],
        "coverage": [],
        "completeness": "complete",
        "selection": "partial",
        "errors": [],
        "exit_code": 0,
    }
    if shard is not None:
        scope["shard"] = shard
    return {"version": 1, "scope": scope}


class Docker(FakeRunner):
    """Answer commands without running an image or fabricating an absent export."""

    def __init__(self, modes=(), shard_cases=None, images=None):
        super().__init__()
        self.modes = set(modes)
        self.shard_cases = shard_cases if shard_cases is not None else [[ONE], [TWO]]
        self.images = images or {"vectors": IMAGE, "vectors-builder": "sha256:" + "b" * 64}
        self.states = {}
        self.tags = {}
        self.sequence = 0
        self.started = []
        self.removed = []

    def run(self, argv, cwd, **kwargs):
        args = tuple(map(str, argv))
        self.calls.append((args, str(cwd)))
        op = args[1]
        if op == "buildx":
            if args[2] == "build":
                if "build-failed" in self.modes:
                    return ok(returncode=1)
                target = args[args.index("--target") + 1]
                result = self.images[target] if "invalid-image" not in self.modes else "invalid"
                tag = args[args.index("--tag") + 1]
                key = args[args.index("--label") + 1].split("=", 1)[1]
                platform = args[args.index("--platform") + 1]
                assert platform == "linux/arm64"
                if "wrong-platform-arch" in self.modes:
                    platform = "linux/amd64"
                label = key if "wrong-platform-label" not in self.modes else "other"
                self.tags[tag] = f"{platform} {result} {label}"
            return ok()
        if op == "info":
            return ok(stdout="linux/aarch64")
        if op == "image":
            return ok(stdout=self.tags[args[-1]]) if args[-1] in self.tags else ok(returncode=1)
        if op == "create":
            assert kwargs.get("env") == {"DOCKER_DEFAULT_PLATFORM": ""}
            image_pos = next(
                index for index, value in enumerate(args) if value in self.images.values()
            )
            command = args[image_pos + 1 :]
            name = args[args.index("--name") + 1]
            label = args[args.index("--label") + 1].split("=")[1]
            mounts = {
                args[index + 1].split(",")[2].removeprefix("dst="): Path(
                    args[index + 1].split(",")[1].removeprefix("src=")
                )
                for index, value in enumerate(args)
                if value == "--mount" and args[index + 1].startswith("type=bind,")
            }
            identity = f"{self.sequence:064x}"
            self.sequence += 1
            self.states[identity] = (name, label, args[image_pos], command, mounts)
            return ok(stdout=identity)
        if op == "container":
            state = self.states.get(args[-1])
            if state is None:
                return ok(returncode=1)
            name, label, image, command, _ = state
            if (
                "--collect-only" not in command
                and "conflict" in self.modes
                and name.startswith("wr-verify-test")
            ):
                label = "another-owner"
            return ok(stdout=f"{args[-1]} {label} {image} false")
        if op == "start":
            name, _, _, command, mounts = self.states[args[-1]]
            self.started.append(name)
            if command[:2] == ("bash", "-c"):
                return ok()
            if command[0] == "bash":
                for artifact in integration._expected_paths(command[2:]):
                    file = mounts["/products"] / artifact
                    file.parent.mkdir(parents=True, exist_ok=True)
                    file.write_text("fixture binary")
                return ok()
            if "--collect-only" in command:
                if "unmatched" in self.modes:
                    return ok(returncode=4)
                document = _export([ONE, TWO])
            else:
                if "interrupt" in self.modes or "timeout" in self.modes:
                    (mounts["/reports"] / "observations.jsonl").write_text(
                        json.dumps({"id": ONE, "status": "passed"}) + "\n"
                    )
                    return ok(returncode=130 if "interrupt" in self.modes else 124)
                if "missing" in self.modes:
                    return ok()
                shard = command[command.index("--shard") + 1] if "--shard" in command else None
                cases = [ONE, TWO] if shard is None else self.shard_cases[int(shard.split("/")[0])]
                if shard and "wrong-shard" in self.modes:
                    shard = "9/10"
                document = _export(cases, shard)
            (mounts["/reports"] / "integration.json").write_text(json.dumps(document))
            return ok()
        if op == "stop":
            assert args[-1] in self.states
            return ok()
        if op == "rm":
            identity = args[-1]
            name = self.states[identity][0]
            if "cleanup-failed" in self.modes and name.startswith("wr-verify-test"):
                return ok(returncode=1)
            self.removed.append(name)
            del self.states[identity]
            return ok()
        if op == "ps":
            return ok()
        raise AssertionError(f"Unexpected Docker call: {args}")


@pytest.fixture
def worktree(tmp_path, monkeypatch):
    monkeypatch.delenv("GITHUB_ACTIONS", raising=False)
    root = tmp_path / "worktree"
    root.mkdir()
    subprocess.run(["git", "init", "-q", root], check=True)
    for name in (
        *collection_paths("integration"),
        "tests/integration/Dockerfile",
        "tests/integration/Dockerfile.dockerignore",
        "tools/repo/linux-rust-packages.txt",
        "tools/repo/protoc-linux.sh",
        ".gitignore",
    ):
        path = root / name
        path.parent.mkdir(parents=True, exist_ok=True)
        text = {
            ".gitignore": ".data/\n",
            "tests/integration/Dockerfile.dockerignore": "**\n!tests/integration/Dockerfile\n",
        }
        path.write_text(text.get(name, "fixture"))
    subprocess.run(["git", "-C", root, "add", "."], check=True)
    subprocess.run(
        [
            "git",
            "-C",
            root,
            "-c",
            "user.name=Example",
            "-c",
            "user.email=a@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
        check=True,
    )
    return root


def run(root, runner, *, name="run", **options):
    request = testing.TestRequest(suite="integration", group="protocol-vectors", **options)
    result = integration.collect(root, runner, request, root / ".data" / name)
    return result.exit_code, json.loads((result.report_directory / "report.json").read_text())


def test_linux_leaves_are_a_partition_and_require_only_their_products():
    root = Path(__file__).resolve().parents[3]
    validate_inventory(root)
    assert set(collection_paths("protocol")) == set(collection_paths("protocol-vectors")) | set(
        collection_paths("protocol-http")
    )
    assert selected_artifacts("protocol-vectors") == (
        "node-dependencies",
        "noise-peer",
        "proto-peer",
    )
    assert "storage-tests" not in selected_artifacts("protocol")
    assert "storage-tests" in selected_artifacts("storage")


def test_parallel_has_one_build_and_exact_cover(worktree):
    docker = Docker()
    events = []
    display = SimpleNamespace(event=events.append)
    docker.progress = display
    code, report = run(worktree, docker, parallel=2)
    assert code == 0, report["verification"]
    assert events == [{"event": "phase", "description": "Integration · 2 cases in 2 shards"}]
    assert docker.progress is display
    builds = [args for args, _ in docker.calls if args[1:3] == ("buildx", "build")]
    assert [args[args.index("--target") + 1] for args in builds] == ["vectors", "vectors-builder"]
    creates = [args for args, _ in docker.calls if args[1] == "create"]
    assert len({args[args.index("--name") + 1] for args in creates}) == 5
    shards = [args for args in creates if "--shard" in args]
    assert [args[args.index("--shard") + 1] for args in shards] == ["0/2", "1/2"]
    for args in shards:
        assert args[args.index("--network") + 1] == "none"
        assert "tests/integration/cases/test_storage.py" not in args
        assert any(value.endswith("dst=/workspace,readonly") for value in args)
    assert [case["id"] for case in report["scopes"][0]["tests"]] == [ONE, TWO]
    assert report["verification"]["collected_cases"] == [ONE, TWO]
    assert report["verification"]["cleanup"]["status"] == "complete"
    assert len(docker.removed) == 5
    assert not docker.states


@pytest.mark.parametrize(
    ("mode", "expected", "status"),
    [
        ("build-failed", 1, "linux-platform-build-failed"),
        ("invalid-image", 1, "invalid-image-identity"),
        ("wrong-platform-label", 1, "platform-image-input-mismatch"),
        ("wrong-platform-arch", 1, "platform-image-native-mismatch"),
        ("unmatched", 2, "invalid-integration-selection"),
        ("missing", 1, "integration-execution-failed"),
        ("conflict", 1, "docker-ownership-conflict"),
        ("cleanup-failed", 1, "cleanup-failed"),
        ("interrupt", 130, "interrupted"),
        ("timeout", 1, "deadline-exceeded"),
        ("wrong-shard", 1, "integration-execution-failed"),
    ],
)
def test_failure_facts_and_owned_cleanup(worktree, mode, expected, status):
    docker = Docker([mode])
    code, report = run(worktree, docker, parallel=2 if mode == "wrong-shard" else 1)
    assert code == expected
    assert report["verification"]["error_code"] == status
    if mode in {"unmatched", "build-failed", "invalid-image"}:
        assert not any(name.startswith("wr-verify-test") for name in docker.started)
    if mode in {"conflict", "cleanup-failed"}:
        assert report["verification"]["cleanup"]["status"] == "failed"
        assert all(not name.startswith("wr-verify-test") for name in docker.removed)
    else:
        assert report["verification"]["cleanup"]["status"] == "complete"
        assert not docker.states
    if mode in {"interrupt", "timeout"}:
        assert any(case["id"] == ONE for scope in report["scopes"] for case in scope["tests"])
    if mode == "timeout":
        assert report["completeness"] == "incomplete"
        assert "Integration execution exceeded its deadline" in report["scopes"][0]["errors"]
    if mode == "cleanup-failed":
        assert [case["status"] for case in report["scopes"][0]["tests"]] == ["passed", "passed"]


def test_parallel_rejects_duplicates_and_missing_collected_ids(worktree):
    docker = Docker(shard_cases=[[ONE], [ONE]])
    code, report = run(worktree, docker, parallel=2)
    assert code == 1
    assert "Duplicate integration case identity across shards" in report["scopes"][0]["errors"]
    assert (
        "Observed integration cases do not match the collected inventory"
        in report["scopes"][0]["errors"]
    )


def test_parallel_accepts_intentionally_empty_shard(worktree):
    code, report = run(worktree, Docker(shard_cases=[[ONE, TWO], []]), parallel=2)
    assert code == 0
    assert report["completeness"] == "complete"


def test_listing_never_starts_assertion_container(worktree):
    docker = Docker()
    code, report = run(worktree, docker, list_only=True, only="fixture")
    assert code == 0
    assert [case["status"] for case in report["scopes"][0]["tests"]] == ["todo", "todo"]
    assert not any(name.startswith("wr-verify-test") for name in docker.started)


def test_reuses_tagged_platform_for_test_edit_and_rebuilds_for_platform_edit(worktree):
    docker = Docker()
    _, first = run(worktree, docker)
    test = worktree / "tests/integration/cases/test_observation_artifact.py"
    test.write_text("changed test source")
    _, second = run(worktree, docker, name="second")
    before = first["verification"]["artifacts"]["platform-vectors"]
    after = second["verification"]["artifacts"]["platform-vectors"]
    assert before["reused"] is False
    assert after == {**before, "reused": True}
    assert len([args for args, _ in docker.calls if args[1:3] == ("buildx", "build")]) == 2
    (worktree / "tests/integration/Dockerfile").write_text("changed platform source")
    _, third = run(worktree, docker, name="third")
    final = third["verification"]["artifacts"]["platform-vectors"]
    assert final["inputs"] != after["inputs"]
    assert final["reused"] is False
