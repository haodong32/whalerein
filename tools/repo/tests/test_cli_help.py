"""Command discovery is read-only and usage errors preserve structured results."""

import json
import subprocess
import sys

import pytest

from repo import cli


def forbid_dispatch(*args, **kwargs):
    pytest.fail("informational request attempted repository discovery or execution")


def test_bare_command_prints_help_without_discovering_projects(monkeypatch, capsys):
    monkeypatch.setattr(cli, "find_root", forbid_dispatch)
    assert cli.main([]) == 0
    captured = capsys.readouterr()
    assert "commands:" in captured.out
    assert "./repo COMMAND --help" in captured.out
    assert "./repo projects" in captured.out
    assert captured.err == ""


@pytest.mark.parametrize(
    "command", [None, "projects", "doctor", "fmt", "check", "build", "test", "report", "proto"]
)
def test_help_is_informational_even_with_json(monkeypatch, capsys, command):
    monkeypatch.setattr(cli, "dispatch", forbid_dispatch)
    arguments = ["--json", *([command] if command else []), "--help"]
    with pytest.raises(SystemExit) as stopped:
        cli.main(arguments)
    assert stopped.value.code == 0
    captured = capsys.readouterr()
    assert "Examples:" in captured.out
    assert "tools/repo/README.md" in captured.out
    assert captured.err == ""


@pytest.mark.parametrize(
    ("arguments", "hint"),
    [
        (["unknown"], "./repo --help"),
        (["test", "--unknown"], "./repo test --help"),
        (["report"], "./repo report --help"),
    ],
)
def test_usage_errors_keep_json_envelope_and_suggest_relevant_help(capsys, arguments, hint):
    assert cli.main(["--json", *arguments]) == 2
    captured = capsys.readouterr()
    payload = json.loads(captured.out)
    assert payload["error"]["code"] == "usage_error"
    assert hint in payload["error"]["message"]


def test_test_help_describes_real_targets_and_prerequisites(capsys):
    with pytest.raises(SystemExit) as stopped:
        cli.main(["test", "--help"])
    assert stopped.value.code == 0
    help_text = capsys.readouterr().out
    assert "core" in help_text and "cargo" in help_text
    assert "protocol-vectors" in help_text and "docker" in help_text
    assert "ios-integration" in help_text and "xcodebuild" in help_text
    assert "--project" in help_text
    assert "android-state" in help_text and "java" in help_text


@pytest.mark.parametrize(
    "arguments",
    [
        ["test", "--only", "chosen"],
        ["test", "integration", "--only", "chosen"],
        ["test", "protocol", "--list"],
        ["test", "core", "--parallel", "2"],
        ["test", "core", "--only", " "],
        ["test", "core", "--simulator", "target"],
        ["test", "protocol-http", "--coverage"],
        ["test", "ios-integration", "--parallel", "2"],
        ["test", "ios-unit", "--shard", "1/4"],
        ["test", "ios-ui", "--shard", "5/4"],
        ["test", "ios-ui", "--shard", "1/1"],
        ["test", "ios-ui", "--shard", "1/4", "--only", "case"],
        ["test", "host", "--list"],
        ["check", "protocol-vectors"],
        ["check", "--coverage"],
        ["build", "access", "--coverage"],
    ],
)
def test_invalid_target_options_fail_before_resource_acquisition(monkeypatch, capsys, arguments):
    monkeypatch.setattr(cli.testing, "run", forbid_dispatch)
    monkeypatch.setattr(cli.toolchain, "run", forbid_dispatch)
    assert cli.main(["--json", *arguments]) == 2
    result = json.loads(capsys.readouterr().out)
    assert result["error"]["code"] == "usage_error"


def test_project_alias_selects_the_same_unit_target(monkeypatch, capsys):
    def test(root, projects, runner, request, output):
        assert request.suite == "unit"
        assert request.projects == ("android-state",)
        return {}, 0

    monkeypatch.setattr(cli.testing, "run", test)
    assert cli.main(["test", "--project", "android-state", "--json"]) == 0
    assert json.loads(capsys.readouterr().out)["ok"] is True
    assert cli.main(["test", "core", "--project", "android-state", "--json"]) == 2
    assert json.loads(capsys.readouterr().out)["error"]["code"] == "usage_error"


def test_bare_test_and_check_keep_distinct_local_defaults(monkeypatch, capsys):
    def test(root, projects, runner, request, output):
        assert request.suite == "unit"
        assert request.group is None
        assert {"core", "repo", "proto"}.issubset(request.projects)
        return {}, 0

    def check(projects, runner, action, *, explicit):
        assert action == "check" and not explicit
        assert {"core", "repo", "proto"}.issubset(project.id for project in projects)
        return {}, 0

    monkeypatch.setattr(cli.testing, "run", test)
    monkeypatch.setattr(cli.toolchain, "run", check)
    assert cli.main(["test", "--json"]) == 0
    assert json.loads(capsys.readouterr().out)["command"] == "test"
    assert cli.main(["check", "--json"]) == 0
    assert json.loads(capsys.readouterr().out)["command"] == "check"


