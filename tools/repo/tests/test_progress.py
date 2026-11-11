"""Progress never substitutes for execution results or forwards raw output."""

import json
import sys

from repo.output import Output
from repo.process import Runner
from repo.progress import PREFIX, parse


def test_parses_go_events_but_omits_test_output():
    assert (
        parse(json.dumps({"Action": "output", "Test": "Case", "Output": "private fixture text"}))
        is None
    )
    event = parse(json.dumps({"Action": "pass", "Package": "p", "Test": "Case"}))
    assert event == {"event": "finish", "id": "p::Case", "status": "passed"}


def test_malformed_or_unknown_events_are_ignored():
    for text in ("42", "[]", PREFIX + "{", PREFIX + "{}", "ordinary child output"):
        assert parse(text) is None


def test_stream_drains_partial_stdout_and_stderr_without_waiting_for_newline(tmp_path):
    class Sink:
        def __init__(self):
            self.events = []

        def event(self, event):
            self.events.append(event)

    runner = Runner(Output(quiet=True))
    runner.progress = Sink()
    command = [
        sys.executable,
        "-c",
        'import sys;sys.stdout.write("partial");sys.stdout.flush();'
        'sys.stderr.write("x"*100000);sys.stderr.flush()',
    ]
    result = runner.run(command, tmp_path, timeout=5, quiet=True)
    assert result.ok
    assert result.stdout == "partial"
    assert len(result.stderr) == 100000


def test_plain_verbose_output_uses_final_status_and_keeps_coverage_separate(monkeypatch, capsys):
    from repo.progress import Display

    monkeypatch.setenv("NO_COLOR", "")
    display = Display(verbose=True)
    try:
        display.event({"event": "phase", "description": "Unit fixture", "total": 1})
        display.event(
            {
                "event": "start",
                "id": "case.one",
                "description": "When input is valid, the operation succeeds.",
            }
        )
        display.event({"event": "detail", "description": "Waiting for fixture readiness"})
        display.event(
            {
                "event": "finish",
                "id": "case.one",
                "description": "When input is valid, the operation succeeds.",
                "status": "passed",
            }
        )
        display.event({"event": "phase", "description": "Coverage checks", "count_cases": False})
        display.event({"event": "finish", "id": "case.one", "status": "passed"})
        assert display.finished == 1
        display.result(
            {
                "scopes": [
                    {"kind": "unit", "tests": [{"status": "passed"}], "coverage": [{"ok": False}]}
                ]
            },
            1,
        )
    finally:
        display.close()
    output = capsys.readouterr().err
    assert "\x1b" not in output
    assert "PASS" in output and "Coverage" in output and "FAIL" in output


def test_failure_shows_safe_case_and_phase(monkeypatch, capsys):
    from repo.progress import Display

    monkeypatch.setenv("CI", "1")
    display = Display(verbose=True)
    try:
        display.event(
            {
                "event": "start",
                "id": "case.one",
                "description": "When setup fails, execution stops.",
            }
        )
        display.event(
            {
                "event": "finish",
                "id": "case.one",
                "description": "When setup fails, execution stops.",
                "status": "failed",
                "phase": "setup",
            }
        )
        assert display.failed == 1
    finally:
        display.close()
    output = capsys.readouterr().err
    assert "FAIL" in output and "setup" in output and "case.one" in output


def test_stream_timeout_terminates_owned_process_group(tmp_path):
    class Sink:
        def event(self, event):
            pass

    runner = Runner(Output(quiet=True))
    runner.progress = Sink()
    result = runner.run(
        [sys.executable, "-c", "import time;time.sleep(60)"], tmp_path, timeout=0.1, quiet=True
    )
    assert result.returncode == 124


def test_native_parsers_keep_terminal_status_without_inventing_start():
    assert parse("test sample ... ok")["status"] == "passed"
    assert parse(json.dumps({"name": "suite", "type": "suite", "status": "passed"})) is None
    assert parse(json.dumps({"name": "case", "status": "failed"}))["status"] == "failed"


def test_stream_closes_subprocess_stdin_without_waiting_for_interactive_input(tmp_path):
    runner = Runner(Output(quiet=True))
    result = runner.run(
        [
            sys.executable,
            "-c",
            'import sys;assert not sys.stdin.isatty();assert sys.stdin.read()=="";print("eof")',
        ],
        tmp_path,
        timeout=3,
        quiet=True,
    )
    assert result.ok
    assert result.stdout.strip() == "eof"


