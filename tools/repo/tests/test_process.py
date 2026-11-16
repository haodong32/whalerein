import os
import subprocess
import sys

import pytest

from repo.errors import CommandFailedError, ToolMissingError
from repo.process import ProcessResult, Runner


def test_run_returns_stdout_and_zero_status_on_success(tmp_path, out):
    runner = Runner(out=out)
    result = runner.run([sys.executable, "-c", "print('hello')"], tmp_path)
    assert result.ok
    assert result.stdout.strip() == "hello"
    assert result.argv[0] == sys.executable


def test_run_raises_when_checking_nonzero_exit_status(tmp_path, out):
    runner = Runner(out=out)
    result = runner.run([sys.executable, "-c", "import sys; sys.exit(7)"], tmp_path)
    assert result.returncode == 7
    with pytest.raises(CommandFailedError):
        runner.run([sys.executable, "-c", "import sys; sys.exit(7)"], tmp_path, check=True)


def test_run_reports_missing_executable(tmp_path, out):
    runner = Runner(out=out)
    result = runner.run(["definitely-not-a-real-command-xyz"], tmp_path)
    assert result.returncode == 127
    assert result.missing
    with pytest.raises(ToolMissingError):
        runner.run(["definitely-not-a-real-command-xyz"], tmp_path, check=True)


def test_run_reports_process_timeout(tmp_path, out):
    runner = Runner(out=out)
    result = runner.run([sys.executable, "-c", "import time; time.sleep(2)"], tmp_path, timeout=0.1)
    assert result.returncode == 124
    assert "timed out" in result.stderr


def test_run_merges_environment_without_logging_secrets(tmp_path, out):
    runner = Runner(out=out, environ={"PATH": "/bin:/usr/bin", "BASE": "1"})
    result = runner.run(
        [sys.executable, "-c", "import os; print(os.environ.get('BASE'), os.environ.get('EXTRA'))"],
        tmp_path,
        env={"EXTRA": "two"},
    )
    assert result.stdout.strip() == "1 two"


def test_tool_lookup_and_requirement_check_report_availability(tmp_path, out):
    runner = Runner(out=out)
    assert runner.which("python3") is not None
    assert runner.which("definitely-not-a-real-command-xyz") is None
    with pytest.raises(ToolMissingError):
        runner.require("definitely-not-a-real-command-xyz", "hint")


def test_process_result_helpers_classify_success_and_failure():
    result = ProcessResult(argv=("a",), cwd=".", returncode=0, stdout="x", stderr="y")
    assert result.ok and result.output == "x\ny" and result.tail(100) == "x\ny"
    big = ProcessResult(argv=("a",), cwd=".", returncode=0, stdout="a" * 100)
    assert big.tail(10) == "a" * 10
    assert not ProcessResult(argv=("a",), cwd=".", returncode=1).ok


def test_stream_timeout_allows_child_cleanup(tmp_path, out):
    class Progress:
        def event(self, event):
            pass

    marker = tmp_path / "cleaned"
    ready = tmp_path / "ready"
    script = (
        "import signal,time,pathlib,sys\n"
        f"marker=pathlib.Path({str(marker)!r})\n"
        "def stop(*args):\n"
        "    marker.write_text('cleaned')\n"
        "    sys.exit(0)\n"
        "signal.signal(signal.SIGTERM,stop)\n"
        f"pathlib.Path({str(ready)!r}).write_text('ready')\n"
        "time.sleep(60)\n"
    )
    runner = Runner(out)
    runner.progress = Progress()
    result = runner.run([sys.executable, "-c", script], tmp_path, timeout=1, quiet=True)
    assert result.returncode == 124
    assert ready.exists()
    assert marker.read_text() == "cleaned"


def test_timeout_covers_child_after_output_streams_close(tmp_path, out):
    result = Runner(out).run(
        [sys.executable, "-c", "import os,time; os.close(1); os.close(2); time.sleep(60)"],
        tmp_path,
        timeout=0.1,
    )
    assert result.returncode == 124


def test_buffered_capture_retains_both_streams_and_exit_status(tmp_path, out):
    result = Runner(out)._capture(
        [
            sys.executable,
            "-c",
            "import sys; print('observed'); print('diagnostic',file=sys.stderr); sys.exit(7)",
        ],
        cwd=tmp_path,
        env=os.environ.copy(),
        timeout=5,
    )
    assert result.returncode == 7
    assert result.stdout.strip() == "observed"
    assert result.stderr.strip() == "diagnostic"


def test_buffered_capture_timeout_preserves_observed_output(tmp_path, out):
    with pytest.raises(subprocess.TimeoutExpired) as failure:
        Runner(out)._capture(
            [sys.executable, "-c", "import time; print('observed',flush=True); time.sleep(30)"],
            cwd=tmp_path,
            env=os.environ.copy(),
            timeout=1,
        )
    assert "observed" in failure.value.stdout


@pytest.mark.skipif(
    sys.platform != "win32", reason="requires native Windows process and pipe handling"
)
def test_windows_runner_captures_native_pipes_and_times_out(tmp_path, out):
    runner = Runner(out)
    result = runner.run([sys.executable, "-c", "print('native')"], tmp_path)
    assert result.ok and result.stdout.strip() == "native"
    result = runner.run(
        [sys.executable, "-c", "import time; print('observed',flush=True); time.sleep(30)"],
        tmp_path,
        timeout=1,
    )
    assert result.returncode == 124 and "observed" in result.stdout


@pytest.mark.skipif(
    sys.platform != "win32", reason="requires native Windows process-tree termination"
)
def test_windows_timeout_stops_descendants_and_retains_output(tmp_path, out):
    import ctypes

    pidfile = tmp_path / "child.pid"
    script = (
        "import subprocess,sys,time,pathlib\n"
        "child=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'])\n"
        f"pathlib.Path({str(pidfile)!r}).write_text(str(child.pid))\n"
        "print('descendant started',flush=True)\n"
        "time.sleep(60)\n"
    )
    result = Runner(out).run([sys.executable, "-c", script], tmp_path, timeout=2)
    assert result.returncode == 124 and "descendant started" in result.stdout
    pid = int(pidfile.read_text())
    kernel = ctypes.windll.kernel32
    kernel.OpenProcess.restype = ctypes.c_void_p
    kernel.WaitForSingleObject.argtypes = [ctypes.c_void_p, ctypes.c_ulong]
    kernel.CloseHandle.argtypes = [ctypes.c_void_p]
    handle = kernel.OpenProcess(0x100000, False, pid)
    try:
        assert not handle or kernel.WaitForSingleObject(handle, 0) == 0
    finally:
        if handle:
            kernel.CloseHandle(handle)
        subprocess.run(["taskkill", "/PID", str(pid), "/T", "/F"], capture_output=True, check=False)


@pytest.mark.skipif(sys.platform != "win32", reason="requires native Windows interruption handling")
def test_windows_interruption_retains_output_and_stops_child(monkeypatch, tmp_path, out):
    import time

    original = subprocess.Popen.communicate
    interrupted = False

    def communicate(process, *args, **kwargs):
        nonlocal interrupted
        if not interrupted:
            interrupted = True
            time.sleep(1)
            raise KeyboardInterrupt
        return original(process, *args, **kwargs)

    monkeypatch.setattr(subprocess.Popen, "communicate", communicate)
    result = Runner(out).run(
        [sys.executable, "-c", "import time; print('observed',flush=True); time.sleep(60)"],
        tmp_path,
    )
    assert result.returncode == 130 and "observed" in result.stdout
