from conftest import FakeRunner, result

from repo.doctor import (
    TOOL_SPECS,
    ToolSpec,
    check_tool,
    format_doctor,
    parse_version,
    run_doctor,
    version_at_least,
)


def test_tool_check_reports_missing_executable():
    runner = FakeRunner()
    data = check_tool(runner, ToolSpec("go", ("version",)))
    assert data["found"] is False
    assert data["path"] is None


def test_tool_check_reports_installed_executable():
    runner = FakeRunner(
        which_map={"go": "/usr/bin/go"},
        responses=[result(["go"], stdout="go version go1.27\nextra")],
    )
    data = check_tool(runner, ToolSpec("go", ("version",)))
    assert data["found"] is True
    assert data["version"] == "go version go1.27"
    assert data["probe_ok"] is True


def test_tool_check_skips_version_probe_when_not_configured():
    runner = FakeRunner(which_map={"custom": "/usr/bin/custom"})
    data = check_tool(runner, ToolSpec("custom", ()))
    assert data["found"] and data["version"] is None
    assert runner.calls == []


def test_tool_check_reports_version_probe_failure():
    runner = FakeRunner(
        which_map={"psql": "/usr/bin/psql"},
        responses=[result(["psql"], returncode=1, stderr="bad")],
    )
    data = check_tool(runner, ToolSpec("psql"))
    assert data["found"] and data["probe_ok"] is False


def test_doctor_succeeds_when_all_required_tools_are_present():
    runner = FakeRunner()
    for spec in TOOL_SPECS:
        runner.which_map[spec.name] = f"/bin/{spec.name}"
    data = run_doctor(runner)
    assert data["ok"] is True
    assert data["missing_required"] == []
    assert data["missing_optional"] == []


def test_doctor_reports_missing_required_tools():
    runner = FakeRunner(which_map={"go": "/bin/go"})
    data = run_doctor(runner)
    assert data["ok"] is False
    assert "cargo" in data["missing_required"]
    assert "git" in data["missing_optional"]


def test_doctor_formatter_renders_json_and_human_reports():
    data = {
        "ok": False,
        "tools": [
            {"name": "go", "required": True, "found": True, "version": "go1.27", "probe_ok": True},
            {"name": "cargo", "required": True, "found": False, "version": None},
            {"name": "git", "required": False, "found": False, "version": None},
        ],
        "missing_required": ["cargo"],
        "missing_optional": ["git"],
    }
    text = format_doctor(data)
    assert "go1.27" in text
    assert "MISSING" in text
    assert "optional" in text
    assert "missing required tools: cargo" in text
    data["ok"] = True
    data["missing_required"] = []
    text = format_doctor(data)
    assert "all required tools are available" in text


def test_version_comparison_orders_numeric_components():
    assert parse_version("go version go1.26.0 linux/amd64") == (1, 26, 0)
    assert parse_version("v22.5.1") == (22, 5, 1)
    assert parse_version("rustc 1.85.0 (abc)") == (1, 85, 0)
    assert parse_version("none") is None
    assert version_at_least((1, 26, 0), (1, 26))
    assert not version_at_least((1, 25, 9), (1, 26))


def test_doctor_rejects_go_version_below_minimum():
    runner = FakeRunner(
        which_map={"go": "/bin/go"},
        responses=[result(["go"], stdout="go version go1.25.0 linux/amd64\n")],
    )
    data = check_tool(runner, ToolSpec("go", ("version",), min_version=(1, 26)))
    assert data["found"] is True
    assert data["version_ok"] is False
    text = format_doctor(
        {
            "ok": False,
            "tools": [data],
            "missing_required": [],
            "outdated": ["go"],
            "missing_optional": [],
        }
    )
    assert "OLD" in text
    assert "below the required version: go" in text