def test_stream_reports_rust_partial_start_before_the_child_finishes(tmp_path):
    acknowledgement = tmp_path / "start-observed"

    class Sink:
        def __init__(self):
            self.events = []

        def event(self, event):
            self.events.append(event)
            if event["event"] == "start":
                acknowledgement.touch()

    sink = Sink()
    runner = Runner(Output(quiet=True))
    runner.progress = sink
    script = (
        "import sys,time;from pathlib import Path;"
        'sys.stdout.write("test sample::waiting ... ");sys.stdout.flush();'
        "deadline=time.monotonic()+2\n"
        "while not Path(sys.argv[1]).exists() and time.monotonic()<deadline: time.sleep(0.01)\n"
        'assert Path(sys.argv[1]).exists(),"start was not observed before completion"\n'
        'print("ok")'
    )
    result = runner.run(
        [sys.executable, "-c", script, str(acknowledgement), "--format=pretty"],
        tmp_path,
        timeout=3,
        quiet=True,
    )
    assert result.ok
    assert [event["event"] for event in sink.events] == ["start", "finish"]


def test_default_output_keeps_only_execution_unit_summary(monkeypatch, capsys):
    from repo.progress import Display

    monkeypatch.setenv("CI", "1")
    display = Display()
    try:
        display.event({"event": "phase", "description": "Unit · Core"})
        display.event({"event": "detail", "description": "Core test binary", "total": 2})
        display.event(
            {"event": "start", "id": "first", "description": "When input is valid, it succeeds."}
        )
        display.event({"event": "finish", "id": "first", "status": "passed"})
        display.event({"event": "finish", "id": "second", "status": "skipped"})
        display.event(
            {
                "event": "unit_finish",
                "description": "Unit · Core",
                "passed": 1,
                "failed": 0,
                "skipped": 1,
            }
        )
    finally:
        display.close()
    output = capsys.readouterr().err
    assert "RUN" not in output
    assert "SKIP " not in output
    assert "1 passed" in output and "1 skipped" in output
    assert output.count("Unit · Core") == 1


def test_container_rich_honors_host_terminal_without_docker_tty(monkeypatch):
    from repo.progress import Display

    monkeypatch.delenv("CI", raising=False)
    monkeypatch.delenv("NO_COLOR", raising=False)
    monkeypatch.setenv("WHALEREIN_PROGRESS_TTY", "1")
    display = Display()
    try:
        assert display.console.is_terminal
        assert not display.progress.disable
    finally:
        display.close()


def test_no_color_overrides_host_terminal_hint(monkeypatch):
    from repo.progress import Display

    monkeypatch.setenv("WHALEREIN_PROGRESS_TTY", "1")
    monkeypatch.setenv("NO_COLOR", "")
    display = Display()
    try:
        assert not display.console.is_terminal
        assert display.progress.disable
    finally:
        display.close()


def test_explicit_progress_stream_survives_test_fd_capture(tmp_path, capfd, monkeypatch):
    import os

    from repo.progress import Display

    monkeypatch.setenv("CI", "1")
    path = tmp_path / "console.txt"
    with path.open("w") as original:
        stream = os.fdopen(os.dup(original.fileno()), "w", buffering=1)
        display = Display(verbose=True, stream=stream)
        try:
            display.event(
                {
                    "event": "start",
                    "id": "case.one",
                    "description": "Visible progress while test output is captured.",
                }
            )
            print("captured fixture output")
            display.event(
                {
                    "event": "finish",
                    "id": "case.one",
                    "status": "passed",
                    "description": "Visible progress while test output is captured.",
                }
            )
        finally:
            display.close()
            stream.close()
    assert "PASS" in path.read_text()
    captured = capfd.readouterr()
    assert "captured fixture output" in captured.out
    assert "Visible progress" not in captured.err


def test_long_case_description_preserves_live_counts_and_elapsed(monkeypatch):
    import io

    from repo.progress import Display

    monkeypatch.setenv("CI", "1")
    display = Display(stream=io.StringIO())
    try:
        display.event({"event": "phase", "description": "Integration", "total": 80})
        display.event({"event": "finish", "id": "previous", "status": "passed"})
        description = "When the Node.js MQTT adapter publishes a large result, " * 8
        display.event({"event": "start", "id": "current", "description": description})
        for width in (60, 80, 120):
            display.console.width = width
            with display.console.capture() as captured:
                display.console.print(display.progress.get_renderable())
            rendered = captured.get()
            assert "1/80 finished · 0 failed" in rendered
            assert "0:00:" in rendered
            assert "When" in rendered
            assert len(rendered.splitlines()) == 1
    finally:
        display.close()


def test_cancelled_run_summary_is_interrupted(monkeypatch, capsys):
    from repo.progress import Display

    monkeypatch.setenv("NO_COLOR", "1")
    display = Display()
    display.result({"scopes": []}, 2)
    display.close()
    output = capsys.readouterr().err
    assert "Test execution INTERRUPTED" in output
    assert "Test execution FAIL" not in output
