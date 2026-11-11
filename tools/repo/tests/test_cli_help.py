"""Help stays available without project discovery or native execution."""

import json

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
    monkeypatch.setattr(cli, "find_root", forbid_dispatch)
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
