"""Exercise source identity, cancellation, and failure at the shared entry point."""

import json
import subprocess

import pytest

from repo.sources import SourceSelection
from repo.verification import Invocation, VerificationError, recover_run, run_lock, run_suite


def _git(root, *args):
    subprocess.run(["git", "-C", root, *args], check=True, capture_output=True)


@pytest.fixture
def checkout(tmp_path):
    root = tmp_path / "checkout"
    root.mkdir()
    _git(root, "init")
    (root / "input").write_text("base")
    (root / "deleted").write_text("deleted")
    _git(root, "add", ".")
    _git(
        root,
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-m",
        "base",
    )
    return root


def _run(checkout, tmp_path, platform, revision=None, name="result"):
    result = run_suite(
        Invocation(
            "local",
            "example",
            (SourceSelection("public", checkout, revision=revision),),
            output=tmp_path / name,
        ),
        platform,
    )
    return result.exit_code, json.loads((result.report_directory / "report.json").read_text())


def _pass(ctx):
    ctx.record_scope(
        {
            "suite_id": "example",
            "project": "example",
            "completeness": "complete",
            "tests": [{"id": "case", "status": "passed"}],
        }
    )


def test_local_and_exact_inputs(checkout, tmp_path):
    (checkout / "input").write_text("edited")
    (checkout / "deleted").unlink()
    (checkout / "new").write_text("new")
    (checkout / ".env").write_text("excluded")

    def local(ctx):
        root = ctx.sources["public"].root
        (checkout / "input").write_text("later edit")
        assert (root / "input").read_text() == "edited"
        assert not (root / "deleted").exists()
        assert not (root / ".env").exists()
        assert (root / "new").is_file()
        _pass(ctx)

    def exact(ctx):
        root = ctx.sources["public"].root
        assert (root / "input").read_text() == "base"
        assert (root / "deleted").is_file()
        assert not (root / "new").exists()
        _pass(ctx)

    local_code, local_report = _run(checkout, tmp_path, local)
    exact_code, exact_report = _run(checkout, tmp_path, exact, revision="HEAD", name="exact")
    assert (local_code, exact_code) == (0, 0)
    local_source = local_report["verification"]["sources"]["public"]
    exact_source = exact_report["verification"]["sources"]["public"]
    assert local_source["revision"] == exact_source["revision"]
    assert local_source["fingerprint"] != exact_source["fingerprint"]
    assert local_source["mode"] == "worktree"
    assert exact_source["mode"] == "commit"


@pytest.mark.parametrize("stage", ["build", "readiness", "execution"])
def test_failure_preserves_phase_and_cleans_up(checkout, tmp_path, stage):
    cleaned = []

    def platform(ctx):
        ctx.register_cleanup("first", lambda deadline: cleaned.append("first"))
        ctx.register_cleanup("second", lambda deadline: cleaned.append("second"))
        with ctx.phase(stage):
            ctx.record_case("observed", "passed")
            raise VerificationError(f"{stage}-failed")

    status, report = _run(checkout, tmp_path, platform)
    assert status == 1
    assert report["verification"]["error_code"] == f"{stage}-failed"
    assert report["verification"]["phases"][1]["status"] == "failed"
    assert report["completeness"] == "incomplete"
    assert report["scopes"][0]["tests"][0]["id"] == "observed"
    assert cleaned == ["second", "first"]


def test_interrupt_and_cleanup_failure(checkout, tmp_path):
    def fail_cleanup(deadline):
        raise VerificationError("ownership-mismatch")

    def platform(ctx):
        ctx.register_cleanup("foreign", fail_cleanup)
        raise KeyboardInterrupt

    status, report = _run(checkout, tmp_path, platform)
    assert status == 130
    assert report["verification"]["error_code"] == "interrupted"
    assert report["verification"]["cleanup"] == {
        "status": "failed",
        "errors": [{"resource": "foreign", "code": "ownership-mismatch"}],
    }