@pytest.mark.parametrize("target", ["protocol-vectors", "protocol-http", "protocol", "integration"])
def test_public_linux_targets_share_offline_report_contract(monkeypatch, capsys, tmp_path, target):
    from repo import testing, verification

    def collect(root, runner, request, output):
        assert request.suite == "integration"
        assert (request.group or "integration") == target
        assert output == tmp_path / target

        def execute(context):
            context.record_scope(
                {
                    "suite_id": target,
                    "project": target,
                    "tests": [{"id": "case::chosen", "status": "passed"}],
                    "completeness": "complete",
                }
            )

        return verification.run_suite(
            verification.Invocation("local", target, (), output=output), execute
        )

    monkeypatch.setattr(testing.integration, "collect", collect)
    assert cli.main(["test", target, "--output", str(tmp_path / target), "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["data"]["summary"]["statuses"] == {"passed": 1}
    assert cli.main(["report", str(tmp_path / target), "--json"]) == 0
    assert json.loads(capsys.readouterr().out)["data"]["html"] == payload["data"]["html"]


@pytest.mark.parametrize("action", ["check", "test"])
def test_missing_native_executable_keeps_public_blocked_status(monkeypatch, capsys, action):
    from repo.process import ProcessResult

    def missing(self, argv, cwd, **kwargs):
        return ProcessResult(tuple(argv), str(cwd), 127, missing=True)

    monkeypatch.setattr(cli.Runner, "run", missing)
    assert cli.main([action, "core", "--json"]) == 3
    payload = json.loads(capsys.readouterr().out)
    assert payload["error"]["code"] == "tool_missing"
    assert payload["ok"] is False


def test_windows_cli_import_does_not_require_posix_file_locks():
    result = subprocess.run(
        [
            sys.executable,
            "-c",
            """
import builtins
import subprocess  # Load the standard library's native subprocess implementation.
native_import = builtins.__import__
def import_without_fcntl(name, *args, **kwargs):
    if name == "fcntl":
        raise ModuleNotFoundError("POSIX file locks unavailable")
    return native_import(name, *args, **kwargs)
builtins.__import__ = import_without_fcntl
from repo.cli import main
raise SystemExit(main(["projects", "--json"]))
""",
        ],
        capture_output=True,
        text=True,
        timeout=10,
    )
    assert result.returncode == 0, result.stderr
    assert json.loads(result.stdout)["ok"] is True


def test_cli_recovery_replays_only_the_recorded_owner(monkeypatch, capsys, tmp_path):
    from repo.runners import integration

    run = tmp_path / "abandoned"
    run.mkdir()
    resource = {"kind": "docker-container", "label": "run-label"}
    checkpoint = {"resources": {"container": resource}, "cleanup": {"status": "pending"}}
    (run / "checkpoint.json").write_text(json.dumps(checkpoint))
    observed = []

    def recover_container(runner, root, name, ownership, deadline):
        assert deadline > 0
        observed.append((name, ownership))

    monkeypatch.setattr(integration, "recover_container", recover_container)
    assert cli.main(["recover", str(run), "--json"]) == 0
    payload = json.loads(capsys.readouterr().out)
    assert payload["data"] == {"status": "complete", "errors": []}
    assert observed == [("container", resource)]
    assert cli.main(["recover", str(tmp_path / "absent"), "--json"]) == 2
    payload = json.loads(capsys.readouterr().out)
    assert payload["error"] == {"code": "usage_error", "message": "unknown-run-directory"}


def test_ios_cli_preserves_target_selector_simulator_and_blocked_result(
    monkeypatch, capsys, tmp_path
):
    from repo.runners import ios
    from repo.verification import Invocation, VerificationError, run_suite

    def ios_run(runner, root, target, *, case_filter, output, simulator, list_only, shard):
        assert shard is None
        assert (target, case_filter, output, simulator) == (
            "ios-ui",
            "case",
            tmp_path / "run",
            "id",
        )

        def blocked(context):
            raise VerificationError("requires-macos", 3)

        return run_suite(Invocation("local", target, (), output=output), blocked)

    monkeypatch.setattr(ios, "run_ios", ios_run)
    assert (
        cli.main(
            [
                "test",
                "ios-ui",
                "--only",
                "case",
                "--simulator",
                "id",
                "--output",
                str(tmp_path / "run"),
                "--json",
            ]
        )
        == 3
    )
    payload = json.loads(capsys.readouterr().out)
    assert payload["error"]["message"] == "requires-macos"
    assert payload["data"]["json"] == str(tmp_path / "run" / "report.json")


def test_report_cli_requires_a_saved_run_directory(tmp_path, capsys):
    assert cli.main(["report", str(tmp_path), "--json"]) == 2
    payload = json.loads(capsys.readouterr().out)
    assert "expected a run directory containing report.json" in payload["error"]["message"]