def test_escaping_link_prevents_execution(checkout, tmp_path):
    (checkout / "link").symlink_to("../other")

    def platform(ctx):
        pytest.fail("build must not start for an escaping link")

    status, report = _run(checkout, tmp_path, platform)
    assert status == 1
    assert report["verification"]["error_code"] == "undeclared-source-link"


def test_recovery_requires_abandoned_owner_and_replays_only_named_resources(tmp_path):
    run = tmp_path / "run"
    run.mkdir()
    checkpoint = {
        "suite": "joint",
        "collected_cases": ["observed", "pending"],
        "observed_cases": [{"id": "observed", "status": "passed"}],
        "resources": {
            "one": {"kind": "container", "id": "one"},
            "two": {"kind": "container", "id": "two"},
        },
        "cleanup": {"status": "pending"},
    }
    (run / "checkpoint.json").write_text(json.dumps(checkpoint))
    removed = []

    def clean(name, ownership, deadline):
        assert ownership["id"] == name
        if name == "one":
            raise VerificationError("ownership-conflict")
        removed.append(name)

    with run_lock(run), pytest.raises(VerificationError, match="run-in-use"):
        recover_run(run, {"container": clean})
    assert removed == []
    outcome = recover_run(run, {"container": clean})
    assert removed == ["two"]
    assert outcome == {
        "status": "failed",
        "errors": [{"resource": "one", "code": "ownership-conflict"}],
    }
    assert json.loads((run / "recovery.json").read_text()) == outcome
    assert json.loads((run / "checkpoint.json").read_text()) == checkpoint
    report = json.loads((run / "report.json").read_text())
    assert report["verification"]["error_code"] == "abandoned-run"
    assert report["completeness"] == "incomplete"
    assert report["scopes"][0]["tests"] == [
        {"id": "observed", "status": "passed"},
        {"id": "pending", "status": "not_run"},
    ]


def test_joint_layout_uses_one_public_snapshot_and_checks_the_gitlink(checkout, tmp_path):
    from repo.sources import capture_sources

    public = tmp_path / "public"
    public.mkdir()
    (public / "public-go-source").write_text("pinned")
    _git(public, "init")
    _git(public, "add", ".")
    _git(
        public,
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-m",
        "public input",
    )
    pinned = subprocess.check_output(["git", "-C", public, "rev-parse", "HEAD"], text=True).strip()
    _git(
        checkout,
        "update-index",
        "--add",
        "--cacheinfo",
        f"160000,{pinned},upstream/whalerein",
    )
    _git(
        checkout,
        "-c",
        "user.name=Test",
        "-c",
        "user.email=test@example.invalid",
        "commit",
        "-m",
        "pin public",
    )
    (public / "public-go-source").write_text("current")
    (checkout / "input").write_text("private update")

    def capture(name, revision=None):
        return capture_sources(
            (
                SourceSelection("cloud", checkout, exclude=("upstream/whalerein",)),
                SourceSelection("public", public, "upstream/whalerein", revision=revision),
            ),
            tmp_path / name,
        )

    local = capture("local")
    formal_public = capture("exact-public", revision=pinned)
    nested = local["cloud"].root / "upstream/whalerein"
    assert (local["cloud"].root / "input").read_text() == "private update"
    assert local["public"].root == nested
    assert (nested / "public-go-source").read_text() == "current"
    assert (formal_public["public"].root / "public-go-source").read_text() == "pinned"
    assert local["public"].fingerprint != formal_public["public"].fingerprint
    assert local["cloud"].fingerprint == formal_public["cloud"].fingerprint


def test_cleanup_failure_does_not_erase_completed_scope(checkout, tmp_path):
    def fail_cleanup(deadline):
        raise VerificationError("ownership-mismatch")

    def platform(ctx):
        ctx.register_cleanup("foreign", fail_cleanup)
        with ctx.phase("execution"):
            _pass(ctx)

    status, report = _run(checkout, tmp_path, platform)
    assert report["scopes"][0]["tests"] == [{"id": "case", "status": "passed"}]
    assert report["verification"]["last_completed_phase"] == "execution"
    assert report["verification"]["cleanup"]["status"] == "failed"
    assert status == 1
